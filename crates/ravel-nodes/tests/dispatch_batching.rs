// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! MED-GPU-01 completion tests: the declarative dispatch layer batches a
//! frame of GPU nodes into one submit, and re-evaluating a node with
//! unchanged parameters creates no new uniform buffers or bind groups.
//! Requires a GPU adapter; tests skip gracefully without one.

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor, ResolvedParams};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::types::{FrameBuffer, FrameRate, NodeData};
use ravel_gpu::{GpuContext, GpuFrameBuffer, ShaderManager, TexturePool};
use ravel_nodes::blur::BlurProcessor;
use std::sync::{Arc, Mutex};

fn try_context() -> Option<GpuContext> {
    GpuContext::new_blocking().ok()
}

fn ctx() -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), (8, 8))
}

fn test_pool(gpu: &GpuContext) -> Arc<Mutex<TexturePool>> {
    Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)))
}

fn gradient_fb(width: u32, height: u32) -> FrameBuffer {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&[x as f32 / width as f32, y as f32 / height as f32, 0.5, 1.0]);
        }
    }
    FrameBuffer::from_f32(width, height, data)
}

fn readback(out: &Arc<dyn NodeData>) -> FrameBuffer {
    out.downcast_ref::<GpuFrameBuffer>()
        .expect("GPU node outputs a resident frame")
        .to_frame_buffer()
        .expect("readback")
}

/// Re-evaluating with identical parameters in steady state creates nothing:
/// the uniform buffer is content-addressed and the bind group is keyed by
/// the (pipeline, textures, uniform) identity, which the pooled textures
/// reproduce once the working set has cycled through the pool.
#[test]
fn identical_parameters_create_no_new_bind_groups_or_uniforms() {
    let Some(gpu) = try_context() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = test_pool(&gpu);
    let node = Node::new(NodeId::new(1), "blur")
        .with_input("image", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER);
    let processor = BlurProcessor::new(gpu.clone(), &mut shaders, pool, &node);
    let input: Arc<dyn NodeData> = Arc::new(gradient_fb(8, 8));

    let evaluate_once = |scope: &mut Evaluator| {
        let out = processor
            .process(
                &node,
                &ctx(),
                &[Some(input.clone())],
                &ResolvedParams::default(),
                scope,
            )
            .expect("blur");
        // The readback is the frame's flush point; dropping the output then
        // returns its texture to the pool for the next evaluation.
        let fb = readback(&out);
        assert_eq!((fb.width, fb.height), (8, 8));
    };

    // Warm the caches: the pool rotates its textures through the input /
    // intermediate / output roles, so the first few evaluations legitimately
    // build the bind groups for each assignment.
    let mut scope = Evaluator::new();
    for _ in 0..4 {
        evaluate_once(&mut scope);
    }

    let before = gpu.dispatch_stats();
    evaluate_once(&mut scope);
    let stats = before.delta(&gpu.dispatch_stats());
    assert_eq!(
        stats.uniform_buffers_created, 0,
        "an identical uniform block must be served from the cache"
    );
    assert_eq!(
        stats.bind_groups_created, 0,
        "an identical (pipeline, textures, uniform) dispatch must be served from the cache"
    );
}

/// A frame that runs four dispatches across three GPU nodes submits once:
/// the shared encoder flushes at the frame's readback.
#[test]
fn a_frame_of_gpu_nodes_submits_once() {
    let Some(gpu) = try_context() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = test_pool(&gpu);

    struct FbSource(FrameBuffer);
    impl NodeProcessor for FbSource {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn ravel_core::eval::EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(self.0.clone()))
        }
    }

    // source → blur → color_correct → merge.A, source → merge.B
    let source =
        Node::new(NodeId::new(1), "test.source").with_output("out", DataTypeId::FRAME_BUFFER);
    let blur = Node::new(NodeId::new(2), "blur")
        .with_input("image", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER);
    let cc = Node::new(NodeId::new(3), "color_correct")
        .with_input("image", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER)
        .with_param("brightness", ParameterValue::Float(0.1));
    let merge = Node::new(NodeId::new(4), "merge")
        .with_input("A", &[DataTypeId::FRAME_BUFFER])
        .with_input("B", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER)
        .with_param("operation", ParameterValue::String("over".into()))
        .with_param("mix", ParameterValue::Float(1.0));
    let graph = Graph::new()
        .add_node(source)
        .unwrap()
        .add_node(blur.clone())
        .unwrap()
        .add_node(cc.clone())
        .unwrap()
        .add_node(merge.clone())
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            NodeId::new(1),
            OutputPortIndex(0),
            NodeId::new(2),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(2),
            NodeId::new(2),
            OutputPortIndex(0),
            NodeId::new(3),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(3),
            NodeId::new(3),
            OutputPortIndex(0),
            NodeId::new(4),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(4),
            NodeId::new(1),
            OutputPortIndex(0),
            NodeId::new(4),
            InputPortIndex(1),
        )
        .unwrap();

    let mut ev = Evaluator::new();
    ev.register(NodeId::new(1), Arc::new(FbSource(gradient_fb(8, 8))));
    for (id, node) in [
        (NodeId::new(2), blur),
        (NodeId::new(3), cc),
        (NodeId::new(4), merge),
    ] {
        let proc = ravel_nodes::processor_for_node(&node, &gpu, &mut shaders, &pool)
            .expect("built-in processor");
        ev.register(id, proc);
    }

    let before = gpu.dispatch_stats();
    let out = ev.evaluate(&graph, NodeId::new(4), &ctx()).unwrap();
    // Four dispatches (blur is two passes) are pending; nothing submitted yet.
    assert_eq!(before.delta(&gpu.dispatch_stats()).submits, 0);
    let fb = readback(&out);
    assert_eq!((fb.width, fb.height), (8, 8));
    let stats = before.delta(&gpu.dispatch_stats());
    assert_eq!(
        stats.submits, 1,
        "a frame of GPU nodes batches into a single submit"
    );
}
