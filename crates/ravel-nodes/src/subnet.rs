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

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, PathSegment, ResolvedParams};
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

        // Bind outer pins to the inner In by name: connected pins pass their
        // value through; unconnected pins promote the subnet node's own
        // parameter of the same name (REQ-LAYER-003).
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
                    custom_param_value(&port.name, data_type, params),
                ));
            }
        }

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
}
