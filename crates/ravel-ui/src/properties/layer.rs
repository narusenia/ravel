// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property sections for a selected Layer, and the reverse mapping that
//! applies a field edit back onto the layer shell / its In-node custom
//! parameters (REQ-LAYER-002).

use super::{PropertyField, PropertySection, PropertyValue};
use ravel_core::animation::channel::AnimationChannel;
use ravel_core::composition::{BlendMode, Layer};
use ravel_core::eval::EvalContext;
use ravel_core::graph::ParameterValue;
use ravel_core::network as net;

/// Field-key prefix of the In node's custom parameters.
pub const CUSTOM_FIELD_PREFIX: &str = "custom.";

pub fn sections_for_layer(layer: &Layer, ctx: &EvalContext) -> Vec<PropertySection> {
    let mut sections = vec![
        info_section(layer),
        transform_section(layer, ctx),
        timing_section(layer),
        compositing_section(layer),
    ];
    if let Some(custom) = custom_parameters_section(layer, ctx) {
        sections.push(custom);
    }
    sections
}

fn info_section(layer: &Layer) -> PropertySection {
    // Layer "kinds" are creation templates (REQ-LAYER-008); at runtime a
    // layer is its network. Layers without a frame output are null layers.
    let source_type = if layer.has_frame_output() {
        format!("Network ({} nodes)", layer.network.node_count())
    } else {
        "Null".to_string()
    };

    PropertySection {
        title: "properties.section.layer".into(),
        fields: vec![
            PropertyField::String {
                key: "name".into(),
                value: layer.name.clone(),
            },
            PropertyField::ReadOnly {
                key: "source".into(),
                value: source_type,
            },
            PropertyField::ReadOnly {
                key: "id".into(),
                value: format!("{}", layer.id),
            },
        ],
    }
}

fn channel_value(ch: &AnimationChannel, frame: u64, ctx: &EvalContext) -> f32 {
    ch.evaluate(frame, ctx)
}

fn transform_section(layer: &Layer, ctx: &EvalContext) -> PropertySection {
    let t = &layer.transform;
    // Keyframes live in layer-local time; the network boundary applies
    // `start_frame` via the scoped EvalContext, so mirror that here.
    let frame = (ctx.frame as i64 - layer.start_frame).max(0) as u64;
    PropertySection {
        title: "properties.section.transform".into(),
        fields: vec![
            PropertyField::Float {
                key: "position_x".into(),
                value: channel_value(&t.position[0], frame, ctx),
                range: None,
                ui_range: Some(-2000.0..=2000.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "position_y".into(),
                value: channel_value(&t.position[1], frame, ctx),
                range: None,
                ui_range: Some(-2000.0..=2000.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "scale_x".into(),
                value: channel_value(&t.scale[0], frame, ctx) * 100.0,
                range: Some(0.0..=1000.0),
                ui_range: Some(0.0..=400.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "scale_y".into(),
                value: channel_value(&t.scale[1], frame, ctx) * 100.0,
                range: Some(0.0..=1000.0),
                ui_range: Some(0.0..=400.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "rotation".into(),
                value: channel_value(&t.rotation, frame, ctx),
                range: None,
                ui_range: Some(-360.0..=360.0),
                step: Some(0.1),
            },
            PropertyField::Float {
                key: "opacity".into(),
                value: channel_value(&layer.opacity, frame, ctx) * 100.0,
                range: Some(0.0..=100.0),
                ui_range: Some(0.0..=100.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "anchor_x".into(),
                value: channel_value(&t.anchor_point[0], frame, ctx),
                range: None,
                ui_range: Some(-2000.0..=2000.0),
                step: Some(1.0),
            },
            PropertyField::Float {
                key: "anchor_y".into(),
                value: channel_value(&t.anchor_point[1], frame, ctx),
                range: None,
                ui_range: Some(-2000.0..=2000.0),
                step: Some(1.0),
            },
        ],
    }
}

fn timing_section(layer: &Layer) -> PropertySection {
    PropertySection {
        title: "properties.section.timing".into(),
        fields: vec![
            PropertyField::Int {
                key: "start_frame".into(),
                value: layer.start_frame as i32,
                range: None,
                ui_range: Some(-600..=600),
                step: Some(1),
            },
            PropertyField::Int {
                key: "in_frame".into(),
                value: layer.in_frame as i32,
                range: Some(0..=i32::MAX),
                ui_range: Some(0..=600),
                step: Some(1),
            },
            PropertyField::Int {
                key: "out_frame".into(),
                value: layer.out_frame as i32,
                range: Some(0..=i32::MAX),
                ui_range: Some(0..=600),
                step: Some(1),
            },
            PropertyField::ReadOnly {
                key: "duration".into(),
                value: format!("{} frames", layer.duration()),
            },
        ],
    }
}

fn compositing_section(layer: &Layer) -> PropertySection {
    let blend_mode = match layer.blend_mode {
        BlendMode::Normal => "Normal",
        BlendMode::Add => "Add",
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
        BlendMode::Overlay => "Overlay",
    };

    PropertySection {
        title: "properties.section.compositing".into(),
        fields: vec![
            PropertyField::Enum {
                key: "blend_mode".into(),
                value: blend_mode.into(),
                options: vec![
                    "Normal".into(),
                    "Add".into(),
                    "Multiply".into(),
                    "Screen".into(),
                    "Overlay".into(),
                ],
            },
            PropertyField::Bool {
                key: "solo".into(),
                value: layer.solo,
            },
            PropertyField::Bool {
                key: "muted".into(),
                value: layer.muted,
            },
            PropertyField::Bool {
                key: "locked".into(),
                value: layer.locked,
            },
            PropertyField::Bool {
                key: "adjustment".into(),
                value: layer.adjustment,
            },
        ],
    }
}

/// The In node's custom parameters (custom output ports with a same-name
/// parameter), exposed for display/editing (REQ-LAYER-002). `None` when the
/// network has no In node or no custom parameters.
fn custom_parameters_section(layer: &Layer, ctx: &EvalContext) -> Option<PropertySection> {
    let in_node = net::find_in_node(&layer.network)?;
    let frame = (ctx.frame as i64 - layer.start_frame).max(0) as u64;
    let mut fields = Vec::new();
    for port in &in_node.outputs {
        if matches!(
            port.name.as_str(),
            net::PORT_BASE_GEOMETRY | net::PORT_TIME | net::PORT_SOURCE
        ) {
            continue;
        }
        let Some(param) = in_node.parameters.iter().find(|p| p.key == port.name) else {
            continue;
        };
        let key = format!("{CUSTOM_FIELD_PREFIX}{}", port.name);
        let field = match &param.value {
            ParameterValue::Float(v) => PropertyField::Float {
                key,
                value: *v,
                range: None,
                ui_range: None,
                step: None,
            },
            ParameterValue::Channel(ch) => PropertyField::Float {
                key,
                value: ch.evaluate(frame, ctx),
                range: None,
                ui_range: None,
                step: None,
            },
            ParameterValue::Int(v) => PropertyField::Int {
                key,
                value: *v,
                range: None,
                ui_range: None,
                step: None,
            },
            ParameterValue::Bool(v) => PropertyField::Bool { key, value: *v },
            ParameterValue::String(v) => PropertyField::String {
                key,
                value: v.clone(),
            },
            ParameterValue::Channel4(chs) => PropertyField::Color {
                key,
                r: chs[0].evaluate(frame, ctx),
                g: chs[1].evaluate(frame, ctx),
                b: chs[2].evaluate(frame, ctx),
                a: chs[3].evaluate(frame, ctx),
            },
            // Vec channels have no dedicated widget yet.
            ParameterValue::Channel2(_) | ParameterValue::Channel3(_) => PropertyField::ReadOnly {
                key,
                value: "(vector)".into(),
            },
        };
        fields.push(field);
    }
    if fields.is_empty() {
        return None;
    }
    Some(PropertySection {
        title: "properties.section.parameters".into(),
        fields,
    })
}

/// Apply a Properties-panel field edit to the layer (shell attributes and
/// `custom.*` In-node parameters). Returns `false` for unknown or read-only
/// keys. Transform values become constant channels (keyframe editing is
/// Phase 4).
pub fn apply_layer_field(layer: &mut Layer, key: &str, value: &PropertyValue) -> bool {
    if let Some(name) = key.strip_prefix(CUSTOM_FIELD_PREFIX) {
        return apply_custom_parameter(layer, name, value);
    }
    match (key, value) {
        ("position_x", PropertyValue::Float(v)) => {
            layer.transform.position[0] = AnimationChannel::constant(*v);
        }
        ("position_y", PropertyValue::Float(v)) => {
            layer.transform.position[1] = AnimationChannel::constant(*v);
        }
        // Scale and opacity are displayed in percent.
        ("scale_x", PropertyValue::Float(v)) => {
            layer.transform.scale[0] = AnimationChannel::constant(*v / 100.0);
        }
        ("scale_y", PropertyValue::Float(v)) => {
            layer.transform.scale[1] = AnimationChannel::constant(*v / 100.0);
        }
        ("rotation", PropertyValue::Float(v)) => {
            layer.transform.rotation = AnimationChannel::constant(*v);
        }
        ("opacity", PropertyValue::Float(v)) => {
            layer.opacity = AnimationChannel::constant((*v / 100.0).clamp(0.0, 1.0));
        }
        ("anchor_x", PropertyValue::Float(v)) => {
            layer.transform.anchor_point[0] = AnimationChannel::constant(*v);
        }
        ("anchor_y", PropertyValue::Float(v)) => {
            layer.transform.anchor_point[1] = AnimationChannel::constant(*v);
        }
        ("start_frame", PropertyValue::Int(v)) => {
            layer.start_frame = *v as i64;
        }
        // The display interval stays non-empty: `[in, out)` (REQ-LAYER-006).
        ("in_frame", PropertyValue::Int(v)) => {
            layer.in_frame = (*v.max(&0) as u64).min(layer.out_frame.saturating_sub(1));
        }
        ("out_frame", PropertyValue::Int(v)) => {
            layer.out_frame = (*v.max(&1) as u64).max(layer.in_frame + 1);
        }
        ("blend_mode", PropertyValue::String(v)) => {
            layer.blend_mode = match v.as_str() {
                "Normal" => BlendMode::Normal,
                "Add" => BlendMode::Add,
                "Multiply" => BlendMode::Multiply,
                "Screen" => BlendMode::Screen,
                "Overlay" => BlendMode::Overlay,
                _ => return false,
            };
        }
        ("solo", PropertyValue::Bool(v)) => layer.solo = *v,
        ("muted", PropertyValue::Bool(v)) => layer.muted = *v,
        ("locked", PropertyValue::Bool(v)) => layer.locked = *v,
        ("adjustment", PropertyValue::Bool(v)) => layer.adjustment = *v,
        _ => return false,
    }
    true
}

/// Update the value of the In node's custom parameter `name` inside the
/// layer's owned network. Returns `false` when the parameter is missing or
/// the value type does not fit.
fn apply_custom_parameter(layer: &mut Layer, name: &str, value: &PropertyValue) -> bool {
    let Some(in_node) = net::find_in_node(&layer.network) else {
        return false;
    };
    let mut updated = (**in_node).clone();
    let Some(param) = updated.parameters.iter_mut().find(|p| p.key == name) else {
        return false;
    };
    match (&param.value, value) {
        (ParameterValue::Float(_), PropertyValue::Float(v)) => {
            param.value = ParameterValue::Float(*v);
        }
        // Scrubbing a keyframed custom parameter flattens it to a constant
        // (keyframe editing is Phase 4).
        (ParameterValue::Channel(_), PropertyValue::Float(v)) => {
            param.value = ParameterValue::Channel(AnimationChannel::constant(*v));
        }
        (ParameterValue::Int(_), PropertyValue::Int(v)) => {
            param.value = ParameterValue::Int(*v);
        }
        (ParameterValue::Bool(_), PropertyValue::Bool(v)) => {
            param.value = ParameterValue::Bool(*v);
        }
        (ParameterValue::String(_), PropertyValue::String(v)) => {
            param.value = ParameterValue::String(v.clone());
        }
        _ => return false,
    }
    layer.network = layer
        .network
        .clone()
        .replace_node(std::sync::Arc::new(updated));
    true
}

/// The In node's id, for parameter-scoped invalidation after a `custom.*`
/// edit.
pub fn in_node_id(layer: &Layer) -> Option<ravel_core::id::NodeId> {
    net::find_in_node(&layer.network).map(|n| n.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::graph::{Graph, Node};
    use ravel_core::id::{DataTypeId, LayerId, NodeId};
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    fn test_layer() -> Layer {
        let out = Node::new(NodeId::new(1), ravel_core::network::NET_OUT_TYPE_KEY)
            .with_input(ravel_core::network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let network = Graph::new().add_node(out).unwrap();
        Layer::new(LayerId::new(1), "Test Layer", network).with_time(10, 0, 300)
    }

    #[test]
    fn sections_contains_four_groups() {
        let sections = sections_for_layer(&test_layer(), &ctx());
        assert_eq!(sections.len(), 4);
        assert_eq!(sections[0].title, "properties.section.layer");
        assert_eq!(sections[1].title, "properties.section.transform");
        assert_eq!(sections[2].title, "properties.section.timing");
        assert_eq!(sections[3].title, "properties.section.compositing");
    }

    #[test]
    fn transform_default_values() {
        let sections = sections_for_layer(&test_layer(), &ctx());
        let transform = &sections[1];
        let pos_x = transform.fields.iter().find(|f| f.key() == "position_x");
        assert!(pos_x.is_some());
        if let Some(PropertyField::Float { value, .. }) = pos_x {
            assert!((*value - 0.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn info_section_shows_source_type() {
        let sections = sections_for_layer(&test_layer(), &ctx());
        let info = &sections[0];
        let source = info.fields.iter().find(|f| f.key() == "source");
        assert!(source.is_some());
        if let Some(PropertyField::ReadOnly { value, .. }) = source {
            assert_eq!(value, "Network (1 nodes)");
        }
    }

    #[test]
    fn info_section_shows_null_for_frameless_network() {
        let layer = Layer::new(LayerId::new(9), "Null", Graph::new());
        let sections = sections_for_layer(&layer, &ctx());
        let source = sections[0].fields.iter().find(|f| f.key() == "source");
        if let Some(PropertyField::ReadOnly { value, .. }) = source {
            assert_eq!(value, "Null");
        } else {
            panic!("source field missing");
        }
    }

    #[test]
    fn transform_evaluates_in_layer_local_time() {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;

        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 1.0, Interpolation::Linear);
        let mut layer = test_layer(); // start_frame = 10
        layer.transform.position[0] = AnimationChannel::keyframes(curve);

        // Comp frame 15 → layer-local frame 5 → midpoint of the curve.
        let ctx = EvalContext::new(15, FrameRate::new(30, 1), (1920, 1080));
        let sections = sections_for_layer(&layer, &ctx);
        let pos_x = sections[1].fields.iter().find(|f| f.key() == "position_x");
        if let Some(PropertyField::Float { value, .. }) = pos_x {
            assert!((*value - 0.5).abs() < 1e-4);
        } else {
            panic!("position_x field missing");
        }
    }

    fn layer_with_custom_param() -> Layer {
        use ravel_core::id::DataTypeId;
        let in_node = Node::new(NodeId::new(10), ravel_core::network::NET_IN_TYPE_KEY)
            .with_output(
                ravel_core::network::PORT_BASE_GEOMETRY,
                DataTypeId::GEOMETRY,
            )
            .with_output(ravel_core::network::PORT_TIME, DataTypeId::SCALAR)
            .with_output("amount", DataTypeId::SCALAR)
            .with_param("amount", ParameterValue::Float(3.5));
        let out = Node::new(NodeId::new(11), ravel_core::network::NET_OUT_TYPE_KEY)
            .with_input(ravel_core::network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let network = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(out)
            .unwrap();
        Layer::new(LayerId::new(2), "Custom", network).with_time(0, 0, 300)
    }

    #[test]
    fn custom_parameters_expose_as_a_section() {
        let sections = sections_for_layer(&layer_with_custom_param(), &ctx());
        let custom = sections
            .iter()
            .find(|s| s.title == "properties.section.parameters")
            .expect("custom section present");
        match &custom.fields[..] {
            [PropertyField::Float { key, value, .. }] => {
                assert_eq!(key, "custom.amount");
                assert!((*value - 3.5).abs() < f32::EPSILON);
            }
            other => panic!("unexpected custom fields: {other:?}"),
        }
        // Fixed ports never show up as parameters.
        assert!(
            !custom
                .fields
                .iter()
                .any(|f| f.key().contains("base_geometry"))
        );
    }

    #[test]
    fn apply_layer_field_maps_shell_attributes() {
        let mut layer = test_layer();
        assert!(apply_layer_field(
            &mut layer,
            "position_x",
            &PropertyValue::Float(42.0)
        ));
        assert!(apply_layer_field(
            &mut layer,
            "scale_x",
            &PropertyValue::Float(50.0)
        ));
        assert!(apply_layer_field(
            &mut layer,
            "opacity",
            &PropertyValue::Float(25.0)
        ));
        assert!(apply_layer_field(
            &mut layer,
            "blend_mode",
            &PropertyValue::String("Multiply".into())
        ));
        assert!(apply_layer_field(
            &mut layer,
            "adjustment",
            &PropertyValue::Bool(true)
        ));

        let c = ctx();
        assert!((layer.transform.position[0].evaluate(0, &c) - 42.0).abs() < f32::EPSILON);
        assert!((layer.transform.scale[0].evaluate(0, &c) - 0.5).abs() < f32::EPSILON);
        assert!((layer.opacity.evaluate(0, &c) - 0.25).abs() < f32::EPSILON);
        assert_eq!(layer.blend_mode, BlendMode::Multiply);
        assert!(layer.adjustment);
        assert!(!apply_layer_field(
            &mut layer,
            "no_such_field",
            &PropertyValue::Float(1.0)
        ));
    }

    #[test]
    fn apply_layer_field_keeps_the_display_interval_valid() {
        let mut layer = test_layer(); // in=0, out=300
        assert!(apply_layer_field(
            &mut layer,
            "in_frame",
            &PropertyValue::Int(400)
        ));
        assert_eq!(layer.in_frame, 299, "in clamps below out");
        assert!(apply_layer_field(
            &mut layer,
            "out_frame",
            &PropertyValue::Int(0)
        ));
        assert_eq!(layer.out_frame, 300, "out clamps above in");
    }

    #[test]
    fn apply_custom_parameter_updates_the_in_node() {
        let mut layer = layer_with_custom_param();
        assert!(apply_layer_field(
            &mut layer,
            "custom.amount",
            &PropertyValue::Float(9.0)
        ));
        let in_node = ravel_core::network::find_in_node(&layer.network).unwrap();
        let value = in_node
            .parameters
            .iter()
            .find(|p| p.key == "amount")
            .and_then(|p| p.value.as_float());
        assert_eq!(value, Some(9.0));

        // Type mismatches and unknown parameters are rejected.
        assert!(!apply_layer_field(
            &mut layer,
            "custom.amount",
            &PropertyValue::Bool(true)
        ));
        assert!(!apply_layer_field(
            &mut layer,
            "custom.missing",
            &PropertyValue::Float(1.0)
        ));
    }

    #[test]
    fn timing_section_shows_start_frame() {
        let sections = sections_for_layer(&test_layer(), &ctx());
        let timing = &sections[2];
        let start = timing.fields.iter().find(|f| f.key() == "start_frame");
        if let Some(PropertyField::Int { value, .. }) = start {
            assert_eq!(*value, 10);
        }
    }
}
