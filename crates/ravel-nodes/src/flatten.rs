// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared bezier flattening for curved paths (REQ-UI-011 unit 6).
//!
//! Geometry carries curve data as per-point `in_tan` / `out_tan` attributes
//! (segment control points at `P + tangent`; a zero tangent means a straight
//! segment). Both rasterize paths — the CPU zeno pass and the analytic GPU
//! shader — consume the same flattened polyline produced here, so curved
//! rendering stays structurally GPU/CPU-equal instead of relying on two
//! independent curve evaluators.

use ravel_core::types::Vec2;

/// Comp-space flatness tolerance in pixels: a curved segment is subdivided
/// until both control points sit within this distance of the chord.
pub const FLATTEN_TOLERANCE: f32 = 0.25;

/// Subdivision depth cap (2^10 output segments per curve, worst case),
/// guarding against degenerate control polygons (NaN, self-intersection).
const MAX_DEPTH: u32 = 10;

fn is_zero(v: Vec2) -> bool {
    v.0 == 0.0 && v.1 == 0.0
}

/// Flatten one path's control polygon into a polyline. `in_tans` /
/// `out_tans` are per-point tangent offsets; missing slices or missing tail
/// entries count as zero tangents (straight segments). The closing segment
/// of a closed path is flattened too. The returned polyline starts at
/// `points[0]` and never repeats it at the end — closing stays expressed by
/// the primitive's `closed` flag.
pub fn flatten_path(
    points: &[Vec2],
    in_tans: Option<&[Vec2]>,
    out_tans: Option<&[Vec2]>,
    closed: bool,
) -> Vec<Vec2> {
    if points.is_empty() {
        return Vec::new();
    }
    let tangent = |tangents: Option<&[Vec2]>, i: usize| {
        tangents
            .and_then(|t| t.get(i))
            .copied()
            .unwrap_or(Vec2(0.0, 0.0))
    };
    let segment_count = if closed {
        points.len()
    } else {
        points.len() - 1
    };
    let mut out = Vec::with_capacity(points.len());
    out.push(points[0]);
    for i in 0..segment_count {
        let j = (i + 1) % points.len();
        let (p0, p1) = (points[i], points[j]);
        let (out_tan, in_tan) = (tangent(out_tans, i), tangent(in_tans, j));
        if is_zero(out_tan) && is_zero(in_tan) {
            out.push(p1);
            continue;
        }
        let c1 = Vec2(p0.0 + out_tan.0, p0.1 + out_tan.1);
        let c2 = Vec2(p1.0 + in_tan.0, p1.1 + in_tan.1);
        flatten_segment(p0, c1, c2, p1, 0, &mut out);
    }
    // The closing segment of a closed path ends at points[0]; drop that
    // duplicate — closing stays expressed by the primitive's `closed` flag.
    if closed && out.len() > 1 && out.last() == Some(&points[0]) {
        out.pop();
    }
    out
}

fn flatten_segment(p0: Vec2, c1: Vec2, c2: Vec2, p1: Vec2, depth: u32, out: &mut Vec<Vec2>) {
    if depth >= MAX_DEPTH || flat_enough(p0, c1, c2, p1) {
        out.push(p1);
        return;
    }
    // de Casteljau split at t = 0.5.
    let m01 = mid(p0, c1);
    let m12 = mid(c1, c2);
    let m23 = mid(c2, p1);
    let m012 = mid(m01, m12);
    let m123 = mid(m12, m23);
    let m0123 = mid(m012, m123);
    flatten_segment(p0, m01, m012, m0123, depth + 1, out);
    flatten_segment(m0123, m123, m23, p1, depth + 1, out);
}

fn mid(a: Vec2, b: Vec2) -> Vec2 {
    Vec2((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5)
}

/// Both control points within [`FLATTEN_TOLERANCE`] of the chord p0→p1
/// (point-to-segment distance, so control points overshooting a short chord
/// still force subdivision).
fn flat_enough(p0: Vec2, c1: Vec2, c2: Vec2, p1: Vec2) -> bool {
    point_chord_distance(p0, c1, p1) <= FLATTEN_TOLERANCE
        && point_chord_distance(p0, c2, p1) <= FLATTEN_TOLERANCE
}

fn point_chord_distance(p0: Vec2, p: Vec2, p1: Vec2) -> f32 {
    let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
    let len_sq = dx * dx + dy * dy;
    if len_sq == 0.0 {
        return distance(p, p0);
    }
    let t = (((p.0 - p0.0) * dx + (p.1 - p0.1) * dy) / len_sq).clamp(0.0, 1.0);
    distance(p, Vec2(p0.0 + t * dx, p0.1 + t * dy))
}

fn distance(a: Vec2, b: Vec2) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2(x, y)
    }

    /// Largest deviation of the true cubic curve from the flattened
    /// polyline, sampled densely.
    fn max_curve_deviation(p0: Vec2, c1: Vec2, c2: Vec2, p1: Vec2, poly: &[Vec2]) -> f32 {
        let cubic = |t: f32| {
            let mt = 1.0 - t;
            Vec2(
                mt * mt * mt * p0.0
                    + 3.0 * mt * mt * t * c1.0
                    + 3.0 * mt * t * t * c2.0
                    + t * t * t * p1.0,
                mt * mt * mt * p0.1
                    + 3.0 * mt * mt * t * c1.1
                    + 3.0 * mt * t * t * c2.1
                    + t * t * t * p1.1,
            )
        };
        (0..=1000)
            .map(|i| {
                let p = cubic(i as f32 / 1000.0);
                poly.windows(2)
                    .map(|w| point_chord_distance(w[0], p, w[1]))
                    .fold(f32::INFINITY, f32::min)
            })
            .fold(0.0, f32::max)
    }

    #[test]
    fn zero_tangents_pass_through_as_straight_lines() {
        let points = [v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0)];
        let flat = flatten_path(&points, None, None, false);
        assert_eq!(flat, points);
    }

    #[test]
    fn closed_path_flattens_the_closing_segment() {
        // All-zero tangents: the polyline is the control polygon itself.
        let points = [v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0)];
        let flat = flatten_path(&points, None, None, true);
        assert_eq!(flat, points);

        // A tangent on the closing segment subdivides it (3 corners + curve).
        let out_tans = [v(0.0, 0.0), v(0.0, 0.0), v(20.0, 0.0)];
        let flat = flatten_path(&points, None, Some(&out_tans), true);
        assert!(flat.len() > 3);
        assert_eq!(flat[0], points[0]);
    }

    #[test]
    fn curved_segment_stays_within_tolerance() {
        let p0 = v(0.0, 0.0);
        let p1 = v(100.0, 0.0);
        // Strong symmetric bulge: c1 = p0 + out, c2 = p1 + in.
        let out_tans = [v(0.0, 80.0)];
        let in_tans = [v(0.0, 0.0), v(0.0, 80.0)];
        let flat = flatten_path(&[p0, p1], Some(&in_tans), Some(&out_tans), false);
        assert!(flat.len() > 2, "curve must be subdivided");
        assert_eq!(*flat.first().unwrap(), p0);
        assert_eq!(*flat.last().unwrap(), p1);
        let c1 = v(0.0, 80.0);
        let c2 = v(100.0, 80.0);
        let deviation = max_curve_deviation(p0, c1, c2, p1, &flat);
        assert!(
            deviation <= FLATTEN_TOLERANCE,
            "max deviation {deviation} exceeds {FLATTEN_TOLERANCE}"
        );
    }

    #[test]
    fn mixed_corner_and_smooth_segments() {
        let points = [v(0.0, 0.0), v(50.0, 0.0), v(100.0, 0.0)];
        // Curve only between point 0 and 1; point 1→2 is a straight corner.
        let out_tans = [v(0.0, 40.0), v(0.0, 0.0)];
        let in_tans = [v(0.0, 0.0), v(0.0, 40.0)];
        let flat = flatten_path(&points, Some(&in_tans), Some(&out_tans), false);
        assert_eq!(flat[0], points[0]);
        assert_eq!(*flat.last().unwrap(), points[2]);
        // The corner joins the curve and the line at the shared point.
        assert!(flat.iter().any(|p| *p == points[1]));
    }

    #[test]
    fn degenerate_inputs_do_not_blow_up() {
        assert!(flatten_path(&[], None, None, false).is_empty());
        let single = flatten_path(&[v(1.0, 2.0)], None, None, false);
        assert_eq!(single, [v(1.0, 2.0)]);
        // Duplicate points and NaN tangents terminate via the depth cap.
        let points = [v(0.0, 0.0), v(0.0, 0.0)];
        let out_tans = [v(f32::NAN, 0.0)];
        let flat = flatten_path(&points, None, Some(&out_tans), false);
        assert!(flat.len() <= (1 << MAX_DEPTH) + 1);
    }

    #[test]
    fn short_chord_with_overshooting_control_point_subdivides() {
        // The control point sits ON the chord line but far past its end:
        // point-to-segment distance must catch the overshoot.
        let p0 = v(0.0, 0.0);
        let p1 = v(1.0, 0.0);
        let out_tans = [v(50.0, 0.0)];
        let flat = flatten_path(&[p0, p1], None, Some(&out_tans), false);
        assert!(flat.len() > 2);
    }
}
