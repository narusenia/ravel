// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Polygon triangulation, the shared base of Voronoi fracture and the 3D
//! extrusion caps.
//!
//! The algorithm is [`earcut`](https://docs.rs/earcut) — GeoRust's port of
//! mapbox earcut, adopted in `docs/implementation/path-ops-plan.md` Phase 0b.
//! Two properties of that crate are the reason this module is a *type* rather
//! than a free function:
//!
//! - its scratch buffers are cleared, never freed, so one [`Triangulator`]
//!   held across frames triangulates without allocating; and
//! - it writes into a caller-owned `Vec<u32>`, which is exactly the shape
//!   [`Geometry::push_mesh`](super::Geometry::push_mesh) consumes — no
//!   `usize` round trip between the triangulator and the index buffer.
//!
//! Input is an **already flattened** polyline: curved paths are subdivided
//! upstream by `ravel_nodes::flatten` at its `FLATTEN_TOLERANCE`, and this
//! module deliberately introduces no tolerance of its own — there is no
//! second, competing notion of "close enough" to keep in sync.
//!
//! `earcut` panics on exactly two documented inputs, both of them hole-index
//! preconditions. Neither is allowed to reach it: [`Triangulator::triangulate`]
//! validates the ring starts and returns a [`GeometryError`] instead.

use std::fmt;

use earcut::Earcut;

use super::attribute::GeometryError;
use crate::types::Vec2;

/// Triangulates polygons with holes, reusing its buffers between calls.
///
/// Hold one per evaluation path and call [`Self::triangulate`] per polygon;
/// constructing a fresh instance per frame throws away the whole point of the
/// type. The returned slice borrows the instance, so the triangles are copied
/// out (usually straight into `push_mesh`) before the next call.
///
/// ```
/// use ravel_core::geometry::{Geometry, Triangulator};
/// use ravel_core::types::Vec2;
///
/// let ring = vec![
///     Vec2(0.0, 0.0),
///     Vec2(10.0, 0.0),
///     Vec2(10.0, 10.0),
///     Vec2(0.0, 10.0),
/// ];
/// let mut triangulator = Triangulator::new();
/// let triangles = triangulator.triangulate(&ring, &[]).unwrap();
/// assert_eq!(triangles.len(), 6); // two triangles
///
/// let mut geometry = Geometry::from_points(ring);
/// geometry.push_mesh(0..4, triangles);
/// geometry.validate().unwrap();
/// ```
#[derive(Default)]
pub struct Triangulator {
    earcut: Earcut<f32>,
    triangles: Vec<u32>,
}

/// `earcut::Earcut` is opaque and not `Debug`, so report what a caller can
/// act on: how much of the reusable buffer the last polygon needed.
impl fmt::Debug for Triangulator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Triangulator")
            .field("triangles", &(self.triangles.len() / 3))
            .field("capacity", &self.triangles.capacity())
            .finish_non_exhaustive()
    }
}

impl Triangulator {
    /// A triangulator with empty buffers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Triangulates one polygon.
    ///
    /// `vertices` holds the outer ring followed by every hole ring, each ring
    /// given once and **not** repeating its first vertex at the end.
    /// `hole_starts` gives the index in `vertices` where each hole ring
    /// begins, so ring `i` spans `hole_starts[i]` up to `hole_starts[i + 1]`
    /// (or the end of `vertices`). An empty `hole_starts` means a solid
    /// polygon.
    ///
    /// The returned indices are three per triangle and address `vertices`
    /// directly, which is the "relative to `verts.start`" encoding
    /// [`Primitive::Mesh`](super::Primitive::Mesh) stores — pass the slice to
    /// [`Geometry::push_mesh`](super::Geometry::push_mesh) unchanged.
    ///
    /// Degenerate polygons are not errors, because a procedural graph
    /// produces them every time a shape animates through a fold: fewer than
    /// three vertices, a zero-area ring, repeated vertices, and
    /// self-intersecting rings all yield whatever triangles `earcut` can form,
    /// possibly none. Only the hole-index preconditions are rejected, and
    /// those are rejected precisely because `earcut` would panic on them.
    ///
    /// # Errors
    ///
    /// - [`GeometryError::HoleRingsOutOfOrder`] when `hole_starts` descends.
    /// - [`GeometryError::HoleRingOutOfRange`] when a ring starts past
    ///   `vertices`.
    /// - [`GeometryError::LengthMismatch`] when `vertices` is longer than a
    ///   `u32` index can address.
    pub fn triangulate(
        &mut self,
        vertices: &[Vec2],
        hole_starts: &[u32],
    ) -> Result<&[u32], GeometryError> {
        // Clear first so a rejected call cannot hand the previous polygon's
        // triangles to a caller that ignores the error.
        self.triangles.clear();
        let vertex_count = vertices.len();
        if u32::try_from(vertex_count).is_err() {
            return Err(GeometryError::LengthMismatch {
                name: "triangulation vertices".into(),
                expected: u32::MAX as usize,
                actual: vertex_count,
            });
        }

        let mut previous = 0usize;
        for (position, &start) in hole_starts.iter().enumerate() {
            let start = start as usize;
            if start > vertex_count {
                return Err(GeometryError::HoleRingOutOfRange {
                    position,
                    start,
                    vertex_count,
                });
            }
            if position > 0 && start < previous {
                return Err(GeometryError::HoleRingsOutOfOrder {
                    position,
                    previous,
                    start,
                });
            }
            previous = start;
        }

        self.earcut.earcut(
            vertices.iter().map(|v| [v.0, v.1]),
            hole_starts,
            &mut self.triangles,
        );
        Ok(&self.triangles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2(x, y)
    }

    /// A closed ring's signed area (shoelace). Positive is counter-clockwise
    /// in a y-up reading of the coordinates.
    fn ring_area(ring: &[Vec2]) -> f32 {
        let mut sum = 0.0;
        for i in 0..ring.len() {
            let a = ring[i];
            let b = ring[(i + 1) % ring.len()];
            sum += a.0 * b.1 - b.0 * a.1;
        }
        sum * 0.5
    }

    /// Per-triangle signed areas, in output order.
    fn triangle_areas(vertices: &[Vec2], triangles: &[u32]) -> Vec<f32> {
        triangles
            .chunks_exact(3)
            .map(|t| {
                ring_area(&[
                    vertices[t[0] as usize],
                    vertices[t[1] as usize],
                    vertices[t[2] as usize],
                ])
            })
            .collect()
    }

    /// Squares are the smallest polygon with more than one tessellation, so a
    /// count check on them is the base case for `n - 2`.
    #[test]
    fn convex_polygons_produce_n_minus_two_triangles() {
        let mut triangulator = Triangulator::new();

        let square = [v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0), v(0.0, 10.0)];
        assert_eq!(triangulator.triangulate(&square, &[]).unwrap().len(), 3 * 2);

        let hexagon: Vec<Vec2> = (0..6)
            .map(|i| {
                let angle = std::f32::consts::TAU * i as f32 / 6.0;
                v(angle.cos() * 20.0, angle.sin() * 20.0)
            })
            .collect();
        assert_eq!(
            triangulator.triangulate(&hexagon, &[]).unwrap().len(),
            3 * 4
        );
    }

    /// An `L` is the canonical concave case: one reflex corner, so a fan from
    /// any single vertex would leave the shape.
    #[test]
    fn concave_polygon_produces_n_minus_two_triangles() {
        let l_shape = [
            v(0.0, 0.0),
            v(30.0, 0.0),
            v(30.0, 10.0),
            v(10.0, 10.0),
            v(10.0, 30.0),
            v(0.0, 30.0),
        ];
        let mut triangulator = Triangulator::new();
        let triangles = triangulator.triangulate(&l_shape, &[]).unwrap();
        assert_eq!(triangles.len(), 3 * 4);

        // Every triangle stays inside the shape: the notch corner (20, 20) is
        // outside the L, so no triangle may contain it.
        let areas = triangle_areas(&l_shape, triangles);
        let total: f32 = areas.iter().map(|a| a.abs()).sum();
        assert!((total - ring_area(&l_shape).abs()).abs() < 1e-3);
    }

    /// A ring plus one hole: `n + 2h - 2` triangles, the bridge pair being
    /// what the hole costs.
    #[test]
    fn polygon_with_a_hole_produces_n_plus_two_h_minus_two_triangles() {
        let mut vertices = vec![v(0.0, 0.0), v(30.0, 0.0), v(30.0, 30.0), v(0.0, 30.0)];
        // The hole winds the other way, the usual convention; `earcut` does
        // not require it, but a real caller emits it this way.
        vertices.extend([v(10.0, 10.0), v(10.0, 20.0), v(20.0, 20.0), v(20.0, 10.0)]);
        let mut triangulator = Triangulator::new();
        let triangles = triangulator.triangulate(&vertices, &[4]).unwrap();
        assert_eq!(triangles.len(), 3 * (8 + 2 - 2));
    }

    /// `earcut::deviation` is the crate's own correctness check: the relative
    /// difference between the polygon's area and its triangulation's. Zero
    /// means every triangle is accounted for and none overlap.
    #[test]
    fn triangulated_area_matches_the_polygon_area() {
        let cases: [(&[Vec2], &[u32]); 3] = [
            (
                &[v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0), v(0.0, 10.0)],
                &[],
            ),
            (
                &[
                    v(0.0, 0.0),
                    v(30.0, 0.0),
                    v(30.0, 10.0),
                    v(10.0, 10.0),
                    v(10.0, 30.0),
                    v(0.0, 30.0),
                ],
                &[],
            ),
            (
                &[
                    v(0.0, 0.0),
                    v(30.0, 0.0),
                    v(30.0, 30.0),
                    v(0.0, 30.0),
                    v(10.0, 10.0),
                    v(10.0, 20.0),
                    v(20.0, 20.0),
                    v(20.0, 10.0),
                ],
                &[4],
            ),
        ];

        let mut triangulator = Triangulator::new();
        for (vertices, hole_starts) in cases {
            let triangles = triangulator.triangulate(vertices, hole_starts).unwrap();
            let deviation =
                earcut::deviation(vertices.iter().map(|p| [p.0, p.1]), hole_starts, triangles);
            assert!(deviation < 1e-5, "deviation {deviation} for {vertices:?}");
        }
    }

    /// The signed half of the area condition. `earcut` normalises the outer
    /// ring's winding, so the sum's *sign* is its own choice; what has to hold
    /// is that nothing cancels — a flipped or degenerate triangle would make
    /// the signed sum fall short of the sum of magnitudes.
    #[test]
    fn signed_triangle_areas_sum_without_cancelling() {
        for winding in [1.0f32, -1.0] {
            let ring = [
                v(0.0, 0.0),
                v(30.0 * winding, 0.0),
                v(30.0 * winding, 10.0),
                v(10.0 * winding, 10.0),
                v(10.0 * winding, 30.0),
                v(0.0, 30.0),
            ];
            let mut triangulator = Triangulator::new();
            let triangles = triangulator.triangulate(&ring, &[]).unwrap();
            let areas = triangle_areas(&ring, triangles);

            let signed: f32 = areas.iter().sum();
            let magnitude: f32 = areas.iter().map(|a| a.abs()).sum();
            assert!(
                (signed.abs() - magnitude).abs() < 1e-3,
                "signed {signed} vs magnitude {magnitude}: a triangle is flipped"
            );
            assert!(
                (magnitude - ring_area(&ring).abs()).abs() < 1e-3,
                "triangulated area {magnitude} != polygon area {}",
                ring_area(&ring).abs()
            );
        }
    }

    /// Procedural shapes animate through every one of these, so they are
    /// ordinary inputs rather than caller errors: the triangulator must return
    /// whatever it can and never panic.
    #[test]
    fn degenerate_polygons_do_not_panic() {
        let mut triangulator = Triangulator::new();

        assert!(triangulator.triangulate(&[], &[]).unwrap().is_empty());
        assert!(
            triangulator
                .triangulate(&[v(1.0, 2.0)], &[])
                .unwrap()
                .is_empty()
        );
        assert!(
            triangulator
                .triangulate(&[v(1.0, 2.0), v(3.0, 4.0)], &[])
                .unwrap()
                .is_empty()
        );

        // Collinear (zero area) and fully coincident rings.
        let collinear = [v(0.0, 0.0), v(10.0, 0.0), v(20.0, 0.0), v(30.0, 0.0)];
        assert!(
            triangulator
                .triangulate(&collinear, &[])
                .unwrap()
                .is_empty()
        );
        let coincident = [v(5.0, 5.0), v(5.0, 5.0), v(5.0, 5.0)];
        assert!(
            triangulator
                .triangulate(&coincident, &[])
                .unwrap()
                .is_empty()
        );

        // Repeated vertices around an otherwise valid square.
        let duplicated = [
            v(0.0, 0.0),
            v(0.0, 0.0),
            v(10.0, 0.0),
            v(10.0, 10.0),
            v(10.0, 10.0),
            v(0.0, 10.0),
        ];
        let triangles = triangulator.triangulate(&duplicated, &[]).unwrap();
        assert_eq!(triangles.len() % 3, 0);
        let area: f32 = triangle_areas(&duplicated, triangles)
            .iter()
            .map(|a| a.abs())
            .sum();
        assert!((area - 100.0).abs() < 1e-3, "area {area}");

        // Self-intersecting: a bowtie, and a ring that crosses itself twice.
        let bowtie = [v(0.0, 0.0), v(10.0, 10.0), v(10.0, 0.0), v(0.0, 10.0)];
        let triangles = triangulator.triangulate(&bowtie, &[]).unwrap();
        assert_eq!(triangles.len() % 3, 0);
        let star = [
            v(0.0, 0.0),
            v(10.0, 10.0),
            v(20.0, 0.0),
            v(0.0, 6.0),
            v(20.0, 6.0),
        ];
        assert_eq!(triangulator.triangulate(&star, &[]).unwrap().len() % 3, 0);

        // A hole that swallows the whole outer ring, and an empty hole ring.
        let swallowed = [
            v(0.0, 0.0),
            v(10.0, 0.0),
            v(10.0, 10.0),
            v(0.0, 10.0),
            v(-5.0, -5.0),
            v(-5.0, 15.0),
            v(15.0, 15.0),
            v(15.0, -5.0),
        ];
        assert_eq!(
            triangulator.triangulate(&swallowed, &[4]).unwrap().len() % 3,
            0
        );
        let square = [v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0), v(0.0, 10.0)];
        // An empty hole ring (`start == vertex count`) is in range and simply
        // contributes nothing; an empty *outer* ring leaves nothing to fill.
        assert_eq!(
            triangulator.triangulate(&square, &[4]).unwrap().len(),
            3 * 2
        );
        assert!(triangulator.triangulate(&square, &[0]).unwrap().is_empty());
    }

    /// The two inputs `earcut` documents as panics. They must come back as
    /// errors, and the buffer must not leak the previous polygon.
    #[test]
    fn invalid_hole_starts_are_rejected_instead_of_panicking() {
        let vertices = [
            v(0.0, 0.0),
            v(30.0, 0.0),
            v(30.0, 30.0),
            v(0.0, 30.0),
            v(10.0, 10.0),
            v(10.0, 20.0),
            v(20.0, 20.0),
            v(20.0, 10.0),
        ];
        let mut triangulator = Triangulator::new();
        assert!(
            !triangulator
                .triangulate(&vertices, &[4])
                .unwrap()
                .is_empty()
        );

        assert_eq!(
            triangulator.triangulate(&vertices, &[6, 4]),
            Err(GeometryError::HoleRingsOutOfOrder {
                position: 1,
                previous: 6,
                start: 4,
            })
        );
        assert_eq!(
            triangulator.triangulate(&vertices, &[4, 9]),
            Err(GeometryError::HoleRingOutOfRange {
                position: 1,
                start: 9,
                vertex_count: 8,
            })
        );
        // A rejected call leaves nothing behind for the next reader.
        assert!(triangulator.triangulate(&[], &[]).unwrap().is_empty());
    }

    /// Every frame re-triangulates, so identical input has to give identical
    /// output — both from a fresh instance and from a reused one carrying the
    /// previous polygon's scratch state.
    #[test]
    fn triangulation_is_deterministic() {
        let vertices = [
            v(0.0, 0.0),
            v(30.0, 0.0),
            v(30.0, 30.0),
            v(0.0, 30.0),
            v(10.0, 10.0),
            v(10.0, 20.0),
            v(20.0, 20.0),
            v(20.0, 10.0),
        ];
        let other = [v(-4.0, 1.0), v(7.0, -2.0), v(3.0, 9.0)];

        let first = Triangulator::new()
            .triangulate(&vertices, &[4])
            .unwrap()
            .to_vec();

        let mut reused = Triangulator::new();
        reused.triangulate(&other, &[]).unwrap();
        let second = reused.triangulate(&vertices, &[4]).unwrap().to_vec();
        let third = reused.triangulate(&vertices, &[4]).unwrap().to_vec();

        assert_eq!(first, second);
        assert_eq!(second, third);
    }

    /// The reason the triangulator is a struct: a warmed-up instance stops
    /// growing its buffers, so a per-frame call allocates nothing.
    #[test]
    fn buffers_are_reused_across_calls() {
        let vertices = [
            v(0.0, 0.0),
            v(30.0, 0.0),
            v(30.0, 30.0),
            v(0.0, 30.0),
            v(10.0, 10.0),
            v(10.0, 20.0),
            v(20.0, 20.0),
            v(20.0, 10.0),
        ];
        let mut triangulator = Triangulator::new();
        triangulator.triangulate(&vertices, &[4]).unwrap();
        let capacity = triangulator.triangles.capacity();
        assert!(capacity > 0);
        for _ in 0..8 {
            triangulator.triangulate(&vertices, &[4]).unwrap();
        }
        assert_eq!(triangulator.triangles.capacity(), capacity);
    }
}
