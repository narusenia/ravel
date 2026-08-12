// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Colour ramps stored as parameter values.
//!
//! A [`RampParam`] maps one scalar position to one RGBA colour through an
//! ordered set of stops. It is the structural parameter behind
//! [`ParameterValue::Ramp`](crate::graph::ParameterValue::Ramp) and is shared
//! by every ramp consumer — `field.ramp` today, the value-domain `color.ramp`
//! later — so the same stops always produce the same colour.
//!
//! # Relationship to [`CurveParam`](crate::param_curve::CurveParam)
//!
//! The two are the same shape of type: an ordered set of control values keyed
//! by an arbitrary scalar, with the invariant that the keys are **sorted,
//! unique and finite**, enforced by every constructor *and* by a hand-written
//! [`Deserialize`](serde::Deserialize). They differ in what they carry (one
//! output float versus four) and in their interpolation vocabulary — a ramp
//! has no tangents, so [`RampInterpolation`] is a property of the whole ramp
//! rather than of each stop.
//!
//! # Domain
//!
//! Outside `[first.position, last.position]` the ramp **clamps** to the
//! nearest end stop's colour, exactly as `CurveParam` clamps to its end
//! outputs. Repeat and mirror modes belong to the *node* that reads the ramp.
//!
//! Unlike a curve, a ramp is **never empty**: a curve with no points is the
//! identity, which is a meaningful answer, but a ramp with no stops has no
//! colour to give. Every path that would produce an empty ramp — the
//! constructors, `Deserialize` — falls back to [`RampParam::black_to_white`].

use crate::types::Color;

/// One stop of a [`RampParam`]: a position on the ramp and the colour there.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RampStop {
    /// Position this stop is anchored at, normally within `0..=1`.
    pub position: f32,
    /// Colour produced at `position` (linear RGBA, like every other colour
    /// in the core).
    pub color: Color,
}

impl RampStop {
    pub const fn new(position: f32, color: Color) -> Self {
        Self { position, color }
    }

    /// Whether this stop's position and colour are all finite.
    ///
    /// A stop with a non-finite position cannot be ordered against the
    /// others, which is what [`RampParam`]'s binary searches rely on, and a
    /// non-finite channel poisons every sample of the segments it touches.
    /// Such a stop is dropped where one can arrive from outside the
    /// constructors — deserialization.
    pub fn is_finite(&self) -> bool {
        self.position.is_finite()
            && self.color.r.is_finite()
            && self.color.g.is_finite()
            && self.color.b.is_finite()
            && self.color.a.is_finite()
    }
}

/// How a [`RampParam`] fills the span between two stops.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RampInterpolation {
    /// Straight blend from the left stop's colour to the right stop's.
    #[default]
    Linear,
    /// The same blend eased with smoothstep, so the derivative is zero at
    /// both stops and the banding a linear ramp shows is broken up.
    Smooth,
    /// No blend: the nearest stop at or before the position holds until the
    /// next stop, which makes the ramp a set of hard bands.
    Constant,
}

/// An ordered colour ramp: scalar position → RGBA colour.
///
/// Stops are kept sorted ascending by `position` with unique, finite
/// positions, so evaluation is a binary search plus one segment blend. Every
/// constructor **and the deserializer** preserves that invariant, and none of
/// them can produce a ramp with no stops.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct RampParam {
    /// Stops, always sorted ascending by `position` with unique positions,
    /// and never empty.
    stops: Vec<RampStop>,
    /// How the spans between stops are filled.
    interpolation: RampInterpolation,
}

/// Deserialization normalizes instead of trusting the input.
///
/// The reasoning is [`CurveParam`](crate::param_curve::CurveParam)'s, term for
/// term: a `.ravprj` is a text file that can be hand-edited, merged or
/// truncated, and a derived `Deserialize` would hand [`RampParam`] a `stops`
/// vector that is unsorted, repeats a position, holds `NaN`, or is empty — all
/// of which break the `partition_point` and `binary_search_by` that
/// [`RampParam::evaluate`] is built on, yielding silently wrong colours rather
/// than an error. Reading through the same normalization the constructors use
/// costs one pass and makes every `RampParam` in the process valid by
/// construction:
///
/// * non-finite stops ([`RampStop::is_finite`]) are **dropped**;
/// * the rest are **sorted** by position;
/// * stops repeating a position **collapse to the last one**;
/// * a ramp left with no stops becomes [`RampParam::black_to_white`].
impl<'de> serde::Deserialize<'de> for RampParam {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Mirrors the derived `Serialize` wire form exactly (the same two
        // fields under the same struct name), so RON, bincode and any other
        // format keep round-tripping.
        #[derive(serde::Deserialize)]
        #[serde(rename = "RampParam")]
        struct Stored {
            stops: Vec<RampStop>,
            interpolation: RampInterpolation,
        }

        let stored = Stored::deserialize(deserializer)?;
        Ok(Self::new(stored.stops, stored.interpolation))
    }
}

impl Default for RampParam {
    fn default() -> Self {
        Self::black_to_white()
    }
}

impl RampParam {
    /// The default ramp: black at `0`, white at `1`, linearly interpolated.
    ///
    /// This is what a template declares and what every path that would
    /// otherwise produce an empty ramp falls back to. Black → white rather
    /// than a single white stop because a ramp node whose default is one flat
    /// colour looks broken: the point of the node is that the output varies
    /// with the input, and the neutral way to say that is the full range.
    pub fn black_to_white() -> Self {
        Self::linear([(0.0, Color::BLACK), (1.0, Color::WHITE)])
    }

    /// A ramp through `stops` with linear interpolation.
    ///
    /// Stops may be supplied in any order; duplicates of one position
    /// collapse to the last one given.
    pub fn linear(stops: impl IntoIterator<Item = (f32, Color)>) -> Self {
        Self::new(
            stops
                .into_iter()
                .map(|(position, color)| RampStop::new(position, color)),
            RampInterpolation::Linear,
        )
    }

    /// A ramp from stops and an interpolation mode, sorted on the way in.
    ///
    /// Stops may be supplied in any order; duplicates of one position
    /// collapse to the last one given, non-finite stops are dropped, and an
    /// input that leaves no stops at all yields [`Self::black_to_white`].
    pub fn new(
        stops: impl IntoIterator<Item = RampStop>,
        interpolation: RampInterpolation,
    ) -> Self {
        let mut sorted: Vec<RampStop> = Vec::new();
        for stop in stops {
            if !stop.is_finite() {
                continue;
            }
            match sorted.binary_search_by(|existing| existing.position.total_cmp(&stop.position)) {
                Ok(i) => sorted[i] = stop,
                Err(i) => sorted.insert(i, stop),
            }
        }
        if sorted.is_empty() {
            // Not recursion: `black_to_white` builds through this function
            // with two finite stops, which cannot reach this branch.
            return Self::black_to_white().with_interpolation(interpolation);
        }
        Self {
            stops: sorted,
            interpolation,
        }
    }

    /// Builder: replace the interpolation mode.
    pub fn with_interpolation(mut self, interpolation: RampInterpolation) -> Self {
        self.interpolation = interpolation;
        self
    }

    /// Read-only view of the stops (sorted by position, never empty).
    pub fn stops(&self) -> &[RampStop] {
        &self.stops
    }

    /// Number of stops. Always at least one.
    pub fn len(&self) -> usize {
        self.stops.len()
    }

    /// Always `false`; a ramp cannot have no stops. Present because clippy
    /// asks any type with `len` for it, and because a caller reading the pair
    /// should see the invariant rather than infer it.
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn interpolation(&self) -> RampInterpolation {
        self.interpolation
    }

    /// The colour this ramp gives at `position`.
    ///
    /// * At or before the first stop → the first colour (clamp).
    /// * At or after the last stop → the last colour (clamp).
    /// * Exact stop hit → that stop's colour.
    /// * Otherwise → the span's blend, per [`RampInterpolation`].
    ///
    /// A non-finite position takes the same route `CurveParam::evaluate`
    /// takes: `-inf` reads as the first colour, `+inf` and `NaN` as the last.
    /// Fields sample on a worker thread, so this must answer rather than
    /// panic.
    pub fn evaluate(&self, position: f32) -> Color {
        // Both `expect`s: `new` is the only constructor and it never leaves
        // the vector empty.
        let first = *self.stops.first().expect("a ramp has at least one stop");
        let last = *self.stops.last().expect("a ramp has at least one stop");
        if position <= first.position {
            return first.color;
        }
        // `NaN` compares false against both bounds, so it would reach
        // `partition_point` below, take the empty prefix, and index `0 - 1`.
        if position >= last.position || position.is_nan() {
            return last.color;
        }

        // First stop at or after `position`. Both ends were handled above, so
        // this lands in `1..len` and the span `[idx - 1, idx]` exists.
        let idx = self.stops.partition_point(|stop| stop.position < position);
        let right = self.stops[idx];
        if right.position == position {
            return right.color;
        }
        let left = self.stops[idx - 1];

        let span = right.position - left.position;
        // Unreachable through the public API (positions are unique), but a
        // hand-edited file could store a zero-width span; do not divide by it.
        let t = if span > 0.0 {
            (position - left.position) / span
        } else {
            0.0
        };
        match self.interpolation {
            RampInterpolation::Constant => left.color,
            RampInterpolation::Linear => mix(left.color, right.color, t),
            RampInterpolation::Smooth => mix(left.color, right.color, t * t * (3.0 - 2.0 * t)),
        }
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::new(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        a.a + (b.a - a.a) * t,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color::new(1.0, 0.0, 0.0, 1.0);
    const BLUE: Color = Color::new(0.0, 0.0, 1.0, 1.0);

    fn close(a: Color, b: Color) -> bool {
        (a.r - b.r).abs() < 1e-6
            && (a.g - b.g).abs() < 1e-6
            && (a.b - b.b).abs() < 1e-6
            && (a.a - b.a).abs() < 1e-6
    }

    #[test]
    fn the_default_ramp_runs_black_to_white() {
        let ramp = RampParam::default();
        assert_eq!(ramp, RampParam::black_to_white());
        assert_eq!(ramp.evaluate(0.0), Color::BLACK);
        assert_eq!(ramp.evaluate(1.0), Color::WHITE);
        assert!(close(ramp.evaluate(0.5), Color::new(0.5, 0.5, 0.5, 1.0)));
    }

    /// The completion criterion: known stops give a known colour at a known
    /// position.
    #[test]
    fn a_known_position_reads_the_expected_colour() {
        let ramp = RampParam::linear([(0.0, RED), (1.0, BLUE)]);
        assert!(close(ramp.evaluate(0.25), Color::new(0.75, 0.0, 0.25, 1.0)));
        assert!(close(ramp.evaluate(0.5), Color::new(0.5, 0.0, 0.5, 1.0)));
        assert_eq!(ramp.evaluate(0.0), RED);
        assert_eq!(ramp.evaluate(1.0), BLUE);
    }

    #[test]
    fn a_single_stop_is_one_colour_everywhere() {
        let ramp = RampParam::linear([(0.5, RED)]);
        assert_eq!(ramp.len(), 1);
        for position in [-10.0, 0.0, 0.5, 0.9, 100.0] {
            assert_eq!(ramp.evaluate(position), RED, "at {position}");
        }
    }

    #[test]
    fn out_of_domain_positions_clamp_to_the_end_colours() {
        let ramp = RampParam::linear([(0.0, RED), (0.5, Color::WHITE), (1.0, BLUE)]);
        assert_eq!(ramp.evaluate(-3.0), RED);
        assert_eq!(ramp.evaluate(7.0), BLUE);
    }

    /// A field can hand the ramp a non-finite sample (an expression field, a
    /// division by zero). It answers what `CurveParam::evaluate` answers.
    #[test]
    fn non_finite_positions_return_an_end_colour() {
        let ramp = RampParam::linear([(0.0, RED), (1.0, BLUE)]);
        assert_eq!(ramp.evaluate(f32::NEG_INFINITY), RED);
        assert_eq!(ramp.evaluate(f32::INFINITY), BLUE);
        assert_eq!(ramp.evaluate(f32::NAN), BLUE);
    }

    #[test]
    fn constant_interpolation_holds_the_left_stop() {
        let ramp = RampParam::linear([(0.0, RED), (0.5, Color::WHITE), (1.0, BLUE)])
            .with_interpolation(RampInterpolation::Constant);
        assert_eq!(ramp.evaluate(0.0), RED);
        assert_eq!(ramp.evaluate(0.49), RED);
        assert_eq!(ramp.evaluate(0.5), Color::WHITE, "the stop itself wins");
        assert_eq!(ramp.evaluate(0.99), Color::WHITE);
        assert_eq!(ramp.evaluate(1.0), BLUE);
    }

    /// Smooth is the same blend eased: it agrees with linear at the stops and
    /// at the midpoint, and lags it in the first half.
    #[test]
    fn smooth_interpolation_eases_between_the_same_stops() {
        let smooth = RampParam::linear([(0.0, RED), (1.0, BLUE)])
            .with_interpolation(RampInterpolation::Smooth);
        let linear = RampParam::linear([(0.0, RED), (1.0, BLUE)]);
        assert_eq!(smooth.evaluate(0.0), RED);
        assert_eq!(smooth.evaluate(1.0), BLUE);
        assert!(close(smooth.evaluate(0.5), linear.evaluate(0.5)));
        assert!(smooth.evaluate(0.25).b < linear.evaluate(0.25).b);
        assert!(smooth.evaluate(0.75).b > linear.evaluate(0.75).b);
    }

    #[test]
    fn stops_are_sorted_on_construction() {
        let ramp = RampParam::linear([(1.0, BLUE), (0.0, RED), (0.5, Color::WHITE)]);
        let positions: Vec<f32> = ramp.stops().iter().map(|s| s.position).collect();
        assert_eq!(positions, vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn a_repeated_position_keeps_the_last_stop_given() {
        let ramp = RampParam::linear([(0.5, RED), (0.5, BLUE)]);
        assert_eq!(ramp.len(), 1);
        assert_eq!(ramp.evaluate(0.5), BLUE);
    }

    /// A ramp cannot be empty: an input with nothing usable in it becomes the
    /// default rather than a type whose `evaluate` has no colour to answer.
    #[test]
    fn an_empty_input_falls_back_to_the_default_ramp() {
        let ramp = RampParam::new([], RampInterpolation::Smooth);
        assert_eq!(ramp.len(), 2);
        assert!(!ramp.is_empty());
        assert_eq!(ramp.evaluate(0.0), Color::BLACK);
        assert_eq!(ramp.evaluate(1.0), Color::WHITE);
        assert_eq!(
            ramp.interpolation(),
            RampInterpolation::Smooth,
            "the requested mode survives the fallback stops"
        );
    }

    /// A stand-in for whatever a damaged `.ravprj` might hold: the same wire
    /// shape as `RampParam`, but with no invariant on its stops.
    #[derive(serde::Serialize)]
    #[serde(rename = "RampParam")]
    struct StoredRamp {
        stops: Vec<RampStop>,
        interpolation: RampInterpolation,
    }

    fn read_back(stops: Vec<RampStop>) -> RampParam {
        let text = ron::to_string(&StoredRamp {
            stops,
            interpolation: RampInterpolation::Linear,
        })
        .expect("serialize");
        ron::from_str::<RampParam>(&text).expect("deserialize")
    }

    #[test]
    fn deserializing_sorts_unordered_stops() {
        let ramp = read_back(vec![
            RampStop::new(1.0, BLUE),
            RampStop::new(0.0, RED),
            RampStop::new(0.5, Color::WHITE),
        ]);
        let positions: Vec<f32> = ramp.stops().iter().map(|s| s.position).collect();
        assert_eq!(positions, vec![0.0, 0.5, 1.0]);
        assert_eq!(ramp.evaluate(0.5), Color::WHITE);
    }

    #[test]
    fn deserializing_collapses_repeated_positions_to_the_last() {
        let ramp = read_back(vec![
            RampStop::new(0.0, RED),
            RampStop::new(0.5, RED),
            RampStop::new(0.5, BLUE),
            RampStop::new(1.0, Color::WHITE),
        ]);
        assert_eq!(ramp.len(), 3);
        assert_eq!(ramp.evaluate(0.5), BLUE);
    }

    #[test]
    fn deserializing_drops_non_finite_stops() {
        let ramp = read_back(vec![
            RampStop::new(0.0, RED),
            RampStop::new(f32::NAN, BLUE),
            RampStop::new(0.5, Color::new(f32::INFINITY, 0.0, 0.0, 1.0)),
            RampStop::new(1.0, BLUE),
        ]);
        let positions: Vec<f32> = ramp.stops().iter().map(|s| s.position).collect();
        assert_eq!(positions, vec![0.0, 1.0]);
        assert!(ramp.stops().iter().all(RampStop::is_finite));
    }

    /// The worst case together: unsorted, repeated and non-finite in one
    /// file. It must open with a ramp that is merely defined — not panic, and
    /// not return garbage from a binary search over unordered stops.
    #[test]
    fn a_thoroughly_damaged_ramp_still_deserializes_to_a_valid_one() {
        let ramp = read_back(vec![
            RampStop::new(1.0, RED),
            RampStop::new(f32::NEG_INFINITY, Color::WHITE),
            RampStop::new(0.0, Color::BLACK),
            RampStop::new(1.0, BLUE),
            RampStop::new(f32::NAN, Color::new(f32::NAN, 0.0, 0.0, 1.0)),
        ]);
        let positions: Vec<f32> = ramp.stops().iter().map(|s| s.position).collect();
        assert_eq!(positions, vec![0.0, 1.0]);
        assert_eq!(ramp.evaluate(1.0), BLUE, "the last stop at 1.0 wins");
        for step in -10..=20 {
            let color = ramp.evaluate(step as f32 / 10.0);
            assert!(color.r.is_finite() && color.g.is_finite() && color.b.is_finite());
        }
    }

    /// Every stop being unusable leaves the default ramp rather than a ramp
    /// with nothing to sample.
    #[test]
    fn a_ramp_of_only_non_finite_stops_reads_as_the_default() {
        let ramp = read_back(vec![
            RampStop::new(f32::NAN, RED),
            RampStop::new(f32::INFINITY, BLUE),
        ]);
        assert_eq!(ramp, RampParam::black_to_white());
    }

    /// The normalization must not change the wire form: a ramp written by
    /// Ravel reads back identical, struct names and all (the `.ravprj`
    /// serializer sets `struct_names(true)`).
    #[test]
    fn a_well_formed_ramp_survives_the_named_ron_form() {
        let ramp = RampParam::linear([(0.0, RED), (0.5, Color::WHITE), (1.0, BLUE)])
            .with_interpolation(RampInterpolation::Smooth);
        let config = ron::ser::PrettyConfig::new().struct_names(true);
        let text = ron::ser::to_string_pretty(&ramp, config).expect("serialize");
        assert!(text.contains("RampParam("), "{text}");
        assert_eq!(ron::from_str::<RampParam>(&text).unwrap(), ramp);
    }

    #[test]
    fn round_trips_through_ron() {
        let ramp = RampParam::linear([(0.0, RED), (1.0, BLUE)])
            .with_interpolation(RampInterpolation::Constant);
        let text = ron::to_string(&ramp).unwrap();
        assert_eq!(ron::from_str::<RampParam>(&text).unwrap(), ramp);
    }
}
