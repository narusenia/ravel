// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Keyframe editing model for the timeline property tree (layer-network-model
//! plan, Phase 4; REQ-LAYER-004).
//!
//! The timeline lists, per layer, the shell channel groups (Position / Scale
//! / Rotation / Opacity) plus every **network parameter that carries
//! keyframes** — node parameters of the layer's owned network whose
//! [`ParameterValue::Channel`]…[`ParameterValue::Channel4`] components hold a
//! [`ChannelSource::Keyframes`] source. That includes the In node's custom
//! parameters and subnet-promoted parameters (both are plain node parameters
//! of the layer network).
//!
//! All editing functions take and return **layer-local frames**
//! (`comp_frame - start_frame + in_frame`, REQ-LAYER-006) and rebuild the
//! layer through the immutable graph API, so a whole edit lands in the
//! Document as one undo unit via `update_layer`.
//!
//! Removing the last keyframe of a channel reverts it to a constant holding
//! the removed key's value; a network parameter without any keyframed
//! component then drops out of the tree, mirroring the enumeration rule.

use std::sync::Arc;

use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
use ravel_core::animation::curve::KeyframeCurve;
use ravel_core::animation::interpolation::Interpolation;
use ravel_core::composition::Layer;
use ravel_core::graph::ParameterValue;
use ravel_core::id::NodeId;
use ravel_core::network as net;

use crate::panels::timeline::PropertyGroup;

/// Identity of one property-tree row: a shell channel group or a network
/// parameter (`node` id + parameter key) of the layer's owned network.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PropertyRowId {
    Shell(PropertyGroup),
    Network { node: NodeId, key: String },
}

/// One resolved property-tree row.
#[derive(Clone, Debug)]
pub struct PropertyRow {
    pub id: PropertyRowId,
    /// Display label for network rows (the In node's custom parameters show
    /// the bare key; other nodes show `"<label or type> · <key>"`). `None`
    /// for shell rows — the host localizes them (`timeline.property.*`).
    pub label: Option<String>,
    /// Per-component channel names, in component order.
    pub channel_names: Vec<String>,
}

/// The layer-local frame for comp-frame UI:
/// `comp_frame - start_frame + in_frame`, clamped at zero (REQ-LAYER-006).
pub fn layer_local_frame(layer: &Layer, comp_frame: u64) -> u64 {
    (comp_frame as i64 - layer.start_frame + layer.in_frame as i64).max(0) as u64
}

/// The comp-timeline frame a layer-local keyframe is displayed at
/// (`local - in + start`; can be negative when the key sits before `in`).
pub fn comp_frame_for_key(layer: &Layer, local_frame: u64) -> i64 {
    local_frame as i64 - layer.in_frame as i64 + layer.start_frame
}

/// The shell groups always shown in the tree, in display order.
pub const SHELL_GROUPS: [PropertyGroup; 4] = [
    PropertyGroup::Position,
    PropertyGroup::Scale,
    PropertyGroup::Rotation,
    PropertyGroup::Opacity,
];

/// The property-tree rows of a layer: the shell groups, then every network
/// parameter with at least one keyframed component (REQ-LAYER-004), ordered
/// deterministically by node id then parameter position.
pub fn property_rows(layer: &Layer) -> Vec<PropertyRow> {
    let mut rows: Vec<PropertyRow> = SHELL_GROUPS
        .iter()
        .map(|group| PropertyRow {
            id: PropertyRowId::Shell(*group),
            label: None,
            channel_names: shell_channel_names(*group)
                .iter()
                .map(|s| s.to_string())
                .collect(),
        })
        .collect();

    let mut nodes: Vec<_> = layer.network.nodes().collect();
    nodes.sort_by_key(|n| n.id);
    for node in nodes {
        for param in &node.parameters {
            let Some(names) = keyframed_channel_names(&param.value) else {
                continue;
            };
            let label = if node.type_key == net::NET_IN_TYPE_KEY {
                param.key.clone()
            } else {
                let node_label = node.metadata.label.as_deref().unwrap_or(&node.type_key);
                format!("{node_label} · {}", param.key)
            };
            rows.push(PropertyRow {
                id: PropertyRowId::Network {
                    node: node.id,
                    key: param.key.clone(),
                },
                label: Some(label),
                channel_names: names,
            });
        }
    }
    rows
}

/// The component channels of a row, in component order. Resolves regardless
/// of whether the components currently hold keyframes (first-key insertion
/// works on constant channels). `None` when the row no longer resolves
/// (node or parameter deleted).
pub fn row_channels<'a>(layer: &'a Layer, id: &PropertyRowId) -> Option<Vec<&'a AnimationChannel>> {
    match id {
        PropertyRowId::Shell(group) => Some(shell_channels(layer, *group)),
        PropertyRowId::Network { node, key } => {
            let node_ref = layer.network.node(*node)?;
            let param = node_ref.parameters.iter().find(|p| p.key == *key)?;
            channel_components(&param.value)
        }
    }
}

/// Whether the channel at `component` has a keyframe exactly at `frame`.
pub fn has_keyframe_at(layer: &Layer, id: &PropertyRowId, component: usize, frame: u64) -> bool {
    let Some(channels) = row_channels(layer, id) else {
        return false;
    };
    let Some(channel) = channels.get(component) else {
        return false;
    };
    match &channel.source {
        ChannelSource::Keyframes(curve) => curve.keyframes().iter().any(|k| k.frame == frame),
        _ => false,
    }
}

/// Insert (or overwrite) a keyframe at `frame` holding the channel's current
/// value at `frame`. A constant channel is converted to keyframes, keeping
/// its value as the curve's default. Returns `false` when the row or
/// component does not resolve.
pub fn insert_keyframe(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    frame: u64,
) -> bool {
    mutate_channel(layer, id, component, |channel| {
        let value = channel_value(channel, frame);
        match &mut channel.source {
            ChannelSource::Constant(v) => {
                let mut curve = KeyframeCurve::with_default(*v);
                curve.insert(frame, value, Interpolation::Linear);
                channel.source = ChannelSource::Keyframes(curve);
            }
            ChannelSource::Keyframes(curve) => {
                set_curve_value(curve, frame, value);
            }
            // Expressions / node-output bindings / blends are not key-editable.
            _ => return false,
        }
        true
    })
}

/// Remove the keyframe at `frame`. When the curve becomes empty the channel
/// reverts to a constant holding the removed key's value (a fully constant
/// network parameter then drops out of the property tree). Returns `false`
/// when no keyframe exists at `frame`.
pub fn remove_keyframe(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    frame: u64,
) -> bool {
    mutate_channel(layer, id, component, |channel| {
        let ChannelSource::Keyframes(curve) = &mut channel.source else {
            return false;
        };
        let Some(removed) = curve.remove(frame) else {
            return false;
        };
        if curve.is_empty() {
            channel.source = ChannelSource::Constant(removed.value);
        }
        true
    })
}

/// Move the keyframe at `from` to `to`, preserving value and tangents (an
/// existing keyframe at `to` is overwritten). Returns `false` when no
/// keyframe exists at `from`.
pub fn move_keyframe(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    from: u64,
    to: u64,
) -> bool {
    mutate_channel(layer, id, component, |channel| {
        let ChannelSource::Keyframes(curve) = &mut channel.source else {
            return false;
        };
        curve.move_keyframe(from, to)
    })
}

/// Set the channel's value at `frame`: a keyframed channel gets an updated
/// key (preserving its interpolation and tangents) or an inserted one; a
/// constant channel has its constant replaced. Returns `false` when the row
/// or component does not resolve or the source is not key-editable.
pub fn set_channel_value(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    frame: u64,
    value: f32,
) -> bool {
    mutate_channel(layer, id, component, |channel| {
        match &mut channel.source {
            ChannelSource::Constant(v) => *v = value,
            ChannelSource::Keyframes(curve) => {
                set_curve_value(curve, frame, value);
            }
            _ => return false,
        }
        true
    })
}

/// Write `value` at `frame`, keeping an existing key's interpolation mode
/// and tangents (a fresh key is Linear with zero tangents).
pub fn set_curve_value(curve: &mut KeyframeCurve, frame: u64, value: f32) {
    if !curve.modify(frame, value, None) {
        curve.insert(frame, value, Interpolation::Linear);
    }
}

/// Gesture preview for a keyframe drag: restore `baseline` (the curve as it
/// was when the gesture started) and move its key from `origin_frame` to
/// `new_frame`. Deriving every preview from the pre-gesture curve means a
/// transient pass over an occupied frame does not permanently merge the two
/// keys — only the committed end position can overwrite. Returns `false`
/// when the row/component no longer resolves or the baseline has no key at
/// `origin_frame`.
pub fn preview_keyframe_move(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    baseline: &KeyframeCurve,
    origin_frame: u64,
    new_frame: u64,
) -> bool {
    mutate_channel(layer, id, component, |channel| {
        let mut curve = baseline.clone();
        if !curve.move_keyframe(origin_frame, new_frame) {
            return false;
        }
        channel.source = ChannelSource::Keyframes(curve);
        true
    })
}

/// Gesture preview for moving several keyframes in one channel by the same
/// signed frame delta. The preview always rebuilds from `baseline`, removing
/// all moving keys before inserting their shifted copies, so crossing an
/// occupied frame during a live drag cannot permanently discard a key.
///
/// Returns `false` when the row/component no longer resolves or any requested
/// source frame is absent from the baseline. Callers must clamp `delta` so no
/// destination frame is negative.
pub fn preview_keyframe_moves(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    baseline: &KeyframeCurve,
    origin_frames: &[u64],
    delta: i64,
) -> bool {
    let moving = origin_frames
        .iter()
        .map(|frame| {
            baseline
                .keyframes()
                .iter()
                .find(|keyframe| keyframe.frame == *frame)
                .cloned()
        })
        .collect::<Option<Vec<_>>>();
    let Some(moving) = moving else {
        return false;
    };

    mutate_channel(layer, id, component, |channel| {
        let mut curve = baseline.clone();
        for frame in origin_frames {
            curve.remove(*frame);
        }
        for mut keyframe in moving {
            keyframe.frame = (keyframe.frame as i64 + delta) as u64;
            curve.insert_keyframe(keyframe);
        }
        channel.source = ChannelSource::Keyframes(curve);
        true
    })
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// The value a channel holds at `frame` without an evaluation context:
/// the constant, the curve sample, or 0 for unresolvable sources.
fn channel_value(channel: &AnimationChannel, frame: u64) -> f32 {
    match &channel.source {
        ChannelSource::Constant(v) => *v,
        ChannelSource::Keyframes(curve) => curve.sample(frame),
        _ => 0.0,
    }
}

fn shell_channels(layer: &Layer, group: PropertyGroup) -> Vec<&AnimationChannel> {
    match group {
        PropertyGroup::Position => {
            vec![&layer.transform.position[0], &layer.transform.position[1]]
        }
        PropertyGroup::Scale => vec![&layer.transform.scale[0], &layer.transform.scale[1]],
        PropertyGroup::Rotation => vec![&layer.transform.rotation],
        PropertyGroup::Opacity => vec![&layer.opacity],
        PropertyGroup::AnchorPoint => {
            vec![
                &layer.transform.anchor_point[0],
                &layer.transform.anchor_point[1],
            ]
        }
    }
}

/// Display names of a shell group's component channels.
pub fn shell_channel_names(group: PropertyGroup) -> &'static [&'static str] {
    match group {
        PropertyGroup::Position | PropertyGroup::Scale | PropertyGroup::AnchorPoint => &["X", "Y"],
        PropertyGroup::Rotation => &["Rotation"],
        PropertyGroup::Opacity => &["Opacity"],
    }
}

/// The component channels of a `Channel*` parameter value (`None` for
/// non-animatable variants — `Int` / `Bool` are constant-only in v1,
/// REQ-LAYER-004).
fn channel_components(value: &ParameterValue) -> Option<Vec<&AnimationChannel>> {
    match value {
        ParameterValue::Channel(ch) => Some(vec![ch]),
        ParameterValue::Channel2(chs) => Some(chs.iter().collect()),
        ParameterValue::Channel3(chs) => Some(chs.iter().collect()),
        ParameterValue::Channel4(chs) => Some(chs.iter().collect()),
        _ => None,
    }
}

/// Component names when the parameter is a `Channel*` value with at least
/// one keyframed component (`None` = not part of the property tree).
fn keyframed_channel_names(value: &ParameterValue) -> Option<Vec<String>> {
    let components = channel_components(value)?;
    if !components
        .iter()
        .any(|ch| matches!(ch.source, ChannelSource::Keyframes(_)))
    {
        return None;
    }
    let names = match components.len() {
        1 => vec!["Value"],
        2 => vec!["X", "Y"],
        3 => vec!["R", "G", "B"],
        _ => vec!["R", "G", "B", "A"],
    };
    Some(names.into_iter().map(str::to_string).collect())
}

/// Apply `f` to the channel at `component`, rebuilding the owning node for
/// network rows so the layer's immutable graph stays consistent.
fn mutate_channel(
    layer: &mut Layer,
    id: &PropertyRowId,
    component: usize,
    f: impl FnOnce(&mut AnimationChannel) -> bool,
) -> bool {
    match id {
        PropertyRowId::Shell(group) => {
            let channel = match group {
                PropertyGroup::Position => layer.transform.position.get_mut(component),
                PropertyGroup::Scale => layer.transform.scale.get_mut(component),
                PropertyGroup::Rotation => {
                    (component == 0).then_some(&mut layer.transform.rotation)
                }
                PropertyGroup::Opacity => (component == 0).then_some(&mut layer.opacity),
                PropertyGroup::AnchorPoint => layer.transform.anchor_point.get_mut(component),
            };
            let Some(channel) = channel else {
                return false;
            };
            f(channel)
        }
        PropertyRowId::Network { node, key } => {
            let Some(node_ref) = layer.network.node(*node) else {
                return false;
            };
            let mut updated = (**node_ref).clone();
            let Some(param) = updated.parameters.iter_mut().find(|p| p.key == *key) else {
                return false;
            };
            let channel = match &mut param.value {
                ParameterValue::Channel(ch) if component == 0 => Some(ch),
                ParameterValue::Channel2(chs) => chs.get_mut(component),
                ParameterValue::Channel3(chs) => chs.get_mut(component),
                ParameterValue::Channel4(chs) => chs.get_mut(component),
                _ => None,
            };
            let Some(channel) = channel else {
                return false;
            };
            if !f(channel) {
                return false;
            }
            layer.network = layer.network.clone().replace_node(Arc::new(updated));
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::graph::{Graph, Node};
    use ravel_core::id::{DataTypeId, LayerId};
    use ravel_core::types::FrameRate;

    fn curve_0_to_10() -> KeyframeCurve {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 1.0, Interpolation::Linear);
        curve
    }

    fn eval_ctx() -> ravel_core::eval::EvalContext {
        ravel_core::eval::EvalContext::new(0, FrameRate::new(30, 1), (16, 16))
    }

    /// Layer with an In custom parameter `amount` (constant) and a node
    /// `blur` whose `radius` is keyframed.
    fn test_layer() -> Layer {
        let in_node = Node::new(NodeId::new(10), net::NET_IN_TYPE_KEY)
            .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
            .with_output(net::PORT_TIME, DataTypeId::SCALAR)
            .with_output("amount", DataTypeId::SCALAR)
            .with_param("amount", ParameterValue::Float(2.0));
        let blur = Node::new(NodeId::new(20), "blur")
            .with_param(
                "radius",
                ParameterValue::Channel(AnimationChannel::keyframes(curve_0_to_10())),
            )
            .with_param("mix", ParameterValue::Float(0.5));
        let network = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(blur)
            .unwrap();
        Layer::new(LayerId::new(1), "L", network).with_time(10, 5, 300)
    }

    #[test]
    fn local_frame_conversion_roundtrips() {
        let layer = test_layer(); // start 10, in 5
        assert_eq!(layer_local_frame(&layer, 15), 10); // 15 - 10 + 5
        assert_eq!(comp_frame_for_key(&layer, 10), 15);
        assert_eq!(layer_local_frame(&layer, 0), 0, "clamped at zero");
    }

    #[test]
    fn rows_list_shell_groups_then_keyframed_network_params() {
        let rows = property_rows(&test_layer());
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].id, PropertyRowId::Shell(PropertyGroup::Position));
        assert_eq!(rows[3].id, PropertyRowId::Shell(PropertyGroup::Opacity));
        assert_eq!(
            rows[4].id,
            PropertyRowId::Network {
                node: NodeId::new(20),
                key: "radius".into()
            }
        );
        assert_eq!(rows[4].label.as_deref(), Some("blur · radius"));
        // Constant-only params (Float `mix`, `amount`) are not listed.
        assert!(!rows.iter().any(|r| matches!(
            &r.id,
            PropertyRowId::Network { key, .. } if key == "mix" || key == "amount"
        )));
    }

    #[test]
    fn insert_keyframe_converts_a_constant_custom_param() {
        let mut layer = test_layer();
        let in_id = PropertyRowId::Network {
            node: NodeId::new(10),
            key: "amount".into(),
        };
        // Give the custom param a constant channel first (the properties
        // toggle does this conversion; `Float` stays constant-only here).
        let in_node = layer.network.node(NodeId::new(10)).unwrap();
        let mut updated = (**in_node).clone();
        updated
            .parameters
            .iter_mut()
            .find(|p| p.key == "amount")
            .unwrap()
            .value = ParameterValue::Channel(AnimationChannel::constant(2.0));
        layer.network = layer.network.clone().replace_node(Arc::new(updated));

        assert!(insert_keyframe(&mut layer, &in_id, 0, 7));
        // The constant 2.0 became a keyframed channel keyed at frame 7.
        let channels = row_channels(&layer, &in_id).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.len(), 1);
        assert!((curve.sample(7) - 2.0).abs() < f32::EPSILON);
        // …and the param now shows up in the tree with the In bare-key label.
        let row = property_rows(&layer)
            .into_iter()
            .find(|r| r.id == in_id)
            .expect("keyframed custom param listed");
        assert_eq!(row.label.as_deref(), Some("amount"));
    }

    #[test]
    fn insert_on_keyframed_channel_samples_the_current_value() {
        let mut layer = test_layer();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        };
        assert!(insert_keyframe(&mut layer, &row, 0, 5));
        let channels = row_channels(&layer, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.len(), 3);
        assert!(
            (curve.sample(5) - 0.5).abs() < 1e-4,
            "interpolated value kept"
        );
    }

    #[test]
    fn remove_last_keyframe_reverts_to_constant_and_drops_the_row() {
        let mut layer = test_layer();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        };
        assert!(remove_keyframe(&mut layer, &row, 0, 0));
        assert!(remove_keyframe(&mut layer, &row, 0, 10));
        assert!(!remove_keyframe(&mut layer, &row, 0, 10), "already gone");
        let channels = row_channels(&layer, &row).unwrap();
        assert_eq!(channels[0].source, ChannelSource::Constant(1.0));
        assert!(!property_rows(&layer).iter().any(|r| r.id == row));
    }

    #[test]
    fn move_keyframe_preserves_the_value() {
        let mut layer = test_layer();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        };
        assert!(move_keyframe(&mut layer, &row, 0, 10, 20));
        assert!(has_keyframe_at(&layer, &row, 0, 20));
        assert!(!has_keyframe_at(&layer, &row, 0, 10));
        let channels = row_channels(&layer, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert!((curve.sample(20) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn shell_channels_are_editable() {
        let mut layer = test_layer();
        let row = PropertyRowId::Shell(PropertyGroup::Position);
        // Constant → keyframe at the current value.
        assert!(insert_keyframe(&mut layer, &row, 0, 4));
        assert!(has_keyframe_at(&layer, &row, 0, 4));
        // Scrub-style set keys the animated channel.
        assert!(set_channel_value(&mut layer, &row, 0, 4, 12.0));
        assert!(set_channel_value(&mut layer, &row, 0, 8, 20.0));
        let ctx = eval_ctx();
        assert!((layer.transform.position[0].evaluate(4, &ctx) - 12.0).abs() < f32::EPSILON);
        assert!((layer.transform.position[0].evaluate(8, &ctx) - 20.0).abs() < f32::EPSILON);
        // Out-of-range component is rejected.
        assert!(!set_channel_value(
            &mut layer,
            &PropertyRowId::Shell(PropertyGroup::Rotation),
            1,
            0,
            1.0
        ));
    }

    #[test]
    fn missing_node_or_param_is_rejected() {
        let mut layer = test_layer();
        let bogus_node = PropertyRowId::Network {
            node: NodeId::new(999),
            key: "x".into(),
        };
        assert!(!insert_keyframe(&mut layer, &bogus_node, 0, 0));
        let bogus_key = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "nope".into(),
        };
        assert!(!insert_keyframe(&mut layer, &bogus_key, 0, 0));
        // Int params are constant-only in v1 (REQ-LAYER-004).
        let float_key = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "mix".into(),
        };
        assert!(!insert_keyframe(&mut layer, &float_key, 0, 0));
    }

    #[test]
    fn multi_component_params_report_component_names() {
        let color = Node::new(NodeId::new(30), "constant.color").with_param(
            "color",
            ParameterValue::Channel4([
                AnimationChannel::keyframes(curve_0_to_10()),
                AnimationChannel::constant(0.5),
                AnimationChannel::constant(0.5),
                AnimationChannel::constant(1.0),
            ]),
        );
        let network = Graph::new().add_node(color).unwrap();
        let layer = Layer::new(LayerId::new(2), "C", network).with_time(0, 0, 100);
        let rows = property_rows(&layer);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[4].channel_names, vec!["R", "G", "B", "A"]);
        // Per-component editing targets the keyframed component only.
        let mut layer = layer;
        let row = rows[4].id.clone();
        assert!(insert_keyframe(&mut layer, &row, 1, 3));
        assert!(has_keyframe_at(&layer, &row, 1, 3));
        assert!(!has_keyframe_at(&layer, &row, 2, 3));
    }

    /// Drag previews derive from the gesture baseline: passing over an
    /// occupied frame must not destroy the other key.
    #[test]
    fn preview_move_across_a_collision_restores_the_other_key() {
        let mut layer = test_layer();
        let row = PropertyRowId::Network {
            node: NodeId::new(20),
            key: "radius".into(),
        }; // keys at 0 and 10
        let baseline = {
            let channels = row_channels(&layer, &row).unwrap();
            let ChannelSource::Keyframes(curve) = &channels[0].source else {
                panic!("expected keyframes");
            };
            curve.clone()
        };

        // Drag 0 → 10 (overwrites the key at 10 in the preview)…
        assert!(preview_keyframe_move(&mut layer, &row, 0, &baseline, 0, 10));
        // …then keep going to 20: the frame-10 key is restored, not merged.
        assert!(preview_keyframe_move(&mut layer, &row, 0, &baseline, 0, 20));
        assert!(has_keyframe_at(&layer, &row, 0, 10));
        assert!(has_keyframe_at(&layer, &row, 0, 20));
        let channels = row_channels(&layer, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.len(), 2);
        assert!((curve.sample(10) - 1.0).abs() < f32::EPSILON);
        assert!((curve.sample(20) - 0.0).abs() < f32::EPSILON);
        // Releasing on the occupied frame does overwrite (end position).
        assert!(preview_keyframe_move(&mut layer, &row, 0, &baseline, 0, 10));
        let channels = row_channels(&layer, &row).unwrap();
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.len(), 1);
    }
}
