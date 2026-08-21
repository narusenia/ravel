// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property sections for graph nodes.

use ravel_core::animation::channel::AnimationChannel;
use ravel_core::eval::EvalContext;
use ravel_core::graph::{Node, Parameter, ParameterValue, PortSide};
use ravel_core::network::{
    CustomPortType, NetworkContext, custom_port_type, is_fixed_port, is_in_node, is_out_node,
};
use ravel_core::registry::{NodeRegistry, ParamRange};

use std::collections::HashSet;

use super::{DrivenParam, PortRow, PropertyField, PropertySection};

/// Field key of the interface node's port list. One list per node, so the key
/// names the section's single field rather than any port.
pub const FIELD_PORTS: &str = "ports";

/// Display value for an animated channel at `frame` (the owning layer's
/// local frame, REQ-LAYER-004/006).
///
/// This is the channel's own evaluation, so an expression-driven parameter
/// displays what it actually computes rather than a placeholder. `eval` is
/// read for the vocabulary an expression may name (`fps`, the resolutions);
/// the frame is passed separately because a layer's local frame is not the
/// composition's. Sources with no resolved value yet — a node output, an
/// audio-reactive binding — still answer the channel default.
///
/// Shared with read-only displays that sample the document the same way (the
/// node editor's hover popover).
pub fn channel_display_value(ch: &AnimationChannel, frame: u64, eval: &EvalContext) -> f32 {
    ch.evaluate(frame as f64, eval)
}

/// Build an info section with read-only node metadata.
///
/// The label field carries literal text or a locale key (see
/// [`crate::node_locale::label_or_key`]): a user rename as-is, the
/// `node.<type_key>.label` key for a registered type — which the host
/// translates through `read_only_value` — else the bare `type_key`.
pub fn node_info_section(node: &Node, registry: &NodeRegistry) -> PropertySection {
    let label = crate::node_locale::label_or_key(node, registry);

    PropertySection {
        title: "properties.section.node_info".into(),
        fields: vec![
            PropertyField::ReadOnly {
                key: "type".into(),
                value: node.type_key.clone(),
            },
            PropertyField::ReadOnly {
                key: "label".into(),
                value: label,
            },
            PropertyField::ReadOnly {
                key: "id".into(),
                value: format!("{}", node.id),
            },
        ],
    }
}

/// One int row, shared by the constant `Int` and the animatable `IntChannel`:
/// the two are one quantity stored two ways, and a row that differed between
/// them would change shape under the user the moment they keyed it.
fn int_field(key: String, value: i32, ranges: Option<&ParamRange>) -> PropertyField {
    PropertyField::Int {
        key,
        value,
        range: ranges.map(|r| (*r.hard.start() as i32)..=(*r.hard.end() as i32)),
        ui_range: ranges.map(|r| (*r.ui.start() as i32)..=(*r.ui.end() as i32)),
        step: Some(1),
    }
}

/// One string row, shared by the constant `String` and the animatable
/// `StringSteps` for the same reason. A registry-declared closed option set
/// renders as an enum dropdown; free-form strings stay editable text.
fn string_field(
    key: String,
    value: String,
    registry: &NodeRegistry,
    type_key: &str,
) -> PropertyField {
    match registry.param_options(type_key, &key) {
        Some(options) => PropertyField::Enum {
            key,
            value,
            options: options.to_vec(),
        },
        None => PropertyField::String { key, value },
    }
}

/// Split `node`'s parameters into display sections.
///
/// Returns `(group, title, parameters)` in section order. `group` is the
/// identity the collapse state keys on (`""` for the implicit group) and
/// `title` is what the host shows: the group's locale key for a
/// type-declared group, the user's own text for an In node's instance group.
///
/// Two sources of grouping, in precedence order:
///
/// 1. **The node's own** [`Node::param_groups`], on a network-interface **In**
///    node only — the instance groups the user assigns to custom parameters,
///    which have no type to declare them (`NETIF-2`, PGRP-4). They win
///    outright on that node: the user assigned them by hand, and merging them
///    with a type declaration would leave it unclear which half a parameter
///    answers to. Their section order follows the parameter order, which is
///    the order the ports were added.
/// 2. **The registry template's** [`NodeTemplate::param_groups`] — what the
///    node *type* declares.
///
/// A type that declares nothing (and an instance that assigns nothing) gets
/// one section holding every parameter, exactly as before groups existed.
///
/// A non-In node carrying [`Node::param_groups`] — only a hand-edited
/// `.ravprj` can produce one, since nothing writes there — falls back to its
/// type's declaration. The editing path exists on In nodes alone, so the
/// reading path matches it; refusing to *open* such a document instead would
/// trade a cosmetic oddity for a project that saved and will not load
/// (`HIGH-26`).
///
/// Parameters no group claims come **first**, in one section titled
/// `properties.section.parameters`, so a type that declares no groups (or
/// declares them for only some of its parameters) keeps the order and the
/// heading it always had.
///
/// A declaration naming a parameter the node does not have contributes
/// nothing, and a group left with no members is dropped rather than rendered
/// empty: a typo in a template costs that group, never the panel's rows. A
/// key named by two groups belongs to the **first** one that names it, and a
/// group name declared twice is one section — the name is the collapse
/// identity, so two sections under it could not be told apart.
///
/// [`NodeTemplate::param_groups`]: ravel_core::registry::NodeTemplate::param_groups
fn grouped_params<'a>(
    node: &'a Node,
    registry: &NodeRegistry,
) -> Vec<(String, String, Vec<&'a Parameter>)> {
    let mut groups: Vec<(String, String, Vec<&'a Parameter>)> = Vec::new();
    let mut claimed: HashSet<&str> = HashSet::new();
    let push = |groups: &mut Vec<(String, String, Vec<&'a Parameter>)>,
                group: &str,
                title: String,
                param: &'a Parameter| {
        match groups.iter_mut().find(|(id, ..)| id == group) {
            Some((.., members)) => members.push(param),
            None => groups.push((group.to_string(), title, vec![param])),
        }
    };

    if ravel_core::network::is_in_node(node) && !node.param_groups.is_empty() {
        for param in &node.parameters {
            let Some(group) = node.param_groups.get(&param.key).filter(|g| !g.is_empty()) else {
                continue;
            };
            push(&mut groups, group, group.clone(), param);
            claimed.insert(param.key.as_str());
        }
    } else if let Some(template) = registry.get(&node.type_key) {
        for (group, keys) in template.param_group_declarations() {
            for key in keys {
                if claimed.contains(key.as_str()) {
                    continue;
                }
                let Some(param) = node.parameters.iter().find(|p| &p.key == key) else {
                    continue;
                };
                claimed.insert(param.key.as_str());
                push(
                    &mut groups,
                    group,
                    crate::node_locale::group_key(&node.type_key, group),
                    param,
                );
            }
        }
    }

    let ungrouped: Vec<&'a Parameter> = node
        .parameters
        .iter()
        .filter(|p| !claimed.contains(p.key.as_str()))
        .collect();
    if !ungrouped.is_empty() {
        groups.insert(
            0,
            (
                String::new(),
                "properties.section.parameters".to_string(),
                ungrouped,
            ),
        );
    }
    groups
}

/// The parameter groups `node` displays: `(group, section title)` in section
/// order, the same split [`node_params_sections`] renders.
///
/// The host needs the pair to key the collapse state on `(type_key, group)`
/// while showing `title`: `title` alone would put a locale key in
/// `ui_state.json`. An In node's instance groups key on `net.in` like any
/// other type, so folding "Look" away folds it on every In node that has
/// one — the same trade the type-declared groups make.
pub fn param_group_titles(node: &Node, registry: &NodeRegistry) -> Vec<(String, String)> {
    grouped_params(node, registry)
        .into_iter()
        .map(|(group, title, _)| (group, title))
        .collect()
}

/// One property row for the parameter `p` of `node`, sampling animated
/// channels at `frame` (the owning layer's local frame).
///
/// Each `ParameterValue` variant maps to the corresponding `PropertyField`
/// variant. Numeric fields pick up hard/UI ranges from the node's registry
/// template when one is declared. String parameters with a registry-declared
/// option set (e.g. merge `operation`, math `op`) render as an `Enum`.
fn param_field(
    node: &Node,
    p: &Parameter,
    registry: &NodeRegistry,
    frame: u64,
    eval: &EvalContext,
    driven: &[DrivenParam],
) -> PropertyField {
    // A parameter driven by a connected port is read-only: the
    // stored value is an inert fallback while the edge exists
    // (param-input-ports-plan Phase 4).
    if let Some(driving) = driven.iter().find(|d| d.key == p.key) {
        let value = driving.value.as_deref().unwrap_or("connected");
        return PropertyField::ReadOnly {
            key: p.key.clone(),
            value: format!("{value} ← {}", driving.source),
        };
    }
    let ranges = registry.param_range(&node.type_key, &p.key);
    match &p.value {
        ParameterValue::Float(v) => PropertyField::Float {
            key: p.key.clone(),
            value: *v,
            range: ranges.map(|r| r.hard.clone()),
            ui_range: ranges.map(|r| r.ui.clone()),
            step: Some(0.01),
        },
        ParameterValue::Int(v) => int_field(p.key.clone(), *v, ranges),
        ParameterValue::Bool(v) => PropertyField::Bool {
            key: p.key.clone(),
            value: *v,
        },
        ParameterValue::String(v) => {
            string_field(p.key.clone(), v.clone(), registry, &node.type_key)
        }
        ParameterValue::Channel(ch) => PropertyField::Float {
            key: p.key.clone(),
            value: channel_display_value(ch, frame, eval),
            range: ranges.map(|r| r.hard.clone()),
            ui_range: ranges.map(|r| r.ui.clone()),
            step: Some(0.01),
        },
        ParameterValue::Channel2(chs) => PropertyField::Vector {
            key: p.key.clone(),
            components: chs
                .iter()
                .map(|ch| channel_display_value(ch, frame, eval))
                .collect(),
            range: ranges.map(|r| r.hard.clone()),
            ui_range: ranges.map(|r| r.ui.clone()),
            step: Some(0.01),
        },
        ParameterValue::Channel3(chs) => PropertyField::Vector {
            key: p.key.clone(),
            components: chs
                .iter()
                .map(|ch| channel_display_value(ch, frame, eval))
                .collect(),
            range: ranges.map(|r| r.hard.clone()),
            ui_range: ranges.map(|r| r.ui.clone()),
            step: Some(0.01),
        },
        ParameterValue::Channel4(chs) => PropertyField::Color {
            key: p.key.clone(),
            r: channel_display_value(&chs[0], frame, eval),
            g: channel_display_value(&chs[1], frame, eval),
            b: channel_display_value(&chs[2], frame, eval),
            a: channel_display_value(&chs[3], frame, eval),
        },
        // Path control points are edited on the canvas (pen tool);
        // Properties shows a read-only summary (REQ-UI-011).
        ParameterValue::PathPoints(points) => PropertyField::ReadOnly {
            key: p.key.clone(),
            value: format!("{} points", points.len()),
        },
        // Curves carry their control points into the row; the host
        // shows a thumbnail and expands an inline editor under it.
        ParameterValue::Curve(curve) => PropertyField::Curve {
            key: p.key.clone(),
            curve: curve.clone(),
        },
        // Ramps carry their stops into the row the same way; the host
        // shows a gradient band and expands an inline editor under it.
        ParameterValue::Ramp(ramp) => PropertyField::Ramp {
            key: p.key.clone(),
            ramp: ramp.clone(),
        },
        // An animatable int is the same row as a constant one: the
        // spinner edits the int this frame reads, and the write path
        // keeps the channel (a keyed channel gains a key at the frame,
        // a constant one has its constant replaced). Anything else
        // would make the row a display that discards its own edits.
        ParameterValue::IntChannel(ch) => int_field(
            p.key.clone(),
            channel_display_value(ch, frame, eval).round() as i32,
            ranges,
        ),
        // Likewise for an animatable string: the same text box or
        // dropdown as the constant spelling, showing the string this
        // frame holds.
        ParameterValue::StringSteps(steps) => string_field(
            p.key.clone(),
            steps.sample(frame as f64).clone(),
            registry,
            &node.type_key,
        ),
    }
}

/// Build the parameter sections of a node, sampling animated channels at
/// `frame` (the owning layer's local frame).
///
/// One section per display group ([`grouped_params`]); a node whose type
/// declares no groups gets the single section it always had.
pub fn node_params_sections(
    node: &Node,
    registry: &NodeRegistry,
    frame: u64,
    eval: &EvalContext,
    driven: &[DrivenParam],
) -> Vec<PropertySection> {
    grouped_params(node, registry)
        .into_iter()
        .map(|(_, title, params)| PropertySection {
            title,
            fields: params
                .into_iter()
                .map(|p| param_field(node, p, registry, frame, eval, driven))
                .collect(),
        })
        .collect()
}

/// Build the Ports section of a network interface node, or `None` for any
/// other node (REQ-LAYER-002/003).
///
/// The In node's custom ports are outputs and the Out node's are inputs
/// (`interface_side`), so the section describes exactly one side and says
/// which. Every port on that side becomes a row, fixed ones included: the
/// section is a view of the node's interface, and a list that quietly omitted
/// `base_geometry` would not match the node the user is looking at.
///
/// `context` picks the type menu — the shell feeds a layer-root In values
/// only, while a subnet's inner In is a pin boundary that takes anything a
/// wire carries. An Out node's set does not depend on it
/// ([`CustomPortType::allowed_for_out`]).
pub fn node_ports_section(node: &Node, context: NetworkContext) -> Option<PropertySection> {
    let (side, options) = if is_in_node(node) {
        (PortSide::Output, CustomPortType::allowed_for_in(context))
    } else if is_out_node(node) {
        (PortSide::Input, CustomPortType::allowed_for_out())
    } else {
        return None;
    };
    let names: Vec<&str> = match side {
        PortSide::Input => node.inputs.iter().map(|p| p.name.as_str()).collect(),
        PortSide::Output => node.outputs.iter().map(|p| p.name.as_str()).collect(),
    };
    let rows = names
        .into_iter()
        .map(|name| PortRow {
            name: name.to_string(),
            port_type: custom_port_type(node, side, name),
            fixed: is_fixed_port(node, side, name),
            // A group belongs to the parameter, so a port without one has no
            // cell rather than an empty one. Read from the node, which is the
            // only place the assignment lives.
            group: node
                .parameters
                .iter()
                .any(|p| p.key == name)
                .then(|| node.param_groups.get(name).cloned().unwrap_or_default()),
        })
        .collect();
    Some(PropertySection {
        title: "properties.section.ports".into(),
        fields: vec![PropertyField::PortList {
            key: FIELD_PORTS.into(),
            side,
            rows,
            options: options.to_vec(),
        }],
    })
}

/// Build all sections for a single node, sampling animated channels at
/// `frame` (the owning layer's local frame).
///
/// `context` only reaches [`node_ports_section`]; every other section is the
/// same wherever the network sits.
pub fn sections_for_node(
    node: &Node,
    registry: &NodeRegistry,
    frame: u64,
    eval: &EvalContext,
    driven: &[DrivenParam],
    context: NetworkContext,
) -> Vec<PropertySection> {
    let mut sections = vec![node_info_section(node, registry)];
    sections.extend(node_params_sections(node, registry, frame, eval, driven));
    // Last: the ports are the node's shape, and a user reading an In node
    // wants its values before its plumbing.
    sections.extend(node_ports_section(node, context));
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::registry::builtin::register_builtins;

    /// The single parameters section of a node whose type declares no groups
    /// — the shape every test below this line was written against (an empty
    /// one for a node with no parameters, which is what the pre-split
    /// builder returned). It shadows the glob-imported plural, so a node that
    /// *does* declare groups fails here instead of silently having its extra
    /// sections dropped.
    fn node_params_section(
        node: &Node,
        registry: &NodeRegistry,
        frame: u64,
        eval: &EvalContext,
        driven: &[DrivenParam],
    ) -> PropertySection {
        let mut sections = node_params_sections(node, registry, frame, eval, driven);
        assert!(
            sections.len() <= 1,
            "{} declares parameter groups",
            node.type_key
        );
        sections.pop().unwrap_or(PropertySection {
            title: "properties.section.parameters".into(),
            fields: Vec::new(),
        })
    }

    /// Every parameter row of a node, whatever group it sits in — for the
    /// tests that look a row up by key and do not care which section holds
    /// it.
    fn node_params_fields(
        node: &Node,
        registry: &NodeRegistry,
        frame: u64,
        eval: &EvalContext,
        driven: &[DrivenParam],
    ) -> Vec<PropertyField> {
        node_params_sections(node, registry, frame, eval, driven)
            .into_iter()
            .flat_map(|section| section.fields)
            .collect()
    }

    /// Display context for the sections. Only `fps` and the resolutions are
    /// read, and only by an expression-driven channel.
    fn eval() -> ravel_core::eval::EvalContext {
        ravel_core::eval::EvalContext::new(
            0,
            ravel_core::types::FrameRate::new(30, 1),
            (1920, 1080),
        )
    }

    fn registry() -> NodeRegistry {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        reg
    }

    #[test]
    fn info_section_shows_type_and_label() {
        let node = Node::new(NodeId::new(1), "blur")
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_label("My Blur");
        let section = node_info_section(&node, &registry());
        assert_eq!(section.title, "properties.section.node_info");
        assert_eq!(section.fields.len(), 3);
        match &section.fields[0] {
            PropertyField::ReadOnly { key, value } => {
                assert_eq!(key, "type");
                assert_eq!(value, "blur");
            }
            _ => panic!("expected ReadOnly"),
        }
        match &section.fields[1] {
            PropertyField::ReadOnly { key, value } => {
                assert_eq!(key, "label");
                assert_eq!(value, "My Blur");
            }
            _ => panic!("expected ReadOnly"),
        }
    }

    /// A node that still carries its template's default label shows the
    /// locale key instead; the host translates it (a user rename would win).
    #[test]
    fn info_section_emits_the_locale_key_for_a_default_label() {
        let registry = registry();
        let node = registry
            .create_node("blur", NodeId::new(1))
            .expect("blur is registered");
        let section = node_info_section(&node, &registry);
        match &section.fields[1] {
            PropertyField::ReadOnly { key, value } => {
                assert_eq!(key, "label");
                assert_eq!(value, "node.blur.label");
            }
            _ => panic!("expected ReadOnly"),
        }
    }

    /// An unregistered type has no locale entry to point at, so the label
    /// falls back to the bare type key.
    #[test]
    fn info_section_falls_back_to_the_type_key_for_unknown_types() {
        let node = Node::new(NodeId::new(1), "plugin.custom");
        let section = node_info_section(&node, &registry());
        match &section.fields[1] {
            PropertyField::ReadOnly { key, value } => {
                assert_eq!(key, "label");
                assert_eq!(value, "plugin.custom");
            }
            _ => panic!("expected ReadOnly"),
        }
    }

    #[test]
    fn params_section_maps_float() {
        let node =
            Node::new(NodeId::new(1), "blur").with_param("radius", ParameterValue::Float(5.0));
        let section = node_params_section(&node, &registry(), 0, &eval(), &[]);
        assert_eq!(section.fields.len(), 1);
        match &section.fields[0] {
            PropertyField::Float { key, value, .. } => {
                assert_eq!(key, "radius");
                assert!((value - 5.0).abs() < f32::EPSILON);
            }
            _ => panic!("expected Float"),
        }
    }

    #[test]
    fn params_section_maps_operation_to_enum() {
        let node = Node::new(NodeId::new(1), "merge")
            .with_param("operation", ParameterValue::String("over".into()));
        let section = node_params_section(&node, &registry(), 0, &eval(), &[]);
        match &section.fields[0] {
            PropertyField::Enum {
                key,
                value,
                options,
            } => {
                assert_eq!(key, "operation");
                assert_eq!(value, "over");
                assert_eq!(options.len(), 3);
            }
            _ => panic!("expected Enum"),
        }
    }

    #[test]
    fn sections_for_node_returns_info_and_params() {
        let node = Node::new(NodeId::new(1), "color_correct")
            .with_param("brightness", ParameterValue::Float(0.0))
            .with_param("contrast", ParameterValue::Float(1.0));
        let sections = sections_for_node(
            &node,
            &registry(),
            0,
            &eval(),
            &[],
            NetworkContext::LayerRoot,
        );
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "properties.section.node_info");
        assert_eq!(sections[1].title, "properties.section.parameters");
    }

    #[test]
    fn sections_for_node_without_params() {
        let node = Node::new(NodeId::new(1), "passthrough");
        let sections = sections_for_node(
            &node,
            &registry(),
            0,
            &eval(),
            &[],
            NetworkContext::LayerRoot,
        );
        assert_eq!(sections.len(), 1);
    }

    #[test]
    fn params_section_picks_up_registry_ranges() {
        let node =
            Node::new(NodeId::new(1), "blur").with_param("radius", ParameterValue::Float(5.0));
        let section = node_params_section(&node, &registry(), 0, &eval(), &[]);
        match &section.fields[0] {
            PropertyField::Float {
                range, ui_range, ..
            } => {
                assert_eq!(range.clone().unwrap(), 0.0..=64.0);
                assert_eq!(ui_range.clone().unwrap(), 0.0..=50.0);
            }
            _ => panic!("expected Float"),
        }
    }

    #[test]
    fn int_params_cast_registry_ranges() {
        let node =
            Node::new(NodeId::new(1), "shape.polygon").with_param("sides", ParameterValue::Int(6));
        let section = node_params_section(&node, &registry(), 0, &eval(), &[]);
        match &section.fields[0] {
            PropertyField::Int {
                range, ui_range, ..
            } => {
                assert_eq!(range.clone().unwrap(), 3..=128);
                assert_eq!(ui_range.clone().unwrap(), 3..=32);
            }
            _ => panic!("expected Int"),
        }
    }

    #[test]
    fn unknown_type_key_yields_no_ranges() {
        let node = Node::new(NodeId::new(1), "plugin.custom")
            .with_param("strength", ParameterValue::Float(1.0));
        let section = node_params_section(&node, &registry(), 0, &eval(), &[]);
        match &section.fields[0] {
            PropertyField::Float {
                range, ui_range, ..
            } => {
                assert!(range.is_none());
                assert!(ui_range.is_none());
            }
            _ => panic!("expected Float"),
        }
    }

    /// Curve parameters reach the panel as a curve row carrying the control
    /// points, so the inline editor edits the stored curve rather than a
    /// re-parsed summary.
    #[test]
    fn params_section_maps_curves_to_curve_rows() {
        use ravel_core::param_curve::CurveParam;
        let stored = CurveParam::linear([(0.0, 0.0), (0.5, 0.8), (1.0, 1.0)]);
        let node = Node::new(NodeId::new(1), "field.curve_remap")
            .with_param("points", ParameterValue::Curve(stored.clone()));
        let section = node_params_section(&node, &registry(), 0, &eval(), &[]);
        match &section.fields[0] {
            PropertyField::Curve { key, curve } => {
                assert_eq!(key, "points");
                assert_eq!(curve, &stored);
            }
            other => panic!("expected Curve, got {other:?}"),
        }
    }

    /// The registry template of `field.curve_remap` — the shipped curve
    /// consumer — produces a curve row without any per-node special casing.
    #[test]
    fn the_curve_remap_template_produces_a_curve_row() {
        let registry = registry();
        let node = registry
            .create_node("field.curve_remap", NodeId::new(1))
            .expect("field.curve_remap is registered");
        let section = node_params_section(&node, &registry, 0, &eval(), &[]);
        assert!(
            section
                .fields
                .iter()
                .any(|field| matches!(field, PropertyField::Curve { key, .. } if key == "points")),
            "field.curve_remap must offer an editable curve row: {:?}",
            section.fields
        );
    }

    #[test]
    fn the_math_curve_template_produces_the_same_curve_row() {
        let registry = registry();
        let node = registry
            .create_node("math.curve", NodeId::new(1))
            .expect("math.curve is registered");
        let fields = node_params_fields(&node, &registry, 0, &eval(), &[]);
        assert!(
            fields
                .iter()
                .any(|field| matches!(field, PropertyField::Curve { key, .. } if key == "curve")),
            "math.curve must offer the same editable curve row: {fields:?}"
        );
    }

    /// Ramp parameters reach the panel as a ramp row carrying the stops, so
    /// the inline gradient editor edits the stored ramp rather than a
    /// re-parsed summary — the same contract curve rows have.
    #[test]
    fn params_section_maps_ramps_to_ramp_rows() {
        use ravel_core::param_ramp::{RampInterpolation, RampParam};
        use ravel_core::types::Color;
        let stored = RampParam::linear([(0.0, Color::BLACK), (0.5, Color::WHITE)])
            .with_interpolation(RampInterpolation::Smooth);
        let node = Node::new(NodeId::new(1), "field.ramp")
            .with_param("stops", ParameterValue::Ramp(stored.clone()));
        let section = node_params_section(&node, &registry(), 0, &eval(), &[]);
        match &section.fields[0] {
            PropertyField::Ramp { key, ramp } => {
                assert_eq!(key, "stops");
                assert_eq!(ramp, &stored);
            }
            other => panic!("expected Ramp, got {other:?}"),
        }
    }

    /// The registry template of `field.ramp` — the shipped ramp consumer —
    /// produces a ramp row without any per-node special casing.
    #[test]
    fn the_field_ramp_template_produces_a_ramp_row() {
        let registry = registry();
        let node = registry
            .create_node("field.ramp", NodeId::new(1))
            .expect("field.ramp is registered");
        let section = node_params_section(&node, &registry, 0, &eval(), &[]);
        assert!(
            section
                .fields
                .iter()
                .any(|field| matches!(field, PropertyField::Ramp { key, .. } if key == "stops")),
            "field.ramp must offer an editable ramp row: {:?}",
            section.fields
        );
    }

    /// `color.ramp` gets the same gradient row `field.ramp` gets, out of the
    /// template alone — the ramp editor is a `ParameterValue::Ramp` row, not a
    /// per-node arrangement.
    #[test]
    fn the_color_ramp_template_produces_the_same_ramp_row() {
        let registry = registry();
        let node = registry
            .create_node("color.ramp", NodeId::new(1))
            .expect("color.ramp is registered");
        let section = node_params_section(&node, &registry, 0, &eval(), &[]);
        assert!(
            section
                .fields
                .iter()
                .any(|field| matches!(field, PropertyField::Ramp { key, .. } if key == "stops")),
            "color.ramp must offer the same editable ramp row: {:?}",
            section.fields
        );
    }

    #[test]
    fn driven_params_render_read_only_with_source() {
        let node = Node::new(NodeId::new(1), "blur")
            .with_param("radius", ParameterValue::Float(5.0))
            .with_param("other", ParameterValue::Float(2.0));
        let driven = [DrivenParam {
            key: "radius".into(),
            source: "Constant".into(),
            value: Some("12.000".into()),
        }];
        let section = node_params_section(&node, &registry(), 0, &eval(), &driven);
        match &section.fields[0] {
            PropertyField::ReadOnly { key, value } => {
                assert_eq!(key, "radius");
                assert_eq!(value, "12.000 ← Constant");
            }
            other => panic!("expected ReadOnly, got {other:?}"),
        }
        assert!(
            matches!(&section.fields[1], PropertyField::Float { .. }),
            "undriven params stay editable"
        );

        // Unknown source value renders as "connected".
        let driven = [DrivenParam {
            key: "radius".into(),
            source: "Noise".into(),
            value: None,
        }];
        let section = node_params_section(&node, &registry(), 0, &eval(), &driven);
        match &section.fields[0] {
            PropertyField::ReadOnly { value, .. } => assert_eq!(value, "connected ← Noise"),
            other => panic!("expected ReadOnly, got {other:?}"),
        }
    }

    #[test]
    fn channel2_params_map_to_editable_vectors() {
        use ravel_core::animation::channel::AnimationChannel;
        let node = Node::new(NodeId::new(1), "plugin.custom").with_param(
            "center",
            ParameterValue::Channel2([
                AnimationChannel::constant(3.0),
                AnimationChannel::constant(-1.5),
            ]),
        );
        let section = node_params_section(&node, &registry(), 0, &eval(), &[]);
        match &section.fields[0] {
            PropertyField::Vector {
                key, components, ..
            } => {
                assert_eq!(key, "center");
                assert_eq!(components, &[3.0, -1.5]);
            }
            other => panic!("expected Vector, got {other:?}"),
        }
    }

    #[test]
    fn channel3_params_map_to_editable_vectors() {
        use ravel_core::animation::channel::AnimationChannel;
        let node = Node::new(NodeId::new(1), "plugin.custom").with_param(
            "direction",
            ParameterValue::Channel3([
                AnimationChannel::constant(1.0),
                AnimationChannel::constant(2.0),
                AnimationChannel::constant(3.0),
            ]),
        );
        let section = node_params_section(&node, &registry(), 0, &eval(), &[]);
        match &section.fields[0] {
            PropertyField::Vector { components, .. } => {
                assert_eq!(components, &[1.0, 2.0, 3.0]);
            }
            other => panic!("expected Vector, got {other:?}"),
        }
    }

    /// The folded builtin vector parameters reach the Vector row, sharing one
    /// registry range across their components — the pre-fold `_x` / `_y`
    /// Floats produced two independent Float rows instead.
    #[test]
    fn folded_builtin_vector_params_render_as_vector_rows() {
        let registry = registry();
        for (type_key, key, arity) in [
            ("shape.rect", "center", 2),
            ("shape.ellipse", "radius", 2),
            ("scatter.grid", "spacing", 2),
            ("geometry.transform", "translate", 3),
            ("geometry.transform", "rotation", 3),
            ("geometry.transform", "scale", 3),
            ("geometry.transform", "pivot", 3),
            ("transform", "translate", 3),
            ("field.falloff", "direction", 3),
        ] {
            let node = registry
                .create_node(type_key, NodeId::new(1))
                .unwrap_or_else(|| panic!("{type_key}"));
            let fields = node_params_fields(&node, &registry, 0, &eval(), &[]);
            let field = fields
                .iter()
                .find(|field| field.key() == key)
                .unwrap_or_else(|| panic!("{type_key}.{key} has no field"));
            match field {
                PropertyField::Vector {
                    components, range, ..
                } => {
                    assert_eq!(components.len(), arity, "{type_key}.{key}");
                    assert!(range.is_some(), "{type_key}.{key} shares one range");
                }
                other => panic!("{type_key}.{key} is {other:?}, not a Vector row"),
            }
        }
        // No template leaks a per-component row any more.
        for template in registry.all_templates() {
            let node = registry
                .create_node(&template.type_key, NodeId::new(1))
                .unwrap();
            for field in node_params_fields(&node, &registry, 0, &eval(), &[]) {
                assert!(
                    !matches!(
                        field.key(),
                        "center_x" | "center_y" | "translate_x" | "translate_y"
                    ),
                    "{} still shows {}",
                    template.type_key,
                    field.key()
                );
            }
        }
    }

    /// `attribute.set`'s `value` follows its `type`: a vector type renders as
    /// one Vector row, the scalar types as a single Float row.
    #[test]
    fn attribute_set_value_renders_at_the_arity_its_type_selects() {
        let registry = registry();
        let node_for = |type_name: &str| {
            let mut node = registry
                .create_node("attribute.set", NodeId::new(1))
                .unwrap();
            let value = ravel_core::registry::builtin::attribute_set_value_for_type(
                type_name,
                &node
                    .parameters
                    .iter()
                    .find(|p| p.key == "value")
                    .unwrap()
                    .value,
            )
            .unwrap();
            for param in node.parameters.iter_mut() {
                match param.key.as_str() {
                    "type" => param.value = ParameterValue::String(type_name.into()),
                    "value" => param.value = value.clone(),
                    _ => {}
                }
            }
            node
        };
        for (type_name, arity) in [("vec2", 2), ("vec3", 3)] {
            let node = node_for(type_name);
            let fields = node_params_fields(&node, &registry, 0, &eval(), &[]);
            let field = fields
                .iter()
                .find(|field| field.key() == "value")
                .expect("value field");
            match field {
                PropertyField::Vector { components, .. } => {
                    assert_eq!(components.len(), arity, "{type_name}");
                }
                other => panic!("{type_name} is {other:?}, not a Vector row"),
            }
        }
        // `f32` keeps the single editable number it always had.
        let fields = node_params_fields(&node_for("f32"), &registry, 0, &eval(), &[]);
        assert!(matches!(
            fields.iter().find(|f| f.key() == "value"),
            Some(PropertyField::Float { .. })
        ));
        // The type selector is a closed set, so the retype path is reachable
        // from a dropdown rather than free text.
        assert!(matches!(
            fields.iter().find(|f| f.key() == "type"),
            Some(PropertyField::Enum { .. })
        ));
    }

    #[test]
    fn channel4_params_map_to_color_fields() {
        use ravel_core::animation::channel::AnimationChannel;
        let node = Node::new(NodeId::new(1), "constant.color").with_param(
            "color",
            ParameterValue::Channel4([
                AnimationChannel::constant(1.0),
                AnimationChannel::constant(0.5),
                AnimationChannel::constant(0.25),
                AnimationChannel::constant(0.8),
            ]),
        );
        let section = node_params_section(&node, &registry(), 0, &eval(), &[]);
        match &section.fields[0] {
            PropertyField::Color { key, r, g, b, a } => {
                assert_eq!(key, "color");
                assert_eq!((*r, *g, *b, *a), (1.0, 0.5, 0.25, 0.8));
            }
            other => panic!("expected Color, got {other:?}"),
        }
    }

    // ----- ports section ---------------------------------------------------

    use ravel_core::graph::Graph;
    use ravel_core::network::{
        NET_IN_TYPE_KEY, NET_OUT_TYPE_KEY, PORT_BASE_GEOMETRY, PORT_FRAME, PORT_FRAME_INDEX,
        PORT_SOURCE, PORT_TIME, add_custom_port,
    };

    fn in_node_with(context: NetworkContext, ports: &[(&str, CustomPortType)]) -> Node {
        let id = NodeId::new(1);
        let node = Node::new(id, NET_IN_TYPE_KEY)
            .with_output(PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
            .with_output(PORT_TIME, DataTypeId::SCALAR)
            .with_output(PORT_FRAME_INDEX, DataTypeId::SCALAR)
            .with_output(PORT_SOURCE, DataTypeId::FRAME_BUFFER);
        let mut graph = Graph::new().add_node(node).unwrap();
        for (name, port_type) in ports {
            graph = add_custom_port(graph, id, name, *port_type, context).unwrap();
        }
        (**graph.node(id).unwrap()).clone()
    }

    fn port_list(section: &PropertySection) -> (&PortSide, &[PortRow], &[CustomPortType]) {
        match &section.fields[0] {
            PropertyField::PortList {
                side,
                rows,
                options,
                ..
            } => (side, rows, options),
            other => panic!("expected a PortList, got {other:?}"),
        }
    }

    /// Selecting the In node offers a Ports section listing every port it
    /// declares — the shell's fixed ones marked as such, the user's not.
    #[test]
    fn the_in_node_lists_fixed_and_custom_ports_apart() {
        let node = in_node_with(
            NetworkContext::LayerRoot,
            &[("amount", CustomPortType::Float)],
        );
        let sections = sections_for_node(
            &node,
            &registry(),
            0,
            &eval(),
            &[],
            NetworkContext::LayerRoot,
        );
        let section = sections.last().expect("a ports section");
        assert_eq!(section.title, "properties.section.ports");

        let (side, rows, _) = port_list(section);
        assert_eq!(
            *side,
            PortSide::Output,
            "an In node declares its custom ports as outputs"
        );
        assert_eq!(
            rows.iter()
                .map(|row| (row.name.as_str(), row.fixed))
                .collect::<Vec<_>>(),
            vec![
                (PORT_BASE_GEOMETRY, true),
                (PORT_TIME, true),
                (PORT_FRAME_INDEX, true),
                (PORT_SOURCE, true),
                ("amount", false),
            ]
        );
        assert_eq!(rows[0].port_type, Some(CustomPortType::Geometry));
        assert_eq!(rows[4].port_type, Some(CustomPortType::Float));
    }

    /// The Out node's custom ports are inputs, and `frame` is the fixed one.
    #[test]
    fn the_out_node_lists_its_input_ports() {
        let id = NodeId::new(2);
        let node =
            Node::new(id, NET_OUT_TYPE_KEY).with_input(PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let graph = add_custom_port(
            Graph::new().add_node(node).unwrap(),
            id,
            "mask",
            CustomPortType::Geometry,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let node = graph.node(id).unwrap();
        let sections = sections_for_node(
            node,
            &registry(),
            0,
            &eval(),
            &[],
            NetworkContext::LayerRoot,
        );
        let (side, rows, _) = port_list(sections.last().expect("a ports section"));
        assert_eq!(*side, PortSide::Input);
        assert_eq!(
            rows.iter()
                .map(|row| (row.name.as_str(), row.fixed, row.port_type))
                .collect::<Vec<_>>(),
            vec![
                (PORT_FRAME, true, Some(CustomPortType::FrameBuffer)),
                ("mask", false, Some(CustomPortType::Geometry)),
            ]
        );
    }

    /// The type menu is the context's answer, not a fixed list: the shell
    /// supplies values to a layer-root In, a subnet's inner In is a pin
    /// boundary, and an Out node is neither.
    #[test]
    fn the_type_menu_follows_the_network_context() {
        let node = in_node_with(NetworkContext::LayerRoot, &[]);
        for context in [NetworkContext::LayerRoot, NetworkContext::Subnet] {
            let sections = sections_for_node(&node, &registry(), 0, &eval(), &[], context);
            let (_, _, options) = port_list(sections.last().expect("a ports section"));
            assert_eq!(
                options,
                CustomPortType::allowed_for_in(context),
                "{context:?}"
            );
        }
        assert!(
            !CustomPortType::allowed_for_in(NetworkContext::LayerRoot)
                .contains(&CustomPortType::Geometry),
            "the layer root has no wire-only types to offer"
        );

        let out = Node::new(NodeId::new(2), NET_OUT_TYPE_KEY)
            .with_input(PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let sections = sections_for_node(
            &out,
            &registry(),
            0,
            &eval(),
            &[],
            NetworkContext::LayerRoot,
        );
        let (_, _, options) = port_list(sections.last().expect("a ports section"));
        assert_eq!(options, CustomPortType::allowed_for_out());
    }

    /// Only the interface nodes have an interface to edit.
    #[test]
    fn an_ordinary_node_has_no_ports_section() {
        let node = Node::new(NodeId::new(1), "blur")
            .with_input("input", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER);
        assert!(node_ports_section(&node, NetworkContext::LayerRoot).is_none());
        assert!(
            sections_for_node(
                &node,
                &registry(),
                0,
                &eval(),
                &[],
                NetworkContext::LayerRoot
            )
            .iter()
            .all(|section| section.title != "properties.section.ports")
        );
    }

    /// A legacy custom `f` (an `f` output carrying a same-named parameter) is
    /// the user's port, so the row is editable even though the name is a
    /// built-in one.
    #[test]
    fn a_legacy_custom_frame_index_row_is_not_fixed() {
        let node = Node::new(NodeId::new(1), NET_IN_TYPE_KEY)
            .with_output(PORT_TIME, DataTypeId::SCALAR)
            .with_output(PORT_FRAME_INDEX, DataTypeId::SCALAR)
            .with_param(PORT_FRAME_INDEX, ParameterValue::Int(3));
        let section = node_ports_section(&node, NetworkContext::LayerRoot).expect("ports section");
        let (_, rows, _) = port_list(&section);
        assert_eq!(
            rows.iter()
                .map(|row| (row.name.as_str(), row.fixed, row.port_type))
                .collect::<Vec<_>>(),
            vec![
                (PORT_TIME, true, Some(CustomPortType::Float)),
                (PORT_FRAME_INDEX, false, Some(CustomPortType::Int)),
            ]
        );
    }

    /// Animated channels display the value at the given frame, not frame 0
    /// (the panel passes the playhead's layer-local frame, REQ-LAYER-004).
    #[test]
    fn channel_params_display_the_value_at_the_given_frame() {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 100.0, Interpolation::Linear);
        let node = Node::new(NodeId::new(1), "blur").with_param(
            "radius",
            ParameterValue::Channel(AnimationChannel::keyframes(curve)),
        );
        let section = node_params_section(&node, &registry(), 5, &eval(), &[]);
        match &section.fields[0] {
            PropertyField::Float { value, .. } => {
                assert!((*value - 50.0).abs() < 1e-3, "sampled at frame 5");
            }
            _ => panic!("expected Float"),
        }
    }
    // ----- parameter groups (parameter-groups-plan, PGRP-1) ----------------

    /// A registry holding one template that declares groups over the six
    /// parameters `grouped_node` creates.
    fn grouped_registry(groups: &[(&str, &[&str])]) -> NodeRegistry {
        use ravel_core::registry::{NodeCategory, NodeTemplate};
        let mut template = NodeTemplate::new("test.grouped", "Grouped", NodeCategory::Utility);
        for key in ["a", "b", "c", "d", "e", "f"] {
            template = template.with_param(ravel_core::graph::Parameter {
                key: key.into(),
                value: ParameterValue::Float(0.0),
            });
        }
        for (name, keys) in groups {
            template = template.with_param_group(*name, keys.iter().copied());
        }
        let mut registry = NodeRegistry::new();
        registry.register(template);
        registry
    }

    fn grouped_node(registry: &NodeRegistry) -> Node {
        registry
            .create_node("test.grouped", NodeId::new(1))
            .expect("test.grouped is registered")
    }

    /// The section titles and the field keys under each, which is the whole
    /// observable output of the split.
    fn split(node: &Node, registry: &NodeRegistry) -> Vec<(String, Vec<String>)> {
        node_params_sections(node, registry, 0, &eval(), &[])
            .into_iter()
            .map(|section| {
                (
                    section.title,
                    section
                        .fields
                        .iter()
                        .map(|field| field.key().to_string())
                        .collect(),
                )
            })
            .collect()
    }

    /// A type that declares no group keeps the single section it always had,
    /// holding every parameter in declaration order.
    #[test]
    fn a_type_without_group_declarations_keeps_one_parameters_section() {
        let registry = grouped_registry(&[]);
        assert_eq!(
            split(&grouped_node(&registry), &registry),
            vec![(
                "properties.section.parameters".to_string(),
                vec!["a", "b", "c", "d", "e", "f"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>()
            )]
        );
    }

    /// Declared groups become sections in declaration order, each holding the
    /// keys it names in the order it names them, and each titled with its
    /// `node.<type_key>.group.<name>` locale key.
    #[test]
    fn declared_groups_split_into_sections_in_declaration_order() {
        let registry = grouped_registry(&[
            ("shape", &["c", "a"]),
            ("paint", &["b", "d"]),
            ("output", &["f", "e"]),
        ]);
        assert_eq!(
            split(&grouped_node(&registry), &registry),
            vec![
                (
                    "node.test.grouped.group.shape".to_string(),
                    vec!["c".to_string(), "a".to_string()]
                ),
                (
                    "node.test.grouped.group.paint".to_string(),
                    vec!["b".to_string(), "d".to_string()]
                ),
                (
                    "node.test.grouped.group.output".to_string(),
                    vec!["f".to_string(), "e".to_string()]
                ),
            ]
        );
    }

    /// A parameter no declaration names lands in the implicit section, which
    /// comes **first** so a partially grouped type still opens on the rows it
    /// always opened on.
    #[test]
    fn a_key_no_group_names_falls_into_the_leading_implicit_section() {
        let registry = grouped_registry(&[("shape", &["c", "d"])]);
        assert_eq!(
            split(&grouped_node(&registry), &registry),
            vec![
                (
                    "properties.section.parameters".to_string(),
                    vec!["a", "b", "e", "f"]
                        .into_iter()
                        .map(String::from)
                        .collect::<Vec<_>>()
                ),
                (
                    "node.test.grouped.group.shape".to_string(),
                    vec!["c".to_string(), "d".to_string()]
                ),
            ]
        );
    }

    /// A declaration naming a key the type does not have drops that key, and
    /// a group left with nothing is not rendered at all: a typo in a template
    /// must not cost the panel a row or add an empty heading.
    #[test]
    fn a_group_naming_a_missing_parameter_drops_it_and_an_empty_group() {
        let registry = grouped_registry(&[("shape", &["a", "typo"]), ("ghost", &["nope"])]);
        assert_eq!(
            split(&grouped_node(&registry), &registry),
            vec![
                (
                    "properties.section.parameters".to_string(),
                    vec!["b", "c", "d", "e", "f"]
                        .into_iter()
                        .map(String::from)
                        .collect::<Vec<_>>()
                ),
                (
                    "node.test.grouped.group.shape".to_string(),
                    vec!["a".to_string()]
                ),
            ]
        );
    }

    /// A key named by two groups belongs to the first that names it — one
    /// parameter is one row, and the alternative (rendering it twice) would
    /// give the same parameter two widgets writing the same value.
    #[test]
    fn a_key_named_by_two_groups_belongs_to_the_first() {
        let registry = grouped_registry(&[("shape", &["a", "b"]), ("paint", &["b", "c"])]);
        assert_eq!(
            split(&grouped_node(&registry), &registry),
            vec![
                (
                    "properties.section.parameters".to_string(),
                    vec!["d", "e", "f"]
                        .into_iter()
                        .map(String::from)
                        .collect::<Vec<_>>()
                ),
                (
                    "node.test.grouped.group.shape".to_string(),
                    vec!["a".to_string(), "b".to_string()]
                ),
                (
                    "node.test.grouped.group.paint".to_string(),
                    vec!["c".to_string()]
                ),
            ]
        );
    }

    /// One group name is one section even when declared twice: the name is
    /// the collapse identity, so two sections under it could not be told
    /// apart.
    #[test]
    fn a_group_name_declared_twice_is_one_section() {
        let registry = grouped_registry(&[("shape", &["a"]), ("paint", &["b"]), ("shape", &["c"])]);
        assert_eq!(
            split(&grouped_node(&registry), &registry),
            vec![
                (
                    "properties.section.parameters".to_string(),
                    vec!["d", "e", "f"]
                        .into_iter()
                        .map(String::from)
                        .collect::<Vec<_>>()
                ),
                (
                    "node.test.grouped.group.shape".to_string(),
                    vec!["a".to_string(), "c".to_string()]
                ),
                (
                    "node.test.grouped.group.paint".to_string(),
                    vec!["b".to_string()]
                ),
            ]
        );
    }

    /// The collapse identities the host keys on, paired with what it shows.
    #[test]
    fn param_group_titles_pair_the_identity_with_the_heading() {
        let registry = grouped_registry(&[("shape", &["a"])]);
        assert_eq!(
            param_group_titles(&grouped_node(&registry), &registry),
            vec![
                (String::new(), "properties.section.parameters".to_string()),
                (
                    "shape".to_string(),
                    "node.test.grouped.group.shape".to_string()
                ),
            ]
        );
    }

    /// Every section the split produces reaches [`sections_for_node`], between
    /// the info section and the ports section.
    #[test]
    fn sections_for_node_carries_every_parameter_group() {
        let registry = grouped_registry(&[("shape", &["a"]), ("paint", &["b"])]);
        let titles: Vec<String> = sections_for_node(
            &grouped_node(&registry),
            &registry,
            0,
            &eval(),
            &[],
            NetworkContext::LayerRoot,
        )
        .into_iter()
        .map(|section| section.title)
        .collect();
        assert_eq!(
            titles,
            vec![
                "properties.section.node_info",
                "properties.section.parameters",
                "node.test.grouped.group.shape",
                "node.test.grouped.group.paint",
            ]
        );
    }
    // ----- instance parameter groups (parameter-groups-plan, PGRP-4) -------

    /// The In node from [`in_node_with`], with `assign` applied to its
    /// `param_groups`.
    fn in_node_grouped(
        context: NetworkContext,
        ports: &[(&str, CustomPortType)],
        assign: &[(&str, &str)],
    ) -> Node {
        let mut node = in_node_with(context, ports);
        for (key, group) in assign {
            node = ravel_core::network::set_custom_port_group(
                Graph::new().add_node(node).unwrap(),
                NodeId::new(1),
                key,
                group,
            )
            .map(|graph| (**graph.node(NodeId::new(1)).unwrap()).clone())
            .expect("the parameter exists");
        }
        node
    }

    /// An In node's custom parameters split by the groups the user assigned,
    /// in the order the parameters appear (the order the ports were added).
    /// A parameter with no assignment stays in the leading section.
    #[test]
    fn an_in_node_instance_group_splits_its_parameters() {
        let node = in_node_grouped(
            NetworkContext::LayerRoot,
            &[
                ("width", CustomPortType::Float),
                ("height", CustomPortType::Float),
                ("seed", CustomPortType::Int),
            ],
            &[("width", "Size"), ("height", "Size")],
        );
        assert_eq!(
            split(&node, &registry()),
            vec![
                (
                    "properties.section.parameters".to_string(),
                    vec!["seed".to_string()]
                ),
                (
                    "Size".to_string(),
                    vec!["width".to_string(), "height".to_string()]
                ),
            ]
        );
    }

    /// The group name is the user's own text, so it is the section title as
    /// typed — never run through a locale key.
    #[test]
    fn an_instance_group_title_is_the_users_own_text() {
        let node = in_node_grouped(
            NetworkContext::LayerRoot,
            &[("width", CustomPortType::Float)],
            &[("width", "ばね")],
        );
        assert_eq!(
            param_group_titles(&node, &registry()),
            vec![("ばね".to_string(), "ばね".to_string())]
        );
    }

    /// When the same In node has both an instance assignment and a type
    /// declaration, the instance wins outright: the user made that assignment
    /// by hand, and honouring half of each would leave it unclear which one a
    /// parameter answers to.
    #[test]
    fn an_instance_group_wins_over_the_type_declaration() {
        use ravel_core::registry::{NodeCategory, NodeTemplate};
        let node = in_node_grouped(
            NetworkContext::LayerRoot,
            &[
                ("width", CustomPortType::Float),
                ("height", CustomPortType::Float),
            ],
            &[("width", "Size")],
        );
        let mut registry = NodeRegistry::new();
        registry.register(
            NodeTemplate::new(NET_IN_TYPE_KEY, "In", NodeCategory::Utility)
                .with_param_group("declared", ["width", "height"]),
        );
        assert_eq!(
            split(&node, &registry),
            vec![
                (
                    "properties.section.parameters".to_string(),
                    vec!["height".to_string()]
                ),
                ("Size".to_string(), vec!["width".to_string()]),
            ],
            "the type's `declared` group is not consulted"
        );

        // Clearing the assignment hands the node back to its type.
        let bare = in_node_with(
            NetworkContext::LayerRoot,
            &[
                ("width", CustomPortType::Float),
                ("height", CustomPortType::Float),
            ],
        );
        assert_eq!(
            split(&bare, &registry),
            vec![(
                "node.net.in.group.declared".to_string(),
                vec!["width".to_string(), "height".to_string()]
            )]
        );
    }

    /// Only an In node reads its own `param_groups`. Any other node carrying
    /// them — which only a hand-edited `.ravprj` can produce, since nothing
    /// writes there — falls back to its type's declaration rather than being
    /// refused. Refusing to open would trade a cosmetic oddity for a project
    /// that saved and will not load.
    #[test]
    fn instance_groups_on_a_non_in_node_fall_back_to_the_type_declaration() {
        let registry = grouped_registry(&[("shape", &["a", "b"])]);
        let mut node = grouped_node(&registry);
        node.param_groups
            .insert("a".to_string(), "Hand edited".to_string());
        assert_eq!(
            split(&node, &registry),
            vec![
                (
                    "properties.section.parameters".to_string(),
                    vec!["c", "d", "e", "f"]
                        .into_iter()
                        .map(String::from)
                        .collect::<Vec<_>>()
                ),
                (
                    "node.test.grouped.group.shape".to_string(),
                    vec!["a".to_string(), "b".to_string()]
                ),
            ]
        );
    }

    /// The Ports section carries each port's group so the row can edit it —
    /// and `None` for a port with no parameter, which has nothing to group
    /// (every fixed In port, and every port of an Out node).
    #[test]
    fn the_ports_section_carries_the_group_of_each_parameter_carrying_port() {
        // A wire-only custom type is only offered inside a subnet, so this
        // covers both halves in the one context that can hold them.
        let node = in_node_grouped(
            NetworkContext::Subnet,
            &[
                ("width", CustomPortType::Float),
                ("shape", CustomPortType::Geometry),
            ],
            &[("width", "Size")],
        );
        let section = node_ports_section(&node, NetworkContext::Subnet).expect("in node");
        let (_, rows, _) = port_list(&section);
        assert_eq!(
            rows.iter()
                .map(|row| (row.name.as_str(), row.group.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (PORT_BASE_GEOMETRY, None),
                (PORT_TIME, None),
                (PORT_FRAME_INDEX, None),
                (PORT_SOURCE, None),
                // Assigned by the user.
                ("width", Some("Size")),
                // A Geometry port is wire-only: no parameter, no group cell.
                ("shape", None),
            ]
        );

        let out = Node::new(NodeId::new(2), NET_OUT_TYPE_KEY)
            .with_input(PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let section = node_ports_section(&out, NetworkContext::LayerRoot).expect("out node");
        let (_, rows, _) = port_list(&section);
        assert!(rows.iter().all(|row| row.group.is_none()));
    }
}
