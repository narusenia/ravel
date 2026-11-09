// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Basic 3D primitives: `mesh.box`, `mesh.sphere`, `mesh.cylinder`,
//! `mesh.plane` (REQ-3D-005).
//!
//! Each processor emits one [`Primitive::Mesh`] with per-point `P` (`Vec3`),
//! `N` (unit, pointing out of the solid) and `uv` (`(0, 0)` top-left, `v`
//! growing downwards — what `scene.render` samples through).
//!
//! # Conventions
//!
//! * Scene space is +X right / +Y down / +Z away from the camera. "Up" on a
//!   solid is therefore −Y: a sphere's first pole is at `y = -radius`, the one
//!   `v = 0` belongs to.
//! * Cylinder and sphere axes are Y.
//! * A triangle is wound so that `(b - a) × (c - a)` points the way its
//!   vertex normals do, i.e. out of the solid. [`MeshBuilder::tri`] enforces it
//!   against a normal hint, so no builder has to get the winding right by hand.
//!   Nothing is culled by the renderer; the winding is for importers and
//!   extrusion that will need to agree with it.
//! * Faces that meet at a hard edge do not share vertices (a box has 24 for one
//!   division per side), because a vertex carries one normal and one `uv`.
//! * Each primitive carries the transform section: `geometry.transform`
//!   turns `N` by the inverse-transpose and flips the winding of a mirror, so a
//!   rotated, scaled or mirrored primitive keeps outward normals and a winding
//!   that agrees with them. `center` is the grabbable point; the section is the
//!   extra move.

use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use anyhow::bail;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{AttributeArray, Geometry, names};
use ravel_core::graph::Node;
use ravel_core::types::{NodeData, Vec2, Vec3};

/// Most vertices one primitive node will build. A division count is an `Int`
/// the user can type, and the cost is a product of them: this turns a slip of
/// a zero into an evaluation error instead of an allocation of gigabytes.
pub const MAX_MESH_VERTICES: usize = 4_000_000;

/// Smallest `segments` of a round primitive: below three the section is not a
/// polygon.
const MIN_ROUND_SEGMENTS: i32 = 3;

fn add(a: Vec3, b: Vec3) -> Vec3 {
    Vec3(a.0 + b.0, a.1 + b.1, a.2 + b.2)
}

fn scale(a: Vec3, s: f32) -> Vec3 {
    Vec3(a.0 * s, a.1 * s, a.2 * s)
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    Vec3(
        a.1 * b.2 - a.2 * b.1,
        a.2 * b.0 - a.0 * b.2,
        a.0 * b.1 - a.1 * b.0,
    )
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a.0 * b.0 + a.1 * b.1 + a.2 * b.2
}

/// Accumulates the three point columns and the triangle list of one mesh.
#[derive(Default)]
struct MeshBuilder {
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    uvs: Vec<Vec2>,
    triangles: Vec<u32>,
}

impl MeshBuilder {
    fn vertex(&mut self, position: Vec3, normal: Vec3, uv: Vec2) -> u32 {
        self.positions.push(position);
        self.normals.push(normal);
        self.uvs.push(uv);
        (self.positions.len() - 1) as u32
    }

    /// Adds triangle `(a, b, c)`, swapped to `(a, c, b)` when its geometric
    /// normal opposes the sum of its vertices' normals.
    fn tri(&mut self, a: u32, b: u32, c: u32) {
        let at = |i: u32| self.positions[i as usize];
        let geometric = cross(
            Vec3(at(b).0 - at(a).0, at(b).1 - at(a).1, at(b).2 - at(a).2),
            Vec3(at(c).0 - at(a).0, at(c).1 - at(a).1, at(c).2 - at(a).2),
        );
        let hint = [a, b, c].iter().fold(Vec3(0.0, 0.0, 0.0), |sum, &i| {
            add(sum, self.normals[i as usize])
        });
        if dot(geometric, hint) >= 0.0 {
            self.triangles.extend([a, b, c]);
        } else {
            self.triangles.extend([a, c, b]);
        }
    }

    /// A `nu × nv` grid of quads spanning `origin + s·du + t·dv`, `s, t ∈ [0,
    /// 1]`, all with `normal`; `uv` runs `(0,0)` at `origin` to `(1,1)`.
    fn grid(&mut self, origin: Vec3, du: Vec3, dv: Vec3, nu: usize, nv: usize, normal: Vec3) {
        let first = self.positions.len() as u32;
        for j in 0..=nv {
            for i in 0..=nu {
                let (s, t) = (i as f32 / nu as f32, j as f32 / nv as f32);
                self.vertex(
                    add(origin, add(scale(du, s), scale(dv, t))),
                    normal,
                    Vec2(s, t),
                );
            }
        }
        let at = |i: usize, j: usize| first + (j * (nu + 1) + i) as u32;
        for j in 0..nv {
            for i in 0..nu {
                self.tri(at(i, j), at(i + 1, j), at(i + 1, j + 1));
                self.tri(at(i, j), at(i + 1, j + 1), at(i, j + 1));
            }
        }
    }

    fn finish(self, center: Vec3) -> anyhow::Result<Arc<dyn NodeData>> {
        let count = self.positions.len();
        let positions = self.positions.into_iter().map(|p| add(p, center)).collect();
        let mut geometry = Geometry::from_points3(positions);
        geometry
            .points_mut()
            .insert(names::N, AttributeArray::Vec3(self.normals))?;
        geometry
            .points_mut()
            .insert(names::UV, AttributeArray::Vec2(self.uvs))?;
        geometry.push_mesh(0..count, &self.triangles);
        geometry.validate()?;
        Ok(Arc::new(geometry))
    }
}

/// Refuses a mesh of more than [`MAX_MESH_VERTICES`] vertices (`vertices` is
/// the count the node computed from its divisions).
fn check_budget(node: &str, vertices: usize) -> anyhow::Result<()> {
    if vertices > MAX_MESH_VERTICES {
        bail!("{node} would build {vertices} vertices; the limit is {MAX_MESH_VERTICES}");
    }
    Ok(())
}

fn divisions(params: &ResolvedParams, key: &str, default: i32, min: i32) -> usize {
    params.i32_or(key, default).max(min) as usize
}

// ---------------------------------------------------------------------------
// Box
// ---------------------------------------------------------------------------

pub struct BoxProcessor;

impl BoxProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

/// Vertex count of a box divided `(sx, sy, sz)` ways: six independent grids.
pub fn box_vertex_count(sx: usize, sy: usize, sz: usize) -> usize {
    2 * ((sx + 1) * (sy + 1) + (sx + 1) * (sz + 1) + (sy + 1) * (sz + 1))
}

impl NodeProcessor for BoxProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let [w, h, d] = params
            .vec3_or("size", [100.0, 100.0, 100.0])
            .map(|v| v.max(0.0));
        let [cx, cy, cz] = params.vec3_or("center", [0.0, 0.0, 0.0]);
        let (sx, sy, sz) = (
            divisions(params, "segments_x", 1, 1),
            divisions(params, "segments_y", 1, 1),
            divisions(params, "segments_z", 1, 1),
        );
        check_budget("mesh.box", box_vertex_count(sx, sy, sz))?;

        let (hw, hh, hd) = (w / 2.0, h / 2.0, d / 2.0);
        let (x, y, z) = (
            Vec3(1.0, 0.0, 0.0),
            Vec3(0.0, 1.0, 0.0),
            Vec3(0.0, 0.0, 1.0),
        );
        let neg = |v: Vec3| scale(v, -1.0);
        let mut b = MeshBuilder::default();
        // Each face: outward normal, then the in-face right / down directions
        // a viewer looking at it from outside would see, so the texture reads
        // unmirrored (the −Z face is the one a camera at −Z sees, right = +X).
        // `grid` takes the corner and the two edge vectors from them.
        let mut face = |normal: Vec3, right: Vec3, down: Vec3, across: f32, along: f32, nu, nv| {
            let center = scale(normal, dot(normal, Vec3(hw, hh, hd)).abs());
            let du = scale(right, across);
            let dv = scale(down, along);
            let origin = add(center, add(scale(du, -0.5), scale(dv, -0.5)));
            b.grid(origin, du, dv, nu, nv, normal);
        };
        face(neg(z), x, y, w, h, sx, sy); // front, facing the camera
        face(z, neg(x), y, w, h, sx, sy); // back
        face(x, z, y, d, h, sz, sy); // +X
        face(neg(x), neg(z), y, d, h, sz, sy); // −X
        face(neg(y), x, neg(z), w, d, sx, sz); // top (−Y is up)
        face(y, x, z, w, d, sx, sz); // bottom
        b.finish(Vec3(cx, cy, cz))
    }
}

// ---------------------------------------------------------------------------
// Plane
// ---------------------------------------------------------------------------

pub struct PlaneProcessor;

impl PlaneProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for PlaneProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let [w, h] = params.vec2_or("size", [100.0, 100.0]).map(|v| v.max(0.0));
        let [cx, cy, cz] = params.vec3_or("center", [0.0, 0.0, 0.0]);
        let (sx, sy) = (
            divisions(params, "segments_x", 1, 1),
            divisions(params, "segments_y", 1, 1),
        );
        check_budget("mesh.plane", (sx + 1) * (sy + 1))?;

        // In the XY plane facing the camera (normal −Z), the same orientation
        // and uv as the unit quad `scene.render` stamps images on.
        let mut b = MeshBuilder::default();
        b.grid(
            Vec3(-w / 2.0, -h / 2.0, 0.0),
            Vec3(w, 0.0, 0.0),
            Vec3(0.0, h, 0.0),
            sx,
            sy,
            Vec3(0.0, 0.0, -1.0),
        );
        b.finish(Vec3(cx, cy, cz))
    }
}

// ---------------------------------------------------------------------------
// Sphere
// ---------------------------------------------------------------------------

pub struct SphereProcessor;

impl SphereProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

/// Vertex count of a UV sphere: each pole is `segments` separate vertices (one
/// per triangle of its fan, so `uv.u` can sit mid-segment), and every ring
/// between them repeats its first vertex to close the `uv` seam.
pub fn sphere_vertex_count(segments: usize, rings: usize) -> usize {
    2 * segments + (rings - 1) * (segments + 1)
}

impl NodeProcessor for SphereProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let radius = params.f32_or("radius", 50.0).max(0.0);
        let [cx, cy, cz] = params.vec3_or("center", [0.0, 0.0, 0.0]);
        let segments = divisions(params, "segments", 32, MIN_ROUND_SEGMENTS);
        let rings = divisions(params, "rings", 16, 2);
        check_budget("mesh.sphere", sphere_vertex_count(segments, rings))?;

        let mut b = MeshBuilder::default();
        let on_sphere = |ring: usize, segment: usize| {
            // `ring 0` is the −Y pole (up), `ring == rings` the +Y pole.
            let phi = PI * ring as f32 / rings as f32;
            let theta = TAU * segment as f32 / segments as f32;
            Vec3(phi.sin() * theta.cos(), -phi.cos(), phi.sin() * theta.sin())
        };
        let vertex = |b: &mut MeshBuilder, unit: Vec3, u: f32, v: f32| {
            b.vertex(scale(unit, radius), unit, Vec2(u, v))
        };

        // Top fan: one vertex per segment, exactly on the axis.
        let top: Vec<u32> = (0..segments)
            .map(|s| {
                vertex(
                    &mut b,
                    Vec3(0.0, -1.0, 0.0),
                    (s as f32 + 0.5) / segments as f32,
                    0.0,
                )
            })
            .collect();
        // Rings 1..rings-1, each closed by a duplicate of its first vertex.
        let middle: Vec<Vec<u32>> = (1..rings)
            .map(|ring| {
                (0..=segments)
                    .map(|s| {
                        vertex(
                            &mut b,
                            on_sphere(ring, s % segments),
                            s as f32 / segments as f32,
                            ring as f32 / rings as f32,
                        )
                    })
                    .collect()
            })
            .collect();
        let bottom: Vec<u32> = (0..segments)
            .map(|s| {
                vertex(
                    &mut b,
                    Vec3(0.0, 1.0, 0.0),
                    (s as f32 + 0.5) / segments as f32,
                    1.0,
                )
            })
            .collect();

        // A pole band is a fan of single triangles; a band between two rings is
        // two triangles per quad. No triangle is degenerate.
        for s in 0..segments {
            b.tri(top[s], middle[0][s], middle[0][s + 1]);
        }
        for pair in middle.windows(2) {
            for s in 0..segments {
                b.tri(pair[0][s], pair[1][s], pair[1][s + 1]);
                b.tri(pair[0][s], pair[1][s + 1], pair[0][s + 1]);
            }
        }
        let last = &middle[middle.len() - 1];
        for s in 0..segments {
            b.tri(last[s], bottom[s], last[s + 1]);
        }
        b.finish(Vec3(cx, cy, cz))
    }
}

// ---------------------------------------------------------------------------
// Cylinder
// ---------------------------------------------------------------------------

pub struct CylinderProcessor;

impl CylinderProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

/// Vertex count of a cylinder: the side is `(segments + 1) × (rows + 1)`
/// (seam duplicated), and each cap is a centre plus `segments` rim vertices.
pub fn cylinder_vertex_count(segments: usize, rows: usize, caps: bool) -> usize {
    (segments + 1) * (rows + 1) + if caps { 2 * (segments + 1) } else { 0 }
}

impl NodeProcessor for CylinderProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let radius = params.f32_or("radius", 50.0).max(0.0);
        let height = params.f32_or("height", 100.0).max(0.0);
        let [cx, cy, cz] = params.vec3_or("center", [0.0, 0.0, 0.0]);
        let segments = divisions(params, "segments", 32, MIN_ROUND_SEGMENTS);
        let rows = divisions(params, "height_segments", 1, 1);
        let caps = params.bool_or("caps", true);
        check_budget("mesh.cylinder", cylinder_vertex_count(segments, rows, caps))?;

        let mut b = MeshBuilder::default();
        let half = height / 2.0;
        let rim = |s: usize| {
            let theta = TAU * s as f32 / segments as f32;
            (theta.cos(), theta.sin())
        };

        // Side: u around the axis, v down the height (−Y is the top, v = 0).
        let side: Vec<Vec<u32>> = (0..=rows)
            .map(|row| {
                let t = row as f32 / rows as f32;
                (0..=segments)
                    .map(|s| {
                        let (c, n) = rim(s % segments);
                        b.vertex(
                            Vec3(radius * c, -half + height * t, radius * n),
                            Vec3(c, 0.0, n),
                            Vec2(s as f32 / segments as f32, t),
                        )
                    })
                    .collect()
            })
            .collect();
        for pair in side.windows(2) {
            for s in 0..segments {
                b.tri(pair[0][s], pair[1][s], pair[1][s + 1]);
                b.tri(pair[0][s], pair[1][s + 1], pair[0][s + 1]);
            }
        }

        if caps {
            // A planar disc: uv is the unit circle mapped into the unit square.
            for (y, normal) in [(-half, Vec3(0.0, -1.0, 0.0)), (half, Vec3(0.0, 1.0, 0.0))] {
                let centre = b.vertex(Vec3(0.0, y, 0.0), normal, Vec2(0.5, 0.5));
                let ring: Vec<u32> = (0..segments)
                    .map(|s| {
                        let (c, n) = rim(s);
                        b.vertex(
                            Vec3(radius * c, y, radius * n),
                            normal,
                            Vec2(0.5 + 0.5 * c, 0.5 + 0.5 * n),
                        )
                    })
                    .collect();
                for s in 0..segments {
                    b.tri(centre, ring[s], ring[(s + 1) % segments]);
                }
            }
        }
        b.finish(Vec3(cx, cy, cz))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::{Evaluator, ResolvedValue};
    use ravel_core::graph::ParameterValue;
    use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::registry::{NodeRegistry, builtin::register_builtins};
    use ravel_core::types::FrameRate;

    fn run(
        processor: &dyn NodeProcessor,
        values: &[(&str, ResolvedValue)],
    ) -> anyhow::Result<Geometry> {
        let mut params = ResolvedParams::default();
        for (key, value) in values {
            params.set(key, value.clone());
        }
        let node = Node::new(NodeId::new(1), "mesh.test");
        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (100, 100));
        let out = processor.process(&node, &ctx, &[], &params, &mut Evaluator::new())?;
        Ok(out.downcast_ref::<Geometry>().expect("a geometry").clone())
    }

    fn int(n: i32) -> ResolvedValue {
        ResolvedValue::Int(n)
    }

    fn positions(g: &Geometry) -> Vec<Vec3> {
        g.points()
            .get(names::P)
            .unwrap()
            .as_vec3(names::P)
            .unwrap()
            .to_vec()
    }

    fn normals(g: &Geometry) -> Vec<Vec3> {
        g.points()
            .get(names::N)
            .unwrap()
            .as_vec3(names::N)
            .unwrap()
            .to_vec()
    }

    fn uvs(g: &Geometry) -> Vec<Vec2> {
        g.points()
            .get(names::UV)
            .unwrap()
            .as_vec2(names::UV)
            .unwrap()
            .to_vec()
    }

    fn triangles(g: &Geometry) -> Vec<[usize; 3]> {
        g.indices()
            .chunks(3)
            .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
            .collect()
    }

    fn sub(a: Vec3, b: Vec3) -> Vec3 {
        Vec3(a.0 - b.0, a.1 - b.1, a.2 - b.2)
    }

    fn length(a: Vec3) -> f32 {
        dot(a, a).sqrt()
    }

    /// `(b - a) × (c - a)` of a triangle, whose length is twice its area.
    fn face_normal(g: &Geometry, t: [usize; 3]) -> Vec3 {
        let p = positions(g);
        cross(sub(p[t[1]], p[t[0]]), sub(p[t[2]], p[t[0]]))
    }

    fn assert_counts(g: &Geometry, vertices: usize, faces: usize) {
        assert_eq!(g.point_count(), vertices, "vertices");
        assert_eq!(g.indices().len(), faces * 3, "faces");
        assert_eq!(g.primitives().len(), 1, "one mesh primitive");
        g.validate().unwrap();
    }

    /// Hand-counted, not read back from the `*_vertex_count` helpers: a box is
    /// six grids of `(a+1)(b+1)` vertices and `2ab` triangles, and so on.
    #[test]
    fn vertex_and_face_counts_follow_the_division_counts() {
        let b = |sx, sy, sz| {
            run(
                &BoxProcessor,
                &[
                    ("segments_x", int(sx)),
                    ("segments_y", int(sy)),
                    ("segments_z", int(sz)),
                ],
            )
            .unwrap()
        };
        assert_counts(&b(1, 1, 1), 24, 12);
        // 2*(3*4 + 3*5 + 4*5) vertices, 4*(2*3 + 2*4 + 3*4) triangles.
        assert_counts(&b(2, 3, 4), 94, 104);

        let sphere = |segments, rings| {
            run(
                &SphereProcessor,
                &[("segments", int(segments)), ("rings", int(rings))],
            )
            .unwrap()
        };
        // Poles 2*8, three full rings of 9: 43. Two fans of 8 and two bands of
        // 16: 48.
        assert_counts(&sphere(8, 4), 43, 48);
        assert_counts(&sphere(3, 2), 2 * 3 + 4, 6);

        let cylinder = |caps| {
            run(
                &CylinderProcessor,
                &[
                    ("segments", int(8)),
                    ("height_segments", int(2)),
                    ("caps", ResolvedValue::Bool(caps)),
                ],
            )
            .unwrap()
        };
        // Side 9 x 3 = 27 vertices and 2*8*2 = 32 triangles; each cap adds a
        // centre and 8 rim vertices and 8 triangles.
        assert_counts(&cylinder(false), 27, 32);
        assert_counts(&cylinder(true), 45, 48);

        let plane = run(
            &PlaneProcessor,
            &[("segments_x", int(3)), ("segments_y", int(2))],
        )
        .unwrap();
        assert_counts(&plane, 12, 12);
    }

    fn every_primitive(center: [f32; 3]) -> Vec<(&'static str, Geometry)> {
        let at = ("center", ResolvedValue::Vec3(center));
        vec![
            (
                "box",
                run(
                    &BoxProcessor,
                    &[at.clone(), ("segments_x", int(2)), ("segments_z", int(3))],
                )
                .unwrap(),
            ),
            (
                "sphere",
                run(
                    &SphereProcessor,
                    &[at.clone(), ("segments", int(12)), ("rings", int(6))],
                )
                .unwrap(),
            ),
            (
                "cylinder",
                run(
                    &CylinderProcessor,
                    &[
                        at.clone(),
                        ("segments", int(12)),
                        ("height_segments", int(3)),
                    ],
                )
                .unwrap(),
            ),
            (
                "plane",
                run(
                    &PlaneProcessor,
                    &[at, ("segments_x", int(2)), ("segments_y", int(2))],
                )
                .unwrap(),
            ),
        ]
    }

    /// `N` is a unit vector at every vertex, and every triangle is wound the
    /// way its normals point (the geometric normal agrees in sign), for all
    /// four primitives with an off-origin centre.
    #[test]
    fn normals_are_unit_and_triangles_are_wound_along_them() {
        for (name, g) in every_primitive([30.0, -20.0, 10.0]) {
            for n in normals(&g) {
                assert!(
                    (length(n) - 1.0).abs() < 1e-5,
                    "{name}: |N| = {}",
                    length(n)
                );
            }
            let n = normals(&g);
            for t in triangles(&g) {
                let hint = add(add(n[t[0]], n[t[1]]), n[t[2]]);
                assert!(
                    dot(face_normal(&g, t), hint) > 0.0,
                    "{name}: triangle {t:?} is wound against its normals"
                );
            }
        }
    }

    /// Outward means away from the solid: from the centre for the convex
    /// bodies, and from the axis (or along ±Y on a cap) for the cylinder.
    #[test]
    fn normals_point_out_of_the_solid() {
        let centre = Vec3(30.0, -20.0, 10.0);
        let all = every_primitive([centre.0, centre.1, centre.2]);
        for (name, g) in &all {
            let (p, n) = (positions(g), normals(g));
            for (point, normal) in p.iter().zip(&n) {
                let r = sub(*point, centre);
                match *name {
                    "box" => assert!(dot(*normal, r) > 0.0, "box: {point:?} / {normal:?}"),
                    "sphere" => {
                        assert!((dot(*normal, r) - length(r)).abs() < 1e-3, "sphere");
                    }
                    "cylinder" if normal.1 == 0.0 => {
                        assert!(dot(*normal, Vec3(r.0, 0.0, r.2)) > 49.0, "cylinder side");
                    }
                    // A cap normal runs along the axis, away from the middle.
                    "cylinder" => assert!(normal.1 * r.1 > 0.0, "cylinder cap"),
                    "plane" => assert_eq!(*normal, Vec3(0.0, 0.0, -1.0)),
                    _ => unreachable!(),
                }
            }
        }
    }

    /// The poles are defined, not accidental: no triangle collapses, each pole
    /// is exactly on the Y axis with a normal exactly along it, and a pole
    /// takes one triangle per segment, with `v` pinned to 0 (up, −Y) and 1.
    #[test]
    fn a_spheres_poles_are_a_clean_fan_with_no_degenerate_triangle() {
        let (segments, rings, radius) = (10usize, 5usize, 40.0);
        let g = run(
            &SphereProcessor,
            &[
                ("segments", int(segments as i32)),
                ("rings", int(rings as i32)),
                ("radius", ResolvedValue::Float(radius)),
            ],
        )
        .unwrap();
        let (p, n, uv) = (positions(&g), normals(&g), uvs(&g));
        for t in triangles(&g) {
            assert!(
                length(face_normal(&g, t)) > 1e-3,
                "degenerate triangle {t:?}"
            );
        }
        let poles = |sign: f32| -> Vec<usize> {
            (0..p.len()).filter(|&i| p[i].1 == sign * radius).collect()
        };
        for (sign, v) in [(-1.0, 0.0), (1.0, 1.0)] {
            let verts = poles(sign);
            assert_eq!(verts.len(), segments, "one vertex per fan triangle");
            for &i in &verts {
                assert_eq!((p[i].0, p[i].2), (0.0, 0.0), "pole is on the axis");
                assert_eq!(n[i], Vec3(0.0, sign, 0.0), "pole normal is the axis");
                assert_eq!(uv[i].1, v);
            }
            let fan = triangles(&g)
                .into_iter()
                .filter(|t| t.iter().any(|i| verts.contains(i)))
                .count();
            assert_eq!(fan, segments, "one triangle per segment at the pole");
        }
        // The first pole is up in a +Y-down space, where v = 0 is the top.
        assert!(poles(-1.0).iter().all(|&i| uv[i].1 == 0.0));
    }

    /// `uv` is `(0, 0)` top-left with `v` growing downwards (+Y), the way the
    /// renderer's sampler and its unit quad read it.
    #[test]
    fn uv_has_its_origin_top_left_and_v_grows_downwards() {
        let plane = run(
            &PlaneProcessor,
            &[("size", ResolvedValue::Vec2([20.0, 10.0]))],
        )
        .unwrap();
        // The renderer's unit quad scaled to the size: uv (s, t) sits at
        // (-10 + 20 s, -5 + 10 t), whatever order the vertices come in.
        for (point, uv) in positions(&plane).into_iter().zip(uvs(&plane)) {
            assert_eq!(point, Vec3(-10.0 + 20.0 * uv.0, -5.0 + 10.0 * uv.1, 0.0));
        }
        assert_eq!(plane.point_count(), 4);

        let cylinder = run(&CylinderProcessor, &[("segments", int(8))]).unwrap();
        let (p, uv) = (positions(&cylinder), uvs(&cylinder));
        // Side vertices (the first rows): v = 0 is the top (−Y), v = 1 the bottom.
        for i in 0..2 * 9 {
            assert_eq!(uv[i].1, if p[i].1 < 0.0 { 0.0 } else { 1.0 });
        }
        // u runs once around: the seam vertex repeats the first with u = 1.
        assert_eq!((uv[0].0, uv[8].0), (0.0, 1.0));
        assert_eq!(p[0], Vec3(50.0, p[0].1, 0.0));
    }

    /// Each box face reads unmirrored from outside: on the face a camera at −Z
    /// sees, `u` grows with +X and `v` with +Y; on the back face `u` grows
    /// with −X.
    #[test]
    fn box_faces_are_unmirrored_when_seen_from_outside() {
        let g = run(&BoxProcessor, &[]).unwrap();
        let (p, uv) = (positions(&g), uvs(&g));
        // Faces are emitted front, back, +X, −X, top, bottom, four vertices each.
        let corner = |face: usize, u: f32, v: f32| {
            (0..4)
                .map(|k| face * 4 + k)
                .find(|&i| uv[i] == Vec2(u, v))
                .map(|i| p[i])
                .unwrap()
        };
        assert_eq!(corner(0, 0.0, 0.0), Vec3(-50.0, -50.0, -50.0));
        assert_eq!(corner(0, 1.0, 1.0), Vec3(50.0, 50.0, -50.0));
        assert_eq!(corner(1, 0.0, 0.0), Vec3(50.0, -50.0, 50.0));
        assert_eq!(corner(2, 0.0, 0.0), Vec3(50.0, -50.0, -50.0));
        assert_eq!(corner(3, 0.0, 0.0), Vec3(-50.0, -50.0, 50.0));
    }

    #[test]
    fn an_oversized_division_is_an_evaluation_error() {
        let error = run(
            &BoxProcessor,
            &[
                ("segments_x", int(1000)),
                ("segments_y", int(1000)),
                ("segments_z", int(1000)),
            ],
        )
        .expect_err("a billion-vertex box is refused");
        assert!(error.to_string().contains("mesh.box"), "{error}");
    }

    /// A degenerate input still evaluates: counts clamp up to the minimum a
    /// closed mesh needs rather than yielding an empty or invalid geometry.
    #[test]
    fn division_counts_clamp_to_the_smallest_valid_mesh() {
        let sphere = run(&SphereProcessor, &[("segments", int(0)), ("rings", int(0))]).unwrap();
        assert_counts(&sphere, 2 * 3 + 4, 6);
        let cylinder = run(
            &CylinderProcessor,
            &[("segments", int(-4)), ("height_segments", int(0))],
        )
        .unwrap();
        assert_counts(&cylinder, 4 * 2 + 2 * 4, 3 * 2 + 2 * 3);
    }

    /// `field.apply` modulates a primitive like any geometry: the whole chain
    /// goes through the evaluator with the real processors and templates.
    #[test]
    fn a_field_modulates_a_primitive_per_point() {
        use crate::field::{ApplyFieldProcessor, AttributeFieldProcessor};

        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        let make =
            |id: u64, type_key: &str| registry.get(type_key).unwrap().create_node(NodeId::new(id));
        let sphere = make(1, "mesh.sphere");
        let mut field = make(2, "field.attribute");
        let mut apply = make(3, "field.apply");
        let set = |node: &mut Node, key: &str, value: &str| {
            let slot = node.parameters.iter_mut().find(|p| p.key == key).unwrap();
            slot.value = ParameterValue::String(value.into());
        };
        set(&mut field, "name", "P");
        set(&mut field, "component", "y");
        set(&mut apply, "domain", "point");
        set(&mut apply, "target", names::PSCALE);
        set(&mut apply, "combine", "set");
        let graph = ravel_core::graph::Graph::new()
            .add_node(sphere.clone())
            .unwrap()
            .add_node(field.clone())
            .unwrap()
            .add_node(apply.clone())
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                sphere.id,
                OutputPortIndex(0),
                apply.id,
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                field.id,
                OutputPortIndex(0),
                apply.id,
                InputPortIndex(1),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(sphere.id, Arc::new(SphereProcessor));
        ev.register(
            field.id,
            Arc::new(AttributeFieldProcessor::from_node(&field)),
        );
        ev.register(apply.id, Arc::new(ApplyFieldProcessor::from_node(&apply)));
        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (100, 100));
        let out = ev.evaluate(&graph, apply.id, &ctx).unwrap();
        let g = out.downcast_ref::<Geometry>().unwrap();

        let p = positions(g);
        let pscale = g
            .points()
            .get(names::PSCALE)
            .unwrap()
            .as_f32(names::PSCALE)
            .unwrap();
        assert_eq!(pscale.len(), p.len());
        assert!(pscale.iter().any(|&v| v < -40.0) && pscale.iter().any(|&v| v > 40.0));
        for (value, point) in pscale.iter().zip(&p) {
            assert!((value - point.1).abs() < 1e-4, "{value} vs {point:?}");
        }
        // The mesh itself is untouched by the modulation.
        assert_eq!(g.indices().len(), 2 * 32 * 15 * 3);
        assert!(g.has_mesh());
    }

    /// The declared transform section reaches the primitive's normals: turned a
    /// quarter about Z, the +X face (third face of a box, points 8..12)
    /// faces +Y, and a mirror keeps the faces outward.
    #[test]
    fn the_transform_section_carries_the_normals() {
        let node = Node::new(NodeId::new(1), "mesh.box");
        let wrapped = crate::transform_section::wrap(&node, Arc::new(BoxProcessor));
        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (100, 100));
        let run_with = |extra: &[(&str, ResolvedValue)]| {
            let mut params = ResolvedParams::default();
            for (key, value) in extra {
                params.set(key, value.clone());
            }
            let out = wrapped
                .process(&node, &ctx, &[], &params, &mut Evaluator::new())
                .unwrap();
            out.downcast_ref::<Geometry>().unwrap().clone()
        };
        let turned = run_with(&[("rotation", ResolvedValue::Vec3([0.0, 0.0, 90.0]))]);
        let n = normals(&turned);
        for (got, want) in [(n[8], Vec3(0.0, 1.0, 0.0)), (n[0], Vec3(0.0, 0.0, -1.0))] {
            assert!(length(sub(got, want)) < 1e-4, "{got:?} != {want:?}");
        }

        // Mirrored in x: every triangle is still wound along its normals, and
        // every normal still points away from the middle.
        let mirrored = run_with(&[("scale", ResolvedValue::Vec3([-1.0, 1.0, 1.0]))]);
        let (p, n) = (positions(&mirrored), normals(&mirrored));
        for t in triangles(&mirrored) {
            let hint = add(add(n[t[0]], n[t[1]]), n[t[2]]);
            assert!(dot(face_normal(&mirrored, t), hint) > 0.0);
        }
        for (point, normal) in p.iter().zip(&n) {
            assert!(dot(*point, *normal) > 0.0);
        }
    }
}
