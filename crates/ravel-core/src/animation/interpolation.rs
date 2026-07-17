// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Keyframe interpolation algorithms: Bézier, linear, and step (hold).
//!
//! The interpolation between two adjacent keyframes is driven by the **left**
//! keyframe's [`Interpolation`] mode:
//!
//! * [`Interpolation::Step`] holds the left value until the next keyframe.
//! * [`Interpolation::Linear`] linearly interpolates between the two values.
//! * [`Interpolation::Bezier`] follows a cubic Bézier defined by the left
//!   keyframe's out tangent and the right keyframe's in tangent — reproducing
//!   After-Effects-style easing curves.

use crate::types::Vec2;

/// Interpolation mode used between a keyframe and its successor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Interpolation {
    /// Cubic Bézier easing (uses the keyframes' tangent handles).
    #[default]
    Bezier,
    /// Straight linear ramp between the two values.
    Linear,
    /// Hold the left value until the next keyframe (a.k.a. "hold"/"constant").
    Step,
}

/// Linearly interpolate `value` for `frame` between `(f0, v0)` and `(f1, v1)`.
///
/// `frame` is expected to lie within `[f0, f1]`; callers guarantee this.
pub fn linear(f0: u64, v0: f32, f1: u64, v1: f32, frame: u64) -> f32 {
    debug_assert!(f1 >= f0, "keyframes must be ordered by frame");
    if f1 == f0 {
        return v1;
    }
    let t = (frame - f0) as f32 / (f1 - f0) as f32;
    v0 + (v1 - v0) * t
}

/// Evaluate a cubic Bézier on a single axis at parameter `t ∈ [0, 1]`.
fn cubic(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let u = 1.0 - t;
    u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
}

/// Solve for the Bézier parameter `t` whose x-coordinate equals `target_x`.
///
/// The control x-coordinates are clamped into `[x0, x3]` by the caller so the
/// x(t) curve is monotonically non-decreasing; bisection therefore converges to
/// a unique solution. 60 iterations yield far better than the 1e-4 precision the
/// specification requires.
fn solve_t_for_x(x0: f32, x1: f32, x2: f32, x3: f32, target_x: f32) -> f32 {
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    let mut mid = 0.5f32;
    for _ in 0..60 {
        mid = 0.5 * (lo + hi);
        let x = cubic(x0, x1, x2, x3, mid);
        if x < target_x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    mid
}

/// Cubic Bézier interpolation between two keyframes.
///
/// Control points are formed in (frame, value) space from the left keyframe's
/// out tangent and the right keyframe's in tangent, both expressed as offsets:
///
/// ```text
/// c0 = (f0, v0)
/// c1 = (f0, v0) + tangent_out      (left out handle)
/// c2 = (f1, v1) + tangent_in       (right in handle)
/// c3 = (f1, v1)
/// ```
///
/// The control x-coordinates are clamped to `[f0, f1]` so the curve remains a
/// proper function of the frame axis (one value per frame).
#[allow(clippy::too_many_arguments)]
pub fn bezier(
    f0: u64,
    v0: f32,
    tangent_out: Vec2,
    f1: u64,
    v1: f32,
    tangent_in: Vec2,
    frame: u64,
) -> f32 {
    debug_assert!(f1 >= f0, "keyframes must be ordered by frame");
    if f1 == f0 {
        return v1;
    }
    let x0 = f0 as f32;
    let x3 = f1 as f32;
    let x1 = (x0 + tangent_out.0).clamp(x0, x3);
    let x2 = (x3 + tangent_in.0).clamp(x0, x3);
    let y0 = v0;
    let y1 = v0 + tangent_out.1;
    let y2 = v1 + tangent_in.1;
    let y3 = v1;

    let t = solve_t_for_x(x0, x1, x2, x3, frame as f32);
    cubic(y0, y1, y2, y3, t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_midpoint() {
        assert!((linear(0, 0.0, 10, 1.0, 5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn linear_endpoints() {
        assert!((linear(0, 0.0, 10, 1.0, 0) - 0.0).abs() < 1e-6);
        assert!((linear(0, 0.0, 10, 1.0, 10) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn linear_zero_width_segment() {
        // Degenerate segment returns the right value.
        assert!((linear(5, 0.0, 5, 2.0, 5) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn bezier_with_linear_tangents_matches_linear() {
        // Tangents placed at 1/3 along the connecting line reduce the cubic
        // Bézier to a straight line.
        let tan_out = Vec2(10.0 / 3.0, 1.0 / 3.0);
        let tan_in = Vec2(-10.0 / 3.0, -1.0 / 3.0);
        for frame in 0..=10u64 {
            let b = bezier(0, 0.0, tan_out, 10, 1.0, tan_in, frame);
            let l = linear(0, 0.0, 10, 1.0, frame);
            assert!(
                (b - l).abs() < 1e-4,
                "frame {frame}: bezier {b} vs linear {l}"
            );
        }
    }

    #[test]
    fn bezier_symmetric_ease_midpoint_is_half() {
        // A symmetric ease-in/ease-out curve passes through 0.5 at the temporal
        // midpoint by symmetry.
        let tan_out = Vec2(3.0, 0.0);
        let tan_in = Vec2(-3.0, 0.0);
        let mid = bezier(0, 0.0, tan_out, 10, 1.0, tan_in, 5);
        assert!((mid - 0.5).abs() < 1e-4, "midpoint was {mid}");
    }

    #[test]
    fn bezier_hits_endpoints_exactly() {
        let tan_out = Vec2(3.0, 0.0);
        let tan_in = Vec2(-3.0, 0.0);
        let start = bezier(0, 0.0, tan_out, 10, 1.0, tan_in, 0);
        let end = bezier(0, 0.0, tan_out, 10, 1.0, tan_in, 10);
        assert!((start - 0.0).abs() < 1e-4);
        assert!((end - 1.0).abs() < 1e-4);
    }
}
