// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden pixel test for a Shape layer in the layer-network model
//! (REQ-LAYER-007).
//!
//! The layer's owned network is `shape.rect → rasterize → net.out(frame)`;
//! the shell compiler wraps it in the synthetic chain
//! `Background → boundary → Transform → Opacity → Merge`, and the boundary evaluates the
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
use ravel_gpu::{GpuContext, GpuFrameBuffer, ShaderManager};
use ravel_nodes::{register_all_processors, shared_texture_pool};
use std::sync::Arc;

fn pixel(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
    let idx = ((y * fb.width + x) * 4) as usize;
    fb.data[idx..idx + 4].try_into().unwrap()
}

fn output_frame(value: &Arc<dyn ravel_core::types::NodeData>) -> FrameBuffer {
    if let Some(frame) = value.downcast_ref::<FrameBuffer>() {
        frame.clone()
    } else {
        value
            .downcast_ref::<GpuFrameBuffer>()
            .expect("output is a resident or CPU FrameBuffer")
            .to_frame_buffer()
            .expect("resident output reads back")
    }
}

/// `shape.rect → rasterize → net.out(frame)`, plus the conventional
/// `net.in` (unused by this network).
fn shape_rect_network(center: f32, size: f32) -> (Graph, NodeId) {
    let shape = Node::new(NodeId::new(500), "shape.rect")
        .with_output("output", DataTypeId::GEOMETRY)
        .with_param("center", ParameterValue::vec2(center, center))
        .with_param("width", ParameterValue::Float(size))
        .with_param("height", ParameterValue::Float(size));
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
    let (network, rasterize_id) = shape_rect_network(32.0, 32.0);
    let mut comp = Composition::new(
        CompId::new(1),
        "Golden",
        (64, 64),
        FrameRate::new(30, 1),
        300,
    )
    .add_layer(Layer::new(LayerId::new(1), "Rect", network.clone()).with_time(0, 0, 300));
    comp.background_color = ravel_core::types::Color::TRANSPARENT;
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, result) =
        build_evaluator(&comp, &[&network], Some((rasterize_id, &network)));
    evaluator.set_document(Arc::new(doc));

    let ctx = EvalContext::new(0, FrameRate::new(30, 1), (64, 64));
    let out = evaluator
        .evaluate(&result.graph, result.output_node, &ctx)
        .expect("evaluation succeeds");
    let fb = output_frame(&out);

    assert_eq!(fb.width, 64);
    assert_eq!(fb.height, 64);

    // Interior: opaque white (rasterize default Cd).
    for (x, y) in [(32, 32), (20, 20), (44, 44)] {
        let p = pixel(&fb, x, y);
        assert!(p[3] > 0.9, "interior ({x},{y}) covered: {p:?}");
        assert!(p[0] > 0.9 && p[1] > 0.9 && p[2] > 0.9, "default white fill");
    }

    // Exterior: fully transparent.
    for (x, y) in [(4, 4), (60, 4), (4, 60), (60, 60), (32, 8), (8, 32)] {
        let p = pixel(&fb, x, y);
        assert!(p[3] < 1e-6, "exterior ({x},{y}) transparent: {p:?}");
    }

    // Edge rows just inside/outside the rect boundary (y = 16 boundary).
    assert!(pixel(&fb, 32, 17)[3] > 0.5, "just inside top edge");
    assert!(pixel(&fb, 32, 14)[3] < 0.1, "just outside top edge");
}

/// The same network in the `.ravprj` v4 shape: `center_x` / `center_y` as
/// separate Floats, which is what `Document::fold_component_params` upgrades.
fn v4_shape_rect_network(center: f32, size: f32) -> (Graph, NodeId) {
    let (network, rasterize_id) = shape_rect_network(center, size);
    let mut shape = (**network.node(NodeId::new(500)).unwrap()).clone();
    shape.parameters.retain(|p| p.key != "center");
    shape.parameters.insert(
        0,
        ravel_core::graph::Parameter {
            key: "center_x".into(),
            value: ParameterValue::Float(center),
        },
    );
    shape.parameters.insert(
        1,
        ravel_core::graph::Parameter {
            key: "center_y".into(),
            value: ParameterValue::Float(center),
        },
    );
    (network.replace_node(Arc::new(shape)), rasterize_id)
}

fn render_shape_layer(network: &Graph, rasterize_id: NodeId) -> FrameBuffer {
    let mut comp = Composition::new(
        CompId::new(1),
        "Golden",
        (64, 64),
        FrameRate::new(30, 1),
        300,
    )
    .add_layer(Layer::new(LayerId::new(1), "Rect", network.clone()).with_time(0, 0, 300));
    comp.background_color = ravel_core::types::Color::TRANSPARENT;
    let doc = Document::default().with_composition(comp.clone());
    let (mut evaluator, result) = build_evaluator(&comp, &[network], Some((rasterize_id, network)));
    evaluator.set_document(Arc::new(doc));
    let ctx = EvalContext::new(0, FrameRate::new(30, 1), (64, 64));
    output_frame(
        &evaluator
            .evaluate(&result.graph, result.output_node, &ctx)
            .expect("evaluation succeeds"),
    )
}

/// A v4 document folded on load renders exactly what the same network written
/// in the v5 shape renders — the fold is a representation change, not a
/// behaviour change.
#[test]
fn a_folded_v4_network_renders_the_same_pixels_as_a_v5_one() {
    let (v5_network, rasterize_id) = shape_rect_network(32.0, 32.0);
    let (v4_network, _) = v4_shape_rect_network(32.0, 32.0);

    let comp = Composition::new(
        CompId::new(1),
        "Legacy",
        (64, 64),
        FrameRate::new(30, 1),
        300,
    )
    .add_layer(Layer::new(LayerId::new(1), "Rect", v4_network).with_time(0, 0, 300));
    let folded = Document::default()
        .with_composition(comp)
        .fold_component_params();
    let folded_network = folded.get_composition(CompId::new(1)).expect("comp").layers[0]
        .network
        .clone();

    let expected = render_shape_layer(&v5_network, rasterize_id);
    let actual = render_shape_layer(&folded_network, rasterize_id);
    assert_eq!(
        (actual.width, actual.height),
        (expected.width, expected.height)
    );
    assert_eq!(actual.data, expected.data, "folded v4 pixels match v5");
    // Guard against both sides rendering an empty frame.
    assert!(
        pixel(&expected, 32, 32)[3] > 0.9,
        "the rect is drawn at all"
    );
}

#[test]
fn shape_layer_scales_comp_coordinates_without_cropping() {
    let (network, rasterize_id) = shape_rect_network(64.0, 32.0);
    let mut comp = Composition::new(
        CompId::new(1),
        "Scaled",
        (128, 128),
        FrameRate::new(30, 1),
        300,
    )
    .add_layer(Layer::new(LayerId::new(1), "Rect", network.clone()).with_time(0, 0, 300));
    comp.background_color = ravel_core::types::Color::TRANSPARENT;
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, result) =
        build_evaluator(&comp, &[&network], Some((rasterize_id, &network)));
    evaluator.set_document(Arc::new(doc));

    let ctx =
        EvalContext::new(0, FrameRate::new(30, 1), (64, 64)).with_comp_resolution(comp.resolution);
    let out = evaluator
        .evaluate(&result.graph, result.output_node, &ctx)
        .expect("evaluation succeeds");
    let fb = output_frame(&out);

    assert_eq!((fb.width, fb.height), (64, 64));
    assert!(
        pixel(&fb, 32, 32)[3] > 0.9,
        "comp center lands at canvas center"
    );
    assert!(
        pixel(&fb, 25, 32)[3] > 0.9,
        "scaled rect interior is preserved"
    );
    assert!(pixel(&fb, 15, 32)[3] < 0.1, "rect is not left-top cropped");
}

#[test]
fn unconnected_frame_port_evaluates_to_empty_frame() {
    // A network whose Out `frame` port is unconnected produces a transparent
    // FrameBuffer instead of failing.
    let out_node = Node::new(NodeId::new(510), net::NET_OUT_TYPE_KEY)
        .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
    let network = Graph::new().add_node(out_node).unwrap();

    let mut comp = Composition::new(
        CompId::new(1),
        "Empty",
        (16, 16),
        FrameRate::new(30, 1),
        300,
    )
    .add_layer(Layer::new(LayerId::new(1), "Ghost", network.clone()).with_time(0, 0, 300));
    comp.background_color = ravel_core::types::Color::TRANSPARENT;
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, result) = build_evaluator(&comp, &[&network], None);
    evaluator.set_document(Arc::new(doc));

    let ctx = EvalContext::new(0, FrameRate::new(30, 1), (16, 16));
    let out = evaluator
        .evaluate(&result.graph, result.output_node, &ctx)
        .expect("evaluation succeeds");
    let fb = output_frame(&out);

    assert!(
        fb.data.iter().skip(3).step_by(4).all(|a| *a < 1e-6),
        "every pixel transparent"
    );
}
