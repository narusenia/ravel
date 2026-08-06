// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Unified animation channel: the common value source for every parameter.
//!
//! A parameter's value can come from any [`ChannelSource`] — a constant, a
//! keyframe curve, a scalar expression, another node's output, an
//! audio-reactive analysis, or a blend of two sources — and these can be
//! swapped or composed without the consuming node knowing the difference
//! (REQ-CORE-007).
//!
//! The audio-reactive source is still a **placeholder** at this milestone: its
//! evaluation lands with the audio engine (MS5), and until then it returns
//! [`ChannelSource::DEFAULT_VALUE`] rather than panicking.

use std::sync::Arc;

use crate::animation::blend::BlendMode;
use crate::animation::curve::KeyframeCurve;
use crate::eval::EvalContext;
use crate::expression::{self, ExpressionError, Program, Scope};
use crate::id::{NodeId, OutputPortIndex};

/// An expression driving a parameter (REQ-CORE-014).
///
/// Holds the source text **and** the form it compiles to. Only the text is
/// persisted: the language's internal representation must not leak into the
/// `.ravprj` format, and a project that predates this type round-trips
/// unchanged because the serialized shape is still the single `source` field
/// it always was. Compiling happens when the value is constructed — which is
/// to say on load and on edit — so evaluation never parses.
///
/// A source that does not compile is kept verbatim and evaluates to the
/// channel default. That is deliberate: an author must be able to save a
/// half-written expression without the project refusing to load it, and the
/// error is what an editor shows them (EXPR-4). Nothing in the evaluation path
/// can fail, so nothing there has to handle it.
///
/// Both outcomes sit behind an `Arc` so that this stays two words wide. A
/// `ChannelSource` is a variant of `ParameterValue`, four of which make a
/// `Channel4`, so an inline instruction list would widen every parameter in
/// the graph — and it would make cloning a channel copy the compiled program,
/// which the immutable graph does constantly.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(from = "SerializedExpression", into = "SerializedExpression")]
pub struct ParameterExpression {
    source: String,
    /// `Err` when `source` does not compile; the error is for the editor.
    program: Result<Arc<Program>, Arc<ExpressionError>>,
}

/// The persisted shape of a [`ParameterExpression`]: the source, nothing else.
#[derive(serde::Serialize, serde::Deserialize)]
struct SerializedExpression {
    source: String,
}

impl ParameterExpression {
    /// Compile `source` against the parameter vocabulary.
    pub fn new(source: impl Into<String>) -> Self {
        let source = source.into();
        let program = expression::compile(&source, &Scope::parameter_context())
            .map(Arc::new)
            .map_err(Arc::new);
        Self { source, program }
    }

    /// The source text, exactly as the author wrote it.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Replace the source, recompiling.
    pub fn set_source(&mut self, source: impl Into<String>) {
        *self = Self::new(source);
    }

    /// The compiled program, or `None` when the source does not compile.
    pub fn program(&self) -> Option<&Program> {
        self.program.as_deref().ok()
    }

    /// Why the source does not compile, if it does not.
    pub fn error(&self) -> Option<&ExpressionError> {
        self.program.as_ref().err().map(|error| &**error)
    }

    /// Whether this expression's value changes as the frame position moves.
    ///
    /// `frame` and `time` are the same axis — `time` is `frame / fps` — so an
    /// expression reading either is time-varying and one reading neither is
    /// not. Everything else in the vocabulary (`fps`, the two resolutions) is
    /// already an axis of the evaluation cache's identity, so a program that
    /// reads only those keeps one value for the whole timeline.
    pub fn is_time_varying(&self) -> bool {
        self.program().is_some_and(|program| {
            let dependencies = program.dependencies();
            dependencies.references_variable("frame") || dependencies.references_variable("time")
        })
    }

    /// Evaluate at the continuous frame position `frame`.
    ///
    /// Returns [`ChannelSource::DEFAULT_VALUE`] when the source does not
    /// compile, when it holds no expression at all, and when the result is not
    /// a finite `f32`. **This is the one place non-finite values are dropped**
    /// (REQ-CORE-014): the language itself propagates `inf` and `NaN` as IEEE
    /// says, and the check happens after the narrowing to `f32` so that a
    /// finite `f64` too large to represent is caught as well.
    pub fn evaluate(&self, frame: f64, ctx: &EvalContext) -> f32 {
        let Some(program) = self.program() else {
            return ChannelSource::DEFAULT_VALUE;
        };
        if program.is_empty() {
            return ChannelSource::DEFAULT_VALUE;
        }
        let value = program.evaluate(&expression::parameter_values(frame, ctx)) as f32;
        if value.is_finite() {
            value
        } else {
            ChannelSource::DEFAULT_VALUE
        }
    }
}

impl Default for ParameterExpression {
    fn default() -> Self {
        Self::new(String::new())
    }
}

/// Equality is equality of the source. The compiled form is a function of it,
/// and comparing instruction lists would make two spellings of the same
/// program compare equal — which would be wrong for undo and for the cache
/// identity that hashes the source.
impl PartialEq for ParameterExpression {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for ParameterExpression {}

impl From<SerializedExpression> for ParameterExpression {
    fn from(serialized: SerializedExpression) -> Self {
        Self::new(serialized.source)
    }
}

impl From<ParameterExpression> for SerializedExpression {
    fn from(expression: ParameterExpression) -> Self {
        Self {
            source: expression.source,
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
    /// A scalar expression over the evaluation context (REQ-CORE-014).
    Expression(ParameterExpression),
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
    /// `frame` is a continuous frame position: keyframes stay anchored to
    /// integer frames, but sub-frame contexts (motion blur, time remapping)
    /// sample between them. Use [`EvalContext::sample_frame`] to derive it
    /// from a context.
    ///
    /// `ctx` carries the resolution, frame rate and composition basis an
    /// expression reads; node-output resolution consumes it in the evaluator.
    /// The signature returns a plain `f32` and always will: the expression
    /// language is total, so there is no error for the hundred-odd call sites
    /// to propagate.
    pub fn evaluate(&self, frame: f64, ctx: &EvalContext) -> f32 {
        match self {
            ChannelSource::Constant(v) => *v,
            ChannelSource::Keyframes(curve) => curve.sample(frame),
            ChannelSource::Expression(expression) => expression.evaluate(frame, ctx),
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

    /// Evaluate the channel value at the continuous frame position `frame`
    /// (see [`ChannelSource::evaluate`]).
    pub fn evaluate(&self, frame: f64, ctx: &EvalContext) -> f32 {
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
        assert!((ch.evaluate(0.0, &ctx()) - 4.2).abs() < f32::EPSILON);
        assert!((ch.evaluate(999.0, &ctx()) - 4.2).abs() < f32::EPSILON);
    }

    // ---- Keyframes --------------------------------------------------------

    #[test]
    fn keyframes_source_interpolates() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 1.0, Interpolation::Linear);
        let ch = AnimationChannel::keyframes(curve);
        assert!((ch.evaluate(5.0, &ctx()) - 0.5).abs() < 1e-4);
    }

    // ---- expressions ------------------------------------------------------

    fn expression(source: &str) -> AnimationChannel {
        AnimationChannel::new(ChannelSource::Expression(ParameterExpression::new(source)))
    }

    #[test]
    fn an_expression_reads_the_frame_position() {
        assert_eq!(expression("frame * 2").evaluate(7.0, &ctx()), 14.0);
    }

    #[test]
    fn an_expression_reads_time_fps_and_both_resolutions() {
        let ctx = EvalContext::new(0, FPS, (1920, 1080)).with_comp_resolution((1280, 720));
        // `time` is the frame position in seconds, so it tracks the sampled
        // position rather than `ctx.frame`.
        assert_eq!(expression("time").evaluate(15.0, &ctx), 0.5);
        assert_eq!(expression("fps").evaluate(0.0, &ctx), 30.0);
        assert_eq!(expression("res.width").evaluate(0.0, &ctx), 1920.0);
        assert_eq!(expression("res.height").evaluate(0.0, &ctx), 1080.0);
        assert_eq!(expression("comp.width").evaluate(0.0, &ctx), 1280.0);
        assert_eq!(expression("comp.height").evaluate(0.0, &ctx), 720.0);
        assert_eq!(
            expression("res.aspect * comp.aspect").evaluate(0.0, &ctx),
            (1920.0 / 1080.0) * (1280.0 / 720.0)
        );
    }

    #[test]
    fn an_oscillator_matches_the_value_computed_by_hand() {
        // The shape REQ-CORE-014 names: amplitude * sin(frame * frequency + phase).
        let channel = expression("100 * sin(frame * 0.25 + 0.5)");
        for frame in [0.0, 1.0, 7.0, 23.0] {
            let expected = 100.0 * (frame * 0.25 + 0.5f64).sin();
            assert!((channel.evaluate(frame, &ctx()) - expected as f32).abs() < 1e-4);
        }
    }

    #[test]
    fn a_sub_frame_position_evaluates_continuously() {
        // Motion blur and time remapping sample between integer frames; an
        // expression must interpolate there rather than step like a keyframe.
        let channel = expression("frame * 10");
        assert_eq!(channel.evaluate(2.5, &ctx()), 25.0);
        let midpoint = channel.evaluate(3.5, &ctx());
        let neighbours = 0.5 * (channel.evaluate(3.0, &ctx()) + channel.evaluate(4.0, &ctx()));
        assert!((midpoint - neighbours).abs() < 1e-4);
    }

    #[test]
    fn evaluation_is_deterministic() {
        // The three-tier cache keys on "same input, same output" (REQ-CORE-006).
        let channel = expression("noise(frame * 0.1, time) * 100");
        for frame in [0.0, 4.25, 91.5] {
            assert_eq!(
                channel.evaluate(frame, &ctx()),
                channel.evaluate(frame, &ctx())
            );
        }
    }

    #[test]
    fn an_expression_that_does_not_compile_returns_the_default() {
        for source in [
            "frame *",      // incomplete
            "unknown_name", // undefined variable
            "@P.x",         // attributes: not in a parameter scope
            "min(1)",       // wrong arity
            "1 $ 2",        // invalid token
        ] {
            let channel = expression(source);
            assert_eq!(
                channel.evaluate(7.0, &ctx()),
                ChannelSource::DEFAULT_VALUE,
                "`{source}` must fall back rather than fail"
            );
            let ChannelSource::Expression(expression) = &channel.source else {
                unreachable!("built as an expression");
            };
            assert!(expression.error().is_some(), "`{source}` must report why");
            assert_eq!(expression.source(), source, "the text is kept verbatim");
        }
    }

    #[test]
    fn an_empty_expression_returns_the_default() {
        // Clearing the box is "no expression", not a syntax error.
        assert_eq!(
            expression("").evaluate(7.0, &ctx()),
            ChannelSource::DEFAULT_VALUE
        );
        assert_eq!(
            expression("   ").evaluate(7.0, &ctx()),
            ChannelSource::DEFAULT_VALUE
        );
        assert!(ParameterExpression::new("").error().is_none());
        assert!(ParameterExpression::default().error().is_none());
    }

    #[test]
    fn a_non_finite_result_falls_to_the_default() {
        // The language propagates IEEE; the channel boundary is where that
        // stops, and it stops after the narrowing to f32 as well.
        for source in ["1 / 0", "-1 / 0", "sqrt(-1)", "log(0)", "pow(10, 400)"] {
            assert_eq!(
                expression(source).evaluate(7.0, &ctx()),
                ChannelSource::DEFAULT_VALUE,
                "`{source}` must not leak a non-finite value"
            );
        }
    }

    #[test]
    fn the_source_round_trips_through_the_project_format() {
        // The persisted shape is the single `source` field it has always been,
        // so a project written before expressions evaluated still loads.
        let source = ChannelSource::Expression(ParameterExpression::new("frame * 2 + 1"));
        let text = ron::to_string(&source).expect("serialize");
        assert_eq!(text, "Expression((source:\"frame * 2 + 1\"))");

        let restored: ChannelSource = ron::from_str(&text).expect("deserialize");
        assert_eq!(restored, source);
        // Deserializing compiles: loading a project must not leave every
        // expression inert until something edits it.
        assert_eq!(restored.evaluate(3.0, &ctx()), 7.0);
    }

    #[test]
    fn a_source_that_does_not_compile_still_round_trips() {
        let source = ChannelSource::Expression(ParameterExpression::new("frame *"));
        let text = ron::to_string(&source).expect("serialize");
        let restored: ChannelSource = ron::from_str(&text).expect("deserialize");
        assert_eq!(restored, source);
    }

    #[test]
    fn blend_composes_an_expression_with_another_source() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 10.0, Interpolation::Linear);
        let expression = Box::new(ChannelSource::Expression(ParameterExpression::new(
            "frame * 4",
        )));
        // At frame 5 the curve yields 5.0 and the expression 20.0; mix → 12.5.
        let ch = AnimationChannel::new(ChannelSource::Blend(
            Box::new(ChannelSource::Keyframes(curve)),
            expression,
            BlendMode::Mix,
            0.5,
        ));
        assert!((ch.evaluate(5.0, &ctx()) - 12.5).abs() < 1e-4);
    }

    #[test]
    fn time_varying_follows_the_time_axis_only() {
        let varying = |source: &str| ParameterExpression::new(source).is_time_varying();
        assert!(varying("frame * 2"));
        assert!(varying("sin(time)"));
        assert!(!varying("res.width / comp.width"));
        assert!(!varying("fps"));
        assert!(!varying("2 * pi"));
        // A source that does not compile evaluates to a constant default.
        assert!(!varying("frame *"));
    }

    // ---- placeholders -----------------------------------------------------

    #[test]
    fn audio_reactive_placeholder_returns_default() {
        let ch = AnimationChannel::new(ChannelSource::AudioReactive(
            AudioReactivePlaceholder::new("kick"),
        ));
        assert_eq!(ch.evaluate(7.0, &ctx()), ChannelSource::DEFAULT_VALUE);
    }

    #[test]
    fn node_output_returns_default() {
        let ch = AnimationChannel::new(ChannelSource::NodeOutput(
            NodeId::new(1),
            OutputPortIndex(0),
        ));
        assert_eq!(ch.evaluate(0.0, &ctx()), ChannelSource::DEFAULT_VALUE);
    }

    // ---- Blend ------------------------------------------------------------

    #[test]
    fn blend_of_two_constants() {
        let a = Box::new(ChannelSource::Constant(10.0));
        let b = Box::new(ChannelSource::Constant(20.0));
        let ch = AnimationChannel::new(ChannelSource::Blend(a, b, BlendMode::Mix, 0.5));
        assert!((ch.evaluate(0.0, &ctx()) - 15.0).abs() < f32::EPSILON);
    }

    #[test]
    fn blend_add_of_two_constants() {
        let a = Box::new(ChannelSource::Constant(10.0));
        let b = Box::new(ChannelSource::Constant(20.0));
        let ch = AnimationChannel::new(ChannelSource::Blend(a, b, BlendMode::Add, 1.0));
        assert!((ch.evaluate(0.0, &ctx()) - 30.0).abs() < f32::EPSILON);
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
        assert!((ch.evaluate(5.0, &ctx()) - 12.5).abs() < 1e-4);
    }
}
