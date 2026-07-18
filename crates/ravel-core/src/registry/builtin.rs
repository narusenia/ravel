// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Built-in node template definitions.

use crate::animation::channel::AnimationChannel;
use crate::graph::{InputPort, OutputPort, Parameter, ParameterValue};
use crate::id::DataTypeId;
use crate::registry::{NodeCategory, NodeRegistry, NodeTemplate};

pub fn register_builtins(reg: &mut NodeRegistry) {
    reg.register(constant());
    reg.register(constant_color());
    reg.register(video());
    reg.register(layer_ref());
    reg.register(subnet());
    reg.register(merge());
    reg.register(math_scalar());
    reg.register(math_remap());
    reg.register(geometry_transform());
    reg.register(geometry_merge());
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
        is_param: false,
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
        is_param: false,
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
        .with_param_range("value", -1e9..=1e9, -10.0..=10.0)
        .with_param_range("value_y", -1e9..=1e9, -10.0..=10.0)
        .with_param_range("value_z", -1e9..=1e9, -10.0..=10.0)
        .with_param_range("value_w", -1e9..=1e9, -10.0..=10.0)
        .with_param_range("int_value", -1e9..=1e9, -100.0..=100.0)
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
    .with_param_range("distance", 0.0..=1e6, 0.0..=1000.0)
}

fn field_noise() -> NodeTemplate {
    NodeTemplate::new("field.noise", "Noise Field", NodeCategory::Utility)
        .with_output(field_output())
        .with_param(int_parameter("seed", 0))
        .with_param(float_parameter("frequency", 1.0))
        .with_param(int_parameter("octaves", 1))
        .with_param_range("seed", 0.0..=1e9, 0.0..=1000.0)
        .with_param_range("frequency", 0.0..=1000.0, 0.0..=10.0)
        .with_param_range("octaves", 1.0..=8.0, 1.0..=8.0)
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
        .with_param_range("center_x", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("center_y", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("inner_radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("outer_radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("direction_x", -1.0..=1.0, -1.0..=1.0)
        .with_param_range("direction_y", -1.0..=1.0, -1.0..=1.0)
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
    .with_param_range("default", -1e9..=1e9, -10.0..=10.0)
}

fn field_binary(type_key: &str, label: &str) -> NodeTemplate {
    NodeTemplate::new(type_key, label, NodeCategory::Utility)
        .with_input(field_input("left"))
        .with_input(field_input("right"))
        .with_output(field_output())
}

fn field_blend() -> NodeTemplate {
    field_binary("field.blend", "Field Blend")
        .with_param(float_parameter("amount", 0.5))
        .with_param_range("amount", 0.0..=1.0, 0.0..=1.0)
}

fn field_apply() -> NodeTemplate {
    NodeTemplate::new("field.apply", "Apply Field", NodeCategory::Utility)
        .with_input(geometry_input("geometry"))
        .with_input(field_input("field"))
        .with_output(geometry_output())
        .with_param(string_parameter("domain", "point"))
        .with_param(string_parameter("target", "value"))
        .with_param(float_parameter("amount", 1.0))
        .with_param_range("amount", -10.0..=10.0, 0.0..=1.0)
}

fn rasterize() -> NodeTemplate {
    NodeTemplate::new("rasterize", "Rasterize", NodeCategory::Generator)
        .with_input(InputPort {
            name: "geometry".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
        })
        // Pre-exposed parameter port: the evaluator overlays a connected
        // color onto the `color` parameter (attribute > pin > parameter,
        // REQ-LAYER-008 — the priority rule this node pioneered, now served
        // by the general parameter-port mechanism).
        .with_input(InputPort {
            name: "color".into(),
            accepted_types: vec![DataTypeId::COLOR],
            is_param: true,
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
        // Element color priority: Cd/alpha attributes > `color` pin > this
        // parameter (REQ-LAYER-008).
        .with_param(Parameter {
            key: "color".into(),
            value: ParameterValue::Channel4([
                AnimationChannel::constant(1.0),
                AnimationChannel::constant(1.0),
                AnimationChannel::constant(1.0),
                AnimationChannel::constant(1.0),
            ]),
        })
        .with_param_range("stroke_width", 0.0..=1000.0, 0.0..=20.0)
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
        .with_param_range("value", -1e9..=1e9, -10.0..=10.0)
}

fn video() -> NodeTemplate {
    NodeTemplate::new("video", "Video", NodeCategory::Generator)
        .with_output(OutputPort {
            name: "frame".into(),
            data_type: DataTypeId::FRAME_BUFFER,
        })
        .with_param(string_parameter("asset_id", ""))
}

fn subnet() -> NodeTemplate {
    // Pins are dynamic: the inner net.in / net.out definitions become the
    // node's ports (REQ-LAYER-003). The template starts empty.
    NodeTemplate::new("subnet", "Subnet", NodeCategory::Utility)
}

fn layer_ref() -> NodeTemplate {
    NodeTemplate::new("layer.ref", "Layer Ref", NodeCategory::Utility)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::FRAME_BUFFER,
        })
        // Target layer id within the same composition (REQ-LAYER-005).
        // Layer ids fit 24 bits (deterministic shell-id packing).
        .with_param(int_parameter("layer", -1))
        .with_param(string_parameter("port", "frame"))
        .with_param_range("layer", -1.0..=16_777_215.0, -1.0..=1000.0)
}

fn constant_color() -> NodeTemplate {
    NodeTemplate::new("constant.color", "RGB Color", NodeCategory::Generator)
        .with_output(OutputPort {
            name: "color".into(),
            data_type: DataTypeId::COLOR,
        })
        .with_param(Parameter {
            key: "color".into(),
            value: ParameterValue::Channel4([
                AnimationChannel::constant(1.0),
                AnimationChannel::constant(1.0),
                AnimationChannel::constant(1.0),
                AnimationChannel::constant(1.0),
            ]),
        })
}

fn merge() -> NodeTemplate {
    NodeTemplate::new("merge", "Merge", NodeCategory::Compositor)
        .with_input(InputPort {
            name: "A".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
            is_param: false,
        })
        .with_input(InputPort {
            name: "B".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
            is_param: false,
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::FRAME_BUFFER,
        })
        .with_param(Parameter {
            key: "operation".into(),
            value: ParameterValue::String("over".into()),
        })
        .with_param_options("operation", ["over", "add", "multiply"])
        .with_param(Parameter {
            key: "mix".into(),
            value: ParameterValue::Float(1.0),
        })
        .with_param_range("mix", 0.0..=1.0, 0.0..=1.0)
}

/// Ops of `math.scalar`; binary ops read `a` and `b`, unary ops read `a`.
pub const MATH_SCALAR_OPS: [&str; 16] = [
    "add", "subtract", "multiply", "divide", "min", "max", "mod", "pow", "abs", "negate", "floor",
    "ceil", "round", "sqrt", "sin", "cos",
];

fn math_scalar() -> NodeTemplate {
    NodeTemplate::new("math.scalar", "Math", NodeCategory::Utility)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::SCALAR,
        })
        .with_param(string_parameter("op", "add"))
        .with_param_options("op", MATH_SCALAR_OPS)
        .with_param(float_parameter("a", 0.0))
        .with_param(float_parameter("b", 1.0))
        .with_param_range("a", -1e9..=1e9, -10.0..=10.0)
        .with_param_range("b", -1e9..=1e9, -10.0..=10.0)
}

fn math_remap() -> NodeTemplate {
    NodeTemplate::new("math.remap", "Remap", NodeCategory::Utility)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::SCALAR,
        })
        .with_param(float_parameter("value", 0.0))
        .with_param(float_parameter("in_min", 0.0))
        .with_param(float_parameter("in_max", 1.0))
        .with_param(float_parameter("out_min", 0.0))
        .with_param(float_parameter("out_max", 1.0))
        .with_param(Parameter {
            key: "clamp".into(),
            value: ParameterValue::Bool(false),
        })
        .with_param_range("value", -1e9..=1e9, -10.0..=10.0)
        .with_param_range("in_min", -1e9..=1e9, -10.0..=10.0)
        .with_param_range("in_max", -1e9..=1e9, -10.0..=10.0)
        .with_param_range("out_min", -1e9..=1e9, -10.0..=10.0)
        .with_param_range("out_max", -1e9..=1e9, -10.0..=10.0)
}

fn geometry_transform() -> NodeTemplate {
    NodeTemplate::new(
        "geometry.transform",
        "Geometry Transform",
        NodeCategory::Transform,
    )
    .with_input(geometry_input("geometry"))
    .with_output(geometry_output())
    .with_param(float_parameter("translate_x", 0.0))
    .with_param(float_parameter("translate_y", 0.0))
    .with_param(float_parameter("rotation", 0.0))
    .with_param(float_parameter("scale_x", 1.0))
    .with_param(float_parameter("scale_y", 1.0))
    .with_param(Parameter {
        key: "use_centroid".into(),
        value: ParameterValue::Bool(true),
    })
    .with_param(float_parameter("pivot_x", 0.0))
    .with_param(float_parameter("pivot_y", 0.0))
    .with_param_range("translate_x", -1e9..=1e9, -1000.0..=1000.0)
    .with_param_range("translate_y", -1e9..=1e9, -1000.0..=1000.0)
    .with_param_range("rotation", -1e9..=1e9, -360.0..=360.0)
    .with_param_range("scale_x", -1e9..=1e9, -10.0..=10.0)
    .with_param_range("scale_y", -1e9..=1e9, -10.0..=10.0)
    .with_param_range("pivot_x", -1e9..=1e9, -1000.0..=1000.0)
    .with_param_range("pivot_y", -1e9..=1e9, -1000.0..=1000.0)
}

fn geometry_merge() -> NodeTemplate {
    NodeTemplate::new("geometry.merge", "Geometry Merge", NodeCategory::Utility)
        .with_input(geometry_input("A"))
        .with_input(geometry_input("B"))
        .with_output(geometry_output())
}

fn blur() -> NodeTemplate {
    NodeTemplate::new("blur", "Blur", NodeCategory::Filter)
        .with_input(InputPort {
            name: "image".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
            is_param: false,
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::FRAME_BUFFER,
        })
        .with_param(Parameter {
            key: "radius".into(),
            value: ParameterValue::Float(5.0),
        })
        .with_param_range("radius", 0.0..=500.0, 0.0..=50.0)
}

fn transform() -> NodeTemplate {
    NodeTemplate::new("transform", "Transform", NodeCategory::Transform)
        .with_input(InputPort {
            name: "image".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
            is_param: false,
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
        .with_param_range("translate_x", -1e5..=1e5, -1000.0..=1000.0)
        .with_param_range("translate_y", -1e5..=1e5, -1000.0..=1000.0)
        .with_param_range("rotation", -36000.0..=36000.0, -360.0..=360.0)
        .with_param_range("scale", -100.0..=100.0, 0.0..=4.0)
}

fn color_correct() -> NodeTemplate {
    NodeTemplate::new("color_correct", "Color Correct", NodeCategory::Color)
        .with_input(InputPort {
            name: "image".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
            is_param: false,
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
        .with_param_range("brightness", -1.0..=1.0, -1.0..=1.0)
        .with_param_range("contrast", 0.0..=10.0, 0.0..=2.0)
        .with_param_range("saturation", 0.0..=10.0, 0.0..=2.0)
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
        .with_param_range("center_x", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("center_y", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("width", 0.0..=1e5, 0.0..=1000.0)
        .with_param_range("height", 0.0..=1e5, 0.0..=1000.0)
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
        .with_param_range("center_x", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("center_y", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("radius_x", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("radius_y", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("segments", 3.0..=512.0, 3.0..=128.0)
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
        .with_param_range("center_x", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("center_y", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("sides", 3.0..=128.0, 3.0..=32.0)
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
        .with_param_range("center_x", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("center_y", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("outer_radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("inner_radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("points", 3.0..=128.0, 3.0..=32.0)
}

fn scatter_grid() -> NodeTemplate {
    NodeTemplate::new("scatter.grid", "Grid", NodeCategory::Generator)
        .with_input(InputPort {
            name: "instance_source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
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
        .with_param_range("count_x", 1.0..=1000.0, 1.0..=50.0)
        .with_param_range("count_y", 1.0..=1000.0, 1.0..=50.0)
        .with_param_range("spacing_x", -1e5..=1e5, 0.0..=200.0)
        .with_param_range("spacing_y", -1e5..=1e5, 0.0..=200.0)
        .with_param_range("center_x", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("center_y", -1e5..=1e5, -2000.0..=2000.0)
}

fn scatter_circular() -> NodeTemplate {
    NodeTemplate::new("scatter.circular", "Circular", NodeCategory::Generator)
        .with_input(InputPort {
            name: "instance_source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
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
        .with_param_range("count", 1.0..=10000.0, 1.0..=100.0)
        .with_param_range("radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("center_x", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("center_y", -1e5..=1e5, -2000.0..=2000.0)
}

fn scatter_path_array() -> NodeTemplate {
    NodeTemplate::new("scatter.path_array", "Path Array", NodeCategory::Generator)
        .with_input(InputPort {
            name: "path".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
        })
        .with_input(InputPort {
            name: "instance_source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(Parameter {
            key: "count".into(),
            value: ParameterValue::Int(10),
        })
        .with_param_range("count", 1.0..=100000.0, 1.0..=100.0)
}

fn scatter_scatter() -> NodeTemplate {
    NodeTemplate::new("scatter.scatter", "Scatter", NodeCategory::Generator)
        .with_input(InputPort {
            name: "instance_source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
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
        .with_param_range("count", 0.0..=100000.0, 0.0..=500.0)
        .with_param_range("area_x", 0.0..=1e5, 0.0..=2000.0)
        .with_param_range("area_y", 0.0..=1e5, 0.0..=2000.0)
        .with_param_range("center_x", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("center_y", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("seed", 0.0..=1e9, 0.0..=1000.0)
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
        assert_eq!(reg.all_templates().count(), 36);
    }

    #[test]
    fn builtins_cover_expected_categories() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        assert_eq!(reg.list_by_category(NodeCategory::Generator).len(), 13);
        assert_eq!(reg.list_by_category(NodeCategory::Compositor).len(), 1);
        assert_eq!(reg.list_by_category(NodeCategory::Filter).len(), 1);
        assert_eq!(reg.list_by_category(NodeCategory::Transform).len(), 2);
        assert_eq!(reg.list_by_category(NodeCategory::Color).len(), 1);
        assert_eq!(reg.list_by_category(NodeCategory::Utility).len(), 18);
    }

    #[test]
    fn enum_params_declare_their_options() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        let ops = reg.param_options("math.scalar", "op").unwrap();
        assert_eq!(ops, MATH_SCALAR_OPS);
        let merge_ops = reg.param_options("merge", "operation").unwrap();
        assert_eq!(merge_ops, ["over", "add", "multiply"]);
        // Numeric parameters carry no option set.
        assert!(reg.param_options("math.scalar", "a").is_none());
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

    #[test]
    fn every_numeric_param_declares_a_range() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        for tmpl in reg.all_templates() {
            for param in &tmpl.default_params {
                let numeric = matches!(
                    param.value,
                    ParameterValue::Float(_) | ParameterValue::Int(_)
                );
                if numeric {
                    assert!(
                        tmpl.param_range(&param.key).is_some(),
                        "{}.{} has no ParamRange",
                        tmpl.type_key,
                        param.key
                    );
                }
            }
        }
    }

    #[test]
    fn ui_ranges_are_contained_in_hard_ranges() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        for tmpl in reg.all_templates() {
            for (key, range) in &tmpl.param_ranges {
                assert!(
                    range.hard.start() <= range.ui.start() && range.ui.end() <= range.hard.end(),
                    "{}.{}: ui {:?} outside hard {:?}",
                    tmpl.type_key,
                    key,
                    range.ui,
                    range.hard
                );
            }
        }
    }

    #[test]
    fn default_values_lie_within_hard_ranges() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        for tmpl in reg.all_templates() {
            for param in &tmpl.default_params {
                let value = match param.value {
                    ParameterValue::Float(v) => v,
                    ParameterValue::Int(v) => v as f32,
                    _ => continue,
                };
                if let Some(range) = tmpl.param_range(&param.key) {
                    assert!(
                        range.hard.contains(&value),
                        "{}.{}: default {value} outside hard {:?}",
                        tmpl.type_key,
                        param.key,
                        range.hard
                    );
                }
            }
        }
    }
}
