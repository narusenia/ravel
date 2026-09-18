// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `.ravprj` v12 → v13, second half: make every `layer.ref` output declare
//! the type of the port its `port` parameter names.
//!
//! # Why the type can be wrong
//!
//! Before v13 `port` was a free string, so a document may hold any port name
//! the target layer's `net.out` declares — not only the `"frame"` the
//! template defaults to. The output type that follows the named port is
//! computed on the **parameter edit** path only
//! (`apply_property_change` → [`dependent_port_updates`] →
//! [`Graph::set_params_and_output_types`]); the upgrade in
//! [`layer_ref_upgrade`](super::layer_ref_upgrade) goes through
//! [`Graph::set_params`], which retypes nothing. So a pre-v13 node whose
//! `port` names a geometry output evaluates to geometry while still declaring
//! `FRAME_BUFFER` — the node lies about what it produces until somebody
//! touches `layer` or `port` and the edit path repairs it.
//!
//! This pass runs the same computation over the loaded document, so the
//! repair does not wait for an edit.
//!
//! # Composition-scoped, not graph-scoped
//!
//! [`dependent_port_updates`] needs the [`Composition`] the node lives in:
//! the type comes from **another layer's** `net.out`, which a `Graph` cannot
//! see. So the walk is the one [`asset_upgrade`](super::asset_upgrade) uses —
//! the document's compositions, each layer's network, and every nested subnet
//! — rather than [`Document::map_graphs`](super::Document::map_graphs), which
//! hands over a graph with no composition attached.
//!
//! **The document's legacy flat graph is deliberately not walked.** It
//! belongs to no composition, so there is no sibling layer for a reference in
//! it to resolve against, and [`dependent_port_updates`] answers "no retype"
//! for a caller that cannot supply one. Walking it would be a guaranteed
//! no-op over every node of a graph that is not even evaluated (REQ-LAYER-007).
//!
//! # Edges the new type cannot carry are dropped
//!
//! Retyping an output drops the edges out of it whose target does not accept
//! the new type — the one rule [`Graph::set_params_and_output_types`] applies
//! for every retype, edit path included. Such an edge was already broken: the
//! declared `FRAME_BUFFER` is what let it be drawn, while evaluation handed
//! the target the referenced port's value instead, so nothing that worked
//! stops working. It is still a visible change to the document, so each one
//! is logged.
//!
//! # Idempotence
//!
//! A node whose output already declares the right type is left alone, so a
//! second run changes nothing — and so does a run over a document that was
//! never wrong.
//!
//! [`dependent_port_updates`]: crate::registry::builtin::dependent_port_updates
//! [`Graph::set_params`]: crate::graph::Graph::set_params
//! [`Graph::set_params_and_output_types`]: crate::graph::Graph::set_params_and_output_types

use std::sync::Arc;

use crate::graph::Graph;
use crate::registry::builtin::dependent_port_updates;

use super::validate::LAYER_REF_TYPE_KEY;
use super::{Composition, Document};

/// The parameter naming the port of the target layer. `layer.ref`'s other
/// identifier parameter has a constant beside the type key; this one is read
/// here and in [`dependent_port_updates`] only.
const LAYER_REF_PORT_PARAM: &str = "port";

/// Retype every `layer.ref` output in `document` to the port its `port`
/// parameter names.
pub(super) fn retype(mut document: Document) -> Document {
    let comp_ids: Vec<_> = document.compositions.keys().copied().collect();
    let mut compositions = document.compositions.clone();
    for comp_id in comp_ids {
        let Some(comp) = compositions.get(&comp_id) else {
            continue;
        };
        // Every layer resolves against the composition as it was read, so the
        // order the layers are walked in cannot change the answer. Retyping a
        // `layer.ref` output touches no `net.out` interface, so the two are
        // the same graph for this pass's purposes — taking the snapshot says
        // so rather than relying on it.
        let source = (**comp).clone();
        let mut updated = source.clone();
        for layer in updated.layers.iter_mut() {
            layer.network = retype_graph(&layer.network, &source);
        }
        compositions.insert(comp_id, Arc::new(updated));
    }
    document.compositions = compositions;
    document
}

/// Retype one layer network, descending into subnets.
fn retype_graph(graph: &Graph, comp: &Composition) -> Graph {
    super::graph_walk::map_subnets(graph, &|graph| retype_level(graph, comp))
}

/// Retype one graph's own nodes, ignoring its subnets — the shared walk
/// visits those separately.
fn retype_level(graph: &Graph, comp: &Composition) -> Graph {
    let mut retyped = graph.clone();
    for id in graph.node_ids().collect::<Vec<_>>() {
        let Some(node) = retyped.node(id) else {
            continue;
        };
        if node.type_key != LAYER_REF_TYPE_KEY {
            continue;
        }
        // The value is not edited — only the type follows it — so the
        // current `port` is what the dependency is computed from. A node
        // without the parameter predates it and evaluates through the
        // `"frame"` default, which is the type the template already declares.
        let Some(changed) = node
            .parameters
            .iter()
            .find(|p| p.key == LAYER_REF_PORT_PARAM)
            .cloned()
        else {
            continue;
        };
        let retypes = dependent_port_updates(node, &changed, Some(comp));
        // Nothing to do for a reference that resolves to nothing, and nothing
        // to do for one that already declares the right type.
        let pending: Vec<_> = retypes
            .into_iter()
            .filter(|retype| {
                node.outputs
                    .iter()
                    .any(|out| out.name == retype.port && out.data_type != retype.data_type)
            })
            .collect();
        if pending.is_empty() {
            continue;
        }
        // Read off the node before it is replaced, for the log below.
        let was: Vec<_> = pending
            .iter()
            .map(|retype| {
                node.outputs
                    .iter()
                    .find(|out| out.name == retype.port)
                    .map(|out| out.data_type)
            })
            .collect();
        let edges_before = retyped.edges().count();
        // No parameter updates: the stored value is already right, and
        // rewriting it would be the one thing that could make the pass lose
        // information. The graph is cloned into the call because it is
        // consumed.
        retyped = match retyped
            .clone()
            .set_params_and_output_types(id, &[], &pending)
        {
            Ok(graph) => graph,
            Err(err) => {
                // The untouched graph stands — a load must not fail over one
                // node — but the node then keeps declaring a type it does not
                // produce, and nothing else would say so.
                tracing::warn!(
                    node = ?id,
                    %err,
                    "layer.ref output left declaring a type it does not produce"
                );
                continue;
            }
        };
        let dropped = edges_before.saturating_sub(retyped.edges().count());
        if dropped > 0 {
            // The type is now honest, and that is the point; but an edge
            // disappearing between one open and the next is the kind of change
            // a user has to be able to trace, so each node that costs one says
            // which type it moved between.
            tracing::warn!(
                node = ?id,
                dropped,
                from = ?was,
                to = ?pending,
                "layer.ref output retyped to the port it references; edges its \
                 targets cannot accept were dropped"
            );
        }
    }
    retyped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::{Layer, validate::LAYER_REF_LAYER_PARAM};
    use crate::graph::{Node, ParameterValue};
    use crate::id::{CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex};
    use crate::network::{self, PORT_FRAME};
    use crate::types::FrameRate;

    /// A layer whose network exposes `frame` plus one geometry port named
    /// `geo_out`, which is what a pre-v13 `port` string could name.
    fn target_layer(id: u64) -> Layer {
        let out = Node::new(NodeId::new(100 + id), network::NET_OUT_TYPE_KEY)
            .with_input(PORT_FRAME, &[DataTypeId::FRAME_BUFFER])
            .with_input("geo_out", &[DataTypeId::GEOMETRY]);
        Layer::new(
            LayerId::new(id),
            format!("Target {id}"),
            Graph::new().add_node(out).unwrap(),
        )
    }

    fn layer_ref(id: u64, target: u64, port: &str) -> Node {
        Node::new(NodeId::new(id), LAYER_REF_TYPE_KEY)
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param(
                LAYER_REF_LAYER_PARAM,
                ParameterValue::String(target.to_string()),
            )
            .with_param(LAYER_REF_PORT_PARAM, ParameterValue::String(port.into()))
    }

    fn comp_with(referrer: Graph) -> Composition {
        Composition::new(
            CompId::new(1),
            "Comp",
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        )
        .add_layer(target_layer(1))
        .add_layer(Layer::new(LayerId::new(2), "Referrer", referrer))
    }

    fn output_type(comp: &Composition, node: u64) -> DataTypeId {
        comp.get_layer(LayerId::new(2))
            .expect("the referrer")
            .network
            .node(NodeId::new(node))
            .expect("the node")
            .outputs
            .first()
            .expect("its output")
            .data_type
    }

    fn retyped(referrer: Graph) -> Composition {
        let document = Document::default().with_composition(comp_with(referrer));
        let document = retype(document);
        document
            .get_composition(CompId::new(1))
            .expect("the composition")
            .as_ref()
            .clone()
    }

    /// The point of the pass: a `port` naming a geometry output makes the
    /// reference's own output declare geometry.
    #[test]
    fn a_non_frame_port_retypes_the_output() {
        let comp = retyped(Graph::new().add_node(layer_ref(1, 1, "geo_out")).unwrap());
        assert_eq!(output_type(&comp, 1), DataTypeId::GEOMETRY);
    }

    /// The default `port` names a real port whose type is what the template
    /// already declares, so the overwhelmingly common document is untouched.
    #[test]
    fn the_default_port_changes_nothing() {
        let graph = Graph::new()
            .add_node(layer_ref(1, 1, PORT_FRAME))
            .unwrap()
            .add_node(
                Node::new(NodeId::new(2), "merge").with_input("A", &[DataTypeId::FRAME_BUFFER]),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        let comp = retyped(graph);
        assert_eq!(output_type(&comp, 1), DataTypeId::FRAME_BUFFER);
        assert_eq!(
            comp.get_layer(LayerId::new(2))
                .expect("the referrer")
                .network
                .edges()
                .count(),
            1,
            "a no-op retype drops no edge"
        );
    }

    /// An unresolvable reference leaves the declared type alone rather than
    /// falling back to the default — the rule `dependent_port_updates` owns.
    #[test]
    fn an_unresolvable_reference_retypes_nothing() {
        for (target, port) in [(77, "geo_out"), (1, "not_a_port")] {
            let comp = retyped(Graph::new().add_node(layer_ref(1, target, port)).unwrap());
            assert_eq!(output_type(&comp, 1), DataTypeId::FRAME_BUFFER);
        }
    }

    /// The edges out of the retyped output follow the one rule every retype
    /// follows: a target that cannot accept the new type loses its edge, a
    /// target that can keeps it.
    #[test]
    fn only_the_edges_the_new_type_cannot_travel_are_dropped() {
        let graph = Graph::new()
            .add_node(layer_ref(1, 1, "geo_out"))
            .unwrap()
            .add_node(
                Node::new(NodeId::new(2), "merge").with_input("A", &[DataTypeId::FRAME_BUFFER]),
            )
            .unwrap()
            // An input that accepts both is the one a real pre-v13 document
            // could have an edge into: the output declared `FRAME_BUFFER`,
            // so nothing else would have been connectable.
            .add_node(
                Node::new(NodeId::new(3), "switch")
                    .with_input("any", &[DataTypeId::FRAME_BUFFER, DataTypeId::GEOMETRY]),
            )
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
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap();
        let comp = retyped(graph);
        let network = &comp
            .get_layer(LayerId::new(2))
            .expect("the referrer")
            .network;
        assert_eq!(output_type(&comp, 1), DataTypeId::GEOMETRY);
        assert!(
            network.edge(EdgeId::new(1)).is_none(),
            "the frame input cannot accept geometry"
        );
        assert!(
            network.edge(EdgeId::new(2)).is_some(),
            "an input that accepts geometry too keeps its edge"
        );
    }

    /// Nested subnets are reached, which is where the shared walk earns its
    /// keep.
    #[test]
    fn nested_subnets_are_retyped() {
        let inner = Graph::new().add_node(layer_ref(3, 1, "geo_out")).unwrap();
        let outer = Node::new(NodeId::new(2), crate::network::SUBNET_TYPE_KEY)
            .with_subnet(Graph::new().add_node(layer_ref(4, 1, "geo_out")).unwrap());
        let mut outer = outer;
        outer.subnet = Some(Arc::new(
            Graph::new()
                .add_node({
                    let mut mid = Node::new(NodeId::new(5), crate::network::SUBNET_TYPE_KEY);
                    mid.subnet = Some(Arc::new(inner));
                    mid
                })
                .unwrap(),
        ));
        let comp = retyped(Graph::new().add_node(outer).unwrap());
        let deep = comp
            .get_layer(LayerId::new(2))
            .expect("the referrer")
            .network
            .node(NodeId::new(2))
            .and_then(|node| node.subnet.clone())
            .and_then(|mid| mid.node(NodeId::new(5)).and_then(|n| n.subnet.clone()))
            .expect("the doubly nested subnet");
        assert_eq!(
            deep.node(NodeId::new(3))
                .expect("the nested reference")
                .outputs
                .first()
                .expect("its output")
                .data_type,
            DataTypeId::GEOMETRY
        );
    }

    /// Running the pass over a document it has already retyped changes
    /// nothing.
    #[test]
    fn the_pass_is_idempotent() {
        let document = Document::default().with_composition(comp_with(
            Graph::new().add_node(layer_ref(1, 1, "geo_out")).unwrap(),
        ));
        let once = retype(document);
        let twice = retype(once.clone());
        assert_eq!(once.compositions, twice.compositions);
    }
}
