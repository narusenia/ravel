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
    reg.register(attribute_set());
    reg.register(attribute_promote());
    reg.register(attribute_transfer());
    reg.register(attribute_path_sample());
    reg.register(field_noise());
    reg.register(field_falloff());
    reg.register(field_curve_remap());
    reg.register(field_expression());
    reg.register(field_binary("field.add", "Field Add"));
    reg.register(field_binary("field.multiply", "Field Multiply"));
    reg.register(field_binary("field.max", "Field Max"));
    reg.register(field_blend());
    reg.register(field_apply());
}

fn geometry_input(name: &str) -> InputPort {
    InputPort {
        name: name.into(),
        accepted_types: vec![DataTypeId::GEOMETRY],
    }
}

fn geometry_output() -> OutputPort {
    OutputPort {
        name: "output".into(),
        data_type: DataTypeId::GEOMETRY,
    }
}

fn field_input(name: &str) -> InputPort {
    InputPort {
        name: name.into(),
        accepted_types: vec![DataTypeId::FIELD],
    }
}

fn field_output() -> OutputPort {
    OutputPort {
        name: "field".into(),
        data_type: DataTypeId::FIELD,
    }
}

fn string_parameter(key: &str, value: &str) -> Parameter {
    Parameter {
        key: key.into(),
        value: ParameterValue::String(value.into()),
    }
}

fn float_parameter(key: &str, value: f32) -> Parameter {
    Parameter {
        key: key.into(),
        value: ParameterValue::Float(value),
    }
}

fn int_parameter(key: &str, value: i32) -> Parameter {
    Parameter {
        key: key.into(),
        value: ParameterValue::Int(value),
    }
}

fn attribute_set() -> NodeTemplate {
    NodeTemplate::new("attribute.set", "Attribute Set", NodeCategory::Utility)
        .with_input(geometry_input("geometry"))
        .with_output(geometry_output())
        .with_param(string_parameter("domain", "point"))
        .with_param(string_parameter("name", "value"))
        .with_param(string_parameter("type", "f32"))
        .with_param(float_parameter("value", 0.0))
        .with_param(float_parameter("value_y", 0.0))
        .with_param(float_parameter("value_z", 0.0))
        .with_param(float_parameter("value_w", 0.0))
        .with_param(int_parameter("int_value", 0))
        .with_param(Parameter {
            key: "bool_value".into(),
            value: ParameterValue::Bool(false),
        })
        .with_param(string_parameter("string_value", ""))
}

fn attribute_promote() -> NodeTemplate {
    NodeTemplate::new(
        "attribute.promote",
        "Attribute Promote",
        NodeCategory::Utility,
    )
    .with_input(geometry_input("geometry"))
    .with_output(geometry_output())
    .with_param(string_parameter("source_domain", "point"))
    .with_param(string_parameter("target_domain", "detail"))
    .with_param(string_parameter("name", "value"))
    .with_param(string_parameter("aggregate", "average"))
}

fn attribute_transfer() -> NodeTemplate {
    NodeTemplate::new(
        "attribute.transfer",
        "Attribute Transfer",
        NodeCategory::Utility,
    )
    .with_input(geometry_input("target"))
    .with_input(geometry_input("source"))
    .with_output(geometry_output())
    .with_param(string_parameter("target_domain", "point"))
    .with_param(string_parameter("source_domain", "point"))
    .with_param(string_parameter("name", "value"))
    .with_param(string_parameter("mode", "nearest"))
}

fn attribute_path_sample() -> NodeTemplate {
    NodeTemplate::new(
        "attribute.path_sample",
        "Path Sample",
        NodeCategory::Utility,
    )
    .with_input(geometry_input("path"))
    .with_output(geometry_output())
    .with_param(float_parameter("distance", 0.0))
}

fn field_noise() -> NodeTemplate {
    NodeTemplate::new("field.noise", "Noise Field", NodeCategory::Utility)
        .with_output(field_output())
        .with_param(int_parameter("seed", 0))
        .with_param(float_parameter("frequency", 1.0))
        .with_param(int_parameter("octaves", 1))
}

fn field_falloff() -> NodeTemplate {
    NodeTemplate::new("field.falloff", "Falloff Field", NodeCategory::Utility)
        .with_output(field_output())
        .with_param(string_parameter("shape", "sphere"))
        .with_param(float_parameter("center_x", 0.0))
        .with_param(float_parameter("center_y", 0.0))
        .with_param(float_parameter("inner_radius", 0.0))
        .with_param(float_parameter("outer_radius", 1.0))
        .with_param(float_parameter("direction_x", 1.0))
        .with_param(float_parameter("direction_y", 0.0))
}

fn field_curve_remap() -> NodeTemplate {
    NodeTemplate::new(
        "field.curve_remap",
        "Curve Remap Field",
        NodeCategory::Utility,
    )
    .with_input(field_input("field"))
    .with_output(field_output())
    .with_param(string_parameter("points", "0:0,1:1"))
}

fn field_expression() -> NodeTemplate {
    NodeTemplate::new(
        "field.expression",
        "Expression Field",
        NodeCategory::Utility,
    )
    .with_output(field_output())
    .with_param(string_parameter("expression", ""))
    .with_param(float_parameter("default", 0.0))
}

fn field_binary(type_key: &str, label: &str) -> NodeTemplate {
    NodeTemplate::new(type_key, label, NodeCategory::Utility)
        .with_input(field_input("left"))
        .with_input(field_input("right"))
        .with_output(field_output())
}

fn field_blend() -> NodeTemplate {
    field_binary("field.blend", "Field Blend").with_param(float_parameter("amount", 0.5))
}

fn field_apply() -> NodeTemplate {
    NodeTemplate::new("field.apply", "Apply Field", NodeCategory::Utility)
        .with_input(geometry_input("geometry"))
        .with_input(field_input("field"))
        .with_output(geometry_output())
        .with_param(string_parameter("domain", "point"))
        .with_param(string_parameter("target", "value"))
        .with_param(float_parameter("amount", 1.0))
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
        assert_eq!(reg.all_templates().count(), 28);
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
        assert_eq!(reg.list_by_category(NodeCategory::Utility).len(), 13);
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
