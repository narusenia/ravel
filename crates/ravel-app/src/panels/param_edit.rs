// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared parameter-write semantics for direct manipulation and Properties.

use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
use ravel_core::graph::ParameterValue;
use ravel_core::registry::ParamRange;

fn edited_channel(
    channel: &AnimationChannel,
    value: f32,
    local_frame: Option<u64>,
) -> AnimationChannel {
    match &channel.source {
        ChannelSource::Constant(_) => AnimationChannel::constant(value),
        ChannelSource::Keyframes(curve) => match local_frame {
            Some(frame) => {
                let mut curve = curve.clone();
                ravel_ui::keyframes::set_curve_value(&mut curve, frame, value);
                AnimationChannel::keyframes(curve)
            }
            None => AnimationChannel::constant(value),
        },
        _ => channel.clone(),
    }
}

pub(super) fn edited_float_param(
    existing: &ParameterValue,
    value: f32,
    local_frame: Option<u64>,
) -> ParameterValue {
    match existing {
        ParameterValue::Channel(channel) => match &channel.source {
            ChannelSource::Constant(_) => {
                ParameterValue::Channel(AnimationChannel::constant(value))
            }
            ChannelSource::Keyframes(curve) => match local_frame {
                Some(frame) => {
                    let mut curve = curve.clone();
                    ravel_ui::keyframes::set_curve_value(&mut curve, frame, value);
                    ParameterValue::Channel(AnimationChannel::keyframes(curve))
                }
                None => ParameterValue::Float(value),
            },
            _ => ParameterValue::Float(value),
        },
        _ => ParameterValue::Float(value),
    }
}

pub(super) fn edited_param_value(
    existing: &ParameterValue,
    value: &ravel_ui::properties::PropertyValue,
    range: Option<&ParamRange>,
    local_frame: Option<u64>,
) -> Option<ParameterValue> {
    use ravel_ui::properties::PropertyValue;
    match value {
        PropertyValue::Float(value) => {
            let value = range.map_or(*value, |range| range.clamp(*value));
            match existing {
                ParameterValue::Channel2(_)
                | ParameterValue::Channel3(_)
                | ParameterValue::Channel4(_) => None,
                _ => Some(edited_float_param(existing, value, local_frame)),
            }
        }
        PropertyValue::Int(value) => {
            Some(ParameterValue::Int(range.map_or(*value, |range| {
                range.clamp(*value as f32).round() as i32
            })))
        }
        PropertyValue::Bool(value) => Some(ParameterValue::Bool(*value)),
        PropertyValue::String(value) => Some(ParameterValue::String(value.clone())),
        PropertyValue::Vector(components) => {
            let clamped: Vec<f32> = components
                .iter()
                .map(|value| range.map_or(*value, |range| range.clamp(*value)))
                .collect();
            match (existing, clamped.as_slice()) {
                (ParameterValue::Channel2(channels), [x, y]) => Some(ParameterValue::Channel2([
                    edited_channel(&channels[0], *x, local_frame),
                    edited_channel(&channels[1], *y, local_frame),
                ])),
                (ParameterValue::Channel3(channels), [x, y, z]) => {
                    Some(ParameterValue::Channel3([
                        edited_channel(&channels[0], *x, local_frame),
                        edited_channel(&channels[1], *y, local_frame),
                        edited_channel(&channels[2], *z, local_frame),
                    ]))
                }
                _ => None,
            }
        }
        PropertyValue::Color { r, g, b, a } => match existing {
            ParameterValue::Channel4(channels) => Some(ParameterValue::Channel4([
                edited_channel(&channels[0], *r, local_frame),
                edited_channel(&channels[1], *g, local_frame),
                edited_channel(&channels[2], *b, local_frame),
                edited_channel(&channels[3], *a, local_frame),
            ])),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::animation::curve::KeyframeCurve;
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_ui::properties::PropertyValue;

    #[test]
    fn float_edit_preserves_plain_float() {
        assert_eq!(
            edited_param_value(
                &ParameterValue::Float(1.0),
                &PropertyValue::Float(4.0),
                None,
                Some(7),
            ),
            Some(ParameterValue::Float(4.0))
        );
    }

    #[test]
    fn float_edit_updates_constant_channel() {
        let existing = ParameterValue::Channel(AnimationChannel::constant(1.0));
        let Some(ParameterValue::Channel(channel)) =
            edited_param_value(&existing, &PropertyValue::Float(4.0), None, Some(7))
        else {
            panic!("expected channel");
        };
        assert!(matches!(channel.source, ChannelSource::Constant(4.0)));
    }

    #[test]
    fn float_edit_inserts_key_at_local_frame() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 1.0, Interpolation::Linear);
        let existing = ParameterValue::Channel(AnimationChannel::keyframes(curve));
        let Some(ParameterValue::Channel(channel)) =
            edited_param_value(&existing, &PropertyValue::Float(4.0), None, Some(7))
        else {
            panic!("expected channel");
        };
        let ChannelSource::Keyframes(curve) = channel.source else {
            panic!("expected keyframes");
        };
        assert_eq!(curve.sample(7), 4.0);
        assert!(curve.keyframes().iter().any(|key| key.frame == 7));
    }

    #[test]
    fn vector_edits_write_every_channel_component() {
        let existing = ParameterValue::Channel2([
            AnimationChannel::constant(0.0),
            AnimationChannel::constant(0.0),
        ]);
        let value = PropertyValue::Vector(vec![4.0, -2.0]);
        let Some(ParameterValue::Channel2(channels)) =
            edited_param_value(&existing, &value, None, None)
        else {
            panic!("expected Channel2");
        };
        assert!(matches!(channels[0].source, ChannelSource::Constant(4.0)));
        assert!(matches!(channels[1].source, ChannelSource::Constant(-2.0)));

        let wrong = PropertyValue::Vector(vec![1.0, 2.0, 3.0]);
        assert!(edited_param_value(&existing, &wrong, None, None).is_none());
    }

    #[test]
    fn color_edits_keep_keyframed_components_animated() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 1.0, Interpolation::Linear);
        let existing = ParameterValue::Channel4([
            AnimationChannel::keyframes(curve),
            AnimationChannel::constant(0.5),
            AnimationChannel::constant(0.5),
            AnimationChannel::constant(1.0),
        ]);
        let value = PropertyValue::Color {
            r: 0.25,
            g: 0.75,
            b: 0.75,
            a: 1.0,
        };
        let Some(ParameterValue::Channel4(channels)) =
            edited_param_value(&existing, &value, None, Some(5))
        else {
            panic!("expected Channel4");
        };
        let ChannelSource::Keyframes(curve) = &channels[0].source else {
            panic!("component stays keyframed");
        };
        assert_eq!(curve.keyframes().len(), 3);
        assert!(matches!(channels[1].source, ChannelSource::Constant(0.75)));
    }
}
