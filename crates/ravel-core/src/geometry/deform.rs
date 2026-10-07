// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Controlled deformers: bend, twist and taper along an axis.
//!
//! Each is a **pure function of a point's position** ([`deform_point`]), so the
//! same input always lands in the same place and a deformer composes with
//! anything that moves points. The bezier tangents (`in_tan` / `out_tan`) are
//! offsets from their anchor, so they are carried by the deformation's
//! Jacobian at the anchor ([`deform_vector`]); leaving them alone would turn
//! every curve control point back toward where it was and kink the curve.
//!
//! One axis / range / group handling serves all three. Work happens in axis
//! coordinates `(a, b)`: `a` runs along the axis, `b` is the perpendicular.
//! The deformation acts over `a` in `start..end`; before `start` points do not
//! move, past `end` they follow the end rigidly (bend), keep the final twist
//! (twist) or keep the final width (taper), so nothing tears at the range
//! edge.
//!
//! Planar only: a 3D `P` is an error, the same stance `attribute.path_sample`
//! takes. Point domain only: instance placements are not deformed.

use super::{Geometry, GeometryError, names};
use crate::types::Vec2;

/// The three deformations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deformer {
    /// `amount` is the total bend in degrees over the range: the axis becomes
    /// an arc of that angle, curling toward the positive perpendicular.
    Bend,
    /// `amount` is the total twist in degrees over the range. In the plane a
    /// twist around the axis shows as the perpendicular foreshortening by
    /// `cos(angle)` (the orthographic view of the 3D rotation), so a half turn
    /// mirrors the shape.
    Twist,
    /// The width at the end of the range is `1 - amount` times the original,
    /// narrowing linearly from the start.
    Taper,
}

/// Which coordinate runs along the axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeformAxis {
    X,
    Y,
}

/// A deformation: the shared parameters of the three nodes.
#[derive(Clone, Copy, Debug)]
pub struct DeformSpec<'a> {
    pub deformer: Deformer,
    pub axis: DeformAxis,
    pub start: f32,
    pub end: f32,
    pub amount: f32,
    /// Bool point attribute restricting the points moved; empty is all.
    pub group: &'a str,
}

type Jacobian = [[f64; 2]; 2];

/// `(a', b', J)` for axis coordinates, or `None` where the point does not move.
fn map_local(spec: &DeformSpec<'_>, a: f64, b: f64) -> Option<(f64, f64, Jacobian)> {
    let start = spec.start as f64;
    let len = spec.end as f64 - start;
    let t = a - start;
    if t <= 0.0 || len <= 0.0 {
        return None;
    }
    let amount = spec.amount as f64;
    match spec.deformer {
        Deformer::Bend => {
            let theta = amount.to_radians();
            let u = t.min(len);
            let alpha = theta * u / len;
            let (sin, cos) = alpha.sin_cos();
            // R sin(alpha) and R (1 - cos(alpha)) with R = u / alpha, written
            // so that theta -> 0 is the identity rather than 0 / 0.
            let (sinc, vers) = if alpha.abs() < 1e-3 {
                (
                    1.0 - alpha * alpha / 6.0,
                    alpha / 2.0 - alpha.powi(3) / 24.0,
                )
            } else {
                (sin / alpha, (1.0 - cos) / alpha)
            };
            let mut a2 = start + u * sinc - b * sin;
            let mut b2 = u * vers + b * cos;
            let jacobian = if t <= len {
                let k = 1.0 - b * theta / len;
                [[k * cos, -sin], [k * sin, cos]]
            } else {
                // Past the end the shape continues straight along the end
                // tangent, rigidly turned by the whole bend.
                let rest = t - len;
                a2 += rest * cos;
                b2 += rest * sin;
                [[cos, -sin], [sin, cos]]
            };
            Some((a2, b2, jacobian))
        }
        Deformer::Twist => {
            let theta = amount.to_radians();
            let phi = theta * (t / len).min(1.0);
            let dphi = if t < len { theta / len } else { 0.0 };
            let (sin, cos) = phi.sin_cos();
            Some((a, b * cos, [[1.0, 0.0], [-b * sin * dphi, cos]]))
        }
        Deformer::Taper => {
            let s = 1.0 - amount * (t / len).min(1.0);
            let ds = if t < len { -amount / len } else { 0.0 };
            Some((a, b * s, [[1.0, 0.0], [b * ds, s]]))
        }
    }
}

fn to_local(axis: DeformAxis, v: Vec2) -> (f64, f64) {
    match axis {
        DeformAxis::X => (v.0 as f64, v.1 as f64),
        DeformAxis::Y => (v.1 as f64, v.0 as f64),
    }
}

fn from_local(axis: DeformAxis, a: f64, b: f64) -> Vec2 {
    match axis {
        DeformAxis::X => Vec2(a as f32, b as f32),
        DeformAxis::Y => Vec2(b as f32, a as f32),
    }
}

/// Where the deformation puts the point `p`.
pub fn deform_point(spec: &DeformSpec<'_>, p: Vec2) -> Vec2 {
    let (a, b) = to_local(spec.axis, p);
    match map_local(spec, a, b) {
        Some((a2, b2, _)) => from_local(spec.axis, a2, b2),
        None => p,
    }
}

/// The offset `v` (a bezier tangent) at anchor `p` after the deformation:
/// the Jacobian at `p` applied to it.
pub fn deform_vector(spec: &DeformSpec<'_>, p: Vec2, v: Vec2) -> Vec2 {
    let (a, b) = to_local(spec.axis, p);
    let Some((_, _, j)) = map_local(spec, a, b) else {
        return v;
    };
    let (da, db) = to_local(spec.axis, v);
    from_local(
        spec.axis,
        j[0][0] * da + j[0][1] * db,
        j[1][0] * da + j[1][1] * db,
    )
}

/// Deforms the point domain of `geometry`: `P`, and `in_tan` / `out_tan` when
/// present. Points outside the group, and every other column, are untouched.
pub fn deform(geometry: &Geometry, spec: &DeformSpec<'_>) -> Result<Geometry, GeometryError> {
    // A zero amount is the identity exactly; recomputing positions would only
    // add rounding.
    if spec.amount == 0.0 || spec.end <= spec.start || geometry.points().get(names::P).is_none() {
        if spec.end <= spec.start {
            tracing::warn!(
                start = spec.start,
                end = spec.end,
                "deformer range is empty; leaving the geometry unchanged"
            );
        }
        return Ok(geometry.clone());
    }
    let positions = geometry
        .positions(super::Domain::Point)
        .expect("P checked above")?
        .require_planar("deformers")?
        .to_vec();
    let selection = super::field::group_selection(geometry.points(), spec.group, positions.len());
    let selected = |i: usize| selection.as_ref().is_none_or(|flags| flags[i]);

    let mut result = geometry.clone();
    for name in [names::IN_TAN, names::OUT_TAN] {
        if geometry.points().get(name).is_none() {
            continue;
        }
        let tangents = result.points_mut().make_mut(name)?.as_vec2_mut(name)?;
        for (i, tangent) in tangents.iter_mut().enumerate() {
            if selected(i) {
                *tangent = deform_vector(spec, positions[i], *tangent);
            }
        }
    }
    let moved = result
        .points_mut()
        .make_mut(names::P)?
        .as_vec2_mut(names::P)?;
    for (i, p) in moved.iter_mut().enumerate() {
        if selected(i) {
            *p = deform_point(spec, positions[i]);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::AttributeArray;
    use std::sync::Arc;

    const KINDS: [Deformer; 3] = [Deformer::Bend, Deformer::Twist, Deformer::Taper];

    fn spec(deformer: Deformer, amount: f32) -> DeformSpec<'static> {
        DeformSpec {
            deformer,
            axis: DeformAxis::X,
            start: 0.0,
            end: 100.0,
            amount,
            group: "",
        }
    }

    fn close(actual: Vec2, expected: (f32, f32)) {
        assert!(
            (actual.0 - expected.0).abs() < 1e-3 && (actual.1 - expected.1).abs() < 1e-3,
            "{actual:?} vs {expected:?}"
        );
    }

    fn points(g: &Geometry) -> Vec<Vec2> {
        g.points()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap()
            .to_vec()
    }

    fn with_tangents(points: Vec<Vec2>, out_tan: Vec<Vec2>) -> Geometry {
        let mut g = Geometry::from_points(points);
        let n = out_tan.len();
        g.points_mut()
            .insert(names::OUT_TAN, AttributeArray::Vec2(out_tan))
            .unwrap();
        g.points_mut()
            // Distinct from `out_tan`, so one moved without the other shows.
            .insert(
                names::IN_TAN,
                AttributeArray::Vec2(vec![Vec2(-3.0, 2.0); n]),
            )
            .unwrap();
        g
    }

    #[test]
    fn amount_zero_is_the_input_exactly() {
        let g = with_tangents(
            vec![Vec2(3.0, 4.0), Vec2(50.0, -7.0), Vec2(180.0, 2.0)],
            vec![Vec2(1.0, 2.0); 3],
        );
        for kind in KINDS {
            let out = deform(&g, &spec(kind, 0.0)).unwrap();
            for name in [names::P, names::IN_TAN, names::OUT_TAN] {
                assert!(
                    Arc::ptr_eq(
                        g.points().get(name).unwrap(),
                        out.points().get(name).unwrap()
                    ),
                    "{kind:?} {name}"
                );
            }
        }
        // Not a short circuit: a point before `start` is also exact when the
        // amount is non-zero.
        let out = deform(
            &Geometry::from_points(vec![Vec2(-5.0, 9.0)]),
            &spec(Deformer::Bend, 90.0),
        )
        .unwrap();
        assert_eq!(points(&out), [Vec2(-5.0, 9.0)]);
    }

    #[test]
    fn bend_curls_the_axis_into_an_arc() {
        // 90 degrees over 100 units: radius 100 / (pi / 2).
        let r = 100.0 / std::f32::consts::FRAC_PI_2;
        let g = Geometry::from_points(vec![
            Vec2(50.0, 0.0),
            Vec2(100.0, 0.0),
            Vec2(100.0, 10.0),
            Vec2(150.0, 0.0),
        ]);
        let out = points(&deform(&g, &spec(Deformer::Bend, 90.0)).unwrap());
        let s45 = std::f32::consts::FRAC_1_SQRT_2;
        close(out[0], (r * s45, r * (1.0 - s45)));
        close(out[1], (r, r));
        // Off the axis by b, toward the centre of curvature.
        close(out[2], (r - 10.0, r));
        // Past the end it runs on straight along the end tangent.
        close(out[3], (r, r + 50.0));
    }

    #[test]
    fn twist_foreshortens_the_perpendicular_by_the_angle() {
        let g = Geometry::from_points(vec![
            Vec2(0.0, 10.0),
            Vec2(50.0, 10.0),
            Vec2(100.0, 10.0),
            Vec2(200.0, 10.0),
        ]);
        let out = points(&deform(&g, &spec(Deformer::Twist, 180.0)).unwrap());
        close(out[0], (0.0, 10.0));
        close(out[1], (50.0, 0.0));
        close(out[2], (100.0, -10.0));
        close(out[3], (200.0, -10.0));
    }

    #[test]
    fn taper_narrows_linearly_and_keeps_the_end_width() {
        let g = Geometry::from_points(vec![
            Vec2(50.0, 10.0),
            Vec2(100.0, 10.0),
            Vec2(150.0, 10.0),
            Vec2(50.0, 0.0),
        ]);
        let out = points(&deform(&g, &spec(Deformer::Taper, 0.5)).unwrap());
        close(out[0], (50.0, 7.5));
        close(out[1], (100.0, 5.0));
        close(out[2], (150.0, 5.0));
        close(out[3], (50.0, 0.0));
        // The y axis is the same thing with the coordinates exchanged.
        let along_y = DeformSpec {
            axis: DeformAxis::Y,
            ..spec(Deformer::Taper, 0.5)
        };
        close(
            deform(&Geometry::from_points(vec![Vec2(10.0, 50.0)]), &along_y)
                .unwrap()
                .points()
                .get(names::P)
                .unwrap()
                .as_vec2(names::P)
                .unwrap()[0],
            (7.5, 50.0),
        );
    }

    /// The point of carrying tangents by the Jacobian: the deformed tangent is
    /// the derivative of the deformed curve, so a control point stays on the
    /// side of its anchor the curve leaves by. Compared against a finite
    /// difference of the position function, over anchors inside the range,
    /// off the axis, and past the end.
    #[test]
    fn tangents_follow_the_deformation_of_the_curve() {
        let anchors = [
            Vec2(20.0, 0.0),
            Vec2(40.0, 15.0),
            Vec2(70.0, -12.0),
            Vec2(130.0, 8.0),
        ];
        let tangents = [
            Vec2(9.0, 0.0),
            Vec2(0.0, 6.0),
            Vec2(5.0, -4.0),
            Vec2(7.0, 3.0),
        ];
        for kind in KINDS {
            for axis in [DeformAxis::X, DeformAxis::Y] {
                let spec = DeformSpec {
                    axis,
                    amount: if kind == Deformer::Taper { 0.4 } else { 120.0 },
                    ..spec(kind, 0.0)
                };
                let swap = |v: Vec2| {
                    if axis == DeformAxis::X {
                        v
                    } else {
                        Vec2(v.1, v.0)
                    }
                };
                let mut g = with_tangents(
                    anchors.iter().map(|p| swap(*p)).collect(),
                    tangents.iter().map(|t| swap(*t)).collect(),
                );
                g.points_mut()
                    .insert(
                        names::IN_TAN,
                        AttributeArray::Vec2(vec![swap(Vec2(-3.0, 2.0)); anchors.len()]),
                    )
                    .unwrap();
                let out = deform(&g, &spec).unwrap();
                let eps = 1e-3f32;
                for name in [names::OUT_TAN, names::IN_TAN] {
                    let column = out.points().get(name).unwrap().as_vec2(name).unwrap();
                    for i in 0..anchors.len() {
                        let p = swap(anchors[i]);
                        let t = if name == names::OUT_TAN {
                            swap(tangents[i])
                        } else {
                            swap(Vec2(-3.0, 2.0))
                        };
                        let a = deform_point(&spec, p);
                        let b = deform_point(&spec, Vec2(p.0 + eps * t.0, p.1 + eps * t.1));
                        let fd = ((b.0 - a.0) / eps, (b.1 - a.1) / eps);
                        assert!(
                            (column[i].0 - fd.0).abs() < 0.05 && (column[i].1 - fd.1).abs() < 0.05,
                            "{kind:?} {axis:?} {name} anchor {i}: {:?} vs finite difference {fd:?}",
                            column[i]
                        );
                    }
                }
                let out_tan = out
                    .points()
                    .get(names::OUT_TAN)
                    .unwrap()
                    .as_vec2(names::OUT_TAN)
                    .unwrap();
                // And they really moved: a deformer that left them alone would
                // pass a comparison against itself, not this one.
                assert_ne!(
                    out_tan[1],
                    swap(tangents[1]).clone(),
                    "{kind:?} left a tangent"
                );
            }
        }
    }

    #[test]
    fn points_outside_the_group_are_untouched() {
        let mut g = with_tangents(
            vec![Vec2(30.0, 5.0), Vec2(60.0, 5.0), Vec2(90.0, 5.0)],
            vec![Vec2(4.0, 1.0); 3],
        );
        g.points_mut()
            .insert("pick", AttributeArray::Bool(vec![false, true, false]))
            .unwrap();
        for kind in KINDS {
            let amount = if kind == Deformer::Taper { 0.5 } else { 90.0 };
            let out = deform(
                &g,
                &DeformSpec {
                    group: "pick",
                    ..spec(kind, amount)
                },
            )
            .unwrap();
            let (before, after) = (points(&g), points(&out));
            assert_eq!(after[0], before[0], "{kind:?}");
            assert_eq!(after[2], before[2], "{kind:?}");
            assert_ne!(after[1], before[1], "{kind:?}");
            let tan = |g: &Geometry| {
                g.points()
                    .get(names::OUT_TAN)
                    .unwrap()
                    .as_vec2(names::OUT_TAN)
                    .unwrap()
                    .to_vec()
            };
            assert_eq!(tan(&out)[0], tan(&g)[0]);
            assert_eq!(tan(&out)[2], tan(&g)[2]);
            assert_ne!(tan(&out)[1], tan(&g)[1], "{kind:?}");
            // An unusable group falls back to every point.
            let all = deform(
                &g,
                &DeformSpec {
                    group: "typo",
                    ..spec(kind, amount)
                },
            )
            .unwrap();
            assert_ne!(points(&all)[0], before[0], "{kind:?}");
        }
    }

    #[test]
    fn a_3d_position_column_is_refused() {
        let g = Geometry::from_points3(vec![crate::types::Vec3(1.0, 2.0, 3.0)]);
        assert!(deform(&g, &spec(Deformer::Bend, 90.0)).is_err());
    }

    #[test]
    fn a_tiny_bend_is_continuous_with_none() {
        let g = Geometry::from_points(vec![Vec2(60.0, 8.0)]);
        let near = points(&deform(&g, &spec(Deformer::Bend, 1e-4)).unwrap());
        close(near[0], (60.0, 8.0));
    }
}
