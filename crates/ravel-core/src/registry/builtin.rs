// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Built-in node template definitions.

use crate::animation::channel::AnimationChannel;
use crate::graph::{InputPort, Node, OutputPort, Parameter, ParameterValue};
use crate::id::DataTypeId;
use crate::param_curve::CurveParam;
use crate::param_ramp::RampParam;
use crate::registry::{NodeCategory, NodeRegistry, NodeTemplate};
use crate::scene::camera;

/// Direct separable blur loop budget. Larger visual radii need a future
/// downsampled or multi-pass approximation instead of an unbounded shader loop.
pub const MAX_BLUR_RADIUS: f32 = 64.0;

pub fn register_builtins(reg: &mut NodeRegistry) {
    reg.register(constant());
    reg.register(constant_color());
    reg.register(constant_vector(
        CONSTANT_VEC2,
        "Constant Vec2",
        DataTypeId::VEC2,
        ParameterValue::vec2(0.0, 0.0),
    ));
    reg.register(constant_vector(
        CONSTANT_VEC3,
        "Constant Vec3",
        DataTypeId::VEC3,
        ParameterValue::vec3(0.0, 0.0, 0.0),
    ));
    reg.register(constant_vector(
        CONSTANT_VEC4,
        "Constant Vec4",
        DataTypeId::VEC4,
        ParameterValue::Channel4([
            AnimationChannel::constant(0.0),
            AnimationChannel::constant(0.0),
            AnimationChannel::constant(0.0),
            AnimationChannel::constant(0.0),
        ]),
    ));
    reg.register(media());
    reg.register(layer_ref());
    reg.register(subnet());
    reg.register(merge());
    reg.register(math_scalar());
    reg.register(math_remap());
    reg.register(math_curve());
    reg.register(vector_construct(
        VECTOR_CONSTRUCT_VEC2,
        "Construct Vec2",
        DataTypeId::VEC2,
        2,
    ));
    reg.register(vector_construct(
        VECTOR_CONSTRUCT_VEC3,
        "Construct Vec3",
        DataTypeId::VEC3,
        3,
    ));
    reg.register(vector_construct(
        VECTOR_CONSTRUCT_VEC4,
        "Construct Vec4",
        DataTypeId::VEC4,
        4,
    ));
    reg.register(vector_split(
        VECTOR_SPLIT_VEC2,
        "Split Vec2",
        DataTypeId::VEC2,
        2,
    ));
    reg.register(vector_split(
        VECTOR_SPLIT_VEC3,
        "Split Vec3",
        DataTypeId::VEC3,
        3,
    ));
    reg.register(vector_split(
        VECTOR_SPLIT_VEC4,
        "Split Vec4",
        DataTypeId::VEC4,
        4,
    ));
    reg.register(vector_swizzle(
        VECTOR_SWIZZLE_VEC2,
        "Swizzle Vec2",
        DataTypeId::VEC2,
        2,
    ));
    reg.register(vector_swizzle(
        VECTOR_SWIZZLE_VEC3,
        "Swizzle Vec3",
        DataTypeId::VEC3,
        3,
    ));
    reg.register(vector_swizzle(
        VECTOR_SWIZZLE_VEC4,
        "Swizzle Vec4",
        DataTypeId::VEC4,
        4,
    ));
    reg.register(vector_length());
    reg.register(vector_normalize(
        VECTOR_NORMALIZE_VEC2,
        "Normalize Vec2",
        DataTypeId::VEC2,
    ));
    reg.register(vector_normalize(
        VECTOR_NORMALIZE_VEC3,
        "Normalize Vec3",
        DataTypeId::VEC3,
    ));
    reg.register(vector_normalize(
        VECTOR_NORMALIZE_VEC4,
        "Normalize Vec4",
        DataTypeId::VEC4,
    ));
    reg.register(vector_dot());
    reg.register(vector_cross(
        VECTOR_CROSS_VEC2,
        "Cross Vec2",
        DataTypeId::VEC2,
        "cross",
        DataTypeId::SCALAR,
    ));
    reg.register(vector_cross(
        VECTOR_CROSS_VEC3,
        "Cross Vec3",
        DataTypeId::VEC3,
        "vector",
        DataTypeId::VEC3,
    ));
    reg.register(geometry_transform());
    reg.register(geometry_merge());
    reg.register(geometry_connect());
    reg.register(geometry_from_image());
    reg.register(scene_add());
    reg.register(scene_merge());
    reg.register(scene_camera());
    reg.register(blur());
    reg.register(transform());
    reg.register(color_correct());
    reg.register(rasterize());
    reg.register(shape_rect());
    reg.register(shape_ellipse());
    reg.register(shape_polygon());
    reg.register(shape_star());
    reg.register(shape_line());
    reg.register(shape_grid());
    reg.register(shape_custom_path());
    reg.register(scatter_grid());
    reg.register(scatter_circular());
    reg.register(scatter_path_array());
    reg.register(scatter_scatter());
    reg.register(attribute_set());
    reg.register(style_fill());
    reg.register(style_stroke());
    reg.register(style_dash());
    reg.register(attribute_promote());
    reg.register(attribute_transfer());
    reg.register(attribute_path_sample());
    reg.register(attribute_curveu());
    reg.register(field_noise());
    reg.register(field_falloff());
    reg.register(field_curve_remap());
    reg.register(field_ramp());
    reg.register(field_expression());
    reg.register(field_binary("field.add", "Field Add"));
    reg.register(field_binary("field.multiply", "Field Multiply"));
    reg.register(field_binary("field.max", "Field Max"));
    reg.register(field_blend());
    reg.register(field_length());
    reg.register(field_angle());
    reg.register(field_component());
    reg.register(field_compose(FIELD_COMPOSE_VEC2, "Field Compose Vec2", 2));
    reg.register(field_compose(FIELD_COMPOSE_VEC3, "Field Compose Vec3", 3));
    reg.register(field_compose(FIELD_COMPOSE_VEC4, "Field Compose Vec4", 4));
    reg.register(field_attribute());
    reg.register(field_apply());
}

fn geometry_input(name: &str) -> InputPort {
    InputPort {
        name: name.into(),
        accepted_types: vec![DataTypeId::GEOMETRY],
        is_param: false,
        is_variadic: false,
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
        is_variadic: false,
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

/// A structural transfer-curve parameter. Edited through the curve editor,
/// never as text — the string form it replaced is upgraded on load
/// (`.ravprj` v5 → v6).
fn curve_parameter(key: &str, value: CurveParam) -> Parameter {
    Parameter {
        key: key.into(),
        value: ParameterValue::Curve(value),
    }
}

/// A structural colour-ramp parameter. Edited through the gradient editor,
/// never as text, for the same reason a curve is not a string.
fn ramp_parameter(key: &str, value: RampParam) -> Parameter {
    Parameter {
        key: key.into(),
        value: ParameterValue::Ramp(value),
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

/// A 2-component vector parameter. Geometric vectors are declared as one
/// `Channel2` rather than a `_x` / `_y` pair of Floats so Properties renders
/// one Vector row, one parameter port carries the whole value (VEC2), and
/// `ParamRole` has a single parameter to attach a meaning to.
fn channel2_parameter(key: &str, x: f32, y: f32) -> Parameter {
    Parameter {
        key: key.into(),
        value: ParameterValue::vec2(x, y),
    }
}

/// A 3-component vector parameter. Parameters that gain a Z component with
/// 3D support are declared `Channel3` from the start so `.ravprj` migration
/// runs once instead of twice (`3d-scene-plan.md` unit 1a).
fn channel3_parameter(key: &str, x: f32, y: f32, z: f32) -> Parameter {
    Parameter {
        key: key.into(),
        value: ParameterValue::vec3(x, y, z),
    }
}

/// A colour parameter. Declared `Channel4` so Properties draws a colour
/// swatch and the `.ravprj` migration tells a colour from a `VEC4`
/// (`docs/dev/add-node.md`).
fn color_parameter(key: &str, rgba: [f32; 4]) -> Parameter {
    Parameter {
        key: key.into(),
        value: ParameterValue::Channel4(rgba.map(AnimationChannel::constant)),
    }
}

/// Domains the style nodes can write their per-element attributes on. Detail
/// is absent on purpose: `rasterize` resolves `fill` / `stroke_width` /
/// `stroke_color` per primitive and per instance, so a Detail column would be
/// written and never read.
/// Domains `style.fill` / `style.stroke` can write to.
///
/// **`point` is deliberately absent.** `rasterize` reads a path's style from
/// the Primitive domain and an instance's from the Instance domain; nothing
/// reads `fill` or `stroke_width` from a point. Offering the domain anyway
/// would put a third "succeeds and changes nothing" trap next to the two this
/// codebase already paid for — the `stroke_align` attribute that unit 1 of
/// `style-attributes-plan.md` declined to declare until it worked, and the
/// `attribute.set(domain = "primitive")` that silently wrote to points.
///
/// Per-point colour is `attribute.set` / `field.apply` writing `Cd`, which
/// loose points do render. Per-point styling *of a path* needs vertex
/// interpolation in the rasterizer — `path-shading-plan.md`.
pub const STYLE_DOMAINS: [&str; 2] = ["primitive", "instance"];

/// Stroke end shapes `style.stroke` can select, ordered to match the `cap`
/// attribute codes (`geometry::names::CAP_*`).
pub const STROKE_CAPS: [&str; 3] = ["butt", "round", "square"];

/// Stroke corner shapes `style.stroke` can select, ordered to match the `join`
/// attribute codes (`geometry::names::JOIN_*`).
pub const STROKE_JOINS: [&str; 3] = ["miter", "round", "bevel"];

/// Attribute types `attribute.set` can write.
pub const ATTRIBUTE_SET_TYPES: [&str; 8] = [
    "f32", "vec2", "vec3", "vec4", "color", "i32", "bool", "string",
];

/// The `attribute.set` `type` that `value` is shaped for when the node is
/// created (and the fallback the processor uses for an unknown `type`).
pub const ATTRIBUTE_SET_DEFAULT_TYPE: &str = "f32";

/// Per-component defaults of `attribute.set`'s `value` for one `type`, which
/// also fix its arity. An empty slice means the type reads a different
/// parameter (`int_value` / `bool_value` / `string_value`) and `value` is
/// carried along as a 1-component channel.
///
/// Colour alpha defaults to 1, matching every other colour in the registry.
/// The `.ravprj` v4 templates wrote all four `value_*` components, so a real
/// v4 file always supplies its own alpha and this default only fills a gap no
/// Ravel-written file has.
pub fn attribute_set_value_defaults(type_name: &str) -> &'static [f32] {
    match type_name {
        "vec2" => &[0.0, 0.0],
        "vec3" => &[0.0, 0.0, 0.0],
        "vec4" => &[0.0, 0.0, 0.0, 0.0],
        "color" => &[0.0, 0.0, 0.0, 1.0],
        _ => &[0.0],
    }
}

/// Component count of `attribute.set`'s `value` for one `type`.
pub fn attribute_set_value_arity(type_name: &str) -> usize {
    attribute_set_value_defaults(type_name).len()
}

/// Reshape `existing` into the `value` an `attribute.set` of `type_name`
/// reads: components both shapes have are kept (channels and their keyframes
/// included), components the new shape adds take
/// [`attribute_set_value_defaults`], and components it drops are discarded.
/// `None` when `existing` carries no float components at all.
pub fn attribute_set_value_for_type(
    type_name: &str,
    existing: &ParameterValue,
) -> Option<ParameterValue> {
    let defaults = attribute_set_value_defaults(type_name);
    let kept = existing.channels()?;
    let channels: Vec<AnimationChannel> = defaults
        .iter()
        .enumerate()
        .map(|(index, default)| {
            kept.get(index)
                .cloned()
                .unwrap_or_else(|| AnimationChannel::constant(*default))
        })
        .collect();
    ParameterValue::from_channels(channels)
}

/// Parameter updates that must accompany `changed` for `node` to stay
/// self-consistent, so one command writes both (the Document snapshot is the
/// undo unit — a half-applied change must never be committable).
///
/// The only such dependency today is `attribute.set`'s `value`, whose arity
/// follows its `type`. `Graph::set_params` applies the result and re-types the
/// affected parameter port.
pub fn dependent_param_updates(node: &Node, changed: &Parameter) -> Vec<Parameter> {
    if node.type_key != "attribute.set" || changed.key != "type" {
        return Vec::new();
    }
    let Some(type_name) = changed.value.as_str() else {
        return Vec::new();
    };
    let Some(value) = node.parameters.iter().find(|p| p.key == "value") else {
        return Vec::new();
    };
    match attribute_set_value_for_type(type_name, &value.value) {
        Some(reshaped) if reshaped != value.value => vec![Parameter {
            key: "value".into(),
            value: reshaped,
        }],
        _ => Vec::new(),
    }
}

/// `value` is one parameter whose arity follows `type` (`f32` → `Channel`,
/// `vec2` → `Channel2`, …, `color` → `Channel4`), not a `value` / `value_y` /
/// `value_z` / `value_w` family. Editing `type` reshapes it through
/// [`dependent_param_updates`]; `.ravprj` v4 files are folded on load. The
/// `i32` / `bool` / `string` types read their own parameters and leave `value`
/// as an inert 1-component channel.
fn attribute_set() -> NodeTemplate {
    NodeTemplate::new("attribute.set", "Attribute Set", NodeCategory::Geometry)
        .with_input(geometry_input("geometry"))
        .with_output(geometry_output())
        .with_param(string_parameter("domain", "point"))
        .with_param(string_parameter("name", "value"))
        .with_param(string_parameter("type", ATTRIBUTE_SET_DEFAULT_TYPE))
        .with_param_options("type", ATTRIBUTE_SET_TYPES)
        .with_param(Parameter {
            key: "value".into(),
            value: ParameterValue::Channel(AnimationChannel::constant(0.0)),
        })
        .with_param(int_parameter("int_value", 0))
        .with_param(Parameter {
            key: "bool_value".into(),
            value: ParameterValue::Bool(false),
        })
        .with_param(string_parameter("string_value", ""))
        .with_param_range("value", -1e9..=1e9, -10.0..=10.0)
        .with_param_range("int_value", -1e9..=1e9, -100.0..=100.0)
}

/// Writes the fill attributes (`fill`, `Cd`) onto the chosen domain.
///
/// Style nodes do not rasterize: the picture is still made by `rasterize`
/// alone, which resolves these attributes per element (attribute > its own
/// parameter > default). That is what lets `field.apply` modulate the look
/// between here and there.
fn style_fill() -> NodeTemplate {
    NodeTemplate::new("style.fill", "Style Fill", NodeCategory::Geometry)
        .with_input(geometry_input("geometry"))
        .with_output(geometry_output())
        .with_param(Parameter {
            key: "enabled".into(),
            value: ParameterValue::Bool(true),
        })
        .with_param(color_parameter("color", [1.0, 1.0, 1.0, 1.0]))
        .with_param(string_parameter("domain", "primitive"))
        .with_param_options("domain", STYLE_DOMAINS)
        .with_param(string_parameter("group", ""))
}

/// Writes the stroke attributes (`stroke_width`, `stroke_color`) onto the
/// chosen domain. See [`style_fill`] for why this does not draw anything.
fn style_stroke() -> NodeTemplate {
    NodeTemplate::new("style.stroke", "Style Stroke", NodeCategory::Geometry)
        .with_input(geometry_input("geometry"))
        .with_output(geometry_output())
        .with_param(float_parameter("width", 1.0))
        .with_param(color_parameter("color", [1.0, 1.0, 1.0, 1.0]))
        .with_param(string_parameter("domain", "primitive"))
        .with_param_options("domain", STYLE_DOMAINS)
        .with_param(string_parameter("group", ""))
        // Cap and join are Detail: one value for the whole geometry, because a
        // per-element corner shape would grow the GPU instance buffer for a
        // need nobody has. `group` and `domain` do not apply to them.
        .with_param(string_parameter("cap", STROKE_CAPS[1]))
        .with_param_options("cap", STROKE_CAPS)
        .with_param(string_parameter("join", STROKE_JOINS[1]))
        .with_param_options("join", STROKE_JOINS)
        .with_param_range("width", 0.0..=1000.0, 0.0..=20.0)
}

/// Writes the Detail dash attributes. Separate from `style.stroke` because a
/// dash is the one stroke property that costs the rasterizer its GPU path
/// (`style-attributes-plan.md` unit 3), so it should be visible in the graph
/// that a dash was asked for.
fn style_dash() -> NodeTemplate {
    NodeTemplate::new("style.dash", "Style Dash", NodeCategory::Geometry)
        .with_input(geometry_input("geometry"))
        .with_output(geometry_output())
        .with_param(string_parameter("pattern", ""))
        .with_param(float_parameter("offset", 0.0))
        .with_param_range("offset", -1e6..=1e6, -100.0..=100.0)
}

fn attribute_promote() -> NodeTemplate {
    NodeTemplate::new(
        "attribute.promote",
        "Attribute Promote",
        NodeCategory::Geometry,
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
        NodeCategory::Geometry,
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
        NodeCategory::Geometry,
    )
    .with_input(geometry_input("path"))
    .with_output(geometry_output())
    .with_param(float_parameter("distance", 0.0))
    .with_param_range("distance", 0.0..=1e6, 0.0..=1000.0)
}

/// `attribute.curveu`: write the path parameter `u` on every point.
///
/// The companion of `attribute.path_sample`, which reads *one* place on a
/// path; this one labels every point with where it sits, which is what
/// `field.attribute("u")` needs to drive anything along the path.
fn attribute_curveu() -> NodeTemplate {
    NodeTemplate::new("attribute.curveu", "Curve U", NodeCategory::Geometry)
        .with_input(geometry_input("path"))
        .with_output(geometry_output())
        .with_param(string_parameter("mode", "by_arc_length"))
        .with_param_options("mode", ["by_arc_length", "by_vertex_order"])
}

fn field_noise() -> NodeTemplate {
    NodeTemplate::new("field.noise", "Noise Field", NodeCategory::Field)
        .with_output(field_output())
        .with_param(int_parameter("seed", 0))
        .with_param(float_parameter("frequency", 1.0))
        .with_param(int_parameter("octaves", 1))
        .with_param_range("seed", 0.0..=1e9, 0.0..=1000.0)
        .with_param_range("frequency", 0.0..=1000.0, 0.0..=10.0)
        .with_param_range("octaves", 1.0..=8.0, 1.0..=8.0)
}

fn field_falloff() -> NodeTemplate {
    NodeTemplate::new("field.falloff", "Falloff Field", NodeCategory::Field)
        .with_output(field_output())
        .with_param(string_parameter("shape", "sphere"))
        .with_param(channel3_parameter("center", 0.0, 0.0, 0.0))
        .with_param(float_parameter("inner_radius", 0.0))
        .with_param(float_parameter("outer_radius", 1.0))
        .with_param(channel3_parameter("direction", 1.0, 0.0, 0.0))
        .with_param_range("center", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("inner_radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("outer_radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("direction", -1.0..=1.0, -1.0..=1.0)
}

fn field_curve_remap() -> NodeTemplate {
    NodeTemplate::new(
        "field.curve_remap",
        "Curve Remap Field",
        NodeCategory::Field,
    )
    .with_input(field_input("field"))
    .with_output(field_output())
    // The control points were a comma-separated string until `.ravprj` v6;
    // `Document::upgrade_curve_params` converts stored ones on load.
    .with_param(curve_parameter("points", CurveParam::identity()))
}

/// `field.ramp`: a scalar field read through a colour ramp.
///
/// The only field that turns a number into a hue: `in_min` / `in_max`
/// normalize the input onto the ramp's `0..=1` domain, and the result drops
/// straight into `field.apply`'s Color targets (`Cd`, `stroke_color`).
fn field_ramp() -> NodeTemplate {
    NodeTemplate::new("field.ramp", "Ramp Field", NodeCategory::Field)
        .with_input(field_input("field"))
        .with_output(field_output())
        .with_param(ramp_parameter("stops", RampParam::default()))
        .with_param(float_parameter("in_min", 0.0))
        .with_param(float_parameter("in_max", 1.0))
        .with_param_range("in_min", -1e9..=1e9, -10.0..=10.0)
        .with_param_range("in_max", -1e9..=1e9, -10.0..=10.0)
}

fn field_expression() -> NodeTemplate {
    NodeTemplate::new("field.expression", "Expression Field", NodeCategory::Field)
        .with_output(field_output())
        .with_param(string_parameter("expression", ""))
        .with_param(float_parameter("default", 0.0))
        .with_param_range("default", -1e9..=1e9, -10.0..=10.0)
}

fn field_binary(type_key: &str, label: &str) -> NodeTemplate {
    NodeTemplate::new(type_key, label, NodeCategory::Field)
        .with_input(field_input("left"))
        .with_input(field_input("right"))
        .with_output(field_output())
}

fn field_blend() -> NodeTemplate {
    field_binary("field.blend", "Field Blend")
        .with_param(float_parameter("amount", 0.5))
        .with_param_range("amount", 0.0..=1.0, 0.0..=1.0)
}

/// Single-component selectors offered by `field.attribute` and
/// `field.component`, which double as the input port names of
/// `field.compose`.
pub const FIELD_COMPONENTS: [&str; 4] = ["x", "y", "z", "w"];

/// `type_key` of the 2-component `field.compose` template.
pub const FIELD_COMPOSE_VEC2: &str = "field.compose.vec2";
/// `type_key` of the 3-component `field.compose` template.
pub const FIELD_COMPOSE_VEC3: &str = "field.compose.vec3";
/// `type_key` of the 4-component `field.compose` template.
pub const FIELD_COMPOSE_VEC4: &str = "field.compose.vec4";

/// `field.length`: a vector field's magnitude, as a scalar field.
///
/// One `type_key` covers every arity. Unlike the value-domain
/// `vector.normalize`, arity cannot be read off a port here — every field
/// wire is `FIELD` whatever the field samples to — so the only reason to
/// split would be the *number of ports*, and this node has one of each.
fn field_length() -> NodeTemplate {
    NodeTemplate::new("field.length", "Field Length", NodeCategory::Field)
        .with_input(field_input("field"))
        .with_output(field_output())
}

/// `field.angle`: the direction of a vector field as radians, `atan2(y, x)`.
///
/// The bridge from a direction field to `rot`, which is an F32 and cannot
/// take a vector (`vector-field-plan.md`: no implicit conversion, an explicit
/// node instead).
fn field_angle() -> NodeTemplate {
    NodeTemplate::new("field.angle", "Field Angle", NodeCategory::Field)
        .with_input(field_input("field"))
        .with_output(field_output())
}

/// `field.component`: one component of a vector field, as a scalar field.
fn field_component() -> NodeTemplate {
    NodeTemplate::new("field.component", "Field Component", NodeCategory::Field)
        .with_input(field_input("field"))
        .with_output(field_output())
        .with_param(string_parameter("component", "x"))
        .with_param_options("component", FIELD_COMPONENTS)
}

/// One arity of `field.compose`: scalar fields in, a vector field out.
///
/// Arity is a separate `type_key` because the **number of input ports**
/// follows it and port lists live on the node instance — the same reason
/// `vector.split` splits, only on the input side. The output port is `FIELD`
/// either way, so nothing about the wire type forces the split.
fn field_compose(type_key: &str, label: &str, components: usize) -> NodeTemplate {
    let mut template =
        NodeTemplate::new(type_key, label, NodeCategory::Field).with_output(field_output());
    for key in &FIELD_COMPONENTS[..components] {
        template = template.with_input(field_input(key));
    }
    template
}

fn field_attribute() -> NodeTemplate {
    NodeTemplate::new("field.attribute", "Attribute Field", NodeCategory::Field)
        .with_output(field_output())
        .with_param(string_parameter("name", "index"))
        .with_param(string_parameter("component", "x"))
        .with_param_options("component", FIELD_COMPONENTS)
        .with_param(Parameter {
            key: "normalize".into(),
            value: ParameterValue::Bool(false),
        })
        .with_param(float_parameter("default", 0.0))
        .with_param_range("default", -1e9..=1e9, -10.0..=10.0)
}

/// How `field.apply` combines a sampled value with the existing one.
/// Mirrors [`crate::geometry::CombineMode`].
pub const FIELD_COMBINE_MODES: [&str; 5] = ["set", "add", "multiply", "min", "max"];

fn field_apply() -> NodeTemplate {
    NodeTemplate::new("field.apply", "Apply Field", NodeCategory::Field)
        .with_input(geometry_input("geometry"))
        .with_input(field_input("field"))
        .with_output(geometry_output())
        .with_param(string_parameter("domain", "point"))
        .with_param(string_parameter("target", "value"))
        .with_param(float_parameter("amount", 1.0))
        .with_param_range("amount", -10.0..=10.0, 0.0..=1.0)
        .with_param(string_parameter("combine", "set"))
        .with_param_options("combine", FIELD_COMBINE_MODES)
        // Empty selects every component; "xy" / "rgb" / "a" narrow it.
        .with_param(string_parameter("components", ""))
        // Empty affects every element; otherwise the name of a Bool attribute.
        .with_param(string_parameter("group", ""))
        // On by default: a target the geometry does not carry yet is created
        // with the reserved attribute's declared type and default rather than
        // failing (`stroke_color` / `stroke_width` start out absent).
        .with_param(Parameter {
            key: "create_if_missing".into(),
            value: ParameterValue::Bool(true),
        })
}

fn rasterize() -> NodeTemplate {
    NodeTemplate::new("rasterize", "Rasterize", NodeCategory::Image)
        .with_input(InputPort {
            name: "geometry".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
            is_variadic: false,
        })
        // Pre-exposed parameter port: the evaluator overlays a connected
        // color onto the `color` parameter (attribute > pin > parameter,
        // REQ-LAYER-008 — the priority rule this node pioneered, now served
        // by the general parameter-port mechanism).
        .with_input(InputPort {
            name: "color".into(),
            accepted_types: vec![DataTypeId::COLOR],
            is_param: true,
            is_variadic: false,
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
    NodeTemplate::new("constant", "Constant", NodeCategory::Utility)
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

/// `type_key` of the 2-component vector constant template.
pub const CONSTANT_VEC2: &str = "constant.vec2";
/// `type_key` of the 3-component vector constant template.
pub const CONSTANT_VEC3: &str = "constant.vec3";
/// `type_key` of the 4-component vector constant template.
pub const CONSTANT_VEC4: &str = "constant.vec4";

/// One animatable vector constant. The arity is part of the type key because
/// the output port type is stored on each node instance.
fn constant_vector(
    type_key: &str,
    label: &str,
    data_type: DataTypeId,
    value: ParameterValue,
) -> NodeTemplate {
    NodeTemplate::new(type_key, label, NodeCategory::Utility)
        .with_output(OutputPort {
            name: "value".into(),
            data_type,
        })
        .with_param(Parameter {
            key: "value".into(),
            value,
        })
        .with_param_range("value", -1e9..=1e9, -10.0..=10.0)
}

/// `type_key` of the 2-component `vector.construct` template.
pub const VECTOR_CONSTRUCT_VEC2: &str = "vector.construct.vec2";
/// `type_key` of the 3-component `vector.construct` template.
pub const VECTOR_CONSTRUCT_VEC3: &str = "vector.construct.vec3";
/// `type_key` of the 4-component `vector.construct` template.
pub const VECTOR_CONSTRUCT_VEC4: &str = "vector.construct.vec4";

/// Component parameter keys of the `vector.construct` templates, in order.
/// A template of arity *n* declares the first *n* of these.
pub const VECTOR_COMPONENT_KEYS: [&str; 4] = ["x", "y", "z", "w"];

/// One arity of `vector.construct`: Scalar components in, one vector out.
///
/// Arity is a separate `type_key` rather than a `type` parameter because port
/// types are stored on the node instance, so switching arity in place would
/// have to retype the output port and reconcile its edges. That machinery
/// belongs to network-interface editing; the vector constant nodes split by
/// arity for the same reason.
fn vector_construct(
    type_key: &str,
    label: &str,
    data_type: DataTypeId,
    components: usize,
) -> NodeTemplate {
    let mut template =
        NodeTemplate::new(type_key, label, NodeCategory::Utility).with_output(OutputPort {
            name: "vector".into(),
            data_type,
        });
    // Components are Float parameters with no fixed input ports, like
    // `math.scalar`: editable in Properties while unconnected and drivable
    // through an exposed parameter port once connected.
    for key in &VECTOR_COMPONENT_KEYS[..components] {
        template = template
            .with_param(float_parameter(key, 0.0))
            .with_param_range(*key, -1e9..=1e9, -10.0..=10.0);
    }
    template
}

/// `type_key` of the 2-component `vector.split` template.
pub const VECTOR_SPLIT_VEC2: &str = "vector.split.vec2";
/// `type_key` of the 3-component `vector.split` template.
pub const VECTOR_SPLIT_VEC3: &str = "vector.split.vec3";
/// `type_key` of the 4-component `vector.split` template.
pub const VECTOR_SPLIT_VEC4: &str = "vector.split.vec4";

/// An input port that accepts exactly one value-domain vector arity.
fn vector_input(name: &str, data_type: DataTypeId) -> InputPort {
    InputPort {
        name: name.into(),
        accepted_types: vec![data_type],
        is_param: false,
        is_variadic: false,
    }
}

/// An input port that accepts any value-domain vector arity. Used where the
/// output type does not depend on the input arity (`vector.length`) or where
/// the node reports a mismatch itself (`vector.dot`, `vector.swizzle`).
fn any_vector_input(name: &str) -> InputPort {
    InputPort {
        name: name.into(),
        accepted_types: vec![DataTypeId::VEC2, DataTypeId::VEC3, DataTypeId::VEC4],
        is_param: false,
        is_variadic: false,
    }
}

/// One arity of `vector.split`: one vector in, its components out as
/// separate Scalar ports (a multi-output node, so its value is a
/// [`crate::types::PortRecord`] in port order).
///
/// Arity is a separate `type_key` for the same reason as `vector.construct`,
/// only more so: here the *number of output ports* follows the arity, and
/// port lists live on the node instance. Changing arity in place would have
/// to add or remove output ports and re-index the edges behind them
/// (`Graph::insert_output_port` / `remove_output_port`), which is
/// network-interface editing, not a parameter edit.
fn vector_split(
    type_key: &str,
    label: &str,
    data_type: DataTypeId,
    components: usize,
) -> NodeTemplate {
    let mut template = NodeTemplate::new(type_key, label, NodeCategory::Utility)
        .with_input(vector_input("vector", data_type));
    for key in &VECTOR_COMPONENT_KEYS[..components] {
        template = template.with_output(OutputPort {
            name: (*key).into(),
            data_type: DataTypeId::SCALAR,
        });
    }
    template
}

/// `type_key` of the 2-component `vector.swizzle` template.
pub const VECTOR_SWIZZLE_VEC2: &str = "vector.swizzle.vec2";
/// `type_key` of the 3-component `vector.swizzle` template.
pub const VECTOR_SWIZZLE_VEC3: &str = "vector.swizzle.vec3";
/// `type_key` of the 4-component `vector.swizzle` template.
pub const VECTOR_SWIZZLE_VEC4: &str = "vector.swizzle.vec4";

/// Parameter key of the `vector.swizzle` component pattern.
pub const VECTOR_SWIZZLE_PATTERN: &str = "pattern";

/// One *output* arity of `vector.swizzle`: any vector in, a reordered vector
/// of this template's arity out.
///
/// The pattern is a free string (`"xy"`, `"zyx"`, `"xxx"`), so its length
/// would otherwise decide the output type — but the output port's type lives
/// on the node instance, so the arity has to be fixed by the `type_key` and
/// the pattern has to match it. The input accepts every arity because the
/// *input* type is not what the port declaration pins down; a pattern naming
/// a component the wired vector does not have is an evaluation error.
fn vector_swizzle(
    type_key: &str,
    label: &str,
    data_type: DataTypeId,
    components: usize,
) -> NodeTemplate {
    NodeTemplate::new(type_key, label, NodeCategory::Utility)
        .with_input(any_vector_input("vector"))
        .with_output(OutputPort {
            name: "vector".into(),
            data_type,
        })
        .with_param(string_parameter(
            VECTOR_SWIZZLE_PATTERN,
            &VECTOR_COMPONENT_KEYS[..components].concat(),
        ))
}

/// `type_key` of the vector magnitude template.
pub const VECTOR_LENGTH: &str = "vector.length";
/// `type_key` of the 2-component `vector.normalize` template.
pub const VECTOR_NORMALIZE_VEC2: &str = "vector.normalize.vec2";
/// `type_key` of the 3-component `vector.normalize` template.
pub const VECTOR_NORMALIZE_VEC3: &str = "vector.normalize.vec3";
/// `type_key` of the 4-component `vector.normalize` template.
pub const VECTOR_NORMALIZE_VEC4: &str = "vector.normalize.vec4";
/// `type_key` of the dot-product template.
pub const VECTOR_DOT: &str = "vector.dot";
/// `type_key` of the 2D cross-product template (a scalar).
pub const VECTOR_CROSS_VEC2: &str = "vector.cross.vec2";
/// `type_key` of the 3D cross-product template (a vector).
pub const VECTOR_CROSS_VEC3: &str = "vector.cross.vec3";

/// `vector.length`: magnitude of a vector of any arity.
///
/// One `type_key` covers every arity because the output is a Scalar whatever
/// goes in — the arity split the other vector nodes need exists only where
/// the *output* port type follows the input.
fn vector_length() -> NodeTemplate {
    NodeTemplate::new(VECTOR_LENGTH, "Vector Length", NodeCategory::Utility)
        .with_input(any_vector_input("vector"))
        .with_output(OutputPort {
            name: "length".into(),
            data_type: DataTypeId::SCALAR,
        })
}

/// One arity of `vector.normalize`: the input scaled to unit length.
///
/// The output arity is the input arity, so the port declares exactly one type
/// and the node editor offers no mismatched connection. The processor still
/// reports one, because `Graph::add_edge` itself does not type-check.
fn vector_normalize(type_key: &str, label: &str, data_type: DataTypeId) -> NodeTemplate {
    NodeTemplate::new(type_key, label, NodeCategory::Utility)
        .with_input(vector_input("vector", data_type))
        .with_output(OutputPort {
            name: "vector".into(),
            data_type,
        })
}

/// `vector.dot`: the dot product of two vectors of the same arity.
///
/// Both inputs accept every arity — the output is a Scalar regardless — so
/// mixing arities is a connection the editor allows, and the processor
/// reports it when the node evaluates.
fn vector_dot() -> NodeTemplate {
    NodeTemplate::new(VECTOR_DOT, "Vector Dot", NodeCategory::Utility)
        .with_input(any_vector_input("a"))
        .with_input(any_vector_input("b"))
        .with_output(OutputPort {
            name: "dot".into(),
            data_type: DataTypeId::SCALAR,
        })
}

/// One arity of `vector.cross`. The output type differs per arity — the 2D
/// cross product is the scalar `ax·by - ay·bx`, the 3D one a vector — so the
/// two are separate templates rather than one polymorphic node, and there is
/// no 4-component cross product to declare.
fn vector_cross(
    type_key: &str,
    label: &str,
    data_type: DataTypeId,
    output: &str,
    output_type: DataTypeId,
) -> NodeTemplate {
    NodeTemplate::new(type_key, label, NodeCategory::Utility)
        .with_input(vector_input("a", data_type))
        .with_input(vector_input("b", data_type))
        .with_output(OutputPort {
            name: output.into(),
            data_type: output_type,
        })
}

fn media() -> NodeTemplate {
    NodeTemplate::new("media", "Media", NodeCategory::Image)
        .with_output(OutputPort {
            name: "frame".into(),
            data_type: DataTypeId::FRAME_BUFFER,
        })
        .with_param(string_parameter("asset_id", ""))
}

fn subnet() -> NodeTemplate {
    // Pins are dynamic: the inner net.in / net.out definitions become the
    // node's ports (REQ-LAYER-003), so the template declares none.
    // `NodeTemplate::create_node` builds that inner pair and derives the pins
    // from it — a subnet is the one node whose interface the template cannot
    // state.
    NodeTemplate::new(
        crate::network::SUBNET_TYPE_KEY,
        "Subnet",
        NodeCategory::Utility,
    )
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
    NodeTemplate::new("constant.color", "RGB Color", NodeCategory::Color)
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
    NodeTemplate::new("merge", "Merge", NodeCategory::Image)
        .with_input(InputPort {
            name: "A".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
            is_param: false,
            is_variadic: false,
        })
        .with_input(InputPort {
            name: "B".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
            is_param: false,
            is_variadic: false,
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
        // `a` starts exposed as an input port. Every op reads it — the unary
        // ones read nothing else — so a `math.scalar` that is not driven
        // through `a` is the exception, and making the common case cost a
        // right-click on a node whose whole purpose is to sit mid-chain is
        // backwards. `b` stays a plain parameter: it is the constant of a
        // binary op far more often than it is a wire.
        //
        // Declared as an input rather than exposed after creation because a
        // template cannot run graph operations — the same shape `rasterize`
        // uses for its `color` pin. `is_param` ports must follow the fixed and
        // variadic ones (see `Graph::expose_param_port`), which holds here
        // because this template declares neither. The accepted types come from
        // the parameter's own value so this port and the one
        // `expose_param_port` would push cannot drift apart.
        .with_input(InputPort {
            name: "a".into(),
            accepted_types: ParameterValue::Float(0.0).port_accepted_types(),
            is_param: true,
            is_variadic: false,
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

fn math_curve() -> NodeTemplate {
    NodeTemplate::new("math.curve", "Curve", NodeCategory::Utility)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::SCALAR,
        })
        .with_param(float_parameter("value", 0.0))
        .with_param(float_parameter("in_min", 0.0))
        .with_param(float_parameter("in_max", 1.0))
        .with_param(float_parameter("out_min", 0.0))
        .with_param(float_parameter("out_max", 1.0))
        .with_param(string_parameter("extrapolation", "clamp"))
        .with_param_options("extrapolation", ["clamp", "repeat", "extend"])
        .with_param(curve_parameter("curve", CurveParam::identity()))
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
        NodeCategory::Geometry,
    )
    .with_input(geometry_input("geometry"))
    .with_output(geometry_output())
    .with_param(channel3_parameter("translate", 0.0, 0.0, 0.0))
    // Euler angles in degrees. 2D rotation is about Z, so `(0, 0, θ)`
    // reproduces the former scalar `rotation` exactly.
    .with_param(channel3_parameter("rotation", 0.0, 0.0, 0.0))
    .with_param(channel3_parameter("scale", 1.0, 1.0, 1.0))
    .with_param(Parameter {
        key: "use_centroid".into(),
        value: ParameterValue::Bool(true),
    })
    .with_param(channel3_parameter("pivot", 0.0, 0.0, 0.0))
    .with_param_range("translate", -1e9..=1e9, -1000.0..=1000.0)
    .with_param_range("rotation", -1e9..=1e9, -360.0..=360.0)
    .with_param_range("scale", -1e9..=1e9, -10.0..=10.0)
    .with_param_range("pivot", -1e9..=1e9, -1000.0..=1000.0)
}

fn geometry_merge() -> NodeTemplate {
    NodeTemplate::new("geometry.merge", "Geometry Merge", NodeCategory::Geometry)
        .with_input(geometry_input("A"))
        .with_input(geometry_input("B"))
        .with_output(geometry_output())
}

/// `geometry.from_image`: a frame buffer as one instance stamping it.
///
/// The domain of the `type_key` is the **output** type, the way every other
/// built-in spells it (`shape.*` / `scatter.*` / `geometry.*` produce a
/// geometry, `comp.*` a frame buffer). No parameters: the rectangle is the
/// source's own pixel resolution, centred on the origin, and everything that
/// would move or resize it is an instance attribute that the geometry
/// operators already write.
fn geometry_from_image() -> NodeTemplate {
    NodeTemplate::new(
        "geometry.from_image",
        "Image to Geometry",
        NodeCategory::Geometry,
    )
    .with_input(InputPort {
        name: "image".into(),
        accepted_types: vec![DataTypeId::FRAME_BUFFER],
        is_param: false,
        is_variadic: false,
    })
    .with_output(geometry_output())
}

/// `geometry.connect`: add connectivity to a point cloud without adding
/// points.
///
/// `group` names the `Bool` column `mode = "group"` reads; the other modes
/// ignore it, the same way `attribute.set`'s typed value parameters sit
/// inert until their `type` selects them.
fn geometry_connect() -> NodeTemplate {
    NodeTemplate::new("geometry.connect", "Connect", NodeCategory::Geometry)
        .with_input(geometry_input("geometry"))
        .with_output(geometry_output())
        .with_param(string_parameter("mode", "order"))
        .with_param_options("mode", ["order", "nearest", "group"])
        .with_param(string_parameter("group", ""))
        .with_param(string_parameter("interpolation", "linear"))
        .with_param_options("interpolation", ["linear", "bezier"])
        .with_param(Parameter {
            key: "closed".into(),
            value: ParameterValue::Bool(false),
        })
}

fn scene_input(name: &str) -> InputPort {
    InputPort {
        name: name.into(),
        accepted_types: vec![DataTypeId::SCENE],
        is_param: false,
        is_variadic: false,
    }
}

fn scene_output() -> OutputPort {
    OutputPort {
        name: "scene".into(),
        data_type: DataTypeId::SCENE,
    }
}

/// `scene.add`: place one value in a scene with a 3D transform.
///
/// The `object` port accepts a geometry or another **scene** — the nesting
/// case, which is how a transform hierarchy is built. `scene` is the scene to
/// add into and may be left unconnected, so a chain of `scene.add` nodes
/// accumulates objects without a `scene.merge` between each pair.
///
/// A frame buffer is deliberately **not** accepted: an image reaches a scene
/// as a geometry carrying it as an instance source, so images and geometry
/// share one placement mechanism
/// (`docs/implementation/image-instancing-plan.md`).
///
/// The transform parameters carry the same keys as `geometry.transform`, so
/// the two read alike in Properties, and each is a `Channel3` on the unified
/// animation channel.
fn scene_add() -> NodeTemplate {
    NodeTemplate::new("scene.add", "Scene Add", NodeCategory::Scene)
        .with_input(InputPort {
            name: "object".into(),
            accepted_types: vec![DataTypeId::GEOMETRY, DataTypeId::SCENE],
            is_param: false,
            is_variadic: false,
        })
        .with_input(scene_input("scene"))
        .with_output(scene_output())
        .with_param(channel3_parameter("translate", 0.0, 0.0, 0.0))
        // Euler angles in degrees, applied Z → Y → X.
        .with_param(channel3_parameter("rotation", 0.0, 0.0, 0.0))
        .with_param(channel3_parameter("scale", 1.0, 1.0, 1.0))
        .with_param(channel3_parameter("pivot", 0.0, 0.0, 0.0))
        .with_param_range("translate", -1e9..=1e9, -1000.0..=1000.0)
        .with_param_range("rotation", -1e9..=1e9, -360.0..=360.0)
        .with_param_range("scale", -1e9..=1e9, -10.0..=10.0)
        .with_param_range("pivot", -1e9..=1e9, -1000.0..=1000.0)
}

fn scene_merge() -> NodeTemplate {
    NodeTemplate::new("scene.merge", "Scene Merge", NodeCategory::Scene)
        .with_input(scene_input("A"))
        .with_input(scene_input("B"))
        .with_output(scene_output())
}

/// `scene.camera`: a scene holding one camera and no objects.
///
/// A camera is carried inside the `Scene` value rather than in a data type of
/// its own, so `scene.merge` combines cameras and objects with one node and a
/// scene can hold several cameras for several render passes (REQ-3D-001).
///
/// `fov` applies to a perspective projection and `ortho_height` to an
/// orthographic one; the unused one is kept rather than reshaped so switching
/// `projection` back and forth does not lose an authored value (or its
/// keyframes).
fn scene_camera() -> NodeTemplate {
    NodeTemplate::new("scene.camera", "Camera", NodeCategory::Scene)
        .with_output(scene_output())
        .with_param(channel3_parameter(
            "position",
            0.0,
            0.0,
            -camera::DEFAULT_DISTANCE,
        ))
        .with_param(channel3_parameter("target", 0.0, 0.0, 0.0))
        .with_param(string_parameter(
            "projection",
            camera::PROJECTION_PERSPECTIVE,
        ))
        .with_param_options("projection", camera::PROJECTION_KINDS)
        .with_param(float_parameter("fov", camera::DEFAULT_FOV_Y_DEGREES))
        .with_param(float_parameter(
            "ortho_height",
            camera::DEFAULT_ORTHOGRAPHIC_HEIGHT,
        ))
        .with_param(float_parameter("near", camera::DEFAULT_NEAR))
        .with_param(float_parameter("far", camera::DEFAULT_FAR))
        .with_param_range("position", -1e9..=1e9, -5000.0..=5000.0)
        .with_param_range("target", -1e9..=1e9, -5000.0..=5000.0)
        // A field of view of 0 or 180 degrees has no projection.
        .with_param_range("fov", 1e-3..=179.999, 10.0..=120.0)
        .with_param_range("ortho_height", 1e-3..=1e9, 1.0..=4000.0)
        .with_param_range("near", 1e-4..=1e9, 0.1..=1000.0)
        .with_param_range("far", 1e-4..=1e9, 100.0..=20000.0)
}

fn blur() -> NodeTemplate {
    NodeTemplate::new("blur", "Blur", NodeCategory::Image)
        .with_input(InputPort {
            name: "image".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
            is_param: false,
            is_variadic: false,
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::FRAME_BUFFER,
        })
        .with_param(Parameter {
            key: "radius".into(),
            value: ParameterValue::Float(5.0),
        })
        .with_param_range("radius", 0.0..=MAX_BLUR_RADIUS, 0.0..=50.0)
}

fn transform() -> NodeTemplate {
    NodeTemplate::new("transform", "Transform", NodeCategory::Image)
        .with_input(InputPort {
            name: "image".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
            is_param: false,
            is_variadic: false,
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::FRAME_BUFFER,
        })
        .with_param(channel3_parameter("translate", 0.0, 0.0, 0.0))
        .with_param(Parameter {
            key: "rotation".into(),
            value: ParameterValue::Float(0.0),
        })
        .with_param(Parameter {
            key: "scale".into(),
            value: ParameterValue::Float(1.0),
        })
        .with_param_range("translate", -1e5..=1e5, -1000.0..=1000.0)
        .with_param_range("rotation", -36000.0..=36000.0, -360.0..=360.0)
        .with_param_range("scale", -100.0..=100.0, 0.0..=4.0)
}

fn color_correct() -> NodeTemplate {
    NodeTemplate::new("color_correct", "Color Correct", NodeCategory::Color)
        .with_input(InputPort {
            name: "image".into(),
            accepted_types: vec![DataTypeId::FRAME_BUFFER],
            is_param: false,
            is_variadic: false,
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
    NodeTemplate::new("shape.rect", "Rectangle", NodeCategory::Geometry)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(channel2_parameter("center", 0.0, 0.0))
        .with_param(Parameter {
            key: "width".into(),
            value: ParameterValue::Float(100.0),
        })
        .with_param(Parameter {
            key: "height".into(),
            value: ParameterValue::Float(100.0),
        })
        .with_param_range("center", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("width", 0.0..=1e5, 0.0..=1000.0)
        .with_param_range("height", 0.0..=1e5, 0.0..=1000.0)
}

fn shape_ellipse() -> NodeTemplate {
    NodeTemplate::new("shape.ellipse", "Ellipse", NodeCategory::Geometry)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(channel2_parameter("center", 0.0, 0.0))
        .with_param(channel2_parameter("radius", 50.0, 50.0))
        .with_param(Parameter {
            key: "segments".into(),
            value: ParameterValue::Int(32),
        })
        .with_param_range("center", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("segments", 3.0..=512.0, 3.0..=128.0)
}

fn shape_polygon() -> NodeTemplate {
    NodeTemplate::new("shape.polygon", "Polygon", NodeCategory::Geometry)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(channel2_parameter("center", 0.0, 0.0))
        .with_param(Parameter {
            key: "radius".into(),
            value: ParameterValue::Float(50.0),
        })
        .with_param(Parameter {
            key: "sides".into(),
            value: ParameterValue::Int(6),
        })
        .with_param_range("center", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("sides", 3.0..=128.0, 3.0..=32.0)
}

fn shape_star() -> NodeTemplate {
    NodeTemplate::new("shape.star", "Star", NodeCategory::Geometry)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(channel2_parameter("center", 0.0, 0.0))
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
        .with_param_range("center", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("outer_radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("inner_radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("points", 3.0..=128.0, 3.0..=32.0)
}

fn scatter_grid() -> NodeTemplate {
    NodeTemplate::new("scatter.grid", "Grid", NodeCategory::Geometry)
        .with_variadic_input_group(InputPort {
            name: "instance_source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
            is_variadic: false,
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
        .with_param(channel2_parameter("spacing", 20.0, 20.0))
        .with_param(channel2_parameter("center", 0.0, 0.0))
        .with_param(Parameter {
            key: "center_input".into(),
            value: ParameterValue::Bool(true),
        })
        .with_param(Parameter {
            key: "source_mode".into(),
            value: ParameterValue::String("sequential".into()),
        })
        .with_param_options("source_mode", ["sequential", "random"])
        .with_param(Parameter {
            key: "source_seed".into(),
            value: ParameterValue::Int(0),
        })
        // `count_x` / `count_y` stay separate Ints: `Channel2` is a pair of
        // float channels, so folding them would change what the value means.
        .with_param_range("count_x", 1.0..=1000.0, 1.0..=50.0)
        .with_param_range("count_y", 1.0..=1000.0, 1.0..=50.0)
        .with_param_range("spacing", -1e5..=1e5, 0.0..=200.0)
        .with_param_range("center", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("source_seed", 0.0..=1e9, 0.0..=1000.0)
}

fn scatter_circular() -> NodeTemplate {
    NodeTemplate::new("scatter.circular", "Circular", NodeCategory::Geometry)
        .with_variadic_input_group(InputPort {
            name: "instance_source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
            is_variadic: false,
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
        .with_param(channel2_parameter("center", 0.0, 0.0))
        .with_param(Parameter {
            key: "align_rotation".into(),
            value: ParameterValue::Bool(true),
        })
        .with_param(Parameter {
            key: "center_input".into(),
            value: ParameterValue::Bool(true),
        })
        .with_param(Parameter {
            key: "source_mode".into(),
            value: ParameterValue::String("sequential".into()),
        })
        .with_param_options("source_mode", ["sequential", "random"])
        .with_param(Parameter {
            key: "source_seed".into(),
            value: ParameterValue::Int(0),
        })
        .with_param_range("count", 1.0..=10000.0, 1.0..=100.0)
        .with_param_range("radius", 0.0..=1e5, 0.0..=500.0)
        .with_param_range("center", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("source_seed", 0.0..=1e9, 0.0..=1000.0)
}

fn scatter_path_array() -> NodeTemplate {
    NodeTemplate::new("scatter.path_array", "Path Array", NodeCategory::Geometry)
        .with_input(InputPort {
            name: "path".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
            is_variadic: false,
        })
        .with_variadic_input_group(InputPort {
            name: "instance_source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
            is_variadic: false,
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(Parameter {
            key: "count".into(),
            value: ParameterValue::Int(10),
        })
        .with_param(Parameter {
            key: "center_input".into(),
            value: ParameterValue::Bool(true),
        })
        .with_param(Parameter {
            key: "source_mode".into(),
            value: ParameterValue::String("sequential".into()),
        })
        .with_param_options("source_mode", ["sequential", "random"])
        .with_param(Parameter {
            key: "source_seed".into(),
            value: ParameterValue::Int(0),
        })
        .with_param_range("count", 1.0..=100000.0, 1.0..=100.0)
        .with_param_range("source_seed", 0.0..=1e9, 0.0..=1000.0)
}

fn scatter_scatter() -> NodeTemplate {
    NodeTemplate::new("scatter.scatter", "Scatter", NodeCategory::Geometry)
        .with_variadic_input_group(InputPort {
            name: "instance_source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
            is_variadic: false,
        })
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(Parameter {
            key: "count".into(),
            value: ParameterValue::Int(20),
        })
        .with_param(channel2_parameter("area", 200.0, 200.0))
        .with_param(channel2_parameter("center", 0.0, 0.0))
        .with_param(Parameter {
            key: "seed".into(),
            value: ParameterValue::Int(0),
        })
        .with_param(Parameter {
            key: "center_input".into(),
            value: ParameterValue::Bool(true),
        })
        .with_param(Parameter {
            key: "source_mode".into(),
            value: ParameterValue::String("sequential".into()),
        })
        .with_param_options("source_mode", ["sequential", "random"])
        .with_param(Parameter {
            key: "source_seed".into(),
            value: ParameterValue::Int(0),
        })
        .with_param_range("count", 0.0..=100000.0, 0.0..=500.0)
        .with_param_range("area", 0.0..=1e5, 0.0..=2000.0)
        .with_param_range("center", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("seed", 0.0..=1e9, 0.0..=1000.0)
        .with_param_range("source_seed", 0.0..=1e9, 0.0..=1000.0)
}

/// `shape.line`: one open path from `start` to `end`.
///
/// `segments` is the number of edges, so the path carries `segments + 1`
/// points and the intermediate ones sit at even parameter steps — they exist
/// to give `field.apply` something to modulate along the line.
fn shape_line() -> NodeTemplate {
    NodeTemplate::new("shape.line", "Line", NodeCategory::Geometry)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(channel2_parameter("start", -50.0, 0.0))
        .with_param(channel2_parameter("end", 50.0, 0.0))
        .with_param(int_parameter("segments", 1))
        .with_param_range("start", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("end", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("segments", 1.0..=1000.0, 1.0..=64.0)
}

/// `shape.grid`: a lattice of open paths — `rows` horizontal lines and
/// `columns` vertical ones, so the geometry carries `rows + columns`
/// primitives.
///
/// The lines are the point of the node: for a lattice of loose *points* use
/// `scatter.grid`, which places instances instead of paths.
///
/// `rows` / `columns` stay separate `Int`s for the same reason
/// `scatter.grid`'s counts do: `Channel2` is a pair of float channels.
fn shape_grid() -> NodeTemplate {
    NodeTemplate::new("shape.grid", "Grid Lines", NodeCategory::Geometry)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        .with_param(channel2_parameter("center", 0.0, 0.0))
        .with_param(channel2_parameter("size", 100.0, 100.0))
        .with_param(int_parameter("rows", 3))
        .with_param(int_parameter("columns", 3))
        .with_param_range("center", -1e5..=1e5, -2000.0..=2000.0)
        .with_param_range("size", 0.0..=1e5, 0.0..=1000.0)
        .with_param_range("rows", 2.0..=1000.0, 2.0..=50.0)
        .with_param_range("columns", 2.0..=1000.0, 2.0..=50.0)
}

fn shape_custom_path() -> NodeTemplate {
    NodeTemplate::new("shape.custom_path", "Custom Path", NodeCategory::Geometry)
        .with_output(OutputPort {
            name: "output".into(),
            data_type: DataTypeId::GEOMETRY,
        })
        // Pen-tool output (REQ-UI-011): control points with bezier tangent
        // offsets (zero tangent = corner). Read-only in Properties.
        .with_param(Parameter {
            key: "points".into(),
            value: ParameterValue::PathPoints(Vec::new()),
        })
        .with_param(Parameter {
            key: "closed".into(),
            value: ParameterValue::Bool(false),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::channel::ChannelSource;

    /// The constant value of a template-declared channel. Template defaults
    /// are always constants, so anything else is a declaration bug.
    fn constant_of(channel: &AnimationChannel) -> f32 {
        match channel.source {
            ChannelSource::Constant(v) => v,
            ref other => panic!("template default is not a constant: {other:?}"),
        }
    }

    /// Every component value a parameter declares, in order.
    fn default_components(value: &ParameterValue) -> Vec<f32> {
        match value {
            ParameterValue::Float(v) => vec![*v],
            ParameterValue::Int(v) => vec![*v as f32],
            ParameterValue::Channel(ch) => vec![constant_of(ch)],
            ParameterValue::Channel2(chs) => chs.iter().map(constant_of).collect(),
            ParameterValue::Channel3(chs) => chs.iter().map(constant_of).collect(),
            ParameterValue::Channel4(chs) => chs.iter().map(constant_of).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn register_all_builtins() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        assert_eq!(reg.all_templates().count(), 75);
    }

    #[test]
    fn builtins_cover_expected_categories() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
<<<<<<< HEAD
        assert_eq!(reg.list_by_category(NodeCategory::Geometry).len(), 22);
=======
        assert_eq!(reg.list_by_category(NodeCategory::Geometry).len(), 20);
>>>>>>> 92191d6 (feat: add geometry.from_image wrapping a frame buffer as an instance source)
        assert_eq!(reg.list_by_category(NodeCategory::Scene).len(), 3);
        assert_eq!(reg.list_by_category(NodeCategory::Field).len(), 17);
        assert_eq!(reg.list_by_category(NodeCategory::Image).len(), 5);
        assert_eq!(reg.list_by_category(NodeCategory::Color).len(), 2);
        assert_eq!(reg.list_by_category(NodeCategory::Time).len(), 0);
        assert_eq!(reg.list_by_category(NodeCategory::Utility).len(), 25);
    }

    /// Each `field.compose` arity declares one `FIELD` input per component,
    /// named after it, and one `FIELD` output. The port *count* is what the
    /// arity decides here — a Vec2 compose reading three inputs would build a
    /// Vec2 out of two of them and silently drop the third.
    #[test]
    fn field_compose_arities_declare_one_input_per_component() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        for (type_key, components) in [
            (FIELD_COMPOSE_VEC2, 2),
            (FIELD_COMPOSE_VEC3, 3),
            (FIELD_COMPOSE_VEC4, 4),
        ] {
            let t = reg.get(type_key).unwrap_or_else(|| panic!("{type_key}"));
            let names: Vec<&str> = t.inputs.iter().map(|p| p.name.as_str()).collect();
            assert_eq!(names, FIELD_COMPONENTS[..components], "{type_key}");
            for port in &t.inputs {
                assert_eq!(port.accepted_types, [DataTypeId::FIELD], "{type_key}");
            }
            assert_eq!(t.outputs.len(), 1, "{type_key}");
            assert_eq!(t.outputs[0].data_type, DataTypeId::FIELD, "{type_key}");
        }
    }

    /// The scalar-answering transforms take one field and give one back, and
    /// `field.component` offers a closed set of component names rather than a
    /// free string.
    #[test]
    fn field_transforms_declare_one_field_in_and_one_out() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        for type_key in ["field.length", "field.angle", "field.component"] {
            let t = reg.get(type_key).unwrap_or_else(|| panic!("{type_key}"));
            assert_eq!(t.inputs.len(), 1, "{type_key}");
            assert_eq!(
                t.inputs[0].accepted_types,
                [DataTypeId::FIELD],
                "{type_key}"
            );
            assert_eq!(t.outputs.len(), 1, "{type_key}");
            assert_eq!(t.outputs[0].data_type, DataTypeId::FIELD, "{type_key}");
        }
        let component = reg.get("field.component").expect("field.component");
        assert_eq!(
            component.param_options.get("component").map(Vec::as_slice),
            Some(&FIELD_COMPONENTS.map(String::from)[..]),
        );
    }

    /// Each `vector.construct` arity outputs its own vector type and declares
    /// exactly its component parameters — a Vec2 must not carry a `z`.
    #[test]
    fn vector_construct_arities_declare_their_components() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        for (type_key, data_type, components) in [
            (VECTOR_CONSTRUCT_VEC2, DataTypeId::VEC2, 2),
            (VECTOR_CONSTRUCT_VEC3, DataTypeId::VEC3, 3),
            (VECTOR_CONSTRUCT_VEC4, DataTypeId::VEC4, 4),
        ] {
            let t = reg.get(type_key).unwrap_or_else(|| panic!("{type_key}"));
            assert!(t.inputs.is_empty(), "{type_key} declares no fixed inputs");
            assert_eq!(t.outputs.len(), 1, "{type_key}");
            assert_eq!(t.outputs[0].data_type, data_type, "{type_key}");
            let keys: Vec<&str> = t.default_params.iter().map(|p| p.key.as_str()).collect();
            assert_eq!(keys, VECTOR_COMPONENT_KEYS[..components], "{type_key}");
            for key in keys {
                assert!(
                    matches!(
                        t.default_params
                            .iter()
                            .find(|p| p.key == key)
                            .map(|p| &p.value),
                        Some(ParameterValue::Float(v)) if *v == 0.0
                    ),
                    "{type_key} {key} defaults to Float(0.0)"
                );
                assert!(
                    reg.param_range(type_key, key).is_some(),
                    "{type_key} {key} has an editing range"
                );
            }
        }
    }

    /// Each `vector.split` arity takes exactly its own vector type and
    /// declares one Scalar output per component, named for that component.
    #[test]
    fn vector_split_arities_declare_one_output_per_component() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        for (type_key, data_type, components) in [
            (VECTOR_SPLIT_VEC2, DataTypeId::VEC2, 2),
            (VECTOR_SPLIT_VEC3, DataTypeId::VEC3, 3),
            (VECTOR_SPLIT_VEC4, DataTypeId::VEC4, 4),
        ] {
            let t = reg.get(type_key).unwrap_or_else(|| panic!("{type_key}"));
            assert_eq!(t.inputs.len(), 1, "{type_key}");
            assert_eq!(t.inputs[0].accepted_types, [data_type], "{type_key}");
            let names: Vec<&str> = t.outputs.iter().map(|p| p.name.as_str()).collect();
            assert_eq!(names, VECTOR_COMPONENT_KEYS[..components], "{type_key}");
            assert!(
                t.outputs.iter().all(|p| p.data_type == DataTypeId::SCALAR),
                "{type_key} splits into scalars"
            );
            assert!(t.default_params.is_empty(), "{type_key} has no parameters");
        }
    }

    /// `vector.swizzle` fixes its *output* arity in the `type_key` and seeds
    /// the pattern with the identity for that arity, so a freshly placed node
    /// evaluates. The input accepts every arity.
    #[test]
    fn vector_swizzle_arities_seed_the_identity_pattern() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        for (type_key, data_type, pattern) in [
            (VECTOR_SWIZZLE_VEC2, DataTypeId::VEC2, "xy"),
            (VECTOR_SWIZZLE_VEC3, DataTypeId::VEC3, "xyz"),
            (VECTOR_SWIZZLE_VEC4, DataTypeId::VEC4, "xyzw"),
        ] {
            let t = reg.get(type_key).unwrap_or_else(|| panic!("{type_key}"));
            assert_eq!(t.inputs.len(), 1, "{type_key}");
            assert_eq!(
                t.inputs[0].accepted_types,
                [DataTypeId::VEC2, DataTypeId::VEC3, DataTypeId::VEC4],
                "{type_key} takes any arity"
            );
            assert_eq!(t.outputs.len(), 1, "{type_key}");
            assert_eq!(t.outputs[0].data_type, data_type, "{type_key}");
            assert!(
                matches!(
                    t.default_params
                        .iter()
                        .find(|p| p.key == VECTOR_SWIZZLE_PATTERN)
                        .map(|p| &p.value),
                    Some(ParameterValue::String(p)) if p == pattern
                ),
                "{type_key} defaults to {pattern:?}"
            );
        }
    }

    /// `math.scalar` ships with `a` already exposed. Pinned two ways because
    /// the template writes the port by hand: it has to be the *only* input, so
    /// the `is_param` ports still come last, and it has to be byte-identical to
    /// what `expose_param_port` would have pushed — a port that differs is one
    /// the graph would refuse to re-create after a remove.
    #[test]
    fn math_scalar_ships_with_a_exposed() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        let id = crate::id::NodeId::new(1);
        let node = reg.create_node("math.scalar", id).unwrap();
        assert_eq!(node.inputs.len(), 1, "`a` is the only input");
        assert_eq!(node.inputs[0].name, "a");
        assert!(node.inputs[0].is_param);

        // The same node with the port removed and re-exposed the ordinary way
        // must be indistinguishable from the one the template built.
        let graph = crate::graph::Graph::new()
            .add_node(node.clone())
            .expect("a fresh graph accepts the node")
            .remove_param_port(id, "a")
            .expect("the template's port is removable")
            .expose_param_port(id, "a")
            .expect("and re-exposable");
        assert_eq!(graph.node(id).unwrap().inputs, node.inputs);
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
    fn math_curve_declares_its_curve_and_extrapolation_options() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        let template = reg.get("math.curve").unwrap();
        assert!(matches!(
            template
                .default_params
                .iter()
                .find(|parameter| parameter.key == "curve")
                .map(|parameter| &parameter.value),
            Some(ParameterValue::Curve(_))
        ));
        assert_eq!(
            reg.param_options("math.curve", "extrapolation").unwrap(),
            ["clamp", "repeat", "extend"]
        );
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
    fn vector_constant_arities_declare_their_value_channels() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        for (type_key, data_type, expected) in [
            (CONSTANT_VEC2, DataTypeId::VEC2, vec![0.0, 0.0]),
            (CONSTANT_VEC3, DataTypeId::VEC3, vec![0.0, 0.0, 0.0]),
            (CONSTANT_VEC4, DataTypeId::VEC4, vec![0.0, 0.0, 0.0, 0.0]),
        ] {
            let template = reg.get(type_key).unwrap_or_else(|| panic!("{type_key}"));
            assert!(template.inputs.is_empty(), "{type_key} declares no inputs");
            assert_eq!(template.outputs.len(), 1, "{type_key}");
            assert_eq!(template.outputs[0].data_type, data_type, "{type_key}");
            assert_eq!(template.default_params.len(), 1, "{type_key}");
            assert_eq!(template.default_params[0].key, "value");
            assert_eq!(
                default_components(&template.default_params[0].value),
                expected
            );
            assert!(template.param_range("value").is_some(), "{type_key}.value");
        }
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
    fn scatter_templates_enable_center_input_by_default() {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);

        for type_key in [
            "scatter.grid",
            "scatter.circular",
            "scatter.path_array",
            "scatter.scatter",
        ] {
            let template = registry.get(type_key).unwrap();
            let center_input = template
                .default_params
                .iter()
                .find(|parameter| parameter.key == "center_input")
                .unwrap_or_else(|| panic!("{type_key} missing center_input"));
            assert_eq!(center_input.value, ParameterValue::Bool(true));
            assert!(template.param_range("center_input").is_none());

            let source_mode = template
                .default_params
                .iter()
                .find(|parameter| parameter.key == "source_mode")
                .unwrap_or_else(|| panic!("{type_key} missing source_mode"));
            assert_eq!(
                source_mode.value,
                ParameterValue::String("sequential".into())
            );
            assert_eq!(
                template.param_option_values("source_mode").unwrap(),
                ["sequential", "random"]
            );

            let source_seed = template
                .default_params
                .iter()
                .find(|parameter| parameter.key == "source_seed")
                .unwrap_or_else(|| panic!("{type_key} missing source_seed"));
            assert_eq!(source_seed.value, ParameterValue::Int(0));
            assert!(template.param_range("source_seed").is_some());

            let variadic_start = template.inputs.len();
            assert_eq!(
                variadic_start,
                usize::from(type_key == "scatter.path_array")
            );
            let node = template.create_node(crate::id::NodeId::new(99));
            assert_eq!(node.inputs.len(), variadic_start + 1);
            assert!(node.inputs[variadic_start].is_variadic);
            assert_eq!(node.inputs[variadic_start].name, "instance_source");
        }
    }

    /// Vector parameters are declared once, not once per component: a
    /// `Channel2` / `Channel3` carries a single key, a single range, and a
    /// single parameter port.
    #[test]
    fn attribute_set_value_arity_follows_the_type() {
        for (type_name, arity) in [
            ("f32", 1),
            ("vec2", 2),
            ("vec3", 3),
            ("vec4", 4),
            ("color", 4),
            // The types that read `int_value` / `bool_value` / `string_value`
            // carry `value` along as an inert single channel.
            ("i32", 1),
            ("bool", 1),
            ("string", 1),
            ("nonsense", 1),
        ] {
            assert_eq!(attribute_set_value_arity(type_name), arity, "{type_name}");
        }
        assert_eq!(attribute_set_value_defaults("color"), &[0.0, 0.0, 0.0, 1.0]);
        assert_eq!(attribute_set_value_defaults("vec4"), &[0.0, 0.0, 0.0, 0.0]);
    }

    /// Retyping keeps the components both shapes share and fills the rest from
    /// the type's defaults; widening then narrowing is not expected to restore
    /// what narrowing dropped.
    #[test]
    fn attribute_set_value_retyping_keeps_shared_components() {
        let sample = |value: &ParameterValue| -> Vec<f32> {
            value
                .channels()
                .unwrap()
                .iter()
                .map(constant_of)
                .collect::<Vec<_>>()
        };
        let scalar = ParameterValue::Float(7.0);
        let widened = attribute_set_value_for_type("vec3", &scalar).unwrap();
        assert_eq!(sample(&widened), vec![7.0, 0.0, 0.0], "x survives");
        assert!(matches!(widened, ParameterValue::Channel3(_)));

        let coloured = attribute_set_value_for_type("color", &scalar).unwrap();
        assert_eq!(
            sample(&coloured),
            vec![7.0, 0.0, 0.0, 1.0],
            "colour alpha fills from its own default"
        );

        let narrowed = attribute_set_value_for_type("f32", &widened).unwrap();
        assert_eq!(sample(&narrowed), vec![7.0]);
        assert!(matches!(narrowed, ParameterValue::Channel(_)));

        // A value with no float components cannot be reshaped.
        assert!(attribute_set_value_for_type("vec2", &ParameterValue::Bool(true)).is_none());
    }

    /// A keyframed component keeps its curve across a retype.
    #[test]
    fn attribute_set_value_retyping_preserves_keyframes() {
        use crate::animation::curve::KeyframeCurve;
        use crate::animation::interpolation::Interpolation;
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 100.0, Interpolation::Linear);
        let existing = ParameterValue::Channel(AnimationChannel::keyframes(curve));
        let ParameterValue::Channel2(chs) =
            attribute_set_value_for_type("vec2", &existing).unwrap()
        else {
            panic!("expected Channel2");
        };
        assert!(matches!(
            chs[0].source,
            crate::animation::channel::ChannelSource::Keyframes(_)
        ));
        assert_eq!(constant_of(&chs[1]), 0.0);
    }

    #[test]
    fn dependent_updates_reshape_only_attribute_set_value() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        let node = reg
            .create_node("attribute.set", crate::id::NodeId::new(1))
            .unwrap();
        let to_vec3 = Parameter {
            key: "type".into(),
            value: ParameterValue::String("vec3".into()),
        };
        let updates = dependent_param_updates(&node, &to_vec3);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].key, "value");
        assert!(matches!(updates[0].value, ParameterValue::Channel3(_)));

        // Same type: nothing to reshape.
        let to_f32 = Parameter {
            key: "type".into(),
            value: ParameterValue::String("f32".into()),
        };
        assert!(dependent_param_updates(&node, &to_f32).is_empty());
        // Another parameter of the same node, and another node type.
        let name = Parameter {
            key: "name".into(),
            value: ParameterValue::String("Cd".into()),
        };
        assert!(dependent_param_updates(&node, &name).is_empty());
        let other = reg
            .create_node("shape.rect", crate::id::NodeId::new(2))
            .unwrap();
        assert!(dependent_param_updates(&other, &to_vec3).is_empty());
    }

    #[test]
    fn vector_params_are_declared_as_channels() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        let arity = |type_key: &str, key: &str| {
            let value = &reg
                .get(type_key)
                .unwrap_or_else(|| panic!("{type_key}"))
                .default_params
                .iter()
                .find(|p| p.key == key)
                .unwrap_or_else(|| panic!("{type_key}.{key}"))
                .value;
            match value {
                ParameterValue::Channel2(chs) => chs.iter().map(constant_of).collect::<Vec<_>>(),
                ParameterValue::Channel3(chs) => chs.iter().map(constant_of).collect::<Vec<_>>(),
                other => panic!("{type_key}.{key} is {other:?}, not a vector channel"),
            }
        };
        // Defaults preserve the pre-fold behaviour: translate 0, scale 1,
        // rotation (0, 0, θ) with θ = 0, pivot 0.
        assert_eq!(
            arity("geometry.transform", "translate"),
            vec![0.0, 0.0, 0.0]
        );
        assert_eq!(arity("geometry.transform", "scale"), vec![1.0, 1.0, 1.0]);
        assert_eq!(arity("geometry.transform", "rotation"), vec![0.0, 0.0, 0.0]);
        assert_eq!(arity("geometry.transform", "pivot"), vec![0.0, 0.0, 0.0]);
        assert_eq!(arity("transform", "translate"), vec![0.0, 0.0, 0.0]);
        assert_eq!(arity("field.falloff", "center"), vec![0.0, 0.0, 0.0]);
        assert_eq!(arity("field.falloff", "direction"), vec![1.0, 0.0, 0.0]);
        assert_eq!(arity("shape.rect", "center"), vec![0.0, 0.0]);
        assert_eq!(arity("shape.ellipse", "radius"), vec![50.0, 50.0]);
        assert_eq!(arity("scatter.grid", "spacing"), vec![20.0, 20.0]);
        for type_key in [
            "shape.rect",
            "shape.ellipse",
            "shape.polygon",
            "shape.star",
            "scatter.grid",
            "scatter.circular",
            "scatter.scatter",
        ] {
            assert_eq!(arity(type_key, "center"), vec![0.0, 0.0], "{type_key}");
        }
        // No template keeps a folded component key any more.
        for tmpl in reg.all_templates() {
            for param in &tmpl.default_params {
                let folded = matches!(
                    param.key.as_str(),
                    "center_x"
                        | "center_y"
                        | "translate_x"
                        | "translate_y"
                        | "scale_x"
                        | "scale_y"
                        | "pivot_x"
                        | "pivot_y"
                        | "radius_x"
                        | "radius_y"
                        | "spacing_x"
                        | "spacing_y"
                        | "direction_x"
                        | "direction_y"
                        | "area_x"
                        | "area_y"
                );
                assert!(!folded, "{}.{} was not folded", tmpl.type_key, param.key);
            }
        }
        // `scatter.grid` counts are Int pairs and stay separate.
        assert!(matches!(
            reg.get("scatter.grid")
                .unwrap()
                .default_params
                .iter()
                .find(|p| p.key == "count_x")
                .map(|p| &p.value),
            Some(ParameterValue::Int(5))
        ));
    }

    #[test]
    fn every_numeric_param_declares_a_range() {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        for tmpl in reg.all_templates() {
            for param in &tmpl.default_params {
                let numeric = matches!(
                    param.value,
                    ParameterValue::Float(_)
                        | ParameterValue::Int(_)
                        | ParameterValue::Channel(_)
                        | ParameterValue::Channel2(_)
                        | ParameterValue::Channel3(_)
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
                let Some(range) = tmpl.param_range(&param.key) else {
                    continue;
                };
                // A vector parameter declares one range shared by every
                // component, so each component must fit it.
                for value in default_components(&param.value) {
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
