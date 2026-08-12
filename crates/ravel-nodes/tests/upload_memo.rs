// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The CPU → GPU upload memo (issue MED-GPU-05): one CPU-resident frame
//! feeding N GPU nodes is uploaded once per evaluation instead of N times,
//! and the memo that makes that true is closed at the end of the evaluation
//! that opened it. Requires a GPU adapter; tests skip gracefully without one.

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::runtime::{EvalWorkerHooks, InvalidationHint, ProcessorSync};
use ravel_core::types::{FrameBuffer, FrameRate, NodeData};
use ravel_gpu::{GpuContext, ShaderManager};
use ravel_media::frame_cache::MediaFrameCache;
use ravel_nodes::{
    GpuEvalHooks, begin_upload_scope, ensure_gpu, register_all_processors, shared_texture_pool,
};
use std::sync::Arc;

const SRC: u64 = 1;
const BLUR: u64 = 2;
const CC: u64 = 3;
const MERGE: u64 = 4;
const OUT: u64 = 5;

fn nid(raw: u64) -> NodeId {
    NodeId::new(raw)
}

fn ctx() -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), (32, 32))
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

/// Hands out clones of one CPU frame — clones share the pixel allocation,
/// which is what the memo keys on.
struct FbSource(FrameBuffer);

impl NodeProcessor for FbSource {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ravel_core::eval::ResolvedParams,
        _scope: &mut dyn ravel_core::eval::EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(self.0.clone()))
    }
}

fn merge_node(id: u64) -> Node {
    Node::new(nid(id), "merge")
        .with_input("A", &[DataTypeId::FRAME_BUFFER])
        .with_input("B", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER)
        .with_param("operation", ParameterValue::String("over".into()))
        .with_param("mix", ParameterValue::Float(1.0))
}

/// Three consumers of the one CPU frame:
///
/// ```text
/// src ─┬─ blur ─┐
///      ├─ cc ───┴─ merge ─┐
///      └──────────────────┴─ out
/// ```
///
/// `blur`, `cc` and `out.B` each adapt `src` for binding; `merge` sees two
/// GPU-resident inputs and adapts neither.
fn fan_out_graph() -> Graph {
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

    let edge = |id: u64, from: u64, to: u64, port: u32| {
        (
            EdgeId::new(id),
            nid(from),
            OutputPortIndex(0),
            nid(to),
            InputPortIndex(port),
        )
    };
    let mut graph = Graph::new()
        .add_node(source)
        .unwrap()
        .add_node(blur)
        .unwrap()
        .add_node(cc)
        .unwrap()
        .add_node(merge_node(MERGE))
        .unwrap()
        .add_node(merge_node(OUT))
        .unwrap();
    for (id, from, out_port, to, in_port) in [
        edge(1, SRC, BLUR, 0),
        edge(2, SRC, CC, 0),
        edge(3, BLUR, MERGE, 0),
        edge(4, CC, MERGE, 1),
        edge(5, MERGE, OUT, 0),
        edge(6, SRC, OUT, 1),
    ] {
        graph = graph.add_edge(id, from, out_port, to, in_port).unwrap();
    }
    graph
}

/// Uploads recorded while evaluating [`fan_out_graph`] once, with the memo
/// installed or not.
fn uploads_for_one_evaluation(gpu: &GpuContext, with_memo: bool) -> u64 {
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = shared_texture_pool(gpu);
    let media_frames = MediaFrameCache::standalone();
    let graph = fan_out_graph();

    let mut evaluator = Evaluator::new();
    register_all_processors(
        &mut evaluator,
        &graph,
        gpu,
        &mut shaders,
        &pool,
        &media_frames,
    );
    evaluator.register(nid(SRC), Arc::new(FbSource(gradient_fb(32, 32))));

    if with_memo {
        begin_upload_scope(&pool);
    }
    let before = gpu.transfer_stats();
    evaluator
        .evaluate(&graph, nid(OUT), &ctx())
        .expect("evaluation succeeds");
    before.delta(&gpu.transfer_stats()).uploads
}

#[test]
fn one_cpu_frame_feeding_three_gpu_nodes_uploads_once() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    // Without the memo the graph's shape is the cost: every consumer of a
    // CPU-resident frame pays the transfer again.
    assert_eq!(
        uploads_for_one_evaluation(&gpu, false),
        3,
        "three consumers of one CPU frame, three uploads"
    );
    assert_eq!(
        uploads_for_one_evaluation(&gpu, true),
        1,
        "the same three consumers share one upload"
    );
}

#[test]
fn a_memoized_texture_is_not_reused_by_the_next_evaluation() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let pool = shared_texture_pool(&gpu);
    let value: Arc<dyn NodeData> = Arc::new(gradient_fb(32, 32));

    let before = gpu.transfer_stats();
    begin_upload_scope(&pool);
    let first = ensure_gpu(&gpu, &pool, value.as_ref()).expect("upload");
    let again = ensure_gpu(&gpu, &pool, value.as_ref()).expect("memo hit");
    assert_eq!(
        first.binding().texture_id(),
        again.binding().texture_id(),
        "the second call must bind the texture the first one uploaded"
    );
    assert_eq!(
        before.delta(&gpu.transfer_stats()).uploads,
        1,
        "the second call must not upload"
    );
    first.release(&pool);
    again.release(&pool);

    // A new evaluation starts: whatever the previous one uploaded is gone,
    // even for a frame at the very same address. Without this the viewer
    // would serve a stale picture for as long as an allocator kept handing
    // out the same buffer.
    begin_upload_scope(&pool);
    ensure_gpu(&gpu, &pool, value.as_ref()).expect("upload");
    assert_eq!(
        before.delta(&gpu.transfer_stats()).uploads,
        2,
        "the frame must be uploaded again for the second evaluation"
    );
}

/// A fresh CPU frame per frame, the way a decode does: every call allocates
/// its own `Arc<[u8]>`, so no two frames share an upload key. Declaring time
/// dependence is what makes the evaluator ask again instead of serving the
/// first frame's cached value.
struct DecodedPerFrame;

impl NodeProcessor for DecodedPerFrame {
    fn is_time_dependent(&self) -> bool {
        true
    }

    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ravel_core::eval::ResolvedParams,
        _scope: &mut dyn ravel_core::eval::EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let mut fb = gradient_fb(32, 32);
        // Distinguish the frames so nothing can collapse them into one value.
        let mut pixels = fb.as_f32().to_vec();
        pixels[0] = ctx.frame as f32;
        fb = FrameBuffer::from_f32(fb.width, fb.height, pixels);
        Ok(Arc::new(fb))
    }
}

/// A render job is **one** `sync` followed by a frame loop of `evaluate` +
/// `finalize` (`ravel_core::runtime::render::render_frames`), so a scope tied
/// to `sync` alone stays open for the whole export: every frame's uploaded
/// texture is remembered, none is ever released, and a 4K job holds gigabytes
/// by the end. The scope has to close once per *evaluation*, which is what
/// this drives — with no hand-rolled `begin_upload_scope` anywhere, because a
/// test that opens the scope itself cannot see this hole.
#[test]
fn an_export_run_does_not_accumulate_leases_across_frames() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let graph = fan_out_graph();
    let mut hooks = GpuEvalHooks::new(gpu.clone());
    let mut evaluator = Evaluator::new();
    hooks.sync(
        &mut ProcessorSync::new(&mut evaluator),
        &graph,
        None,
        &InvalidationHint::Structural,
    );
    evaluator.register(nid(SRC), Arc::new(DecodedPerFrame));

    let render_frame = |evaluator: &mut Evaluator, hooks: &mut GpuEvalHooks, frame: u64| {
        let ctx = EvalContext::new(frame, FrameRate::new(30, 1), (32, 32));
        let value = evaluator
            .evaluate(&graph, nid(OUT), &ctx)
            .expect("evaluation succeeds");
        hooks.finalize(&value, &ctx).expect("finalize succeeds");
    };

    // Two frames to reach the steady state, not one: a texture released
    // while the batch that reads it is still unsubmitted cannot be handed
    // out again, so the second frame legitimately allocates one more. From
    // there the set circulates and the count must stop moving.
    const WARMUP: u64 = 2;
    const FRAMES: u64 = 16;
    for frame in 0..WARMUP {
        render_frame(&mut evaluator, &mut hooks, frame);
    }
    let warm = hooks.texture_pool().lock().unwrap().total_created();

    for frame in WARMUP..WARMUP + FRAMES {
        render_frame(&mut evaluator, &mut hooks, frame);
    }
    let created = hooks.texture_pool().lock().unwrap().total_created();
    assert_eq!(
        created,
        warm,
        "{FRAMES} more frames created {} more textures: a lease that is not \
         released cannot be reused, so the count grows with the frame count",
        created - warm,
    );
}

/// The memo holds the lease for as long as it holds the texture: handing the
/// same texture to another acquirer mid-evaluation would let one node's
/// intermediate overwrite the frame a later node still binds.
#[test]
fn the_pool_cannot_hand_out_a_memoized_texture() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let pool = shared_texture_pool(&gpu);
    let value: Arc<dyn NodeData> = Arc::new(gradient_fb(32, 32));

    begin_upload_scope(&pool);
    let memoized = ensure_gpu(&gpu, &pool, value.as_ref()).expect("upload");
    let other = pool.lock().unwrap().acquire(ravel_gpu::TextureKey::new(
        32,
        32,
        ravel_gpu::TextureFormat::Rgba32Float,
        ravel_gpu::TextureUsage::TEXTURE_BINDING
            | ravel_gpu::TextureUsage::STORAGE_BINDING
            | ravel_gpu::TextureUsage::COPY_SRC
            | ravel_gpu::TextureUsage::COPY_DST,
    ));
    assert_ne!(
        memoized.binding().texture_id(),
        other.binding().texture_id(),
        "a texture the memo holds must not be leased to anyone else"
    );
    pool.lock().unwrap().release(other);
}
