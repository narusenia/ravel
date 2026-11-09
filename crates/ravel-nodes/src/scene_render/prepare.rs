// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! From a [`Scene`] to a draw list: the part of `scene.render` the CPU
//! reference and the GPU renderer share.
//!
//! Nothing here draws. [`prepare`] flattens the scene, splits every object into
//! the pieces the two passes want, and orders them:
//!
//! | Piece | Becomes | Pass |
//! |---|---|---|
//! | A `Primitive::Mesh`, fully opaque | [`MeshObject`] | 1: depth write |
//! | A `Primitive::Mesh` with any alpha below 1 | [`MeshObject`] | 2: sorted |
//! | An image instance | [`MeshObject`] over the shared unit quad, textured | 2: sorted |
//! | Paths and point sprites | [`PathObject`], already projected to pixels | 2: sorted |
//!
//! # The sort key
//!
//! Pass 2 is ordered back to front by the **view-space `z` of the object's
//! centroid** — the mean of its (transformed) vertices — with the farthest
//! corner of its bounding box as the tie-break, and insertion order after
//! that so the result is deterministic. View-space `z` is the distance along
//! the view direction ([`Camera::view_matrix`]), so a larger value is farther
//! away and sorts first.
//!
//! The key is one number per **object**, which is why translucent objects that
//! interleave in depth are not resolved: whichever centroid is farther is drawn
//! first, whole (`two_crossing_translucent_quads_are_drawn_whole_in_centroid_order`
//! pins that this is the behaviour, so an improvement shows up as a changed
//! test). Against opaque meshes the depth test is per fragment and exact.

use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use ravel_core::eval::EvalContext;
use ravel_core::geometry::absent::DEFAULT_COLOR;
use ravel_core::geometry::{
    AttributeArray, Domain, Geometry, InstanceTransform, Positions, Primitive, names,
};
use ravel_core::scene::camera::aspect_ratio;
use ravel_core::scene::{Camera, Mat4, Scene};
use ravel_core::types::{Color, NodeData, Vec2, Vec3};
use ravel_gpu::MeshVertex;

use crate::rasterize::{self, PathPaint};

/// Identity of a mesh's static data: what a GPU buffer built from it is a
/// function of.
///
/// Column identities are the addresses of the `Arc`s the geometry holds, which
/// copy-on-write keeps stable for as long as nobody edits the column (an edit
/// while another holder exists clones the column to a new address). The buffer
/// cache keeps the geometry alive next to the key, so an address cannot be
/// recycled for a different column while an entry still names it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct MeshKey {
    /// `P`, `N`, `uv`, `Cd`, `alpha` — `0` for an absent column.
    columns: [usize; 5],
    /// Address and length of the geometry's shared index blob.
    indices: (usize, usize),
    /// The mesh's point range.
    verts: (usize, usize),
    /// The mesh's range into the index blob.
    index_range: (usize, usize),
}

/// One mesh to draw: a `Primitive::Mesh` of a geometry, placed.
pub(super) struct MeshObject {
    /// Keeps the columns alive and is what [`mesh_data`] reads.
    pub geometry: Arc<Geometry>,
    pub primitive: usize,
    pub key: MeshKey,
    /// Object space → clip space.
    pub mvp: Mat4,
    /// Object space → scene space.
    pub model: Mat4,
    /// Straight-alpha tint multiplied into every fragment.
    pub color: Color,
    /// The frame the fragment stage samples through the mesh's `uv`, if any.
    pub texture: Option<Arc<dyn NodeData>>,
    pub depth: ViewDepth,
}

/// One Path object, projected to output pixels.
pub(super) struct PathObject {
    /// Planar geometry in pixel space: paths and point sprites only.
    pub geometry: Geometry,
    /// Clip-space depth of the object's centroid.
    pub ndc_depth: f32,
    pub depth: ViewDepth,
}

/// An entry of pass 2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Blended {
    Mesh(usize),
    Path(usize),
}

/// The depth an object is sorted by. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ViewDepth {
    /// View-space `z` of the object's centroid.
    pub centroid: f32,
    /// The largest view-space `z` of the object's bounding box.
    pub farthest: f32,
}

pub(super) struct Prepared {
    pub resolution: (u32, u32),
    pub meshes: Vec<MeshObject>,
    /// Indices into `meshes`, in draw order.
    pub opaque: Vec<usize>,
    /// Pass 2, back to front.
    pub blended: Vec<Blended>,
    pub paths: Vec<PathObject>,
    pub paint: PathPaint,
}

pub(super) struct SceneParams {
    pub fill: bool,
    pub stroke_width: f32,
    pub color: Color,
}

/// The unit quad every image instance is drawn on: `±0.5` in x and y, facing
/// the camera, with the corner UVs `(0, 0)` … `(1, 1)`.
///
/// One shared geometry rather than one per image size, so its static GPU
/// buffers are built once; an image's size is the model matrix's scale. Its
/// `uv` is an ordinary column the sampler reads like any other mesh's — the
/// rectangle has no implicit UVs of its own.
fn unit_quad() -> Arc<Geometry> {
    static QUAD: OnceLock<Arc<Geometry>> = OnceLock::new();
    QUAD.get_or_init(|| {
        let mut geometry = Geometry::from_points3(vec![
            Vec3(-0.5, -0.5, 0.0),
            Vec3(0.5, -0.5, 0.0),
            Vec3(0.5, 0.5, 0.0),
            Vec3(-0.5, 0.5, 0.0),
        ]);
        let points = geometry.points_mut();
        points
            .insert(
                names::UV,
                AttributeArray::Vec2(vec![
                    Vec2(0.0, 0.0),
                    Vec2(1.0, 0.0),
                    Vec2(1.0, 1.0),
                    Vec2(0.0, 1.0),
                ]),
            )
            .expect("uv matches P");
        // Facing the camera: the scene looks along +Z.
        points
            .insert(
                names::N,
                AttributeArray::Vec3(vec![Vec3(0.0, 0.0, -1.0); 4]),
            )
            .expect("N matches P");
        geometry.push_mesh(0..4, &[0, 1, 2, 0, 2, 3]);
        Arc::new(geometry)
    })
    .clone()
}

fn column_address(geometry: &Geometry, name: &str) -> usize {
    geometry
        .points()
        .get(name)
        .map_or(0, |column| Arc::as_ptr(column) as usize)
}

fn mesh_key(
    geometry: &Geometry,
    verts: &std::ops::Range<usize>,
    indices: &std::ops::Range<usize>,
) -> MeshKey {
    let blob = geometry.indices();
    MeshKey {
        columns: [
            column_address(geometry, names::P),
            column_address(geometry, names::N),
            column_address(geometry, names::UV),
            column_address(geometry, names::CD),
            column_address(geometry, names::ALPHA),
        ],
        indices: (blob.as_ptr() as usize, blob.len()),
        verts: (verts.start, verts.end),
        index_range: (indices.start, indices.end),
    }
}

/// A mesh's static data in the layout both renderers consume.
pub(super) struct MeshData {
    pub vertices: Vec<MeshVertex>,
    /// One straight-alpha colour per vertex, or empty when the mesh carries
    /// neither `Cd` nor `alpha`.
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

/// Read mesh primitive `primitive` of `geometry` into [`MeshData`].
///
/// A triangle with an index past the end of its vertex run is dropped rather
/// than drawn: `Geometry::validate` rejects it, but a renderer that indexes
/// out of bounds takes the process down on the CPU and reads garbage on the
/// GPU, so it does not rely on the caller having validated.
pub(super) fn mesh_data(geometry: &Geometry, primitive: usize) -> Option<MeshData> {
    let prim = geometry.primitives().get(primitive)?;
    let Primitive::Mesh { verts, .. } = prim else {
        return None;
    };
    let relative = geometry.mesh_indices(prim)?;
    let positions = geometry.positions(Domain::Point)?.ok()?;
    if verts.end > positions.len() {
        return None;
    }
    let count = verts.len();
    let points = geometry.points();
    let column = |name: &str| points.get(name).map(|column| column.as_ref());
    let normals = column(names::N).and_then(|c| c.as_vec3(names::N).ok());
    let uvs = column(names::UV).and_then(|c| c.as_vec2(names::UV).ok());
    let cd = column(names::CD).and_then(|c| c.as_color(names::CD).ok());
    let alpha = column(names::ALPHA).and_then(|c| c.as_f32(names::ALPHA).ok());

    let vertices = (verts.start..verts.end)
        .map(|index| {
            let position = positions.get3(index).unwrap_or(Vec3(0.0, 0.0, 0.0));
            let normal = normals
                .and_then(|n| n.get(index))
                .copied()
                .unwrap_or(Vec3(0.0, 0.0, -1.0));
            let uv = uvs
                .and_then(|u| u.get(index))
                .copied()
                .unwrap_or(Vec2(0.0, 0.0));
            MeshVertex {
                position: [position.0, position.1, position.2],
                normal: [normal.0, normal.1, normal.2],
                uv: [uv.0, uv.1],
            }
        })
        .collect();
    let colors = if cd.is_some() || alpha.is_some() {
        (verts.start..verts.end)
            .map(|index| {
                let base = cd
                    .and_then(|c| c.get(index))
                    .copied()
                    .unwrap_or(DEFAULT_COLOR);
                let a = alpha.and_then(|a| a.get(index)).copied().unwrap_or(1.0);
                [base.r, base.g, base.b, base.a * a]
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut indices = Vec::with_capacity(relative.len());
    for triangle in relative.chunks_exact(3) {
        if triangle.iter().all(|&index| (index as usize) < count) {
            indices.extend_from_slice(triangle);
        }
    }
    Some(MeshData {
        vertices,
        colors,
        indices,
    })
}

fn point_mul(m: &Mat4, p: [f32; 3]) -> [f32; 4] {
    m.transform_vec4([p[0], p[1], p[2], 1.0])
}

/// Object-space bounding box and centroid of `points`.
struct Extent {
    min: [f32; 3],
    max: [f32; 3],
    sum: [f64; 3],
    count: usize,
}

impl Extent {
    fn new() -> Self {
        Self {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
            sum: [0.0; 3],
            count: 0,
        }
    }

    fn add(&mut self, p: [f32; 3]) {
        for (axis, value) in p.into_iter().enumerate() {
            self.min[axis] = self.min[axis].min(value);
            self.max[axis] = self.max[axis].max(value);
            self.sum[axis] += f64::from(value);
        }
        self.count += 1;
    }

    fn centroid(&self) -> [f32; 3] {
        let n = self.count.max(1) as f64;
        [
            (self.sum[0] / n) as f32,
            (self.sum[1] / n) as f32,
            (self.sum[2] / n) as f32,
        ]
    }

    /// The sort key under `world` then `view`.
    fn view_depth(&self, world: &Mat4, view: &Mat4) -> ViewDepth {
        let to_view = view.mul(world);
        let centroid = point_mul(&to_view, self.centroid())[2];
        let mut farthest = f32::NEG_INFINITY;
        for corner in 0..8 {
            let p = [
                if corner & 1 == 0 {
                    self.min[0]
                } else {
                    self.max[0]
                },
                if corner & 2 == 0 {
                    self.min[1]
                } else {
                    self.max[1]
                },
                if corner & 4 == 0 {
                    self.min[2]
                } else {
                    self.max[2]
                },
            ];
            farthest = farthest.max(point_mul(&to_view, p)[2]);
        }
        ViewDepth { centroid, farthest }
    }
}

fn extent_of_mesh(geometry: &Geometry, verts: &std::ops::Range<usize>) -> Extent {
    let mut extent = Extent::new();
    if let Some(Ok(positions)) = geometry.positions(Domain::Point) {
        for index in verts.clone() {
            if let Some(p) = positions.get3(index) {
                extent.add([p.0, p.1, p.2]);
            }
        }
    }
    extent
}

/// The 2D placement of an image instance as a 3D matrix (rows, z untouched).
fn instance_matrix(transform: &InstanceTransform) -> Mat4 {
    let x = transform.apply_vector(Vec2(1.0, 0.0));
    let y = transform.apply_vector(Vec2(0.0, 1.0));
    Mat4::from_rows([
        [x.0, y.0, 0.0, transform.offset.0],
        [x.1, y.1, 0.0, transform.offset.1],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

/// Whether a mesh is drawn translucent: its tint or any vertex has alpha < 1.
fn is_translucent(tint: Color, data: &MeshData) -> bool {
    tint.a < 1.0 || data.colors.iter().any(|color| color[3] < 1.0)
}

struct Projector {
    view: Mat4,
    view_proj: Mat4,
    resolution: (u32, u32),
    /// The camera's `x` axis in scene space, for measuring pixels per unit.
    right: [f32; 3],
}

impl Projector {
    /// Scene-space point → `(pixel x, pixel y, clip depth, clip w)`.
    fn project(&self, world: &Mat4, p: [f32; 3]) -> [f32; 4] {
        let clip = point_mul(&self.view_proj.mul(world), p);
        let w = if clip[3].abs() < 1e-6 {
            1e-6_f32.copysign(clip[3])
        } else {
            clip[3]
        };
        let (width, height) = (self.resolution.0 as f32, self.resolution.1 as f32);
        [
            (clip[0] / w * 0.5 + 0.5) * width,
            (0.5 - clip[1] / w * 0.5) * height,
            clip[2] / w,
            clip[3],
        ]
    }
}

/// One row of a clipped point domain: an original point, or the point a
/// fraction `t` of the way from one original point to another.
#[derive(Clone, Copy, Debug)]
enum Row {
    Original(usize),
    Between(usize, usize, f32),
}

/// Whether a point is on the visible side of the near plane: clip-space
/// `z >= 0`. The clip-space `z` is affine in the object-space position, so a
/// crossing found here is found at the same `t` in object space.
fn in_front(depth: f32) -> bool {
    depth >= 0.0
}

/// Where the segment from a point at clip depth `from` to one at `to` crosses
/// the near plane.
fn crossing(from: f32, to: f32) -> f32 {
    from / (from - to)
}

/// Clip one path against the near plane, as rows of the point domain.
///
/// - **Fully in front**: unchanged, and still closed if it was.
/// - **Fully behind**: nothing.
/// - **Open and crossing**: the visible part, split into one open polyline per
///   run in front (a path that dips behind the camera and returns becomes two).
/// - **Closed and crossing**: the polygon clipped to the half-space, which is
///   again one closed polygon whose new edge lies on the near plane — fill is
///   exact, and a stroke also runs along that edge (the cut is not a contour of
///   the shape, but a closed path has no loose end to leave open).
///
/// The shared rule for the CPU reference and the GPU renderer: both read the
/// geometry this produces, so neither ever projects a point from behind the
/// camera (whose perspective divide inverts and explodes).
fn clip_path(
    verts: std::ops::Range<usize>,
    closed: bool,
    depth: impl Fn(usize) -> f32,
) -> Vec<(Vec<Row>, bool)> {
    let ring: Vec<usize> = verts.collect();
    if ring.iter().all(|&i| in_front(depth(i))) {
        return vec![(ring.into_iter().map(Row::Original).collect(), closed)];
    }
    if ring.iter().all(|&i| !in_front(depth(i))) {
        return Vec::new();
    }
    if closed {
        let mut polygon = Vec::new();
        for (position, &a) in ring.iter().enumerate() {
            let b = ring[(position + 1) % ring.len()];
            let (da, db) = (depth(a), depth(b));
            if in_front(da) {
                polygon.push(Row::Original(a));
            }
            if in_front(da) != in_front(db) {
                polygon.push(Row::Between(a, b, crossing(da, db)));
            }
        }
        return if polygon.len() >= 3 {
            vec![(polygon, true)]
        } else {
            Vec::new()
        };
    }
    let mut pieces = Vec::new();
    let mut current: Vec<Row> = Vec::new();
    for (position, &index) in ring.iter().enumerate() {
        let visible = in_front(depth(index));
        let previous = position.checked_sub(1).map(|p| ring[p]);
        if let Some(previous) = previous {
            let (dp, di) = (depth(previous), depth(index));
            if in_front(dp) != visible {
                current.push(Row::Between(previous, index, crossing(dp, di)));
                if !visible {
                    pieces.push(std::mem::take(&mut current));
                }
            }
        }
        if visible {
            current.push(Row::Original(index));
        }
    }
    pieces.push(current);
    pieces
        .into_iter()
        .filter(|piece| piece.len() >= 2)
        .map(|piece| (piece, false))
        .collect()
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// The column `column` read at `rows`. A row between two points interpolates
/// the numeric types; a type with no interpolation (`I32`, `Bool`, `Str`)
/// takes the first point's value, and a tangent (an offset, meaningless at a
/// point that was never on the curve) is zero there.
fn gather(name: &str, column: &AttributeArray, rows: &[Row]) -> AttributeArray {
    let tangent = name == names::IN_TAN || name == names::OUT_TAN;
    macro_rules! each {
        ($values:expr, $original:expr, $between:expr) => {
            rows.iter()
                .map(|row| match *row {
                    Row::Original(i) => $original(&$values[i]),
                    Row::Between(i, j, t) => $between(&$values[i], &$values[j], t),
                })
                .collect()
        };
    }
    match column {
        AttributeArray::F32(v) => {
            AttributeArray::F32(each!(v, |x: &f32| *x, |a: &f32, b: &f32, t| mix(*a, *b, t)))
        }
        AttributeArray::Vec2(v) => AttributeArray::Vec2(each!(
            v,
            |x: &Vec2| *x,
            |a: &Vec2, b: &Vec2, t| if tangent {
                Vec2(0.0, 0.0)
            } else {
                Vec2(mix(a.0, b.0, t), mix(a.1, b.1, t))
            }
        )),
        AttributeArray::Vec3(v) => AttributeArray::Vec3(each!(
            v,
            |x: &Vec3| *x,
            |a: &Vec3, b: &Vec3, t| Vec3(mix(a.0, b.0, t), mix(a.1, b.1, t), mix(a.2, b.2, t))
        )),
        AttributeArray::Vec4(v) => AttributeArray::Vec4(each!(
            v,
            |x: &ravel_core::types::Vec4| *x,
            |a: &ravel_core::types::Vec4, b: &ravel_core::types::Vec4, t| {
                ravel_core::types::Vec4(
                    mix(a.0, b.0, t),
                    mix(a.1, b.1, t),
                    mix(a.2, b.2, t),
                    mix(a.3, b.3, t),
                )
            }
        )),
        AttributeArray::Color(v) => {
            AttributeArray::Color(each!(v, |x: &Color| *x, |a: &Color, b: &Color, t| {
                Color::new(
                    mix(a.r, b.r, t),
                    mix(a.g, b.g, t),
                    mix(a.b, b.b, t),
                    mix(a.a, b.a, t),
                )
            }))
        }
        AttributeArray::I32(v) => {
            AttributeArray::I32(each!(v, |x: &i32| *x, |a: &i32, _: &i32, _| *a))
        }
        AttributeArray::Bool(v) => {
            AttributeArray::Bool(each!(v, |x: &bool| *x, |a: &bool, _: &bool, _| *a))
        }
        AttributeArray::Str(v) => AttributeArray::Str(each!(
            v,
            |x: &String| x.clone(),
            |a: &String, _: &String, _| a.clone()
        )),
    }
}

/// The path content of `flat` (paths and loose points) with every path clipped
/// to the near plane, as a fresh geometry with a `Vec3` `P` in object space.
///
/// Mesh vertices are left behind (the meshes are drawn as meshes). Each path
/// gets its own run of points, so the result carries no sharing between
/// primitives; primitive attributes follow the path they came from, and a path
/// that is split passes its row to each piece. A loose point behind the near
/// plane is dropped.
fn near_clipped_paths(flat: &Geometry, clip_depth: &Mat4) -> anyhow::Result<Option<Geometry>> {
    let Some(positions) = flat.positions(Domain::Point) else {
        return Ok(None);
    };
    let positions: Positions<'_> = positions.context("scene.render: the point positions")?;
    let count = positions.len();
    let depth = |index: usize| {
        let p = positions.get3(index).unwrap_or(Vec3(0.0, 0.0, 0.0));
        point_mul(clip_depth, [p.0, p.1, p.2])[2]
    };

    let mut rows: Vec<Row> = Vec::new();
    let mut primitives = Vec::new();
    let mut parents = Vec::new();
    let mut covered = vec![false; count];
    for (parent, prim) in flat.primitives().iter().enumerate() {
        for slot in covered
            .iter_mut()
            .take(prim.verts().end)
            .skip(prim.verts().start)
        {
            *slot = true;
        }
        let Primitive::Path { verts, closed } = prim else {
            continue;
        };
        if verts.end > count {
            continue;
        }
        for (piece, closed) in clip_path(verts.clone(), *closed, depth) {
            let start = rows.len();
            rows.extend(piece);
            primitives.push(Primitive::Path {
                verts: start..rows.len(),
                closed,
            });
            parents.push(parent);
        }
    }
    for (index, &is_covered) in covered.iter().enumerate() {
        if !is_covered && in_front(depth(index)) {
            rows.push(Row::Original(index));
        }
    }
    if rows.is_empty() {
        return Ok(None);
    }

    let mut out = Geometry::new();
    for (name, column) in flat.points().iter() {
        if name.as_str() == names::P {
            continue;
        }
        let kept = gather(name, column, &rows);
        out.points_mut().insert(name.clone(), kept)?;
    }
    let p: Vec<Vec3> = rows
        .iter()
        .map(|row| match *row {
            Row::Original(i) => positions.get3(i).unwrap_or(Vec3(0.0, 0.0, 0.0)),
            Row::Between(i, j, t) => {
                let (a, b) = (
                    positions.get3(i).unwrap_or(Vec3(0.0, 0.0, 0.0)),
                    positions.get3(j).unwrap_or(Vec3(0.0, 0.0, 0.0)),
                );
                Vec3(mix(a.0, b.0, t), mix(a.1, b.1, t), mix(a.2, b.2, t))
            }
        })
        .collect();
    out.points_mut().insert(names::P, AttributeArray::Vec3(p))?;
    for primitive in primitives {
        out.push_primitive(primitive);
    }
    if !parents.is_empty() {
        for (name, column) in flat.primitive_attrs().iter() {
            let rows: Vec<Row> = parents.iter().map(|&p| Row::Original(p)).collect();
            if column.len() >= flat.primitives().len() {
                out.primitive_attrs_mut()
                    .insert(name.clone(), gather(name, column, &rows))?;
            }
        }
    }
    *out.detail_mut() = flat.detail().clone();
    Ok(Some(out))
}

/// Project the path content of `flat` (its `P` in object space) to pixels.
///
/// `None` when there is no path content, or when what remains after clipping
/// to the near plane ([`near_clipped_paths`]) has a centroid outside the clip
/// volume (past `far`) — such an object has no depth to be drawn at.
///
/// **Shortcuts, both stated in `docs/specifications/procedural-geometry.md`:**
/// the control polygon and the tangent handles are projected point by point,
/// so a curve under strong perspective is the projection of its control
/// polygon rather than of the curve; and stroke width, sprite radius and
/// `pscale` are scaled by the pixels-per-unit measured **at the centroid**, so
/// an object that spans a large depth range strokes at one width.
fn project_paths(
    flat: &Geometry,
    world: &Mat4,
    projector: &Projector,
) -> anyhow::Result<Option<PathObject>> {
    let Some(flat) = near_clipped_paths(flat, &projector.view_proj.mul(world))? else {
        return Ok(None);
    };
    let flat = &flat;
    let positions = flat
        .positions(Domain::Point)
        .expect("the clipped geometry has P")?;
    let count = positions.len();

    let mut extent = Extent::new();
    for index in 0..count {
        if let Some(p) = positions.get3(index) {
            extent.add([p.0, p.1, p.2]);
        }
    }
    if extent.count == 0 {
        return Ok(None);
    }
    let centroid = extent.centroid();
    let at_centroid = projector.project(world, centroid);
    if !(0.0..=1.0).contains(&at_centroid[2]) || at_centroid[3] <= 0.0 {
        return Ok(None);
    }
    // Pixels per scene unit along the camera's right axis, at the centroid.
    let world_centroid = point_mul(world, centroid);
    let beside = [
        world_centroid[0] + projector.right[0],
        world_centroid[1] + projector.right[1],
        world_centroid[2] + projector.right[2],
    ];
    let a = projector.project(
        &Mat4::IDENTITY,
        [world_centroid[0], world_centroid[1], world_centroid[2]],
    );
    let b = projector.project(&Mat4::IDENTITY, beside);
    let scale = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();

    let mut projected = Vec::with_capacity(count);
    for index in 0..count {
        let p = positions.get3(index).unwrap_or(Vec3(0.0, 0.0, 0.0));
        let out = projector.project(world, [p.0, p.1, p.2]);
        projected.push(Vec2(out[0], out[1]));
    }
    let mut geometry = flat.clone();
    // Tangents are offsets: project the handle's end and subtract the point.
    for name in [names::IN_TAN, names::OUT_TAN] {
        let Some(column) = flat.points().get(name).and_then(|c| c.as_vec2(name).ok()) else {
            continue;
        };
        let handles = column
            .iter()
            .enumerate()
            .map(|(index, tan)| {
                let p = positions.get3(index).unwrap_or(Vec3(0.0, 0.0, 0.0));
                let end = projector.project(world, [p.0 + tan.0, p.1 + tan.1, p.2]);
                Vec2(end[0] - projected[index].0, end[1] - projected[index].1)
            })
            .collect();
        geometry
            .points_mut()
            .insert(name, AttributeArray::Vec2(handles))?;
    }
    if let Some(radii) = flat
        .points()
        .get(names::PSCALE)
        .and_then(|c| c.as_f32(names::PSCALE).ok())
    {
        let scaled = radii.iter().map(|r| r * scale).collect();
        geometry
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(scaled))?;
    }
    if let Some(widths) = flat
        .primitive_attrs()
        .get(names::STROKE_WIDTH)
        .and_then(|c| c.as_f32(names::STROKE_WIDTH).ok())
    {
        let scaled = widths.iter().map(|w| w * scale).collect();
        geometry
            .primitive_attrs_mut()
            .insert(names::STROKE_WIDTH, AttributeArray::F32(scaled))?;
    }
    geometry
        .points_mut()
        .insert(names::P, AttributeArray::Vec2(projected))?;
    // Images are the scene's meshes, not screen-space quads, and the walk that
    // draws path geometry must not find any to draw.
    geometry.set_sources(Vec::new());
    Ok(Some(PathObject {
        geometry,
        ndc_depth: at_centroid[2],
        depth: extent.view_depth(world, &projector.view),
    }))
}

/// Flatten `scene` into the draw list.
pub(super) fn prepare(
    scene: &Scene,
    camera: &Camera,
    ctx: &EvalContext,
    params: &SceneParams,
) -> anyhow::Result<Prepared> {
    let resolution = ctx.resolution;
    anyhow::ensure!(
        resolution.0 > 0 && resolution.1 > 0,
        "scene.render resolution must be non-zero"
    );
    let view = camera.view_matrix();
    let view_proj = camera.projection_matrix(aspect_ratio(ctx)).mul(&view);
    let projector = Projector {
        right: [view.element(0, 0), view.element(0, 1), view.element(0, 2)],
        view,
        view_proj,
        resolution,
    };
    let paint = PathPaint {
        fill: params.fill,
        stroke_width: params.stroke_width,
        color: params.color,
    };

    let mut prepared = Prepared {
        resolution,
        meshes: Vec::new(),
        opaque: Vec::new(),
        blended: Vec::new(),
        paths: Vec::new(),
        paint,
    };
    let quad = unit_quad();
    for object in scene.flatten() {
        let world = object.world_transform;
        let geometry = &object.geometry;

        let mut images = Vec::new();
        rasterize::collect_image_instances(
            geometry,
            0,
            rasterize::identity_placement(),
            &mut images,
        );
        for instance in images {
            let model = world
                .mul(&instance_matrix(&instance.transform))
                .mul(&Mat4::from_scale([
                    instance.image.width() as f32,
                    instance.image.height() as f32,
                    1.0,
                ]));
            let (verts, indices) = match &quad.primitives()[0] {
                Primitive::Mesh { verts, indices } => (verts.clone(), indices.clone()),
                Primitive::Path { .. } => unreachable!("the unit quad is a mesh"),
            };
            let extent = extent_of_mesh(&quad, &verts);
            prepared.meshes.push(MeshObject {
                key: mesh_key(&quad, &verts, &indices),
                geometry: Arc::clone(&quad),
                primitive: 0,
                mvp: view_proj.mul(&model),
                model,
                color: instance.tint,
                texture: Some(Arc::clone(instance.image.frame())),
                depth: extent.view_depth(&model, &view),
            });
            let index = prepared.meshes.len() - 1;
            prepared.blended.push(Blended::Mesh(index));
        }

        // Geometry sources become flat geometry first, so instanced paths and
        // meshes are ordinary primitives below. Image sources stay with the
        // walk above; the expansion drops them with a warning, which is
        // avoided by only expanding when there is a geometry source.
        let has_geometry_source = geometry.sources().iter().any(|s| s.geometry().is_some());
        let flat: Arc<Geometry> = if has_geometry_source {
            Arc::new(
                ravel_core::geometry::expand_instances(geometry)
                    .context("scene.render: expanding the instances of an object")?,
            )
        } else {
            Arc::clone(geometry)
        };

        for (primitive, prim) in flat.primitives().iter().enumerate() {
            let Primitive::Mesh { verts, indices } = prim else {
                continue;
            };
            let Some(data) = mesh_data(&flat, primitive) else {
                continue;
            };
            let has_cd = flat.points().get(names::CD).is_some();
            // Attribute > node colour, as for `rasterize`: a mesh with its own
            // `Cd` is not tinted by the node's colour.
            let color = if has_cd { DEFAULT_COLOR } else { params.color };
            let translucent = is_translucent(color, &data);
            let extent = extent_of_mesh(&flat, verts);
            let model = world;
            prepared.meshes.push(MeshObject {
                key: mesh_key(&flat, verts, indices),
                geometry: Arc::clone(&flat),
                primitive,
                mvp: view_proj.mul(&model),
                model,
                color,
                texture: None,
                depth: extent.view_depth(&model, &view),
            });
            let index = prepared.meshes.len() - 1;
            if translucent {
                prepared.blended.push(Blended::Mesh(index));
            } else {
                prepared.opaque.push(index);
            }
        }

        if let Some(path) = project_paths(&flat, &world, &projector)? {
            prepared.paths.push(path);
            prepared
                .blended
                .push(Blended::Path(prepared.paths.len() - 1));
        }
    }

    // Back to front; the sort is stable, so equal keys keep insertion order.
    let depth_of = |entry: &Blended| match *entry {
        Blended::Mesh(index) => prepared.meshes[index].depth,
        Blended::Path(index) => prepared.paths[index].depth,
    };
    let mut order = std::mem::take(&mut prepared.blended);
    order.sort_by(|a, b| {
        let (a, b) = (depth_of(a), depth_of(b));
        b.centroid
            .total_cmp(&a.centroid)
            .then(b.farthest.total_cmp(&a.farthest))
    });
    prepared.blended = order;
    Ok(prepared)
}

#[cfg(test)]
mod clip_tests {
    use super::*;

    /// Depths by index; `clip_path` over `0..depths.len()`.
    fn clip(depths: &[f32], closed: bool) -> Vec<(Vec<Row>, bool)> {
        clip_path(0..depths.len(), closed, |i| depths[i])
    }

    fn is_between(row: &Row, from: usize, to: usize, expected: f32) -> bool {
        matches!(*row, Row::Between(a, b, t) if a == from && b == to && (t - expected).abs() < 1e-6)
    }

    #[test]
    fn a_path_wholly_in_front_is_untouched_and_keeps_its_closure() {
        let out = clip(&[1.0, 2.0, 3.0], true);
        assert_eq!(out.len(), 1);
        assert!(out[0].1, "still closed");
        assert!(matches!(
            out[0].0.as_slice(),
            [Row::Original(0), Row::Original(1), Row::Original(2)]
        ));
    }

    #[test]
    fn a_path_wholly_behind_vanishes() {
        assert!(clip(&[-1.0, -2.0], false).is_empty());
        assert!(clip(&[-1.0, -2.0, -3.0], true).is_empty());
    }

    #[test]
    fn an_open_path_keeps_only_the_part_in_front_of_the_plane() {
        // Depth 1 at point 0, -3 at point 1: the plane is a quarter of the
        // way along (1 / (1 - -3)).
        let out = clip(&[1.0, -3.0], false);
        assert_eq!(out.len(), 1);
        assert!(!out[0].1);
        assert!(matches!(out[0].0[0], Row::Original(0)));
        assert!(is_between(&out[0].0[1], 0, 1, 0.25));
        // The mirror image enters instead of leaving: -3 then 1 crosses
        // three quarters of the way along.
        let out = clip(&[-3.0, 1.0], false);
        assert!(is_between(&out[0].0[0], 0, 1, 0.75));
        assert!(matches!(out[0].0[1], Row::Original(1)));
    }

    #[test]
    fn an_open_path_that_dips_behind_the_camera_and_returns_is_two_pieces() {
        let out = clip(&[2.0, -2.0, 2.0], false);
        assert_eq!(out.len(), 2, "one piece per run in front");
        assert!(
            out.iter()
                .all(|(piece, closed)| piece.len() == 2 && !closed)
        );
    }

    #[test]
    fn a_closed_path_crossing_the_plane_becomes_one_closed_polygon() {
        // A square with two corners behind: the clipped polygon is the two
        // front corners plus the two plane crossings, closed.
        let out = clip(&[1.0, 1.0, -1.0, -1.0], true);
        assert_eq!(out.len(), 1);
        let (polygon, closed) = &out[0];
        assert!(closed);
        assert_eq!(polygon.len(), 4);
        assert!(is_between(&polygon[2], 1, 2, 0.5));
        assert!(is_between(&polygon[3], 3, 0, 0.5));
    }
}
