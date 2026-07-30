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
//! output drives the single new vector port. This covers 2, 3 and 4
//! components — a 4-component parameter port accepts `VEC4` as well as
//! `COLOR` (see [`ParameterValue::port_accepted_types`]).

use crate::animation::channel::AnimationChannel;
use crate::graph::{Graph, Node, Parameter, ParameterValue};
use crate::id::{DataTypeId, EdgeId, NodeId, OutputPortIndex};
use crate::registry::builtin::{
    ATTRIBUTE_SET_DEFAULT_TYPE, VECTOR_COMPONENT_KEYS, VECTOR_CONSTRUCT_VEC2,
    VECTOR_CONSTRUCT_VEC3, VECTOR_CONSTRUCT_VEC4, attribute_set_value_defaults,
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
    ("scatter.scatter", "center", CENTER_2D),
    (
        "scatter.scatter",
        "area",
        &[(Some("area_x"), 200.0), (Some("area_y"), 200.0)],
    ),
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

/// Whether `node` already stores `key` as a channel-backed value of `arity`
/// components — a v5 document, or a graph folded earlier in this pass. A
/// plain `Float` is *not* folded: arity 1 still becomes a `Channel`.
fn already_folded(node: &Node, key: &str, arity: usize) -> bool {
    node.parameters
        .iter()
        .find(|p| p.key == key)
        .is_some_and(|p| {
            !matches!(p.value, ParameterValue::Float(_))
                && p.value.channels().is_some_and(|chs| chs.len() == arity)
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
    let value = ParameterValue::from_channels(channels.clone())
        .expect("every fold spec declares 1 to 4 components");
    (value, channels)
}

/// The parameter list with `components`' legacy keys replaced by one folded
/// `target` at the position the first of them occupied. `surplus` keys are
/// dropped without contributing anything (`attribute.set`'s `value_z` when
/// its `type` is `vec2`).
fn folded_parameters(
    node: &Node,
    target: &str,
    components: &[Component],
    surplus: &[&str],
    value: ParameterValue,
) -> Vec<Parameter> {
    let legacy: Vec<&str> = components.iter().filter_map(|(key, _)| *key).collect();
    let is_legacy = |key: &str| legacy.contains(&key) || surplus.contains(&key) || key == target;
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

/// The `vector.construct` template that produces a value of `arity`
/// components, or `None` when no node can rebuild that shape from scalars.
///
/// Arity 1 needs no construct: a scalar edge drives a scalar parameter port
/// directly, so the fold leaves that port alone.
fn construct_kind(arity: usize) -> Option<(&'static str, DataTypeId)> {
    match arity {
        2 => Some((VECTOR_CONSTRUCT_VEC2, DataTypeId::VEC2)),
        3 => Some((VECTOR_CONSTRUCT_VEC3, DataTypeId::VEC3)),
        // A 4-component parameter port accepts VEC4 alongside COLOR
        // (`ParameterValue::port_accepted_types`), so the vec4 construct fits.
        4 => Some((VECTOR_CONSTRUCT_VEC4, DataTypeId::VEC4)),
        _ => None,
    }
}

/// A `vector.construct` node of the given arity with its components seeded
/// from `channels`, positioned to the left of `near`.
fn construct_node(id: NodeId, arity: usize, channels: &[AnimationChannel], near: &Node) -> Node {
    let (type_key, data_type) = construct_kind(arity).expect("arity checked by the caller");
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
///
/// `surplus` names v4 keys that are dropped without contributing a component
/// — `attribute.set` stores four `value_*` keys whatever its `type` selects,
/// so the ones the type does not read are surplus.
fn fold_one(
    graph: Graph,
    node_id: NodeId,
    target: &str,
    components: &[Component],
    surplus: &[&str],
) -> Graph {
    let Some(node) = graph.node(node_id) else {
        return graph;
    };
    let has_surplus = node
        .parameters
        .iter()
        .any(|p| surplus.contains(&p.key.as_str()));
    if already_folded(node, target, components.len()) && !has_surplus {
        return graph;
    }
    let legacy_keys: Vec<&str> = components.iter().filter_map(|(key, _)| *key).collect();
    let has_legacy_param = node
        .parameters
        .iter()
        .any(|p| legacy_keys.contains(&p.key.as_str()) && scalar_channel(&p.value).is_some());
    if !has_legacy_param && !has_surplus {
        // Nothing of the old shape is stored; leave the node to the
        // registry defaults rather than inventing a parameter.
        return graph;
    }

    let (value, channels) = folded_value(node, components);
    let new_accepted = value.port_accepted_types();
    // A port whose acceptance set is unchanged keeps its edges: folding a v4
    // scalar into a 1-component `Channel` does not disturb what drives it.
    let target_port_kept = node
        .param_port_index(target)
        .map(|index| &node.inputs[index.0 as usize].accepted_types)
        .is_some_and(|accepted| *accepted == new_accepted);

    // The ports this fold destroys, and what was driving them. Recorded
    // before any removal reindexes the node's inputs.
    let doomed: Vec<&str> = legacy_keys
        .iter()
        .chain(surplus.iter())
        .copied()
        .chain((!target_port_kept).then_some(target))
        .filter(|key| !(target_port_kept && *key == target))
        .collect();
    let driven: Vec<(usize, NodeId, OutputPortIndex)> = components
        .iter()
        .enumerate()
        .filter_map(|(index, (legacy, _))| {
            let key = (*legacy)?;
            if !doomed.contains(&key) {
                return None;
            }
            let port = node.param_port_index(key)?;
            let edge = graph
                .edges()
                .find(|edge| edge.target == node_id && edge.target_port == port)?;
            Some((index, edge.source, edge.source_port))
        })
        .collect();
    let dropped_surplus_edges = surplus
        .iter()
        .filter_map(|key| node.param_port_index(key))
        .filter(|port| {
            graph
                .edges()
                .any(|edge| edge.target == node_id && edge.target_port == *port)
        })
        .count();
    let exposed_any = doomed
        .iter()
        .any(|key| node.param_port_index(key).is_some());

    let parameters = folded_parameters(node, target, components, surplus, value);
    let mut updated = (**node).clone();
    updated.parameters = parameters;
    let near = updated.clone();

    // `replace_node` re-inserts the node wholesale, so strip the doomed ports
    // from the replacement itself. A port whose name equals the folded key
    // (the v4 scalar `rotation`) would otherwise survive with its stale
    // SCALAR type, since a same-named parameter still exists.
    updated
        .inputs
        .retain(|port| !(port.is_param && doomed.contains(&port.name.as_str())));
    let mut graph = graph;
    for key in &doomed {
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
    if dropped_surplus_edges > 0 {
        tracing::warn!(
            node = node_id.raw(),
            key = target,
            dropped_surplus_edges,
            "dropped edges into component parameters this node's type does not read"
        );
    }
    if !exposed_any || target_port_kept {
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
    // The construct is only useful if the folded port accepts what it emits.
    let rescuable = construct_kind(components.len())
        .is_some_and(|(_, emitted)| new_accepted.contains(&emitted));
    if !rescuable {
        // No node rebuilds this shape into something the port takes. The
        // values themselves survive in the folded parameter.
        tracing::warn!(
            node = node_id.raw(),
            key = target,
            dropped_edges = driven.len(),
            "dropped edges into component parameters with no vector.construct equivalent"
        );
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

/// v4 keys of `attribute.set`'s `value` family, in component order.
const ATTRIBUTE_SET_VALUE_KEYS: [&str; 4] = ["value", "value_y", "value_z", "value_w"];

/// Fold `attribute.set`'s `value` family into one parameter shaped by the
/// node's stored `type`: `f32` → `Channel`, `vec2` → `Channel2`, `vec3` →
/// `Channel3`, `vec4` / `color` → `Channel4`. The types that read a different
/// parameter (`i32` / `bool` / `string`) keep `value` as an inert
/// 1-component channel.
///
/// Unlike the static [`FOLDS`] table this arity is per node instance, so the
/// spec is built here and handed to [`fold_one`].
fn fold_attribute_set(graph: Graph, node_id: NodeId) -> Graph {
    let Some(node) = graph.node(node_id) else {
        return graph;
    };
    let type_name = node
        .parameters
        .iter()
        .find(|p| p.key == "type")
        .and_then(|p| p.value.as_str())
        .unwrap_or(ATTRIBUTE_SET_DEFAULT_TYPE)
        .to_string();
    let defaults = attribute_set_value_defaults(&type_name);
    let components: Vec<Component> = defaults
        .iter()
        .enumerate()
        .map(|(index, default)| (Some(ATTRIBUTE_SET_VALUE_KEYS[index]), *default))
        .collect();
    let surplus: Vec<&str> = ATTRIBUTE_SET_VALUE_KEYS[defaults.len()..].to_vec();
    fold_one(graph, node_id, "value", &components, &surplus)
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
        if type_key == "attribute.set" {
            folded = fold_attribute_set(folded, id);
        }
        for (_, target, components) in FOLDS.iter().filter(|(key, _, _)| *key == type_key) {
            folded = fold_one(folded, id, target, components, &[]);
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
        value
            .channels()
            .unwrap_or_else(|| panic!("{key} is {value:?}, not channel-backed"))
            .iter()
            .map(|ch| ch.evaluate(0.0, &ctx()))
            .collect()
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

    /// A node with two foldable pairs folds both: `scatter.scatter` carries
    /// `center` and the scatter extent `area`.
    #[test]
    fn every_foldable_pair_of_a_node_is_folded() {
        let node = Node::new(NodeId::new(1), "scatter.scatter")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("area_x", ParameterValue::Float(120.0))
            .with_param("area_y", ParameterValue::Float(80.0))
            .with_param("center_x", ParameterValue::Float(5.0))
            .with_param("center_y", ParameterValue::Float(-5.0));
        let folded = fold_graph(&Graph::new().add_node(node).unwrap());
        assert_eq!(vector(&folded, NodeId::new(1), "area"), vec![120.0, 80.0]);
        assert_eq!(vector(&folded, NodeId::new(1), "center"), vec![5.0, -5.0]);
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

    /// A v4 `attribute.set` stores four `value_*` Floats whatever its `type`
    /// selects. The fold keeps exactly the components that `type` reads and
    /// drops the rest.
    #[test]
    fn attribute_set_value_folds_to_the_arity_its_type_reads() {
        let v4 = |type_name: &str| {
            Node::new(NodeId::new(1), "attribute.set")
                .with_input("geometry", &[DataTypeId::GEOMETRY])
                .with_output("output", DataTypeId::GEOMETRY)
                .with_param("type", ParameterValue::String(type_name.into()))
                .with_param("value", ParameterValue::Float(1.0))
                .with_param("value_y", ParameterValue::Float(2.0))
                .with_param("value_z", ParameterValue::Float(3.0))
                .with_param("value_w", ParameterValue::Float(4.0))
        };
        for (type_name, expected) in [
            ("f32", vec![1.0]),
            ("vec2", vec![1.0, 2.0]),
            ("vec3", vec![1.0, 2.0, 3.0]),
            ("vec4", vec![1.0, 2.0, 3.0, 4.0]),
            ("color", vec![1.0, 2.0, 3.0, 4.0]),
            // These read `int_value` / `bool_value` / `string_value`; `value`
            // survives as one inert channel.
            ("i32", vec![1.0]),
            ("bool", vec![1.0]),
            ("string", vec![1.0]),
        ] {
            let folded = fold_graph(&Graph::new().add_node(v4(type_name)).unwrap());
            assert_eq!(
                vector(&folded, NodeId::new(1), "value"),
                expected,
                "{type_name}"
            );
            let keys: Vec<&str> = folded
                .node(NodeId::new(1))
                .unwrap()
                .parameters
                .iter()
                .map(|p| p.key.as_str())
                .collect();
            assert_eq!(
                keys,
                ["type", "value"],
                "{type_name} drops the surplus keys"
            );
        }
    }

    /// A v4 file that stored only the first component fills the rest from the
    /// type's defaults — colour alpha included.
    #[test]
    fn attribute_set_partial_components_take_the_type_defaults() {
        let partial = |type_name: &str| {
            Node::new(NodeId::new(1), "attribute.set")
                .with_output("output", DataTypeId::GEOMETRY)
                .with_param("type", ParameterValue::String(type_name.into()))
                .with_param("value", ParameterValue::Float(0.5))
        };
        let folded = fold_graph(&Graph::new().add_node(partial("vec3")).unwrap());
        assert_eq!(
            vector(&folded, NodeId::new(1), "value"),
            vec![0.5, 0.0, 0.0]
        );
        let folded = fold_graph(&Graph::new().add_node(partial("color")).unwrap());
        assert_eq!(
            vector(&folded, NodeId::new(1), "value"),
            vec![0.5, 0.0, 0.0, 1.0],
            "colour alpha fills from its own default, not zero"
        );
    }

    /// A `f32` fold leaves the wire type alone (SCALAR before and after), so
    /// the exposed port and the edge driving it are untouched.
    #[test]
    fn a_scalar_attribute_set_value_keeps_its_port_and_edge() {
        let node = Node::new(NodeId::new(2), "attribute.set")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("type", ParameterValue::String("f32".into()))
            .with_param("value", ParameterValue::Float(1.0));
        let graph = Graph::new()
            .add_node(scalar_source(1, 8.0))
            .unwrap()
            .add_node(node)
            .unwrap()
            .expose_param_port(NodeId::new(2), "value")
            .unwrap();
        let port = graph
            .node(NodeId::new(2))
            .unwrap()
            .param_port_index("value")
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
        assert_eq!(folded.node_count(), 2, "no construct was needed");
        assert_eq!(
            folded
                .node(NodeId::new(2))
                .unwrap()
                .param_port_index("value"),
            Some(port),
            "the port did not move"
        );
        assert_eq!(folded.edge_count(), 1, "its edge survived");
    }

    /// A `vec3` fold changes the wire type to VEC3, so the driven component
    /// ports are rescued through a `vector.construct.vec3`.
    #[test]
    fn a_vec3_attribute_set_value_routes_its_drivers_through_a_construct() {
        let node = Node::new(NodeId::new(3), "attribute.set")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("type", ParameterValue::String("vec3".into()))
            .with_param("value", ParameterValue::Float(0.0))
            .with_param("value_y", ParameterValue::Float(7.0))
            .with_param("value_z", ParameterValue::Float(0.0));
        let graph = Graph::new()
            .add_node(scalar_source(1, 4.0))
            .unwrap()
            .add_node(scalar_source(2, -6.0))
            .unwrap()
            .add_node(node)
            .unwrap()
            .expose_param_port(NodeId::new(3), "value")
            .unwrap()
            .expose_param_port(NodeId::new(3), "value_z")
            .unwrap();
        let target = graph.node(NodeId::new(3)).unwrap();
        let (x, z) = (
            target.param_port_index("value").unwrap(),
            target.param_port_index("value_z").unwrap(),
        );
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
                z,
            )
            .unwrap();

        let folded = fold_graph(&graph);
        let construct = folded
            .nodes()
            .find(|node| node.type_key == VECTOR_CONSTRUCT_VEC3)
            .expect("vec3 construct inserted");
        let driven = |key: &str| {
            let port = construct.param_port_index(key)?;
            folded
                .edges()
                .find(|edge| edge.target == construct.id && edge.target_port == port)
                .map(|edge| edge.source)
        };
        assert_eq!(driven("x"), Some(NodeId::new(1)));
        assert_eq!(driven("z"), Some(NodeId::new(2)));
        assert!(
            construct.param_port_index("y").is_none(),
            "the undriven component stays a parameter"
        );
        // …and it kept the value the old file stored for it.
        let y = construct.parameters.iter().find(|p| p.key == "y").unwrap();
        assert_eq!(scalar_channel(&y.value).unwrap().evaluate(0.0, &ctx()), 7.0);
        let value_port = folded
            .node(NodeId::new(3))
            .unwrap()
            .param_port_index("value")
            .unwrap();
        assert!(folded.edges().any(|edge| edge.source == construct.id
            && edge.target == NodeId::new(3)
            && edge.target_port == value_port));
    }

    /// A 4-component `value` is rescued like any other arity: its parameter
    /// port accepts VEC4 as well as COLOR, so `vector.construct.vec4` can
    /// drive it. Both `vec4` and `color` behave the same — they are the two
    /// readings of the same four floats.
    #[test]
    fn a_four_component_attribute_set_value_keeps_its_drivers() {
        for type_name in ["vec4", "color"] {
            let node = Node::new(NodeId::new(3), "attribute.set")
                .with_output("output", DataTypeId::GEOMETRY)
                .with_param("type", ParameterValue::String(type_name.into()))
                .with_param("value", ParameterValue::Float(0.25))
                .with_param("value_y", ParameterValue::Float(0.5))
                .with_param("value_z", ParameterValue::Float(0.75))
                .with_param("value_w", ParameterValue::Float(1.0));
            let graph = Graph::new()
                .add_node(scalar_source(1, 9.0))
                .unwrap()
                .add_node(scalar_source(2, -3.0))
                .unwrap()
                .add_node(node)
                .unwrap()
                .expose_param_port(NodeId::new(3), "value")
                .unwrap()
                .expose_param_port(NodeId::new(3), "value_w")
                .unwrap();
            let target = graph.node(NodeId::new(3)).unwrap();
            let (x, w) = (
                target.param_port_index("value").unwrap(),
                target.param_port_index("value_w").unwrap(),
            );
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
                    w,
                )
                .unwrap();

            let folded = fold_graph(&graph);
            assert_eq!(
                vector(&folded, NodeId::new(3), "value"),
                vec![0.25, 0.5, 0.75, 1.0],
                "{type_name}: the stored components survive"
            );
            let target = folded.node(NodeId::new(3)).unwrap();
            let port = target.param_port_index("value").expect("{type_name}");
            assert_eq!(
                target.inputs[port.0 as usize].accepted_types,
                vec![DataTypeId::COLOR, DataTypeId::VEC4],
                "{type_name}: the folded port takes either reading"
            );

            let construct = folded
                .nodes()
                .find(|node| node.type_key == VECTOR_CONSTRUCT_VEC4)
                .unwrap_or_else(|| panic!("{type_name}: vec4 construct inserted"));
            let driven = |key: &str| {
                let port = construct.param_port_index(key)?;
                folded
                    .edges()
                    .find(|edge| edge.target == construct.id && edge.target_port == port)
                    .map(|edge| edge.source)
            };
            assert_eq!(driven("x"), Some(NodeId::new(1)), "{type_name}");
            assert_eq!(driven("w"), Some(NodeId::new(2)), "{type_name}");
            assert!(
                construct.param_port_index("y").is_none()
                    && construct.param_port_index("z").is_none(),
                "{type_name}: undriven components stay parameters"
            );
            // …carrying the values the old file stored for them.
            let stored = |key: &str| {
                scalar_channel(
                    &construct
                        .parameters
                        .iter()
                        .find(|p| p.key == key)
                        .unwrap()
                        .value,
                )
                .unwrap()
                .evaluate(0.0, &ctx())
            };
            assert_eq!((stored("y"), stored("z")), (0.5, 0.75), "{type_name}");
            assert!(
                folded.edges().any(|edge| edge.source == construct.id
                    && edge.target == NodeId::new(3)
                    && edge.target_port == port),
                "{type_name}: the construct drives the folded port"
            );
        }
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
