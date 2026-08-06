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
//!
//! Two further rules follow from where an expression can sit.
//!
//! **An expression is found by walking the source, not by matching its top.**
//! [`ChannelSource::Blend`] composes two sources, and EXPR-2 makes
//! `Blend(Keyframes, Expression)` a state the core supports. Every operation
//! here therefore recurses through `Blend`: a component driven inside a blend
//! reads as driven, editing rewrites the expression **in place**, and
//! detaching freezes that expression alone and leaves the blend standing. A
//! blend holding two expressions edits the first in pre-order and freezes both
//! on detach, each at its own value — which is what keeps the blended result
//! continuous across the detach.
//!
//! **Attaching never overwrites a source that carries information.** Seeding
//! an expression replaces whatever drove the component, so doing that to a
//! keyframe curve would destroy the animation and leave the "return to a
//! constant or keyframes" operation with nothing to return to. Only a constant
//! — which the seeded literal reproduces exactly — is converted. Keyframes, a
//! node output, an audio source and a blend are left alone, and
//! [`can_attach`] is what a panel asks so it can say why the badge did not
//! light rather than silently doing nothing. Putting an expression *over*
//! keyframes the way After Effects does needs a `value` binding in the
//! parameter scope, which the expression language does not have; that is a
//! language and model change, not a panel one.

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
///
/// Found anywhere in the component's source, including inside a
/// [`ChannelSource::Blend`]; the first in pre-order when a blend holds
/// several.
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
    first_expression(&channel.source)
}

/// The first expression in `source`, in pre-order.
fn first_expression(source: &ChannelSource) -> Option<&ParameterExpression> {
    match source {
        ChannelSource::Expression(expression) => Some(expression),
        ChannelSource::Blend(a, b, _, _) => first_expression(a).or_else(|| first_expression(b)),
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

/// Whether attaching would give any component an expression.
///
/// False for a parameter whose every component already carries one, and for
/// one whose components are all driven by something an expression would have
/// to destroy. A panel asks this to decide whether the badge can be clicked at
/// all — see the module docs for why attaching refuses rather than overwrites.
pub fn can_attach(value: &ParameterValue) -> bool {
    channels_of(value).is_some_and(|channels| {
        channels
            .iter()
            .any(|channel| matches!(channel.source, ChannelSource::Constant(_)))
    })
}

/// Attach an expression to every component that can take one losslessly, each
/// seeded with the constant it already holds.
///
/// Seeding matters: an empty source means "no expression" and evaluates to the
/// channel default, so attaching a blank one would snap the parameter to zero
/// the instant the author asked for an expression. Writing the current value
/// as a literal makes attaching visually inert, which is what lets the author
/// edit from where they already are — and it is exact only because the
/// component was a constant.
///
/// A component driven by anything else keeps what it has: an already-attached
/// expression (the author's source is not overwritten with a literal), and
/// equally a keyframe curve, a node output, an audio source or a blend, none
/// of which a literal could reproduce.
///
/// `None` when the parameter cannot carry a channel **and when nothing
/// changed**, so a click that attaches nothing does not commit an empty undo
/// step. Seeding reads no frame and no context: a constant is the same number
/// at every frame, which is exactly why it is the only source converted.
pub fn attach(value: &ParameterValue) -> Option<ParameterValue> {
    let mut attached = false;
    let channels = channels_of(value)?
        .into_iter()
        .map(|channel| match &channel.source {
            ChannelSource::Constant(constant) => {
                attached = true;
                let literal = format_literal(*constant);
                AnimationChannel::new(ChannelSource::Expression(ParameterExpression::new(literal)))
            }
            _ => channel,
        })
        .collect();
    if !attached {
        return None;
    }
    rebuild(value, channels)
}

/// Replace one component's expression source, leaving the others untouched.
///
/// An expression already in the component is rewritten **where it sits**, so
/// editing the expression side of a blend keeps the blend, its mode, its
/// factor and its other source. A component holding a bare constant is
/// converted, so editing a component of a partially-driven vector works
/// without a separate attach step. **The source is stored whether or not it
/// compiles.**
///
/// `None` when the component is driven by something an expression would
/// destroy — the same refusal [`attach`] makes, for the same reason.
pub fn set_source(
    value: &ParameterValue,
    component: usize,
    source: &str,
) -> Option<ParameterValue> {
    let mut channels = channels_of(value)?;
    let channel = channels.get_mut(component)?;
    channel.source = replace_expression(&channel.source, source).or_else(|| {
        matches!(channel.source, ChannelSource::Constant(_))
            .then(|| ChannelSource::Expression(ParameterExpression::new(source)))
    })?;
    rebuild(value, channels)
}

/// Rewrite the first expression in `source` (pre-order), keeping every blend
/// it sits under. `None` when `source` holds no expression.
fn replace_expression(source: &ChannelSource, text: &str) -> Option<ChannelSource> {
    match source {
        ChannelSource::Expression(_) => {
            Some(ChannelSource::Expression(ParameterExpression::new(text)))
        }
        ChannelSource::Blend(a, b, mode, factor) => {
            if let Some(a) = replace_expression(a, text) {
                Some(ChannelSource::Blend(Box::new(a), b.clone(), *mode, *factor))
            } else {
                let b = replace_expression(b, text)?;
                Some(ChannelSource::Blend(a.clone(), Box::new(b), *mode, *factor))
            }
        }
        _ => None,
    }
}

/// Drop every expression, freezing each one at the value it shows now.
///
/// The frozen value comes from evaluating the expression, so detaching is as
/// visually inert as attaching: the parameter keeps the number that was on
/// screen. Only the expressions are replaced. A component that was not driven
/// by one is left exactly as it is, keyframes included — detaching must not
/// also throw away a neighbouring component's animation — and an expression
/// **inside a blend** is frozen where it sits, leaving the blend, its mode,
/// its factor and its other source standing. Collapsing the blend to a
/// constant instead would delete the source the author blended with, and would
/// jump the value at the moment of the click.
pub fn detach(value: &ParameterValue, frame: f64, ctx: &EvalContext) -> Option<ParameterValue> {
    let channels = channels_of(value)?
        .into_iter()
        .map(|channel| {
            if first_expression(&channel.source).is_none() {
                return channel;
            }
            AnimationChannel::new(freeze_expressions(&channel.source, frame, ctx))
        })
        .collect();
    rebuild(value, channels)
}

/// Replace every expression in `source` with the value it evaluates to,
/// keeping the surrounding blends.
fn freeze_expressions(source: &ChannelSource, frame: f64, ctx: &EvalContext) -> ChannelSource {
    match source {
        ChannelSource::Expression(expression) => {
            ChannelSource::Constant(expression.evaluate(frame, ctx))
        }
        ChannelSource::Blend(a, b, mode, factor) => ChannelSource::Blend(
            Box::new(freeze_expressions(a, frame, ctx)),
            Box::new(freeze_expressions(b, frame, ctx)),
            *mode,
            *factor,
        ),
        other => other.clone(),
    }
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
    use ravel_core::animation::blend::BlendMode;
    use ravel_core::animation::channel::AudioReactivePlaceholder;
    use ravel_core::animation::curve::KeyframeCurve;
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_core::id::{NodeId, OutputPortIndex};
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    fn source_of(value: &ParameterValue, component: usize) -> Option<String> {
        component_expression(value, component).map(|e| e.source().to_string())
    }

    fn expression_source(source: &str) -> ChannelSource {
        ChannelSource::Expression(ParameterExpression::new(source))
    }

    /// A curve rising 0 → 10 over frames 0 → 10.
    fn ramp() -> KeyframeCurve {
        let mut curve = KeyframeCurve::with_default(0.0);
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 10.0, Interpolation::Linear);
        curve
    }

    #[test]
    fn a_float_parameter_widens_to_a_channel_seeded_with_its_own_value() {
        let attached = attach(&ParameterValue::Float(0.5)).expect("attachable");

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
        let attached = attach(&value).expect("attachable");

        assert_eq!(source_of(&attached, 0).as_deref(), Some("1"));
        assert_eq!(source_of(&attached, 1).as_deref(), Some("-2.5"));
        assert_eq!(source_of(&attached, 2).as_deref(), Some("0"));
    }

    /// Seeding a literal over a curve would delete the animation, and the
    /// "return to a constant or keyframes" operation would have nothing to
    /// return to. The curve is kept and the click reports that it did nothing.
    #[test]
    fn attaching_refuses_to_overwrite_a_keyframed_component() {
        let value = ParameterValue::Channel(AnimationChannel::keyframes(ramp()));

        assert!(!can_attach(&value));
        assert_eq!(attach(&value), None);
    }

    /// Nor a node output, an audio source, or a blend: none is reproducible as
    /// a literal either.
    #[test]
    fn attaching_refuses_every_source_a_literal_cannot_reproduce() {
        for source in [
            ChannelSource::Keyframes(ramp()),
            ChannelSource::NodeOutput(NodeId::new(1), OutputPortIndex(0)),
            ChannelSource::AudioReactive(AudioReactivePlaceholder::new("kick")),
            ChannelSource::Blend(
                Box::new(ChannelSource::Keyframes(ramp())),
                Box::new(ChannelSource::Constant(3.0)),
                BlendMode::Mix,
                0.5,
            ),
        ] {
            let value = ParameterValue::Channel(AnimationChannel::new(source.clone()));
            assert!(!can_attach(&value), "{source:?} must not be attachable");
            assert_eq!(attach(&value), None, "{source:?} must be left alone");
        }
    }

    /// A vector where only some components are safe attaches those and leaves
    /// the rest — the badge lights, and no animation is lost.
    #[test]
    fn attaching_a_partly_keyframed_vector_converts_only_the_constants() {
        let value = ParameterValue::Channel2([
            AnimationChannel::keyframes(ramp()),
            AnimationChannel::constant(3.0),
        ]);

        assert!(can_attach(&value));
        let attached = attach(&value).expect("one component is attachable");

        assert_eq!(source_of(&attached, 0), None, "the curve is kept");
        let ParameterValue::Channel2(channels) = &attached else {
            panic!("expected a channel pair");
        };
        assert!(matches!(channels[0].source, ChannelSource::Keyframes(_)));
        assert_eq!(source_of(&attached, 1).as_deref(), Some("3"));
        assert!(has_expression(&attached));
    }

    #[test]
    fn attaching_leaves_an_existing_source_alone() {
        let value = ParameterValue::Channel2([
            AnimationChannel::new(expression_source("sin(time)")),
            AnimationChannel::constant(3.0),
        ]);
        let attached = attach(&value).expect("attachable");

        assert_eq!(source_of(&attached, 0).as_deref(), Some("sin(time)"));
        assert_eq!(source_of(&attached, 1).as_deref(), Some("3"));
    }

    /// Every component already driven: nothing to do, and reporting "nothing
    /// changed" is what keeps the click from committing an empty undo step.
    #[test]
    fn attaching_an_already_driven_parameter_changes_nothing() {
        let value = ParameterValue::Channel(AnimationChannel::new(expression_source("sin(time)")));

        assert!(!can_attach(&value));
        assert_eq!(attach(&value), None);
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
            assert!(!can_attach(&value));
            assert!(attach(&value).is_none());
            assert!(set_source(&value, 0, "1").is_none());
            assert!(detach(&value, 0.0, &ctx()).is_none());
        }
    }

    // ---- expressions inside a blend ---------------------------------------

    /// `Blend(Keyframes, Expression)` is a state EXPR-2 supports. A badge that
    /// only matched the top of the source would read it as undriven — and the
    /// click that follows would attach over the whole blend.
    #[test]
    fn an_expression_inside_a_blend_reads_as_driven() {
        let value = ParameterValue::Channel(AnimationChannel::new(ChannelSource::Blend(
            Box::new(ChannelSource::Keyframes(ramp())),
            Box::new(expression_source("frame * 4")),
            BlendMode::Mix,
            0.5,
        )));

        assert!(has_expression(&value));
        assert_eq!(source_of(&value, 0).as_deref(), Some("frame * 4"));
        // And it is not attachable, so the toggle detaches rather than
        // overwriting the blend.
        assert!(!can_attach(&value));
    }

    #[test]
    fn editing_an_expression_inside_a_blend_keeps_the_blend() {
        let value = ParameterValue::Channel(AnimationChannel::new(ChannelSource::Blend(
            Box::new(ChannelSource::Keyframes(ramp())),
            Box::new(expression_source("frame * 4")),
            BlendMode::Mix,
            0.5,
        )));

        let edited = set_source(&value, 0, "frame * 2").expect("editable");

        let ParameterValue::Channel(channel) = &edited else {
            panic!("expected a channel");
        };
        let ChannelSource::Blend(a, b, mode, factor) = &channel.source else {
            panic!("the blend must survive the edit");
        };
        assert!(matches!(**a, ChannelSource::Keyframes(_)), "curve kept");
        assert_eq!(**b, expression_source("frame * 2"));
        assert_eq!(*mode, BlendMode::Mix);
        assert_eq!(*factor, 0.5);
    }

    /// Detaching freezes the expression **where it sits**. Collapsing the
    /// blend would delete the curve the author blended with, and would jump
    /// the value at the moment of the click.
    #[test]
    fn detaching_freezes_an_expression_inside_a_blend_and_keeps_the_blend() {
        let value = ParameterValue::Channel(AnimationChannel::new(ChannelSource::Blend(
            Box::new(ChannelSource::Keyframes(ramp())),
            Box::new(expression_source("frame * 4")),
            BlendMode::Mix,
            0.5,
        )));
        // At frame 5 the curve yields 5.0 and the expression 20.0 → 12.5.
        let before = match &value {
            ParameterValue::Channel(channel) => channel.evaluate(5.0, &ctx()),
            _ => unreachable!(),
        };

        let detached = detach(&value, 5.0, &ctx()).expect("detachable");

        assert!(!has_expression(&detached));
        let ParameterValue::Channel(channel) = &detached else {
            panic!("expected a channel");
        };
        let ChannelSource::Blend(a, b, mode, factor) = &channel.source else {
            panic!("the blend must survive the detach");
        };
        assert!(matches!(**a, ChannelSource::Keyframes(_)), "curve kept");
        assert!(matches!(**b, ChannelSource::Constant(v) if v == 20.0));
        assert_eq!(*mode, BlendMode::Mix);
        assert_eq!(*factor, 0.5);
        // Visually inert at the frame it was detached on.
        assert_eq!(channel.evaluate(5.0, &ctx()), before);
    }

    /// Two expressions under one blend: editing rewrites the first in
    /// pre-order, and detaching freezes both — each at its own value, which is
    /// what keeps the blended result continuous.
    #[test]
    fn a_blend_of_two_expressions_edits_the_first_and_freezes_both() {
        let value = ParameterValue::Channel(AnimationChannel::new(ChannelSource::Blend(
            Box::new(expression_source("frame * 4")),
            Box::new(expression_source("frame * 2")),
            BlendMode::Mix,
            0.5,
        )));

        assert_eq!(source_of(&value, 0).as_deref(), Some("frame * 4"));

        let edited = set_source(&value, 0, "frame").expect("editable");
        let ParameterValue::Channel(channel) = &edited else {
            panic!("expected a channel");
        };
        let ChannelSource::Blend(a, b, _, _) = &channel.source else {
            panic!("the blend must survive the edit");
        };
        assert_eq!(**a, expression_source("frame"));
        assert_eq!(**b, expression_source("frame * 2"), "the second is kept");

        let detached = detach(&value, 5.0, &ctx()).expect("detachable");
        assert!(!has_expression(&detached));
        let ParameterValue::Channel(channel) = &detached else {
            panic!("expected a channel");
        };
        let ChannelSource::Blend(a, b, _, _) = &channel.source else {
            panic!("the blend must survive the detach");
        };
        assert!(matches!(**a, ChannelSource::Constant(v) if v == 20.0));
        assert!(matches!(**b, ChannelSource::Constant(v) if v == 10.0));
    }

    /// Editing is refused where attaching is: `set_source` must not become a
    /// back door that overwrites a curve the toggle declined to touch.
    #[test]
    fn editing_refuses_a_component_an_expression_would_destroy() {
        let value = ParameterValue::Channel(AnimationChannel::keyframes(ramp()));
        assert_eq!(set_source(&value, 0, "frame"), None);

        let blend = ParameterValue::Channel(AnimationChannel::new(ChannelSource::Blend(
            Box::new(ChannelSource::Keyframes(ramp())),
            Box::new(ChannelSource::Constant(3.0)),
            BlendMode::Mix,
            0.5,
        )));
        assert_eq!(set_source(&blend, 0, "frame"), None);
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
