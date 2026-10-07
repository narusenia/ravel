// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Size-aware alignment and distribution of a geometry's elements.
//!
//! An *element* is a primitive (its extent is the bounds of its points, as
//! `geometry.measure`'s `size`) or, for a geometry with no primitives, an
//! instance (its extent is the stamped source placed by the instance's scale
//! and turn, [`instance_extents`]). Elements move rigidly along one axis:
//! primitives by translating their points, instances by their `P`.
//!
//! Two things a modulation cannot express live here: aligning by a box edge
//! rather than a centre, and spacing by the **gap between edges**, which
//! needs every element's size.

use super::ops::instance_extents;
use super::{Domain, Geometry, GeometryError, names};
use crate::types::Vec2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistributeAxis {
    X,
    Y,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistributeMode {
    /// Line the low edges (left for x, top for y) up with the group's lowest.
    AlignMin,
    /// Line the centres up with the centre of the group's bounds.
    AlignCenter,
    /// Line the high edges (right for x, bottom for y) up with the group's highest.
    AlignMax,
    /// Equal distance between neighbouring centres.
    SpaceCenters,
    /// Equal empty gap between neighbouring edges.
    SpaceGaps,
}

/// Moves the elements of `geometry` along `axis` as `mode` says.
///
/// **Anchoring** (the two spacing modes): elements are ordered by centre along
/// the axis (ties keep index order). The first and the last in that order stay
/// where they are, as in a design tool, and the others are placed between
/// them. For `SpaceGaps` the fixed span runs from the first element's low edge
/// to the last element's high edge; the gaps share what the sizes leave over
/// and go negative (overlap) when the elements do not fit.
///
/// Fewer than three elements has nothing to space, and a lone element is
/// already aligned to its own bounds, so those return the input unchanged.
/// A geometry with primitives moves only them; an instance domain beside them
/// stays put. A point shared by several primitives moves with the first.
pub fn distribute(
    geometry: &Geometry,
    axis: DistributeAxis,
    mode: DistributeMode,
) -> Result<Geometry, GeometryError> {
    let use_primitives = geometry.primitive_count() > 0;
    let rects = if use_primitives {
        primitive_extents(geometry)?
    } else {
        if let Some(positions) = geometry.positions(Domain::Instance) {
            positions?.require_planar("geometry.distribute")?;
        }
        instance_extents(geometry).unwrap_or_default()
    };
    let spans: Vec<(f32, f32)> = rects
        .iter()
        .map(|r| match axis {
            DistributeAxis::X => (r.x, r.x + r.width),
            DistributeAxis::Y => (r.y, r.y + r.height),
        })
        .collect();
    let shifts = shifts(&spans, mode);
    if shifts.iter().all(|s| *s == 0.0) {
        return Ok(geometry.clone());
    }
    let by = |s: f32| match axis {
        DistributeAxis::X => Vec2(s, 0.0),
        DistributeAxis::Y => Vec2(0.0, s),
    };
    let mut out = geometry.clone();
    if use_primitives {
        let mut moved = vec![false; geometry.point_count()];
        let mut runs = Vec::new();
        for (primitive, shift) in geometry.primitives().iter().zip(&shifts) {
            runs.push((primitive.verts().clone(), *shift));
        }
        let positions = out.points_mut().make_mut(names::P)?.as_vec2_mut(names::P)?;
        for (run, shift) in runs {
            let d = by(shift);
            for i in run {
                if !std::mem::replace(&mut moved[i], true) {
                    positions[i] = Vec2(positions[i].0 + d.0, positions[i].1 + d.1);
                }
            }
        }
    } else {
        let positions = out
            .instances_mut()
            .make_mut(names::P)?
            .as_vec2_mut(names::P)?;
        for (p, shift) in positions.iter_mut().zip(&shifts) {
            let d = by(*shift);
            *p = Vec2(p.0 + d.0, p.1 + d.1);
        }
    }
    Ok(out)
}

/// Bounds of each primitive's points. A primitive with no points sits at the
/// origin with no size.
fn primitive_extents(geometry: &Geometry) -> Result<Vec<crate::types::Rect>, GeometryError> {
    let points = match geometry.positions(Domain::Point) {
        Some(positions) => positions?.require_planar("geometry.distribute")?.to_vec(),
        None => Vec::new(),
    };
    Ok(geometry
        .primitives()
        .iter()
        .map(|primitive| {
            let run = points.get(primitive.verts().clone()).unwrap_or(&[]);
            let (mut lo, mut hi) = (Vec2(0.0, 0.0), Vec2(0.0, 0.0));
            for (i, p) in run.iter().enumerate() {
                if i == 0 {
                    (lo, hi) = (*p, *p);
                }
                lo = Vec2(lo.0.min(p.0), lo.1.min(p.1));
                hi = Vec2(hi.0.max(p.0), hi.1.max(p.1));
            }
            crate::types::Rect {
                x: lo.0,
                y: lo.1,
                width: hi.0 - lo.0,
                height: hi.1 - lo.1,
            }
        })
        .collect())
}

/// The translation of each element along the axis, given its `(low, high)`
/// extent on it.
fn shifts(spans: &[(f32, f32)], mode: DistributeMode) -> Vec<f32> {
    let n = spans.len();
    let mut shifts = vec![0.0; n];
    match mode {
        DistributeMode::AlignMin | DistributeMode::AlignCenter | DistributeMode::AlignMax => {
            if n < 2 {
                return shifts;
            }
            let lo = spans.iter().map(|s| s.0).fold(f32::INFINITY, f32::min);
            let hi = spans.iter().map(|s| s.1).fold(f32::NEG_INFINITY, f32::max);
            for (shift, (a, b)) in shifts.iter_mut().zip(spans) {
                *shift = match mode {
                    DistributeMode::AlignMin => lo - a,
                    DistributeMode::AlignMax => hi - b,
                    _ => (lo + hi) * 0.5 - (a + b) * 0.5,
                };
            }
        }
        DistributeMode::SpaceCenters | DistributeMode::SpaceGaps => {
            if n < 3 {
                return shifts;
            }
            let centre = |i: usize| (spans[i].0 + spans[i].1) * 0.5;
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|a, b| centre(*a).total_cmp(&centre(*b)));
            let (first, last) = (order[0], order[n - 1]);
            if mode == DistributeMode::SpaceCenters {
                let step = (centre(last) - centre(first)) / (n - 1) as f32;
                for (k, i) in order.iter().enumerate() {
                    shifts[*i] = centre(first) + step * k as f32 - centre(*i);
                }
            } else {
                let sizes: f32 = spans.iter().map(|s| s.1 - s.0).sum();
                let gap = (spans[last].1 - spans[first].0 - sizes) / (n - 1) as f32;
                let mut cursor = spans[first].0;
                for i in order {
                    shifts[i] = cursor - spans[i].0;
                    cursor += spans[i].1 - spans[i].0 + gap;
                }
            }
        }
    }
    shifts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{AttributeArray, Primitive};
    use std::sync::Arc;

    /// One closed square-ish path per `(x, width)`, all 10 tall on y = 0..10.
    fn boxes(spans: &[(f32, f32)]) -> Geometry {
        let mut points = Vec::new();
        for (x, w) in spans {
            points.extend([Vec2(*x, 0.0), Vec2(x + w, 0.0), Vec2(x + w, 10.0)]);
        }
        let mut g = Geometry::from_points(points);
        for i in 0..spans.len() {
            g.push_primitive(Primitive::Path {
                verts: i * 3..i * 3 + 3,
                closed: true,
            });
        }
        g
    }

    /// Left and right edge of each box after the operation.
    fn edges(g: &Geometry, count: usize) -> Vec<(f32, f32)> {
        let p = g.points().get(names::P).unwrap().as_vec2(names::P).unwrap();
        (0..count).map(|i| (p[i * 3].0, p[i * 3 + 1].0)).collect()
    }

    fn close(actual: &[(f32, f32)], expected: &[(f32, f32)]) {
        assert_eq!(actual.len(), expected.len());
        for (a, e) in actual.iter().zip(expected) {
            assert!(
                (a.0 - e.0).abs() < 1e-4 && (a.1 - e.1).abs() < 1e-4,
                "{actual:?} vs {expected:?}"
            );
        }
    }

    /// Widths 10, 30, 20 inside 0..100.
    fn uneven() -> Geometry {
        boxes(&[(0.0, 10.0), (20.0, 30.0), (80.0, 20.0)])
    }

    #[test]
    fn spacing_by_centres_and_by_gaps_differ_for_unequal_sizes() {
        let by_centres =
            distribute(&uneven(), DistributeAxis::X, DistributeMode::SpaceCenters).unwrap();
        let by_gaps = distribute(&uneven(), DistributeAxis::X, DistributeMode::SpaceGaps).unwrap();
        assert_ne!(edges(&by_centres, 3), edges(&by_gaps, 3));
    }

    #[test]
    fn gap_spacing_gives_equal_gaps_between_unequal_widths() {
        let out = distribute(&uneven(), DistributeAxis::X, DistributeMode::SpaceGaps).unwrap();
        // span 0..100, sizes 60, so two gaps of 20; first and last stay.
        close(&edges(&out, 3), &[(0.0, 10.0), (30.0, 60.0), (80.0, 100.0)]);
    }

    #[test]
    fn centre_spacing_gives_equal_centre_distance() {
        let out = distribute(&uneven(), DistributeAxis::X, DistributeMode::SpaceCenters).unwrap();
        // centres 5 and 90 are fixed; the middle one goes to 47.5 (width 30).
        close(&edges(&out, 3), &[(0.0, 10.0), (32.5, 62.5), (80.0, 100.0)]);
    }

    #[test]
    fn spacing_orders_by_centre_not_by_index() {
        // Same boxes, listed out of order: the result is the same layout.
        let g = boxes(&[(80.0, 20.0), (0.0, 10.0), (20.0, 30.0)]);
        let out = distribute(&g, DistributeAxis::X, DistributeMode::SpaceGaps).unwrap();
        close(&edges(&out, 3), &[(80.0, 100.0), (0.0, 10.0), (30.0, 60.0)]);
    }

    #[test]
    fn the_six_bounding_box_alignments() {
        // x: widths 10 and 30 at 0..10 and 20..50; y: boxes of height 10 and
        // 40 (the second is stretched below).
        let mut g = boxes(&[(0.0, 10.0), (20.0, 30.0)]);
        g.points_mut()
            .make_mut(names::P)
            .unwrap()
            .as_vec2_mut(names::P)
            .unwrap()[5] = Vec2(50.0, 40.0);
        let run = |axis, mode| distribute(&g, axis, mode).unwrap();
        let x = |mode| edges(&run(DistributeAxis::X, mode), 2);
        close(&x(DistributeMode::AlignMin), &[(0.0, 10.0), (0.0, 30.0)]);
        close(
            &x(DistributeMode::AlignCenter),
            &[(20.0, 30.0), (10.0, 40.0)],
        );
        close(&x(DistributeMode::AlignMax), &[(40.0, 50.0), (20.0, 50.0)]);
        // y extents are 0..10 and 0..40.
        let y = |mode| {
            let out = run(DistributeAxis::Y, mode);
            let p = out
                .points()
                .get(names::P)
                .unwrap()
                .as_vec2(names::P)
                .unwrap()
                .to_vec();
            ((p[0].1, p[2].1), (p[3].1, p[5].1))
        };
        assert_eq!(y(DistributeMode::AlignMin), ((0.0, 10.0), (0.0, 40.0)));
        assert_eq!(y(DistributeMode::AlignCenter), ((15.0, 25.0), (0.0, 40.0)));
        assert_eq!(y(DistributeMode::AlignMax), ((30.0, 40.0), (0.0, 40.0)));
    }

    #[test]
    fn one_and_two_elements_are_left_alone() {
        const ALL: [DistributeMode; 5] = [
            DistributeMode::AlignMin,
            DistributeMode::AlignCenter,
            DistributeMode::AlignMax,
            DistributeMode::SpaceCenters,
            DistributeMode::SpaceGaps,
        ];
        let one = boxes(&[(7.0, 10.0)]);
        for mode in ALL {
            assert_eq!(
                edges(&distribute(&one, DistributeAxis::X, mode).unwrap(), 1),
                [(7.0, 17.0)]
            );
        }
        let two = boxes(&[(0.0, 10.0), (20.0, 30.0)]);
        for mode in [DistributeMode::SpaceCenters, DistributeMode::SpaceGaps] {
            assert_eq!(
                edges(&distribute(&two, DistributeAxis::X, mode).unwrap(), 2),
                [(0.0, 10.0), (20.0, 50.0)]
            );
        }
        // Two elements still align, and an empty geometry does not panic.
        let aligned = distribute(&two, DistributeAxis::X, DistributeMode::AlignMin).unwrap();
        close(&edges(&aligned, 2), &[(0.0, 10.0), (0.0, 30.0)]);
        distribute(
            &Geometry::new(),
            DistributeAxis::X,
            DistributeMode::SpaceGaps,
        )
        .unwrap();
    }

    /// Three instances of a 10-wide square, scaled 1, 3 and 2, at x = 0, 20, 80.
    fn stamped() -> Geometry {
        let mut square = Geometry::from_points(vec![
            Vec2(-5.0, -5.0),
            Vec2(5.0, -5.0),
            Vec2(5.0, 5.0),
            Vec2(-5.0, 5.0),
        ]);
        square.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        let mut g = Geometry::new();
        let instances = g.instances_mut();
        instances
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(20.0, 0.0), Vec2(80.0, 0.0)]),
            )
            .unwrap();
        instances
            .insert(
                names::SCALE,
                AttributeArray::Vec2(vec![Vec2(1.0, 1.0), Vec2(3.0, 1.0), Vec2(2.0, 1.0)]),
            )
            .unwrap();
        g.set_instance_source(Some(Arc::new(square)));
        g
    }

    fn instance_x(g: &Geometry) -> Vec<f32> {
        g.instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap()
            .iter()
            .map(|p| p.0)
            .collect()
    }

    #[test]
    fn instance_placement_accounts_for_source_size_times_scale() {
        // Extents -5..5, 5..35, 70..90 (widths 10, 30, 20). Equal gaps of
        // 17.5 put the middle centre at 37.5; ignoring size would say 40.
        let gaps = distribute(&stamped(), DistributeAxis::X, DistributeMode::SpaceGaps).unwrap();
        assert_eq!(instance_x(&gaps), [0.0, 37.5, 80.0]);
        let centres =
            distribute(&stamped(), DistributeAxis::X, DistributeMode::SpaceCenters).unwrap();
        assert_eq!(instance_x(&centres), [0.0, 40.0, 80.0]);
        // Aligning left edges: each instance's left edge goes to -5.
        let left = distribute(&stamped(), DistributeAxis::X, DistributeMode::AlignMin).unwrap();
        assert_eq!(instance_x(&left), [0.0, 10.0, 5.0]);
    }

    #[test]
    fn evaluating_twice_gives_the_same_result() {
        let run =
            |g: &Geometry| distribute(g, DistributeAxis::X, DistributeMode::SpaceGaps).unwrap();
        let g = uneven();
        assert_eq!(edges(&run(&g), 3), edges(&run(&g), 3));
        let g = stamped();
        assert_eq!(instance_x(&run(&g)), instance_x(&run(&g)));
    }
}
