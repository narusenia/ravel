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

/// Write an int, keeping the parameter's channel shape: an animated
/// `IntChannel` gains (or updates) a key at `local_frame`, a constant one has
/// its constant replaced, and anything else becomes a plain `Int`.
///
/// This is [`edited_float_param`] with the int widened into the f32 channel the
/// variant stores — the rounding back to `i32` happens where the value is read.
/// Without the `IntChannel` case a keyed count would be flattened to a
/// constant by the first spinner edit, which is the destructive answer the row
/// used to be read-only to avoid.
pub(super) fn edited_int_param(
    existing: &ParameterValue,
    value: i32,
    local_frame: Option<u64>,
) -> ParameterValue {
    match existing {
        ParameterValue::IntChannel(channel) => match &channel.source {
            ChannelSource::Constant(_) => {
                ParameterValue::IntChannel(AnimationChannel::constant(value as f32))
            }
            ChannelSource::Keyframes(curve) => match local_frame {
                Some(frame) => {
                    let mut curve = curve.clone();
                    ravel_ui::keyframes::set_curve_value(&mut curve, frame, value as f32);
                    ParameterValue::IntChannel(AnimationChannel::keyframes(curve))
                }
                // No frame to key at: the same fall-back to the constant
                // spelling `edited_float_param` makes.
                None => ParameterValue::Int(value),
            },
            // An expression, a node-output binding or a blend is not something
            // a spinner can write into; leave it as it is.
            _ => existing.clone(),
        },
        _ => ParameterValue::Int(value),
    }
}

/// Write a string, keeping the parameter's step shape: an animated
/// `StringSteps` gains (or overwrites) the key at `local_frame`, anything else
/// becomes a plain `String`.
///
/// Writing *only* the key at the frame is what makes the row an editor of the
/// animation rather than a replacement for it — the same rule the float row
/// follows. With no frame to key at there is nothing to write into, so the
/// value collapses to the constant spelling, exactly as
/// [`edited_float_param`] does.
pub(super) fn edited_string_param(
    existing: &ParameterValue,
    value: &str,
    local_frame: Option<u64>,
) -> ParameterValue {
    match (existing, local_frame) {
        (ParameterValue::StringSteps(steps), Some(frame)) => {
            let mut steps = steps.clone();
            steps.insert(frame, value.to_string());
            ParameterValue::StringSteps(steps)
        }
        _ => ParameterValue::String(value.to_string()),
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
                // A float write over an animated int or string would answer
                // with a constant `Float`, discarding every key on the
                // parameter. Those rows emit `Int` / `String`, so this is only
                // reachable through a stale binding — and a refusal is the only
                // non-destructive answer to one.
                ParameterValue::IntChannel(_) | ParameterValue::StringSteps(_) => None,
                _ => Some(edited_float_param(existing, value, local_frame)),
            }
        }
        PropertyValue::Int(value) => {
            let value = range.map_or(*value, |range| range.clamp(*value as f32).round() as i32);
            Some(edited_int_param(existing, value, local_frame))
        }
        PropertyValue::Bool(value) => Some(ParameterValue::Bool(*value)),
        PropertyValue::String(value) => Some(edited_string_param(existing, value, local_frame)),
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

    /// An int edit on a keyed `IntChannel` keys it at the frame instead of
    /// flattening it, and the parameter stays an int channel — the whole point
    /// of `from_channels` re-typing after the previous value.
    #[test]
    fn int_edit_keys_a_keyframed_int_channel() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 6.0, Interpolation::Linear);
        curve.insert(10, 12.0, Interpolation::Linear);
        let existing = ParameterValue::IntChannel(AnimationChannel::keyframes(curve));

        let Some(ParameterValue::IntChannel(channel)) =
            edited_param_value(&existing, &PropertyValue::Int(9), None, Some(5))
        else {
            panic!("an int edit keeps the int channel");
        };
        let ChannelSource::Keyframes(curve) = &channel.source else {
            panic!("the curve survives: {:?}", channel.source);
        };
        assert_eq!(curve.len(), 3, "a key was added at frame 5");
        assert_eq!(curve.sample(5.0), 9.0);
        assert_eq!(curve.sample(0.0), 6.0, "the existing keys are untouched");
    }

    /// A constant int channel updates its constant and stays a channel; a
    /// plain `Int` stays a plain `Int`.
    #[test]
    fn int_edit_updates_a_constant_int_channel() {
        let existing = ParameterValue::IntChannel(AnimationChannel::constant(6.0));
        let Some(ParameterValue::IntChannel(channel)) =
            edited_param_value(&existing, &PropertyValue::Int(9), None, Some(5))
        else {
            panic!("expected an int channel");
        };
        assert!(matches!(channel.source, ChannelSource::Constant(v) if v == 9.0));

        assert_eq!(
            edited_param_value(
                &ParameterValue::Int(6),
                &PropertyValue::Int(9),
                None,
                Some(5)
            ),
            Some(ParameterValue::Int(9))
        );
    }

    /// A string edit on a `StringSteps` writes the key at the frame and leaves
    /// the other keys alone; a plain `String` is replaced whole.
    #[test]
    fn string_edit_writes_one_step_key() {
        use ravel_core::animation::StepCurve;
        let mut steps = StepCurve::new("seed".to_string());
        steps.insert(0, "first".to_string());
        steps.insert(10, "second".to_string());
        let existing = ParameterValue::StringSteps(steps);

        let Some(ParameterValue::StringSteps(written)) = edited_param_value(
            &existing,
            &PropertyValue::String("edited".into()),
            None,
            Some(10),
        ) else {
            panic!("a string edit keeps the step curve");
        };
        assert_eq!(written.len(), 2, "the key at 10 was overwritten, not added");
        assert_eq!(written.sample(10.0), "edited");
        assert_eq!(written.sample(0.0), "first", "the other key is untouched");

        assert_eq!(
            edited_param_value(
                &ParameterValue::String("a".into()),
                &PropertyValue::String("b".into()),
                None,
                Some(10),
            ),
            Some(ParameterValue::String("b".into()))
        );
    }

    /// A stale float binding must not answer over an animated int or string:
    /// the write would return a constant `Float` and take every key with it.
    #[test]
    fn a_float_edit_refuses_an_animated_int_or_string() {
        use ravel_core::animation::StepCurve;
        assert!(
            edited_param_value(
                &ParameterValue::IntChannel(AnimationChannel::constant(6.0)),
                &PropertyValue::Float(9.0),
                None,
                Some(5),
            )
            .is_none()
        );
        assert!(
            edited_param_value(
                &ParameterValue::StringSteps(StepCurve::keyed(0, "a".to_string())),
                &PropertyValue::Float(9.0),
                None,
                Some(5),
            )
            .is_none()
        );
    }

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
