// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The CPU → GPU upload memo (issue MED-GPU-05): one CPU-resident frame
//! feeding N GPU nodes is uploaded once per evaluation instead of N times,
//! and the memo that makes that true is closed at the end of the evaluation
//! that opened it. Requires a GPU adapter; tests skip gracefully without one.

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::types::{FrameBuffer, FrameRate, NodeData};
use ravel_gpu::{GpuContext, ShaderManager};
use ravel_media::frame_cache::MediaFrameCache;
use ravel_nodes::{begin_upload_scope, ensure_gpu, register_all_processors, shared_texture_pool};
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
