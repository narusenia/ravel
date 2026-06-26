// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Built-in node template definitions.

use crate::graph::{InputPort, OutputPort, Parameter, ParameterValue};
use crate::id::DataTypeId;
use crate::registry::{NodeCategory, NodeRegistry, NodeTemplate};

pub fn register_builtins(reg: &mut NodeRegistry) {
    reg.register(constant());
    reg.register(merge());
    reg.register(blur());
    reg.register(transform());
    reg.register(color_correct());
}

fn constant() -> NodeTemplate {
    NodeTemplate::new("constant", "Constant", NodeCategory::Generator)
        .with_output(OutputPort {
            name: "value".into(),
            data_type: DataTypeId::SCALAR,
        })
        .with_param(Parameter {
            key: "value".into(),
            value: ParameterValue::Float(0.0),
        })
}

fn merge() -> NodeTemplate {
    NodeTemplate::new("merge", "Merge", NodeCategory::Compositor)
        .with_input(InputPort {
            name: "A".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
        })
        .with_input(InputPort {
            name: "B".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::FRAME_BUFFER,
        })
        .with_param(Parameter {
            key: "operation".into(),
            value: ParameterValue::String("over".into()),
        })
        .with_param(Parameter {
            key: "mix".into(),
            value: ParameterValue::Float(1.0),
        })
}

fn blur() -> NodeTemplate {
    NodeTemplate::new("blur", "Blur", NodeCategory::Filter)
        .with_input(InputPort {
            name: "image".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::FRAME_BUFFER,
        })
        .with_param(Parameter {
            key: "radius".into(),
            value: ParameterValue::Float(5.0),
        })
}

fn transform() -> NodeTemplate {
    NodeTemplate::new("transform", "Transform", NodeCategory::Transform)
        .with_input(InputPort {
            name: "image".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::FRAME_BUFFER,
        })
        .with_param(Parameter {
            key: "translate_x".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "translate_y".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "rotation".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "scale".into(),
            value: ParameterValue::Float(1.0),
        })
}

fn color_correct() -> NodeTemplate {
    NodeTemplate::new("color_correct", "Color Correct", NodeCategory::Color)
        .with_input(InputPort {
            name: "image".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::FRAME_BUFFER,
        })
        .with_param(Parameter {
            key: "brightness".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "contrast".into(),
            value: ParameterValue::Float(1.0),
        })
        .with_param(Parameter {
            key: "saturation".into(),
            value: ParameterValue::Float(1.0),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_all_builtins() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        assert_eq!(reg.all_templates().count(), 5);
    }

    #[test]
    fn builtins_cover_expected_categories() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        assert_eq!(reg.list_by_category(NodeCategory::Generator).len(), 1);
        assert_eq!(reg.list_by_category(NodeCategory::Compositor).len(), 1);
        assert_eq!(reg.list_by_category(NodeCategory::Filter).len(), 1);
        assert_eq!(reg.list_by_category(NodeCategory::Transform).len(), 1);
        assert_eq!(reg.list_by_category(NodeCategory::Color).len(), 1);
    }

    #[test]
    fn constant_node_has_no_inputs() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        let tmpl = reg.get("constant").unwrap();
        assert!(tmpl.inputs.is_empty());
        assert_eq!(tmpl.outputs.len(), 1);
    }

    #[test]
    fn merge_node_has_two_inputs() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        let tmpl = reg.get("merge").unwrap();
        assert_eq!(tmpl.inputs.len(), 2);
        assert_eq!(tmpl.inputs[0].name, "A");
        assert_eq!(tmpl.inputs[1].name, "B");
    }
}
