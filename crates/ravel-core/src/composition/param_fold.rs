// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `.ravprj` v4 → v5: fold legacy `_x` / `_y` component parameters into the
//! `Channel2` / `Channel3` vector parameters the builtin templates now
//! declare.
//!
//! Node parameters are free key/value pairs, so a v4 document's
//! `center_x: Float(..)` deserializes intact even though no template declares
//! it any more — it simply stops being read. The upgrade therefore lives here,
//! as a typed pass over the loaded [`Document`], rather than in the untyped
//! `manifest.json` migration chain: the chain never sees `document/main.ron`.
//! [`crate::composition::Document::fold_component_params`] walks every graph
//! in the document (the flat graph, each layer network, and nested subnets).
//!
//! Exposed parameter ports are folded too. Two separately driven scalar ports
//! cannot collapse into one vector port, so a `vector.construct` node is
//! inserted to keep both edges: the old edges drive its components and its
//! output drives the single new vector port.

use crate::animation::channel::AnimationChannel;
use crate::graph::{Graph, Node, Parameter, ParameterValue};
use crate::id::{DataTypeId, EdgeId, NodeId, OutputPortIndex};
use crate::registry::builtin::{
    VECTOR_COMPONENT_KEYS, VECTOR_CONSTRUCT_VEC2, VECTOR_CONSTRUCT_VEC3,
};
use std::sync::Arc;

/// One component of a folded parameter: the v4 parameter key it comes from
/// (`None` for a component that did not exist before, such as the Z of a
/// `Channel3`) and the value to use when that key is absent.
type Component = (Option<&'static str>, f32);

const CENTER_2D: &[Component] = &[(Some("center_x"), 0.0), (Some("center_y"), 0.0)];

/// Every `(type_key, folded key, components)` the v5 templates declare.
///
/// `scatter.grid`'s `count_x` / `count_y` are absent on purpose: they are
/// `Int` pairs, and a `Channel2` is a pair of float channels.
/// `attribute.set`'s `value` family is absent because its arity follows the
/// `type` parameter, which would require retyping the stored parameter
/// whenever `type` changes.
const FOLDS: &[(&str, &str, &[Component])] = &[
    (
        "field.falloff",
        "center",
        &[
            (Some("center_x"), 0.0),
            (Some("center_y"), 0.0),
            (None, 0.0),
        ],
    ),
    (
        "field.falloff",
        "direction",
        &[
            (Some("direction_x"), 1.0),
            (Some("direction_y"), 0.0),
            (None, 0.0),
        ],
    ),
    (
        "geometry.transform",
        "translate",
        &[
            (Some("translate_x"), 0.0),
            (Some("translate_y"), 0.0),
            (None, 0.0),
        ],
    ),
    // The v4 scalar `rotation` is the Z of the Euler triple, so the 2D
    // rotation it encoded is preserved exactly.
    (
        "geometry.transform",
        "rotation",
        &[(None, 0.0), (None, 0.0), (Some("rotation"), 0.0)],
    ),
    (
        "geometry.transform",
        "scale",
        &[(Some("scale_x"), 1.0), (Some("scale_y"), 1.0), (None, 1.0)],
    ),
    (
        "geometry.transform",
        "pivot",
        &[(Some("pivot_x"), 0.0), (Some("pivot_y"), 0.0), (None, 0.0)],
    ),
    (
        "transform",
        "translate",
        &[
            (Some("translate_x"), 0.0),
            (Some("translate_y"), 0.0),
            (None, 0.0),
        ],
    ),
    ("shape.rect", "center", CENTER_2D),
    ("shape.ellipse", "center", CENTER_2D),
    (
        "shape.ellipse",
        "radius",
        &[(Some("radius_x"), 50.0), (Some("radius_y"), 50.0)],
    ),
    ("shape.polygon", "center", CENTER_2D),
    ("shape.star", "center", CENTER_2D),
    ("scatter.grid", "center", CENTER_2D),
    (
        "scatter.grid",
        "spacing",
        &[(Some("spacing_x"), 20.0), (Some("spacing_y"), 20.0)],
    ),
    ("scatter.circular", "center", CENTER_2D),
    ("scatter.path_array", "center", CENTER_2D),
    ("scatter.scatter", "center", CENTER_2D),
];

/// Horizontal offset of an inserted `vector.construct` node from the node it
/// feeds, so the upgraded network is legible in the node editor.
const CONSTRUCT_OFFSET_X: f32 = -220.0;

/// The scalar animation channel a v4 parameter value carries, if it is one.
/// A value that is already a vector channel is not a v4 component.
fn scalar_channel(value: &ParameterValue) -> Option<AnimationChannel> {
    match value {
        ParameterValue::Float(v) => Some(AnimationChannel::constant(*v)),
        ParameterValue::Channel(ch) => Some(ch.clone()),
        _ => None,
    }
}

/// Whether `node` already stores `key` as a vector channel of `arity`
/// components — a v5 document, or a graph folded earlier in this pass.
fn already_folded(node: &Node, key: &str, arity: usize) -> bool {
    node.parameters
        .iter()
        .find(|p| p.key == key)
        .is_some_and(|p| {
            matches!(
                (&p.value, arity),
                (ParameterValue::Channel2(_), 2) | (ParameterValue::Channel3(_), 3)
            )
        })
}

/// The folded value of one spec, plus the per-component channels it was built
/// from (needed to seed an inserted `vector.construct`).
fn folded_value(node: &Node, components: &[Component]) -> (ParameterValue, Vec<AnimationChannel>) {
    let channels: Vec<AnimationChannel> = components
        .iter()
        .map(|(legacy, default)| {
            legacy
                .and_then(|key| node.parameters.iter().find(|p| p.key == key))
                .and_then(|p| scalar_channel(&p.value))
                // A component the old file did not store — one half of the
                // pair was saved, or the Z of a new `Channel3` — takes the
                // template default, which is what the node behaved like.
                .unwrap_or_else(|| AnimationChannel::constant(*default))
        })
        .collect();
    let value = match channels.len() {
        2 => ParameterValue::Channel2([channels[0].clone(), channels[1].clone()]),
        _ => ParameterValue::Channel3([
            channels[0].clone(),
            channels[1].clone(),
            channels[2].clone(),
        ]),
    };
    (value, channels)
}

/// The parameter list with `components`' legacy keys replaced by one folded
/// `target` at the position the first of them occupied.
fn folded_parameters(
    node: &Node,
    target: &str,
    components: &[Component],
    value: ParameterValue,
) -> Vec<Parameter> {
    let legacy: Vec<&str> = components.iter().filter_map(|(key, _)| *key).collect();
    let is_legacy = |key: &str| legacy.contains(&key) || key == target;
    let insert_at = node
        .parameters
        .iter()
        .position(|p| is_legacy(&p.key))
        .unwrap_or(node.parameters.len());
    let mut kept: Vec<Parameter> = node
        .parameters
        .iter()
        .filter(|p| !is_legacy(&p.key))
        .cloned()
        .collect();
    kept.insert(
        insert_at.min(kept.len()),
        Parameter {
            key: target.to_string(),
            value,
        },
    );
    kept
}

/// A `vector.construct` node of the given arity with its components seeded
/// from `channels`, positioned to the left of `near`.
fn construct_node(id: NodeId, arity: usize, channels: &[AnimationChannel], near: &Node) -> Node {
    let (type_key, data_type) = if arity == 2 {
        (VECTOR_CONSTRUCT_VEC2, DataTypeId::VEC2)
    } else {
        (VECTOR_CONSTRUCT_VEC3, DataTypeId::VEC3)
    };
    let mut node = Node::new(id, type_key)
        .with_output("vector", data_type)
        .with_position(
            near.metadata.position.0 + CONSTRUCT_OFFSET_X,
            near.metadata.position.1,
        );
    for (key, channel) in VECTOR_COMPONENT_KEYS.iter().zip(channels) {
        node = node.with_param(*key, ParameterValue::Channel(channel.clone()));
    }
    node
}

/// Fold one node's `target` parameter in `graph`. Returns the graph unchanged
/// when the node has nothing to fold.
fn fold_one(graph: Graph, node_id: NodeId, target: &str, components: &[Component]) -> Graph {
    let Some(node) = graph.node(node_id) else {
        return graph;
    };
    if already_folded(node, target, components.len()) {
        return graph;
    }
    let legacy_keys: Vec<&str> = components.iter().filter_map(|(key, _)| *key).collect();
    let has_legacy_param = node
        .parameters
        .iter()
        .any(|p| legacy_keys.contains(&p.key.as_str()) && scalar_channel(&p.value).is_some());
    if !has_legacy_param {
        // Nothing of the old shape is stored; leave the node to the
        // registry defaults rather than inventing a parameter.
        return graph;
    }

    // Which components were driven through an exposed parameter port, and by
    // what. Recorded before any port removal reindexes the inputs.
    let driven: Vec<(usize, NodeId, OutputPortIndex)> = components
        .iter()
        .enumerate()
        .filter_map(|(index, (legacy, _))| {
            let key = (*legacy)?;
            let port = node.param_port_index(key)?;
            let edge = graph
                .edges()
                .find(|edge| edge.target == node_id && edge.target_port == port)?;
            Some((index, edge.source, edge.source_port))
        })
        .collect();
    let exposed_any = components
        .iter()
        .filter_map(|(legacy, _)| *legacy)
        .any(|key| node.param_port_index(key).is_some());

    let (value, channels) = folded_value(node, components);
    let parameters = folded_parameters(node, target, components, value);
    let mut updated = (**node).clone();
    updated.parameters = parameters;
    let near = updated.clone();

    // Drop the legacy ports (and their edges, whose sources are recorded
    // above), then re-expose the single vector port.
    // `replace_node` re-inserts the node wholesale, so strip the legacy
    // ports from the replacement itself. A port whose name equals the folded
    // key (the v4 scalar `rotation`) would otherwise survive with its stale
    // SCALAR type, since a same-named parameter still exists.
    updated
        .inputs
        .retain(|port| !(port.is_param && legacy_keys.contains(&port.name.as_str())));
    updated
        .inputs
        .retain(|port| !(port.is_param && port.name == target));
    let mut graph = graph;
    for key in legacy_keys.iter().chain(std::iter::once(&target)) {
        if graph
            .node(node_id)
            .is_some_and(|n| n.param_port_index(key).is_some())
        {
            graph = graph
                .remove_param_port(node_id, key)
                .unwrap_or_else(|_| unreachable!("the port was just observed"));
        }
    }
    graph = graph.replace_node(Arc::new(updated));
    if !exposed_any {
        return graph;
    }
    let Ok(exposed) = graph.clone().expose_param_port(node_id, target) else {
        // An input port already claims the folded name; the stored value
        // still carries every component, so the fold itself stands.
        return graph;
    };
    graph = exposed;
    if driven.is_empty() {
        return graph;
    }

    // Several scalar edges cannot share one vector port: route them through a
    // `vector.construct` whose output drives it.
    let construct_id = NodeId::next();
    let construct = construct_node(construct_id, components.len(), &channels, &near);
    let Ok(mut with_construct) = graph.clone().add_node(construct) else {
        return graph;
    };
    for (index, source, source_port) in &driven {
        let key = VECTOR_COMPONENT_KEYS[*index];
        let Ok(exposed) = with_construct.clone().expose_param_port(construct_id, key) else {
            return graph;
        };
        with_construct = exposed;
        let Some(port) = with_construct
            .node(construct_id)
            .and_then(|n| n.param_port_index(key))
        else {
            return graph;
        };
        match with_construct.clone().add_edge(
            EdgeId::next(),
            *source,
            *source_port,
            construct_id,
            port,
        ) {
            Ok(next) => with_construct = next,
            Err(_) => return graph,
        }
    }
    let Some(target_port) = with_construct
        .node(node_id)
        .and_then(|n| n.param_port_index(target))
    else {
        return graph;
    };
    match with_construct.add_edge(
        EdgeId::next(),
        construct_id,
        OutputPortIndex(0),
        node_id,
        target_port,
    ) {
        Ok(next) => next,
        Err(_) => graph,
    }
}

/// Advance the global id counters past every node and edge id in `graph`,
/// including subnets, so a minted [`NodeId`] cannot collide with one the
/// graph already uses. The document path advances past the whole document
/// first; this keeps a bare graph safe too.
fn advance_counters_past(graph: &Graph) {
    for node in graph.nodes() {
        NodeId::advance_counter_past(node.id.raw());
        if let Some(subnet) = &node.subnet {
            advance_counters_past(subnet);
        }
    }
    for edge in graph.edges() {
        EdgeId::advance_counter_past(edge.id.raw());
    }
}

/// Fold every foldable parameter in `graph`, descending into subnets.
pub(super) fn fold_graph(graph: &Graph) -> Graph {
    advance_counters_past(graph);
    let mut folded = graph.clone();

    // Subnet inner graphs first: replacing the outer node afterwards would
    // otherwise discard the inner rewrite.
    for id in folded.node_ids().collect::<Vec<_>>() {
        let Some(node) = folded.node(id) else {
            continue;
        };
        let Some(inner) = node.subnet.as_ref().map(|inner| fold_graph(inner)) else {
            continue;
        };
        let mut updated = (**node).clone();
        updated.subnet = Some(Arc::new(inner));
        folded = folded.replace_node(Arc::new(updated));
    }

    for id in folded.node_ids().collect::<Vec<_>>() {
        let Some(type_key) = folded.node(id).map(|node| node.type_key.clone()) else {
            continue;
        };
        for (_, target, components) in FOLDS.iter().filter(|(key, _, _)| *key == type_key) {
            folded = fold_one(folded, id, target, components);
        }
    }
    folded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::curve::KeyframeCurve;
    use crate::animation::interpolation::Interpolation;
    use crate::eval::EvalContext;
    use crate::id::InputPortIndex;
    use crate::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (64, 64))
    }

    /// The constant components of a folded vector parameter.
    fn vector(graph: &Graph, id: NodeId, key: &str) -> Vec<f32> {
        let value = &graph
            .node(id)
            .unwrap_or_else(|| panic!("node {id:?}"))
            .parameters
            .iter()
            .find(|p| p.key == key)
            .unwrap_or_else(|| panic!("{key} missing"))
            .value;
        match value {
            ParameterValue::Channel2(chs) => {
                chs.iter().map(|ch| ch.evaluate(0.0, &ctx())).collect()
            }
            ParameterValue::Channel3(chs) => {
                chs.iter().map(|ch| ch.evaluate(0.0, &ctx())).collect()
            }
            other => panic!("{key} is {other:?}, not a vector channel"),
        }
    }

    fn scalar_source(id: u64, value: f32) -> Node {
        Node::new(NodeId::new(id), "constant")
            .with_output("value", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(value))
    }

    /// A v4 `shape.rect` with both center components stored as Floats.
    fn v4_rect(id: u64, cx: f32, cy: f32) -> Node {
        Node::new(NodeId::new(id), "shape.rect")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("center_x", ParameterValue::Float(cx))
            .with_param("center_y", ParameterValue::Float(cy))
            .with_param("width", ParameterValue::Float(40.0))
            .with_param("height", ParameterValue::Float(20.0))
    }

    #[test]
    fn component_floats_fold_into_one_vector_parameter() {
        let graph = Graph::new().add_node(v4_rect(1, 12.0, -7.0)).unwrap();
        let folded = fold_graph(&graph);
        let node = folded.node(NodeId::new(1)).unwrap();
        assert_eq!(vector(&folded, NodeId::new(1), "center"), vec![12.0, -7.0]);
        assert!(
            node.parameters.iter().all(|p| p.key != "center_x"),
            "the legacy keys are gone"
        );
        // Neighbouring scalars keep their values and the folded key takes the
        // position of the first component it replaced.
        let keys: Vec<&str> = node.parameters.iter().map(|p| p.key.as_str()).collect();
        assert_eq!(keys, ["center", "width", "height"]);
    }

    /// A file that stored only one half of the pair fills the other from the
    /// template default (`shape.ellipse` radii default to 50).
    #[test]
    fn a_missing_component_takes_the_template_default() {
        let node = Node::new(NodeId::new(1), "shape.ellipse")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("radius_x", ParameterValue::Float(30.0));
        let folded = fold_graph(&Graph::new().add_node(node).unwrap());
        assert_eq!(vector(&folded, NodeId::new(1), "radius"), vec![30.0, 50.0]);
    }

    /// The Z defaults of the `Channel3` folds reproduce the old behaviour:
    /// translate 0, scale 1, rotation `(0, 0, θ)`.
    #[test]
    fn channel3_folds_use_behaviour_preserving_z_defaults() {
        let node = Node::new(NodeId::new(1), "geometry.transform")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("translate_x", ParameterValue::Float(10.0))
            .with_param("translate_y", ParameterValue::Float(-5.0))
            .with_param("rotation", ParameterValue::Float(37.5))
            .with_param("scale_x", ParameterValue::Float(2.0))
            .with_param("scale_y", ParameterValue::Float(3.0))
            .with_param("pivot_x", ParameterValue::Float(1.0))
            .with_param("pivot_y", ParameterValue::Float(2.0));
        let folded = fold_graph(&Graph::new().add_node(node).unwrap());
        let id = NodeId::new(1);
        assert_eq!(vector(&folded, id, "translate"), vec![10.0, -5.0, 0.0]);
        assert_eq!(vector(&folded, id, "scale"), vec![2.0, 3.0, 1.0]);
        assert_eq!(vector(&folded, id, "pivot"), vec![1.0, 2.0, 0.0]);
        assert_eq!(
            vector(&folded, id, "rotation"),
            vec![0.0, 0.0, 37.5],
            "the v4 scalar rotation becomes the Euler Z"
        );
    }

    /// Keyframes survive the fold: the component channel is moved, not
    /// flattened to its current value.
    #[test]
    fn keyframed_components_keep_their_curves() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 100.0, Interpolation::Linear);
        let node = Node::new(NodeId::new(1), "shape.rect")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param(
                "center_x",
                ParameterValue::Channel(AnimationChannel::keyframes(curve)),
            )
            .with_param("center_y", ParameterValue::Float(4.0));
        let folded = fold_graph(&Graph::new().add_node(node).unwrap());
        let ParameterValue::Channel2(chs) = &folded
            .node(NodeId::new(1))
            .unwrap()
            .parameters
            .iter()
            .find(|p| p.key == "center")
            .unwrap()
            .value
        else {
            panic!("expected Channel2");
        };
        assert_eq!(chs[0].evaluate(5.0, &ctx()), 50.0);
        assert_eq!(chs[1].evaluate(5.0, &ctx()), 4.0);
    }

    #[test]
    fn folding_is_idempotent() {
        let once = fold_graph(&Graph::new().add_node(v4_rect(1, 3.0, 4.0)).unwrap());
        let twice = fold_graph(&once);
        assert_eq!(vector(&twice, NodeId::new(1), "center"), vec![3.0, 4.0]);
        assert_eq!(twice.node_count(), once.node_count());
        assert_eq!(twice.edge_count(), once.edge_count());
    }

    /// An unconnected exposed component port collapses into one vector port.
    #[test]
    fn unconnected_component_ports_collapse_into_one_vector_port() {
        let graph = Graph::new()
            .add_node(v4_rect(1, 0.0, 0.0))
            .unwrap()
            .expose_param_port(NodeId::new(1), "center_x")
            .unwrap()
            .expose_param_port(NodeId::new(1), "center_y")
            .unwrap();
        let folded = fold_graph(&graph);
        let node = folded.node(NodeId::new(1)).unwrap();
        let ports: Vec<&str> = node
            .inputs
            .iter()
            .filter(|port| port.is_param)
            .map(|port| port.name.as_str())
            .collect();
        assert_eq!(ports, ["center"]);
        assert_eq!(
            node.inputs[node.param_port_index("center").unwrap().0 as usize].accepted_types,
            vec![DataTypeId::VEC2]
        );
        assert_eq!(folded.node_count(), 1, "no construct node was needed");
    }

    /// Two separately driven component ports cannot share one vector port, so
    /// a `vector.construct` preserves both edges.
    #[test]
    fn two_driven_component_ports_gain_a_vector_construct() {
        let graph = Graph::new()
            .add_node(scalar_source(1, 4.0))
            .unwrap()
            .add_node(scalar_source(2, -6.0))
            .unwrap()
            .add_node(v4_rect(3, 0.0, 0.0))
            .unwrap()
            .expose_param_port(NodeId::new(3), "center_x")
            .unwrap()
            .expose_param_port(NodeId::new(3), "center_y")
            .unwrap();
        let x = graph
            .node(NodeId::new(3))
            .unwrap()
            .param_port_index("center_x")
            .unwrap();
        let y = graph
            .node(NodeId::new(3))
            .unwrap()
            .param_port_index("center_y")
            .unwrap();
        let graph = graph
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(3),
                x,
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                y,
            )
            .unwrap();

        let folded = fold_graph(&graph);
        assert_eq!(folded.node_count(), 4, "one construct node was inserted");
        let construct = folded
            .nodes()
            .find(|node| node.type_key == VECTOR_CONSTRUCT_VEC2)
            .expect("construct inserted");
        // Both original sources now drive the construct's components.
        let driven = |key: &str| {
            let port = construct.param_port_index(key).unwrap();
            folded
                .edges()
                .find(|edge| edge.target == construct.id && edge.target_port == port)
                .map(|edge| edge.source)
        };
        assert_eq!(driven("x"), Some(NodeId::new(1)));
        assert_eq!(driven("y"), Some(NodeId::new(2)));
        // …and the construct drives the single folded vector port.
        let target_port = folded
            .node(NodeId::new(3))
            .unwrap()
            .param_port_index("center")
            .unwrap();
        assert!(
            folded.edges().any(|edge| edge.source == construct.id
                && edge.target == NodeId::new(3)
                && edge.target_port == target_port),
            "construct output drives the folded port"
        );
    }

    /// One driven component and one stored value: the construct keeps the
    /// stored half instead of defaulting it to zero.
    #[test]
    fn a_single_driven_component_keeps_its_sibling_value() {
        let graph = Graph::new()
            .add_node(scalar_source(1, 4.0))
            .unwrap()
            .add_node(v4_rect(2, 0.0, 55.0))
            .unwrap()
            .expose_param_port(NodeId::new(2), "center_y")
            .unwrap();
        let port = graph
            .node(NodeId::new(2))
            .unwrap()
            .param_port_index("center_y")
            .unwrap();
        let graph = graph
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                port,
            )
            .unwrap();
        let folded = fold_graph(&graph);
        let construct = folded
            .nodes()
            .find(|node| node.type_key == VECTOR_CONSTRUCT_VEC2)
            .expect("construct inserted");
        let x = construct
            .parameters
            .iter()
            .find(|p| p.key == "x")
            .expect("x seeded");
        assert_eq!(
            scalar_channel(&x.value).unwrap().evaluate(0.0, &ctx()),
            0.0,
            "the unconnected X keeps its stored value"
        );
        assert!(
            construct.param_port_index("x").is_none(),
            "only the driven component is exposed"
        );
    }

    /// The exposed scalar `rotation` port becomes a VEC3 port fed by a
    /// `vector.construct.vec3` whose Z carries the old edge.
    #[test]
    fn a_driven_scalar_rotation_routes_through_a_vec3_construct() {
        let graph = Graph::new()
            .add_node(scalar_source(1, 90.0))
            .unwrap()
            .add_node(
                Node::new(NodeId::new(2), "geometry.transform")
                    .with_input("geometry", &[DataTypeId::GEOMETRY])
                    .with_output("output", DataTypeId::GEOMETRY)
                    .with_param("rotation", ParameterValue::Float(0.0)),
            )
            .unwrap()
            .expose_param_port(NodeId::new(2), "rotation")
            .unwrap();
        let port = graph
            .node(NodeId::new(2))
            .unwrap()
            .param_port_index("rotation")
            .unwrap();
        let graph = graph
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                port,
            )
            .unwrap();
        let folded = fold_graph(&graph);
        let target = folded.node(NodeId::new(2)).unwrap();
        let rotation_port = target.param_port_index("rotation").unwrap();
        assert_eq!(
            target.inputs[rotation_port.0 as usize].accepted_types,
            vec![DataTypeId::VEC3]
        );
        let construct = folded
            .nodes()
            .find(|node| node.type_key == VECTOR_CONSTRUCT_VEC3)
            .expect("vec3 construct inserted");
        let z = construct.param_port_index("z").expect("z exposed");
        assert!(
            folded.edges().any(|edge| edge.source == NodeId::new(1)
                && edge.target == construct.id
                && edge.target_port == z),
            "the old rotation edge drives the Euler Z"
        );
    }

    #[test]
    fn subnet_inner_graphs_are_folded() {
        let inner = Graph::new().add_node(v4_rect(1, 5.0, 6.0)).unwrap();
        let outer = Graph::new()
            .add_node(
                Node::new(NodeId::new(2), "subnet")
                    .with_subnet(inner)
                    .with_output("out", DataTypeId::GEOMETRY),
            )
            .unwrap();
        let folded = fold_graph(&outer);
        let subnet = folded
            .node(NodeId::new(2))
            .unwrap()
            .subnet
            .clone()
            .expect("subnet preserved");
        assert_eq!(vector(&subnet, NodeId::new(1), "center"), vec![5.0, 6.0]);
    }

    /// A node that stores nothing of the old shape is left alone rather than
    /// gaining an invented parameter.
    #[test]
    fn nodes_without_legacy_components_are_untouched() {
        let node = Node::new(NodeId::new(1), "shape.rect")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("width", ParameterValue::Float(1.0));
        let graph = Graph::new().add_node(node).unwrap();
        let folded = fold_graph(&graph);
        assert!(
            folded
                .node(NodeId::new(1))
                .unwrap()
                .parameters
                .iter()
                .all(|p| p.key != "center")
        );
    }

    /// Folding never disturbs edges into ordinary (non-parameter) input ports.
    #[test]
    fn data_edges_survive_the_port_reindexing() {
        let graph = Graph::new()
            .add_node(v4_rect(1, 0.0, 0.0))
            .unwrap()
            .add_node(
                Node::new(NodeId::new(2), "geometry.transform")
                    .with_input("geometry", &[DataTypeId::GEOMETRY])
                    .with_output("output", DataTypeId::GEOMETRY)
                    .with_param("translate_x", ParameterValue::Float(3.0))
                    .with_param("translate_y", ParameterValue::Float(4.0)),
            )
            .unwrap()
            .expose_param_port(NodeId::new(2), "translate_x")
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        let folded = fold_graph(&graph);
        assert!(
            folded.edges().any(|edge| edge.source == NodeId::new(1)
                && edge.target == NodeId::new(2)
                && edge.target_port == InputPortIndex(0)),
            "the geometry edge still lands on port 0"
        );
        assert_eq!(
            vector(&folded, NodeId::new(2), "translate"),
            vec![3.0, 4.0, 0.0]
        );
    }
}
