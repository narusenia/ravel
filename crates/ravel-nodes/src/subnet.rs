// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `subnet` — a node owning a nested graph (REQ-LAYER-003).
//!
//! The inner graph lives on [`Node::subnet`] and contains its own `net.in` /
//! `net.out` pair. The inner In's custom output ports define the subnet's
//! input pins; the inner Out's input ports define its output pins (any
//! types, multiple allowed). Pulling a subnet output binds the outer input
//! values to the inner In and recursively evaluates the inner Out through
//! [`EvalScope::evaluate_sub`] with [`PathSegment::Subnet`] — the same
//! mechanism as the layer network boundary, so nesting depth is unbounded
//! and cache/dirty state stays per ownership path (REQ-LAYER-009).
//!
//! An **unconnected** input pin resolves from the subnet node's own
//! parameter of the same name (Houdini-style promotion); if that parameter
//! is absent too, the inner In falls back to its own defaults.

use ravel_core::eval::{
    BindingValues, Bindings, EvalContext, EvalError, EvalScope, NodeProcessor, PathSegment, Rebind,
    ResolvedParams,
};
use ravel_core::graph::Node;
use ravel_core::network as net;
use ravel_core::types::{NodeData, PortRecord};
use std::sync::Arc;

use crate::net::custom_param_value;

pub struct SubnetProcessor;

impl SubnetProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

/// Bind outer pins to the inner In by name: connected pins pass their value
/// through; unconnected pins promote the subnet node's own parameter of the
/// same name (REQ-LAYER-003).
///
/// The one place the pins are built, so a time-shifted read of them
/// ([`SubnetPins`]) follows the rule the first read did.
fn pin_bindings(
    node: &Node,
    inputs: &[Option<Arc<dyn NodeData>>],
    params: &ResolvedParams,
    ctx: &EvalContext,
) -> BindingValues {
    let mut bindings: Vec<(String, Arc<dyn NodeData>)> = Vec::new();
    for (index, port) in node.inputs.iter().enumerate() {
        if let Some(value) = inputs.get(index).and_then(|v| v.clone()) {
            bindings.push((port.name.clone(), value));
        } else if params.get(&port.name).is_some() {
            let data_type = port
                .accepted_types
                .first()
                .copied()
                .unwrap_or(ravel_core::id::DataTypeId::SCALAR);
            bindings.push((
                port.name.clone(),
                custom_param_value(&port.name, data_type, params, ctx),
            ));
        }
    }
    bindings
}

/// The pins at another frame: the subnet node's inputs and parameters pulled
/// for that frame in the scope the subnet sits in, bound by [`pin_bindings`].
/// The subnet's inner time is its outer time, so `at` is used as it is.
struct SubnetPins;

impl Rebind for SubnetPins {
    fn rebind(
        &self,
        scope: &mut dyn EvalScope,
        at: &EvalContext,
    ) -> Result<BindingValues, EvalError> {
        let args = scope.process_args_at(at)?;
        Ok(pin_bindings(&args.node, &args.inputs, &args.params, at))
    }
}

impl NodeProcessor for SubnetProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let inner = node
            .subnet
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("subnet: node {} has no inner graph", node.id))?
            .clone();
        let out_node = net::find_out_node(&inner)
            .ok_or_else(|| anyhow::anyhow!("subnet: inner graph has no net.out node"))?
            .clone();

        let bindings = Bindings::rebindable(
            pin_bindings(node, inputs, params, ctx),
            Arc::new(SubnetPins),
        );

        let value = scope.evaluate_sub(
            PathSegment::Subnet(node.id),
            &inner,
            out_node.id,
            ctx,
            bindings,
        )?;
        let record = value
            .downcast_ref::<PortRecord>()
            .ok_or_else(|| anyhow::anyhow!("subnet: inner net.out produced no port record"))?;

        // Map the inner Out's record (inner input-port order) onto the
        // subnet's declared output pins by name; positional fallback keeps
        // name-mismatched setups usable.
        let entry = |name: &str, index: usize| -> anyhow::Result<Arc<dyn NodeData>> {
            out_node
                .inputs
                .iter()
                .position(|p| p.name == name)
                .or(Some(index))
                .and_then(|i| record.0.get(i).cloned())
                .ok_or_else(|| anyhow::anyhow!("subnet: no inner out port for pin {name:?}"))
        };
        if node.outputs.len() <= 1 {
            let name = node.outputs.first().map(|p| p.name.as_str()).unwrap_or("");
            return entry(name, 0);
        }
        let values = node
            .outputs
            .iter()
            .enumerate()
            .map(|(i, p)| entry(&p.name, i))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Arc::new(PortRecord(values)))
    }

    fn is_time_dependent(&self) -> bool {
        // The inner network may be time-driven; inner nodes keep their own
        // scoped caches, so re-pulling per frame is cheap when it is not.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constant::ConstantProcessor;
    use crate::net::{NetInProcessor, NetOutProcessor};
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::{FrameRate, Scalar};

    fn ctx_at(frame: u64) -> EvalContext {
        EvalContext::new(frame, FrameRate::new(30, 1), (16, 16))
    }

    /// Inner graph `net.in(amount) → net.out(result)`; the In's own default
    /// for `amount` is `inner_default`.
    fn passthrough_inner(in_id: u64, out_id: u64, inner_default: f32) -> Graph {
        let in_node = Node::new(NodeId::new(in_id), net::NET_IN_TYPE_KEY)
            .with_output("amount", DataTypeId::SCALAR)
            .with_param("amount", ParameterValue::Float(inner_default));
        let out_node = Node::new(NodeId::new(out_id), net::NET_OUT_TYPE_KEY)
            .with_input("result", &[DataTypeId::SCALAR]);
        Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(out_node)
            .unwrap()
            .add_edge(
                EdgeId::new(in_id * 10),
                NodeId::new(in_id),
                OutputPortIndex(0),
                NodeId::new(out_id),
                InputPortIndex(0),
            )
            .unwrap()
    }

    fn register_net(ev: &mut Evaluator, graph: &Graph) {
        for node in graph.nodes() {
            match node.type_key.as_str() {
                key if key == net::NET_IN_TYPE_KEY => {
                    ev.register(node.id, Arc::new(NetInProcessor::from_node(node)));
                }
                key if key == net::NET_OUT_TYPE_KEY => {
                    ev.register(node.id, Arc::new(NetOutProcessor::from_node(node)));
                }
                "subnet" => {
                    ev.register(node.id, Arc::new(SubnetProcessor::from_node(node)));
                    if let Some(inner) = node.subnet.as_deref() {
                        register_net(ev, inner);
                    }
                }
                "constant" => {
                    ev.register(node.id, Arc::new(ConstantProcessor::from_node(node)));
                }
                _ => {}
            }
        }
    }

    #[test]
    fn unconnected_pin_promotes_subnet_parameter() {
        let subnet_node = Node::new(NodeId::new(5), "subnet")
            .with_input("amount", &[DataTypeId::SCALAR])
            .with_output("result", DataTypeId::SCALAR)
            .with_param("amount", ParameterValue::Float(7.5))
            .with_subnet(passthrough_inner(10, 11, 1.0));
        let graph = Graph::new().add_node(subnet_node).unwrap();
        let mut ev = Evaluator::new();
        register_net(&mut ev, &graph);

        let out = ev.evaluate(&graph, NodeId::new(5), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 7.5).abs() < 1e-6);
    }

    #[test]
    fn connected_pin_overrides_promoted_parameter() {
        let constant = Node::new(NodeId::new(4), "constant")
            .with_output("value", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(3.0));
        let subnet_node = Node::new(NodeId::new(5), "subnet")
            .with_input("amount", &[DataTypeId::SCALAR])
            .with_output("result", DataTypeId::SCALAR)
            .with_param("amount", ParameterValue::Float(7.5))
            .with_subnet(passthrough_inner(10, 11, 1.0));
        let graph = Graph::new()
            .add_node(constant)
            .unwrap()
            .add_node(subnet_node)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(4),
                OutputPortIndex(0),
                NodeId::new(5),
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        register_net(&mut ev, &graph);

        let out = ev.evaluate(&graph, NodeId::new(5), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 3.0).abs() < 1e-6);
    }

    #[test]
    fn inner_default_applies_without_pin_or_parameter() {
        let subnet_node = Node::new(NodeId::new(5), "subnet")
            .with_input("amount", &[DataTypeId::SCALAR])
            .with_output("result", DataTypeId::SCALAR)
            .with_subnet(passthrough_inner(10, 11, 2.0));
        let graph = Graph::new().add_node(subnet_node).unwrap();
        let mut ev = Evaluator::new();
        register_net(&mut ev, &graph);

        let out = ev.evaluate(&graph, NodeId::new(5), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 2.0).abs() < 1e-6);
    }

    #[test]
    fn nested_subnets_evaluate_recursively() {
        // Innermost: net.in(t) → net.out(value); the fixed `t` port makes
        // the nested context observable through two subnet levels.
        let innermost = {
            let in_node = Node::new(NodeId::new(20), net::NET_IN_TYPE_KEY)
                .with_output(net::PORT_TIME, DataTypeId::SCALAR);
            let out_node = Node::new(NodeId::new(21), net::NET_OUT_TYPE_KEY)
                .with_input("value", &[DataTypeId::SCALAR]);
            Graph::new()
                .add_node(in_node)
                .unwrap()
                .add_node(out_node)
                .unwrap()
                .add_edge(
                    EdgeId::new(200),
                    NodeId::new(20),
                    OutputPortIndex(0),
                    NodeId::new(21),
                    InputPortIndex(0),
                )
                .unwrap()
        };
        let inner_subnet = Node::new(NodeId::new(15), "subnet")
            .with_output("value", DataTypeId::SCALAR)
            .with_subnet(innermost);
        let mid = {
            let out_node = Node::new(NodeId::new(16), net::NET_OUT_TYPE_KEY)
                .with_input("value", &[DataTypeId::SCALAR]);
            Graph::new()
                .add_node(inner_subnet)
                .unwrap()
                .add_node(out_node)
                .unwrap()
                .add_edge(
                    EdgeId::new(150),
                    NodeId::new(15),
                    OutputPortIndex(0),
                    NodeId::new(16),
                    InputPortIndex(0),
                )
                .unwrap()
        };
        let outer_subnet = Node::new(NodeId::new(5), "subnet")
            .with_output("value", DataTypeId::SCALAR)
            .with_subnet(mid);
        let graph = Graph::new().add_node(outer_subnet).unwrap();
        let mut ev = Evaluator::new();
        register_net(&mut ev, &graph);

        // Frame 15 at 30 fps → t = 0.5 s, seen through both nesting levels.
        let out = ev.evaluate(&graph, NodeId::new(5), &ctx_at(15)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 0.5).abs() < 1e-6);
    }

    #[test]
    fn multi_output_subnet_yields_port_record() {
        let in_node = Node::new(NodeId::new(10), net::NET_IN_TYPE_KEY)
            .with_output("a", DataTypeId::SCALAR)
            .with_output("b", DataTypeId::SCALAR)
            .with_param("a", ParameterValue::Float(1.0))
            .with_param("b", ParameterValue::Float(2.0));
        let out_node = Node::new(NodeId::new(11), net::NET_OUT_TYPE_KEY)
            .with_input("a", &[DataTypeId::SCALAR])
            .with_input("b", &[DataTypeId::SCALAR]);
        let inner = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(out_node)
            .unwrap()
            .add_edge(
                EdgeId::new(100),
                NodeId::new(10),
                OutputPortIndex(0),
                NodeId::new(11),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(101),
                NodeId::new(10),
                OutputPortIndex(1),
                NodeId::new(11),
                InputPortIndex(1),
            )
            .unwrap();
        // Outputs declared in reverse order to prove name-based mapping.
        let subnet_node = Node::new(NodeId::new(5), "subnet")
            .with_output("b", DataTypeId::SCALAR)
            .with_output("a", DataTypeId::SCALAR)
            .with_subnet(inner);
        let graph = Graph::new().add_node(subnet_node).unwrap();
        let mut ev = Evaluator::new();
        register_net(&mut ev, &graph);

        let out = ev.evaluate(&graph, NodeId::new(5), &ctx_at(0)).unwrap();
        let record = out.downcast_ref::<PortRecord>().unwrap();
        assert!((record.0[0].downcast_ref::<Scalar>().unwrap().0 - 2.0).abs() < 1e-6);
        assert!((record.0[1].downcast_ref::<Scalar>().unwrap().0 - 1.0).abs() < 1e-6);
    }

    /// Regression: an unconnected `GEOMETRY` pin with no promoted parameter
    /// reaches the inner In's fallback, which must answer with an empty
    /// geometry rather than a scalar the downstream node cannot use
    /// (network-interface plan, problem 4).
    #[test]
    fn unconnected_geometry_pin_yields_an_empty_geometry() {
        let in_node = Node::new(NodeId::new(10), net::NET_IN_TYPE_KEY)
            .with_output("geo", DataTypeId::GEOMETRY);
        let out_node = Node::new(NodeId::new(11), net::NET_OUT_TYPE_KEY)
            .with_input("geo", &[DataTypeId::GEOMETRY]);
        let inner = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(out_node)
            .unwrap()
            .add_edge(
                EdgeId::new(100),
                NodeId::new(10),
                OutputPortIndex(0),
                NodeId::new(11),
                InputPortIndex(0),
            )
            .unwrap();
        let subnet_node = Node::new(NodeId::new(5), "subnet")
            .with_input("geo", &[DataTypeId::GEOMETRY])
            .with_output("geo", DataTypeId::GEOMETRY)
            .with_subnet(inner);
        let graph = Graph::new().add_node(subnet_node).unwrap();
        let mut ev = Evaluator::new();
        register_net(&mut ev, &graph);

        let out = ev.evaluate(&graph, NodeId::new(5), &ctx_at(0)).unwrap();
        let geo = out
            .downcast_ref::<ravel_core::geometry::Geometry>()
            .expect("an unconnected GEOMETRY pin must not produce a Scalar");
        assert_eq!(geo.points().element_count(), 0);
    }

    /// A Subnet added from the node palette evaluates. Its inner graph is a
    /// bare In / Out pair, so the answer is the `frame` port's typed zero —
    /// what matters is that it is an answer and not
    /// `subnet: node N has no inner graph`.
    #[test]
    fn a_subnet_created_from_the_registry_evaluates_empty() {
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        let node = registry
            .create_node(net::SUBNET_TYPE_KEY, NodeId::new(5))
            .expect("the subnet template is registered");
        assert!(node.subnet.is_some(), "double-clicking it must descend");

        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        register_net(&mut ev, &graph);

        let out = ev.evaluate(&graph, NodeId::new(5), &ctx_at(0)).unwrap();
        let frame = out
            .downcast_ref::<ravel_core::types::FrameBuffer>()
            .expect("the inner net.out `frame` port becomes the subnet's pin");
        assert_eq!((frame.width, frame.height), (16, 16));
    }

    /// Output pins resolve by **name**, not by slot: a lone pin naming the
    /// inner Out's second port reads that port. Positional resolution would
    /// answer with the first one.
    #[test]
    fn a_single_output_pin_resolves_by_name_not_position() {
        let in_node = Node::new(NodeId::new(10), net::NET_IN_TYPE_KEY)
            .with_output("a", DataTypeId::SCALAR)
            .with_output("b", DataTypeId::SCALAR)
            .with_param("a", ParameterValue::Float(1.0))
            .with_param("b", ParameterValue::Float(2.0));
        let out_node = Node::new(NodeId::new(11), net::NET_OUT_TYPE_KEY)
            .with_input("a", &[DataTypeId::SCALAR])
            .with_input("b", &[DataTypeId::SCALAR]);
        let inner = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(out_node)
            .unwrap()
            .add_edge(
                EdgeId::new(100),
                NodeId::new(10),
                OutputPortIndex(0),
                NodeId::new(11),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(101),
                NodeId::new(10),
                OutputPortIndex(1),
                NodeId::new(11),
                InputPortIndex(1),
            )
            .unwrap();
        let subnet_node = Node::new(NodeId::new(5), "subnet")
            .with_output("b", DataTypeId::SCALAR)
            .with_subnet(inner);
        let graph = Graph::new().add_node(subnet_node).unwrap();
        let mut ev = Evaluator::new();
        register_net(&mut ev, &graph);

        let out = ev.evaluate(&graph, NodeId::new(5), &ctx_at(0)).unwrap();
        assert!((out.downcast_ref::<Scalar>().unwrap().0 - 2.0).abs() < 1e-6);
    }

    #[test]
    fn missing_inner_graph_is_an_error() {
        let subnet_node =
            Node::new(NodeId::new(5), "subnet").with_output("out", DataTypeId::SCALAR);
        let graph = Graph::new().add_node(subnet_node).unwrap();
        let mut ev = Evaluator::new();
        register_net(&mut ev, &graph);
        // No processor registered without a subnet arm match — register
        // explicitly to reach the processor's own validation.
        ev.register(NodeId::new(5), Arc::new(SubnetProcessor));
        assert!(ev.evaluate(&graph, NodeId::new(5), &ctx_at(0)).is_err());
    }

    // ---- bindings read at the frame being pulled (scope inputs) -------------

    use crate::sim_test_util::{Accumulate, FrameScalar, ShiftBack};
    use ravel_core::cache_budget::{CacheBudgetConfig, SharedCacheBudget};
    use ravel_core::eval::stateful_processor;

    const SOURCE: u64 = 1;
    const SUBNET: u64 = 5;
    const INNER_IN: u64 = 10;
    const INNER_OUT: u64 = 11;
    const INNER_MID: u64 = 12;

    fn wire(graph: Graph, edge: u64, from: u64, to: u64) -> Graph {
        graph
            .add_edge(
                EdgeId::new(edge),
                NodeId::new(from),
                OutputPortIndex(0),
                NodeId::new(to),
                InputPortIndex(0),
            )
            .unwrap()
    }

    /// `FrameScalar(1) -> subnet.amount`, the subnet's body being
    /// `net.in(amount) -> mid -> net.out(result)`; `mid` is `type_key`.
    fn animated_pin_graph(type_key: &str) -> Graph {
        let inner = Graph::new()
            .add_node(
                Node::new(NodeId::new(INNER_IN), net::NET_IN_TYPE_KEY)
                    .with_output("amount", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(INNER_MID), type_key)
                    .with_input("a", &[DataTypeId::SCALAR])
                    .with_output("out", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(INNER_OUT), net::NET_OUT_TYPE_KEY)
                    .with_input("result", &[DataTypeId::SCALAR]),
            )
            .unwrap();
        let inner = wire(
            wire(inner, 100, INNER_IN, INNER_MID),
            101,
            INNER_MID,
            INNER_OUT,
        );
        let subnet = Node::new(NodeId::new(SUBNET), "subnet")
            .with_input("amount", &[DataTypeId::SCALAR])
            .with_output("result", DataTypeId::SCALAR)
            .with_subnet(inner);
        let root = Graph::new()
            .add_node(
                Node::new(NodeId::new(SOURCE), "test.frame")
                    .with_output("out", DataTypeId::SCALAR)
                    .with_param("k", ParameterValue::Float(1.0)),
            )
            .unwrap()
            .add_node(subnet)
            .unwrap();
        wire(root, 1, SOURCE, SUBNET)
    }

    fn pin_evaluator(mid: Arc<dyn NodeProcessor>, graph: &Graph, ev: Evaluator) -> Evaluator {
        let mut ev = ev;
        register_net(&mut ev, graph);
        ev.register(NodeId::new(SOURCE), Arc::new(FrameScalar(1.0)));
        ev.register(NodeId::new(INNER_MID), mid);
        ev
    }

    fn scalar(out: &Arc<dyn NodeData>) -> f32 {
        out.downcast_ref::<Scalar>().expect("a scalar").0
    }

    /// `0 + 1 + .. + t`: what a simulation summing its pin reads when every
    /// step is given the pin's value **at that step's frame**.
    fn triangle(t: u64) -> f32 {
        (t * (t + 1) / 2) as f32
    }

    #[test]
    fn a_sim_in_a_subnet_gets_the_pin_of_each_step_frame_while_filling() {
        let graph = animated_pin_graph("test.sim");
        let mid = stateful_processor(Accumulate::default());
        let mut ev = pin_evaluator(mid, &graph, Evaluator::new());

        let out = ev
            .evaluate(&graph, NodeId::new(SUBNET), &ctx_at(6))
            .unwrap();

        // Every step reading frame 6's pin would give 7 * 6 = 42.
        assert_eq!(scalar(&out), triangle(6));
    }

    #[test]
    fn a_thinned_sim_in_a_subnet_replays_with_the_pin_of_each_step_frame() {
        let graph = animated_pin_graph("test.sim");
        let mid = stateful_processor(Accumulate {
            state_bytes: 100,
            ..Accumulate::default()
        });
        // 4000 bytes of RAM: a 1000-byte share, so a 100-frame track of
        // 100-byte states has to thin.
        let budget = SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: 1 << 20,
            ram_bytes: 4000,
            disk_bytes: 0,
            sim_reserve_ratio: 0.25,
        });
        let mut ev = pin_evaluator(mid, &graph, Evaluator::with_budget(budget));
        ev.evaluate(&graph, NodeId::new(SUBNET), &ctx_at(99))
            .unwrap();
        let path = [PathSegment::Subnet(NodeId::new(SUBNET))];
        let inner = NodeId::new(INNER_MID);
        let stored = ev.sim_stored_states(&path, inner).unwrap();
        let covered = ev.sim_cached_frames(&path, inner).unwrap();
        assert!(
            (stored as u64) < covered.end - covered.start,
            "nothing thinned, so no replay is exercised"
        );

        for frame in (0..100).rev().chain((0..100).map(|f| (f * 37) % 100)) {
            let out = ev
                .evaluate(&graph, NodeId::new(SUBNET), &ctx_at(frame))
                .unwrap();
            assert_eq!(scalar(&out), triangle(frame), "frame {frame} differs");
        }
    }

    #[test]
    fn a_stateless_time_shift_reads_the_pin_at_the_shifted_frame() {
        let graph = animated_pin_graph("test.shift");
        let mut ev = pin_evaluator(Arc::new(ShiftBack(4)), &graph, Evaluator::new());

        let out = ev
            .evaluate(&graph, NodeId::new(SUBNET), &ctx_at(10))
            .unwrap();

        // The pin at frame 10 is 10; the shifted read must see frame 6.
        assert_eq!(scalar(&out), 6.0);
        // And a different frame is not served from the first answer.
        let again = ev
            .evaluate(&graph, NodeId::new(SUBNET), &ctx_at(11))
            .unwrap();
        assert_eq!(scalar(&again), 7.0);
    }

    #[test]
    fn a_shift_inside_a_shift_re_pulls_the_pin_from_the_frame_it_lands_on() {
        // net.in -> shift(4) -> shift(3) -> net.out: 10 -> 6 -> 3.
        let mut graph = animated_pin_graph("test.shift");
        let mut subnet = (**graph.node(NodeId::new(SUBNET)).unwrap()).clone();
        let inner = subnet.subnet.as_deref().unwrap().clone();
        let second = Node::new(NodeId::new(13), "test.shift2")
            .with_input("a", &[DataTypeId::SCALAR])
            .with_output("out", DataTypeId::SCALAR);
        let edge_to_out = inner
            .edges()
            .find(|e| e.target == NodeId::new(INNER_OUT))
            .unwrap()
            .id;
        let inner = wire(
            wire(
                inner
                    .remove_edge(edge_to_out)
                    .unwrap()
                    .add_node(second)
                    .unwrap(),
                102,
                INNER_MID,
                13,
            ),
            103,
            13,
            INNER_OUT,
        );
        subnet.subnet = Some(Arc::new(inner));
        graph = graph.replace_node(Arc::new(subnet));
        let mut ev = pin_evaluator(Arc::new(ShiftBack(4)), &graph, Evaluator::new());
        ev.register(NodeId::new(13), Arc::new(ShiftBack(3)));

        let out = ev
            .evaluate(&graph, NodeId::new(SUBNET), &ctx_at(10))
            .unwrap();

        assert_eq!(scalar(&out), 3.0);
    }

    // ---- the premise includes what feeds the pins ---------------------------

    /// Evaluate the subnet at `frame` and report how many times the inner
    /// simulation has started from nothing.
    fn starts(ev: &mut Evaluator, graph: &Graph, counter: &Accumulate, frame: u64) -> usize {
        ev.evaluate(graph, NodeId::new(SUBNET), &ctx_at(frame))
            .unwrap();
        counter.initials.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn with_param(graph: &Graph, node: u64, key: &str, value: f32) -> Graph {
        graph
            .clone()
            .set_params(
                NodeId::new(node),
                &[ravel_core::graph::Parameter {
                    key: key.into(),
                    value: ParameterValue::Float(value),
                }],
            )
            .unwrap()
    }

    #[test]
    fn editing_what_feeds_a_pin_discards_the_inner_sims_track() {
        let counter = Accumulate::default();
        let graph = animated_pin_graph("test.sim");
        let mut ev = pin_evaluator(
            stateful_processor(counter.clone()),
            &graph,
            Evaluator::new(),
        );
        assert_eq!(starts(&mut ev, &graph, &counter, 6), 1);

        // The pin's source changes; the subnet's inner network is the same
        // object, so only the producing scope's upstream can tell.
        let edited = with_param(&graph, SOURCE, "k", 2.0);
        ev.mark_dirty(&edited, NodeId::new(SOURCE));

        assert_eq!(
            starts(&mut ev, &edited, &counter, 8),
            2,
            "the track outlived an edit to what feeds its pin"
        );
    }

    #[test]
    fn an_unrelated_edit_keeps_the_inner_sims_track() {
        let counter = Accumulate::default();
        let graph = animated_pin_graph("test.sim");
        let mut ev = pin_evaluator(
            stateful_processor(counter.clone()),
            &graph,
            Evaluator::new(),
        );
        assert_eq!(starts(&mut ev, &graph, &counter, 6), 1);

        // A node in the subnet's own network that nothing upstream of the sim
        // reads, and one in the outer graph that does not feed the pin.
        let mut subnet = (**graph.node(NodeId::new(SUBNET)).unwrap()).clone();
        let inner = subnet.subnet.as_deref().unwrap().clone();
        subnet.subnet = Some(Arc::new(
            inner
                .add_node(
                    Node::new(NodeId::new(40), "test.frame").with_output("out", DataTypeId::SCALAR),
                )
                .unwrap(),
        ));
        let edited = graph
            .replace_node(Arc::new(subnet))
            .add_node(
                Node::new(NodeId::new(41), "test.frame").with_output("out", DataTypeId::SCALAR),
            )
            .unwrap();
        ev.mark_dirty(&edited, NodeId::new(SUBNET));

        assert_eq!(
            starts(&mut ev, &edited, &counter, 8),
            1,
            "an unrelated edit restarted the simulation"
        );
    }

    #[test]
    fn invalidating_what_feeds_a_pin_discards_the_inner_sims_track() {
        let counter = Accumulate::default();
        let graph = animated_pin_graph("test.sim");
        let mut ev = pin_evaluator(
            stateful_processor(counter.clone()),
            &graph,
            Evaluator::new(),
        );
        assert_eq!(starts(&mut ev, &graph, &counter, 6), 1);

        // The pin's source node is invalidated (a media reload) with the
        // graph untouched; only the producing scope's epochs can tell.
        ev.invalidate_node(NodeId::new(SOURCE));

        assert_eq!(starts(&mut ev, &graph, &counter, 8), 2);
    }
}
