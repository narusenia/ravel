// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scalar transfer curves stored as parameter values.
//!
//! A [`CurveParam`] maps one scalar input to one scalar output through an
//! ordered set of control points. It is the structural parameter behind
//! [`ParameterValue::Curve`](crate::graph::ParameterValue::Curve) and is
//! shared by every curve consumer — `field.curve_remap` today, the value- and
//! raster-domain curves later — so the same control points always produce the
//! same output.
//!
//! # Relationship to [`KeyframeCurve`](crate::animation::curve::KeyframeCurve)
//!
//! The two types share their interpolation modes ([`Interpolation`]), their
//! tangent convention (offsets in input/output space from the anchor), and
//! their sampling rules: the **left** control point's mode governs a segment,
//! and both ends hold. They differ only in the domain of the input axis —
//! keyframes are indexed by integer frames, control points by an arbitrary
//! scalar — so the interpolation itself runs through the shared continuous
//! forms [`linear_at`](crate::animation::interpolation::linear_at) and
//! [`bezier_at`](crate::animation::interpolation::bezier_at).
//!
//! # Domain
//!
//! Outside `[first.x, last.x]` the curve **clamps** to the nearest end point's
//! output. That is the convention `field.curve_remap` has always had, and it
//! keeps a curve authored over `0..=1` usable with inputs that stray. Modes
//! that repeat or extrapolate instead belong to the *node* that reads the
//! curve, not to the curve itself.
//!
//! An empty curve is the identity (`evaluate(x) == x`) rather than a constant:
//! a remap with no control points must not erase the value it is remapping.

use crate::animation::interpolation::{self, Interpolation};
use crate::types::Vec2;

/// One control point of a [`CurveParam`]: an input/output pair, the
/// interpolation mode governing the segment that *leaves* it, and bezier
/// tangent handles.
///
/// Tangent handles are offsets in (input, output) space relative to the
/// point's anchor, exactly as [`Keyframe`](crate::animation::curve::Keyframe)
/// defines them: `tangent_out` shapes the curve leaving this point,
/// `tangent_in` shapes the curve arriving at it, and a zero tangent makes the
/// adjacent segment a straight line.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CurvePoint {
    /// Input value this point is anchored at.
    pub x: f32,
    /// Output value produced at `x`.
    pub y: f32,
    pub interpolation: Interpolation,
    pub tangent_in: Vec2,
    pub tangent_out: Vec2,
}

impl CurvePoint {
    /// A control point with zero tangent handles.
    pub fn new(x: f32, y: f32, interpolation: Interpolation) -> Self {
        Self {
            x,
            y,
            interpolation,
            tangent_in: Vec2(0.0, 0.0),
            tangent_out: Vec2(0.0, 0.0),
        }
    }

    /// Builder: set both tangent handles.
    pub fn with_tangents(mut self, tangent_in: Vec2, tangent_out: Vec2) -> Self {
        self.tangent_in = tangent_in;
        self.tangent_out = tangent_out;
        self
    }
}

/// An ordered scalar transfer curve: input value → output value.
///
/// Control points are kept sorted ascending by `x` with unique `x`, so
/// evaluation is a binary search plus one segment interpolation. Every
/// constructor and mutator preserves that invariant.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CurveParam {
    /// Control points, always sorted ascending by `x` with unique `x`.
    points: Vec<CurvePoint>,
}

impl Default for CurveParam {
    fn default() -> Self {
        Self::identity()
    }
}

impl CurveParam {
    /// The identity curve: `0 → 0`, `1 → 1`, linearly interpolated.
    ///
    /// This is the fallback a load-time migration uses when a stored curve
    /// cannot be read, and the default a template declares.
    pub fn identity() -> Self {
        Self::linear([(0.0, 0.0), (1.0, 1.0)])
    }

    /// A curve through `points` with linear interpolation and no tangents.
    ///
    /// Points may be supplied in any order; duplicates of one `x` collapse to
    /// the last one given.
    pub fn linear(points: impl IntoIterator<Item = (f32, f32)>) -> Self {
        Self::from_points(
            points
                .into_iter()
                .map(|(x, y)| CurvePoint::new(x, y, Interpolation::Linear)),
        )
    }

    /// A curve from fully specified control points, sorted on the way in.
    ///
    /// Points may be supplied in any order; duplicates of one `x` collapse to
    /// the last one given.
    pub fn from_points(points: impl IntoIterator<Item = CurvePoint>) -> Self {
        let mut curve = Self { points: Vec::new() };
        for point in points {
            curve.insert_point(point);
        }
        curve
    }

    /// Number of control points.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Whether the curve has no control points (the identity mapping).
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Read-only view of the control points (sorted by input value).
    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }

    /// Storage index of the control point exactly at `x`, if any.
    fn index_of(&self, x: f32) -> Option<usize> {
        self.points
            .binary_search_by(|point| point.x.total_cmp(&x))
            .ok()
    }

    // ----- CRUD ------------------------------------------------------------

    /// Insert (or overwrite) a control point, keeping the curve sorted.
    ///
    /// A point at an `x` the curve already carries replaces that point.
    pub fn insert_point(&mut self, point: CurvePoint) {
        match self
            .points
            .binary_search_by(|existing| existing.x.total_cmp(&point.x))
        {
            Ok(i) => self.points[i] = point,
            Err(i) => self.points.insert(i, point),
        }
    }

    /// Remove the control point exactly at `x`, returning it if it existed.
    pub fn remove_point(&mut self, x: f32) -> Option<CurvePoint> {
        self.index_of(x).map(|i| self.points.remove(i))
    }

    /// Move the control point at `from_x` to `(to_x, y)`, preserving its
    /// interpolation mode and tangents. Returns `true` on success.
    ///
    /// A point already sitting at `to_x` is overwritten; moving a point onto
    /// its own input value only changes its output.
    pub fn move_point(&mut self, from_x: f32, to_x: f32, y: f32) -> bool {
        let Some(i) = self.index_of(from_x) else {
            return false;
        };
        if from_x.total_cmp(&to_x).is_eq() {
            self.points[i].y = y;
            return true;
        }
        let mut point = self.points.remove(i);
        point.x = to_x;
        point.y = y;
        self.insert_point(point);
        true
    }

    // ----- evaluation ------------------------------------------------------

    /// Map `x` through the curve.
    ///
    /// * Empty curve → `x` unchanged (the identity mapping).
    /// * At or before the first point → the first output (clamp).
    /// * At or after the last point → the last output (clamp).
    /// * Exact control-point hit → that point's output.
    /// * Otherwise → interpolation governed by the left point's mode.
    ///
    /// [`Interpolation::Step`] segments are half-open: the left output is held
    /// over `[left.x, right.x)` and the right point's own output takes over
    /// exactly at `right.x`.
    pub fn evaluate(&self, x: f32) -> f32 {
        let (Some(first), Some(last)) = (self.points.first(), self.points.last()) else {
            return x;
        };
        if x <= first.x {
            return first.y;
        }
        if x >= last.x {
            return last.y;
        }

        // First control point at or after `x`. Both bounds were handled above,
        // so this lands in `1..len` and the segment `[idx - 1, idx]` exists.
        let idx = self.points.partition_point(|point| point.x < x);
        let right = &self.points[idx];
        if right.x == x {
            return right.y;
        }
        let left = &self.points[idx - 1];

        match left.interpolation {
            Interpolation::Step => left.y,
            Interpolation::Linear => interpolation::linear_at(left.x, left.y, right.x, right.y, x),
            Interpolation::Bezier => interpolation::bezier_at(
                left.x,
                left.y,
                left.tangent_out,
                right.x,
                right.y,
                right.tangent_in,
                x,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_maps_its_domain_onto_itself() {
        let curve = CurveParam::identity();
        assert_eq!(curve.evaluate(0.0), 0.0);
        assert!((curve.evaluate(0.25) - 0.25).abs() < 1e-6);
        assert_eq!(curve.evaluate(1.0), 1.0);
    }

    #[test]
    fn default_is_the_identity_curve() {
        assert_eq!(CurveParam::default(), CurveParam::identity());
    }

    #[test]
    fn an_empty_curve_passes_its_input_through() {
        let curve = CurveParam::from_points([]);
        assert!(curve.is_empty());
        assert_eq!(curve.evaluate(-3.0), -3.0);
        assert_eq!(curve.evaluate(7.5), 7.5);
    }

    #[test]
    fn points_are_sorted_on_construction() {
        let curve = CurveParam::linear([(1.0, 10.0), (0.0, 0.0), (0.5, 2.0)]);
        let xs: Vec<f32> = curve.points().iter().map(|p| p.x).collect();
        assert_eq!(xs, vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn a_repeated_input_keeps_the_last_point_given() {
        let curve = CurveParam::linear([(0.5, 1.0), (0.5, 9.0)]);
        assert_eq!(curve.len(), 1);
        assert_eq!(curve.evaluate(0.5), 9.0);
    }

    /// Out of domain the curve clamps to the end outputs rather than
    /// extrapolating: this is what `field.curve_remap` has always done.
    #[test]
    fn out_of_domain_inputs_clamp_to_the_end_outputs() {
        let curve = CurveParam::linear([(0.0, 0.0), (0.5, 2.0), (1.0, 10.0)]);
        assert_eq!(curve.evaluate(-1.0), 0.0);
        assert_eq!(curve.evaluate(2.0), 10.0);
        assert_eq!(curve.evaluate(0.25), 1.0);
    }

    #[test]
    fn a_single_point_is_constant() {
        let curve = CurveParam::linear([(0.5, 3.0)]);
        assert_eq!(curve.evaluate(-1.0), 3.0);
        assert_eq!(curve.evaluate(0.5), 3.0);
        assert_eq!(curve.evaluate(9.0), 3.0);
    }

    #[test]
    fn step_segments_are_half_open() {
        let curve = CurveParam::from_points([
            CurvePoint::new(0.0, 0.0, Interpolation::Step),
            CurvePoint::new(1.0, 5.0, Interpolation::Step),
        ]);
        assert_eq!(curve.evaluate(0.0), 0.0);
        assert_eq!(curve.evaluate(0.999), 0.0);
        assert_eq!(curve.evaluate(1.0), 5.0);
    }

    /// The bezier form is the same one keyframes use, so a symmetric ease
    /// passes through the midpoint and stays monotonic around it.
    #[test]
    fn bezier_segments_ease_through_their_midpoint() {
        let curve = CurveParam::from_points([
            CurvePoint::new(0.0, 0.0, Interpolation::Bezier)
                .with_tangents(Vec2(0.0, 0.0), Vec2(0.3, 0.0)),
            CurvePoint::new(1.0, 1.0, Interpolation::Bezier)
                .with_tangents(Vec2(-0.3, 0.0), Vec2(0.0, 0.0)),
        ]);
        assert!((curve.evaluate(0.5) - 0.5).abs() < 1e-4);
        assert!(curve.evaluate(0.4) < curve.evaluate(0.5));
        assert!(curve.evaluate(0.5) < curve.evaluate(0.6));
    }

    /// A zero-width segment cannot happen through the public API (inputs are
    /// unique), but a hand-edited `.ravprj` could store one; it must not
    /// divide by zero.
    #[test]
    fn a_degenerate_segment_returns_the_right_output() {
        let curve = CurveParam {
            points: vec![
                CurvePoint::new(0.0, 0.0, Interpolation::Linear),
                CurvePoint::new(0.5, 1.0, Interpolation::Linear),
                CurvePoint::new(0.5, 4.0, Interpolation::Linear),
                CurvePoint::new(1.0, 8.0, Interpolation::Linear),
            ],
        };
        assert!(curve.evaluate(0.5).is_finite());
    }

    #[test]
    fn insert_overwrites_a_point_at_the_same_input() {
        let mut curve = CurveParam::linear([(0.0, 0.0), (1.0, 1.0)]);
        curve.insert_point(CurvePoint::new(1.0, 4.0, Interpolation::Step));
        assert_eq!(curve.len(), 2);
        assert_eq!(curve.evaluate(1.0), 4.0);
    }

    #[test]
    fn remove_drops_the_point_and_reshapes_the_curve() {
        let mut curve = CurveParam::linear([(0.0, 0.0), (0.5, 10.0), (1.0, 0.0)]);
        assert!((curve.evaluate(0.5) - 10.0).abs() < 1e-6);
        assert!(curve.remove_point(0.5).is_some());
        assert!(curve.evaluate(0.5).abs() < 1e-6);
        assert!(curve.remove_point(0.5).is_none());
    }

    #[test]
    fn move_point_reorders_and_keeps_tangents() {
        let mut curve = CurveParam::from_points([
            CurvePoint::new(0.0, 0.0, Interpolation::Linear)
                .with_tangents(Vec2(-0.1, 0.0), Vec2(0.1, 0.0)),
            CurvePoint::new(0.5, 1.0, Interpolation::Linear),
        ]);
        assert!(curve.move_point(0.0, 1.0, 2.0));
        let xs: Vec<f32> = curve.points().iter().map(|p| p.x).collect();
        assert_eq!(xs, vec![0.5, 1.0]);
        assert_eq!(curve.points()[1].tangent_out, Vec2(0.1, 0.0));
        assert_eq!(curve.evaluate(1.0), 2.0);
        assert!(!curve.move_point(9.0, 0.0, 0.0));
    }

    #[test]
    fn move_point_onto_itself_only_changes_the_output() {
        let mut curve = CurveParam::linear([(0.0, 0.0), (1.0, 1.0)]);
        assert!(curve.move_point(1.0, 1.0, 3.0));
        assert_eq!(curve.len(), 2);
        assert_eq!(curve.evaluate(1.0), 3.0);
    }

    #[test]
    fn round_trips_through_ron() {
        let curve = CurveParam::from_points([
            CurvePoint::new(0.0, 0.25, Interpolation::Bezier)
                .with_tangents(Vec2(0.0, 0.0), Vec2(0.2, 0.1)),
            CurvePoint::new(1.0, 0.75, Interpolation::Step),
        ]);
        let text = ron::to_string(&curve).unwrap();
        assert_eq!(ron::from_str::<CurveParam>(&text).unwrap(), curve);
    }
}
