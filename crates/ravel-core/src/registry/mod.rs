// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node template registry for built-in and user-defined node types.

pub mod builtin;

use crate::graph::{InputPort, Node, OutputPort, Parameter};
use crate::id::NodeId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::RangeInclusive;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeCategory {
    Generator,
    Compositor,
    Filter,
    Transform,
    Color,
    Time,
    Utility,
}

/// Editing range metadata for a numeric parameter.
///
/// `hard` is the true clamp boundary — a value never leaves it. `ui` is the
/// comfortable editing span widgets present by default (slider bounds, scrub
/// sensitivity); it must be contained in `hard`. Int parameters share the
/// same f32-based ranges and cast at the edges.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamRange {
    pub hard: RangeInclusive<f32>,
    pub ui: RangeInclusive<f32>,
}

impl ParamRange {
    pub fn new(hard: RangeInclusive<f32>, ui: RangeInclusive<f32>) -> Self {
        debug_assert!(
            hard.start() <= ui.start() && ui.end() <= hard.end(),
            "ui range {ui:?} must be contained in hard range {hard:?}"
        );
        Self { hard, ui }
    }

    /// Clamps a value to the hard boundary.
    pub fn clamp(&self, value: f32) -> f32 {
        value.clamp(*self.hard.start(), *self.hard.end())
    }
}

#[derive(Clone, Debug)]
pub struct NodeTemplate {
    pub type_key: String,
    pub label: String,
    pub category: NodeCategory,
    pub inputs: Vec<InputPort>,
    pub outputs: Vec<OutputPort>,
    pub default_params: Vec<Parameter>,
    pub param_ranges: HashMap<String, ParamRange>,
    /// Closed option sets for string parameters (rendered as enum
    /// dropdowns instead of free-text fields).
    pub param_options: HashMap<String, Vec<String>>,
}

impl NodeTemplate {
    pub fn new(
        type_key: impl Into<String>,
        label: impl Into<String>,
        category: NodeCategory,
    ) -> Self {
        Self {
            type_key: type_key.into(),
            label: label.into(),
            category,
            inputs: Vec::new(),
            outputs: Vec::new(),
            default_params: Vec::new(),
            param_ranges: HashMap::new(),
            param_options: HashMap::new(),
        }
    }

    pub fn with_input(mut self, port: InputPort) -> Self {
        self.inputs.push(port);
        self
    }

    pub fn with_output(mut self, port: OutputPort) -> Self {
        self.outputs.push(port);
        self
    }

    pub fn with_param(mut self, param: Parameter) -> Self {
        self.default_params.push(param);
        self
    }

    /// Attaches hard/UI editing ranges to a numeric parameter.
    pub fn with_param_range(
        mut self,
        key: impl Into<String>,
        hard: RangeInclusive<f32>,
        ui: RangeInclusive<f32>,
    ) -> Self {
        self.param_ranges
            .insert(key.into(), ParamRange::new(hard, ui));
        self
    }

    pub fn param_range(&self, key: &str) -> Option<&ParamRange> {
        self.param_ranges.get(key)
    }

    /// Declares the closed option set of a string parameter.
    pub fn with_param_options<S: Into<String>>(
        mut self,
        key: impl Into<String>,
        options: impl IntoIterator<Item = S>,
    ) -> Self {
        self.param_options
            .insert(key.into(), options.into_iter().map(Into::into).collect());
        self
    }

    pub fn param_option_values(&self, key: &str) -> Option<&[String]> {
        self.param_options.get(key).map(|v| v.as_slice())
    }

    pub fn create_node(&self, id: NodeId) -> Node {
        let mut node = Node::new(id, &self.type_key);
        node.inputs = self.inputs.clone();
        node.outputs = self.outputs.clone();
        node.parameters = self.default_params.clone();
        if let Some(label) = Some(&self.label) {
            node.metadata.label = Some(label.clone());
        }
        node
    }
}

#[derive(Debug, Default)]
pub struct NodeRegistry {
    templates: HashMap<String, NodeTemplate>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, template: NodeTemplate) {
        self.templates.insert(template.type_key.clone(), template);
    }

    pub fn get(&self, type_key: &str) -> Option<&NodeTemplate> {
        self.templates.get(type_key)
    }

    pub fn create_node(&self, type_key: &str, id: NodeId) -> Option<Node> {
        self.templates.get(type_key).map(|t| t.create_node(id))
    }

    pub fn list_by_category(&self, category: NodeCategory) -> Vec<&NodeTemplate> {
        self.templates
            .values()
            .filter(|t| t.category == category)
            .collect()
    }

    pub fn all_templates(&self) -> impl Iterator<Item = &NodeTemplate> {
        self.templates.values()
    }

    /// Range metadata for `param_key` on `type_key`, if declared.
    pub fn param_range(&self, type_key: &str, param_key: &str) -> Option<&ParamRange> {
        self.templates.get(type_key)?.param_range(param_key)
    }

    /// Closed option set for a string parameter, if declared.
    pub fn param_options(&self, type_key: &str, param_key: &str) -> Option<&[String]> {
        self.templates.get(type_key)?.param_option_values(param_key)
    }

    pub fn categories(&self) -> Vec<NodeCategory> {
        let mut cats: Vec<_> = self
            .templates
            .values()
            .map(|t| t.category)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        cats.sort_by_key(|c| *c as u8);
        cats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::DataTypeId;

    fn make_template() -> NodeTemplate {
        NodeTemplate::new("blur", "Gaussian Blur", NodeCategory::Filter)
            .with_input(InputPort {
                name: "image".into(),
                accepted_types: vec![DataTypeId::FRAME_BUFFER],
                is_param: false,
            })
            .with_input(InputPort {
                name: "radius".into(),
                accepted_types: vec![DataTypeId::SCALAR],
                is_param: false,
            })
            .with_output(OutputPort {
                name: "output".into(),
                data_type: DataTypeId::FRAME_BUFFER,
            })
    }

    #[test]
    fn register_and_lookup() {
        let mut reg = NodeRegistry::new();
        reg.register(make_template());
        assert!(reg.get("blur").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn create_node_from_template() {
        let mut reg = NodeRegistry::new();
        reg.register(make_template());
        let node = reg.create_node("blur", NodeId::new(1)).unwrap();
        assert_eq!(node.type_key, "blur");
        assert_eq!(node.inputs.len(), 2);
        assert_eq!(node.outputs.len(), 1);
        assert_eq!(node.metadata.label.as_deref(), Some("Gaussian Blur"));
    }

    #[test]
    fn list_by_category() {
        let mut reg = NodeRegistry::new();
        reg.register(make_template());
        reg.register(NodeTemplate::new(
            "constant",
            "Constant",
            NodeCategory::Generator,
        ));
        let filters = reg.list_by_category(NodeCategory::Filter);
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].type_key, "blur");
    }
}
