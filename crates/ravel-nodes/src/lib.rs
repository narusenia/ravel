// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Built-in node processors for the Ravel DAG evaluation pipeline.
//!
//! Each module implements [`ravel_core::eval::NodeProcessor`] for one of the
//! registered built-in node types. GPU-accelerated processors use
//! [`ravel_gpu`] for shader compilation and texture management.

pub mod blur;
pub mod color_correct;
pub mod constant;
mod gpu_util;
pub mod merge;
pub mod transform;

use ravel_core::eval::Evaluator;
use ravel_core::graph::Graph;
use ravel_gpu::{GpuContext, ShaderManager};
use std::sync::Arc;

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
) {
    for node in graph.nodes() {
        let processor: Option<Arc<dyn ravel_core::eval::NodeProcessor>> =
            match node.type_key.as_str() {
                "constant" => Some(Arc::new(constant::ConstantProcessor::from_node(node))),
                "color_correct" => Some(Arc::new(color_correct::ColorCorrectProcessor::new(
                    ctx.clone(),
                    shaders,
                    node,
                ))),
                "blur" => Some(Arc::new(blur::BlurProcessor::new(
                    ctx.clone(),
                    shaders,
                    node,
                ))),
                "transform" => Some(Arc::new(transform::TransformProcessor::new(
                    ctx.clone(),
                    shaders,
                    node,
                ))),
                "merge" => Some(Arc::new(merge::MergeProcessor::new(
                    ctx.clone(),
                    shaders,
                    node,
                ))),
                _ => None,
            };
        if let Some(proc) = processor {
            evaluator.register(node.id, proc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::EvalContext;
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
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders);

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
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders);

        // Processor is registered → is_dirty == true.
        assert!(ev.is_dirty(NodeId::new(1)));
    }

    #[test]
    fn unknown_type_key_skipped_silently() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());

        let node =
            Node::new(NodeId::new(1), "unknown_plugin_node").with_output("out", DataTypeId::SCALAR);
        let graph = Graph::new().add_node(node).unwrap();

        let mut ev = Evaluator::new();
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders);

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
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders);

        // color_correct nodes have no upstream inputs, so we need to provide them
        // manually. For a true E2E test with FrameBuffer sources we'd need a
        // "generate" node. Instead, directly register stub processors that emit
        // solid FrameBuffers.
        struct FbSource(FrameBuffer);
        impl ravel_core::eval::NodeProcessor for FbSource {
            fn process(
                &self,
                _ctx: &EvalContext,
                _inputs: &[&dyn ravel_core::types::NodeData],
            ) -> anyhow::Result<Box<dyn ravel_core::types::NodeData>> {
                Ok(Box::new(self.0.clone()))
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
        let fb = out.downcast_ref::<FrameBuffer>().unwrap();

        assert_eq!(fb.width, 4);
        assert_eq!(fb.height, 4);
        // add mode: (0.3, 0.0, 0.0) + (0.0, 0.5, 0.0) = (0.3, 0.5, 0.0)
        assert!((fb.data[0] - 0.3).abs() < 0.02, "r={}", fb.data[0]);
        assert!((fb.data[1] - 0.5).abs() < 0.02, "g={}", fb.data[1]);
        assert!(fb.data[2] < 0.02, "b={}", fb.data[2]);
    }
}
