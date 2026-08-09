// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `.ravprj` v7 → v8: reinterpret authored colours for the linear working
//! space (`docs/implementation/color-management-plan.md`, CM-2).
//!
//! # What changed under the file
//!
//! Before v8 the pipeline was display-referred: a colour the author typed
//! went to the screen unchanged, so `0.5` *meant* mid-grey on the display.
//! From v8 compositing happens in linear light, and `0.5` means half the
//! light — visibly brighter. Every authored colour therefore has to be
//! reinterpreted once, `srgb → linear`, or every existing project changes
//! appearance.
//!
//! # A typed pass, not a manifest step
//!
//! Like the v4 → v5 fold and the v5 → v6 curve upgrade beside it, this runs
//! over the loaded [`Document`](super::Document).
//! [`migrate_v7_to_v8`](../../../ravel_project/migration/index.html) advances
//! the version stamp and nothing else — the untyped `manifest.json` chain
//! never sees `document/main.ron`.
//!
//! # What is a colour
//!
//! **Not decidable from the value.** A `Channel4` is a colour in
//! `constant.color` and a plain vector in `attribute.set` with
//! `type = "vec4"`, and the two serialize identically. The declared port
//! type decides, so the pass consults the node template
//! ([`is_color_param`]) and **converts nothing** for a node whose template
//! this build does not have — an unknown node type is reported, never
//! guessed at.
//!
//! # What cannot be converted
//!
//! | Channel source | Treatment |
//! |---|---|
//! | `Constant` | converted |
//! | `Keyframes` | every key converted, and **reported**: the keys still hit their old colours but the frames between them no longer interpolate the same way |
//! | `Expression` / `NodeOutput` / `Blend` / `AudioReactive` | **not converted, reported.** The value only exists at evaluation time |
//!
//! Alpha is not a colour channel and is never converted.
//!
//! Everything skipped is returned in a [`ColorMigrationReport`] so the load
//! can say so. Changing a project's look in silence is the one outcome worth
//! more than completeness.

use std::cell::RefCell;

use crate::animation::channel::{AnimationChannel, ChannelSource};
use crate::animation::curve::KeyframeCurve;
use crate::color::{ColorSpace, convert};
use crate::graph::{Graph, Node, ParameterValue};
use crate::id::{DataTypeId, NodeId};
use crate::registry::NodeRegistry;

/// One parameter the pass could not convert, or converted with a caveat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColorMigrationNote {
    pub node: NodeId,
    pub type_key: String,
    pub param: String,
}

/// What the v7 → v8 colour pass did and did not do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColorMigrationReport {
    /// Channels whose constant value was reinterpreted.
    pub converted: usize,
    /// Keyframed colours: the keys were converted, the interpolation between
    /// them was not (and cannot be — linear interpolation of linear light is
    /// a different curve from linear interpolation of sRGB).
    pub keyframed: Vec<ColorMigrationNote>,
    /// Colours driven by an expression, another node, or a blend. Their
    /// values do not exist until evaluation, so nothing could be rewritten.
    pub unresolved: Vec<ColorMigrationNote>,
    /// Vector parameters whose colour-ness could not be decided: an unknown
    /// node type, or a parameter the template does not declare.
    pub undecidable: Vec<ColorMigrationNote>,
}

impl ColorMigrationReport {
    /// Whether anything needs saying to the user.
    pub fn has_warnings(&self) -> bool {
        !self.keyframed.is_empty() || !self.unresolved.is_empty() || !self.undecidable.is_empty()
    }
}

/// Whether `key` on `node` is a colour, or `None` when this build cannot
/// tell.
///
/// The order matters. `attribute.set` carries its declared type in a sibling
/// parameter, so it is asked first; then a declared parameter port, which is
/// the only place a template states `COLOR` explicitly; then the template's
/// own default for the key, whose `Channel4` reads as `COLOR` and `Channel3`
/// as `VEC3` exactly as [`ParameterValue::port_data_type`] defines.
pub fn is_color_param(registry: &NodeRegistry, node: &Node, key: &str) -> Option<bool> {
    let template = registry.get(&node.type_key)?;

    // `attribute.set` writes eight different attribute types through one
    // `value` parameter; its `type` says which. A `vec4` attribute is not a
    // colour even though it is stored as a `Channel4`.
    if node.type_key == "attribute.set" && key == "value" {
        let declared = node
            .parameters
            .iter()
            .find(|p| p.key == "type")
            .and_then(|p| p.value.as_str())?;
        return Some(declared == "color");
    }

    if let Some(port) = template
        .inputs
        .iter()
        .find(|port| port.is_param && port.name == key)
    {
        return Some(
            port.accepted_types.contains(&DataTypeId::COLOR)
                && !port.accepted_types.contains(&DataTypeId::VEC4)
                && !port.accepted_types.contains(&DataTypeId::VEC3),
        );
    }

    let default = template.default_params.iter().find(|p| p.key == key)?;
    Some(default.value.port_data_type() == Some(DataTypeId::COLOR))
}

/// Reinterpret one authored value from the v7 display-referred meaning into
/// the linear working space.
fn linearize(value: f32) -> f32 {
    convert([value, value, value], ColorSpace::SRGB, ColorSpace::WORKING)[0]
}

/// Convert one channel, recording what could not be done.
fn upgrade_channel(
    channel: &AnimationChannel,
    note: &ColorMigrationNote,
    report: &mut ColorMigrationReport,
) -> AnimationChannel {
    match &channel.source {
        ChannelSource::Constant(value) => {
            report.converted += 1;
            AnimationChannel::constant(linearize(*value))
        }
        ChannelSource::Keyframes(curve) => {
            // Keys land on their old colours; the frames between them do
            // not, because the interpolant now runs through linear light.
            // Tangent handles are carried over unchanged for the same
            // reason they cannot be corrected: they describe a slope in the
            // old space and there is no slope in the new one that
            // reproduces the old curve.
            let mut upgraded = KeyframeCurve::with_default(linearize(curve.default_value()));
            for keyframe in curve.keyframes() {
                let mut keyframe = *keyframe;
                keyframe.value = linearize(keyframe.value);
                upgraded.insert_keyframe(keyframe);
            }
            report.converted += 1;
            report.keyframed.push(note.clone());
            AnimationChannel::keyframes(upgraded)
        }
        // Expression, NodeOutput, Blend, AudioReactive: the value is decided
        // at evaluation time. Leave it exactly as authored and say so.
        _ => {
            report.unresolved.push(note.clone());
            channel.clone()
        }
    }
}

/// Convert the colour channels of one parameter value. Alpha — the fourth
/// component of a `Channel4` — is left alone.
fn upgrade_value(
    value: &ParameterValue,
    note: &ColorMigrationNote,
    report: &mut ColorMigrationReport,
) -> Option<ParameterValue> {
    match value {
        ParameterValue::Channel3(channels) => {
            let upgraded = std::array::from_fn(|i| upgrade_channel(&channels[i], note, report));
            Some(ParameterValue::Channel3(upgraded))
        }
        ParameterValue::Channel4(channels) => {
            let upgraded = std::array::from_fn(|i| {
                if i == 3 {
                    channels[3].clone()
                } else {
                    upgrade_channel(&channels[i], note, report)
                }
            });
            Some(ParameterValue::Channel4(upgraded))
        }
        _ => None,
    }
}

/// Upgrade every authored colour in `graph`, descending into subnets.
pub(super) fn upgrade_graph(
    graph: &Graph,
    registry: &NodeRegistry,
    report: &RefCell<ColorMigrationReport>,
) -> Graph {
    super::graph_walk::map_subnets(graph, &|level| upgrade_level(level, registry, report))
}

fn upgrade_level(
    graph: &Graph,
    registry: &NodeRegistry,
    report: &RefCell<ColorMigrationReport>,
) -> Graph {
    let mut upgraded = graph.clone();
    for id in upgraded.node_ids().collect::<Vec<_>>() {
        let Some(node) = upgraded.node(id) else {
            continue;
        };
        // Synthetic nodes are compiled from the layer shell on every load and
        // never persisted, so upgrading one would convert a colour that the
        // next compile discards — or, worse, convert it twice.
        if node.metadata.synthetic {
            continue;
        }
        let mut rewrites = Vec::new();
        for param in &node.parameters {
            if !matches!(
                param.value,
                ParameterValue::Channel3(_) | ParameterValue::Channel4(_)
            ) {
                continue;
            }
            let note = ColorMigrationNote {
                node: id,
                type_key: node.type_key.clone(),
                param: param.key.clone(),
            };
            match is_color_param(registry, node, &param.key) {
                Some(true) => {
                    let mut report = report.borrow_mut();
                    if let Some(value) = upgrade_value(&param.value, &note, &mut report) {
                        rewrites.push((param.key.clone(), value));
                    }
                }
                // A declared vector: not a colour, deliberately untouched.
                Some(false) => {}
                None => report.borrow_mut().undecidable.push(note),
            }
        }
        if rewrites.is_empty() {
            continue;
        }
        // Written through `replace_node` rather than `Graph::set_params`: the
        // value keeps its shape, so no exposed parameter port changes type
        // and none of the port bookkeeping `set_params` exists for applies.
        let mut updated = (**node).clone();
        for (key, value) in rewrites {
            if let Some(param) = updated.parameters.iter_mut().find(|p| p.key == key) {
                param.value = value;
            }
        }
        upgraded = upgraded.replace_node(std::sync::Arc::new(updated));
    }
    upgraded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::Interpolation;
    use crate::graph::Parameter;
    use crate::registry::builtin::register_builtins;

    fn registry() -> NodeRegistry {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        registry
    }

    fn channel4(source: [ChannelSource; 4]) -> ParameterValue {
        ParameterValue::Channel4(source.map(AnimationChannel::new))
    }

    /// `Node::with_param` appends, so a template default has to be replaced
    /// rather than shadowed.
    fn set(mut node: Node, key: &str, value: ParameterValue) -> Node {
        match node.parameters.iter_mut().find(|p| p.key == key) {
            Some(param) => param.value = value,
            None => node.parameters.push(Parameter {
                key: key.into(),
                value,
            }),
        }
        node
    }

    fn param_of(graph: &Graph, id: u64, key: &str) -> ParameterValue {
        graph
            .node(NodeId::new(id))
            .expect("node")
            .parameters
            .iter()
            .find(|p| p.key == key)
            .expect("parameter")
            .value
            .clone()
    }

    fn constants(value: &ParameterValue) -> Vec<Option<f32>> {
        value
            .channels()
            .expect("channels")
            .iter()
            .map(|channel| match channel.source {
                ChannelSource::Constant(v) => Some(v),
                _ => None,
            })
            .collect()
    }

    fn run(graph: Graph) -> (Graph, ColorMigrationReport) {
        let registry = registry();
        let report = RefCell::new(ColorMigrationReport::default());
        let upgraded = upgrade_graph(&graph, &registry, &report);
        (upgraded, report.into_inner())
    }

    /// CM-2: a constant colour is reinterpreted, and `linear → srgb` puts it
    /// back where the author left it.
    #[test]
    fn a_constant_colour_converts_and_inverts() {
        let node = set(
            registry()
                .create_node("constant.color", NodeId::new(1))
                .expect("template"),
            "color",
            channel4([
                ChannelSource::Constant(0.5),
                ChannelSource::Constant(0.25),
                ChannelSource::Constant(1.0),
                ChannelSource::Constant(0.75),
            ]),
        );
        let (graph, report) = run(Graph::new().add_node(node).unwrap());

        let values = constants(&param_of(&graph, 1, "color"));
        assert!(
            (values[0].unwrap() - 0.214_041_1).abs() < 1e-5,
            "{values:?}"
        );
        // Alpha carries no transfer function.
        assert_eq!(values[3], Some(0.75));
        // Round trip: the author's number comes back out.
        for (converted, original) in values.iter().zip([0.5, 0.25, 1.0]) {
            let back = convert(
                [converted.unwrap(); 3],
                ColorSpace::WORKING,
                ColorSpace::SRGB,
            )[0];
            assert!((back - original).abs() < 1e-5, "{back} vs {original}");
        }
        assert_eq!(report.converted, 3);
        assert!(!report.has_warnings());
    }

    /// CM-2: keyframed colours are converted key by key and the changed
    /// interpolation is reported.
    #[test]
    fn keyframed_colours_convert_their_keys_and_warn() {
        let mut curve = KeyframeCurve::with_default(0.5);
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 1.0, Interpolation::Linear);
        curve.insert(20, 0.5, Interpolation::Linear);

        let node = set(
            registry()
                .create_node("constant.color", NodeId::new(1))
                .expect("template"),
            "color",
            channel4([
                ChannelSource::Keyframes(curve),
                ChannelSource::Constant(0.0),
                ChannelSource::Constant(0.0),
                ChannelSource::Constant(1.0),
            ]),
        );
        let (graph, report) = run(Graph::new().add_node(node).unwrap());

        let channels = param_of(&graph, 1, "color").channels().unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        let values: Vec<f32> = curve.keyframes().iter().map(|k| k.value).collect();
        assert_eq!(values[0], 0.0);
        assert_eq!(values[1], 1.0);
        assert!((values[2] - 0.214_041_1).abs() < 1e-5, "{values:?}");

        assert_eq!(report.keyframed.len(), 1);
        assert_eq!(report.keyframed[0].param, "color");
        assert!(report.unresolved.is_empty());
    }

    /// CM-2: an expression-driven colour is left alone and reported, because
    /// its value only exists at evaluation time.
    #[test]
    fn expression_driven_colours_are_reported_not_converted() {
        use crate::animation::channel::ParameterExpression;

        let expression = ChannelSource::Expression(ParameterExpression::new("t"));
        let node = set(
            registry()
                .create_node("constant.color", NodeId::new(1))
                .expect("template"),
            "color",
            channel4([
                expression.clone(),
                ChannelSource::NodeOutput(NodeId::new(9), crate::id::OutputPortIndex(0)),
                ChannelSource::Blend(
                    Box::new(ChannelSource::Constant(0.2)),
                    Box::new(ChannelSource::Constant(0.8)),
                    Default::default(),
                    0.5,
                ),
                ChannelSource::Constant(1.0),
            ]),
        );
        let (graph, report) = run(Graph::new().add_node(node).unwrap());

        let channels = param_of(&graph, 1, "color").channels().unwrap();
        assert_eq!(channels[0].source, expression);
        assert_eq!(report.unresolved.len(), 3);
        assert_eq!(report.converted, 0);
    }

    /// CM-2: a `vec4` is not a colour. `attribute.set` proves it — the same
    /// `Channel4` is a colour or a vector depending on its `type`.
    #[test]
    fn declared_vectors_are_left_alone() {
        let grey = || {
            channel4([
                ChannelSource::Constant(0.5),
                ChannelSource::Constant(0.5),
                ChannelSource::Constant(0.5),
                ChannelSource::Constant(0.5),
            ])
        };
        let vector = set(
            set(
                registry()
                    .create_node("attribute.set", NodeId::new(1))
                    .expect("template"),
                "type",
                ParameterValue::String("vec4".into()),
            ),
            "value",
            grey(),
        );
        let colour = set(
            set(
                registry()
                    .create_node("attribute.set", NodeId::new(2))
                    .expect("template"),
                "type",
                ParameterValue::String("color".into()),
            ),
            "value",
            grey(),
        );
        let (graph, report) = run(Graph::new()
            .add_node(vector)
            .unwrap()
            .add_node(colour)
            .unwrap());

        assert_eq!(
            constants(&param_of(&graph, 1, "value")),
            vec![Some(0.5); 4],
            "a vec4 must not be reinterpreted"
        );
        let converted = constants(&param_of(&graph, 2, "value"));
        assert!((converted[0].unwrap() - 0.214_041_1).abs() < 1e-5);
        assert_eq!(converted[3], Some(0.5), "alpha stays put");
        assert_eq!(report.converted, 3);
        assert!(report.undecidable.is_empty());
    }

    /// A `Channel3` reads as `VEC3`, so it is a vector and stays put.
    #[test]
    fn three_component_vectors_are_left_alone() {
        let node = registry()
            .create_node("transform", NodeId::new(1))
            .expect("template");
        let (graph, report) = run(Graph::new().add_node(node.clone()).unwrap());
        for param in &node.parameters {
            if matches!(param.value, ParameterValue::Channel3(_)) {
                assert_eq!(
                    param_of(&graph, 1, &param.key),
                    param.value,
                    "{} was reinterpreted",
                    param.key
                );
            }
        }
        assert_eq!(report.converted, 0);
    }

    /// CM-2: a node type this build does not know is reported, never
    /// guessed at.
    #[test]
    fn an_unknown_node_type_is_reported_not_converted() {
        let node = Node::new(NodeId::new(1), "third.party.grade").with_param(
            "tint",
            channel4([
                ChannelSource::Constant(0.5),
                ChannelSource::Constant(0.5),
                ChannelSource::Constant(0.5),
                ChannelSource::Constant(1.0),
            ]),
        );
        let (graph, report) = run(Graph::new().add_node(node).unwrap());

        assert_eq!(
            constants(&param_of(&graph, 1, "tint")),
            vec![Some(0.5); 3]
                .into_iter()
                .chain([Some(1.0)])
                .collect::<Vec<_>>()
        );
        assert_eq!(report.converted, 0);
        assert_eq!(report.undecidable.len(), 1);
        assert_eq!(report.undecidable[0].type_key, "third.party.grade");
    }
}
