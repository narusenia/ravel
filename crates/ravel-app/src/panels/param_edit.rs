// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared parameter-write semantics for direct manipulation and Properties.

use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
use ravel_core::graph::ParameterValue;
use ravel_core::registry::ParamRange;

pub(super) fn edited_channel(
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

/// Write `values` into the leading components of a vector parameter,
/// preserving each component's channel shape (a keyframed component stays
/// keyframed, gaining a key at `local_frame`). Trailing components the
/// caller does not supply — the Z of a `Channel3` driven by a 2D canvas
/// gesture — are left untouched. `None` when `existing` is not a vector
/// parameter or declares fewer components than `values`.
pub(super) fn edited_vector_param(
    existing: &ParameterValue,
    values: &[f32],
    local_frame: Option<u64>,
) -> Option<ParameterValue> {
    fn write<const N: usize>(
        channels: &[AnimationChannel; N],
        values: &[f32],
        local_frame: Option<u64>,
    ) -> Option<[AnimationChannel; N]> {
        if values.len() > N {
            return None;
        }
        let mut updated = channels.clone();
        for (slot, value) in updated.iter_mut().zip(values) {
            *slot = edited_channel(slot, *value, local_frame);
        }
        Some(updated)
    }
    match existing {
        ParameterValue::Channel2(chs) => {
            write(chs, values, local_frame).map(ParameterValue::Channel2)
        }
        ParameterValue::Channel3(chs) => {
            write(chs, values, local_frame).map(ParameterValue::Channel3)
        }
        ParameterValue::Channel4(chs) => {
            write(chs, values, local_frame).map(ParameterValue::Channel4)
        }
        _ => None,
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
        // A curve edit replaces the whole control-point set. It only applies
        // to a parameter that already is a curve: a stale binding must never
        // retype a scalar parameter into a structural one.
        PropertyValue::Curve(curve) => match existing {
            ParameterValue::Curve(_) => Some(ParameterValue::Curve(curve.clone())),
            _ => None,
        },
        // A ramp edit replaces the whole stop set, on the same terms and for
        // the same reason: a stale binding must never retype a parameter.
        PropertyValue::Ramp(ramp) => match existing {
            ParameterValue::Ramp(_) => Some(ParameterValue::Ramp(ramp.clone())),
            _ => None,
        },
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
        assert_eq!(curve.sample(7.0), 4.0);
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

    /// A curve edit writes the edited control points and refuses to retype a
    /// parameter that is not already a curve.
    #[test]
    fn curve_edits_replace_the_control_points_of_a_curve_parameter() {
        use ravel_core::param_curve::CurveParam;
        let edited = CurveParam::linear([(0.0, 0.0), (0.5, 0.9), (1.0, 1.0)]);
        assert_eq!(
            edited_param_value(
                &ParameterValue::Curve(CurveParam::identity()),
                &PropertyValue::Curve(edited.clone()),
                None,
                Some(7),
            ),
            Some(ParameterValue::Curve(edited.clone()))
        );
        assert!(
            edited_param_value(
                &ParameterValue::Float(1.0),
                &PropertyValue::Curve(edited),
                None,
                None,
            )
            .is_none()
        );
    }

    /// A ramp edit writes the edited stops and refuses to retype a parameter
    /// that is not already a ramp.
    #[test]
    fn ramp_edits_replace_the_stops_of_a_ramp_parameter() {
        use ravel_core::param_ramp::RampParam;
        use ravel_core::types::Color;
        let edited = RampParam::linear([(0.0, Color::WHITE), (0.5, Color::BLACK)]);
        assert_eq!(
            edited_param_value(
                &ParameterValue::Ramp(RampParam::default()),
                &PropertyValue::Ramp(edited.clone()),
                None,
                Some(7),
            ),
            Some(ParameterValue::Ramp(edited.clone()))
        );
        assert!(
            edited_param_value(
                &ParameterValue::Float(1.0),
                &PropertyValue::Ramp(edited),
                None,
                None,
            )
            .is_none()
        );
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
