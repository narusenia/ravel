// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Keyframe and shell-property writes for [`EditOp::SetKeyframes`],
//! [`EditOp::ClearAnimation`] and [`EditOp::SetLayer`].
//!
//! They resolve a channel through the functions the Timeline uses
//! ([`Layer::shell_channel_mut`], [`ParameterValue::channel_mut`],
//! [`set_curve_interpolation`]) so the UI and the edit layer cannot disagree
//! about which channel a row is or what a bezier key looks like.

use super::param::channel_is_driven;
use super::{EditContext, EditError, KeyInput, KeyInterpolation};
use crate::animation::channel::{AnimationChannel, ChannelSource};
use crate::animation::curve::{KeyframeCurve, set_curve_interpolation};
use crate::animation::interpolation::Interpolation;
use crate::composition::{BlendMode, Layer, ShellProperty};
use crate::graph::{Graph, Parameter, ParameterValue};
use crate::id::{LayerId, NodeId};

/// A curve through `keys`, whose frames are composition frames of `layer`.
/// Keys are stored layer-local, so moving the layer moves them with it.
///
/// Every key goes in linear first and the interpolation is set afterwards,
/// the way the editor switches a key to bezier: that seeds the default
/// handles from the neighbouring key, which has to exist by then.
pub(super) fn build_curve(
    keys: &[KeyInput],
    layer: &Layer,
    clamp: impl Fn(f32) -> f32,
) -> Result<KeyframeCurve, EditError> {
    if keys.is_empty() {
        return Err(EditError::InvalidKeyframes("no keys given".into()));
    }
    let mut curve = KeyframeCurve::new();
    let mut local = Vec::with_capacity(keys.len());
    for key in keys {
        if !key.value.is_finite() {
            return Err(EditError::InvalidKeyframes(format!(
                "the value at frame {} is not a finite number",
                key.frame
            )));
        }
        let frame = layer
            .key_frame_for_comp(key.frame)
            .ok_or(EditError::KeyBeforeLayerStart {
                layer: layer.id,
                frame: key.frame,
            })?;
        if curve.keyframes().iter().any(|k| k.frame == frame) {
            return Err(EditError::InvalidKeyframes(format!(
                "frame {} is given twice",
                key.frame
            )));
        }
        curve.insert(frame, clamp(key.value), Interpolation::Linear);
        local.push((frame, key.interpolation));
    }
    local.sort_by_key(|(frame, _)| *frame);
    for (frame, interpolation) in local {
        if interpolation != KeyInterpolation::Linear {
            set_curve_interpolation(&mut curve, frame, interpolation.into());
        }
    }
    Ok(curve)
}

/// Whether `clear_animation` may discard the source: keys and expressions
/// are the caller's to remove, a node output or a blend is wired elsewhere.
fn removable(source: &ChannelSource) -> bool {
    matches!(
        source,
        ChannelSource::Constant(_) | ChannelSource::Keyframes(_) | ChannelSource::Expression(_)
    )
}

/// How a node-parameter channel is rewritten.
pub(super) enum ChannelWrite {
    Keys(KeyframeCurve),
    Constant(f32),
}

/// Rewrite component `component` of parameter `key` of `node_id`.
pub(super) fn write_param_channel(
    graph: Graph,
    node_id: NodeId,
    key: &str,
    component: u32,
    write: ChannelWrite,
) -> Result<Graph, EditError> {
    let node = graph
        .node(node_id)
        .ok_or(EditError::NodeNotFound(node_id))?;
    let mut value = node
        .parameters
        .iter()
        .find(|p| p.key == key)
        .ok_or_else(|| EditError::UnknownParameter {
            node: node_id,
            key: key.to_string(),
        })?
        .value
        .clone();
    // A plain Float or Int is a constant that has not been keyed yet. Keying
    // it gives it the channel spelling the Timeline edits, as the Properties
    // keyframe toggle does; clearing one leaves it as it is.
    match (&value, &write) {
        (ParameterValue::Float(v), ChannelWrite::Keys(_)) => {
            value = ParameterValue::Channel(AnimationChannel::constant(*v));
        }
        (ParameterValue::Int(v), ChannelWrite::Keys(_)) => {
            value = ParameterValue::IntChannel(AnimationChannel::constant(*v as f32));
        }
        (ParameterValue::Float(_) | ParameterValue::Int(_), ChannelWrite::Constant(v)) => {
            if component != 0 {
                return Err(EditError::UnknownChannel {
                    target: format!("parameter {key:?} of node {node_id:?}"),
                    component,
                });
            }
            value = match value {
                ParameterValue::Int(_) => ParameterValue::Int(v.round() as i32),
                _ => ParameterValue::Float(*v),
            };
            let changed = Parameter {
                key: key.to_string(),
                value,
            };
            return Ok(graph.set_params_and_output_types(node_id, &[changed], &[])?);
        }
        _ => {}
    }
    if value.channel_mut(0).is_none() {
        return Err(EditError::ParameterTypeMismatch {
            node: node_id,
            key: key.to_string(),
        });
    }
    let channel =
        value
            .channel_mut(component as usize)
            .ok_or_else(|| EditError::UnknownChannel {
                target: format!("parameter {key:?} of node {node_id:?}"),
                component,
            })?;
    let permitted = match write {
        ChannelWrite::Keys(_) => !channel_is_driven(channel),
        ChannelWrite::Constant(_) => removable(&channel.source),
    };
    if !permitted {
        return Err(EditError::DrivenParameter {
            node: node_id,
            key: key.to_string(),
        });
    }
    *channel = match write {
        ChannelWrite::Keys(curve) => AnimationChannel::keyframes(curve),
        ChannelWrite::Constant(v) => AnimationChannel::constant(v),
    };
    let changed = Parameter {
        key: key.to_string(),
        value,
    };
    Ok(graph.set_params_and_output_types(node_id, &[changed], &[])?)
}

/// Rewrite component `component` of shell property `property`.
pub(super) fn write_shell_channel(
    layer: &mut Layer,
    property: ShellProperty,
    component: u32,
    write: ChannelWrite,
) -> Result<(), EditError> {
    let id = layer.id;
    // The time remap is optional; it comes into being by being written.
    if property == ShellProperty::TimeRemap && layer.time_remap.is_none() {
        layer.time_remap = Some(AnimationChannel::constant(0.0));
    }
    let channel = layer
        .shell_channel_mut(property, component as usize)
        .ok_or_else(|| EditError::UnknownChannel {
            target: format!("shell property {property:?} of layer {id:?}"),
            component,
        })?;
    let permitted = match write {
        ChannelWrite::Keys(_) => !channel_is_driven(channel),
        ChannelWrite::Constant(_) => removable(&channel.source),
    };
    if !permitted {
        return Err(EditError::DrivenShellProperty {
            layer: id,
            property,
        });
    }
    *channel = match write {
        ChannelWrite::Keys(curve) => AnimationChannel::keyframes(curve),
        ChannelWrite::Constant(v) => AnimationChannel::constant(v),
    };
    Ok(())
}

/// The value clamp a shell property's keys and constants share.
pub(super) fn shell_clamp(property: ShellProperty) -> impl Fn(f32) -> f32 {
    move |v| {
        if property == ShellProperty::Opacity {
            v.clamp(0.0, 1.0)
        } else {
            v
        }
    }
}

/// The fields of [`EditOp::SetLayer`], resolved (the parent is an id).
pub(super) struct LayerEdit {
    pub name: Option<String>,
    pub start_frame: Option<i64>,
    pub in_frame: Option<u64>,
    pub out_frame: Option<u64>,
    pub anchor_point: Option<[f32; 2]>,
    pub position: Option<[f32; 2]>,
    pub scale: Option<[f32; 2]>,
    pub rotation: Option<f32>,
    pub opacity: Option<f32>,
    pub blend_mode: Option<BlendMode>,
    pub parent: Option<Option<LayerId>>,
    pub solo: Option<bool>,
    pub muted: Option<bool>,
    pub locked: Option<bool>,
}

/// Overwrite a transform or opacity channel with a constant, refusing an
/// animated or driven one (a constant cannot be placed on it without
/// discarding the animation).
fn set_constant(
    layer: &mut Layer,
    property: ShellProperty,
    component: usize,
    value: f32,
) -> Result<(), EditError> {
    let id = layer.id;
    if !value.is_finite() {
        return Err(EditError::InvalidLayerEdit(format!(
            "{property:?} is not a finite number"
        )));
    }
    let value = shell_clamp(property)(value);
    let channel = layer
        .shell_channel_mut(property, component)
        .ok_or_else(|| EditError::UnknownChannel {
            target: format!("shell property {property:?} of layer {id:?}"),
            component: component as u32,
        })?;
    match channel.source {
        ChannelSource::Constant(_) => {
            *channel = AnimationChannel::constant(value);
            Ok(())
        }
        ChannelSource::Keyframes(_) => Err(EditError::AnimatedShellProperty {
            layer: id,
            property,
        }),
        _ => Err(EditError::DrivenShellProperty {
            layer: id,
            property,
        }),
    }
}

/// Apply `edit` to `layer`. The caller discards the layer on `Err`.
pub(super) fn apply_layer_edit(layer: &mut Layer, edit: LayerEdit) -> Result<(), EditError> {
    if let Some(name) = edit.name {
        layer.name = name;
    }
    if let Some(v) = edit.start_frame {
        layer.start_frame = v;
    }
    if let Some(v) = edit.in_frame {
        layer.in_frame = v;
    }
    if let Some(v) = edit.out_frame {
        layer.out_frame = v;
    }
    if layer.out_frame < layer.in_frame {
        return Err(EditError::InvalidLayerEdit(format!(
            "out_frame {} is before in_frame {}",
            layer.out_frame, layer.in_frame
        )));
    }
    for (property, value) in [
        (ShellProperty::AnchorPoint, edit.anchor_point),
        (ShellProperty::Position, edit.position),
        (ShellProperty::Scale, edit.scale),
    ] {
        for (component, v) in value.into_iter().flatten().enumerate() {
            set_constant(layer, property, component, v)?;
        }
    }
    if let Some(v) = edit.rotation {
        set_constant(layer, ShellProperty::Rotation, 0, v)?;
    }
    if let Some(v) = edit.opacity {
        set_constant(layer, ShellProperty::Opacity, 0, v)?;
    }
    if let Some(v) = edit.blend_mode {
        layer.blend_mode = v;
    }
    if let Some(v) = edit.parent {
        layer.parent = v;
    }
    if let Some(v) = edit.solo {
        layer.solo = v;
    }
    if let Some(v) = edit.muted {
        layer.muted = v;
    }
    if let Some(v) = edit.locked {
        layer.locked = v;
    }
    Ok(())
}

/// The registry range clamp for numbers written to `key` of `node`.
pub(super) fn param_clamp<'a>(
    graph: &Graph,
    node_id: NodeId,
    key: &str,
    cx: &'a EditContext,
) -> Result<impl Fn(f32) -> f32 + 'a, EditError> {
    let node = graph
        .node(node_id)
        .ok_or(EditError::NodeNotFound(node_id))?;
    let range = cx.registry.param_range(&node.type_key, key);
    Ok(move |v: f32| range.map_or(v, |r| r.clamp(v)))
}
