// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! MED-GPU-01 completion tests: the declarative dispatch layer batches a
//! frame of GPU nodes into one submit, and re-evaluating a node with
//! unchanged parameters creates no new uniform buffers or bind groups.
//! The rasterizer's render pass joins the same batch, so its intermediate
//! attachment's lifetime is covered here too.
//! Requires a GPU adapter; tests skip gracefully without one.

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{Geometry, Primitive};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::types::{FrameBuffer, FrameRate, NodeData, Vec2};
use ravel_gpu::{
    GpuContext, GpuFrameBuffer, ShaderManager, TextureFormat, TextureKey, TexturePool, TextureUsage,
};
use ravel_nodes::blur::BlurProcessor;
use ravel_nodes::rasterize::RasterizeProcessor;
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
        let proc = ravel_nodes::processor_for_node(
            &node,
            &gpu,
            &mut shaders,
            &pool,
            &ravel_media::frame_cache::MediaFrameCache::standalone(),
        )
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

/// The rasterizer used to build its own encoder and submit on every draw,
/// standing outside the frame's batch. Its render pass and its unpremultiply
/// compute pass now record into the shared encoder, so a frame that rasterizes
/// submits exactly once — at the readback, like every other GPU node.
#[test]
fn a_rasterize_frame_submits_once_at_the_readback() {
    let Some(gpu) = try_context() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = test_pool(&gpu);
    let node = Node::new(NodeId::new(1), "rasterize")
        .with_input("geometry", &[DataTypeId::GEOMETRY])
        .with_output("output", DataTypeId::FRAME_BUFFER);
    let processor = RasterizeProcessor::new(gpu.clone(), &mut shaders, pool, &node);
    let geometry: Arc<dyn NodeData> = Arc::new(square_geometry());

    let before = gpu.dispatch_stats();
    let mut scope = Evaluator::new();
    let out = processor
        .process(
            &node,
            &ctx(),
            &[Some(geometry)],
            &ResolvedParams::default(),
            &mut scope,
        )
        .expect("rasterize");
    assert_eq!(
        before.delta(&gpu.dispatch_stats()).submits,
        0,
        "the draw and the unpremultiply pass are recorded, not submitted"
    );

    let fb = readback(&out);
    assert_eq!((fb.width, fb.height), (8, 8));
    assert!(
        fb.as_f32().iter().skip(3).step_by(4).any(|a| *a > 0.5),
        "the batch really ran: the square is drawn"
    );
    assert_eq!(
        before.delta(&gpu.dispatch_stats()).submits,
        1,
        "a frame that rasterizes submits once"
    );
}

/// The premultiplied attachment is returned to the pool immediately after the
/// draw is recorded, while the batch still has to render into it. Handing it
/// to the next acquirer would let that owner overwrite pixels the queued pass
/// has not written yet — the low-reproducibility "occasional black frame"
/// class of bug. The pool must skip it until the flush, and reuse it after.
#[test]
fn the_pending_render_attachment_is_not_reused_before_the_flush() {
    let Some(gpu) = try_context() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = test_pool(&gpu);
    let node = Node::new(NodeId::new(1), "rasterize")
        .with_input("geometry", &[DataTypeId::GEOMETRY])
        .with_output("output", DataTypeId::FRAME_BUFFER);
    let processor = RasterizeProcessor::new(gpu.clone(), &mut shaders, pool.clone(), &node);
    let geometry: Arc<dyn NodeData> = Arc::new(square_geometry());

    let mut scope = Evaluator::new();
    let _out = processor
        .process(
            &node,
            &ctx(),
            &[Some(geometry)],
            &ResolvedParams::default(),
            &mut scope,
        )
        .expect("rasterize");

    // Must match the attachment key the rasterizer acquires: premultiplied
    // Rgba16Float, drawn into and then sampled.
    let attachment = TextureKey::new(
        8,
        8,
        TextureFormat::Rgba16Float,
        TextureUsage::RENDER_ATTACHMENT | TextureUsage::TEXTURE_BINDING,
    );
    let created = pool.lock().unwrap().total_created();
    assert_eq!(
        pool.lock().unwrap().idle_count(),
        1,
        "the attachment was released right after the draw was recorded"
    );

    let skipped = pool.lock().unwrap().acquire(attachment);
    assert_eq!(
        pool.lock().unwrap().total_created(),
        created + 1,
        "an attachment the pending batch still draws into must not be handed out"
    );
    // Hold `skipped` until after the post-flush acquire. Releasing it here
    // would leave two idle textures under this key, and the assertion below
    // would then pass by handing out `skipped` — without ever proving that the
    // recorded attachment left the pending set.
    gpu.flush();
    let reused = pool.lock().unwrap().acquire(attachment);
    assert_eq!(
        pool.lock().unwrap().total_created(),
        created + 1,
        "after the flush the attachment circulates again"
    );
    pool.lock().unwrap().release(reused);
    pool.lock().unwrap().release(skipped);
}

/// A filled 4x4 square inside the 8x8 test frame.
fn square_geometry() -> Geometry {
    let mut geo = Geometry::from_points(vec![
        Vec2(2.0, 2.0),
        Vec2(6.0, 2.0),
        Vec2(6.0, 6.0),
        Vec2(2.0, 6.0),
    ]);
    geo.push_primitive(Primitive::Path {
        verts: 0..4,
        closed: true,
    });
    geo
}
