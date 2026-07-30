// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property sections for graph nodes.

use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::registry::NodeRegistry;

use super::{DrivenParam, PropertyField, PropertySection};

/// Display value for an animated channel at `frame` (the owning layer's
/// local frame, REQ-LAYER-004/006): the constant value, the curve's sample
/// at `frame`, or 0 for not-yet-resolvable sources (expression, node
/// output, audio).
fn channel_display_value(ch: &AnimationChannel, frame: u64) -> f32 {
    match &ch.source {
        ChannelSource::Constant(v) => *v,
        ChannelSource::Keyframes(curve) => curve.sample(frame as f64),
        _ => 0.0,
    }
}

/// Build an info section with read-only node metadata.
pub fn node_info_section(node: &Node) -> PropertySection {
    let label = node
        .metadata
        .label
        .clone()
        .unwrap_or_else(|| node.type_key.clone());

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
                    value: channel_display_value(ch, frame),
                    range: ranges.map(|r| r.hard.clone()),
                    ui_range: ranges.map(|r| r.ui.clone()),
                    step: Some(0.01),
                },
                ParameterValue::Channel2(chs) => PropertyField::Vector {
                    key: p.key.clone(),
                    components: chs
                        .iter()
                        .map(|ch| channel_display_value(ch, frame))
                        .collect(),
                    range: ranges.map(|r| r.hard.clone()),
                    ui_range: ranges.map(|r| r.ui.clone()),
                    step: Some(0.01),
                },
                ParameterValue::Channel3(chs) => PropertyField::Vector {
                    key: p.key.clone(),
                    components: chs
                        .iter()
                        .map(|ch| channel_display_value(ch, frame))
                        .collect(),
                    range: ranges.map(|r| r.hard.clone()),
                    ui_range: ranges.map(|r| r.ui.clone()),
                    step: Some(0.01),
                },
                ParameterValue::Channel4(chs) => PropertyField::Color {
                    key: p.key.clone(),
                    r: channel_display_value(&chs[0], frame),
                    g: channel_display_value(&chs[1], frame),
                    b: channel_display_value(&chs[2], frame),
                    a: channel_display_value(&chs[3], frame),
                },
                // Path control points are edited on the canvas (pen tool);
                // Properties shows a read-only summary (REQ-UI-011).
                ParameterValue::PathPoints(points) => PropertyField::ReadOnly {
                    key: p.key.clone(),
                    value: format!("{} points", points.len()),
                },
                // Curves get an inline editor of their own (the properties
                // parameter-editor plan, unit 2). Until then Properties shows
                // a read-only summary rather than an interim text field that
                // the editor would immediately replace.
                ParameterValue::Curve(curve) => PropertyField::ReadOnly {
                    key: p.key.clone(),
                    value: format!("{} points", curve.len()),
                },
            }
        })
        .collect();

    PropertySection {
        title: "properties.section.parameters".into(),
        fields,
    }
}

/// Build all sections for a single node, sampling animated channels at
/// `frame` (the owning layer's local frame).
pub fn sections_for_node(
    node: &Node,
    registry: &NodeRegistry,
    frame: u64,
    driven: &[DrivenParam],
) -> Vec<PropertySection> {
    let mut sections = vec![node_info_section(node)];
    if !node.parameters.is_empty() {
        sections.push(node_params_section(node, registry, frame, driven));
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::registry::builtin::register_builtins;

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
        let section = node_info_section(&node);
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

    #[test]
    fn params_section_maps_float() {
        let node =
            Node::new(NodeId::new(1), "blur").with_param("radius", ParameterValue::Float(5.0));
        let section = node_params_section(&node, &registry(), 0, &[]);
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
        let section = node_params_section(&node, &registry(), 0, &[]);
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
        let sections = sections_for_node(&node, &registry(), 0, &[]);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "properties.section.node_info");
        assert_eq!(sections[1].title, "properties.section.parameters");
    }

    #[test]
    fn sections_for_node_without_params() {
        let node = Node::new(NodeId::new(1), "passthrough");
        let sections = sections_for_node(&node, &registry(), 0, &[]);
        assert_eq!(sections.len(), 1);
    }

    #[test]
    fn params_section_picks_up_registry_ranges() {
        let node =
            Node::new(NodeId::new(1), "blur").with_param("radius", ParameterValue::Float(5.0));
        let section = node_params_section(&node, &registry(), 0, &[]);
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
        let section = node_params_section(&node, &registry(), 0, &[]);
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
        let section = node_params_section(&node, &registry(), 0, &[]);
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

    /// Curve parameters show a read-only control-point count until the
    /// inline curve editor lands.
    #[test]
    fn params_section_summarises_curves_read_only() {
        use ravel_core::param_curve::CurveParam;
        let node = Node::new(NodeId::new(1), "field.curve_remap").with_param(
            "points",
            ParameterValue::Curve(CurveParam::linear([(0.0, 0.0), (0.5, 0.8), (1.0, 1.0)])),
        );
        let section = node_params_section(&node, &registry(), 0, &[]);
        match &section.fields[0] {
            PropertyField::ReadOnly { key, value } => {
                assert_eq!(key, "points");
                assert_eq!(value, "3 points");
            }
            other => panic!("expected ReadOnly, got {other:?}"),
        }
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
        let section = node_params_section(&node, &registry(), 0, &driven);
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
        let section = node_params_section(&node, &registry(), 0, &driven);
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
        let section = node_params_section(&node, &registry(), 0, &[]);
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
        let section = node_params_section(&node, &registry(), 0, &[]);
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
            let section = node_params_section(&node, &registry, 0, &[]);
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
            for field in node_params_section(&node, &registry, 0, &[]).fields {
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
            let section = node_params_section(&node, &registry, 0, &[]);
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
        let section = node_params_section(&node_for("f32"), &registry, 0, &[]);
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
        let section = node_params_section(&node, &registry(), 0, &[]);
        match &section.fields[0] {
            PropertyField::Color { key, r, g, b, a } => {
                assert_eq!(key, "color");
                assert_eq!((*r, *g, *b, *a), (1.0, 0.5, 0.25, 0.8));
            }
            other => panic!("expected Color, got {other:?}"),
        }
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
        let section = node_params_section(&node, &registry(), 5, &[]);
        match &section.fields[0] {
            PropertyField::Float { value, .. } => {
                assert!((*value - 50.0).abs() < 1e-3, "sampled at frame 5");
            }
            _ => panic!("expected Float"),
        }
    }
}
