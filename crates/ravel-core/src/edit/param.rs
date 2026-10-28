// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Parameter-write semantics shared by direct manipulation, Properties and
//! [`EditOp::SetParameter`](super::EditOp::SetParameter).
//!
//! The `edited_*` functions are pure and keep the long-standing UI rules: a
//! constant is replaced, an animated parameter gets a key at `local_frame`,
//! numbers are clamped by the registry range, and a write of the wrong shape
//! is refused with `None`. They fall back to a constant when an animated
//! parameter has no frame, because a gesture always has a playhead;
//! [`set_parameter`] is the layer entry point and refuses that case instead.

use super::{EditContext, EditError, ParamInput};
use crate::animation::channel::{AnimationChannel, ChannelSource};
use crate::animation::curve::set_curve_value;
use crate::composition::Composition;
use crate::graph::{Graph, Parameter, ParameterValue};
use crate::id::NodeId;
use crate::registry::ParamRange;

pub fn edited_channel(
    channel: &AnimationChannel,
    value: f32,
    local_frame: Option<u64>,
) -> AnimationChannel {
    match &channel.source {
        ChannelSource::Constant(_) => AnimationChannel::constant(value),
        ChannelSource::Keyframes(curve) => match local_frame {
            Some(frame) => {
                let mut curve = curve.clone();
                set_curve_value(&mut curve, frame, value);
                AnimationChannel::keyframes(curve)
            }
            None => AnimationChannel::constant(value),
        },
        _ => channel.clone(),
    }
}

pub fn edited_float_param(
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
                    set_curve_value(&mut curve, frame, value);
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
pub fn edited_int_param(
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
                    set_curve_value(&mut curve, frame, value as f32);
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
pub fn edited_string_param(
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
pub fn edited_vector_param(
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

pub fn edited_param_value(
    existing: &ParameterValue,
    value: &ParamInput,
    range: Option<&ParamRange>,
    local_frame: Option<u64>,
) -> Option<ParameterValue> {
    match value {
        ParamInput::Float(value) => {
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
        ParamInput::Int(value) => {
            let value = range.map_or(*value, |range| range.clamp(*value as f32).round() as i32);
            Some(edited_int_param(existing, value, local_frame))
        }
        ParamInput::Bool(value) => Some(ParameterValue::Bool(*value)),
        ParamInput::String(value) => Some(edited_string_param(existing, value, local_frame)),
        ParamInput::Vector(components) => {
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
                // A non-colour 4-vector; `describe_node` advertises it as one.
                (ParameterValue::Channel4(channels), [x, y, z, w]) => {
                    Some(ParameterValue::Channel4([
                        edited_channel(&channels[0], *x, local_frame),
                        edited_channel(&channels[1], *y, local_frame),
                        edited_channel(&channels[2], *z, local_frame),
                        edited_channel(&channels[3], *w, local_frame),
                    ]))
                }
                _ => None,
            }
        }
        // A curve edit replaces the whole control-point set. It only applies
        // to a parameter that already is a curve: a stale binding must never
        // retype a scalar parameter into a structural one.
        ParamInput::Curve(curve) => match existing {
            ParameterValue::Curve(_) => Some(ParameterValue::Curve(curve.clone())),
            _ => None,
        },
        // A ramp edit replaces the whole stop set, on the same terms and for
        // the same reason: a stale binding must never retype a parameter.
        ParamInput::Ramp(ramp) => match existing {
            ParameterValue::Ramp(_) => Some(ParameterValue::Ramp(ramp.clone())),
            _ => None,
        },
        ParamInput::Color { r, g, b, a } => match existing {
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

/// Whether any component of `value` carries keys that a write without a frame
/// would have to discard.
fn is_keyframed(value: &ParameterValue) -> bool {
    let keyed = |c: &AnimationChannel| matches!(c.source, ChannelSource::Keyframes(_));
    match value {
        ParameterValue::Channel(c) | ParameterValue::IntChannel(c) => keyed(c),
        ParameterValue::Channel2(cs) => cs.iter().any(keyed),
        ParameterValue::Channel3(cs) => cs.iter().any(keyed),
        ParameterValue::Channel4(cs) => cs.iter().any(keyed),
        ParameterValue::StringSteps(_) => true,
        _ => false,
    }
}

/// Whether any component is driven by something other than a constant or
/// keys: an expression, a node output, an audio source or a blend.
fn is_driven(value: &ParameterValue) -> bool {
    let driven = |c: &AnimationChannel| {
        !matches!(
            c.source,
            ChannelSource::Constant(_) | ChannelSource::Keyframes(_)
        )
    };
    match value {
        ParameterValue::Channel(c) | ParameterValue::IntChannel(c) => driven(c),
        ParameterValue::Channel2(cs) => cs.iter().any(driven),
        ParameterValue::Channel3(cs) => cs.iter().any(driven),
        ParameterValue::Channel4(cs) => cs.iter().any(driven),
        _ => false,
    }
}

/// The kind of thing a parameter holds, ignoring whether it is animated. A
/// write must stay inside the kind of the value it replaces.
fn family(value: &ParameterValue) -> u8 {
    match value {
        ParameterValue::Float(_) | ParameterValue::Channel(_) => 0,
        ParameterValue::Int(_) | ParameterValue::IntChannel(_) => 1,
        ParameterValue::Bool(_) => 2,
        ParameterValue::String(_) | ParameterValue::StringSteps(_) => 3,
        ParameterValue::Channel2(_) => 4,
        ParameterValue::Channel3(_) => 5,
        ParameterValue::Channel4(_) => 6,
        ParameterValue::Curve(_) => 7,
        ParameterValue::Ramp(_) => 8,
        _ => u8::MAX,
    }
}

/// Write `value` to parameter `key` of `node_id` in `graph`: clamp it, keep the
/// parameter's channel shape, then bring along everything the value decides
/// (`attribute.set`'s `value` follows its `type`; `layer.ref`'s output port
/// follows the port it references, resolved against `comp`) in one graph
/// snapshot.
///
/// Refuses a driven parameter (unless `cx.allow_driven_overwrite`), an unknown node or key, a value of another kind than the parameter
/// holds, and an animated parameter when `cx.local_frame` is `None` (a key
/// cannot be placed, and flattening would silently delete the animation).
pub fn set_parameter(
    graph: Graph,
    node_id: NodeId,
    key: &str,
    value: &ParamInput,
    cx: &EditContext,
    comp: Option<&Composition>,
) -> Result<Graph, EditError> {
    let node = graph
        .node(node_id)
        .ok_or(EditError::NodeNotFound(node_id))?;
    let existing = node
        .parameters
        .iter()
        .find(|p| p.key == key)
        .ok_or_else(|| EditError::UnknownParameter {
            node: node_id,
            key: key.to_string(),
        })?;
    let mismatch = || EditError::ParameterTypeMismatch {
        node: node_id,
        key: key.to_string(),
    };
    if !cx.allow_driven_overwrite && is_driven(&existing.value) {
        return Err(EditError::DrivenParameter {
            node: node_id,
            key: key.to_string(),
        });
    }
    let animated = is_keyframed(&existing.value);
    let range = cx.registry.param_range(&node.type_key, key);
    // Without a frame an animated parameter is judged as if keyed at 0: the
    // write is only used to tell a mismatch from an `AnimatedParameter`.
    let frame = if animated {
        cx.local_frame.or(Some(0))
    } else {
        cx.local_frame
    };
    let written = edited_param_value(&existing.value, value, range, frame).ok_or_else(mismatch)?;
    if family(&written) != family(&existing.value) {
        return Err(mismatch());
    }
    if animated && cx.local_frame.is_none() {
        return Err(EditError::AnimatedParameter {
            node: node_id,
            key: key.to_string(),
        });
    }
    // One write for the edit and everything it forces, so the node is never
    // left half-converted and the snapshot stays one undo step.
    let changed = Parameter {
        key: key.to_string(),
        value: written,
    };
    let mut updates = crate::registry::builtin::dependent_param_updates(node, &changed);
    let retypes = crate::registry::builtin::dependent_port_updates(node, &changed, comp);
    updates.insert(0, changed);
    Ok(graph.set_params_and_output_types(node_id, &updates, &retypes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::curve::KeyframeCurve;
    use crate::animation::interpolation::Interpolation;

    /// Every channel shape counts as driven when any one of its components is
    /// fed by something other than a constant or a curve.
    #[test]
    fn a_value_is_driven_when_any_component_is() {
        use crate::animation::channel::ParameterExpression;
        use crate::id::{NodeId, OutputPortIndex};
        let constant = || AnimationChannel::constant(1.0);
        let expression =
            || AnimationChannel::new(ChannelSource::Expression(ParameterExpression::new("frame")));
        let wired = AnimationChannel::new(ChannelSource::NodeOutput(
            NodeId::new(7),
            OutputPortIndex(0),
        ));
        let mut keyed = KeyframeCurve::new();
        keyed.insert(0, 1.0, Interpolation::Linear);
        let keyed = AnimationChannel::new(ChannelSource::Keyframes(keyed));

        assert!(!is_driven(&ParameterValue::Channel(constant())));
        assert!(!is_driven(&ParameterValue::Channel(keyed.clone())));
        assert!(!is_driven(&ParameterValue::Float(1.0)));
        assert!(is_driven(&ParameterValue::Channel(wired)));
        assert!(is_driven(&ParameterValue::IntChannel(expression())));
        assert!(!is_driven(&ParameterValue::Channel2([constant(), keyed])));
        assert!(is_driven(&ParameterValue::Channel2([
            constant(),
            expression()
        ])));
        assert!(is_driven(&ParameterValue::Channel3([
            constant(),
            constant(),
            expression()
        ])));
        assert!(is_driven(&ParameterValue::Channel4([
            expression(),
            constant(),
            constant(),
            constant()
        ])));
    }

    #[test]
    fn a_four_component_vector_writes_a_channel4() {
        let existing =
            ParameterValue::Channel4(std::array::from_fn(|_| AnimationChannel::constant(0.0)));
        let written = edited_param_value(
            &existing,
            &ParamInput::Vector(vec![1.0, 2.0, 3.0, 4.0]),
            None,
            None,
        );
        let Some(ParameterValue::Channel4(chs)) = written else {
            panic!("a 4-vector is accepted: {written:?}");
        };
        let values: Vec<f32> = chs
            .iter()
            .map(|c| match c.source {
                ChannelSource::Constant(v) => v,
                _ => f32::NAN,
            })
            .collect();
        assert_eq!(values, [1.0, 2.0, 3.0, 4.0]);
    }

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
            edited_param_value(&existing, &ParamInput::Int(9), None, Some(5))
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
            edited_param_value(&existing, &ParamInput::Int(9), None, Some(5))
        else {
            panic!("expected an int channel");
        };
        assert!(matches!(channel.source, ChannelSource::Constant(v) if v == 9.0));

        assert_eq!(
            edited_param_value(&ParameterValue::Int(6), &ParamInput::Int(9), None, Some(5)),
            Some(ParameterValue::Int(9))
        );
    }

    /// A string edit on a `StringSteps` writes the key at the frame and leaves
    /// the other keys alone; a plain `String` is replaced whole.
    #[test]
    fn string_edit_writes_one_step_key() {
        use crate::animation::StepCurve;
        let mut steps = StepCurve::new("seed".to_string());
        steps.insert(0, "first".to_string());
        steps.insert(10, "second".to_string());
        let existing = ParameterValue::StringSteps(steps);

        let Some(ParameterValue::StringSteps(written)) = edited_param_value(
            &existing,
            &ParamInput::String("edited".into()),
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
                &ParamInput::String("b".into()),
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
        use crate::animation::StepCurve;
        assert!(
            edited_param_value(
                &ParameterValue::IntChannel(AnimationChannel::constant(6.0)),
                &ParamInput::Float(9.0),
                None,
                Some(5),
            )
            .is_none()
        );
        assert!(
            edited_param_value(
                &ParameterValue::StringSteps(StepCurve::keyed(0, "a".to_string())),
                &ParamInput::Float(9.0),
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
                &ParamInput::Float(4.0),
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
            edited_param_value(&existing, &ParamInput::Float(4.0), None, Some(7))
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
            edited_param_value(&existing, &ParamInput::Float(4.0), None, Some(7))
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
        let value = ParamInput::Vector(vec![4.0, -2.0]);
        let Some(ParameterValue::Channel2(channels)) =
            edited_param_value(&existing, &value, None, None)
        else {
            panic!("expected Channel2");
        };
        assert!(matches!(channels[0].source, ChannelSource::Constant(4.0)));
        assert!(matches!(channels[1].source, ChannelSource::Constant(-2.0)));

        let wrong = ParamInput::Vector(vec![1.0, 2.0, 3.0]);
        assert!(edited_param_value(&existing, &wrong, None, None).is_none());
    }

    /// A curve edit writes the edited control points and refuses to retype a
    /// parameter that is not already a curve.
    #[test]
    fn curve_edits_replace_the_control_points_of_a_curve_parameter() {
        use crate::param_curve::CurveParam;
        let edited = CurveParam::linear([(0.0, 0.0), (0.5, 0.9), (1.0, 1.0)]);
        assert_eq!(
            edited_param_value(
                &ParameterValue::Curve(CurveParam::identity()),
                &ParamInput::Curve(edited.clone()),
                None,
                Some(7),
            ),
            Some(ParameterValue::Curve(edited.clone()))
        );
        assert!(
            edited_param_value(
                &ParameterValue::Float(1.0),
                &ParamInput::Curve(edited),
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
        use crate::param_ramp::RampParam;
        use crate::types::Color;
        let edited = RampParam::linear([(0.0, Color::WHITE), (0.5, Color::BLACK)]);
        assert_eq!(
            edited_param_value(
                &ParameterValue::Ramp(RampParam::default()),
                &ParamInput::Ramp(edited.clone()),
                None,
                Some(7),
            ),
            Some(ParameterValue::Ramp(edited.clone()))
        );
        assert!(
            edited_param_value(
                &ParameterValue::Float(1.0),
                &ParamInput::Ramp(edited),
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
        let value = ParamInput::Color {
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
