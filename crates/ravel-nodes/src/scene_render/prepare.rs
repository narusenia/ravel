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

/// Project the path content of `flat` (its `P` in object space) to pixels.
///
/// `None` when there is no path content, or when the object's centroid is
/// outside the clip volume (behind the camera, nearer than `near`, or past
/// `far`) — such an object has no depth to be drawn at.
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
    let Some(positions) = flat.positions(Domain::Point) else {
        return Ok(None);
    };
    let positions: Positions<'_> = positions.context("scene.render: the point positions")?;
    let count = positions.len();
    // Points covered by a mesh are its vertices, not sprites or path points.
    let mut covered = vec![false; count];
    for prim in flat.primitives() {
        if prim.is_mesh() {
            for slot in covered
                .iter_mut()
                .take(prim.verts().end)
                .skip(prim.verts().start)
            {
                *slot = true;
            }
        }
    }
    let has_path = flat.primitives().iter().any(|prim| !prim.is_mesh());
    if !has_path && covered.iter().all(|&c| c) {
        return Ok(None);
    }

    let mut extent = Extent::new();
    for (index, is_covered) in covered.iter().enumerate() {
        if !is_covered && let Some(p) = positions.get3(index) {
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
