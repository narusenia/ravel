// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden pixel test for a Shape layer in the layer-network model
//! (REQ-LAYER-007).
//!
//! The layer's owned network is `shape.rect → rasterize → net.out(frame)`;
//! the shell compiler wraps it in the synthetic chain
//! `boundary → Transform → Opacity → Merge`, and the boundary evaluates the
//! network through the scoped evaluator. The CPU reference rasterizer is
//! registered explicitly so the pinned pixels match the established zeno
//! reference.

use ravel_core::composition::compile::compile_composition;
use ravel_core::composition::{Composition, Document, Layer};
use ravel_core::eval::{EvalContext, Evaluator};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{
    CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex,
};
use ravel_core::network as net;
use ravel_core::types::{FrameBuffer, FrameRate};
use ravel_gpu::{GpuContext, ShaderManager};
use ravel_nodes::{register_all_processors, shared_texture_pool};
use std::sync::Arc;

fn pixel(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
    let idx = ((y * fb.width + x) * 4) as usize;
    fb.data[idx..idx + 4].try_into().unwrap()
}

/// `shape.rect → rasterize → net.out(frame)`, plus the conventional
/// `net.in` (unused by this network).
fn shape_rect_network() -> (Graph, NodeId) {
    let shape = Node::new(NodeId::new(500), "shape.rect")
        .with_output("output", DataTypeId::GEOMETRY)
        .with_param("center_x", ParameterValue::Float(32.0))
        .with_param("center_y", ParameterValue::Float(32.0))
        .with_param("width", ParameterValue::Float(32.0))
        .with_param("height", ParameterValue::Float(32.0));
    let rasterize = Node::new(NodeId::new(501), "rasterize")
        .with_input("geometry", &[DataTypeId::GEOMETRY])
        .with_output("output", DataTypeId::FRAME_BUFFER);
    let in_node = Node::new(NodeId::new(502), net::NET_IN_TYPE_KEY)
        .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
        .with_output(net::PORT_TIME, DataTypeId::SCALAR);
    let out_node = Node::new(NodeId::new(503), net::NET_OUT_TYPE_KEY)
        .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);

    let network = Graph::new()
        .add_node(shape)
        .unwrap()
        .add_node(rasterize)
        .unwrap()
        .add_node(in_node)
        .unwrap()
        .add_node(out_node)
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            NodeId::new(500),
            OutputPortIndex(0),
            NodeId::new(501),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(2),
            NodeId::new(501),
            OutputPortIndex(0),
            NodeId::new(503),
            InputPortIndex(0),
        )
        .unwrap();
    (network, NodeId::new(501))
}

fn build_evaluator(
    comp: &Composition,
    networks: &[&Graph],
    cpu_rasterize: Option<(NodeId, &Graph)>,
) -> (
    Evaluator,
    ravel_core::composition::compile::CompilationResult,
) {
    let result = compile_composition(comp, Graph::new()).expect("compile succeeds");

    let gpu = GpuContext::new_blocking().expect("GPU adapter required for registration");
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = shared_texture_pool(&gpu);
    let mut evaluator = Evaluator::new();
    register_all_processors(&mut evaluator, &result.graph, &gpu, &mut shaders, &pool);
    for network in networks {
        register_all_processors(&mut evaluator, network, &gpu, &mut shaders, &pool);
    }
    // Pin the CPU reference rasterizer for deterministic pixels.
    if let Some((rasterize_id, network)) = cpu_rasterize {
        let node = network.node(rasterize_id).unwrap().as_ref().clone();
        evaluator.register(
            rasterize_id,
            Arc::new(ravel_nodes::rasterize::RasterizeProcessor::from_node(&node)),
        );
    }
    (evaluator, result)
}

#[test]
fn shape_layer_network_rasterizes_rect_pixels() {
    // 64x64 comp; rect centered at (32, 32) with size 32x32 → interior
    // covers [16, 48) on both axes.
    let (network, rasterize_id) = shape_rect_network();
    let comp = Composition::new(
        CompId::new(1),
        "Golden",
        (64, 64),
        FrameRate::new(30, 1),
        300,
    )
    .add_layer(Layer::new(LayerId::new(1), "Rect", network.clone()).with_time(0, 0, 300));
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, result) =
        build_evaluator(&comp, &[&network], Some((rasterize_id, &network)));
    evaluator.set_document(Arc::new(doc));

    let ctx = EvalContext::new(0, FrameRate::new(30, 1), (64, 64));
    let out = evaluator
        .evaluate(&result.graph, result.output_node, &ctx)
        .expect("evaluation succeeds");
    let fb = out
        .downcast_ref::<FrameBuffer>()
        .expect("output is a CPU FrameBuffer");

    assert_eq!(fb.width, 64);
    assert_eq!(fb.height, 64);

    // Interior: opaque white (rasterize default Cd).
    for (x, y) in [(32, 32), (20, 20), (44, 44)] {
        let p = pixel(fb, x, y);
        assert!(p[3] > 0.9, "interior ({x},{y}) covered: {p:?}");
        assert!(p[0] > 0.9 && p[1] > 0.9 && p[2] > 0.9, "default white fill");
    }

    // Exterior: fully transparent.
    for (x, y) in [(4, 4), (60, 4), (4, 60), (60, 60), (32, 8), (8, 32)] {
        let p = pixel(fb, x, y);
        assert!(p[3] < 1e-6, "exterior ({x},{y}) transparent: {p:?}");
    }

    // Edge rows just inside/outside the rect boundary (y = 16 boundary).
    assert!(pixel(fb, 32, 17)[3] > 0.5, "just inside top edge");
    assert!(pixel(fb, 32, 14)[3] < 0.1, "just outside top edge");
}

#[test]
fn unconnected_frame_port_evaluates_to_empty_frame() {
    // A network whose Out `frame` port is unconnected produces a transparent
    // FrameBuffer instead of failing.
    let out_node = Node::new(NodeId::new(510), net::NET_OUT_TYPE_KEY)
        .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
    let network = Graph::new().add_node(out_node).unwrap();

    let comp = Composition::new(
        CompId::new(1),
        "Empty",
        (16, 16),
        FrameRate::new(30, 1),
        300,
    )
    .add_layer(Layer::new(LayerId::new(1), "Ghost", network.clone()).with_time(0, 0, 300));
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, result) = build_evaluator(&comp, &[&network], None);
    evaluator.set_document(Arc::new(doc));

    let ctx = EvalContext::new(0, FrameRate::new(30, 1), (16, 16));
    let out = evaluator
        .evaluate(&result.graph, result.output_node, &ctx)
        .expect("evaluation succeeds");
    let fb = out
        .downcast_ref::<FrameBuffer>()
        .expect("output is a FrameBuffer");

    assert!(
        fb.data.iter().skip(3).step_by(4).all(|a| *a < 1e-6),
        "every pixel transparent"
    );
}
