// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property sections for graph nodes.

use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::registry::NodeRegistry;

use super::{PropertyField, PropertySection};

/// Display value for an animated channel without an evaluation context:
/// the constant value, the curve's frame-0 sample, or 0 for
/// not-yet-resolvable sources (expression, node output, audio).
fn channel_display_value(ch: &AnimationChannel) -> f32 {
    match &ch.source {
        ChannelSource::Constant(v) => *v,
        ChannelSource::Keyframes(curve) => curve.sample(0),
        _ => 0.0,
    }
}

/// Read-only component list for vector/color channels (vec editing UI is a
/// later milestone).
fn channel_components_display(chs: &[AnimationChannel]) -> String {
    let parts: Vec<String> = chs
        .iter()
        .map(|ch| format!("{:.3}", channel_display_value(ch)))
        .collect();
    format!("[{}]", parts.join(", "))
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

/// Build a parameters section from the node's parameter list.
///
/// Each `ParameterValue` variant maps to the corresponding `PropertyField`
/// variant. Numeric fields pick up hard/UI ranges from the node's registry
/// template when one is declared. The `operation` parameter on merge nodes
/// is treated as an `Enum` with known options.
pub fn node_params_section(node: &Node, registry: &NodeRegistry) -> PropertySection {
    let fields = node
        .parameters
        .iter()
        .map(|p| {
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
                    if p.key == "operation" {
                        PropertyField::Enum {
                            key: p.key.clone(),
                            value: v.clone(),
                            options: vec!["over".into(), "add".into(), "multiply".into()],
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
                    value: channel_display_value(ch),
                    range: ranges.map(|r| r.hard.clone()),
                    ui_range: ranges.map(|r| r.ui.clone()),
                    step: Some(0.01),
                },
                ParameterValue::Channel2(chs) => PropertyField::ReadOnly {
                    key: p.key.clone(),
                    value: channel_components_display(chs),
                },
                ParameterValue::Channel3(chs) => PropertyField::ReadOnly {
                    key: p.key.clone(),
                    value: channel_components_display(chs),
                },
                ParameterValue::Channel4(chs) => PropertyField::ReadOnly {
                    key: p.key.clone(),
                    value: channel_components_display(chs),
                },
            }
        })
        .collect();

    PropertySection {
        title: "properties.section.parameters".into(),
        fields,
    }
}

/// Build all sections for a single node.
pub fn sections_for_node(node: &Node, registry: &NodeRegistry) -> Vec<PropertySection> {
    let mut sections = vec![node_info_section(node)];
    if !node.parameters.is_empty() {
        sections.push(node_params_section(node, registry));
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
        let section = node_params_section(&node, &registry());
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
        let section = node_params_section(&node, &registry());
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
        let sections = sections_for_node(&node, &registry());
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "properties.section.node_info");
        assert_eq!(sections[1].title, "properties.section.parameters");
    }

    #[test]
    fn sections_for_node_without_params() {
        let node = Node::new(NodeId::new(1), "passthrough");
        let sections = sections_for_node(&node, &registry());
        assert_eq!(sections.len(), 1);
    }

    #[test]
    fn params_section_picks_up_registry_ranges() {
        let node =
            Node::new(NodeId::new(1), "blur").with_param("radius", ParameterValue::Float(5.0));
        let section = node_params_section(&node, &registry());
        match &section.fields[0] {
            PropertyField::Float {
                range, ui_range, ..
            } => {
                assert_eq!(range.clone().unwrap(), 0.0..=500.0);
                assert_eq!(ui_range.clone().unwrap(), 0.0..=50.0);
            }
            _ => panic!("expected Float"),
        }
    }

    #[test]
    fn int_params_cast_registry_ranges() {
        let node =
            Node::new(NodeId::new(1), "shape.polygon").with_param("sides", ParameterValue::Int(6));
        let section = node_params_section(&node, &registry());
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
        let section = node_params_section(&node, &registry());
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
}
