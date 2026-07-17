// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Evaluation tests for the layer-network model: layer-local time
//! (REQ-LAYER-006), custom parameters on the In node (REQ-LAYER-002/004),
//! adjustment layers (REQ-LAYER-010), and null layers (REQ-LAYER-005).

use ravel_core::animation::channel::AnimationChannel;
use ravel_core::animation::curve::KeyframeCurve;
use ravel_core::animation::interpolation::Interpolation;
use ravel_core::composition::compile::compile_composition;
use ravel_core::composition::{Composition, Document, Layer};
use ravel_core::eval::{EvalContext, EvalScope, Evaluator, NodeProcessor, ResolvedParams};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{
    CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex,
};
use ravel_core::network as net;
use ravel_core::types::{FrameBuffer, FrameRate, NodeData, Scalar};
use ravel_gpu::{GpuContext, ShaderManager};
use ravel_nodes::{register_all_processors, shared_texture_pool};
use std::sync::Arc;

const FPS: FrameRate = FrameRate { num: 30, den: 1 };

// ===========================================================================
// Helpers
// ===========================================================================

/// Emits a fixed FrameBuffer regardless of inputs (test source node).
struct FbSource(FrameBuffer);

impl NodeProcessor for FbSource {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(self.0.clone()))
    }
}

fn solid_fb(width: u32, height: u32, rgba: [f32; 4]) -> FrameBuffer {
    let n = (width * height) as usize;
    let mut data = Vec::with_capacity(n * 4);
    for _ in 0..n {
        data.extend_from_slice(&rgba);
    }
    FrameBuffer {
        width,
        height,
        data: Arc::from(data),
    }
}

fn in_node(id: u64) -> Node {
    Node::new(NodeId::new(id), net::NET_IN_TYPE_KEY)
        .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
        .with_output(net::PORT_TIME, DataTypeId::SCALAR)
        .with_output(net::PORT_SOURCE, DataTypeId::FRAME_BUFFER)
}

fn out_node(id: u64) -> Node {
    Node::new(NodeId::new(id), net::NET_OUT_TYPE_KEY)
        .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER])
}

fn setup(comp: &Composition, networks: &[&Graph]) -> (Evaluator, Graph, NodeId) {
    let result = compile_composition(comp, Graph::new()).expect("compile succeeds");
    let gpu = GpuContext::new_blocking().expect("GPU adapter required for registration");
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = shared_texture_pool(&gpu);
    let mut evaluator = Evaluator::new();
    register_all_processors(&mut evaluator, &result.graph, &gpu, &mut shaders, &pool);
    for network in networks {
        register_all_processors(&mut evaluator, network, &gpu, &mut shaders, &pool);
    }
    (evaluator, result.graph, result.output_node)
}

// ===========================================================================
// Layer-local time (REQ-LAYER-006)
// ===========================================================================

/// `net.in(t) → net.out(frame)`: the boundary returns the layer-local time
/// in seconds, so tests can observe exactly which context the network saw.
fn time_probe_network() -> Graph {
    Graph::new()
        .add_node(in_node(600))
        .unwrap()
        .add_node(out_node(601))
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            NodeId::new(600),
            OutputPortIndex(1), // `t`
            NodeId::new(601),
            InputPortIndex(0), // `frame`
        )
        .unwrap()
}

#[test]
fn network_evaluates_in_layer_local_time() {
    let network = time_probe_network();
    let comp = Composition::new(CompId::new(1), "Time", (16, 16), FPS, 300)
        .add_layer(Layer::new(LayerId::new(1), "Probe", network.clone()).with_time(10, 0, 300));
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, graph, output) = setup(&comp, &[&network]);
    evaluator.set_document(Arc::new(doc));

    // Comp frame 15 → local frame 5 → t = 5/30 s.
    let ctx = EvalContext::new(15, FPS, (16, 16));
    let out = evaluator.evaluate(&graph, output, &ctx).unwrap();
    let t = out.downcast_ref::<Scalar>().unwrap();
    assert!((t.0 - 5.0 / 30.0).abs() < 1e-6, "t = {}", t.0);

    // Comp frame 25 → local frame 15 → t = 15/30 s. The boundary sees a new
    // local frame, so the network re-evaluates without any dirty marking.
    let ctx = EvalContext::new(25, FPS, (16, 16));
    let out = evaluator.evaluate(&graph, output, &ctx).unwrap();
    let t = out.downcast_ref::<Scalar>().unwrap();
    assert!((t.0 - 15.0 / 30.0).abs() < 1e-6, "t = {}", t.0);
}

#[test]
fn outside_display_interval_is_transparent_without_evaluating() {
    let network = time_probe_network();
    let comp = Composition::new(CompId::new(1), "Time", (16, 16), FPS, 300)
        .add_layer(Layer::new(LayerId::new(1), "Probe", network.clone()).with_time(10, 0, 20));
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, graph, output) = setup(&comp, &[&network]);
    evaluator.set_document(Arc::new(doc));

    // Comp frame 5 < start_frame 10 → transparent frame, network not evaluated.
    let ctx = EvalContext::new(5, FPS, (16, 16));
    let out = evaluator.evaluate(&graph, output, &ctx).unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    assert!(fb.data.iter().all(|v| v.abs() < 1e-6));
}

// ===========================================================================
// Custom parameters on the In node (REQ-LAYER-002/004)
// ===========================================================================

#[test]
fn in_custom_parameter_port_flows_and_animates() {
    // net.in(amount) → net.out(frame); `amount` is a keyframed custom param.
    let mut curve = KeyframeCurve::new();
    curve.insert(0, 0.0, Interpolation::Linear);
    curve.insert(10, 10.0, Interpolation::Linear);

    let in_n = in_node(610)
        .with_output("amount", DataTypeId::SCALAR)
        .with_param(
            "amount",
            ParameterValue::Channel(AnimationChannel::keyframes(curve)),
        );
    let network = Graph::new()
        .add_node(in_n)
        .unwrap()
        .add_node(out_node(611))
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            NodeId::new(610),
            OutputPortIndex(3), // custom `amount` port
            NodeId::new(611),
            InputPortIndex(0),
        )
        .unwrap();

    let comp = Composition::new(CompId::new(1), "Params", (16, 16), FPS, 300)
        .add_layer(Layer::new(LayerId::new(1), "ParamLayer", network.clone()).with_time(0, 0, 300));
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, graph, output) = setup(&comp, &[&network]);
    evaluator.set_document(Arc::new(doc));

    let v0 = evaluator
        .evaluate(&graph, output, &EvalContext::new(0, FPS, (16, 16)))
        .unwrap();
    assert!((v0.downcast_ref::<Scalar>().unwrap().0 - 0.0).abs() < 1e-4);

    let v5 = evaluator
        .evaluate(&graph, output, &EvalContext::new(5, FPS, (16, 16)))
        .unwrap();
    assert!((v5.downcast_ref::<Scalar>().unwrap().0 - 5.0).abs() < 1e-4);
}

// ===========================================================================
// Adjustment layers (REQ-LAYER-010)
// ===========================================================================

#[test]
fn adjustment_layer_receives_composited_lower_stack() {
    // Layer 1: solid red source → out(frame).
    let l1_source =
        Node::new(NodeId::new(700), "test.fb").with_output("output", DataTypeId::FRAME_BUFFER);
    let l1_out = out_node(701);
    let l1_network = Graph::new()
        .add_node(l1_source)
        .unwrap()
        .add_node(l1_out)
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            NodeId::new(700),
            OutputPortIndex(0),
            NodeId::new(701),
            InputPortIndex(0),
        )
        .unwrap();

    // Layer 2 (adjustment): net.in(source) → out(frame) — passes the lower
    // stack through untouched, which is observable at the final output.
    let l2_in = in_node(702);
    let l2_out = out_node(703);
    let l2_network = Graph::new()
        .add_node(l2_in)
        .unwrap()
        .add_node(l2_out)
        .unwrap()
        .add_edge(
            EdgeId::new(2),
            NodeId::new(702),
            OutputPortIndex(2), // `source`
            NodeId::new(703),
            InputPortIndex(0),
        )
        .unwrap();

    let mut layer2 = Layer::new(LayerId::new(2), "Adj", l2_network.clone()).with_time(0, 0, 300);
    layer2.adjustment = true;
    let comp = Composition::new(CompId::new(1), "Adj", (8, 8), FPS, 300)
        .add_layer(Layer::new(LayerId::new(1), "Red", l1_network.clone()).with_time(0, 0, 300))
        .add_layer(layer2);
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, graph, output) = setup(&comp, &[&l1_network, &l2_network]);
    evaluator.register(
        NodeId::new(700),
        Arc::new(FbSource(solid_fb(8, 8, [0.5, 0.0, 0.0, 1.0]))),
    );
    evaluator.set_document(Arc::new(doc));

    let out = evaluator
        .evaluate(&graph, output, &EvalContext::new(0, FPS, (8, 8)))
        .unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    assert!(
        fb.data
            .chunks_exact(4)
            .all(|p| (p[0] - 0.5).abs() < 1e-6 && p[1].abs() < 1e-6 && p[3] > 0.9),
        "adjustment passes the lower stack through"
    );
}

// ===========================================================================
// Null layers (REQ-LAYER-005)
// ===========================================================================

#[test]
fn null_layer_is_excluded_from_merge_chain() {
    // Layer 1: solid green source. Layer 2: null (empty network).
    let l1_source =
        Node::new(NodeId::new(710), "test.fb").with_output("output", DataTypeId::FRAME_BUFFER);
    let l1_network = Graph::new()
        .add_node(l1_source)
        .unwrap()
        .add_node(out_node(711))
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            NodeId::new(710),
            OutputPortIndex(0),
            NodeId::new(711),
            InputPortIndex(0),
        )
        .unwrap();

    let comp = Composition::new(CompId::new(1), "Null", (8, 8), FPS, 300)
        .add_layer(Layer::new(LayerId::new(1), "Green", l1_network.clone()).with_time(0, 0, 300))
        .add_layer(Layer::new(LayerId::new(2), "Null", Graph::new()).with_time(0, 0, 300));
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, graph, output) = setup(&comp, &[&l1_network]);
    evaluator.register(
        NodeId::new(710),
        Arc::new(FbSource(solid_fb(8, 8, [0.0, 0.5, 0.0, 1.0]))),
    );
    evaluator.set_document(Arc::new(doc));

    let out = evaluator
        .evaluate(&graph, output, &EvalContext::new(0, FPS, (8, 8)))
        .unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    assert!(
        fb.data
            .chunks_exact(4)
            .all(|p| p[0].abs() < 1e-6 && (p[1] - 0.5).abs() < 1e-6 && p[3] > 0.9),
        "null layer does not affect the composite"
    );
}
