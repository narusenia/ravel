// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property sections for graph nodes.

use ravel_core::graph::{Node, ParameterValue};

use super::{PropertyField, PropertySection};

/// Build an info section with read-only node metadata.
pub fn node_info_section(node: &Node) -> PropertySection {
    let label = node
        .metadata
        .label
        .clone()
        .unwrap_or_else(|| node.type_key.clone());

    PropertySection {
        title: "Node Info".into(),
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
/// variant. The `operation` parameter on merge nodes is treated as an `Enum`
/// with known options.
pub fn node_params_section(node: &Node) -> PropertySection {
    let fields = node
        .parameters
        .iter()
        .map(|p| match &p.value {
            ParameterValue::Float(v) => PropertyField::Float {
                key: p.key.clone(),
                value: *v,
                range: None,
                step: Some(0.01),
            },
            ParameterValue::Int(v) => PropertyField::Int {
                key: p.key.clone(),
                value: *v,
                range: None,
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
        })
        .collect();

    PropertySection {
        title: "Parameters".into(),
        fields,
    }
}

/// Build all sections for a single node.
pub fn sections_for_node(node: &Node) -> Vec<PropertySection> {
    let mut sections = vec![node_info_section(node)];
    if !node.parameters.is_empty() {
        sections.push(node_params_section(node));
    }
    sections
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::id::{DataTypeId, NodeId};

    #[test]
    fn info_section_shows_type_and_label() {
        let node = Node::new(NodeId::new(1), "blur")
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_label("My Blur");
        let section = node_info_section(&node);
        assert_eq!(section.title, "Node Info");
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
        let section = node_params_section(&node);
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
        let section = node_params_section(&node);
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
        let sections = sections_for_node(&node);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].title, "Node Info");
        assert_eq!(sections[1].title, "Parameters");
    }

    #[test]
    fn sections_for_node_without_params() {
        let node = Node::new(NodeId::new(1), "passthrough");
        let sections = sections_for_node(&node);
        assert_eq!(sections.len(), 1);
    }
}
