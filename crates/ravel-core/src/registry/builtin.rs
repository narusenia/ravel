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
    reg.register(rasterize());
    reg.register(shape_rect());
    reg.register(shape_ellipse());
    reg.register(shape_polygon());
    reg.register(shape_star());
    reg.register(shape_custom_path());
    reg.register(scatter_grid());
    reg.register(scatter_circular());
    reg.register(scatter_path_array());
    reg.register(scatter_scatter());
}

fn rasterize() -> NodeTemplate {
    NodeTemplate::new("rasterize", "Rasterize", NodeCategory::Generator)
        .with_input(InputPort {
            name: "geometry".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::FRAME_BUFFER,
        })
        .with_param(Parameter {
            key: "fill".into(),
            value: ParameterValue::Bool(true),
        })
        .with_param(Parameter {
            key: "stroke_width".into(),
            value: ParameterValue::Float(0.0),
        })
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

fn shape_rect() -> NodeTemplate {
    NodeTemplate::new("shape.rect", "Rectangle", NodeCategory::Generator)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(Parameter {
            key: "center_x".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "center_y".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "width".into(),
            value: ParameterValue::Float(100.0),
        })
        .with_param(Parameter {
            key: "height".into(),
            value: ParameterValue::Float(100.0),
        })
}

fn shape_ellipse() -> NodeTemplate {
    NodeTemplate::new("shape.ellipse", "Ellipse", NodeCategory::Generator)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(Parameter {
            key: "center_x".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "center_y".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "radius_x".into(),
            value: ParameterValue::Float(50.0),
        })
        .with_param(Parameter {
            key: "radius_y".into(),
            value: ParameterValue::Float(50.0),
        })
        .with_param(Parameter {
            key: "segments".into(),
            value: ParameterValue::Int(32),
        })
}

fn shape_polygon() -> NodeTemplate {
    NodeTemplate::new("shape.polygon", "Polygon", NodeCategory::Generator)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(Parameter {
            key: "center_x".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "center_y".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "radius".into(),
            value: ParameterValue::Float(50.0),
        })
        .with_param(Parameter {
            key: "sides".into(),
            value: ParameterValue::Int(6),
        })
}

fn shape_star() -> NodeTemplate {
    NodeTemplate::new("shape.star", "Star", NodeCategory::Generator)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(Parameter {
            key: "center_x".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "center_y".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "outer_radius".into(),
            value: ParameterValue::Float(50.0),
        })
        .with_param(Parameter {
            key: "inner_radius".into(),
            value: ParameterValue::Float(25.0),
        })
        .with_param(Parameter {
            key: "points".into(),
            value: ParameterValue::Int(5),
        })
}

fn scatter_grid() -> NodeTemplate {
    NodeTemplate::new("scatter.grid", "Grid", NodeCategory::Generator)
        .with_input(InputPort {
            name: "instance_source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(Parameter {
            key: "count_x".into(),
            value: ParameterValue::Int(5),
        })
        .with_param(Parameter {
            key: "count_y".into(),
            value: ParameterValue::Int(5),
        })
        .with_param(Parameter {
            key: "spacing_x".into(),
            value: ParameterValue::Float(20.0),
        })
        .with_param(Parameter {
            key: "spacing_y".into(),
            value: ParameterValue::Float(20.0),
        })
        .with_param(Parameter {
            key: "center_x".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "center_y".into(),
            value: ParameterValue::Float(0.0),
        })
}

fn scatter_circular() -> NodeTemplate {
    NodeTemplate::new("scatter.circular", "Circular", NodeCategory::Generator)
        .with_input(InputPort {
            name: "instance_source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(Parameter {
            key: "count".into(),
            value: ParameterValue::Int(8),
        })
        .with_param(Parameter {
            key: "radius".into(),
            value: ParameterValue::Float(50.0),
        })
        .with_param(Parameter {
            key: "center_x".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "center_y".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "align_rotation".into(),
            value: ParameterValue::Bool(true),
        })
}

fn scatter_path_array() -> NodeTemplate {
    NodeTemplate::new("scatter.path_array", "Path Array", NodeCategory::Generator)
        .with_input(InputPort {
            name: "path".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
        })
        .with_input(InputPort {
            name: "instance_source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(Parameter {
            key: "count".into(),
            value: ParameterValue::Int(10),
        })
}

fn scatter_scatter() -> NodeTemplate {
    NodeTemplate::new("scatter.scatter", "Scatter", NodeCategory::Generator)
        .with_input(InputPort {
            name: "instance_source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(Parameter {
            key: "count".into(),
            value: ParameterValue::Int(20),
        })
        .with_param(Parameter {
            key: "area_x".into(),
            value: ParameterValue::Float(200.0),
        })
        .with_param(Parameter {
            key: "area_y".into(),
            value: ParameterValue::Float(200.0),
        })
        .with_param(Parameter {
            key: "center_x".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "center_y".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "seed".into(),
            value: ParameterValue::Int(0),
        })
}

fn shape_custom_path() -> NodeTemplate {
    NodeTemplate::new("shape.custom_path", "Custom Path", NodeCategory::Generator).with_output(
        OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_all_builtins() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        assert_eq!(reg.all_templates().count(), 15);
    }

    #[test]
    fn builtins_cover_expected_categories() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        assert_eq!(reg.list_by_category(NodeCategory::Generator).len(), 11);
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
