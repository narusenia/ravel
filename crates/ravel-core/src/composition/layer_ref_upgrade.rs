// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `.ravprj` v12 → v13: hold a `layer.ref` target as the decimal
//! [`LayerId`](crate::id::LayerId) in a [`ParameterValue::String`] instead of
//! an [`ParameterValue::Int`].
//!
//! # Why the type changed
//!
//! What the user picks is a *sibling layer*, not a number: a raw id is not
//! something anybody knows, and the Properties row that can carry candidates
//! with labels is the string row (`PropertyField::Enum`). So the template
//! declares `string_parameter("layer", "")` plus
//! [`ContextualKind::SiblingLayer`](crate::registry::ContextualKind), and the
//! stored value keeps the decimal id — which makes this upgrade one type
//! change rather than a rename of every reference.
//!
//! # A typed pass, not a manifest step
//!
//! Like the v4 → v5 fold and the v5 → v6 curve upgrade beside it, this runs
//! over the loaded [`Document`](super::Document); `migrate_v12_to_v13`
//! advances the version stamp and nothing else, because the untyped
//! `manifest.json` chain never sees `document/main.ron`.
//!
//! # What the old values map to
//!
//! | v12 value | v13 value |
//! |---|---|
//! | `Int(n)`, `n >= 0` | `String("n")` |
//! | `Int(-1)` — the old "no target" default — or any other negative / unrepresentable `Int` | `String("")` |
//! | a constant `IntChannel` | `String` of the id it names |
//! | an animated `IntChannel` (only a hand-edited document has one) | `String("")` — it referenced nothing before the upgrade either |
//! | `String` / `StringSteps` | untouched, which is what makes the pass idempotent |
//!
//! # The exposed parameter port goes
//!
//! A v12 document may have `layer` exposed as a `SCALAR` parameter port with
//! an edge into it. A `String` has no wire type at all
//! ([`ParameterValue::port_accepted_types`]), so the rewrite goes through
//! [`Graph::set_params`], which drops the port and its edge under the rule it
//! already owns: a port must never declare types that cannot flow through it.
//! Nothing is lost that was doing anything — the evaluator has always ignored
//! a wire into an identifier parameter
//! ([`ParameterValue::identifier`](crate::graph::ParameterValue::identifier)),
//! so the edge fed a value no reference could ever take.
//!
//! [`ParameterValue::port_accepted_types`]: crate::graph::ParameterValue::port_accepted_types

use crate::graph::{Graph, Parameter, ParameterValue};
use crate::id::NodeId;

use super::validate::{LAYER_REF_LAYER_PARAM, LAYER_REF_TYPE_KEY};

/// Upgrade every `layer.ref` target in `graph`, descending into subnets.
pub(super) fn upgrade_graph(graph: &Graph) -> Graph {
    super::graph_walk::map_subnets(graph, &upgrade_level)
}

/// The v13 spelling of a v12 `layer` value, or `None` when there is nothing
/// to do.
fn target_text(value: &ParameterValue) -> Option<String> {
    match value {
        // Already v13, in either spelling: left alone, which is what makes
        // running the pass twice a no-op.
        ParameterValue::String(_) | ParameterValue::StringSteps(_) => None,
        // One mouth for both int spellings.
        // [`ParameterValue::static_identifier`] answers `None` for exactly the
        // values that named no layer — the old `-1` default, any other
        // negative number, and an animated channel — and those become the
        // empty string the v13 template defaults to.
        ParameterValue::Int(_) | ParameterValue::IntChannel(_) => Some(
            value
                .static_identifier()
                .map(|raw| raw.to_string())
                .unwrap_or_default(),
        ),
        // No other shape ever spelled a layer reference.
        _ => None,
    }
}

fn upgrade_level(graph: &Graph) -> Graph {
    let rewrites: Vec<(NodeId, String)> = graph
        .nodes()
        .filter(|node| node.type_key == LAYER_REF_TYPE_KEY)
        .filter_map(|node| {
            let param = node
                .parameters
                .iter()
                .find(|p| p.key == LAYER_REF_LAYER_PARAM)?;
            Some((node.id, target_text(&param.value)?))
        })
        .collect();

    let mut upgraded = graph.clone();
    for (id, text) in rewrites {
        let update = [Parameter {
            key: LAYER_REF_LAYER_PARAM.to_string(),
            value: ParameterValue::String(text),
        }];
        // The graph is cloned into the call because `set_params` consumes it.
        // A load must not fail over one reference, so the untouched graph
        // stands — but it must not fail *silently* either: the node keeps its
        // `Int` while the archive is stamped v13, and the int spelling is one
        // nothing reads any more. The reference stops resolving, the
        // watermark scan stops seeing it, and no other signal exists. So the
        // one thing that can still be done is say so.
        //
        // `NodeNotFound` is ruled out by the scan above; what remains is a
        // hand-built node the port rules reject (`ParamAlreadyExposed`, a
        // port that cannot carry the type).
        upgraded = match upgraded.clone().set_params(id, &update) {
            Ok(graph) => graph,
            Err(err) => {
                tracing::warn!(
                    node = ?id,
                    %err,
                    "layer.ref target left in the pre-v13 int spelling; the reference will not resolve"
                );
                upgraded
            }
        };
    }
    upgraded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::channel::AnimationChannel;
    use crate::animation::curve::KeyframeCurve;
    use crate::animation::interpolation::Interpolation;
    use crate::animation::step::StepCurve;
    use crate::graph::Node;
    use crate::id::{DataTypeId, EdgeId, InputPortIndex, OutputPortIndex};

    fn layer_ref(id: u64, value: ParameterValue) -> Node {
        Node::new(NodeId::new(id), LAYER_REF_TYPE_KEY)
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param(LAYER_REF_LAYER_PARAM, value)
    }

    fn target_of(graph: &Graph, id: u64) -> ParameterValue {
        graph
            .node(NodeId::new(id))
            .expect("node")
            .parameters
            .iter()
            .find(|p| p.key == LAYER_REF_LAYER_PARAM)
            .expect("the target parameter")
            .value
            .clone()
    }

    fn upgraded(value: ParameterValue) -> ParameterValue {
        let graph = Graph::new().add_node(layer_ref(1, value)).unwrap();
        target_of(&upgrade_graph(&graph), 1)
    }

    /// A stored target keeps naming the same layer, in the new spelling.
    #[test]
    fn a_stored_target_becomes_its_decimal_spelling() {
        assert_eq!(
            upgraded(ParameterValue::Int(12)),
            ParameterValue::String("12".into())
        );
        assert_eq!(
            upgraded(ParameterValue::IntChannel(AnimationChannel::constant(12.0))),
            ParameterValue::String("12".into()),
            "a constant channel names the layer a constant Int does"
        );
    }

    /// The v12 "no target" default, and every other value that named no
    /// layer, becomes the empty string — the spelling `identifier()` reads as
    /// `Unset`.
    #[test]
    fn a_value_that_named_no_layer_becomes_empty() {
        assert_eq!(
            upgraded(ParameterValue::Int(-1)),
            ParameterValue::String(String::new())
        );
        assert_eq!(
            upgraded(ParameterValue::Int(-7)),
            ParameterValue::String(String::new())
        );

        let mut curve = KeyframeCurve::new();
        curve.insert(0, 3.0, Interpolation::Linear);
        curve.insert(24, 9.0, Interpolation::Linear);
        assert_eq!(
            upgraded(ParameterValue::IntChannel(AnimationChannel::keyframes(
                curve
            ))),
            ParameterValue::String(String::new()),
            "an animated identifier referenced nothing before the upgrade either"
        );
    }

    /// Running the pass over a document it has already upgraded changes
    /// nothing, in either string spelling.
    #[test]
    fn the_pass_is_idempotent() {
        let already = ParameterValue::String("12".into());
        assert_eq!(upgraded(already.clone()), already);

        let stepped = ParameterValue::StringSteps(StepCurve::keyed(0, "12".to_string()));
        assert_eq!(upgraded(stepped.clone()), stepped);
    }

    /// Nested subnets are reached, which is where the walk (rather than a
    /// flat scan) earns its keep.
    #[test]
    fn nested_subnets_are_upgraded() {
        let inner = Graph::new()
            .add_node(layer_ref(3, ParameterValue::Int(5)))
            .unwrap();
        let mut outer_node = Node::new(NodeId::new(2), crate::network::SUBNET_TYPE_KEY);
        outer_node.subnet = Some(std::sync::Arc::new(
            Graph::new()
                .add_node({
                    let mut mid = Node::new(NodeId::new(4), crate::network::SUBNET_TYPE_KEY);
                    mid.subnet = Some(std::sync::Arc::new(inner));
                    mid
                })
                .unwrap(),
        ));
        let graph = Graph::new()
            .add_node(layer_ref(1, ParameterValue::Int(6)))
            .unwrap()
            .add_node(outer_node)
            .unwrap();

        let upgraded = upgrade_graph(&graph);
        assert_eq!(target_of(&upgraded, 1), ParameterValue::String("6".into()));
        let deep = upgraded
            .node(NodeId::new(2))
            .and_then(|node| node.subnet.clone())
            .and_then(|mid| mid.node(NodeId::new(4)).and_then(|n| n.subnet.clone()))
            .expect("the doubly nested subnet");
        assert_eq!(target_of(&deep, 3), ParameterValue::String("5".into()));
    }

    /// A v12 document may have `layer` exposed as a `SCALAR` port with an edge
    /// into it. A `String` has no wire type, so the port and the edge go — the
    /// rule `set_params` already enforces for every retyped parameter.
    #[test]
    fn an_exposed_scalar_port_on_the_target_is_dropped_with_its_edge() {
        let graph = Graph::new()
            .add_node(Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR))
            .unwrap()
            .add_node(layer_ref(2, ParameterValue::Int(12)))
            .unwrap()
            .expose_param_port(NodeId::new(2), LAYER_REF_LAYER_PARAM)
            .expect("an Int parameter exposes a SCALAR port")
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();

        let upgraded = upgrade_graph(&graph);
        assert_eq!(target_of(&upgraded, 2), ParameterValue::String("12".into()));
        assert!(
            upgraded
                .node(NodeId::new(2))
                .expect("the node survives")
                .param_port_index(LAYER_REF_LAYER_PARAM)
                .is_none(),
            "a String parameter cannot carry a port"
        );
        assert_eq!(
            upgraded.edges().count(),
            0,
            "the edge into the dropped port goes with it"
        );
    }
}
