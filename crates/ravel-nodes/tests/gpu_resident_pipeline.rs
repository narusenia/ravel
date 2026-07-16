// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Phase 2 completion tests (`eval-render-performance-plan.md`): GPU node
//! chains keep intermediates resident in VRAM with zero CPU readbacks, and
//! the resident path is pixel-equivalent to staging through the CPU between
//! nodes. Requires a GPU adapter; tests skip gracefully without one.

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::types::{FrameBuffer, FrameRate, NodeData};
use ravel_gpu::{GpuContext, GpuFrameBuffer, ShaderManager};
use ravel_nodes::{register_all_processors, shared_texture_pool};
use std::sync::Arc;

const SRC: u64 = 1;
const BLUR: u64 = 2;
const CC: u64 = 3;
const MERGE: u64 = 4;

fn nid(raw: u64) -> NodeId {
    NodeId::new(raw)
}

fn ctx() -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), (32, 32))
}

struct FbSource(FrameBuffer);

impl NodeProcessor for FbSource {
    fn process(
        &self,
        _ctx: &EvalContext,
        _inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        Ok(Box::new(self.0.clone()))
    }
}

fn gradient_fb(width: u32, height: u32) -> FrameBuffer {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&[x as f32 / width as f32, y as f32 / height as f32, 0.5, 1.0]);
        }
    }
    FrameBuffer {
        width,
        height,
        data: Arc::from(data),
    }
}

/// source → blur → color_correct → merge.A, source → merge.B
fn effect_graph() -> Graph {
    let source = Node::new(nid(SRC), "test.source").with_output("output", DataTypeId::FRAME_BUFFER);
    let blur = Node::new(nid(BLUR), "blur")
        .with_input("image", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER)
        .with_param("radius", ParameterValue::Float(2.0));
    let cc = Node::new(nid(CC), "color_correct")
        .with_input("image", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER)
        .with_param("brightness", ParameterValue::Float(0.1))
        .with_param("contrast", ParameterValue::Float(1.1))
        .with_param("saturation", ParameterValue::Float(0.9));
    let merge = Node::new(nid(MERGE), "merge")
        .with_input("A", &[DataTypeId::FRAME_BUFFER])
        .with_input("B", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER)
        .with_param("operation", ParameterValue::String("over".into()))
        .with_param("mix", ParameterValue::Float(1.0));

    Graph::new()
        .add_node(source)
        .unwrap()
        .add_node(blur)
        .unwrap()
        .add_node(cc)
        .unwrap()
        .add_node(merge)
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            nid(SRC),
            OutputPortIndex(0),
            nid(BLUR),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(2),
            nid(BLUR),
            OutputPortIndex(0),
            nid(CC),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(3),
            nid(CC),
            OutputPortIndex(0),
            nid(MERGE),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(4),
            nid(SRC),
            OutputPortIndex(0),
            nid(MERGE),
            InputPortIndex(1),
        )
        .unwrap()
}

#[test]
fn gpu_chain_evaluates_with_zero_intermediate_readbacks() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = shared_texture_pool(&gpu);
    let graph = effect_graph();

    let mut evaluator = Evaluator::new();
    register_all_processors(&mut evaluator, &graph, &gpu, &mut shaders, &pool);
    evaluator.register(nid(SRC), Arc::new(FbSource(gradient_fb(32, 32))));

    let before = gpu.transfer_stats();
    let out = evaluator.evaluate(&graph, nid(MERGE), &ctx()).unwrap();
    let delta = before.delta(&gpu.transfer_stats());

    // The CPU source is uploaded where it enters the GPU chain (blur, and
    // merge input B); every intermediate stays resident.
    assert_eq!(delta.readbacks, 0, "no intermediate readbacks: {delta:?}");
    assert_eq!(delta.uploads, 2, "source uploads only: {delta:?}");

    // The chain output is a GPU handle; displaying it costs exactly one
    // readback.
    let frame = out
        .downcast_ref::<GpuFrameBuffer>()
        .expect("merge output stays GPU-resident");
    let before = gpu.transfer_stats();
    let fb = frame.to_frame_buffer().unwrap();
    let delta = before.delta(&gpu.transfer_stats());
    assert_eq!(delta.readbacks, 1);
    assert_eq!(fb.width, 32);
    assert!(fb.data.iter().any(|v| *v > 0.0), "non-empty output");
}

#[test]
fn resident_path_matches_cpu_staged_path() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = shared_texture_pool(&gpu);

    let blur_node = Node::new(nid(BLUR), "blur").with_param("radius", ParameterValue::Float(3.0));
    let cc_node = Node::new(nid(CC), "color_correct")
        .with_param("brightness", ParameterValue::Float(0.2))
        .with_param("contrast", ParameterValue::Float(1.2))
        .with_param("saturation", ParameterValue::Float(0.8));
    let blur =
        ravel_nodes::blur::BlurProcessor::new(gpu.clone(), &mut shaders, pool.clone(), &blur_node);
    let cc = ravel_nodes::color_correct::ColorCorrectProcessor::new(
        gpu.clone(),
        &mut shaders,
        pool.clone(),
        &cc_node,
    );

    let source = gradient_fb(16, 16);
    let ctx = ctx();

    // Resident: blur → cc with the intermediate staying in VRAM.
    let blurred = blur.process(&ctx, &[&source as &dyn NodeData]).unwrap();
    let corrected = cc.process(&ctx, &[blurred.as_ref()]).unwrap();
    let resident = corrected
        .downcast_ref::<GpuFrameBuffer>()
        .unwrap()
        .to_frame_buffer()
        .unwrap();

    // Staged: read the blur result back to the CPU and re-upload it.
    let blurred_cpu = blurred
        .downcast_ref::<GpuFrameBuffer>()
        .unwrap()
        .to_frame_buffer()
        .unwrap();
    let corrected_staged = cc.process(&ctx, &[&blurred_cpu as &dyn NodeData]).unwrap();
    let staged = corrected_staged
        .downcast_ref::<GpuFrameBuffer>()
        .unwrap()
        .to_frame_buffer()
        .unwrap();

    assert_eq!(resident.data.len(), staged.data.len());
    for (i, (a, b)) in resident.data.iter().zip(staged.data.iter()).enumerate() {
        assert!(
            (a - b).abs() < 1e-5,
            "pixel component {i} differs: resident={a}, staged={b}"
        );
    }
}

#[test]
fn dropping_cached_results_returns_textures_to_the_pool() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = shared_texture_pool(&gpu);
    let graph = effect_graph();

    let mut evaluator = Evaluator::new();
    register_all_processors(&mut evaluator, &graph, &gpu, &mut shaders, &pool);
    evaluator.register(nid(SRC), Arc::new(FbSource(gradient_fb(32, 32))));
    let out = evaluator.evaluate(&graph, nid(MERGE), &ctx()).unwrap();

    // Cached results (and the returned handle) hold pool textures; dropping
    // both must return every resident texture to the pool for reuse.
    drop(out);
    let idle_before = pool.lock().unwrap().idle_count();
    evaluator.invalidate_all();
    let idle_after = pool.lock().unwrap().idle_count();
    assert!(
        idle_after > idle_before,
        "cache invalidation must release resident textures ({idle_before} -> {idle_after})"
    );
}
