// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPU-backed [`EvalWorkerHooks`] implementation for the evaluation workers.
//!
//! Owns the `GpuContext` and `ShaderManager` on the worker thread so every
//! wgpu queue submission of the evaluation path happens off the UI thread
//! and on a single thread (no queue contention with GPUI's renderer, which
//! uses its own device).
//!
//! It lives here rather than in the GPUI host because it names nothing but
//! `ravel-core`, `ravel-gpu` and this crate's own registration entry points,
//! and because **both** workers need it: the interactive
//! [`EvalService`](ravel_core::runtime::EvalService) the application spawns
//! and the [`RenderQueue`](ravel_core::runtime::RenderQueue) the headless
//! `ravel-cli` spawns. A hooks implementation that only a GUI binary could
//! reach would make "the CLI renders through the same worker" impossible to
//! hold (`docs/implementation/render-export-plan.md`).

use ravel_core::cache_budget::SharedCacheBudget;
use ravel_core::composition::Document;
use ravel_core::eval::{EvalContext, NodeProcessor as _, ProcessorRegistry as _};
use ravel_core::geometry::Geometry;
use ravel_core::graph::{Graph, Node};
use ravel_core::id::NodeId;
use ravel_core::runtime::{EvalWorkerHooks, InvalidationHint, ProcessorSync};
use ravel_core::types::NodeData;
use ravel_gpu::{GpuContext, GpuDeviceState, GpuFrameBuffer, ShaderManager, TexturePool};

use crate::display::DisplayTransform;
use ravel_media::frame_cache::MediaFrameCache;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, Mutex};

pub struct GpuEvalHooks {
    gpu: GpuContext,
    shaders: ShaderManager,
    pool: Arc<Mutex<TexturePool>>,
    /// The viewer's display transform (`CM-7`), present only when the host
    /// asked for one. A render must not have it: the export path encodes with
    /// `to_output_space` while the frame is still float, and it has no
    /// business inheriting the viewer's display LUT.
    display: Option<DisplayTransform>,
    /// One decode cache for the whole worker, so every `media` node it
    /// registers shares it (`CACHE-8`). Built here for the same reason the
    /// texture pool is: the processors are constructed from these hooks.
    media_frames: MediaFrameCache,
    /// The rasterizer [`Self::finalize`] draws `Geometry` outputs with, built
    /// on first use and kept: it owns a compiled shader and a render pipeline,
    /// which is not work to redo per frame of a scrub.
    viewer_rasterize: Option<crate::rasterize::RasterizeProcessor>,
}

impl GpuEvalHooks {
    /// Hooks with a standalone texture pool and decode cache (each with a
    /// fixed budget of its own). For tests and any host without a process
    /// cache budget.
    pub fn new(gpu: GpuContext) -> Self {
        let shaders = ShaderManager::new(gpu.clone());
        let pool = crate::shared_texture_pool(&gpu);
        Self {
            gpu,
            shaders,
            pool,
            display: None,
            media_frames: MediaFrameCache::standalone(),
            viewer_rasterize: None,
        }
    }

    /// Hooks whose texture pool and decode cache answer to `budget`.
    ///
    /// Both are built here, before the evaluation worker exists, so the
    /// budget has to reach this call and `EvalService::spawn_with_budget`
    /// from the same place — see `ProjectState::new`.
    pub fn with_budget(gpu: GpuContext, budget: SharedCacheBudget) -> Self {
        let shaders = ShaderManager::new(gpu.clone());
        let pool = crate::shared_texture_pool_with_budget(&gpu, budget.clone());
        Self {
            gpu,
            shaders,
            pool,
            display: None,
            media_frames: MediaFrameCache::new(budget),
            viewer_rasterize: None,
        }
    }

    /// The shared device state observed by this worker's GPU resources.
    #[inline]
    pub fn device_state(&self) -> GpuDeviceState {
        self.gpu.device_state()
    }

    /// The texture pool every processor this worker registers shares.
    ///
    /// Exposed so a caller can observe what the worker is holding —
    /// `TexturePool::total_created` is how `tests/upload_memo.rs` catches
    /// leases that pile up across frames instead of circulating.
    #[inline]
    pub fn texture_pool(&self) -> &Arc<Mutex<TexturePool>> {
        &self.pool
    }

    /// Finish frames for a screen rather than for a file: [`Self::finalize`]
    /// then yields a [`DisplayFrame`](crate::DisplayFrame) instead of a linear
    /// [`FrameBuffer`](ravel_core::types::FrameBuffer).
    ///
    /// The interactive viewer opts in; the export worker and `ravel-cli` do
    /// not (`CM-7`).
    ///
    /// Compiles nothing: the shader is validated and its pipeline created on
    /// the first frame, which happens on the evaluation worker. A host calls
    /// this from wherever it builds its hooks — `ProjectState::new` runs on
    /// the UI thread, and shader validation plus pipeline creation is not
    /// work that belongs there.
    pub fn with_display_transform(mut self) -> Self {
        self.display = Some(DisplayTransform::new());
        self
    }

    /// Install the viewer display transform with a host-controlled output
    /// mode. The flag is shared because the hooks run on the evaluation
    /// worker while the GPUI device capability is discovered by the host.
    ///
    /// Order-independent with respect to [`Self::with_display_channel`]:
    /// whichever comes second keeps what the first installed.
    pub fn with_display_surface_mode(mut self, zero_copy_surface: Arc<AtomicBool>) -> Self {
        self.display = Some(
            self.display
                .take()
                .unwrap_or_default()
                .with_surface_cell(zero_copy_surface),
        );
        self
    }

    /// Install the viewer display transform with a host-controlled display
    /// channel (`INSP-2`).
    ///
    /// Order-independent with respect to [`Self::with_display_surface_mode`]:
    /// whichever comes second keeps what the first installed, so a host can
    /// name the two capabilities in either order without silently losing one.
    pub fn with_display_channel(mut self, channel: Arc<AtomicU32>) -> Self {
        self.display = Some(
            self.display
                .take()
                .unwrap_or_default()
                .with_channel(channel),
        );
        self
    }

    /// Install (or clear) the user's display LUT. `None` restores the built-in
    /// transfer function. No-op on hooks with no display transform.
    pub fn set_display_lut(&mut self, lut: Option<ravel_core::color::CubeLut>) {
        if let Some(display) = &mut self.display {
            display.set_lut(lut);
        }
    }
}

/// Find `id` in `graph` or any nested subnet graph (depth-first).
fn find_node_recursive(graph: &Graph, id: NodeId) -> Option<Arc<Node>> {
    if let Some(node) = graph.node(id) {
        return Some(node.clone());
    }
    graph
        .nodes()
        .filter_map(|n| n.subnet.as_ref())
        .find_map(|inner| find_node_recursive(inner, id))
}

/// Find `id` in `graph` or in any layer network of `document` (subnets
/// included) — parameter edits may target nodes that live outside the
/// requested graph (e.g. an In-node custom parameter edited from the
/// Properties panel while the node editor shows another network).
fn find_node(graph: &Graph, document: Option<&Document>, id: NodeId) -> Option<Arc<Node>> {
    if let Some(node) = find_node_recursive(graph, id) {
        return Some(node);
    }
    let document = document?;
    document.compositions.values().find_map(|comp| {
        comp.layers
            .iter()
            .find_map(|layer| find_node_recursive(&layer.network, id))
    })
}

/// The decode cache lives here rather than in the evaluator, so this is
/// where it joins the worker's eviction settling (`CACHE-8`).
///
/// `drop_evicted` parks what the cache does not own and
/// `take_foreign_evictions` returns exactly that plus anything parked
/// earlier — which is the contract this method states, with no bookkeeping
/// of its own.
impl EvalWorkerHooks for GpuEvalHooks {
    fn reconcile_evictions(
        &mut self,
        evicted: Vec<ravel_core::cache_budget::Evicted>,
    ) -> Vec<ravel_core::cache_budget::Evicted> {
        self.media_frames.drop_evicted(&evicted);
        self.media_frames.take_foreign_evictions()
    }

    fn sync(
        &mut self,
        evaluator: &mut ProcessorSync<'_>,
        graph: &Graph,
        document: Option<&Document>,
        hint: &InvalidationHint,
    ) {
        // Opens the first upload scope of this worker's life. `finalize`
        // rotates it per evaluation from then on — `sync` cannot, because a
        // render job syncs once and then evaluates every frame of the range.
        crate::gpu_util::begin_upload_scope(&self.pool);
        match hint {
            InvalidationHint::None => {}
            InvalidationHint::Params(ids) => {
                for id in ids {
                    // A processor that reads everything from the node and
                    // params handed to `process` holds nothing stale, so the
                    // edit only needs its cached values dropped. Asking the
                    // registration that already exists keeps this correct by
                    // default: `rebuild_on_node_change` is `true` unless a
                    // processor opts out, so an unknown node type still gets
                    // rebuilt. For the GPU processors the rebuild it skips is a
                    // shader recompile plus a pipeline creation per edit tick.
                    if evaluator
                        .processor(*id)
                        .is_some_and(|proc| !proc.rebuild_on_node_change())
                    {
                        evaluator.invalidate_node(*id);
                        continue;
                    }
                    if let Some(node) = find_node(graph, document, *id)
                        && let Some(proc) = crate::processor_for_node(
                            &node,
                            &self.gpu,
                            &mut self.shaders,
                            &self.pool,
                            &self.media_frames,
                        )
                    {
                        evaluator.register(*id, proc);
                    }
                }
            }
            InvalidationHint::Structural => {
                // The evaluator arrives already reset: `EvalService` clears
                // it for a structural hint, which is what keeps the cache
                // budget (state the service owns) across the resync.
                crate::register_all_processors(
                    evaluator,
                    graph,
                    &self.gpu,
                    &mut self.shaders,
                    &self.pool,
                    &self.media_frames,
                );
                // Layer networks are evaluated through the document, not the
                // requested graph — register their processors too
                // (register_all_processors recurses into subnets).
                if let Some(document) = document {
                    for comp in document.compositions.values() {
                        for layer in &comp.layers {
                            crate::register_all_processors(
                                evaluator,
                                &layer.network,
                                &self.gpu,
                                &mut self.shaders,
                                &self.pool,
                                &self.media_frames,
                            );
                        }
                    }
                }
            }
        }
    }

    /// Adapts evaluation outputs for the Viewer boundary: `Geometry` outputs
    /// are rasterized with the same ad-hoc parameters the NodeEditor
    /// previously used on the UI thread, and the resulting frame leaves the
    /// GPU exactly once — as display bytes when a display transform is
    /// installed (`CM-7`), otherwise as the linear frame the render exits
    /// encode themselves.
    ///
    /// A failure returns `None` rather than the untouched input: the caller
    /// still shows that value, but must not cache it, or one lost readback
    /// would be served back on every later hit and blank the viewer for good.
    /// The host reads an un-finalized frame as "the display transform did not
    /// run" and surfaces it as an error rather than drawing linear light.
    fn finalize(
        &mut self,
        value: &Arc<dyn NodeData>,
        ctx: &EvalContext,
    ) -> Option<Arc<dyn NodeData>> {
        // One evaluation has just produced `value`, so its uploads are spent:
        // close that scope and open the next one (MED-GPU-05). This is the
        // boundary rather than `sync` because it is the only hook both
        // workers run *per evaluation* — the interactive service syncs per
        // request, but a render job syncs once and then walks a whole frame
        // range, so a scope anchored to `sync` would hold every frame's
        // source texture until the export finished. Rotating here, before
        // anything below can upload, also keeps the display transform's own
        // upload inside a scope.
        crate::gpu_util::begin_upload_scope(&self.pool);
        let value = self.rasterize_geometry(value, ctx)?;
        // Frames only: a `Scalar` target has nothing to display and passes
        // straight through.
        if self.display.is_some() && crate::gpu_util::frame_size(value.as_ref()).is_some() {
            // Split borrows: the transform compiles its pipeline through the
            // shader manager the hooks own, and both live on this worker.
            // `media_frames` is named rather than elided with `..` so that a
            // field added later is a compile error here again — the split
            // borrow has to be revisited whenever this struct grows.
            let Self {
                gpu,
                shaders,
                pool,
                display,
                media_frames: _,
                viewer_rasterize: _,
            } = self;
            let display = display.as_mut().expect("checked above");
            return match display.run(gpu, shaders, pool, value.as_ref()) {
                Ok(frame) => Some(Arc::new(frame)),
                // No CPU rescue: a second implementation of the transform is
                // exactly what `CM-7` removed. The host shows the error.
                Err(err) => {
                    tracing::warn!(%err, "viewer display transform failed");
                    None
                }
            };
        }
        if let Some(frame) = value.downcast_ref::<GpuFrameBuffer>() {
            return match frame.to_frame_buffer() {
                Ok(fb) => Some(Arc::new(fb)),
                Err(err) => {
                    tracing::warn!(%err, "viewer readback failed");
                    None
                }
            };
        }
        Some(value)
    }
}

impl GpuEvalHooks {
    /// Rasterize a `Geometry` output so the viewer has something to draw.
    /// Any other value passes through untouched; a failed rasterization is
    /// `None`, which keeps it out of the frame cache.
    ///
    /// On the GPU, with the context and pool these hooks already hold: a
    /// shape or scatter node previewed while scrubbing used to run the zeno
    /// CPU rasterizer once per frame, and its result then had to be uploaded
    /// again by the display transform (issue MED-GPU-04). The resident frame
    /// this produces feeds the transform directly.
    fn rasterize_geometry(
        &mut self,
        value: &Arc<dyn NodeData>,
        ctx: &EvalContext,
    ) -> Option<Arc<dyn NodeData>> {
        if value.downcast_ref::<Geometry>().is_none() {
            return Some(value.clone());
        }
        // Ad-hoc parameters: the processor reads fill and stroke width from
        // the resolved parameters below, not from this node, so the node
        // exists only to satisfy the signature.
        let rast_node = ravel_core::graph::Node::new(NodeId::new(u64::MAX), "rasterize")
            .with_param("fill", ravel_core::graph::ParameterValue::Bool(true))
            .with_param(
                "stroke_width",
                ravel_core::graph::ParameterValue::Float(0.0),
            );
        // Split borrows: building the rasterizer compiles through the shader
        // manager while the slot it lands in is borrowed mutably. Every field
        // is named for the reason `finalize` names them — a field added later
        // has to be considered here too.
        let Self {
            gpu,
            shaders,
            pool,
            display: _,
            media_frames: _,
            viewer_rasterize,
        } = self;
        let proc = viewer_rasterize.get_or_insert_with(|| {
            crate::rasterize::RasterizeProcessor::new(
                gpu.clone(),
                shaders,
                pool.clone(),
                &rast_node,
            )
        });
        let inputs: Vec<Option<Arc<dyn NodeData>>> = vec![Some(value.clone())];
        let mut scope = ravel_core::eval::Evaluator::new();
        match proc.process(
            &rast_node,
            ctx,
            &inputs,
            &ravel_core::eval::ResolvedParams::default(),
            &mut scope,
        ) {
            Ok(fb) => Some(fb),
            Err(err) => {
                tracing::warn!(%err, "viewer rasterize failed");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::registry::NodeRegistry;
    use ravel_core::registry::builtin::register_builtins;
    use ravel_core::types::{FrameBuffer, FrameRate};

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (32, 32))
    }

    /// Both host capabilities survive, whichever order they are named in.
    ///
    /// Each installer used to build a fresh [`DisplayTransform`], so naming
    /// the second one threw the first away — a viewer stuck in RGB, or one
    /// that never took the zero-copy surface, depending on the order the host
    /// happened to write (`INSP-2`). Neither shows up as a failure anywhere
    /// else: both fall back to a *working* transform, just not the one the
    /// host asked for.
    #[test]
    fn the_display_capabilities_do_not_overwrite_each_other() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let surface = Arc::new(AtomicBool::new(true));
        let channel = Arc::new(AtomicU32::new(
            ravel_core::color::DisplayChannel::Blue.to_u32(),
        ));

        for hooks in [
            GpuEvalHooks::new(gpu.clone())
                .with_display_surface_mode(surface.clone())
                .with_display_channel(channel.clone()),
            GpuEvalHooks::new(gpu.clone())
                .with_display_channel(channel.clone())
                .with_display_surface_mode(surface.clone()),
        ] {
            let display = hooks.display.as_ref().expect("no display transform");
            assert_eq!(
                display.channel(),
                ravel_core::color::DisplayChannel::Blue,
                "the channel cell was replaced"
            );
            assert!(display.zero_copy_surface(), "the surface cell was replaced");
        }
    }

    /// The viewer boundary rasterizes on the GPU (issue MED-GPU-04): the
    /// recorded pass count has to move, or the zeno CPU path ran instead —
    /// which is invisible in the output, since a viewer without a display
    /// transform reads the frame back either way.
    #[test]
    fn finalize_rasterizes_geometry_output_on_the_gpu() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut hooks = GpuEvalHooks::new(gpu.clone());

        let geo = Geometry::from_points(vec![
            ravel_core::types::Vec2(0.0, 0.0),
            ravel_core::types::Vec2(10.0, 0.0),
            ravel_core::types::Vec2(10.0, 10.0),
        ]);
        let value: Arc<dyn NodeData> = Arc::new(geo);
        let before = gpu.dispatch_stats();
        let out = hooks.finalize(&value, &ctx()).expect("rasterize succeeded");
        let recorded = before.delta(&gpu.dispatch_stats()).dispatches;
        assert!(out.downcast_ref::<FrameBuffer>().is_some());
        assert!(
            recorded >= 2,
            "the draw and the unpremultiply pass must both be recorded, got {recorded}"
        );
    }

    #[test]
    fn finalize_reads_back_gpu_frames_for_the_viewer() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut hooks = GpuEvalHooks::new(gpu.clone());

        let pool = crate::shared_texture_pool(&gpu);
        let cpu = FrameBuffer::from_f32(4, 4, vec![0.5f32; 4 * 4 * 4]);
        let frame = GpuFrameBuffer::from_frame_buffer(gpu, &pool, &cpu).expect("upload");

        let value: Arc<dyn NodeData> = Arc::new(frame);
        let out = hooks.finalize(&value, &ctx()).expect("readback succeeded");
        let fb = out
            .downcast_ref::<FrameBuffer>()
            .expect("viewer boundary yields a CPU frame");
        assert!((fb.as_f32()[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn params_hint_rebuilds_only_listed_nodes() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut hooks = GpuEvalHooks::new(gpu);
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        let node_id = NodeId::new(1);
        let rect_v1 = {
            let mut n = registry.create_node("shape.rect", node_id).unwrap();
            if let Some(p) = n.parameters.iter_mut().find(|p| p.key == "width") {
                p.value = ravel_core::graph::ParameterValue::Float(10.0);
            }
            n
        };
        let graph_v1 = Graph::new().add_node(rect_v1).unwrap();

        use ravel_core::types::GeometricData as _;

        let mut evaluator = Evaluator::new();
        hooks.sync(
            &mut ProcessorSync::new(&mut evaluator),
            &graph_v1,
            None,
            &InvalidationHint::Structural,
        );
        let out_v1 = evaluator.evaluate(&graph_v1, node_id, &ctx()).unwrap();
        let bounds_v1 = out_v1.downcast_ref::<Geometry>().unwrap().bounds();

        // Widen the rect; Params hint must pick up the new parameter.
        let node_v2 = {
            let node = graph_v1.node(node_id).unwrap();
            let mut updated = (**node).clone();
            if let Some(p) = updated.parameters.iter_mut().find(|p| p.key == "width") {
                p.value = ravel_core::graph::ParameterValue::Float(20.0);
            }
            updated
        };
        let graph_v2 = graph_v1.clone().replace_node(Arc::new(node_v2));
        hooks.sync(
            &mut ProcessorSync::new(&mut evaluator),
            &graph_v2,
            None,
            &InvalidationHint::Params(vec![node_id]),
        );
        let out_v2 = evaluator.evaluate(&graph_v2, node_id, &ctx()).unwrap();
        let bounds_v2 = out_v2.downcast_ref::<Geometry>().unwrap().bounds();

        assert!(
            (bounds_v2.width - bounds_v1.width * 2.0).abs() < 1e-3,
            "parameter edit must change the evaluated output: {bounds_v1:?} vs {bounds_v2:?}"
        );
    }

    /// RESP-3 (issue HIGH-06): a parameter edit used to reconstruct the edited
    /// node's processor, and for a GPU node that means recompiling a shader and
    /// creating a compute pipeline — per drag tick. A processor that holds
    /// nothing off its node is invalidated instead of rebuilt; one that captured
    /// node state still is.
    #[test]
    fn params_hint_invalidates_gpu_processors_instead_of_rebuilding_them() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut hooks = GpuEvalHooks::new(gpu);
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        let blur_id = NodeId::new(1);
        let rect_id = NodeId::new(2);
        let graph = Graph::new()
            .add_node(registry.create_node("blur", blur_id).unwrap())
            .unwrap()
            .add_node(registry.create_node("shape.rect", rect_id).unwrap())
            .unwrap();

        let mut evaluator = Evaluator::new();
        hooks.sync(
            &mut ProcessorSync::new(&mut evaluator),
            &graph,
            None,
            &InvalidationHint::Structural,
        );
        let blur_before = evaluator.processor(blur_id).cloned().expect("blur");
        let rect_before = evaluator.processor(rect_id).cloned().expect("rect");

        hooks.sync(
            &mut ProcessorSync::new(&mut evaluator),
            &graph,
            None,
            &InvalidationHint::Params(vec![blur_id, rect_id]),
        );

        let blur_after = evaluator.processor(blur_id).cloned().expect("blur");
        assert!(
            Arc::ptr_eq(&blur_before, &blur_after),
            "a GPU processor must be reused, not rebuilt, on a parameter edit"
        );
        assert!(
            evaluator.is_dirty(blur_id),
            "but its cached value must still be dropped"
        );

        let rect_after = evaluator.processor(rect_id).cloned().expect("rect");
        assert!(
            !Arc::ptr_eq(&rect_before, &rect_after),
            "a processor that captured node state must still be rebuilt"
        );
    }

    /// The skip must not depend on the node being findable: an unregistered node
    /// (or one whose processor was never built) still takes the rebuild path, so
    /// a first parameter edit cannot silently leave a node without a processor.
    #[test]
    fn params_hint_registers_a_node_with_no_processor_yet() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut hooks = GpuEvalHooks::new(gpu);
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        let blur_id = NodeId::new(1);
        let graph = Graph::new()
            .add_node(registry.create_node("blur", blur_id).unwrap())
            .unwrap();

        let mut evaluator = Evaluator::new();
        assert!(evaluator.processor(blur_id).is_none());
        hooks.sync(
            &mut ProcessorSync::new(&mut evaluator),
            &graph,
            None,
            &InvalidationHint::Params(vec![blur_id]),
        );
        assert!(
            evaluator.processor(blur_id).is_some(),
            "a Params hint for an unregistered node must register it"
        );
    }
}
