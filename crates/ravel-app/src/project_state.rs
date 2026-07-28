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
//! The Viewer permanently evaluates the **active composition output**
//! (REQ-LAYER-007, REQ-UI-013): the shell chain is compiled with
//! deterministic ids and evaluated Document-aware, so layer networks are
//! pulled recursively by the boundary nodes. `ProjectState` is also the only
//! writer of the [`crate::panels::ActiveComposition`] global — it owns the
//! document the id must resolve in, and a switch has to drop the compiled
//! chain and re-request the evaluation.

use gpui::{App, Context, Global, WeakEntity};
use ravel_core::composition::compile::{CompileError, compile_composition};
use ravel_core::composition::{AssetKind, AssetPath, Composition, Document, MediaAssetEntry};
use ravel_core::eval::EvalContext;
use ravel_core::graph::Graph;
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::runtime::{EvalRequest, EvalService, EvalUpdate, InvalidationHint};
use ravel_core::types::{FrameBuffer, FrameRate};
use ravel_gpu::GpuContext;
use ravel_ui::document::{
    CompositionSettings, DocumentStore, add_composition, add_layer_from_template, default_document,
    duplicate_composition, neighbour_composition, remove_composition, update_composition,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

/// One save request: the destination plus the document snapshot and
/// document-generation captured when the user asked for it. Queued
/// requests write what the user saw at request time, and adopt their path
/// only while the identity still matches.
struct SaveRequest {
    path: PathBuf,
    document: Document,
    /// The composition the user was on when they asked for the save
    /// (REQ-UI-013) — captured with the document so a queued save records
    /// the session it describes.
    active_comp: Option<CompId>,
    generation: u64,
    revision: u64,
    completion: Option<SaveCompletion>,
}

type SaveCompletion = Box<dyn FnOnce(SaveOutcome, &mut App)>;

/// Result delivered to a caller waiting for one particular save request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The request saved the current document and no later edit remains.
    Saved,
    /// The request succeeded, but the document changed again while it ran.
    SavedButDirty,
    /// The request belonged to a document that has since been replaced.
    Superseded,
    /// The project archive could not be written.
    Failed,
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
    /// Compiled shell chain of the active composition, rebuilt after every
    /// document change and every composition switch (deterministic ids keep
    /// the evaluator caches warm).
    compiled: Option<CompiledRoot>,
    /// Invalidation accumulated while no request could be posted (e.g. an
    /// empty composition). Merged into the next posted request so a
    /// structural change is never lost.
    pending_hint: InvalidationHint,
    /// Path of the currently open `.ravprj`, set after a successful save or
    /// load; `None` for a never-saved document.
    project_path: Option<PathBuf>,
    /// Document identity counter, bumped when the document is replaced
    /// wholesale (new/load). An async save adopts its path only while the
    /// identity is unchanged — a path must never leak onto an unrelated
    /// replacement document.
    generation: u64,
    /// Document mutation counter, bumped by every user-driven store
    /// mutation (edits, undo/redo, New). An async load applies its result
    /// only while no intervening mutation happened — user edits must never
    /// be silently discarded. Load applications themselves do not bump it:
    /// a pending newer load must not be invalidated by an older one.
    revision: u64,
    /// Revision captured by the most recent successful save of this document.
    /// This advances on save completion, not request: a save writes its
    /// request-time snapshot, so edits made while it runs must remain dirty.
    /// New and loaded documents reset this to their current revision.
    saved_revision: u64,
    /// Whether an async save is currently in flight; a save requested
    /// while one runs is queued in `pending_saves` and started on
    /// completion, so writes never reach the disk out of order.
    save_in_flight: bool,
    /// Queued save requests, oldest first (see `save_in_flight`).
    pending_saves: std::collections::VecDeque<SaveRequest>,
    /// Monotonic load-request counter; only the newest load may apply
    /// (latest-wins for overlapping File ▸ Open requests).
    load_request: u64,
    /// Eval generation of the currently displayed [`ViewerFrame`]. An
    /// arriving update is published only when it is newer, so results
    /// always move the display forward; direct blanks (empty composition,
    /// compile error) advance this to the post-`cancel_pending` generation
    /// so an in-flight older result cannot overwrite them.
    published_generation: u64,
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

        let store = DocumentStore::new(default_document());
        // The startup document opens on its root composition; from here on
        // the active composition is UI state, never written back to the
        // document (REQ-UI-013).
        crate::panels::set_active_composition(store.document().root_comp, cx);

        Self {
            store,
            registry,
            eval,
            compiled: None,
            pending_hint: InvalidationHint::None,
            project_path: None,
            generation: 0,
            revision: 0,
            saved_revision: 0,
            save_in_flight: false,
            pending_saves: std::collections::VecDeque::new(),
            load_request: 0,
            published_generation: 0,
        }
    }

    pub fn document(&self) -> &Document {
        self.store.document()
    }

    /// Path of the currently open `.ravprj`, if the document was saved or
    /// loaded.
    pub fn project_path(&self) -> Option<&Path> {
        self.project_path.as_deref()
    }

    /// Whether the live document has changes newer than its last completed
    /// save (or its New/load baseline).
    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn registry(&self) -> &NodeRegistry {
        &self.registry
    }

    /// The composition the UI is editing, resolved in the live document.
    /// `None` when nothing is active or the active id is not in this
    /// document (composition 0).
    pub fn active_composition(&self, cx: &App) -> Option<&Composition> {
        crate::panels::active_composition_in(self.store.document(), cx)
    }

    /// Switch the composition the UI edits (REQ-UI-013). The layer selection
    /// is reset with it, the compiled chain is dropped, and the viewer
    /// re-evaluates. The document is untouched: `root_comp` keeps naming the
    /// composition a reopened document starts on, so a switch lands in
    /// neither the undo history nor the saved file.
    pub fn set_active_composition(&mut self, comp: Option<CompId>, cx: &mut Context<Self>) {
        if crate::panels::active_composition(cx) == comp {
            return;
        }
        crate::panels::set_active_composition(comp, cx);
        self.compiled = None;
        crate::audio::sync_from_document(self.store.document(), cx);
        self.request_viewer_eval(InvalidationHint::Structural, cx);
        cx.notify();
    }

    // ----- document edits ----------------------------------------------------

    /// Live (mid-gesture) document update: no undo step is recorded.
    pub fn apply_document(
        &mut self,
        doc: Document,
        hint: InvalidationHint,
        cx: &mut Context<Self>,
    ) {
        self.revision += 1;
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
        self.revision += 1;
        self.store.commit(doc);
        self.document_changed(hint, cx);
    }

    /// Discard uncommitted live edits (cancelled gestures), restoring the
    /// last committed snapshot. Returns whether anything changed.
    pub fn revert_document(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = self.store.revert();
        if changed {
            self.revision += 1;
            self.document_changed(InvalidationHint::Structural, cx);
        }
        changed
    }

    /// Document-level undo (REQ-LAYER-009). Returns whether a step was taken.
    pub fn undo(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = self.store.undo();
        if changed {
            self.revision += 1;
            self.document_changed(InvalidationHint::Structural, cx);
        }
        changed
    }

    /// Document-level redo. Returns whether a step was taken.
    pub fn redo(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = self.store.redo();
        if changed {
            self.revision += 1;
            self.document_changed(InvalidationHint::Structural, cx);
        }
        changed
    }

    // ----- project file (`.ravprj`) -------------------------------------------

    /// Replace the document with a fresh default one (File ▸ New). The undo
    /// history and project path are reset along with the document.
    pub fn new_document(&mut self, cx: &mut Context<Self>) {
        // A user-driven replacement: invalidates in-flight loads.
        let document = default_document();
        let active_comp = document.root_comp;
        self.revision += 1;
        self.replace_document(document, None, active_comp, cx);
    }

    /// Save the current document as a `.ravprj` at `path` (File ▸ Save /
    /// Save As). The document snapshot is cloned cheaply (`im` structural
    /// sharing) **at request time** and travels with the request, so a
    /// queued save writes the document the user asked about, not whatever
    /// is current when it starts. RON encoding, zip packing, and the file
    /// write all run on the background executor so the UI thread never
    /// blocks. `project_path` is updated only on success. Saves requested
    /// while another is in flight are queued and run in request order, so
    /// writes never land out of order.
    pub fn save_project_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.enqueue_save(path, None, cx);
    }

    /// Save and notify `completion` when this specific request finishes.
    /// Requests made during another save retain FIFO order, so the callback
    /// cannot run until all earlier queued saves have completed.
    pub fn save_project_to_then(
        &mut self,
        path: PathBuf,
        completion: impl FnOnce(SaveOutcome, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.enqueue_save(path, Some(Box::new(completion)), cx);
    }

    fn enqueue_save(
        &mut self,
        path: PathBuf,
        completion: Option<SaveCompletion>,
        cx: &mut Context<Self>,
    ) {
        let request = SaveRequest {
            path,
            document: self.store.document().clone(),
            active_comp: crate::panels::active_composition(cx),
            generation: self.generation,
            revision: self.revision,
            completion,
        };
        if self.save_in_flight {
            self.pending_saves.push_back(request);
            return;
        }
        self.save_in_flight = true;
        self.spawn_save(request, cx);
    }

    /// Run one save; the caller holds `save_in_flight`.
    fn spawn_save(&mut self, request: SaveRequest, cx: &mut Context<Self>) {
        let SaveRequest {
            path,
            document,
            active_comp,
            generation,
            revision,
            completion,
        } = request;
        let write_path = path.clone();
        let write = cx.background_executor().spawn(async move {
            // Overwriting an existing project keeps its original creation
            // timestamp; anything unreadable falls back to now.
            let created_at = crate::project::read_created_at(&write_path)
                .unwrap_or_else(crate::project::timestamp::rfc3339_now);
            let project_name = write_path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".to_string());
            let mut file =
                crate::project::ProjectFile::from_document(project_name, created_at, document);
            file.manifest.modified_at = crate::project::timestamp::rfc3339_now();
            file.ui_state = crate::project::ui_state::UiState::with_active_comp(active_comp);
            file.save(&write_path)
        });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            let _ = this.update(cx, |this, cx| {
                let outcome = match result {
                    Ok(()) => {
                        // Adopt the path only while the document identity is
                        // unchanged since the request: a New/Open during the
                        // write must not inherit a path that describes
                        // different content.
                        if this.generation == generation {
                            this.project_path = Some(path);
                            this.saved_revision = revision;
                            if this.revision == revision {
                                SaveOutcome::Saved
                            } else {
                                SaveOutcome::SavedButDirty
                            }
                        } else {
                            tracing::warn!(
                                path = %path.display(),
                                "save finished after the document was replaced; path not adopted"
                            );
                            SaveOutcome::Superseded
                        }
                    }
                    Err(err) => {
                        tracing::error!(%err, path = %path.display(), "failed to save project");
                        SaveOutcome::Failed
                    }
                };
                this.save_in_flight = false;
                if let Some(next) = this.pending_saves.pop_front() {
                    this.save_in_flight = true;
                    this.spawn_save(next, cx);
                }
                if let Some(completion) = completion {
                    // Run after this entity update ends: replacement callbacks
                    // may update ProjectState again through the workspace.
                    cx.defer(move |cx| completion(outcome, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Load a `.ravprj` from `path`, replacing the current document (File ▸
    /// Open). The file read runs on the background executor; loading is not
    /// an undo step (the store and its history are replaced wholesale).
    /// Latest-wins for overlapping requests, and the result is discarded
    /// when the user edited (or replaced) the document while the read was
    /// in flight — edits are never silently lost.
    pub fn load_project_from(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.load_request += 1;
        let request = self.load_request;
        let revision = self.revision;
        let read = cx.background_executor().spawn({
            let path = path.clone();
            async move { crate::project::ProjectFile::load(&path) }
        });
        cx.spawn(async move |this, cx| match read.await {
            Ok(file) => {
                let _ = this.update(cx, |this, cx| {
                    if this.load_request == request && this.revision == revision {
                        // The saved session's composition, or the document
                        // root when the archive predates `ui_state.json`
                        // (or names a composition it no longer has).
                        let active_comp = file.ui_state.initial_active_comp(&file.document);
                        this.replace_document(file.document, Some(path), active_comp, cx);
                    } else {
                        tracing::warn!(
                            path = %path.display(),
                            "discarding loaded project: superseded or edited while reading"
                        );
                    }
                });
            }
            Err(err) => {
                tracing::error!(%err, path = %path.display(), "failed to load project");
            }
        })
        .detach();
    }

    /// Swap in a whole new document (new project / loaded project): fresh
    /// undo history, dropped compile cache and stale invalidation, and one
    /// structural viewer re-evaluation. Bumps `generation` only — the
    /// caller is responsible for `revision` when the replacement comes from
    /// a user action, so load applications do not invalidate pending newer
    /// loads.
    ///
    /// `active_comp` is the composition the replacement opens on: the
    /// document root for a new project, the restored `ui_state.json` entry
    /// for a loaded one (REQ-UI-013).
    fn replace_document(
        &mut self,
        document: Document,
        path: Option<PathBuf>,
        active_comp: Option<CompId>,
        cx: &mut Context<Self>,
    ) {
        // The layer selection of the previous document never carries over —
        // even a reloaded project reuses composition ids for different
        // content. Published after the swap so observers resolve the new id
        // in the document that actually holds it.
        self.store = DocumentStore::new(document);
        crate::panels::set_active_composition(active_comp, cx);
        self.project_path = path;
        self.generation += 1;
        self.saved_revision = self.revision;
        self.compiled = None;
        self.pending_hint = InvalidationHint::None;
        // Asset ids may be reused for different files across documents:
        // drop the audio cache/tracks before the first sync of the new one.
        crate::audio::document_replaced(cx);
        crate::audio::sync_from_document(self.store.document(), cx);
        self.request_viewer_eval(InvalidationHint::Structural, cx);
        cx.notify();
    }

    /// Create a layer from a builtin template on top of the active
    /// composition's stack (REQ-LAYER-008).
    pub fn add_layer_from_template(
        &mut self,
        template_key: &str,
        cx: &mut Context<Self>,
    ) -> Option<LayerId> {
        let comp = self.active_composition(cx)?.id;
        let Some(template) =
            ravel_core::composition::templates::builtin_layer_template(template_key)
        else {
            tracing::warn!(template_key, "unknown layer template");
            return None;
        };
        match add_layer_from_template(self.store.document(), comp, template, &self.registry) {
            Ok(Some((doc, layer_id))) => {
                self.commit_document(doc, InvalidationHint::Structural, cx);
                Some(layer_id)
            }
            Ok(None) => None,
            Err(err) => {
                tracing::error!(%err, template_key, "layer template instantiation failed");
                None
            }
        }
    }

    // ----- media import (REQ-UI-010) -------------------------------------------

    /// Apply one batch of probed media files to the document (File ▸ Import
    /// / OS file drop): register each as a media asset — reusing the
    /// existing entry when the same absolute path is already present — and
    /// stack a media layer for it at the playhead (decision 4). The whole
    /// batch is a single `commit_document`, i.e. one undo step.
    ///
    /// Probing happened before this call (background executor, see
    /// [`crate::media::import`]); this method is the synchronous document
    /// edit. Composition settings are never touched (decision 5).
    pub fn import_media(
        &mut self,
        probed: Vec<crate::media::import::ProbedAsset>,
        skipped: Vec<crate::media::import::ImportFailure>,
        cx: &mut Context<Self>,
    ) -> crate::media::import::ImportSummary {
        let mut summary = crate::media::import::ImportSummary {
            skipped,
            ..crate::media::import::ImportSummary::default()
        };
        if probed.is_empty() {
            return summary;
        }

        let playhead = cx
            .try_global::<crate::panels::PlaybackPosition>()
            .map(|position| position.frame)
            .unwrap_or(0);
        let project_root = self
            .project_path
            .as_deref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf);
        let active = self
            .active_composition(cx)
            .map(|comp| (comp.id, comp.frame_rate, comp.duration_frames));

        let mut doc = self.store.document().clone();
        // Dedupe within the batch as well as against the document: two
        // frames of one sequence (or the same file picked twice) resolve to
        // one asset.
        let mut batch_ids: HashMap<PathBuf, String> = HashMap::new();
        let mut layer_specs = Vec::new();
        for asset in probed {
            let id = match batch_ids.get(&asset.path).cloned().or_else(|| {
                doc.media_assets.iter().find_map(|(id, entry)| {
                    (entry.resolved.as_deref() == Some(asset.path.as_path())).then(|| id.clone())
                })
            }) {
                Some(id) => id,
                None => {
                    let id = unique_asset_id(&doc, &asset.path);
                    doc = doc.with_media_asset_entry(
                        id.clone(),
                        MediaAssetEntry {
                            path: AssetPath::for_project_root(&asset.path, project_root.as_deref()),
                            kind: asset.kind.clone(),
                            metadata: asset.metadata.clone(),
                            resolved: Some(asset.path.clone()),
                        },
                    );
                    id
                }
            };
            batch_ids.insert(asset.path.clone(), id.clone());
            summary.imported.push((id.clone(), asset.path.clone()));
            layer_specs.push((id, asset));
        }

        // "Add as layer": the media template with `asset_id` bound, placed
        // at the playhead with the asset's own length; an unknown duration
        // spans the whole composition. A file with audio also gets the
        // shell's `AudioSource` bound to the same asset id (audio-plan
        // unit 4), and an audio-only file uses the frameless `audio`
        // template instead of a `media` node that has no picture to decode.
        if let Some((comp, comp_fps, comp_duration)) = active {
            for (id, asset) in layer_specs {
                let audio_stream_index = asset.metadata.first_audio_stream_index();
                let template_key = if audio_stream_index.is_some() && is_audio_only(&asset) {
                    "audio"
                } else {
                    "media"
                };
                let Some(template) =
                    ravel_core::composition::templates::builtin_layer_template(template_key)
                else {
                    tracing::warn!(template_key, "media import: layer template missing");
                    continue;
                };
                let out_frame = asset
                    .metadata
                    .duration_secs
                    .map(|secs| (secs * comp_fps.as_f64()).ceil().max(1.0) as u64)
                    .unwrap_or(comp_duration);
                let name_base = asset
                    .path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .filter(|stem| !stem.is_empty())
                    .unwrap_or_else(|| "Media".to_string());
                match ravel_ui::document::add_media_layer(
                    &doc,
                    comp,
                    template,
                    &self.registry,
                    ravel_ui::document::MediaLayerSpec {
                        name_base: &name_base,
                        asset_id: &id,
                        start_frame: playhead as i64,
                        out_frame,
                        audio_stream_index,
                    },
                ) {
                    Ok(Some((next, layer_id))) => {
                        doc = next;
                        summary.layers.push(layer_id);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        tracing::error!(%err, "media import: layer creation failed");
                    }
                }
            }
        } else {
            tracing::warn!("media import: no active composition; imported without layers");
        }

        self.commit_document(doc, InvalidationHint::Structural, cx);
        summary
    }

    // ----- composition management (REQ-UI-013) --------------------------------

    /// Create a composition from `settings` and make it the active one. One
    /// undo step: the settings are already final when this is called (the
    /// dialog holds unconfirmed values), so nothing has to be created and then
    /// corrected.
    pub fn create_composition(
        &mut self,
        settings: CompositionSettings,
        cx: &mut Context<Self>,
    ) -> CompId {
        let (doc, id) = add_composition(self.store.document(), settings);
        self.commit_document(doc, InvalidationHint::Structural, cx);
        self.set_active_composition(Some(id), cx);
        id
    }

    /// Replace a composition's settings, keeping its layers. One undo step.
    pub fn apply_composition_settings(
        &mut self,
        comp: CompId,
        settings: CompositionSettings,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = update_composition(self.store.document(), comp, |current| {
            settings.apply_to(current)
        }) else {
            return;
        };
        self.commit_document(doc, InvalidationHint::Structural, cx);
    }

    /// Deep-copy a composition and switch to the copy — the copy is what the
    /// user goes on to edit. One undo step.
    pub fn duplicate_composition(
        &mut self,
        comp: CompId,
        cx: &mut Context<Self>,
    ) -> Option<CompId> {
        let (doc, id) = duplicate_composition(self.store.document(), comp)?;
        self.commit_document(doc, InvalidationHint::Structural, cx);
        self.set_active_composition(Some(id), cx);
        Some(id)
    }

    /// Delete a composition. When it was the active one, the neighbour in
    /// display order takes over (`None` when it was the last composition —
    /// composition 0 is a valid state). One undo step.
    ///
    /// Undo restores the document, but not which composition was active: a
    /// composition switch is UI state and deliberately outside the undo
    /// history, so an undone delete leaves the neighbour active until the user
    /// switches back.
    pub fn delete_composition(&mut self, comp: CompId, cx: &mut Context<Self>) {
        let successor = neighbour_composition(self.store.document(), comp);
        let Some(doc) = remove_composition(self.store.document(), comp) else {
            return;
        };
        self.commit_document(doc, InvalidationHint::Structural, cx);
        crate::panels::drop_composition_properties_target(comp, cx);
        if crate::panels::active_composition(cx) == Some(comp) {
            self.set_active_composition(successor, cx);
        }
    }

    fn document_changed(&mut self, hint: InvalidationHint, cx: &mut Context<Self>) {
        self.compiled = None;
        // Every document change funnels through here (edit, revert, undo,
        // redo), which is the one place that can keep the shared layer
        // selection free of layers the document has lost — no panel has to
        // exist for that to hold.
        let document = self.store.document().clone();
        crate::panels::prune_layer_selection(&document, cx);
        crate::panels::prune_media_selection(&document, cx);
        crate::audio::sync_from_document(self.store.document(), cx);
        self.request_viewer_eval(hint, cx);
        cx.notify();
    }

    // ----- viewer evaluation ---------------------------------------------------

    /// Post one background evaluation of the active composition output at the
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

        let request = match self.build_viewer_request(position.frame, cx) {
            Ok(Some(request)) => request,
            Ok(None) => {
                // Nothing evaluable (no active composition, or an empty
                // one): blank the viewer and outdate in-flight results (the
                // fence keeps an older in-flight result from overwriting the
                // blank).
                if let Some(eval) = self.eval.as_mut() {
                    self.published_generation = eval.cancel_pending();
                }
                let frame = self.viewer_blank(cx);
                cx.set_global(frame);
                return;
            }
            Err(err) => {
                // The composition no longer compiles: surface the error in
                // the viewer — a silent blank would read as "empty", not
                // "broken".
                tracing::error!(%err, "active composition compilation failed");
                if let Some(eval) = self.eval.as_mut() {
                    self.published_generation = eval.cancel_pending();
                }
                let frame = self.viewer_error(err.to_string().into(), cx);
                cx.set_global(frame);
                return;
            }
        };
        let hint = std::mem::replace(&mut self.pending_hint, InvalidationHint::None);
        if let Some(eval) = self.eval.as_mut() {
            eval.request(EvalRequest { hint, ..request });
        } else {
            // No worker (tests): the hint stays pending.
            self.pending_hint = hint;
        }
    }

    /// Assemble the active-composition evaluation request, without the hint
    /// (filled by the caller). `Ok(None)` when nothing is evaluable,
    /// `Err` when the composition fails to compile.
    fn build_viewer_request(
        &mut self,
        frame: u64,
        cx: &App,
    ) -> Result<Option<EvalRequest>, CompileError> {
        let document = Arc::new(self.store.document().clone());
        let Some(comp) = crate::panels::active_composition_in(&document, cx) else {
            return Ok(None);
        };
        let fps = comp.frame_rate;
        let resolution = viewer_resolution(comp.resolution);
        let comp_resolution = comp.resolution;
        let Some(compiled) = self.compiled_root(cx)? else {
            return Ok(None);
        };
        Ok(Some(EvalRequest {
            graph: compiled.graph.clone(),
            node: compiled.output,
            path: Vec::new(),
            ctx: EvalContext::new(frame, fps, resolution).with_comp_resolution(comp_resolution),
            document: Some(document),
            hint: InvalidationHint::None,
        }))
    }

    /// `Ok(None)`: nothing to draw (no active composition, or no active
    /// layers). `Err`: the composition exists but failed to compile — the
    /// caller surfaces this in the viewer instead of blanking it.
    fn compiled_root(&mut self, cx: &App) -> Result<Option<&CompiledRoot>, CompileError> {
        if self.compiled.is_none() {
            let Some(comp) = crate::panels::active_composition_in(self.store.document(), cx) else {
                return Ok(None);
            };
            match compile_composition(comp, Graph::new()) {
                Ok(result) => {
                    self.compiled = Some(CompiledRoot {
                        graph: result.graph,
                        output: result.output_node,
                    });
                }
                Err(CompileError::NoActiveLayers(_)) => return Ok(None),
                Err(err) => return Err(err),
            }
        }
        Ok(self.compiled.as_ref())
    }

    /// An error state for the viewer, carrying the composition resolution so
    /// the panel can draw its black frame behind the message.
    fn viewer_error(&self, message: gpui::SharedString, cx: &App) -> crate::panels::ViewerFrame {
        let composition_resolution = self.active_composition(cx).map(|c| c.resolution);
        crate::panels::ViewerFrame::Error {
            message,
            composition_resolution,
        }
    }

    fn viewer_blank(&self, cx: &App) -> crate::panels::ViewerFrame {
        crate::panels::ViewerFrame::Blank {
            composition_resolution: self.active_composition(cx).map(|c| c.resolution),
        }
    }

    /// Receives a background evaluation result. Any result newer than the
    /// displayed one is published; older results are dropped (but their
    /// timings still update the load readout). Requiring the very latest
    /// generation instead would starve the viewer under load: with the
    /// generation advancing every playback tick, an evaluation slower than
    /// one frame interval is always "stale" by completion, so nothing would
    /// ever be published while playing. Monotonic acceptance still keeps
    /// ordering — an older in-flight result can never overwrite a newer
    /// one, and direct blanks fence via `published_generation`. A failed
    /// evaluation publishes [`ViewerFrame::Error`] — keeping the previous
    /// frame would show content the document no longer produces (e.g. a
    /// deleted Geometry node still visible because the Rasterize input went
    /// missing).
    ///
    /// Results reach the UI through globals only ([`crate::panels::ViewerFrame`]
    /// and [`NodeEvalTimings`]), never through an entity notify. `ProjectState`
    /// observers are panels that mirror the *document*, and the document does
    /// not change when an evaluation completes: notifying them here made all
    /// five rebuild their models on every playback frame. A panel that needs
    /// evaluation output subscribes to the global that carries it.
    fn on_eval_update(&mut self, update: EvalUpdate, cx: &mut Context<Self>) {
        if !update.timings.is_empty() {
            let mut timings = cx
                .try_global::<NodeEvalTimings>()
                .cloned()
                .unwrap_or_default();
            timings.0.extend(update.timings.iter().copied());
            cx.set_global(timings);
        }

        if update.generation <= self.published_generation {
            // The worker logged this result as sent; pairing that with an
            // explicit drop keeps "worker Ok but nothing published" visible
            // in the log.
            tracing::debug!(
                generation = update.generation,
                published = self.published_generation,
                frame = update.frame,
                dropped = true,
                "viewer update dropped (older than published)"
            );
            return;
        }
        let frame = match update.result {
            Ok(data) => match data.downcast_ref::<FrameBuffer>() {
                Some(fb) => crate::panels::ViewerFrame::Frame {
                    buffer: Arc::new(fb.clone()),
                    composition_resolution: self
                        .active_composition(cx)
                        .map(|c| c.resolution)
                        .unwrap_or((fb.width, fb.height)),
                },
                None => self.viewer_blank(cx),
            },
            Err(err) => {
                tracing::debug!(%err, "viewer evaluation failed");
                self.viewer_error(err.to_string().into(), cx)
            }
        };
        let published = match &frame {
            crate::panels::ViewerFrame::Frame { .. } => "frame",
            crate::panels::ViewerFrame::Blank { .. } => "blank",
            crate::panels::ViewerFrame::Error { .. } => "error",
        };
        tracing::debug!(
            generation = update.generation,
            frame = update.frame,
            published,
            "viewer frame published"
        );
        self.published_generation = update.generation;
        cx.set_global(frame);
    }

    /// Frame rate and duration of the active composition, for the playback
    /// clock.
    pub fn playback_params(&self, cx: &App) -> Option<(FrameRate, u64)> {
        self.active_composition(cx)
            .map(|c| (c.frame_rate, c.duration_frames))
    }
}

/// Whether a probed asset is a container with sound but no picture.
///
/// Such a file becomes a frameless `audio` layer instead of a `media` node:
/// the node would have no video stream to decode and would contribute a
/// transparent frame plus a warning to every evaluation, while the shell's
/// `AudioSource` is the part that actually plays (audio-plan decision 1,
/// unit 4). Only a container can be audio-only — a still or a sequence is
/// picture by definition.
fn is_audio_only(asset: &crate::media::import::ProbedAsset) -> bool {
    asset.kind == AssetKind::Container && asset.metadata.width.is_none()
}

/// A readable, collision-free asset id derived from the file name.
fn unique_asset_id(doc: &Document, path: &Path) -> String {
    let base = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "asset".to_string());
    if !doc.media_assets.contains_key(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base} {n}");
        if !doc.media_assets.contains_key(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};
    use ravel_core::animation::channel::AnimationChannel;
    use ravel_core::animation::curve::KeyframeCurve;
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_core::composition::{BlendMode, Layer};
    use ravel_core::graph::{Node, ParameterValue};
    use ravel_core::id::{DataTypeId, LayerId};
    use ravel_core::network as net;

    #[test]
    fn viewer_resolution_caps_the_long_edge() {
        assert_eq!(viewer_resolution((1920, 1080)), (1024, 576));
        assert_eq!(viewer_resolution((1080, 1920)), (576, 1024));
        // Small comps evaluate at native resolution.
        assert_eq!(viewer_resolution((640, 480)), (640, 480));
        assert_eq!(viewer_resolution((1024, 1024)), (1024, 1024));
    }

    #[gpui::test]
    fn viewer_request_keeps_composition_coordinate_resolution(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let (comp_resolution, ctx) = project.update(cx, |project, cx| {
            let comp_id = project.document().root_comp.unwrap();
            let document =
                ravel_ui::document::add_layer(project.document(), comp_id, content_layer())
                    .unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);
            let comp_resolution = project.active_composition(cx).unwrap().resolution;
            let request = project.build_viewer_request(0, cx).unwrap().unwrap();
            (comp_resolution, request.ctx)
        });

        assert_eq!(ctx.resolution, viewer_resolution(comp_resolution));
        assert_eq!(ctx.comp_resolution, comp_resolution);
    }

    /// A layer whose network carries a keyframed custom parameter on the In
    /// node, plus keyframed opacity on the shell.
    fn content_layer() -> Layer {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(24, 1.0, Interpolation::Linear);
        let intensity = AnimationChannel::keyframes(curve);

        let mut opacity_curve = KeyframeCurve::new();
        opacity_curve.insert(0, 0.0, Interpolation::Linear);
        opacity_curve.insert(15, 1.0, Interpolation::Linear);

        let network = Graph::new()
            .add_node(
                Node::new(NodeId::next(), net::NET_IN_TYPE_KEY)
                    .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
                    .with_output(net::PORT_TIME, DataTypeId::SCALAR)
                    .with_output("intensity", DataTypeId::SCALAR)
                    // Current-format In nodes carry `f` (see the load-time
                    // port normalization) so the roundtrip stays exact.
                    .with_output(net::PORT_FRAME_INDEX, DataTypeId::SCALAR)
                    .with_param("intensity", ParameterValue::Channel(intensity)),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::next(), net::NET_OUT_TYPE_KEY)
                    .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
            )
            .unwrap();

        Layer {
            opacity: AnimationChannel::keyframes(opacity_curve),
            ..Layer::new(LayerId::next(), "Solid 1", network)
                .with_time(0, 0, 300)
                .with_blend_mode(BlendMode::Screen)
        }
    }

    /// Stand-in for one of the five panels that observe `ProjectState` to
    /// mirror the document, counting how often it would rebuild its model.
    struct ObserverProbe {
        rebuilds: usize,
        _sub: gpui::Subscription,
    }

    fn observer_probe(
        project: &gpui::Entity<ProjectState>,
        cx: &mut TestAppContext,
    ) -> gpui::Entity<ObserverProbe> {
        let project = project.clone();
        cx.new(|cx| ObserverProbe {
            rebuilds: 0,
            _sub: cx.observe(&project, |this: &mut ObserverProbe, _project, _cx| {
                this.rebuilds += 1;
            }),
        })
    }

    /// RESP-1 regression (CRIT-01): an evaluation result reaches the UI through
    /// globals only. Notifying `ProjectState` observers here made all five
    /// document-mirroring panels rebuild their models on every playback frame,
    /// which multiplied every other render cost by the frame rate.
    #[gpui::test]
    fn eval_results_do_not_rebuild_document_panels(cx: &mut TestAppContext) {
        use crate::panels::ViewerFrame;

        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let probe = observer_probe(&project, cx);
        cx.run_until_parked();
        let baseline = probe.read_with(cx, |probe, _| probe.rebuilds);

        for generation in 1..=3 {
            project.update(cx, |project, cx| {
                project.on_eval_update(
                    EvalUpdate {
                        generation,
                        frame: generation,
                        node: NodeId::new(1),
                        result: Ok(Arc::new(FrameBuffer::new_zeroed(4, 4))),
                        timings: Vec::new(),
                    },
                    cx,
                );
            });
        }
        cx.run_until_parked();

        // The frames did arrive — this is not a test that nothing happened.
        assert!(matches!(
            cx.update(|cx| cx.try_global::<ViewerFrame>().cloned()),
            Some(ViewerFrame::Frame { .. })
        ));
        assert_eq!(
            probe.read_with(cx, |probe, _| probe.rebuilds),
            baseline,
            "evaluation results must not notify document-mirroring observers"
        );

        // The probe is wired: a real document change still reaches it.
        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let document =
                ravel_ui::document::add_layer(project.document(), comp, content_layer()).unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();
        assert!(
            probe.read_with(cx, |probe, _| probe.rebuilds) > baseline,
            "a document edit must still notify observers"
        );
    }

    /// Per-node timings are the one evaluation output a panel reads outside
    /// `ViewerFrame` (the Node Editor load readout). They are published as a
    /// global for every arriving result, including one dropped as stale, so
    /// dropping the entity notify cannot stall the readout.
    #[gpui::test]
    fn timings_publish_even_for_a_dropped_stale_result(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let update = |generation, node: u64, micros| EvalUpdate {
            generation,
            frame: 0,
            node: NodeId::new(node),
            result: Ok(Arc::new(FrameBuffer::new_zeroed(4, 4))),
            timings: vec![(NodeId::new(node), std::time::Duration::from_micros(micros))],
        };

        project.update(cx, |project, cx| {
            project.on_eval_update(update(2, 1, 500), cx)
        });
        // Generation 1 is older than the published 2, so its frame is dropped.
        project.update(cx, |project, cx| {
            project.on_eval_update(update(1, 7, 900), cx)
        });

        cx.update(|cx| {
            let timings = cx.try_global::<NodeEvalTimings>().expect("timings global");
            assert_eq!(
                timings.0.get(&NodeId::new(1)).copied(),
                Some(std::time::Duration::from_micros(500))
            );
            assert_eq!(
                timings.0.get(&NodeId::new(7)).copied(),
                Some(std::time::Duration::from_micros(900)),
                "a dropped result still contributes its timings"
            );
        });
    }

    #[gpui::test]
    fn dirty_tracks_edits_save_completion_and_new_baseline(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        assert!(!project.read_with(cx, |project, _| project.is_dirty()));

        let dir = std::env::temp_dir().join(format!("ravel_dirty_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dirty.ravprj");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(crate::project::container::backup_path(&path));

        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let document =
                ravel_ui::document::add_layer(project.document(), comp, content_layer()).unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);
            assert!(project.is_dirty());
            project.save_project_to(path.clone(), cx);
            // A request alone is not a completed save.
            assert!(project.is_dirty());
        });
        cx.run_until_parked();
        assert!(!project.read_with(cx, |project, _| project.is_dirty()));

        project.update(cx, |project, cx| project.new_document(cx));
        assert!(!project.read_with(cx, |project, _| project.is_dirty()));

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(crate::project::container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    #[gpui::test]
    fn edit_after_save_request_remains_dirty(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_stale_save_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stale.ravprj");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(crate::project::container::backup_path(&path));

        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let one_layer =
                ravel_ui::document::add_layer(project.document(), comp, content_layer()).unwrap();
            project.commit_document(one_layer, InvalidationHint::Structural, cx);
            project.save_project_to(path.clone(), cx);

            let two_layers =
                ravel_ui::document::add_layer(project.document(), comp, content_layer()).unwrap();
            project.commit_document(two_layers, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();

        assert!(project.read_with(cx, |project, _| project.is_dirty()));
        let saved = crate::project::ProjectFile::load(&path).unwrap();
        assert_eq!(
            ravel_ui::document::root_composition(&saved.document)
                .unwrap()
                .layer_count(),
            1,
            "the completed save must retain its request-time snapshot"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(crate::project::container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    #[gpui::test]
    fn guarded_save_callback_waits_behind_an_in_flight_save(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_guard_queue_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.ravprj");
        let guarded = dir.join("guarded.ravprj");
        for path in [&first, &guarded] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(crate::project::container::backup_path(path));
        }

        let outcome = std::rc::Rc::new(std::cell::Cell::new(None));
        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let document =
                ravel_ui::document::add_layer(project.document(), comp, content_layer()).unwrap();
            project.commit_document(document, InvalidationHint::Structural, cx);
            project.save_project_to(first.clone(), cx);
            let callback_outcome = outcome.clone();
            project.save_project_to_then(
                guarded.clone(),
                move |result, _cx| callback_outcome.set(Some(result)),
                cx,
            );
            assert_eq!(outcome.get(), None);
        });
        cx.run_until_parked();

        assert_eq!(outcome.get(), Some(SaveOutcome::Saved));
        assert!(first.exists());
        assert!(guarded.exists());
        assert!(!project.read_with(cx, |project, _| project.is_dirty()));

        for path in [&first, &guarded] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(crate::project::container::backup_path(path));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    /// Save → New → Load restores layers, keyframes, and custom parameters;
    /// loading replaces the undo history wholesale (REQ-LAYER-009).
    #[gpui::test]
    fn save_new_load_roundtrips_the_document(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_project_state_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.ravprj");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(crate::project::container::backup_path(&path));

        // Commit content, then save (the write completes on the test
        // dispatcher).
        let saved = project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let doc = ravel_ui::document::add_layer(project.document(), comp, content_layer())
                .expect("add layer");
            project.commit_document(doc, InvalidationHint::Structural, cx);
            project.save_project_to(path.clone(), cx);
            project.document().clone()
        });
        cx.run_until_parked();
        project.read_with(cx, |project, _| {
            assert_eq!(project.project_path(), Some(path.as_path()));
            assert!(!project.is_dirty());
        });

        // File ▸ New: default document, cleared path, fresh undo history.
        project.update(cx, |project, cx| {
            project.new_document(cx);
            assert!(project.project_path().is_none());
            assert!(!project.is_dirty());
            assert!(!project.undo(cx), "a new document has no undo history");
            assert_eq!(project.active_composition(cx).unwrap().layer_count(), 0);
        });

        // File ▸ Open: the saved content is restored exactly.
        project.update(cx, |project, cx| {
            project.load_project_from(path.clone(), cx);
        });
        cx.run_until_parked();
        project.update(cx, |project, cx| {
            assert_eq!(project.document(), &saved);
            assert_eq!(project.project_path(), Some(path.as_path()));
            assert!(!project.is_dirty());
            assert!(!project.undo(cx), "loading is not an undo step");
        });

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(crate::project::container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    /// A failed load keeps the current document and path untouched.
    #[gpui::test]
    fn failed_load_keeps_the_current_document(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);
        let before = project.read_with(cx, |project, _| project.document().clone());

        let missing = std::env::temp_dir().join("ravel_definitely_missing_12345.ravprj");
        let _ = std::fs::remove_file(&missing);
        project.update(cx, |project, cx| {
            project.load_project_from(missing, cx);
        });
        cx.run_until_parked();

        project.read_with(cx, |project, _| {
            assert_eq!(project.document(), &before);
            assert!(project.project_path().is_none());
        });
    }

    /// A save whose write finishes after File ▸ New must not adopt its path
    /// onto the fresh document (the path describes different content).
    #[gpui::test]
    fn save_completing_after_new_does_not_adopt_the_path(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_project_race_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("race.ravprj");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(crate::project::container::backup_path(&path));

        project.update(cx, |project, cx| {
            project.save_project_to(path.clone(), cx);
            // New replaces the document identity before the write lands.
            project.new_document(cx);
        });
        cx.run_until_parked();

        project.read_with(cx, |project, _| {
            assert!(project.project_path().is_none());
        });

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(crate::project::container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    /// A load whose read finishes after an intervening edit is discarded
    /// rather than silently dropping the user's edit.
    #[gpui::test]
    fn load_completing_after_an_edit_is_discarded(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_load_race_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("load_race.ravprj");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(crate::project::container::backup_path(&path));

        // Save a document with one layer, then start over.
        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let doc = ravel_ui::document::add_layer(project.document(), comp, content_layer())
                .expect("add layer");
            project.commit_document(doc, InvalidationHint::Structural, cx);
            project.save_project_to(path.clone(), cx);
        });
        cx.run_until_parked();
        project.update(cx, |project, cx| project.new_document(cx));

        // Start loading, then edit before the read completes.
        project.update(cx, |project, cx| {
            project.load_project_from(path.clone(), cx);
            let comp = project.document().root_comp.expect("root comp");
            let doc = ravel_ui::document::add_layer(project.document(), comp, content_layer())
                .expect("add layer");
            project.commit_document(doc, InvalidationHint::Structural, cx);
        });
        cx.run_until_parked();

        project.read_with(cx, |project, cx| {
            // The edit survived; the in-flight load was discarded.
            assert!(project.project_path().is_none());
            assert_eq!(project.active_composition(cx).unwrap().layer_count(), 1);
        });

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(crate::project::container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    /// A save requested while another is in flight is queued, not run
    /// concurrently: both files are written in order and the final adopted
    /// path is the last request's.
    #[gpui::test]
    fn concurrent_saves_are_serialized(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_save_queue_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.ravprj");
        let second = dir.join("second.ravprj");
        for path in [&first, &second] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(crate::project::container::backup_path(path));
        }

        project.update(cx, |project, cx| {
            project.save_project_to(first.clone(), cx);
            project.save_project_to(second.clone(), cx);
        });
        cx.run_until_parked();

        project.read_with(cx, |project, _| {
            assert_eq!(project.project_path(), Some(second.as_path()));
        });
        assert!(first.exists());
        assert!(second.exists());

        for path in [&first, &second] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(crate::project::container::backup_path(path));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    /// A queued save writes the document snapshot from request time: a New
    /// between request and execution must neither change what the queued
    /// save writes nor let it adopt the path.
    #[gpui::test]
    fn queued_save_uses_the_request_time_snapshot(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_save_snap_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.ravprj");
        let second = dir.join("second.ravprj");
        for path in [&first, &second] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(crate::project::container::backup_path(path));
        }

        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let doc = ravel_ui::document::add_layer(project.document(), comp, content_layer())
                .expect("add layer");
            project.commit_document(doc, InvalidationHint::Structural, cx);
            // A runs; B is queued. Both snapshot the one-layer document.
            project.save_project_to(first.clone(), cx);
            project.save_project_to(second.clone(), cx);
            // New replaces the document before B executes.
            project.new_document(cx);
        });
        cx.run_until_parked();

        // B wrote the request-time snapshot (one layer), not the empty
        // replacement document; neither save adopted its path.
        project.read_with(cx, |project, _| {
            assert!(project.project_path().is_none());
        });
        let loaded_b = crate::project::ProjectFile::load(&second).unwrap();
        let root_b =
            ravel_ui::document::root_composition(&loaded_b.document).expect("root comp in B");
        assert_eq!(root_b.layer_count(), 1, "B must contain the old document");

        for path in [&first, &second] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(crate::project::container::backup_path(path));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    /// Three rapid saves queue all destinations; none is silently dropped.
    #[gpui::test]
    fn a_third_save_does_not_overwrite_the_second(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_save_third_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<_> = ["one.ravprj", "two.ravprj", "three.ravprj"]
            .iter()
            .map(|name| dir.join(name))
            .collect();
        for path in &paths {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(crate::project::container::backup_path(path));
        }

        project.update(cx, |project, cx| {
            for path in &paths {
                project.save_project_to(path.clone(), cx);
            }
        });
        cx.run_until_parked();

        for path in &paths {
            assert!(path.exists(), "{} was written", path.display());
        }
        project.read_with(cx, |project, _| {
            assert_eq!(project.project_path(), Some(paths[2].as_path()));
        });

        for path in &paths {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(crate::project::container::backup_path(path));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    /// Overlapping File ▸ Open requests resolve latest-wins: the earlier
    /// load is discarded even if it completes first.
    #[gpui::test]
    fn overlapping_loads_resolve_latest_wins(cx: &mut TestAppContext) {
        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let dir = std::env::temp_dir().join(format!("ravel_load_wins_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path_a = dir.join("a.ravprj");
        let path_b = dir.join("b.ravprj");
        for path in [&path_a, &path_b] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(crate::project::container::backup_path(path));
        }

        // File A: one layer. File B: two layers.
        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let doc = ravel_ui::document::add_layer(project.document(), comp, content_layer())
                .expect("add layer");
            project.commit_document(doc, InvalidationHint::Structural, cx);
            project.save_project_to(path_a.clone(), cx);
        });
        cx.run_until_parked();
        project.update(cx, |project, cx| {
            let comp = project.document().root_comp.expect("root comp");
            let doc = ravel_ui::document::add_layer(project.document(), comp, content_layer())
                .expect("add layer");
            project.commit_document(doc, InvalidationHint::Structural, cx);
            project.save_project_to(path_b.clone(), cx);
        });
        cx.run_until_parked();
        project.update(cx, |project, cx| project.new_document(cx));

        // Request A, then B, before either completes.
        project.update(cx, |project, cx| {
            project.load_project_from(path_a.clone(), cx);
            project.load_project_from(path_b.clone(), cx);
        });
        cx.run_until_parked();

        project.read_with(cx, |project, cx| {
            assert_eq!(project.project_path(), Some(path_b.as_path()));
            assert_eq!(project.active_composition(cx).unwrap().layer_count(), 2);
        });

        for path in [&path_a, &path_b] {
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(crate::project::container::backup_path(path));
        }
        let _ = std::fs::remove_dir(&dir);
    }

    /// Reproduces the stale-viewer bug: a structural edit (Geometry node
    /// deleted upstream of a Rasterize) makes the evaluation fail, and the
    /// error must replace the previously shown frame instead of leaving it
    /// on screen; a later successful evaluation restores normal drawing.
    #[gpui::test]
    fn eval_error_replaces_the_frame_and_recovers(cx: &mut TestAppContext) {
        use crate::panels::ViewerFrame;

        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        let ok_update = |generation| EvalUpdate {
            generation,
            frame: 0,
            node: NodeId::new(1),
            result: Ok(Arc::new(FrameBuffer::new_zeroed(4, 4))),
            timings: Vec::new(),
        };
        let err_update = |generation| EvalUpdate {
            generation,
            frame: 0,
            node: NodeId::new(1),
            result: Err(ravel_core::eval::EvalError::NodeNotFound(NodeId::new(42))),
            timings: Vec::new(),
        };

        // A good frame is published.
        project.update(cx, |project, cx| project.on_eval_update(ok_update(1), cx));
        project.read_with(cx, |project, cx| match cx.try_global::<ViewerFrame>() {
            Some(ViewerFrame::Frame {
                buffer,
                composition_resolution,
            }) => {
                assert_eq!((buffer.width, buffer.height), (4, 4));
                assert_eq!(
                    *composition_resolution,
                    project.active_composition(cx).unwrap().resolution
                );
            }
            other => panic!("expected a published frame, got {other:?}"),
        });

        // The structural edit makes the evaluation fail: the error replaces
        // the frame (before the fix, the stale frame was kept).
        project.update(cx, |project, cx| project.on_eval_update(err_update(2), cx));
        project.read_with(cx, |project, cx| match cx.try_global::<ViewerFrame>() {
            Some(ViewerFrame::Error {
                message,
                composition_resolution,
            }) => {
                assert!(message.contains("42"), "unexpected message: {message}");
                // The error carries the full composition resolution so the
                // panel can share normal output's viewport geometry.
                let comp = project.active_composition(cx).expect("root comp");
                assert_eq!(*composition_resolution, Some(comp.resolution));
            }
            other => panic!("expected an error state, got {other:?}"),
        });

        // Fixing the graph restores normal drawing.
        project.update(cx, |project, cx| project.on_eval_update(ok_update(3), cx));
        project.read_with(cx, |_, cx| match cx.try_global::<ViewerFrame>() {
            Some(ViewerFrame::Frame { .. }) => {}
            other => panic!("expected recovery to a frame, got {other:?}"),
        });
    }

    /// A non-frame (e.g. Geometry) successful output blanks the viewer.
    #[gpui::test]
    fn non_frame_output_blanks_the_viewer(cx: &mut TestAppContext) {
        use crate::panels::ViewerFrame;

        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        project.update(cx, |project, cx| {
            project.on_eval_update(
                EvalUpdate {
                    generation: 1,
                    frame: 0,
                    node: NodeId::new(1),
                    result: Ok(Arc::new(ravel_core::types::Scalar(1.0))),
                    timings: Vec::new(),
                },
                cx,
            );
        });
        project.read_with(cx, |project, cx| {
            let expected = project.active_composition(cx).unwrap().resolution;
            assert!(matches!(
                cx.try_global::<ViewerFrame>(),
                Some(ViewerFrame::Blank {
                    composition_resolution: Some(resolution),
                }) if *resolution == expected
            ));
        });
    }

    /// Monotonic publication (generation-starvation regression): a result
    /// that is newer than the displayed one is published even when it is no
    /// longer the very latest request, and an older in-flight result can
    /// never overwrite a newer one.
    #[gpui::test]
    fn newer_results_publish_and_older_ones_never_regress(cx: &mut TestAppContext) {
        use crate::panels::ViewerFrame;

        disable_background_eval_for_tests();
        let project = cx.new(ProjectState::new);

        // Distinguish generations by frame size.
        let update = |generation, size| EvalUpdate {
            generation,
            frame: 0,
            node: NodeId::new(1),
            result: Ok(Arc::new(FrameBuffer::new_zeroed(size, size))),
            timings: Vec::new(),
        };
        let shown_size = |cx: &mut TestAppContext| {
            project.read_with(cx, |_, cx| match cx.try_global::<ViewerFrame>() {
                Some(ViewerFrame::Frame { buffer, .. }) => buffer.width,
                other => panic!("expected a frame, got {other:?}"),
            })
        };

        project.update(cx, |project, cx| project.on_eval_update(update(1, 1), cx));
        assert_eq!(shown_size(cx), 1);

        // Under load the coalescing window has moved on (a newer request
        // exists), but this completed result is still newer than what is on
        // screen: it must be published, not starved.
        project.update(cx, |project, cx| project.on_eval_update(update(3, 3), cx));
        assert_eq!(shown_size(cx), 3);

        // An older in-flight result arriving late must not move the display
        // backwards.
        project.update(cx, |project, cx| project.on_eval_update(update(2, 2), cx));
        assert_eq!(shown_size(cx), 3);
    }
}
