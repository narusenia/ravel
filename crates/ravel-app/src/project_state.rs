// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The app-wide document state and its background evaluation
//! (layer-network-model plan, Phase 3).
//!
//! [`ProjectState`] is the single owner of the live [`Document`], the
//! Document-level undo stack (REQ-LAYER-009), and the background
//! [`EvalService`]. Every document edit — timeline layer edits, node editor
//! network edits, properties shell edits — flows through
//! [`ProjectState::apply_document`] / [`ProjectState::commit_document`],
//! which swap in the new snapshot and re-request the viewer evaluation.
//!
//! The Viewer permanently evaluates the **root composition output**
//! (REQ-LAYER-007): the shell chain is compiled with deterministic ids and
//! evaluated Document-aware, so layer networks are pulled recursively by the
//! boundary nodes.

use gpui::{Context, Global, WeakEntity};
use ravel_core::composition::compile::{CompileError, compile_composition};
use ravel_core::composition::{Composition, Document};
use ravel_core::eval::EvalContext;
use ravel_core::graph::Graph;
use ravel_core::id::NodeId;
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::runtime::{EvalRequest, EvalService, EvalUpdate, InvalidationHint};
use ravel_core::types::{FrameBuffer, FrameRate};
use ravel_gpu::GpuContext;
use ravel_ui::document::{
    DocumentStore, add_layer_from_template, default_document, root_composition,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// Long-edge cap (pixels) for interactive viewer evaluation. The shell
/// compositing chain still runs on the CPU with a readback per GPU node, so
/// full-resolution evaluation cannot hold frame rate yet; render-quality
/// output at composition resolution is Phase 4 scope (GPU compositing /
/// zero-copy viewer).
const VIEWER_MAX_DIM: u32 = 1024;

/// The composition resolution scaled down to fit [`VIEWER_MAX_DIM`]
/// (aspect preserved).
fn viewer_resolution((w, h): (u32, u32)) -> (u32, u32) {
    let long = w.max(h);
    if long <= VIEWER_MAX_DIM {
        return (w, h);
    }
    let scale = VIEWER_MAX_DIM as f64 / long as f64;
    (
        ((w as f64 * scale).round() as u32).max(1),
        ((h as f64 * scale).round() as u32).max(1),
    )
}

/// When set, [`ProjectState::new`] skips spawning the background evaluation
/// worker. gpui's deterministic test scheduler panics when a foreign OS
/// thread wakes it (even the worker's shutdown does), so test harnesses that
/// build real workspaces/panels must call
/// [`disable_background_eval_for_tests`] first.
static EVAL_DISABLED_FOR_TESTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Disable the background evaluation worker for gpui tests.
pub fn disable_background_eval_for_tests() {
    EVAL_DISABLED_FOR_TESTS.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Durable registry of the app's single [`ProjectState`]. Panels resolve it
/// at construction; a stale weak entity simply fails to upgrade.
pub struct ProjectStateHandle(pub WeakEntity<ProjectState>);

impl Global for ProjectStateHandle {}

/// Durable shared state: latest per-node evaluation durations, merged across
/// background evaluations. Read by the node editor's load readout.
#[derive(Clone, Default)]
pub struct NodeEvalTimings(pub HashMap<NodeId, Duration>);

impl Global for NodeEvalTimings {}

struct CompiledRoot {
    graph: Graph,
    output: NodeId,
}

/// GPUI entity owning the document, its undo history, and the background
/// evaluation service.
pub struct ProjectState {
    store: DocumentStore,
    registry: NodeRegistry,
    /// Background evaluation worker; owns the Evaluator, GpuContext, and
    /// ShaderManager so the UI thread never blocks on evaluation. `None`
    /// only in tests (a live worker thread breaks the deterministic gpui
    /// test scheduler).
    eval: Option<EvalService>,
    /// Compiled shell chain of the root composition, rebuilt after every
    /// document change (deterministic ids keep the evaluator caches warm).
    compiled: Option<CompiledRoot>,
    /// Invalidation accumulated while no request could be posted (e.g. an
    /// empty composition). Merged into the next posted request so a
    /// structural change is never lost.
    pending_hint: InvalidationHint,
}

impl ProjectState {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let eval = if EVAL_DISABLED_FOR_TESTS.load(std::sync::atomic::Ordering::SeqCst) {
            None
        } else {
            let gpu_ctx = GpuContext::new_blocking().expect("GPU context initialization failed");
            let (update_tx, mut update_rx) = futures::channel::mpsc::unbounded::<EvalUpdate>();
            let eval = EvalService::spawn(
                crate::eval_hooks::GpuEvalHooks::new(gpu_ctx),
                move |update| {
                    let _ = update_tx.unbounded_send(update);
                },
            );
            cx.spawn(async move |this, cx| {
                use futures::StreamExt as _;
                while let Some(update) = update_rx.next().await {
                    if this
                        .update(cx, |this, cx| this.on_eval_update(update, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            Some(eval)
        };

        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        Self {
            store: DocumentStore::new(default_document()),
            registry,
            eval,
            compiled: None,
            pending_hint: InvalidationHint::None,
        }
    }

    pub fn document(&self) -> &Document {
        self.store.document()
    }

    pub fn registry(&self) -> &NodeRegistry {
        &self.registry
    }

    /// The root composition, if the document has one.
    pub fn root_composition(&self) -> Option<&Composition> {
        root_composition(self.store.document())
    }

    // ----- document edits ----------------------------------------------------

    /// Live (mid-gesture) document update: no undo step is recorded.
    pub fn apply_document(
        &mut self,
        doc: Document,
        hint: InvalidationHint,
        cx: &mut Context<Self>,
    ) {
        self.store.apply(doc);
        self.document_changed(hint, cx);
    }

    /// Committed document update: records one undo step.
    pub fn commit_document(
        &mut self,
        doc: Document,
        hint: InvalidationHint,
        cx: &mut Context<Self>,
    ) {
        self.store.commit(doc);
        self.document_changed(hint, cx);
    }

    /// Document-level undo (REQ-LAYER-009). Returns whether a step was taken.
    pub fn undo(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = self.store.undo();
        if changed {
            self.document_changed(InvalidationHint::Structural, cx);
        }
        changed
    }

    /// Document-level redo. Returns whether a step was taken.
    pub fn redo(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = self.store.redo();
        if changed {
            self.document_changed(InvalidationHint::Structural, cx);
        }
        changed
    }

    /// Create a layer from a builtin template on top of the root
    /// composition's stack (REQ-LAYER-008).
    pub fn add_layer_from_template(&mut self, template_key: &str, cx: &mut Context<Self>) {
        let Some(comp) = self.store.document().root_comp else {
            return;
        };
        let Some(template) =
            ravel_core::composition::templates::builtin_layer_template(template_key)
        else {
            tracing::warn!(template_key, "unknown layer template");
            return;
        };
        match add_layer_from_template(self.store.document(), comp, template, &self.registry) {
            Ok(Some((doc, _layer))) => {
                self.commit_document(doc, InvalidationHint::Structural, cx);
            }
            Ok(None) => {}
            Err(err) => tracing::error!(%err, template_key, "layer template instantiation failed"),
        }
    }

    fn document_changed(&mut self, hint: InvalidationHint, cx: &mut Context<Self>) {
        self.compiled = None;
        self.request_viewer_eval(hint, cx);
        cx.notify();
    }

    // ----- viewer evaluation ---------------------------------------------------

    /// Post one background evaluation of the root composition output at the
    /// current playback position (REQ-LAYER-007). The worker coalesces
    /// rapid-fire requests latest-wins; hints of skipped requests are merged
    /// there, and hints that could not be posted at all are retained
    /// locally.
    pub fn request_viewer_eval(&mut self, hint: InvalidationHint, cx: &mut Context<Self>) {
        // Accumulate first: every early return below must retain the hint.
        let pending = std::mem::replace(&mut self.pending_hint, InvalidationHint::None);
        self.pending_hint = pending.merge(hint);

        let position = cx
            .try_global::<crate::panels::PlaybackPosition>()
            .copied()
            .unwrap_or_default();

        let Some(request) = self.build_viewer_request(position.frame) else {
            // Nothing evaluable (empty composition): blank the viewer and
            // outdate in-flight results.
            if let Some(eval) = self.eval.as_mut() {
                eval.cancel_pending();
            }
            cx.set_global(crate::panels::ViewerFrame(None));
            return;
        };
        let hint = std::mem::replace(&mut self.pending_hint, InvalidationHint::None);
        if let Some(eval) = self.eval.as_mut() {
            eval.request(EvalRequest { hint, ..request });
        } else {
            // No worker (tests): the hint stays pending.
            self.pending_hint = hint;
        }
    }

    /// Assemble the root-composition evaluation request, without the hint
    /// (filled by the caller). `None` when nothing is evaluable.
    fn build_viewer_request(&mut self, frame: u64) -> Option<EvalRequest> {
        let document = Arc::new(self.store.document().clone());
        let comp = root_composition(&document)?;
        let fps = comp.frame_rate;
        let resolution = viewer_resolution(comp.resolution);
        let compiled = self.compiled_root()?;
        Some(EvalRequest {
            graph: compiled.graph.clone(),
            node: compiled.output,
            path: Vec::new(),
            ctx: EvalContext::new(frame, fps, resolution),
            document: Some(document),
            hint: InvalidationHint::None,
        })
    }

    fn compiled_root(&mut self) -> Option<&CompiledRoot> {
        if self.compiled.is_none() {
            let comp = root_composition(self.store.document())?;
            match compile_composition(comp, Graph::new()) {
                Ok(result) => {
                    self.compiled = Some(CompiledRoot {
                        graph: result.graph,
                        output: result.output_node,
                    });
                }
                Err(CompileError::NoActiveLayers(_)) => return None,
                Err(err) => {
                    tracing::error!(%err, "root composition compilation failed");
                    return None;
                }
            }
        }
        self.compiled.as_ref()
    }

    /// Receives a background evaluation result. Only the most recently
    /// requested generation is published; stale results are dropped (but
    /// their timings still update the load readout).
    fn on_eval_update(&mut self, update: EvalUpdate, cx: &mut Context<Self>) {
        if !update.timings.is_empty() {
            let mut timings = cx
                .try_global::<NodeEvalTimings>()
                .cloned()
                .unwrap_or_default();
            timings.0.extend(update.timings.iter().copied());
            cx.set_global(timings);
        }

        let latest = self.eval.as_ref().map(EvalService::latest_generation);
        if latest != Some(update.generation) {
            return;
        }
        // A transient failure (e.g. mid-edit graph state) keeps the last
        // good frame instead of blanking the viewer; only the explicit
        // nothing-to-show path (empty composition) clears it.
        let frame = match update.result {
            Ok(data) => data
                .downcast_ref::<FrameBuffer>()
                .map(|fb| Arc::new(fb.clone())),
            Err(err) => {
                tracing::debug!(%err, "viewer evaluation failed; keeping last frame");
                return;
            }
        };
        cx.set_global(crate::panels::ViewerFrame(frame));
        cx.notify();
    }

    /// Frame rate and duration of the root composition, for the playback
    /// clock.
    pub fn playback_params(&self) -> Option<(FrameRate, u64)> {
        self.root_composition()
            .map(|c| (c.frame_rate, c.duration_frames))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_resolution_caps_the_long_edge() {
        assert_eq!(viewer_resolution((1920, 1080)), (1024, 576));
        assert_eq!(viewer_resolution((1080, 1920)), (576, 1024));
        // Small comps evaluate at native resolution.
        assert_eq!(viewer_resolution((640, 480)), (640, 480));
        assert_eq!(viewer_resolution((1024, 1024)), (1024, 1024));
    }
}
