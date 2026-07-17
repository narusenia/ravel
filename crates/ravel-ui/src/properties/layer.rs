// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property sections for a selected Layer, and the reverse mapping that
//! applies a field edit back onto the layer shell / its In-node custom
//! parameters (REQ-LAYER-002).

use super::{PropertyField, PropertySection, PropertyValue};
use crate::keyframes::{
    PropertyRowId, has_keyframe_at, insert_keyframe, remove_keyframe, set_channel_value,
};
use crate::panels::timeline::PropertyGroup;
use ravel_core::animation::channel::AnimationChannel;
use ravel_core::composition::{BlendMode, Layer};
use ravel_core::eval::EvalContext;
use ravel_core::graph::ParameterValue;
use ravel_core::id::NodeId;
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

/// The layer-local frame for channel display:
/// `comp_frame - start_frame + in_frame`, clamped at zero — the same formula
/// the shell processors use (REQ-LAYER-006).
fn layer_local_frame(layer: &Layer, ctx: &EvalContext) -> u64 {
    (ctx.frame as i64 - layer.start_frame + layer.in_frame as i64).max(0) as u64
}

fn transform_section(layer: &Layer, ctx: &EvalContext) -> PropertySection {
    let t = &layer.transform;
    // Keyframes live in layer-local time; mirror the shell processors'
    // `comp_frame - start_frame + in_frame` (REQ-LAYER-006).
    let frame = layer_local_frame(layer, ctx);
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
    let frame = layer_local_frame(layer, ctx);
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
/// keys.
///
/// `local_frame` is the layer-local frame the edit applies at (REQ-LAYER-006):
/// transform / opacity / channel-backed custom parameters **insert or update
/// a keyframe** there when the channel is animated, and replace the constant
/// otherwise (REQ-LAYER-004). Non-animatable fields ignore it.
pub fn apply_layer_field(
    layer: &mut Layer,
    key: &str,
    value: &PropertyValue,
    local_frame: u64,
) -> bool {
    if let Some(name) = key.strip_prefix(CUSTOM_FIELD_PREFIX) {
        return apply_custom_parameter(layer, name, value, local_frame);
    }
    // Scale and opacity are displayed in percent.
    let channel_edit: Option<(PropertyGroup, usize, f32)> = match (key, value) {
        ("position_x", PropertyValue::Float(v)) => Some((PropertyGroup::Position, 0, *v)),
        ("position_y", PropertyValue::Float(v)) => Some((PropertyGroup::Position, 1, *v)),
        ("scale_x", PropertyValue::Float(v)) => Some((PropertyGroup::Scale, 0, *v / 100.0)),
        ("scale_y", PropertyValue::Float(v)) => Some((PropertyGroup::Scale, 1, *v / 100.0)),
        ("rotation", PropertyValue::Float(v)) => Some((PropertyGroup::Rotation, 0, *v)),
        ("opacity", PropertyValue::Float(v)) => {
            Some((PropertyGroup::Opacity, 0, (*v / 100.0).clamp(0.0, 1.0)))
        }
        ("anchor_x", PropertyValue::Float(v)) => Some((PropertyGroup::AnchorPoint, 0, *v)),
        ("anchor_y", PropertyValue::Float(v)) => Some((PropertyGroup::AnchorPoint, 1, *v)),
        _ => None,
    };
    if let Some((group, component, value)) = channel_edit {
        return set_channel_value(
            layer,
            &PropertyRowId::Shell(group),
            component,
            local_frame,
            value,
        );
    }
    match (key, value) {
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

/// The animatable components backing a field key, for the key-toggle button:
/// the shell transform/opacity channels and `custom.*` In-node parameters
/// (`Float` converts to a channel on first key; `Int` / `Bool` / `String`
/// stay constant-only in v1, REQ-LAYER-004). Multi-component parameters
/// (vec/color) key all components together.
fn keyframe_components(layer: &Layer, key: &str) -> Option<(PropertyRowId, Vec<usize>)> {
    if let Some(name) = key.strip_prefix(CUSTOM_FIELD_PREFIX) {
        let in_node = net::find_in_node(&layer.network)?;
        let param = in_node.parameters.iter().find(|p| p.key == name)?;
        let count = match &param.value {
            ParameterValue::Float(_) | ParameterValue::Channel(_) => 1,
            ParameterValue::Channel2(_) => 2,
            ParameterValue::Channel3(_) => 3,
            ParameterValue::Channel4(_) => 4,
            _ => return None,
        };
        return Some((
            PropertyRowId::Network {
                node: in_node.id,
                key: name.to_string(),
            },
            (0..count).collect(),
        ));
    }
    let (group, component) = match key {
        "position_x" => (PropertyGroup::Position, 0),
        "position_y" => (PropertyGroup::Position, 1),
        "scale_x" => (PropertyGroup::Scale, 0),
        "scale_y" => (PropertyGroup::Scale, 1),
        "rotation" => (PropertyGroup::Rotation, 0),
        "opacity" => (PropertyGroup::Opacity, 0),
        "anchor_x" => (PropertyGroup::AnchorPoint, 0),
        "anchor_y" => (PropertyGroup::AnchorPoint, 1),
        _ => return None,
    };
    Some((PropertyRowId::Shell(group), vec![component]))
}

/// Whether the field's channel(s) have a keyframe at `local_frame` (all
/// components for vec/color fields). `None` when the field is not animatable.
pub fn layer_field_keyframed(layer: &Layer, key: &str, local_frame: u64) -> Option<bool> {
    let (row, components) = keyframe_components(layer, key)?;
    Some(
        components
            .iter()
            .all(|&c| has_keyframe_at(layer, &row, c, local_frame)),
    )
}

/// Toggle a keyframe at `local_frame` on the field's channel(s): inserts a
/// key holding the current value when any component lacks one, otherwise
/// removes the key from every component. Returns the new keyed state, or
/// `None` when the field is not animatable.
pub fn toggle_layer_keyframe(layer: &mut Layer, key: &str, local_frame: u64) -> Option<bool> {
    let (row, components) = keyframe_components(layer, key)?;
    if let PropertyRowId::Network { node, key } = &row {
        ensure_channel_parameter(layer, *node, key);
    }
    let keyed = components
        .iter()
        .all(|&c| has_keyframe_at(layer, &row, c, local_frame));
    if keyed {
        for c in components {
            remove_keyframe(layer, &row, c, local_frame);
        }
        Some(false)
    } else {
        for c in components {
            insert_keyframe(layer, &row, c, local_frame);
        }
        Some(true)
    }
}

/// Convert an In-node `Float` parameter to a constant channel so it can
/// carry keyframes. No-op for parameters that already are channels (or are
/// not key-editable at all).
fn ensure_channel_parameter(layer: &mut Layer, node: NodeId, key: &str) {
    let Some(node_ref) = layer.network.node(node) else {
        return;
    };
    let Some(param) = node_ref.parameters.iter().find(|p| p.key == key) else {
        return;
    };
    let ParameterValue::Float(value) = param.value else {
        return;
    };
    let mut updated = (**node_ref).clone();
    let param = updated
        .parameters
        .iter_mut()
        .find(|p| p.key == key)
        .expect("parameter checked above");
    param.value = ParameterValue::Channel(AnimationChannel::constant(value));
    layer.network = layer
        .network
        .clone()
        .replace_node(std::sync::Arc::new(updated));
}

/// Update the value of the In node's custom parameter `name` inside the
/// layer's owned network. Returns `false` when the parameter is missing or
/// the value type does not fit. Channel-backed parameters insert or update a
/// keyframe at `local_frame` instead of flattening to a constant
/// (REQ-LAYER-004).
fn apply_custom_parameter(
    layer: &mut Layer,
    name: &str,
    value: &PropertyValue,
    local_frame: u64,
) -> bool {
    let Some(in_node) = net::find_in_node(&layer.network) else {
        return false;
    };
    // Channel-backed float params route through the keyframe model.
    let is_channel = in_node
        .parameters
        .iter()
        .find(|p| p.key == name)
        .is_some_and(|p| matches!(p.value, ParameterValue::Channel(_)));
    if is_channel && let PropertyValue::Float(v) = value {
        let row = PropertyRowId::Network {
            node: in_node.id,
            key: name.to_string(),
        };
        return set_channel_value(layer, &row, 0, local_frame, *v);
    }
    let mut updated = (**in_node).clone();
    let Some(param) = updated.parameters.iter_mut().find(|p| p.key == name) else {
        return false;
    };
    match (&param.value, value) {
        (ParameterValue::Float(_), PropertyValue::Float(v)) => {
            param.value = ParameterValue::Float(*v);
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

        // Trimming the in edge shifts local time: comp 15 with in_frame 5
        // → local frame 10 → curve end (REQ-LAYER-006).
        let mut trimmed = layer.clone();
        trimmed.in_frame = 5;
        let sections = sections_for_layer(&trimmed, &ctx);
        let pos_x = sections[1].fields.iter().find(|f| f.key() == "position_x");
        if let Some(PropertyField::Float { value, .. }) = pos_x {
            assert!(
                (*value - 1.0).abs() < 1e-4,
                "trimmed local frame, got {value}"
            );
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
            &PropertyValue::Float(42.0),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "scale_x",
            &PropertyValue::Float(50.0),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "opacity",
            &PropertyValue::Float(25.0),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "blend_mode",
            &PropertyValue::String("Multiply".into()),
            0
        ));
        assert!(apply_layer_field(
            &mut layer,
            "adjustment",
            &PropertyValue::Bool(true),
            0
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
            &PropertyValue::Float(1.0),
            0
        ));
    }

    #[test]
    fn apply_layer_field_keeps_the_display_interval_valid() {
        let mut layer = test_layer(); // in=0, out=300
        assert!(apply_layer_field(
            &mut layer,
            "in_frame",
            &PropertyValue::Int(400),
            0
        ));
        assert_eq!(layer.in_frame, 299, "in clamps below out");
        assert!(apply_layer_field(
            &mut layer,
            "out_frame",
            &PropertyValue::Int(0),
            0
        ));
        assert_eq!(layer.out_frame, 300, "out clamps above in");
    }

    /// Scrubbing an animated shell channel keys it at the edit frame instead
    /// of flattening the curve (REQ-LAYER-004).
    #[test]
    fn apply_layer_field_keys_animated_channels() {
        let mut layer = test_layer();
        assert!(toggle_layer_keyframe(&mut layer, "position_x", 0).unwrap());
        assert!(apply_layer_field(
            &mut layer,
            "position_x",
            &PropertyValue::Float(50.0),
            10
        ));
        let c = ctx();
        assert!((layer.transform.position[0].evaluate(0, &c) - 0.0).abs() < f32::EPSILON);
        assert!((layer.transform.position[0].evaluate(10, &c) - 50.0).abs() < f32::EPSILON);
        assert_eq!(layer_field_keyframed(&layer, "position_x", 10), Some(true));
        assert_eq!(layer_field_keyframed(&layer, "position_x", 5), Some(false));
    }

    /// The key toggle converts a constant custom parameter to a keyframed
    /// channel, and removes it again (REQ-LAYER-002/004).
    #[test]
    fn toggle_layer_keyframe_converts_custom_float_param() {
        let mut layer = layer_with_custom_param();
        assert_eq!(
            layer_field_keyframed(&layer, "custom.amount", 0),
            Some(false)
        );
        assert_eq!(
            toggle_layer_keyframe(&mut layer, "custom.amount", 4),
            Some(true)
        );
        // Keyframed with the constant value (3.5) at frame 4.
        let in_node = ravel_core::network::find_in_node(&layer.network).unwrap();
        let param = in_node
            .parameters
            .iter()
            .find(|p| p.key == "amount")
            .unwrap();
        let ParameterValue::Channel(ch) = &param.value else {
            panic!("converted to a channel");
        };
        let c = ctx();
        assert!((ch.evaluate(4, &c) - 3.5).abs() < f32::EPSILON);
        // Scrubbing the keyframed param updates the curve, not the variant.
        assert!(apply_layer_field(
            &mut layer,
            "custom.amount",
            &PropertyValue::Float(9.0),
            4
        ));
        let in_node = ravel_core::network::find_in_node(&layer.network).unwrap();
        let param = in_node
            .parameters
            .iter()
            .find(|p| p.key == "amount")
            .unwrap();
        let ParameterValue::Channel(ch) = &param.value else {
            panic!("still a channel");
        };
        assert!((ch.evaluate(4, &c) - 9.0).abs() < f32::EPSILON);
        // Toggling off removes the last key → constant again.
        assert_eq!(
            toggle_layer_keyframe(&mut layer, "custom.amount", 4),
            Some(false)
        );
        let in_node = ravel_core::network::find_in_node(&layer.network).unwrap();
        let param = in_node
            .parameters
            .iter()
            .find(|p| p.key == "amount")
            .unwrap();
        let ParameterValue::Channel(ch) = &param.value else {
            panic!("constant channel after last key removal");
        };
        assert_eq!(
            ch.source,
            ravel_core::animation::channel::ChannelSource::Constant(9.0)
        );
        // Non-animatable fields report None.
        assert_eq!(layer_field_keyframed(&layer, "start_frame", 0), None);
        assert_eq!(toggle_layer_keyframe(&mut layer, "start_frame", 0), None);
    }

    #[test]
    fn apply_custom_parameter_updates_the_in_node() {
        let mut layer = layer_with_custom_param();
        assert!(apply_layer_field(
            &mut layer,
            "custom.amount",
            &PropertyValue::Float(9.0),
            0
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
            &PropertyValue::Bool(true),
            0
        ));
        assert!(!apply_layer_field(
            &mut layer,
            "custom.missing",
            &PropertyValue::Float(1.0),
            0
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
