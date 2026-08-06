// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property sections for graph nodes.

use ravel_core::animation::channel::AnimationChannel;
use ravel_core::eval::EvalContext;
use ravel_core::graph::{Node, ParameterValue, PortSide};
use ravel_core::network::{
    CustomPortType, NetworkContext, custom_port_type, is_fixed_port, is_in_node, is_out_node,
};
use ravel_core::registry::NodeRegistry;

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

/// Build a parameters section from the node's parameter list, sampling
/// animated channels at `frame` (the owning layer's local frame).
///
/// Each `ParameterValue` variant maps to the corresponding `PropertyField`
/// variant. Numeric fields pick up hard/UI ranges from the node's registry
/// template when one is declared. String parameters with a registry-declared
/// option set (e.g. merge `operation`, math `op`) render as an `Enum`.
pub fn node_params_section(
    node: &Node,
    registry: &NodeRegistry,
    frame: u64,
    eval: &EvalContext,
    driven: &[DrivenParam],
) -> PropertySection {
    let fields = node
        .parameters
        .iter()
        .map(|p| {
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
                ParameterValue::Int(v) => PropertyField::Int {
                    key: p.key.clone(),
                    value: *v,
                    range: ranges.map(|r| (*r.hard.start() as i32)..=(*r.hard.end() as i32)),
                    ui_range: ranges.map(|r| (*r.ui.start() as i32)..=(*r.ui.end() as i32)),
                    step: Some(1),
                },
                ParameterValue::Bool(v) => PropertyField::Bool {
                    key: p.key.clone(),
                    value: *v,
                },
                ParameterValue::String(v) => {
                    // A registry-declared closed option set renders as an
                    // enum dropdown; free-form strings stay editable text.
                    if let Some(options) = registry.param_options(&node.type_key, &p.key) {
                        PropertyField::Enum {
                            key: p.key.clone(),
                            value: v.clone(),
                            options: options.to_vec(),
                        }
                    } else {
                        PropertyField::String {
                            key: p.key.clone(),
                            value: v.clone(),
                        }
                    }
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
            }
        })
        .collect();

    PropertySection {
        title: "properties.section.parameters".into(),
        fields,
    }
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
    if !node.parameters.is_empty() {
        sections.push(node_params_section(node, registry, frame, eval, driven));
    }
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
            let section = node_params_section(&node, &registry, 0, &eval(), &[]);
            let field = section
                .fields
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
            for field in node_params_section(&node, &registry, 0, &eval(), &[]).fields {
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
            let section = node_params_section(&node, &registry, 0, &eval(), &[]);
            let field = section
                .fields
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
        let section = node_params_section(&node_for("f32"), &registry, 0, &eval(), &[]);
        assert!(matches!(
            section.fields.iter().find(|f| f.key() == "value"),
            Some(PropertyField::Float { .. })
        ));
        // The type selector is a closed set, so the retype path is reachable
        // from a dropdown rather than free text.
        assert!(matches!(
            section.fields.iter().find(|f| f.key() == "type"),
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
}
