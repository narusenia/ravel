// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPU-backed [`EvalWorkerHooks`] implementation for the background
//! evaluation service.
//!
//! Owns the `GpuContext` and `ShaderManager` on the worker thread so every
//! wgpu queue submission of the evaluation path happens off the UI thread
//! and on a single thread (no queue contention with GPUI's renderer, which
//! uses its own device).

use ravel_core::composition::Document;
use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor as _};
use ravel_core::geometry::Geometry;
use ravel_core::graph::{Graph, Node};
use ravel_core::id::NodeId;
use ravel_core::runtime::{EvalWorkerHooks, InvalidationHint};
use ravel_core::types::NodeData;
use ravel_gpu::{GpuContext, GpuFrameBuffer, ShaderManager, TexturePool};
use std::sync::{Arc, Mutex};

pub struct GpuEvalHooks {
    gpu: GpuContext,
    shaders: ShaderManager,
    pool: Arc<Mutex<TexturePool>>,
}

impl GpuEvalHooks {
    pub fn new(gpu: GpuContext) -> Self {
        let shaders = ShaderManager::new(gpu.clone());
        let pool = ravel_nodes::shared_texture_pool(&gpu);
        Self { gpu, shaders, pool }
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

impl EvalWorkerHooks for GpuEvalHooks {
    fn sync(
        &mut self,
        evaluator: &mut Evaluator,
        graph: &Graph,
        document: Option<&Document>,
        hint: &InvalidationHint,
    ) {
        match hint {
            InvalidationHint::None => {}
            InvalidationHint::Params(ids) => {
                for id in ids {
                    if let Some(node) = find_node(graph, document, *id)
                        && let Some(proc) = ravel_nodes::processor_for_node(
                            &node,
                            &self.gpu,
                            &mut self.shaders,
                            &self.pool,
                        )
                    {
                        evaluator.register(*id, proc);
                    }
                }
            }
            InvalidationHint::Structural => {
                *evaluator = Evaluator::new();
                ravel_nodes::register_all_processors(
                    evaluator,
                    graph,
                    &self.gpu,
                    &mut self.shaders,
                    &self.pool,
                );
                // Layer networks are evaluated through the document, not the
                // requested graph — register their processors too
                // (register_all_processors recurses into subnets).
                if let Some(document) = document {
                    for comp in document.compositions.values() {
                        for layer in &comp.layers {
                            ravel_nodes::register_all_processors(
                                evaluator,
                                &layer.network,
                                &self.gpu,
                                &mut self.shaders,
                                &self.pool,
                            );
                        }
                    }
                }
            }
        }
    }

    /// Adapts evaluation outputs for the Viewer boundary: GPU-resident
    /// frames are read back exactly once here (the only readback in the
    /// chain until Phase 4 moves display to the GPU), and `Geometry`
    /// outputs are rasterized with the same ad-hoc parameters the
    /// NodeEditor previously used on the UI thread.
    fn finalize(&mut self, value: Arc<dyn NodeData>, ctx: &EvalContext) -> Arc<dyn NodeData> {
        if let Some(frame) = value.downcast_ref::<GpuFrameBuffer>() {
            return match frame.to_frame_buffer() {
                Ok(fb) => Arc::new(fb),
                Err(err) => {
                    tracing::warn!(%err, "viewer readback failed");
                    value
                }
            };
        }
        if value.downcast_ref::<Geometry>().is_none() {
            return value;
        }
        let rast_node = ravel_core::graph::Node::new(NodeId::new(u64::MAX), "rasterize")
            .with_param("fill", ravel_core::graph::ParameterValue::Bool(true))
            .with_param(
                "stroke_width",
                ravel_core::graph::ParameterValue::Float(0.0),
            );
        let proc = ravel_nodes::rasterize::RasterizeProcessor::from_node(&rast_node);
        let inputs: Vec<Option<Arc<dyn NodeData>>> = vec![Some(value.clone())];
        let mut scope = ravel_core::eval::Evaluator::new();
        match proc.process(
            &rast_node,
            ctx,
            &inputs,
            &ravel_core::eval::ResolvedParams::default(),
            &mut scope,
        ) {
            Ok(fb) => fb,
            Err(err) => {
                tracing::warn!(%err, "viewer rasterize failed; passing geometry through");
                value
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::registry::NodeRegistry;
    use ravel_core::registry::builtin::register_builtins;
    use ravel_core::types::{FrameBuffer, FrameRate};

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (32, 32))
    }

    #[test]
    fn finalize_rasterizes_geometry_output() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut hooks = GpuEvalHooks::new(gpu);

        let geo = Geometry::from_points(vec![
            ravel_core::types::Vec2(0.0, 0.0),
            ravel_core::types::Vec2(10.0, 0.0),
            ravel_core::types::Vec2(10.0, 10.0),
        ]);
        let out = hooks.finalize(Arc::new(geo), &ctx());
        assert!(out.downcast_ref::<FrameBuffer>().is_some());
    }

    #[test]
    fn finalize_reads_back_gpu_frames_for_the_viewer() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut hooks = GpuEvalHooks::new(gpu.clone());

        let pool = ravel_nodes::shared_texture_pool(&gpu);
        let cpu = FrameBuffer {
            width: 4,
            height: 4,
            data: Arc::from(vec![0.5f32; 4 * 4 * 4]),
        };
        let frame = GpuFrameBuffer::from_frame_buffer(gpu, &pool, &cpu);

        let out = hooks.finalize(Arc::new(frame), &ctx());
        let fb = out
            .downcast_ref::<FrameBuffer>()
            .expect("viewer boundary yields a CPU frame");
        assert!((fb.data[0] - 0.5).abs() < 1e-6);
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
            &mut evaluator,
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
            &mut evaluator,
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
}
