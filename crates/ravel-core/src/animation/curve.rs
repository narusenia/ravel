// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Keyframe curves: an ordered set of keyframes sampled at arbitrary frames.
//!
//! A [`KeyframeCurve`] keeps its keyframes sorted by frame at all times so that
//! sampling is a binary search plus a single segment interpolation. CRUD
//! operations ([`insert`](KeyframeCurve::insert), [`remove`](KeyframeCurve::remove),
//! [`modify`](KeyframeCurve::modify), [`move_keyframe`](KeyframeCurve::move_keyframe))
//! preserve that invariant.

use crate::animation::interpolation::{self, Interpolation};
use crate::types::Vec2;

/// A single keyframe: a value anchored at a frame with tangent handles.
///
/// Tangent handles are offsets in (frame, value) space relative to the
/// keyframe's anchor point. `tangent_out` shapes the curve leaving this
/// keyframe; `tangent_in` shapes the curve arriving at it.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Keyframe {
    pub frame: u64,
    pub value: f32,
    pub interpolation: Interpolation,
    pub tangent_in: Vec2,
    pub tangent_out: Vec2,
}

impl Keyframe {
    /// Create a keyframe with zero tangent handles.
    pub fn new(frame: u64, value: f32, interpolation: Interpolation) -> Self {
        Self {
            frame,
            value,
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

/// An ordered keyframe curve sampled at arbitrary frames.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct KeyframeCurve {
    /// Keyframes, always sorted ascending by `frame` with unique frames.
    keyframes: Vec<Keyframe>,
    /// Value returned when the curve has no keyframes.
    default_value: f32,
}

impl Default for KeyframeCurve {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyframeCurve {
    /// Create an empty curve whose default value is `0.0`.
    pub fn new() -> Self {
        Self {
            keyframes: Vec::new(),
            default_value: 0.0,
        }
    }

    /// Create an empty curve with a custom default value (used when empty).
    pub fn with_default(default_value: f32) -> Self {
        Self {
            keyframes: Vec::new(),
            default_value,
        }
    }

    /// Number of keyframes.
    pub fn len(&self) -> usize {
        self.keyframes.len()
    }

    /// Whether the curve has no keyframes.
    pub fn is_empty(&self) -> bool {
        self.keyframes.is_empty()
    }

    /// Read-only view of the keyframes (sorted by frame).
    pub fn keyframes(&self) -> &[Keyframe] {
        &self.keyframes
    }

    /// The value the curve returns while it has no keyframes.
    pub fn default_value(&self) -> f32 {
        self.default_value
    }

    /// Find the storage index of the keyframe exactly at `frame`, if any.
    fn index_of(&self, frame: u64) -> Option<usize> {
        self.keyframes
            .binary_search_by_key(&frame, |k| k.frame)
            .ok()
    }

    // ----- CRUD ------------------------------------------------------------

    /// Insert (or overwrite) a keyframe at `frame` with zero tangents.
    ///
    /// If a keyframe already exists at `frame`, its value and interpolation are
    /// replaced. Keyframe ordering is preserved.
    pub fn insert(&mut self, frame: u64, value: f32, interpolation: Interpolation) {
        self.insert_keyframe(Keyframe::new(frame, value, interpolation));
    }

    /// Insert (or overwrite) a fully-specified keyframe, keeping the curve
    /// sorted by frame.
    pub fn insert_keyframe(&mut self, kf: Keyframe) {
        match self.keyframes.binary_search_by_key(&kf.frame, |k| k.frame) {
            Ok(i) => self.keyframes[i] = kf,
            Err(i) => self.keyframes.insert(i, kf),
        }
    }

    /// Remove the keyframe at `frame`, returning it if it existed.
    pub fn remove(&mut self, frame: u64) -> Option<Keyframe> {
        self.index_of(frame).map(|i| self.keyframes.remove(i))
    }

    /// Modify the value and (optionally) the tangents of the keyframe at
    /// `frame`. Returns `true` if a keyframe was found and updated.
    pub fn modify(
        &mut self,
        frame: u64,
        new_value: f32,
        new_tangents: Option<(Vec2, Vec2)>,
    ) -> bool {
        match self.index_of(frame) {
            Some(i) => {
                let kf = &mut self.keyframes[i];
                kf.value = new_value;
                if let Some((tin, tout)) = new_tangents {
                    kf.tangent_in = tin;
                    kf.tangent_out = tout;
                }
                true
            }
            None => false,
        }
    }

    /// Move the keyframe at `old_frame` to `new_frame`, preserving its value
    /// and tangents. Returns `true` on success.
    ///
    /// If a keyframe already exists at `new_frame` it is overwritten. Moving a
    /// keyframe onto its own current frame is a no-op that returns `true`.
    pub fn move_keyframe(&mut self, old_frame: u64, new_frame: u64) -> bool {
        if old_frame == new_frame {
            return self.index_of(old_frame).is_some();
        }
        let Some(i) = self.index_of(old_frame) else {
            return false;
        };
        let mut kf = self.keyframes.remove(i);
        kf.frame = new_frame;
        self.insert_keyframe(kf);
        true
    }

    // ----- sampling --------------------------------------------------------

    /// Sample the curve at `frame`, which may sit between integer frames.
    ///
    /// Keyframes are anchored to integer frames but sampling is continuous, so
    /// sub-frame contexts (motion blur, time remapping) read distinct values
    /// within one frame instead of the same value repeated.
    ///
    /// * Empty curve → the default value.
    /// * Before the first keyframe → the first value (extrapolation = hold).
    /// * After the last keyframe → the last value (extrapolation = hold).
    /// * Exact keyframe hit → that keyframe's value.
    /// * Otherwise → interpolation governed by the left keyframe's mode.
    ///
    /// [`Interpolation::Step`] segments are half-open: the left value is held
    /// over `[left.frame, right.frame)` and the right keyframe's own value
    /// takes over exactly at `right.frame`.
    pub fn sample(&self, frame: f64) -> f32 {
        if self.keyframes.is_empty() {
            return self.default_value;
        }
        let first = &self.keyframes[0];
        let last = self.keyframes.last().unwrap();
        if frame <= first.frame as f64 {
            return first.value;
        }
        if frame >= last.frame as f64 {
            return last.value;
        }

        // First keyframe at or after `frame`. Both bounds were handled above,
        // so this lands in `1..len` and the segment `[idx - 1, idx]` exists.
        let idx = self.keyframes.partition_point(|k| (k.frame as f64) < frame);
        if self.keyframes[idx].frame as f64 == frame {
            // Exact hit.
            return self.keyframes[idx].value;
        }
        let left = &self.keyframes[idx - 1];
        let right = &self.keyframes[idx];

        match left.interpolation {
            Interpolation::Step => left.value,
            Interpolation::Linear => {
                interpolation::linear(left.frame, left.value, right.frame, right.value, frame)
            }
            Interpolation::Bezier => interpolation::bezier(
                left.frame,
                left.value,
                left.tangent_out,
                right.frame,
                right.value,
                right.tangent_in,
                frame,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear_curve() -> KeyframeCurve {
        let mut c = KeyframeCurve::new();
        c.insert(0, 0.0, Interpolation::Linear);
        c.insert(10, 1.0, Interpolation::Linear);
        c
    }

    // ---- empty / default --------------------------------------------------

    #[test]
    fn empty_curve_returns_default() {
        let c = KeyframeCurve::new();
        assert_eq!(c.sample(0.0), 0.0);
        assert_eq!(c.sample(100.0), 0.0);
    }

    #[test]
    fn empty_curve_custom_default() {
        let c = KeyframeCurve::with_default(3.5);
        assert!((c.sample(42.0) - 3.5).abs() < f32::EPSILON);
    }

    // ---- linear -----------------------------------------------------------

    #[test]
    fn linear_interpolation_midpoint() {
        let c = linear_curve();
        assert!((c.sample(5.0) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn linear_boundary_values() {
        let c = linear_curve();
        assert!((c.sample(0.0) - 0.0).abs() < 1e-6);
        assert!((c.sample(10.0) - 1.0).abs() < 1e-6);
    }

    // ---- step -------------------------------------------------------------

    #[test]
    fn step_holds_left_value() {
        let mut c = KeyframeCurve::new();
        c.insert(0, 0.0, Interpolation::Step);
        c.insert(10, 1.0, Interpolation::Step);
        assert!((c.sample(5.0) - 0.0).abs() < f32::EPSILON); // holds left
        assert!((c.sample(9.0) - 0.0).abs() < f32::EPSILON);
        assert!((c.sample(10.0) - 1.0).abs() < f32::EPSILON); // exact hit
    }

    #[test]
    fn step_segment_is_half_open() {
        let mut c = KeyframeCurve::new();
        c.insert(0, 0.0, Interpolation::Step);
        c.insert(10, 1.0, Interpolation::Step);
        // `[left.frame, right.frame)` holds the left value, including the very
        // last sub-frame position before the right keyframe.
        assert!((c.sample(0.0) - 0.0).abs() < f32::EPSILON);
        assert!((c.sample(0.5) - 0.0).abs() < f32::EPSILON);
        assert!((c.sample(9.999) - 0.0).abs() < f32::EPSILON);
        // The right keyframe takes over exactly at its own frame.
        assert!((c.sample(10.0) - 1.0).abs() < f32::EPSILON);
    }

    // ---- sub-frame sampling ------------------------------------------------

    #[test]
    fn linear_interpolates_between_integer_frames() {
        let c = linear_curve(); // 0→1 over ten frames
        assert!((c.sample(2.5) - 0.25).abs() < 1e-4);
        assert!((c.sample(7.5) - 0.75).abs() < 1e-4);
        // Consecutive sub-frame positions must differ; a quantising
        // implementation would return the same value across a whole frame.
        assert!(c.sample(2.25) < c.sample(2.75));
    }

    #[test]
    fn bezier_interpolates_between_integer_frames() {
        let mut c = KeyframeCurve::new();
        c.insert_keyframe(
            Keyframe::new(0, 0.0, Interpolation::Bezier)
                .with_tangents(Vec2(0.0, 0.0), Vec2(3.0, 0.0)),
        );
        c.insert_keyframe(
            Keyframe::new(10, 1.0, Interpolation::Bezier)
                .with_tangents(Vec2(-3.0, 0.0), Vec2(0.0, 0.0)),
        );
        assert!((c.sample(5.0) - 0.5).abs() < 1e-4);
        assert!(c.sample(4.5) < c.sample(5.0));
        assert!(c.sample(5.0) < c.sample(5.5));
    }

    #[test]
    fn integer_frames_are_unaffected_by_continuous_sampling() {
        // Sampling exactly on the frame grid must stay bit-identical to the
        // keyframe values and to the integer-only interpolation it replaced.
        let mut c = KeyframeCurve::new();
        c.insert(0, 0.0, Interpolation::Linear);
        c.insert(4, 2.0, Interpolation::Linear);
        c.insert(8, -1.0, Interpolation::Linear);
        assert_eq!(c.sample(0.0), 0.0);
        assert_eq!(c.sample(4.0), 2.0);
        assert_eq!(c.sample(8.0), -1.0);
        // Midpoints of each segment land on exact halves.
        assert_eq!(c.sample(2.0), 1.0);
        assert_eq!(c.sample(6.0), 0.5);
    }

    #[test]
    fn sub_frame_positions_outside_the_range_still_hold() {
        let mut c = KeyframeCurve::new();
        c.insert(5, 2.0, Interpolation::Linear);
        c.insert(10, 4.0, Interpolation::Linear);
        assert!((c.sample(4.999) - 2.0).abs() < f32::EPSILON);
        assert!((c.sample(10.001) - 4.0).abs() < f32::EPSILON);
    }

    // ---- out-of-range extrapolation ---------------------------------------

    #[test]
    fn before_first_holds_first_value() {
        let mut c = KeyframeCurve::new();
        c.insert(5, 2.0, Interpolation::Linear);
        c.insert(10, 4.0, Interpolation::Linear);
        assert!((c.sample(0.0) - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn after_last_holds_last_value() {
        let mut c = KeyframeCurve::new();
        c.insert(5, 2.0, Interpolation::Linear);
        c.insert(10, 4.0, Interpolation::Linear);
        assert!((c.sample(100.0) - 4.0).abs() < f32::EPSILON);
    }

    #[test]
    fn single_keyframe_is_constant() {
        let mut c = KeyframeCurve::new();
        c.insert(5, 7.0, Interpolation::Linear);
        assert!((c.sample(0.0) - 7.0).abs() < f32::EPSILON);
        assert!((c.sample(5.0) - 7.0).abs() < f32::EPSILON);
        assert!((c.sample(50.0) - 7.0).abs() < f32::EPSILON);
    }

    // ---- CRUD -------------------------------------------------------------

    #[test]
    fn insert_keeps_sorted_and_overwrites() {
        let mut c = KeyframeCurve::new();
        c.insert(10, 1.0, Interpolation::Linear);
        c.insert(0, 0.0, Interpolation::Linear);
        c.insert(5, 0.25, Interpolation::Linear);
        let frames: Vec<u64> = c.keyframes().iter().map(|k| k.frame).collect();
        assert_eq!(frames, vec![0, 5, 10]);

        // Overwrite frame 5.
        c.insert(5, 0.9, Interpolation::Step);
        assert_eq!(c.len(), 3);
        assert!((c.keyframes()[1].value - 0.9).abs() < f32::EPSILON);
        assert_eq!(c.keyframes()[1].interpolation, Interpolation::Step);
    }

    #[test]
    fn remove_keyframe() {
        let mut c = linear_curve();
        let removed = c.remove(10);
        assert!(removed.is_some());
        assert_eq!(c.len(), 1);
        // With only frame 0 left, the curve is constant 0.0.
        assert!((c.sample(5.0) - 0.0).abs() < f32::EPSILON);
        assert!(c.remove(999).is_none());
    }

    #[test]
    fn remove_recomputes_curve() {
        let mut c = KeyframeCurve::new();
        c.insert(0, 0.0, Interpolation::Linear);
        c.insert(5, 10.0, Interpolation::Linear);
        c.insert(10, 0.0, Interpolation::Linear);
        // Before removal, midpoint pulled toward the spike at frame 5.
        assert!((c.sample(5.0) - 10.0).abs() < 1e-4);
        c.remove(5);
        // After removal the curve is a flat 0→0 line.
        assert!((c.sample(5.0) - 0.0).abs() < 1e-4);
    }

    #[test]
    fn modify_value_and_tangents() {
        let mut c = linear_curve();
        assert!(c.modify(10, 2.0, None));
        assert!((c.sample(10.0) - 2.0).abs() < f32::EPSILON);
        assert!((c.sample(5.0) - 1.0).abs() < 1e-4); // linear 0→2 at midpoint

        let tin = Vec2(-1.0, 0.5);
        let tout = Vec2(1.0, -0.5);
        assert!(c.modify(0, 0.0, Some((tin, tout))));
        assert_eq!(c.keyframes()[0].tangent_in, tin);
        assert_eq!(c.keyframes()[0].tangent_out, tout);

        assert!(!c.modify(999, 0.0, None)); // missing keyframe
    }

    #[test]
    fn move_keyframe_reorders() {
        let mut c = linear_curve();
        // Move frame 0 → frame 20 (now past the frame-10 keyframe).
        assert!(c.move_keyframe(0, 20));
        let frames: Vec<u64> = c.keyframes().iter().map(|k| k.frame).collect();
        assert_eq!(frames, vec![10, 20]);
        // Value preserved: old frame-0 keyframe (value 0.0) now at frame 20.
        assert!((c.sample(20.0) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn move_keyframe_same_frame_is_noop() {
        let mut c = linear_curve();
        assert!(c.move_keyframe(10, 10));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn move_missing_keyframe_fails() {
        let mut c = linear_curve();
        assert!(!c.move_keyframe(999, 1));
    }
}
