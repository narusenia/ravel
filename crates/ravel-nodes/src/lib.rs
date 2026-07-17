// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Built-in node processors for the Ravel DAG evaluation pipeline.
//!
//! Each module implements [`ravel_core::eval::NodeProcessor`] for one of the
//! registered built-in node types. GPU-accelerated processors use
//! [`ravel_gpu`] for shader compilation and texture management.

pub mod attribute;
pub mod blur;
pub mod color_correct;
pub mod comp;
pub mod constant;
pub mod field;
mod gpu_util;
pub use gpu_util::{GpuImage, clone_frame_value, ensure_cpu, ensure_gpu};
pub mod layer_ref;
pub mod merge;
pub mod net;
pub mod rasterize;
pub mod scatter;
pub mod shape;
pub mod transform;
pub mod video;

use ravel_core::eval::Evaluator;
use ravel_core::graph::{Graph, Node};
use ravel_gpu::{GpuContext, ShaderManager, TexturePool};
use std::sync::{Arc, Mutex};

/// Register a [`NodeProcessor`] for every node in `graph` whose `type_key`
/// matches a built-in processor.
///
/// Nodes with unrecognized type keys are silently skipped — they may be
/// handled by plugins or user scripts.
pub fn register_all_processors(
    evaluator: &mut Evaluator,
    graph: &Graph,
    ctx: &GpuContext,
    shaders: &mut ShaderManager,
    pool: &Arc<Mutex<TexturePool>>,
) {
    let span = tracing::debug_span!("register_processors", nodes = graph.nodes().count());
    let _guard = span.enter();
    for node in graph.nodes() {
        if let Some(proc) = processor_for_node(node, ctx, shaders, pool) {
            evaluator.register(node.id, proc);
        }
    }
}

/// Convenience constructor for the shared eval-worker texture pool.
///
/// One pool per evaluation worker: GPU node processors allocate their
/// intermediates and resident outputs from it, and `GpuFrameBuffer` handles
/// return textures on drop. The default idle budget is 512 MiB.
pub fn shared_texture_pool(ctx: &GpuContext) -> Arc<Mutex<TexturePool>> {
    Arc::new(Mutex::new(TexturePool::new(ctx.clone(), 512 * 1024 * 1024)))
}

/// Build the built-in processor for a single `node`, or `None` when its
/// `type_key` is not a built-in (plugin space).
///
/// Processors capture their parameter values at construction, so a parameter
/// edit requires rebuilding the edited node's processor via this function.
pub fn processor_for_node(
    node: &Node,
    ctx: &GpuContext,
    shaders: &mut ShaderManager,
    pool: &Arc<Mutex<TexturePool>>,
) -> Option<Arc<dyn ravel_core::eval::NodeProcessor>> {
    let processor: Option<Arc<dyn ravel_core::eval::NodeProcessor>> = match node.type_key.as_str() {
        "attribute.set" => Some(Arc::new(attribute::AttributeSetProcessor::from_node(node))),
        "attribute.promote" => Some(Arc::new(attribute::AttributePromoteProcessor::from_node(
            node,
        ))),
        "attribute.transfer" => Some(Arc::new(attribute::AttributeTransferProcessor::from_node(
            node,
        ))),
        "attribute.path_sample" => Some(Arc::new(attribute::PathSampleProcessor::from_node(node))),
        "constant" => Some(Arc::new(constant::ConstantProcessor::from_node(node))),
        "constant.color" => Some(Arc::new(constant::ColorConstantProcessor::from_node(node))),
        // Keep Composition compiler synthetic nodes on the CPU reference path:
        // shape_layer_golden intentionally pins their established pixels. User
        // rasterize nodes use the resident GPU path.
        "rasterize" if node.metadata.synthetic => {
            Some(Arc::new(rasterize::RasterizeProcessor::from_node(node)))
        }
        "rasterize" => Some(Arc::new(rasterize::RasterizeProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        "color_correct" => Some(Arc::new(color_correct::ColorCorrectProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        "blur" => Some(Arc::new(blur::BlurProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        "transform" => Some(Arc::new(transform::TransformProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        "merge" => Some(Arc::new(merge::MergeProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        "field.noise" => Some(Arc::new(field::NoiseFieldProcessor::from_node(node))),
        "field.falloff" => Some(Arc::new(field::FalloffFieldProcessor::from_node(node))),
        "field.curve_remap" => Some(Arc::new(field::CurveRemapFieldProcessor::from_node(node))),
        "field.expression" => Some(Arc::new(field::ExpressionFieldProcessor::from_node(node))),
        "field.add" => Some(Arc::new(field::AddFieldProcessor)),
        "field.multiply" => Some(Arc::new(field::MultiplyFieldProcessor)),
        "field.max" => Some(Arc::new(field::MaxFieldProcessor)),
        "field.blend" => Some(Arc::new(field::BlendFieldProcessor::from_node(node))),
        "field.apply" => Some(Arc::new(field::ApplyFieldProcessor::from_node(node))),
        // Shape generators
        "shape.rect" => Some(Arc::new(shape::RectProcessor::from_node(node))),
        "shape.ellipse" => Some(Arc::new(shape::EllipseProcessor::from_node(node))),
        "shape.polygon" => Some(Arc::new(shape::PolygonProcessor::from_node(node))),
        "shape.star" => Some(Arc::new(shape::StarProcessor::from_node(node))),
        "shape.custom_path" => Some(Arc::new(shape::CustomPathProcessor::from_node(node))),
        // Scatter / instance duplication
        "scatter.grid" => Some(Arc::new(scatter::GridProcessor::from_node(node))),
        "scatter.circular" => Some(Arc::new(scatter::CircularProcessor::from_node(node))),
        "scatter.path_array" => Some(Arc::new(scatter::PathArrayProcessor::from_node(node))),
        "scatter.scatter" => Some(Arc::new(scatter::ScatterProcessor::from_node(node))),
        // Composition shell (synthetic) nodes
        "comp.network" => Some(Arc::new(comp::CompNetworkProcessor::from_node(node))),
        "comp.transform" => Some(Arc::new(comp::CompTransformProcessor::from_node(node))),
        "comp.opacity" => Some(Arc::new(comp::CompOpacityProcessor::from_node(node))),
        t if t.starts_with("comp.merge.") => {
            Some(Arc::new(comp::CompMergeProcessor::from_node(node)))
        }
        // Media
        "video" => Some(Arc::new(video::VideoProcessor::from_node(node))),
        // Cross-layer reference (REQ-LAYER-005)
        "layer.ref" => Some(Arc::new(layer_ref::LayerRefProcessor::from_node(node))),
        // Network interface nodes
        "net.in" => Some(Arc::new(net::NetInProcessor::from_node(node))),
        "net.out" => Some(Arc::new(net::NetOutProcessor::from_node(node))),
        _ => None,
    };
    processor
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::EvalContext;
    use ravel_core::geometry::Geometry;
    use ravel_core::graph::{Node, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::{FrameBuffer, FrameRate, Scalar};

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (4, 4))
    }

    fn solid_fb(width: u32, height: u32, r: f32, g: f32, b: f32, a: f32) -> FrameBuffer {
        let n = (width * height) as usize;
        let mut data = Vec::with_capacity(n * 4);
        for _ in 0..n {
            data.extend_from_slice(&[r, g, b, a]);
        }
        FrameBuffer {
            width,
            height,
            data: Arc::from(data),
        }
    }

    #[test]
    fn register_all_covers_constant() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());

        let node = Node::new(NodeId::new(1), "constant")
            .with_output("value", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(7.0));
        let graph = Graph::new().add_node(node).unwrap();

        let mut ev = Evaluator::new();
        let pool = shared_texture_pool(&gpu);
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool);

        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        let s = out.downcast_ref::<Scalar>().unwrap();
        assert!((s.0 - 7.0).abs() < f32::EPSILON);
    }

    #[test]
    fn register_all_covers_gpu_nodes() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());

        // constant(0.5) feeds a FrameBuffer-producing chain is hard to test
        // without a FrameBuffer source. Instead test that color_correct registers
        // correctly by building: color_correct node.
        let cc_node = Node::new(NodeId::new(1), "color_correct")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("brightness", ParameterValue::Float(0.0))
            .with_param("contrast", ParameterValue::Float(1.0))
            .with_param("saturation", ParameterValue::Float(1.0));
        let graph = Graph::new().add_node(cc_node).unwrap();

        let mut ev = Evaluator::new();
        let pool = shared_texture_pool(&gpu);
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool);

        // Processor is registered → is_dirty == true.
        assert!(ev.is_dirty(NodeId::new(1)));
    }

    #[test]
    fn processor_factory_selects_gpu_for_user_rasterize_only() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let pool = shared_texture_pool(&gpu);
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = Node::new(NodeId::new(1), "rasterize");
        let mut scope = Evaluator::new();
        let geo: Arc<dyn ravel_core::types::NodeData> = Arc::new(Geometry::new());
        let processor = processor_for_node(&node, &gpu, &mut shaders, &pool).unwrap();
        let out = processor
            .process(
                &node,
                &ctx(),
                &[Some(geo.clone())],
                &ravel_core::eval::ResolvedParams::default(),
                &mut scope,
            )
            .unwrap();
        assert!(out.downcast_ref::<ravel_gpu::GpuFrameBuffer>().is_some());

        let mut synthetic = node.clone();
        synthetic.metadata.synthetic = true;
        let processor = processor_for_node(&synthetic, &gpu, &mut shaders, &pool).unwrap();
        let out = processor
            .process(
                &synthetic,
                &ctx(),
                &[Some(geo)],
                &ravel_core::eval::ResolvedParams::default(),
                &mut scope,
            )
            .unwrap();
        assert!(out.downcast_ref::<FrameBuffer>().is_some());
    }

    #[test]
    fn unknown_type_key_skipped_silently() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());

        let node =
            Node::new(NodeId::new(1), "unknown_plugin_node").with_output("out", DataTypeId::SCALAR);
        let graph = Graph::new().add_node(node).unwrap();

        let mut ev = Evaluator::new();
        let pool = shared_texture_pool(&gpu);
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool);

        // No processor registered → is_dirty returns false (not in dirty set).
        assert!(!ev.is_dirty(NodeId::new(1)));
    }

    #[test]
    fn integration_merge_two_constants_through_color_correct() {
        // Graph:
        //  const_a(value=0.3) → A \
        //                            merge(over) → color_correct(brightness=0.1)
        //  const_b(value=0.6) → B /
        //
        // Constants output Scalar, but merge expects FrameBuffer. To test the full
        // pipeline E2E, we build a simpler graph: two color_correct nodes feeding
        // into merge.

        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());

        // We'll manually provide FrameBuffer inputs and test the chain:
        // color_correct(identity) → merge(add)

        let cc_a = Node::new(NodeId::new(1), "color_correct")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("brightness", ParameterValue::Float(0.0))
            .with_param("contrast", ParameterValue::Float(1.0))
            .with_param("saturation", ParameterValue::Float(1.0));

        let cc_b = Node::new(NodeId::new(2), "color_correct")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("brightness", ParameterValue::Float(0.0))
            .with_param("contrast", ParameterValue::Float(1.0))
            .with_param("saturation", ParameterValue::Float(1.0));

        let merge = Node::new(NodeId::new(3), "merge")
            .with_input("A", &[DataTypeId::FRAME_BUFFER])
            .with_input("B", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("operation", ParameterValue::String("add".into()))
            .with_param("mix", ParameterValue::Float(1.0));

        let graph = Graph::new()
            .add_node(cc_a)
            .unwrap()
            .add_node(cc_b)
            .unwrap()
            .add_node(merge)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(1),
            )
            .unwrap();

        let mut ev = Evaluator::new();
        let pool = shared_texture_pool(&gpu);
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool);

        // color_correct nodes have no upstream inputs, so we need to provide them
        // manually. For a true E2E test with FrameBuffer sources we'd need a
        // "generate" node. Instead, directly register stub processors that emit
        // solid FrameBuffers.
        struct FbSource(FrameBuffer);
        impl ravel_core::eval::NodeProcessor for FbSource {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn ravel_core::types::NodeData>>],
                _params: &ravel_core::eval::ResolvedParams,
                _scope: &mut dyn ravel_core::eval::EvalScope,
            ) -> anyhow::Result<Arc<dyn ravel_core::types::NodeData>> {
                Ok(Arc::new(self.0.clone()))
            }
        }

        ev.register(
            NodeId::new(1),
            Arc::new(FbSource(solid_fb(4, 4, 0.3, 0.0, 0.0, 1.0))),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(FbSource(solid_fb(4, 4, 0.0, 0.5, 0.0, 1.0))),
        );

        let out = ev.evaluate(&graph, NodeId::new(3), &ctx()).unwrap();
        let fb = out
            .downcast_ref::<ravel_gpu::GpuFrameBuffer>()
            .expect("merge output stays GPU-resident")
            .to_frame_buffer()
            .unwrap();

        assert_eq!(fb.width, 4);
        assert_eq!(fb.height, 4);
        // add mode: (0.3, 0.0, 0.0) + (0.0, 0.5, 0.0) = (0.3, 0.5, 0.0)
        assert!((fb.data[0] - 0.3).abs() < 0.02, "r={}", fb.data[0]);
        assert!((fb.data[1] - 0.5).abs() < 0.02, "g={}", fb.data[1]);
        assert!(fb.data[2] < 0.02, "b={}", fb.data[2]);
    }
}
