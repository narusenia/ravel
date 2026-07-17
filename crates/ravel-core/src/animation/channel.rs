// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Unified animation channel: the common value source for every parameter.
//!
//! A parameter's value can come from any [`ChannelSource`] — a constant, a
//! keyframe curve, a Lua expression, another node's output, an audio-reactive
//! analysis, or a blend of two sources — and these can be swapped or composed
//! without the consuming node knowing the difference (REQ-CORE-007).
//!
//! Expression and audio-reactive sources are **placeholders** at this
//! milestone: their full evaluation lands with the Lua runtime (MS6) and the
//! audio engine (MS5) respectively. Until then they return
//! [`ChannelSource::DEFAULT_VALUE`] rather than panicking.

use crate::animation::blend::BlendMode;
use crate::animation::curve::KeyframeCurve;
use crate::eval::EvalContext;
use crate::id::{NodeId, OutputPortIndex};

/// Placeholder for a Lua expression source (full evaluation arrives in MS6).
///
/// The expression text is retained so existing projects round-trip once the
/// scripting runtime is wired up.
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ExpressionPlaceholder {
    /// Raw Lua source the expression will eventually evaluate.
    pub source: String,
}

impl ExpressionPlaceholder {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
        }
    }
}

/// Placeholder for an audio-reactive source (full evaluation arrives in MS5).
///
/// The reference identifies the audio analysis the source will sample from.
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct AudioReactivePlaceholder {
    /// Identifier of the audio analysis to sample.
    pub reference: String,
}

impl AudioReactivePlaceholder {
    pub fn new(reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
        }
    }
}

/// The value source backing an [`AnimationChannel`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ChannelSource {
    /// A fixed value.
    Constant(f32),
    /// A keyframe curve sampled by frame.
    Keyframes(KeyframeCurve),
    /// A Lua expression (placeholder — evaluates to the default value).
    Expression(ExpressionPlaceholder),
    /// Another node's output port value.
    ///
    /// Resolution requires a graph-evaluation context and is deferred to a
    /// later milestone; until then this evaluates to the default value.
    NodeOutput(NodeId, OutputPortIndex),
    /// An audio-reactive source (placeholder — evaluates to the default value).
    AudioReactive(AudioReactivePlaceholder),
    /// A blend of two sources combined by `mode` at `factor`.
    Blend(Box<ChannelSource>, Box<ChannelSource>, BlendMode, f32),
}

impl ChannelSource {
    /// Value returned by placeholder and not-yet-resolvable sources.
    pub const DEFAULT_VALUE: f32 = 0.0;

    /// Evaluate this source at `frame` within the evaluation context `ctx`.
    ///
    /// `ctx` is currently only threaded through `Blend` recursion; it is part
    /// of the stable signature so expression and node-output resolution can
    /// consume it without an API break in a later milestone.
    #[allow(clippy::only_used_in_recursion)]
    pub fn evaluate(&self, frame: u64, ctx: &EvalContext) -> f32 {
        match self {
            ChannelSource::Constant(v) => *v,
            ChannelSource::Keyframes(curve) => curve.sample(frame),
            // Placeholder — see module docs.
            ChannelSource::Expression(_) => Self::DEFAULT_VALUE,
            // Resolving a node output needs a graph context (future work).
            ChannelSource::NodeOutput(_, _) => Self::DEFAULT_VALUE,
            // Placeholder — see module docs.
            ChannelSource::AudioReactive(_) => Self::DEFAULT_VALUE,
            ChannelSource::Blend(a, b, mode, factor) => {
                mode.blend(a.evaluate(frame, ctx), b.evaluate(frame, ctx), *factor)
            }
        }
    }
}

/// A parameter's unified animation channel.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AnimationChannel {
    pub source: ChannelSource,
}

impl AnimationChannel {
    /// Wrap an arbitrary [`ChannelSource`].
    pub fn new(source: ChannelSource) -> Self {
        Self { source }
    }

    /// Convenience constructor for a constant channel.
    pub fn constant(value: f32) -> Self {
        Self::new(ChannelSource::Constant(value))
    }

    /// Convenience constructor for a keyframed channel.
    pub fn keyframes(curve: KeyframeCurve) -> Self {
        Self::new(ChannelSource::Keyframes(curve))
    }

    /// Evaluate the channel value at `frame`.
    pub fn evaluate(&self, frame: u64, ctx: &EvalContext) -> f32 {
        self.source.evaluate(frame, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::interpolation::Interpolation;
    use crate::types::FrameRate;

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };

    fn ctx() -> EvalContext {
        EvalContext::new(0, FPS, (1920, 1080))
    }

    // ---- Constant ---------------------------------------------------------

    #[test]
    fn constant_returns_fixed_value() {
        let ch = AnimationChannel::constant(4.2);
        assert!((ch.evaluate(0, &ctx()) - 4.2).abs() < f32::EPSILON);
        assert!((ch.evaluate(999, &ctx()) - 4.2).abs() < f32::EPSILON);
    }

    // ---- Keyframes --------------------------------------------------------

    #[test]
    fn keyframes_source_interpolates() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 1.0, Interpolation::Linear);
        let ch = AnimationChannel::keyframes(curve);
        assert!((ch.evaluate(5, &ctx()) - 0.5).abs() < 1e-4);
    }

    // ---- placeholders -----------------------------------------------------

    #[test]
    fn expression_placeholder_returns_default() {
        let ch = AnimationChannel::new(ChannelSource::Expression(ExpressionPlaceholder::new(
            "frame * 2",
        )));
        assert_eq!(ch.evaluate(7, &ctx()), ChannelSource::DEFAULT_VALUE);
    }

    #[test]
    fn audio_reactive_placeholder_returns_default() {
        let ch = AnimationChannel::new(ChannelSource::AudioReactive(
            AudioReactivePlaceholder::new("kick"),
        ));
        assert_eq!(ch.evaluate(7, &ctx()), ChannelSource::DEFAULT_VALUE);
    }

    #[test]
    fn node_output_returns_default() {
        let ch = AnimationChannel::new(ChannelSource::NodeOutput(
            NodeId::new(1),
            OutputPortIndex(0),
        ));
        assert_eq!(ch.evaluate(0, &ctx()), ChannelSource::DEFAULT_VALUE);
    }

    // ---- Blend ------------------------------------------------------------

    #[test]
    fn blend_of_two_constants() {
        let a = Box::new(ChannelSource::Constant(10.0));
        let b = Box::new(ChannelSource::Constant(20.0));
        let ch = AnimationChannel::new(ChannelSource::Blend(a, b, BlendMode::Mix, 0.5));
        assert!((ch.evaluate(0, &ctx()) - 15.0).abs() < f32::EPSILON);
    }

    #[test]
    fn blend_add_of_two_constants() {
        let a = Box::new(ChannelSource::Constant(10.0));
        let b = Box::new(ChannelSource::Constant(20.0));
        let ch = AnimationChannel::new(ChannelSource::Blend(a, b, BlendMode::Add, 1.0));
        assert!((ch.evaluate(0, &ctx()) - 30.0).abs() < f32::EPSILON);
    }

    #[test]
    fn nested_blend_with_keyframes() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 10.0, Interpolation::Linear);
        let a = Box::new(ChannelSource::Keyframes(curve));
        let b = Box::new(ChannelSource::Constant(20.0));
        // At frame 5 the curve yields 5.0; mix with 20.0 at 0.5 → 12.5.
        let ch = AnimationChannel::new(ChannelSource::Blend(a, b, BlendMode::Mix, 0.5));
        assert!((ch.evaluate(5, &ctx()) - 12.5).abs() < 1e-4);
    }
}
