// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Attaching, editing and detaching a parameter expression (REQ-CORE-014).
//!
//! The operations a Properties row offers, as pure functions over a
//! [`ParameterValue`]. Keeping them here rather than in the GPUI panel is what
//! makes them testable without a window, and it keeps the one rule that is
//! easy to get wrong in a single place:
//!
//! > **A source that does not compile is still stored.**
//!
//! [`ParameterExpression`] holds its source whether or not it compiled, and
//! only the source is persisted, so a half-typed expression survives a save
//! and reload. A panel that refused to commit a broken source would instead
//! throw the author's work away at the moment they most need it kept — while
//! they are still typing. Nothing here inspects
//! [`ParameterExpression::error`] before writing.

use ravel_core::animation::channel::{AnimationChannel, ChannelSource, ParameterExpression};
use ravel_core::eval::EvalContext;
use ravel_core::graph::ParameterValue;

/// How many animation channels a parameter carries, or `None` when it carries
/// none.
///
/// An expression lives in a [`ChannelSource`], so a parameter without channels
/// has nowhere to put one. `Float` counts as one: it converts to a channel on
/// attach, exactly as it does when it gains its first keyframe.
pub fn channel_count(value: &ParameterValue) -> Option<usize> {
    Some(match value {
        ParameterValue::Float(_) | ParameterValue::Channel(_) => 1,
        ParameterValue::Channel2(_) => 2,
        ParameterValue::Channel3(_) => 3,
        ParameterValue::Channel4(_) => 4,
        _ => return None,
    })
}

/// The expression driving one component, if one does.
pub fn component_expression(
    value: &ParameterValue,
    component: usize,
) -> Option<&ParameterExpression> {
    let channel = match value {
        ParameterValue::Channel(channel) if component == 0 => channel,
        ParameterValue::Channel2(channels) => channels.get(component)?,
        ParameterValue::Channel3(channels) => channels.get(component)?,
        ParameterValue::Channel4(channels) => channels.get(component)?,
        _ => return None,
    };
    match &channel.source {
        ChannelSource::Expression(expression) => Some(expression),
        _ => None,
    }
}

/// Whether any component of the parameter is driven by an expression.
///
/// This is what a row badge answers. It is deliberately "any" rather than
/// "all": a partially-driven vector is a state the model permits, and a badge
/// that only lit up for the fully-driven case would hide it.
pub fn has_expression(value: &ParameterValue) -> bool {
    (0..channel_count(value).unwrap_or(0))
        .any(|component| component_expression(value, component).is_some())
}

/// Attach an expression to every component, each seeded with the value that
/// component shows right now.
///
/// Seeding matters: an empty source means "no expression" and evaluates to the
/// channel default, so attaching a blank one would snap the parameter to zero
/// the instant the author asked for an expression. Writing the current value
/// as a literal makes attaching visually inert, which is what lets the author
/// edit from where they already are.
///
/// `None` when the parameter cannot carry a channel.
pub fn attach(value: &ParameterValue, frame: f64, ctx: &EvalContext) -> Option<ParameterValue> {
    let channels = channels_of(value)?
        .into_iter()
        .map(|channel| match &channel.source {
            // Already driven: leave the author's source alone rather than
            // overwriting it with a literal.
            ChannelSource::Expression(_) => channel,
            _ => {
                let literal = format_literal(channel.evaluate(frame, ctx));
                AnimationChannel::new(ChannelSource::Expression(ParameterExpression::new(literal)))
            }
        })
        .collect();
    rebuild(value, channels)
}

/// Replace one component's expression source, leaving the others untouched.
///
/// The component is converted to an expression channel if it was not one
/// already, so editing a component of a partially-driven vector works without
/// a separate attach step. **The source is stored whether or not it
/// compiles.**
pub fn set_source(
    value: &ParameterValue,
    component: usize,
    source: &str,
) -> Option<ParameterValue> {
    let mut channels = channels_of(value)?;
    let channel = channels.get_mut(component)?;
    channel.source = ChannelSource::Expression(ParameterExpression::new(source));
    rebuild(value, channels)
}

/// Drop every expression, freezing each component at the value it shows now.
///
/// The frozen value comes from evaluating the channel, so detaching is as
/// visually inert as attaching: the parameter keeps the number that was on
/// screen. A component that was not driven by an expression is left exactly as
/// it is, keyframes included — detaching an expression must not also throw
/// away a neighbouring component's animation.
pub fn detach(value: &ParameterValue, frame: f64, ctx: &EvalContext) -> Option<ParameterValue> {
    let channels = channels_of(value)?
        .into_iter()
        .map(|channel| match &channel.source {
            ChannelSource::Expression(_) => {
                AnimationChannel::constant(channel.evaluate(frame, ctx))
            }
            _ => channel,
        })
        .collect();
    rebuild(value, channels)
}

/// The parameter's channels, synthesising one for a bare `Float`.
fn channels_of(value: &ParameterValue) -> Option<Vec<AnimationChannel>> {
    Some(match value {
        ParameterValue::Float(constant) => vec![AnimationChannel::constant(*constant)],
        ParameterValue::Channel(channel) => vec![channel.clone()],
        ParameterValue::Channel2(channels) => channels.to_vec(),
        ParameterValue::Channel3(channels) => channels.to_vec(),
        ParameterValue::Channel4(channels) => channels.to_vec(),
        _ => return None,
    })
}

/// Put `channels` back into the shape `original` had, widening a `Float` to a
/// `Channel` the way the keyframe path does.
fn rebuild(original: &ParameterValue, channels: Vec<AnimationChannel>) -> Option<ParameterValue> {
    let mut channels = channels.into_iter();
    Some(match original {
        ParameterValue::Float(_) | ParameterValue::Channel(_) => {
            ParameterValue::Channel(channels.next()?)
        }
        ParameterValue::Channel2(_) => ParameterValue::Channel2(collect_array(channels.collect())?),
        ParameterValue::Channel3(_) => ParameterValue::Channel3(collect_array(channels.collect())?),
        ParameterValue::Channel4(_) => ParameterValue::Channel4(collect_array(channels.collect())?),
        _ => return None,
    })
}

fn collect_array<const N: usize>(channels: Vec<AnimationChannel>) -> Option<[AnimationChannel; N]> {
    channels.try_into().ok()
}

/// A finite `f32` as an expression source that evaluates back to it.
///
/// `ParameterExpression::evaluate` never answers a non-finite value, so the
/// input is finite and the shortest round-tripping form Rust prints is always
/// a valid number literal in the expression grammar.
fn format_literal(value: f32) -> String {
    format!("{value}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::animation::curve::KeyframeCurve;
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    fn source_of(value: &ParameterValue, component: usize) -> Option<String> {
        component_expression(value, component).map(|e| e.source().to_string())
    }

    #[test]
    fn a_float_parameter_widens_to_a_channel_seeded_with_its_own_value() {
        let attached = attach(&ParameterValue::Float(0.5), 0.0, &ctx()).expect("attachable");

        assert_eq!(source_of(&attached, 0).as_deref(), Some("0.5"));
        assert!(has_expression(&attached));
        // Inert: the value on screen does not move when the expression is
        // attached.
        let ParameterValue::Channel(channel) = &attached else {
            panic!("expected a channel");
        };
        assert_eq!(channel.evaluate(0.0, &ctx()), 0.5);
    }

    #[test]
    fn attaching_seeds_each_component_from_its_own_current_value() {
        let value = ParameterValue::Channel3([
            AnimationChannel::constant(1.0),
            AnimationChannel::constant(-2.5),
            AnimationChannel::constant(0.0),
        ]);
        let attached = attach(&value, 0.0, &ctx()).expect("attachable");

        assert_eq!(source_of(&attached, 0).as_deref(), Some("1"));
        assert_eq!(source_of(&attached, 1).as_deref(), Some("-2.5"));
        assert_eq!(source_of(&attached, 2).as_deref(), Some("0"));
    }

    #[test]
    fn attaching_seeds_a_keyframed_component_from_its_sample_at_the_frame() {
        let mut curve = KeyframeCurve::with_default(0.0);
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 10.0, Interpolation::Linear);
        let value = ParameterValue::Channel(AnimationChannel::keyframes(curve));

        let attached = attach(&value, 5.0, &ctx()).expect("attachable");

        assert_eq!(source_of(&attached, 0).as_deref(), Some("5"));
    }

    #[test]
    fn attaching_leaves_an_existing_source_alone() {
        let value = ParameterValue::Channel2([
            AnimationChannel::new(ChannelSource::Expression(ParameterExpression::new(
                "sin(time)",
            ))),
            AnimationChannel::constant(3.0),
        ]);
        let attached = attach(&value, 0.0, &ctx()).expect("attachable");

        assert_eq!(source_of(&attached, 0).as_deref(), Some("sin(time)"));
        assert_eq!(source_of(&attached, 1).as_deref(), Some("3"));
    }

    /// The rule the whole module exists to hold: a broken source is stored,
    /// not refused. Losing it would mean the author cannot stop typing
    /// mid-expression.
    #[test]
    fn a_source_that_does_not_compile_is_still_stored() {
        let value = ParameterValue::Channel(AnimationChannel::constant(0.0));
        let edited = set_source(&value, 0, "frame *").expect("editable");

        let expression = component_expression(&edited, 0).expect("stored");
        assert_eq!(expression.source(), "frame *");
        assert!(expression.error().is_some(), "and the error is available");
        // It evaluates to the channel default rather than failing.
        let ParameterValue::Channel(channel) = &edited else {
            panic!("expected a channel");
        };
        assert_eq!(channel.evaluate(0.0, &ctx()), ChannelSource::DEFAULT_VALUE);
    }

    #[test]
    fn editing_one_component_leaves_the_others_untouched() {
        let value = ParameterValue::Channel2([
            AnimationChannel::constant(1.0),
            AnimationChannel::constant(2.0),
        ]);
        let edited = set_source(&value, 1, "frame * 2").expect("editable");

        assert_eq!(source_of(&edited, 0), None, "component 0 stays constant");
        assert_eq!(source_of(&edited, 1).as_deref(), Some("frame * 2"));
    }

    #[test]
    fn detaching_freezes_the_value_that_was_on_screen() {
        let value = ParameterValue::Channel(AnimationChannel::new(ChannelSource::Expression(
            ParameterExpression::new("frame * 2"),
        )));
        let detached = detach(&value, 4.0, &ctx()).expect("detachable");

        assert!(!has_expression(&detached));
        let ParameterValue::Channel(channel) = &detached else {
            panic!("expected a channel");
        };
        assert!(matches!(channel.source, ChannelSource::Constant(v) if v == 8.0));
    }

    #[test]
    fn detaching_keeps_a_neighbouring_components_keyframes() {
        let mut curve = KeyframeCurve::with_default(0.0);
        curve.insert(0, 4.0, Interpolation::Linear);
        let value = ParameterValue::Channel2([
            AnimationChannel::new(ChannelSource::Expression(ParameterExpression::new("1 + 1"))),
            AnimationChannel::keyframes(curve),
        ]);
        let detached = detach(&value, 0.0, &ctx()).expect("detachable");

        let ParameterValue::Channel2(channels) = &detached else {
            panic!("expected a channel pair");
        };
        assert!(matches!(channels[0].source, ChannelSource::Constant(v) if v == 2.0));
        assert!(matches!(channels[1].source, ChannelSource::Keyframes(_)));
    }

    #[test]
    fn a_parameter_without_channels_cannot_carry_an_expression() {
        for value in [
            ParameterValue::Int(1),
            ParameterValue::Bool(true),
            ParameterValue::String("x".into()),
        ] {
            assert_eq!(channel_count(&value), None);
            assert!(!has_expression(&value));
            assert!(attach(&value, 0.0, &ctx()).is_none());
            assert!(set_source(&value, 0, "1").is_none());
            assert!(detach(&value, 0.0, &ctx()).is_none());
        }
    }

    #[test]
    fn a_component_out_of_range_is_refused_rather_than_wrapping() {
        let value = ParameterValue::Channel2([
            AnimationChannel::constant(0.0),
            AnimationChannel::constant(0.0),
        ]);
        assert!(set_source(&value, 2, "1").is_none());
        assert_eq!(component_expression(&value, 2), None);
    }
}
