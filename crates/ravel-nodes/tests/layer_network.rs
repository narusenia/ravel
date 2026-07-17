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

// ===========================================================================
// Regression: adjustment bypass and binding staleness
// ===========================================================================

/// Layer 1 with a `test.fb` source of node id `source_id` feeding out(frame).
fn fb_source_network(source_id: u64, out_id: u64) -> Graph {
    let source = Node::new(NodeId::new(source_id), "test.fb")
        .with_output("output", DataTypeId::FRAME_BUFFER);
    Graph::new()
        .add_node(source)
        .unwrap()
        .add_node(out_node(out_id))
        .unwrap()
        .add_edge(
            EdgeId::new(source_id * 10),
            NodeId::new(source_id),
            OutputPortIndex(0),
            NodeId::new(out_id),
            InputPortIndex(0),
        )
        .unwrap()
}

/// Adjustment network: `net.in(source) → out(frame)` passthrough.
fn adjustment_passthrough_network(in_id: u64, out_id: u64) -> Graph {
    Graph::new()
        .add_node(in_node(in_id))
        .unwrap()
        .add_node(out_node(out_id))
        .unwrap()
        .add_edge(
            EdgeId::new(in_id * 10),
            NodeId::new(in_id),
            OutputPortIndex(2), // `source`
            NodeId::new(out_id),
            InputPortIndex(0),
        )
        .unwrap()
}

#[test]
fn adjustment_inactive_passes_background() {
    // Layer 2 is an adjustment layer starting at frame 10: at frame 5 it is
    // outside its interval and must not blank the composite.
    let l1_network = fb_source_network(700, 701);
    let l2_network = adjustment_passthrough_network(702, 703);

    let mut layer2 = Layer::new(LayerId::new(2), "Adj", l2_network.clone()).with_time(10, 0, 300);
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
        .evaluate(&graph, output, &EvalContext::new(5, FPS, (8, 8)))
        .unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    assert!(
        fb.data
            .chunks_exact(4)
            .all(|p| (p[0] - 0.5).abs() < 1e-6 && p[3] > 0.9),
        "inactive adjustment must pass the background through"
    );
}

#[test]
fn adjustment_tracks_lower_stack_edits_at_same_frame() {
    // doc1: layer 1 = red source (node 700); doc2: layer 1 = blue source
    // (node 720, structurally different network). Same frame re-evaluation
    // must observe the edit (scoped-cache invalidation on document swap).
    let l1_red = fb_source_network(700, 701);
    let l1_blue = fb_source_network(720, 721);
    let l2_network = adjustment_passthrough_network(702, 703);

    let mut layer2 = Layer::new(LayerId::new(2), "Adj", l2_network.clone()).with_time(0, 0, 300);
    layer2.adjustment = true;

    let make_doc = |l1_network: Graph| {
        Document::default().with_composition(
            Composition::new(CompId::new(1), "Adj", (8, 8), FPS, 300)
                .add_layer(Layer::new(LayerId::new(1), "Source", l1_network).with_time(0, 0, 300))
                .add_layer(layer2.clone()),
        )
    };
    let comp = make_doc(l1_red.clone())
        .get_composition(CompId::new(1))
        .unwrap()
        .as_ref()
        .clone();

    let (mut evaluator, graph, output) = setup(&comp, &[&l1_red, &l1_blue, &l2_network]);
    evaluator.register(
        NodeId::new(700),
        Arc::new(FbSource(solid_fb(8, 8, [0.5, 0.0, 0.0, 1.0]))),
    );
    evaluator.register(
        NodeId::new(720),
        Arc::new(FbSource(solid_fb(8, 8, [0.0, 0.0, 0.5, 1.0]))),
    );

    evaluator.set_document(Arc::new(make_doc(l1_red)));
    let out = evaluator
        .evaluate(&graph, output, &EvalContext::new(0, FPS, (8, 8)))
        .unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    assert!(fb.data.chunks_exact(4).all(|p| p[0] > 0.4 && p[2] < 0.1));

    // Swap to the edited document at the same frame: the adjustment layer's
    // bindings change, and the new lower stack must flow through.
    evaluator.set_document(Arc::new(make_doc(l1_blue)));
    let out = evaluator
        .evaluate(&graph, output, &EvalContext::new(0, FPS, (8, 8)))
        .unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    assert!(
        fb.data
            .chunks_exact(4)
            .all(|p| p[0] < 0.1 && (p[2] - 0.5).abs() < 1e-6),
        "edited lower stack must reach the adjustment layer at the same frame"
    );
}

// ===========================================================================
// Shell compositing: Transform / Opacity / Merge (REQ-LAYER-001/010)
// ===========================================================================

#[test]
fn shell_transform_translates_layer_pixels() {
    let network = fb_source_network(730, 731);
    let mut layer = Layer::new(LayerId::new(1), "Moved", network.clone()).with_time(0, 0, 300);
    layer.transform.position[0] = AnimationChannel::constant(3.0);
    let comp = Composition::new(CompId::new(1), "Xform", (8, 8), FPS, 300).add_layer(layer);
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, graph, output) = setup(&comp, &[&network]);
    evaluator.register(
        NodeId::new(730),
        Arc::new(FbSource(solid_fb(8, 8, [1.0, 0.0, 0.0, 1.0]))),
    );
    evaluator.set_document(Arc::new(doc));

    let out = evaluator
        .evaluate(&graph, output, &EvalContext::new(0, FPS, (8, 8)))
        .unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    let alpha = |x: u32, y: u32| fb.data[((y * 8 + x) * 4 + 3) as usize];
    // Shifted right by 3: the leftmost columns become transparent, the
    // interior stays opaque.
    assert!(alpha(1, 4) < 0.05, "vacated column transparent");
    assert!(alpha(6, 4) > 0.95, "shifted content opaque");
}

#[test]
fn shell_transform_inherits_parent_position() {
    // Parent is a null layer with a position offset; the child's own
    // transform is identity, so any shift comes from inheritance.
    let network = fb_source_network(740, 741);
    let mut parent = Layer::new(LayerId::new(1), "Null", Graph::new()).with_time(0, 0, 300);
    parent.transform.position[0] = AnimationChannel::constant(3.0);
    let child = Layer::new(LayerId::new(2), "Child", network.clone()).with_parent(LayerId::new(1));
    let child = child.with_time(0, 0, 300);
    let comp = Composition::new(CompId::new(1), "Parented", (8, 8), FPS, 300)
        .add_layer(parent)
        .add_layer(child);
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, graph, output) = setup(&comp, &[&network]);
    evaluator.register(
        NodeId::new(740),
        Arc::new(FbSource(solid_fb(8, 8, [0.0, 1.0, 0.0, 1.0]))),
    );
    evaluator.set_document(Arc::new(doc));

    let out = evaluator
        .evaluate(&graph, output, &EvalContext::new(0, FPS, (8, 8)))
        .unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    let alpha = |x: u32, y: u32| fb.data[((y * 8 + x) * 4 + 3) as usize];
    assert!(alpha(1, 4) < 0.05, "child inherits the parent offset");
    assert!(alpha(6, 4) > 0.95);
}

#[test]
fn shell_opacity_scales_alpha() {
    let network = fb_source_network(750, 751);
    let mut layer = Layer::new(LayerId::new(1), "Faded", network.clone()).with_time(0, 0, 300);
    layer.opacity = AnimationChannel::constant(0.5);
    let comp = Composition::new(CompId::new(1), "Opacity", (8, 8), FPS, 300).add_layer(layer);
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, graph, output) = setup(&comp, &[&network]);
    evaluator.register(
        NodeId::new(750),
        Arc::new(FbSource(solid_fb(8, 8, [1.0, 0.0, 0.0, 1.0]))),
    );
    evaluator.set_document(Arc::new(doc));

    let out = evaluator
        .evaluate(&graph, output, &EvalContext::new(0, FPS, (8, 8)))
        .unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    assert!(
        fb.data
            .chunks_exact(4)
            .all(|p| (p[3] - 0.5).abs() < 1e-6 && (p[0] - 1.0).abs() < 1e-6),
        "alpha halves, color stays straight"
    );
}

#[test]
fn shell_merge_composites_stack_with_normal_over() {
    // Opaque red under half-transparent green → half red, half green.
    let l1 = fb_source_network(760, 761);
    let l2 = fb_source_network(762, 763);
    let comp = Composition::new(CompId::new(1), "Over", (8, 8), FPS, 300)
        .add_layer(Layer::new(LayerId::new(1), "Red", l1.clone()).with_time(0, 0, 300))
        .add_layer(Layer::new(LayerId::new(2), "Green", l2.clone()).with_time(0, 0, 300));
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, graph, output) = setup(&comp, &[&l1, &l2]);
    evaluator.register(
        NodeId::new(760),
        Arc::new(FbSource(solid_fb(8, 8, [1.0, 0.0, 0.0, 1.0]))),
    );
    evaluator.register(
        NodeId::new(762),
        Arc::new(FbSource(solid_fb(8, 8, [0.0, 1.0, 0.0, 0.5]))),
    );
    evaluator.set_document(Arc::new(doc));

    let out = evaluator
        .evaluate(&graph, output, &EvalContext::new(0, FPS, (8, 8)))
        .unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    assert!(
        fb.data.chunks_exact(4).all(|p| (p[0] - 0.5).abs() < 1e-6
            && (p[1] - 0.5).abs() < 1e-6
            && (p[3] - 1.0).abs() < 1e-6),
        "normal over composite"
    );
}

#[test]
fn shell_merge_applies_blend_mode() {
    use ravel_core::composition::BlendMode;
    let l1 = fb_source_network(770, 771);
    let l2 = fb_source_network(772, 773);
    let top = Layer::new(LayerId::new(2), "Add", l2.clone())
        .with_time(0, 0, 300)
        .with_blend_mode(BlendMode::Add);
    let comp = Composition::new(CompId::new(1), "Add", (8, 8), FPS, 300)
        .add_layer(Layer::new(LayerId::new(1), "Base", l1.clone()).with_time(0, 0, 300))
        .add_layer(top);
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, graph, output) = setup(&comp, &[&l1, &l2]);
    evaluator.register(
        NodeId::new(770),
        Arc::new(FbSource(solid_fb(8, 8, [0.25, 0.0, 0.0, 1.0]))),
    );
    evaluator.register(
        NodeId::new(772),
        Arc::new(FbSource(solid_fb(8, 8, [0.5, 0.0, 0.0, 1.0]))),
    );
    evaluator.set_document(Arc::new(doc));

    let out = evaluator
        .evaluate(&graph, output, &EvalContext::new(0, FPS, (8, 8)))
        .unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    assert!(
        fb.data.chunks_exact(4).all(|p| (p[0] - 0.75).abs() < 1e-6),
        "additive blend sums the stack"
    );
}

#[test]
fn adjustment_opacity_mixes_effect_strength() {
    // Adjustment network replaces the stack with solid blue; at opacity 0.5
    // the result is half red (original), half blue (adjusted).
    let l1 = fb_source_network(780, 781);
    let adj_source =
        Node::new(NodeId::new(782), "test.fb").with_output("output", DataTypeId::FRAME_BUFFER);
    let adj_network = Graph::new()
        .add_node(adj_source)
        .unwrap()
        .add_node(out_node(783))
        .unwrap()
        .add_edge(
            EdgeId::new(7820),
            NodeId::new(782),
            OutputPortIndex(0),
            NodeId::new(783),
            InputPortIndex(0),
        )
        .unwrap();

    let mut adj = Layer::new(LayerId::new(2), "Adj", adj_network.clone()).with_time(0, 0, 300);
    adj.adjustment = true;
    adj.opacity = AnimationChannel::constant(0.5);
    let comp = Composition::new(CompId::new(1), "Strength", (8, 8), FPS, 300)
        .add_layer(Layer::new(LayerId::new(1), "Red", l1.clone()).with_time(0, 0, 300))
        .add_layer(adj);
    let doc = Document::default().with_composition(comp.clone());

    let (mut evaluator, graph, output) = setup(&comp, &[&l1, &adj_network]);
    evaluator.register(
        NodeId::new(780),
        Arc::new(FbSource(solid_fb(8, 8, [1.0, 0.0, 0.0, 1.0]))),
    );
    evaluator.register(
        NodeId::new(782),
        Arc::new(FbSource(solid_fb(8, 8, [0.0, 0.0, 1.0, 1.0]))),
    );
    evaluator.set_document(Arc::new(doc));

    let out = evaluator
        .evaluate(&graph, output, &EvalContext::new(0, FPS, (8, 8)))
        .unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    assert!(
        fb.data.chunks_exact(4).all(|p| (p[0] - 0.5).abs() < 1e-6
            && (p[2] - 0.5).abs() < 1e-6
            && (p[3] - 1.0).abs() < 1e-6),
        "opacity acts as adjustment strength"
    );
}

#[test]
fn shell_timing_edit_invalidates_boundary_at_same_frame() {
    // doc1: start_frame=10 → comp frame 15 sees local frame 5.
    // doc2: start_frame=20 → comp frame 15 is outside the interval.
    let network = time_probe_network();
    let make_doc = |start: i64| {
        Document::default().with_composition(
            Composition::new(CompId::new(1), "Time", (16, 16), FPS, 300).add_layer(
                Layer::new(LayerId::new(1), "Probe", network.clone()).with_time(start, 0, 300),
            ),
        )
    };
    let comp = make_doc(10)
        .get_composition(CompId::new(1))
        .unwrap()
        .as_ref()
        .clone();

    let (mut evaluator, graph, output) = setup(&comp, &[&network]);
    evaluator.set_document(Arc::new(make_doc(10)));
    let out = evaluator
        .evaluate(&graph, output, &EvalContext::new(15, FPS, (16, 16)))
        .unwrap();
    let t = out.downcast_ref::<Scalar>().unwrap();
    assert!((t.0 - 5.0 / 30.0).abs() < 1e-6);

    // Same frame, shell-only edit (start_frame): the boundary must recompute.
    evaluator.set_document(Arc::new(make_doc(20)));
    let out = evaluator
        .evaluate(&graph, output, &EvalContext::new(15, FPS, (16, 16)))
        .unwrap();
    let fb = out.downcast_ref::<FrameBuffer>().unwrap();
    assert!(
        fb.data.iter().all(|v| v.abs() < 1e-6),
        "shell timing edit must re-evaluate the boundary"
    );
}
