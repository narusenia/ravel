// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The column-oriented `Geometry` container with four attribute domains.

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use super::attribute::{AttrName, AttributeArray, AttributeSet, AttributeType, GeometryError};
use super::names;
use crate::id::DataTypeId;
use crate::types::{GeometricData, NodeData, Rect, Transform2D, Vec2, Vec3};

/// A primitive built from a contiguous run of point indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Primitive {
    /// A polyline/path over `verts` into the point domain.
    Path { verts: Range<usize>, closed: bool },
    /// An indexed triangle mesh over `verts` into the point domain
    /// (REQ-3D-003). `indices` is a range into [`Geometry::indices`], read
    /// three at a time; each value is an offset **relative to `verts.start`**,
    /// so the run stays valid when the owning points move — `geometry.merge`
    /// shifts `verts` and appends the index blob without rewriting a value.
    Mesh {
        verts: Range<usize>,
        indices: Range<usize>,
    },
}

impl Primitive {
    /// The run of point indices this primitive is built from. Every variant
    /// owns one, so element operations that only relocate or bound-check
    /// points stay variant-agnostic.
    pub fn verts(&self) -> &Range<usize> {
        match self {
            Self::Path { verts, .. } | Self::Mesh { verts, .. } => verts,
        }
    }

    /// Whether this primitive is a mesh. Operations defined only for paths
    /// test this and raise [`GeometryError::RequiresPathPrimitives`].
    pub fn is_mesh(&self) -> bool {
        matches!(self, Self::Mesh { .. })
    }

    /// The same primitive relocated by `points` point positions and `indices`
    /// index-buffer positions, for concatenating geometries.
    ///
    /// Index *values* need no rewriting because they are relative to
    /// `verts.start`; only the two ranges move. That is the whole reason the
    /// relative encoding was chosen — `geometry.merge` appends both index
    /// blobs untouched instead of remapping every triangle.
    pub fn shifted(&self, points: usize, indices: usize) -> Self {
        match self {
            Self::Path { verts, closed } => Self::Path {
                verts: (verts.start + points)..(verts.end + points),
                closed: *closed,
            },
            Self::Mesh {
                verts,
                indices: run,
            } => Self::Mesh {
                verts: (verts.start + points)..(verts.end + points),
                indices: (run.start + indices)..(run.end + indices),
            },
        }
    }
}

/// The `P` column of one attribute domain, read at whichever dimension the
/// geometry carries: `Vec2` in 2D, `Vec3` in 3D (REQ-3D-003).
///
/// Every node that reads positions goes through this instead of
/// `as_vec2(names::P)`, so "handled / dimension-agnostic / explicit error" is
/// a choice each call site has to make rather than a panic or a silent empty
/// result. The classification is the position dimension table in
/// `docs/specifications/procedural-geometry.md`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Positions<'a> {
    D2(&'a [Vec2]),
    D3(&'a [Vec3]),
}

impl<'a> Positions<'a> {
    /// Reads a position column, rejecting every type that is not a position.
    pub fn from_column(column: &'a AttributeArray) -> Result<Self, GeometryError> {
        match column {
            AttributeArray::Vec2(values) => Ok(Self::D2(values)),
            AttributeArray::Vec3(values) => Ok(Self::D3(values)),
            other => Err(GeometryError::PositionTypeMismatch {
                name: names::P.into(),
                actual: other.attr_type(),
            }),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::D2(values) => values.len(),
            Self::D3(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn attr_type(&self) -> AttributeType {
        match self {
            Self::D2(_) => AttributeType::Vec2,
            Self::D3(_) => AttributeType::Vec3,
        }
    }

    /// Number of components, 2 or 3.
    pub fn dimension(&self) -> usize {
        match self {
            Self::D2(_) => 2,
            Self::D3(_) => 3,
        }
    }

    /// The borrowed 2D slice, or `None` for 3D positions. The zero-copy fast
    /// path every existing 2D consumer keeps taking.
    pub fn planar(&self) -> Option<&'a [Vec2]> {
        match self {
            Self::D2(values) => Some(values),
            Self::D3(_) => None,
        }
    }

    /// The 2D slice, or an error naming `operation` when the geometry is 3D.
    /// Used by operations whose definition is planar (arc length, the
    /// analytic rasterizer).
    pub fn require_planar(&self, operation: &'static str) -> Result<&'a [Vec2], GeometryError> {
        self.planar().ok_or_else(|| GeometryError::RequiresPlanarP {
            operation,
            name: names::P.into(),
            actual: self.attr_type(),
        })
    }

    /// The xy projection: borrowed in 2D, materialized in 3D. Only for
    /// consumers that are documented as planar-by-construction (field
    /// sampling); anything whose result would silently lose meaning must use
    /// [`Self::require_planar`] instead.
    pub fn projected(&self) -> Cow<'a, [Vec2]> {
        match self {
            Self::D2(values) => Cow::Borrowed(values),
            Self::D3(values) => Cow::Owned(values.iter().map(|v| Vec2(v.0, v.1)).collect()),
        }
    }

    /// Position `index` as a 3D vector; 2D positions read back with `z = 0`.
    pub fn get3(&self, index: usize) -> Option<Vec3> {
        match self {
            Self::D2(values) => values.get(index).map(|v| Vec3(v.0, v.1, 0.0)),
            Self::D3(values) => values.get(index).copied(),
        }
    }

    /// Every position as a 3D vector; 2D positions read back with `z = 0`.
    /// The extra zero term is exact in binary floating point, so a 2D input
    /// produces bit-identical arithmetic.
    pub fn iter3(&self) -> Box<dyn Iterator<Item = Vec3> + 'a> {
        match *self {
            Self::D2(values) => Box::new(values.iter().map(|v| Vec3(v.0, v.1, 0.0))),
            Self::D3(values) => Box::new(values.iter().copied()),
        }
    }
}

/// A frame buffer stamped by the instance domain, sized in composition units
/// by its own pixel resolution.
///
/// The frame is held as an `Arc<dyn NodeData>` rather than a concrete
/// `FrameBuffer` so a GPU-resident frame passes through without a readback:
/// both representations are tagged [`DataTypeId::FRAME_BUFFER`] and only the
/// crate that owns the GPU one can name it. The resolution is captured
/// alongside, which is what lets `ravel-core` size the rectangle without
/// knowing which representation it holds
/// (`docs/implementation/done/image-instancing-plan.md`, decisions 5 and 6).
#[derive(Clone)]
pub struct InstanceImage {
    frame: Arc<dyn NodeData>,
    width: u32,
    height: u32,
}

impl InstanceImage {
    /// Stamp `frame` as a rectangle of `width` × `height` composition units.
    ///
    /// The size is the image's own pixel resolution, so the rectangle keeps
    /// the source's aspect ratio by construction.
    pub fn new(frame: Arc<dyn NodeData>, width: u32, height: u32) -> Result<Self, GeometryError> {
        if frame.data_type_id() != DataTypeId::FRAME_BUFFER {
            return Err(GeometryError::NotAFrameBuffer {
                data_type: frame.data_type_id().raw(),
            });
        }
        if width == 0 || height == 0 {
            return Err(GeometryError::EmptyImage { width, height });
        }
        Ok(Self {
            frame,
            width,
            height,
        })
    }

    /// The frame buffer value, in whichever representation it arrived.
    pub fn frame(&self) -> &Arc<dyn NodeData> {
        &self.frame
    }

    /// Width of the source image in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height of the source image in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Aspect ratio (width / height) of the source image.
    pub fn aspect_ratio(&self) -> f32 {
        self.width as f32 / self.height as f32
    }

    /// The rectangle this image occupies in the instance's own space, centred
    /// on the origin and sized from the image resolution.
    pub fn rect(&self) -> Rect {
        let (width, height) = (self.width as f32, self.height as f32);
        Rect {
            x: -width * 0.5,
            y: -height * 0.5,
            width,
            height,
        }
    }
}

/// Hand-written because `Arc<dyn NodeData>` has no `Debug`; printing the
/// resolution and the residency is what a reader of a `Geometry` dump wants
/// anyway.
impl fmt::Debug for InstanceImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstanceImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("gpu_resident", &self.frame.is_gpu_resident())
            .finish()
    }
}

/// One source the instance domain can stamp: a geometry, or a frame buffer as
/// a textured rectangle.
///
/// Every operation that only *moves* sources around — `geometry.merge`,
/// point reordering, `geometry.blast` — handles this opaquely; only the
/// rasterizer looks inside.
#[derive(Clone, Debug)]
pub enum InstanceSource {
    Geometry(Arc<Geometry>),
    Image(InstanceImage),
}

impl InstanceSource {
    /// The geometry this source stamps, or `None` for an image.
    pub fn geometry(&self) -> Option<&Arc<Geometry>> {
        match self {
            Self::Geometry(geometry) => Some(geometry),
            Self::Image(_) => None,
        }
    }

    /// The image this source stamps, or `None` for a geometry.
    pub fn image(&self) -> Option<&InstanceImage> {
        match self {
            Self::Image(image) => Some(image),
            Self::Geometry(_) => None,
        }
    }

    /// Whether two sources are the *same* value, by pointer. What
    /// `geometry.merge` compares when it decides whether two instance domains
    /// agree on their sources, without looking at what the source holds.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Geometry(a), Self::Geometry(b)) => Arc::ptr_eq(a, b),
            (Self::Image(a), Self::Image(b)) => {
                Arc::ptr_eq(&a.frame, &b.frame) && a.width == b.width && a.height == b.height
            }
            _ => false,
        }
    }

    /// Whether this source holds GPU-resident memory, recursing through a
    /// nested geometry's own sources.
    fn is_gpu_resident(&self) -> bool {
        match self {
            Self::Geometry(geometry) => geometry.is_gpu_resident(),
            Self::Image(image) => image.frame.is_gpu_resident(),
        }
    }

    /// Approximate bytes this source holds.
    fn byte_size(&self) -> u64 {
        match self {
            Self::Geometry(geometry) => geometry.byte_size(),
            Self::Image(image) => image.frame.byte_size(),
        }
    }
}

/// Instance nesting guard: instances-of-instances beyond this depth are
/// skipped rather than recursed (the spec limits stateful/sim nesting
/// similarly).
///
/// One constant for the whole instance model: `rasterize` stops drawing at
/// this depth and [`ops::expand_instances`](super::ops::expand_instances)
/// stops flattening at it, so a nesting too deep to draw is also one too deep
/// to convert — the picture and the geometry agree about what exists.
pub const MAX_INSTANCE_DEPTH: u32 = 4;

/// The affine placement one instance applies to its source: scale, then
/// rotate, then translate.
///
/// The single definition of that composition, because two consumers have to
/// agree on it exactly. `rasterize` reads it per instance while it draws, and
/// [`ops::expand_instances`](super::ops::expand_instances) bakes it into the
/// points when an instance geometry is flattened into one geometry; a text
/// drawn as instances and the same text converted to paths have to land on
/// the same pixels.
///
/// The columns are [`names::P`], [`names::ROT`] and [`names::SCALE`], each
/// defaulting to the identity when the instance domain does not carry it.
/// Two-dimensional: the 3D placement (`orient` / `scale3`) is a later unit,
/// and a caller handed 3D positions raises
/// [`GeometryError::RequiresPlanarP`] rather than projecting them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InstanceTransform {
    /// Where the instance sits, in the coordinate space of the geometry that
    /// owns the instance domain.
    pub offset: Vec2,
    /// Turn in radians, applied after [`Self::scale`].
    pub rot: f32,
    /// Per-axis scale, applied before [`Self::rot`].
    pub scale: Vec2,
}

impl InstanceTransform {
    /// The placement that changes nothing.
    pub const IDENTITY: Self = Self {
        offset: Vec2(0.0, 0.0),
        rot: 0.0,
        scale: Vec2(1.0, 1.0),
    };

    /// A point placed: scaled about the source origin, turned, then moved.
    pub fn apply(&self, p: Vec2) -> Vec2 {
        let scaled = Vec2(p.0 * self.scale.0, p.1 * self.scale.1);
        let (sin, cos) = self.rot.sin_cos();
        Vec2(
            self.offset.0 + scaled.0 * cos - scaled.1 * sin,
            self.offset.1 + scaled.0 * sin + scaled.1 * cos,
        )
    }

    /// A *difference* placed: scaled and turned, but not moved.
    ///
    /// Bezier tangents ([`names::IN_TAN`] / [`names::OUT_TAN`]) are offsets
    /// from their own point rather than positions, so this is what carries a
    /// glyph's curves through an expansion. Translating them instead would
    /// pull every control point to the instance's origin.
    pub fn apply_vector(&self, v: Vec2) -> Vec2 {
        let scaled = Vec2(v.0 * self.scale.0, v.1 * self.scale.1);
        let (sin, cos) = self.rot.sin_cos();
        Vec2(
            scaled.0 * cos - scaled.1 * sin,
            scaled.0 * sin + scaled.1 * cos,
        )
    }

    /// `outer ∘ inner`: the placement of an instance nested inside another.
    ///
    /// **Not the exact composition of the two affine maps.** The result is
    /// again a scale-rotate-translate, so the turns add and the scales
    /// multiply per axis; the true composition of a non-uniform scale with
    /// two different turns is a shear, which this representation cannot
    /// hold. Exact whenever either scale is uniform or either turn is zero,
    /// which covers every instance geometry the built-in nodes produce (a
    /// `scatter` writes a uniform scale, `text.layout` writes no turn at
    /// all). This is the composition `rasterize` has always drawn; it is
    /// stated here rather than fixed so that expanding a nesting and drawing
    /// it stay the same picture.
    pub fn compose(outer: Self, inner: Self) -> Self {
        Self {
            offset: outer.apply(inner.offset),
            rot: outer.rot + inner.rot,
            scale: Vec2(outer.scale.0 * inner.scale.0, outer.scale.1 * inner.scale.1),
        }
    }

    /// The mean absolute scale, which is what a stroke width scales by: a
    /// stroke has one width, so a non-uniform scale has to collapse to one
    /// number somewhere.
    pub fn uniform_scale(&self) -> f32 {
        (self.scale.0.abs() + self.scale.1.abs()) * 0.5
    }
}

/// The attribute domain an operation targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    Point,
    Primitive,
    Instance,
    Detail,
}

/// Column-oriented geometry: points, primitives, instances, and detail
/// attributes. Structural sharing via [`AttributeSet`] keeps clones cheap and
/// undo-compatible.
#[derive(Clone, Debug, Default)]
pub struct Geometry {
    points: AttributeSet,
    primitives: Vec<Primitive>,
    primitive_attrs: AttributeSet,
    /// Triangle indices shared by every [`Primitive::Mesh`], each slice
    /// addressed by that primitive's `indices` range. Held behind an `Arc` for
    /// the same reason attribute columns are: a geometry that only edits
    /// points must not deep-copy the index blob (REQ-CORE-004).
    indices: Arc<Vec<u32>>,
    instances: AttributeSet,
    /// Sources stamped by the instance domain: geometries, images, or both.
    instance_sources: Vec<InstanceSource>,
    detail: AttributeSet,
}

impl Geometry {
    /// An empty geometry with no points, primitives, or instances.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a 2D point cloud carrying the required `P` and stable `index`
    /// standard attributes.
    pub fn from_points(positions: Vec<Vec2>) -> Self {
        Self::from_position_column(AttributeArray::Vec2(positions))
    }

    /// Builds a 3D point cloud (REQ-3D-003). The `P` column is `Vec3`; every
    /// other standard attribute is identical to [`Self::from_points`].
    pub fn from_points3(positions: Vec<Vec3>) -> Self {
        Self::from_position_column(AttributeArray::Vec3(positions))
    }

    fn from_position_column(positions: AttributeArray) -> Self {
        let index: Vec<i32> = (0..positions.len() as i32).collect();
        let mut points = AttributeSet::new();
        points
            .insert(names::P, positions)
            .expect("first column cannot mismatch");
        points
            .insert(names::INDEX, AttributeArray::I32(index))
            .expect("index column matches P length");
        Self {
            points,
            ..Self::default()
        }
    }

    /// The `P` column of `domain`, or `None` when the domain has no `P`.
    ///
    /// `P` may be `Vec2` or `Vec3` and the dimension is chosen per domain, so
    /// a 2D instance source placed by 3D instances is a valid geometry
    /// (REQ-3D-003).
    pub fn positions(&self, domain: Domain) -> Option<Result<Positions<'_>, GeometryError>> {
        self.attribute_set(domain)
            .get(names::P)
            .map(|column| Positions::from_column(column))
    }

    /// Validates cross-domain invariants. Called after construction or a
    /// batch of mutations, mirroring the "validate at construction" rule from
    /// the procedural geometry spec.
    pub fn validate(&self) -> Result<(), GeometryError> {
        // `P` is a position column in either dimension; the choice is made per
        // domain, and a column is homogeneous by construction, so nothing
        // else is needed to keep the dimension consistent inside a domain.
        for domain in [Domain::Point, Domain::Instance] {
            if let Some(positions) = self.positions(domain) {
                positions?;
            }
        }
        // Both position-carrying domains owe a `P`: an instance without one
        // has no placement, and every reader of the instance domain asks for
        // it. The point domain has always been checked here; the instance
        // domain was not, which let a geometry with placements but no `P`
        // through construction and fail later at the reader.
        if self.points.get(names::P).is_none() && self.point_count() > 0 {
            return Err(GeometryError::AttributeNotFound {
                name: names::P.into(),
            });
        }
        if self.instances.get(names::P).is_none() && self.instance_count() > 0 {
            return Err(GeometryError::AttributeNotFound {
                name: names::P.into(),
            });
        }

        let point_count = self.point_count();
        for prim in &self.primitives {
            let verts = prim.verts();
            if verts.end > point_count || verts.start > verts.end {
                return Err(GeometryError::LengthMismatch {
                    name: names::P.into(),
                    expected: point_count,
                    actual: verts.end,
                });
            }
            // A mesh owes the same bound check one level down: its `indices`
            // run has to sit inside the shared buffer, and every value in it
            // has to address a vertex of *this* primitive. Without the second
            // check a mesh could reach another primitive's points, which the
            // vertex-range check above would never catch.
            if let Primitive::Mesh { indices, .. } = prim {
                if indices.end > self.indices.len() || indices.start > indices.end {
                    return Err(GeometryError::LengthMismatch {
                        name: "mesh indices".into(),
                        expected: self.indices.len(),
                        actual: indices.end,
                    });
                }
                if indices.len() % 3 != 0 {
                    return Err(GeometryError::LengthMismatch {
                        name: "mesh indices".into(),
                        expected: indices.len().next_multiple_of(3),
                        actual: indices.len(),
                    });
                }
                let vert_count = verts.len();
                if let Some(&out_of_range) = self.indices[indices.clone()]
                    .iter()
                    .find(|&&index| index as usize >= vert_count)
                {
                    return Err(GeometryError::LengthMismatch {
                        name: "mesh indices".into(),
                        expected: vert_count,
                        actual: out_of_range as usize + 1,
                    });
                }
            }
        }

        if self.primitive_len() != self.primitives.len() && self.primitive_len() != 0 {
            return Err(GeometryError::LengthMismatch {
                name: "primitive attributes".into(),
                expected: self.primitives.len(),
                actual: self.primitive_len(),
            });
        }

        for (name, column) in self.detail.iter() {
            if column.len() != 1 {
                return Err(GeometryError::LengthMismatch {
                    name: name.clone(),
                    expected: 1,
                    actual: column.len(),
                });
            }
        }

        Ok(())
    }

    // ----- Domain access ----------------------------------------------------

    pub fn points(&self) -> &AttributeSet {
        &self.points
    }

    pub fn points_mut(&mut self) -> &mut AttributeSet {
        &mut self.points
    }

    pub fn primitives(&self) -> &[Primitive] {
        &self.primitives
    }

    pub fn push_primitive(&mut self, prim: Primitive) {
        self.primitives.push(prim);
    }

    /// Replaces the primitive list wholesale. For the element operations that
    /// reorder or remove primitives, which have to keep the list and the
    /// primitive attribute columns in step — building it up with
    /// [`Self::push_primitive`] would leave the old ones in front.
    pub(crate) fn set_primitives(&mut self, primitives: Vec<Primitive>) {
        self.primitives = primitives;
    }

    /// The shared triangle index buffer backing every [`Primitive::Mesh`].
    pub fn indices(&self) -> &[u32] {
        &self.indices
    }

    /// Appends a mesh over `verts` whose `triangles` are offsets relative to
    /// `verts.start`, three per face. The single supported way to build a
    /// mesh, so the index range and the buffer cannot drift apart.
    pub fn push_mesh(&mut self, verts: Range<usize>, triangles: &[u32]) {
        let start = self.indices.len();
        Arc::make_mut(&mut self.indices).extend_from_slice(triangles);
        let indices = start..self.indices.len();
        self.primitives.push(Primitive::Mesh { verts, indices });
    }

    /// Appends raw triangle indices and returns the offset the run starts at.
    /// For callers that relocate whole primitives with [`Primitive::shifted`]
    /// rather than building one mesh at a time.
    pub fn extend_indices(&mut self, extra: &[u32]) -> usize {
        let start = self.indices.len();
        Arc::make_mut(&mut self.indices).extend_from_slice(extra);
        start
    }

    /// The triangle indices of one mesh primitive, or `None` for a path or an
    /// out-of-range run.
    pub fn mesh_indices(&self, prim: &Primitive) -> Option<&[u32]> {
        match prim {
            Primitive::Mesh { indices, .. } => self.indices.get(indices.clone()),
            Primitive::Path { .. } => None,
        }
    }

    /// Whether any primitive is a mesh.
    pub fn has_mesh(&self) -> bool {
        self.primitives.iter().any(Primitive::is_mesh)
    }

    /// `Ok` when every primitive is a path, or an error naming `operation`.
    /// The primitive-kind counterpart of [`Positions::require_planar`], used
    /// by operations whose definition is a polyline one (arc length, the
    /// analytic rasterizer).
    pub fn require_paths(&self, operation: &'static str) -> Result<(), GeometryError> {
        if self.has_mesh() {
            return Err(GeometryError::RequiresPathPrimitives { operation });
        }
        Ok(())
    }

    pub fn primitive_attrs(&self) -> &AttributeSet {
        &self.primitive_attrs
    }

    pub fn primitive_attrs_mut(&mut self) -> &mut AttributeSet {
        &mut self.primitive_attrs
    }

    pub fn instances(&self) -> &AttributeSet {
        &self.instances
    }

    pub fn instances_mut(&mut self) -> &mut AttributeSet {
        &mut self.instances
    }

    /// Every source available to the instance domain, images included, in the
    /// order `source_index` addresses them.
    pub fn sources(&self) -> &[InstanceSource] {
        &self.instance_sources
    }

    /// Replaces every source available to the instance domain.
    pub fn set_sources(&mut self, sources: Vec<InstanceSource>) {
        self.instance_sources = sources;
    }

    /// The first source, when it is a geometry. The convenience the
    /// geometry-only call sites keep using; [`Geometry::sources`] is the
    /// accessor that sees images too.
    pub fn instance_source(&self) -> Option<&Arc<Geometry>> {
        self.instance_sources
            .first()
            .and_then(InstanceSource::geometry)
    }

    pub fn set_instance_source(&mut self, source: Option<Arc<Geometry>>) {
        self.instance_sources = source.into_iter().map(InstanceSource::Geometry).collect();
    }

    /// Replaces the source geometries available to the instance domain,
    /// dropping any image source that was there.
    pub fn set_instance_sources(&mut self, sources: Vec<Arc<Geometry>>) {
        self.instance_sources = sources.into_iter().map(InstanceSource::Geometry).collect();
    }

    pub fn detail(&self) -> &AttributeSet {
        &self.detail
    }

    pub fn detail_mut(&mut self) -> &mut AttributeSet {
        &mut self.detail
    }

    pub fn attribute_set(&self, domain: Domain) -> &AttributeSet {
        match domain {
            Domain::Point => &self.points,
            Domain::Primitive => &self.primitive_attrs,
            Domain::Instance => &self.instances,
            Domain::Detail => &self.detail,
        }
    }

    pub(crate) fn attribute_set_mut(&mut self, domain: Domain) -> &mut AttributeSet {
        match domain {
            Domain::Point => &mut self.points,
            Domain::Primitive => &mut self.primitive_attrs,
            Domain::Instance => &mut self.instances,
            Domain::Detail => &mut self.detail,
        }
    }

    // ----- Element counts ----------------------------------------------------

    pub fn point_count(&self) -> usize {
        self.points.element_count()
    }

    pub fn primitive_count(&self) -> usize {
        self.primitives.len()
    }

    pub fn instance_count(&self) -> usize {
        self.instances.element_count()
    }

    fn primitive_len(&self) -> usize {
        self.primitive_attrs.element_count()
    }

    // ----- Summary ------------------------------------------------------------

    /// Debug/properties summary: element counts and attribute listings.
    pub fn summary(&self) -> GeometrySummary {
        GeometrySummary {
            point_count: self.point_count(),
            primitive_count: self.primitive_count(),
            instance_count: self.instance_count(),
            points: self.points.describe(),
            primitives: self.primitive_attrs.describe(),
            instances: self.instances.describe(),
            detail: self.detail.describe(),
        }
    }

    /// The xy extent of the point positions. `Rect` is a 2D value, so a 3D
    /// geometry reports the extent of its projection — depth is a
    /// `scene.render` concern, not a container one.
    fn positions_bounds(&self) -> Option<Rect> {
        let positions = self.positions(Domain::Point)?.ok()?;
        let mut components = positions.iter3();
        let first = components.next()?;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (first.0, first.1, first.0, first.1);
        for v in components {
            min_x = min_x.min(v.0);
            min_y = min_y.min(v.1);
            max_x = max_x.max(v.0);
            max_y = max_y.max(v.1);
        }
        Some(Rect {
            x: min_x,
            y: min_y,
            width: max_x - min_x,
            height: max_y - min_y,
        })
    }
}

/// Element counts and per-domain attribute listings for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeometrySummary {
    pub point_count: usize,
    pub primitive_count: usize,
    pub instance_count: usize,
    pub points: Vec<(AttrName, AttributeType)>,
    pub primitives: Vec<(AttrName, AttributeType)>,
    pub instances: Vec<(AttrName, AttributeType)>,
    pub detail: Vec<(AttrName, AttributeType)>,
}

impl NodeData for Geometry {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::GEOMETRY
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    /// A geometry reports GPU residency when any source it stamps is
    /// resident, because that is what the flag documents: the value is then
    /// not wholly CPU-readable and cannot be persisted or inspected without a
    /// readback through the owning crate.
    ///
    /// The cache tier is a single choice per value, so a geometry that mixes
    /// host attribute columns with one resident image is charged to VRAM in
    /// full — the same deliberate over-charge `Scene` makes, for the same
    /// reason: evicting sooner is cheap, under-reporting VRAM breaks a render.
    fn is_gpu_resident(&self) -> bool {
        self.instance_sources
            .iter()
            .any(InstanceSource::is_gpu_resident)
    }

    fn byte_size(&self) -> u64 {
        // Attribute columns dominate; the primitive and index blobs matter
        // for meshes. Instance sources recurse — a stamped geometry can be
        // arbitrarily large and is exactly what the budget must see.
        //
        // The additions saturate. An image source holds an `Arc<dyn NodeData>`
        // whose `byte_size` is whatever that implementation says, so a hostile
        // or simply wrong estimate must not overflow the accounting: a debug
        // panic or a release wrap would turn a bad estimate into a broken
        // budget, and `u64::MAX` is a perfectly good answer for "more than the
        // budget will ever hold".
        let sources = self.instance_sources.iter().fold(0u64, |total, source| {
            total.saturating_add(source.byte_size())
        });
        (size_of::<Self>() as u64)
            .saturating_add(self.points.byte_size())
            .saturating_add(self.primitive_attrs.byte_size())
            .saturating_add(self.instances.byte_size())
            .saturating_add(self.detail.byte_size())
            .saturating_add(
                (self.primitives.len() as u64).saturating_mul(size_of::<Primitive>() as u64),
            )
            .saturating_add((self.indices.len() as u64).saturating_mul(size_of::<u32>() as u64))
            .saturating_add(sources)
    }
}

impl GeometricData for Geometry {
    fn bounds(&self) -> Rect {
        self.positions_bounds().unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        })
    }

    fn transform(&self) -> Transform2D {
        // The container carries no intrinsic transform; placement is an
        // attribute/node concern.
        Transform2D::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FrameBuffer;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn two_point_geo() -> Geometry {
        Geometry::from_points(vec![Vec2(-1.0, 2.0), Vec2(3.0, -4.0)])
    }

    /// A CPU frame stamped as an image source.
    fn image(width: u32, height: u32) -> InstanceImage {
        InstanceImage::new(
            Arc::new(FrameBuffer::new_zeroed(width, height)),
            width,
            height,
        )
        .expect("a zeroed frame buffer is a valid instance image")
    }

    /// A frame buffer stand-in whose residency, size, and pixel access are all
    /// observable. `ravel-core` cannot name a GPU frame — only the crate that
    /// owns one can — so the behaviours a real `GpuFrameBuffer` would bring
    /// are declared here instead.
    struct FakeFrame {
        resident: bool,
        bytes: u64,
        /// How many times anything asked for the concrete value behind the
        /// `dyn NodeData`. Reaching the pixels means downcasting first, so a
        /// readback cannot happen without moving this.
        reads: AtomicUsize,
    }

    impl FakeFrame {
        fn new(resident: bool, bytes: u64) -> Arc<Self> {
            Arc::new(Self {
                resident,
                bytes,
                reads: AtomicUsize::new(0),
            })
        }
    }

    impl NodeData for FakeFrame {
        fn data_type_id(&self) -> DataTypeId {
            DataTypeId::FRAME_BUFFER
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self.reads.fetch_add(1, Ordering::Relaxed);
            self
        }

        fn is_gpu_resident(&self) -> bool {
            self.resident
        }

        fn byte_size(&self) -> u64 {
            self.bytes
        }
    }

    /// A geometry stamping one image source.
    fn geometry_with_image(image: InstanceImage) -> Geometry {
        let mut geo = Geometry::new();
        geo.instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))
            .unwrap();
        geo.set_sources(vec![InstanceSource::Image(image)]);
        geo
    }

    /// The rectangle an image source occupies is its pixel resolution, centred
    /// on the origin, so its aspect ratio is the source's (decision 5 of
    /// `docs/implementation/done/image-instancing-plan.md`).
    #[test]
    fn an_image_source_is_sized_by_its_pixel_resolution() {
        let wide = image(1920, 1080);
        assert_eq!(wide.width(), 1920);
        assert_eq!(wide.height(), 1080);
        let rect = wide.rect();
        assert_eq!((rect.x, rect.y), (-960.0, -540.0));
        assert_eq!((rect.width, rect.height), (1920.0, 1080.0));
        assert!((wide.aspect_ratio() - 16.0 / 9.0).abs() < 1e-4);
    }

    #[test]
    fn an_image_source_requires_a_frame_buffer_with_pixels() {
        let geometry: Arc<dyn NodeData> = Arc::new(two_point_geo());
        assert_eq!(
            InstanceImage::new(geometry, 4, 4).unwrap_err(),
            GeometryError::NotAFrameBuffer {
                data_type: DataTypeId::GEOMETRY.raw()
            }
        );

        for (width, height) in [(0, 4), (4, 0)] {
            let frame: Arc<dyn NodeData> = Arc::new(FrameBuffer::new_zeroed(width.max(1), height));
            assert_eq!(
                InstanceImage::new(frame, width, height).unwrap_err(),
                GeometryError::EmptyImage { width, height }
            );
        }
    }

    /// The pixels a geometry stamps are part of what it costs, so the cache
    /// budget has to see them.
    #[test]
    fn byte_size_counts_the_image_a_source_holds() {
        let bare = Geometry::new().byte_size();
        // 64 * 64 RgbaF32 pixels is 64 KiB of pixel bytes.
        let with_image = geometry_with_image(image(64, 64)).byte_size();
        assert!(
            with_image >= bare + 64 * 64 * 16,
            "an image source must account for its pixels: {with_image} vs {bare}"
        );
    }

    /// A `byte_size` an implementation is free to invent must not overflow the
    /// accounting into a debug panic or a release wrap.
    ///
    /// The second source is deliberately **small**: two `u64::MAX` sources
    /// wrap back to `u64::MAX - 1` and then saturate in the outer sum, which
    /// would let a wrapping fold pass. `MAX + 1024` wraps to `1023`, which
    /// nothing downstream can turn back into a large number.
    #[test]
    fn byte_size_saturates_instead_of_overflowing() {
        let source = |bytes| {
            InstanceSource::Image(
                InstanceImage::new(FakeFrame::new(false, bytes), 1, 1).expect("frame buffer"),
            )
        };
        let mut geo = Geometry::new();
        geo.set_sources(vec![source(u64::MAX), source(1024)]);
        assert_eq!(geo.byte_size(), u64::MAX);

        // And through a nesting level, which recurses into the same addition.
        let mut outer = Geometry::new();
        outer.set_instance_source(Some(Arc::new(geo)));
        assert_eq!(outer.byte_size(), u64::MAX);
    }

    /// Residency follows the content: a geometry holding a resident frame is
    /// not wholly CPU-readable, and that answer has to survive nesting.
    #[test]
    fn a_resident_image_makes_the_geometry_gpu_resident() {
        let cpu = geometry_with_image(image(8, 8));
        assert!(!cpu.is_gpu_resident(), "a CPU frame is not resident");
        assert!(
            !two_point_geo().is_gpu_resident(),
            "attribute columns alone are not resident"
        );

        let resident = geometry_with_image(
            InstanceImage::new(FakeFrame::new(true, 4096), 32, 32).expect("frame buffer"),
        );
        assert!(resident.is_gpu_resident());

        // Through a nested instance source, and past a CPU sibling so the
        // recursion cannot pass by finding the first source resident.
        let mut nested = Geometry::new();
        nested.set_instance_sources(vec![Arc::new(cpu), Arc::new(resident)]);
        assert!(nested.is_gpu_resident(), "residency propagates upward");

        let mut deeper = Geometry::new();
        deeper.set_instance_source(Some(Arc::new(nested)));
        assert!(deeper.is_gpu_resident(), "two levels up as well");
    }

    /// Holding a frame must not read it. A GPU-resident frame reaches its
    /// texture only through a downcast, so a construction, a size query, or a
    /// residency query that never downcasts cannot have read anything back.
    #[test]
    fn building_an_image_source_does_not_read_the_frame() {
        let frame = FakeFrame::new(true, 4096);
        let geo = geometry_with_image(
            InstanceImage::new(Arc::clone(&frame) as Arc<dyn NodeData>, 32, 32)
                .expect("frame buffer"),
        );
        assert!(geo.is_gpu_resident());
        assert!(geo.byte_size() >= 4096);
        let _ = geo.clone();
        assert_eq!(
            frame.reads.load(Ordering::Relaxed),
            0,
            "nothing may reach the pixels while the frame is only being held"
        );
    }

    /// [`Geometry::sources`] is the whole list, images included and in the
    /// order `source_index` addresses. The one geometry-only view left is the
    /// singular one, and it says so by returning `None` rather than by
    /// quietly renumbering what follows.
    #[test]
    fn sources_keep_images_in_the_indexed_positions() {
        let stamped = Arc::new(two_point_geo());
        let mut geo = Geometry::new();
        geo.set_sources(vec![
            InstanceSource::Image(image(4, 4)),
            InstanceSource::Geometry(Arc::clone(&stamped)),
        ]);

        assert_eq!(geo.sources().len(), 2);
        assert!(geo.sources()[0].image().is_some());
        assert!(geo.sources()[0].geometry().is_none());
        assert!(Arc::ptr_eq(
            geo.sources()[1].geometry().expect("a geometry source"),
            &stamped
        ));
        assert!(
            geo.instance_source().is_none(),
            "the first source is an image, so the singular geometry view is empty"
        );
    }

    /// `geometry.merge` compares sources without looking inside them, so the
    /// comparison has to distinguish two distinct images as well as two
    /// distinct geometries.
    #[test]
    fn sources_compare_by_identity_across_both_kinds() {
        let frame: Arc<dyn NodeData> = Arc::new(FrameBuffer::new_zeroed(4, 4));
        let shared = InstanceSource::Image(
            InstanceImage::new(Arc::clone(&frame), 4, 4).expect("frame buffer"),
        );
        assert!(shared.ptr_eq(&shared.clone()));
        assert!(
            !shared.ptr_eq(&InstanceSource::Image(image(4, 4))),
            "a different frame is a different source"
        );

        let geometry = InstanceSource::Geometry(Arc::new(two_point_geo()));
        assert!(geometry.ptr_eq(&geometry.clone()));
        assert!(!geometry.ptr_eq(&shared), "kinds never match each other");
    }

    /// `Geometry` derives `Debug`, so the hand-written image formatter has to
    /// stay printable — and it prints the residency rather than the pixels.
    #[test]
    fn an_image_source_prints_its_resolution_and_residency() {
        let text = format!("{:?}", geometry_with_image(image(16, 9)));
        assert!(text.contains("width: 16"), "{text}");
        assert!(text.contains("height: 9"), "{text}");
        assert!(text.contains("gpu_resident: false"), "{text}");
    }

    #[test]
    fn byte_size_grows_with_the_point_count() {
        let small = Geometry::from_points(vec![Vec2(0.0, 0.0); 16]);
        let large = Geometry::from_points(vec![Vec2(0.0, 0.0); 16_384]);
        // `P` (Vec2) plus `index` (i32) per point, at minimum.
        assert!(large.byte_size() - small.byte_size() >= (16_384 - 16) * 12);
    }

    #[test]
    fn from_points_sets_standard_attributes() {
        let geo = two_point_geo();
        assert_eq!(geo.point_count(), 2);
        assert_eq!(
            geo.points().get(names::P).unwrap().attr_type(),
            AttributeType::Vec2
        );
        assert_eq!(
            geo.points().get(names::INDEX).unwrap().as_i32(names::INDEX),
            Ok(&[0, 1][..])
        );
        assert_eq!(geo.validate(), Ok(()));
    }

    #[test]
    fn custom_attribute_on_point_and_instance_domains() {
        let mut geo = two_point_geo();
        geo.points_mut()
            .insert("heat", AttributeArray::F32(vec![0.5, 1.0]))
            .unwrap();
        geo.instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))
            .unwrap();
        geo.instances_mut()
            .insert(names::ROT, AttributeArray::F32(vec![1.57]))
            .unwrap();
        assert_eq!(geo.validate(), Ok(()));
        assert_eq!(geo.instance_count(), 1);
    }

    #[test]
    fn single_instance_source_api_wraps_first_plural_source() {
        let first = Arc::new(Geometry::from_points(vec![Vec2(1.0, 0.0)]));
        let second = Arc::new(Geometry::from_points(vec![Vec2(2.0, 0.0)]));
        let mut geo = Geometry::new();

        geo.set_instance_sources(vec![first.clone(), second.clone()]);
        assert_eq!(geo.sources().len(), 2);
        assert!(Arc::ptr_eq(geo.instance_source().unwrap(), &first));

        geo.set_instance_source(Some(second.clone()));
        assert_eq!(geo.sources().len(), 1);
        assert!(Arc::ptr_eq(geo.instance_source().unwrap(), &second));

        geo.set_instance_source(None);
        assert!(geo.sources().is_empty());
        assert!(geo.instance_source().is_none());
    }

    #[test]
    fn validate_rejects_wrong_p_type() {
        for domain in [Domain::Point, Domain::Instance] {
            let mut geo = Geometry::new();
            geo.attribute_set_mut(domain)
                .insert(names::P, AttributeArray::F32(vec![1.0]))
                .unwrap();
            assert!(matches!(
                geo.validate(),
                Err(GeometryError::PositionTypeMismatch { .. })
            ));
        }
    }

    /// `P` is a position column in either dimension, chosen per domain
    /// (REQ-3D-003): 3D points, and 2D points placed by 3D instances, are both
    /// valid geometry.
    #[test]
    fn validate_accepts_three_dimensional_positions() {
        let geo = Geometry::from_points3(vec![Vec3(1.0, 2.0, 3.0), Vec3(-4.0, 5.0, -6.0)]);
        assert_eq!(geo.validate(), Ok(()));
        assert_eq!(geo.point_count(), 2);
        assert_eq!(
            geo.points().get(names::INDEX).unwrap().as_i32(names::INDEX),
            Ok(&[0, 1][..])
        );

        let mut mixed = Geometry::from_points(vec![Vec2(0.0, 0.0)]);
        mixed
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec3(vec![Vec3(0.0, 0.0, 1.0), Vec3(0.0, 0.0, 2.0)]),
            )
            .unwrap();
        assert_eq!(mixed.validate(), Ok(()));
        assert_eq!(
            mixed.positions(Domain::Point).unwrap().unwrap().dimension(),
            2
        );
        assert_eq!(
            mixed
                .positions(Domain::Instance)
                .unwrap()
                .unwrap()
                .dimension(),
            3
        );
        assert!(mixed.positions(Domain::Detail).is_none());
    }

    #[test]
    fn positions_read_back_as_three_components_in_either_dimension() {
        let planar = Geometry::from_points(vec![Vec2(1.0, 2.0)]);
        let flat = planar.positions(Domain::Point).unwrap().unwrap();
        assert_eq!(flat.planar(), Some(&[Vec2(1.0, 2.0)][..]));
        assert_eq!(flat.get3(0), Some(Vec3(1.0, 2.0, 0.0)));
        assert_eq!(flat.require_planar("test").unwrap(), &[Vec2(1.0, 2.0)]);
        assert!(matches!(flat.projected(), std::borrow::Cow::Borrowed(_)));

        let spatial = Geometry::from_points3(vec![Vec3(1.0, 2.0, 3.0)]);
        let deep = spatial.positions(Domain::Point).unwrap().unwrap();
        assert_eq!(deep.planar(), None);
        assert_eq!(deep.get3(0), Some(Vec3(1.0, 2.0, 3.0)));
        assert_eq!(deep.projected().as_ref(), &[Vec2(1.0, 2.0)]);
        assert!(matches!(
            deep.require_planar("geometry.demo"),
            Err(GeometryError::RequiresPlanarP {
                operation: "geometry.demo",
                actual: AttributeType::Vec3,
                ..
            })
        ));
    }

    #[test]
    fn bounds_of_three_dimensional_points_is_the_xy_extent() {
        let geo = Geometry::from_points3(vec![Vec3(-1.0, 2.0, 100.0), Vec3(3.0, -4.0, -100.0)]);
        let b = geo.bounds();
        assert_eq!((b.x, b.y, b.width, b.height), (-1.0, -4.0, 4.0, 6.0));
    }

    /// An instance domain with placements owes a `P`, the same way the point
    /// domain does. Without the check the geometry constructs and fails later
    /// at whichever reader asks for the placement.
    #[test]
    fn validate_rejects_instances_without_positions() {
        let mut geo = Geometry::from_points(vec![Vec2(0.0, 0.0)]);
        geo.instances_mut()
            .insert(names::INDEX, AttributeArray::I32(vec![0, 1]))
            .unwrap();
        assert_eq!(geo.instance_count(), 2);
        assert_eq!(
            geo.validate(),
            Err(GeometryError::AttributeNotFound {
                name: names::P.into(),
            })
        );

        geo.instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(1.0, 1.0), Vec2(2.0, 2.0)]),
            )
            .unwrap();
        assert_eq!(geo.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_out_of_range_primitive() {
        let mut geo = two_point_geo();
        geo.push_primitive(Primitive::Path {
            verts: 0..5,
            closed: false,
        });
        assert!(matches!(
            geo.validate(),
            Err(GeometryError::LengthMismatch { .. })
        ));
    }

    /// A quad as two triangles over four points. The 2D case is the one that
    /// has to work first: a mesh is a primitive kind, independent of whether
    /// `P` is `Vec2` or `Vec3` (REQ-3D-003), so a planar triangulation is
    /// valid geometry long before anything can render it.
    fn planar_quad() -> Geometry {
        let mut geo = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(1.0, 0.0),
            Vec2(1.0, 1.0),
            Vec2(0.0, 1.0),
        ]);
        geo.push_mesh(0..4, &[0, 1, 2, 0, 2, 3]);
        geo
    }

    #[test]
    fn planar_mesh_is_valid_geometry() {
        let geo = planar_quad();
        assert_eq!(geo.validate(), Ok(()));
        assert_eq!(geo.primitive_count(), 1);
        assert_eq!(geo.indices(), &[0, 1, 2, 0, 2, 3]);
        assert_eq!(
            geo.points().get(names::P).unwrap().attr_type(),
            AttributeType::Vec2,
            "a mesh does not force positions to three dimensions"
        );
        let prim = &geo.primitives()[0];
        assert!(prim.is_mesh());
        assert_eq!(*prim.verts(), 0..4);
        assert_eq!(geo.mesh_indices(prim), Some(&[0, 1, 2, 0, 2, 3][..]));
    }

    #[test]
    fn three_dimensional_mesh_is_valid_geometry() {
        let mut geo = Geometry::from_points3(vec![
            Vec3(0.0, 0.0, 0.0),
            Vec3(1.0, 0.0, 0.0),
            Vec3(0.0, 1.0, 1.0),
        ]);
        geo.push_mesh(0..3, &[0, 1, 2]);
        assert_eq!(geo.validate(), Ok(()));
        assert!(geo.has_mesh());
    }

    #[test]
    fn mesh_indices_are_relative_to_the_vertex_range() {
        let mut geo = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(1.0, 0.0),
            Vec2(1.0, 1.0),
            Vec2(5.0, 5.0),
            Vec2(6.0, 5.0),
            Vec2(6.0, 6.0),
        ]);
        // The second mesh addresses points 3..6 with the same 0,1,2 values as
        // the first: index values are offsets from `verts.start`, not absolute
        // point indices.
        geo.push_mesh(0..3, &[0, 1, 2]);
        geo.push_mesh(3..6, &[0, 1, 2]);
        assert_eq!(geo.validate(), Ok(()));
        assert_eq!(geo.indices(), &[0, 1, 2, 0, 1, 2]);
        assert_eq!(*geo.primitives()[1].verts(), 3..6);
        assert_eq!(
            geo.mesh_indices(&geo.primitives()[1]),
            Some(&[0, 1, 2][..]),
            "the second mesh owns the second half of the shared buffer"
        );
    }

    #[test]
    fn validate_rejects_index_past_the_vertex_range() {
        let mut geo = two_point_geo();
        // Value 2 is out of range for a two-vertex mesh. Without the relative
        // encoding this would look like a valid absolute point index.
        geo.push_mesh(0..2, &[0, 1, 2]);
        assert!(matches!(
            geo.validate(),
            Err(GeometryError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn validate_rejects_index_run_that_is_not_whole_triangles() {
        let mut geo = two_point_geo();
        geo.push_mesh(0..2, &[0, 1]);
        assert!(matches!(
            geo.validate(),
            Err(GeometryError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn validate_rejects_index_run_past_the_shared_buffer() {
        let mut geo = planar_quad();
        geo.push_primitive(Primitive::Mesh {
            verts: 0..4,
            indices: 3..99,
        });
        assert!(matches!(
            geo.validate(),
            Err(GeometryError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn require_paths_reports_the_operation_that_refused_the_mesh() {
        assert_eq!(two_point_geo().require_paths("demo.op"), Ok(()));
        assert_eq!(
            planar_quad().require_paths("demo.op"),
            Err(GeometryError::RequiresPathPrimitives {
                operation: "demo.op"
            })
        );
    }

    /// The index buffer follows the same copy-on-write rule as an attribute
    /// column: cloning a geometry and editing only its points must not
    /// duplicate the triangles (REQ-CORE-004).
    #[test]
    fn cloning_shares_the_index_buffer_until_it_is_edited() {
        let original = planar_quad();
        let mut edited = original.clone();
        assert!(Arc::ptr_eq(&original.indices, &edited.indices));

        edited
            .points_mut()
            .make_mut(names::P)
            .unwrap()
            .as_vec2_mut(names::P)
            .unwrap()[0] = Vec2(9.0, 9.0);
        assert!(
            Arc::ptr_eq(&original.indices, &edited.indices),
            "editing points leaves the index buffer shared"
        );

        edited.push_mesh(0..4, &[0, 1, 2]);
        assert!(!Arc::ptr_eq(&original.indices, &edited.indices));
        assert_eq!(
            original.indices(),
            &[0, 1, 2, 0, 2, 3],
            "the original keeps its own triangles"
        );
    }

    #[test]
    fn validate_rejects_multi_value_detail() {
        let mut geo = two_point_geo();
        geo.detail_mut()
            .insert("comment", AttributeArray::Str(vec!["a".into(), "b".into()]))
            .unwrap();
        assert!(matches!(
            geo.validate(),
            Err(GeometryError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn clone_shares_columns_until_mutation() {
        let original = two_point_geo();
        let mut copy = original.clone();
        assert!(Arc::ptr_eq(
            original.points().get(names::P).unwrap(),
            copy.points().get(names::P).unwrap()
        ));
        copy.points_mut()
            .make_mut(names::P)
            .unwrap()
            .as_vec2_mut(names::P)
            .unwrap()[0] = Vec2(9.0, 9.0);
        assert!(!Arc::ptr_eq(
            original.points().get(names::P).unwrap(),
            copy.points().get(names::P).unwrap()
        ));
        // Untouched column still shared.
        assert!(Arc::ptr_eq(
            original.points().get(names::INDEX).unwrap(),
            copy.points().get(names::INDEX).unwrap()
        ));
    }

    #[test]
    fn geometry_flows_as_node_data() {
        let geo = two_point_geo();
        let data: &dyn NodeData = &geo;
        assert_eq!(data.data_type_id(), DataTypeId::GEOMETRY);
        let roundtrip = data.downcast_ref::<Geometry>().unwrap();
        assert_eq!(roundtrip.point_count(), 2);
    }

    #[test]
    fn bounds_covers_all_points() {
        let geo = two_point_geo();
        let b = geo.bounds();
        assert_eq!((b.x, b.y, b.width, b.height), (-1.0, -4.0, 4.0, 6.0));
    }

    #[test]
    fn summary_lists_counts_and_attributes() {
        let geo = two_point_geo();
        let s = geo.summary();
        assert_eq!(s.point_count, 2);
        assert_eq!(s.primitive_count, 0);
        assert_eq!(s.instance_count, 0);
        assert!(
            s.points
                .iter()
                .any(|(n, t)| n == names::P && *t == AttributeType::Vec2)
        );
        assert!(
            s.points
                .iter()
                .any(|(n, t)| n == names::INDEX && *t == AttributeType::I32)
        );
    }

    /// The placement is scale, then rotate, then translate — in that order.
    /// Any other order puts a scaled instance somewhere else entirely.
    #[test]
    fn an_instance_transform_scales_then_turns_then_moves() {
        let placement = InstanceTransform {
            offset: Vec2(10.0, -4.0),
            rot: std::f32::consts::FRAC_PI_2,
            scale: Vec2(3.0, 2.0),
        };
        // (1, 0) scales to (3, 0), turns a quarter turn to (0, 3), and moves.
        let placed = placement.apply(Vec2(1.0, 0.0));
        assert!(
            (placed.0 - 10.0).abs() < 1e-5 && (placed.1 - (-1.0)).abs() < 1e-5,
            "scale-rotate-translate puts it at (10, -1), not {placed:?}"
        );
        // Translating before rotating would land at (0 + 3*cos - (-4)*sin,
        // ...) = (4, 6) instead, so the two orders are distinguishable here.
    }

    /// A tangent is a difference, so it takes the linear part of the
    /// placement and none of the translation.
    #[test]
    fn an_instance_transform_carries_a_vector_without_the_translation() {
        let placement = InstanceTransform {
            offset: Vec2(100.0, 100.0),
            rot: std::f32::consts::FRAC_PI_2,
            scale: Vec2(2.0, 2.0),
        };
        let carried = placement.apply_vector(Vec2(1.0, 0.0));
        assert!(
            (carried.0 - 0.0).abs() < 1e-5 && (carried.1 - 2.0).abs() < 1e-5,
            "a tangent scales and turns but does not move: {carried:?}"
        );
    }

    /// `compose(outer, inner)` has to equal applying `inner` and then
    /// `outer`, which is what makes nested instances land where the two
    /// separate walks would put them.
    #[test]
    fn composing_two_instance_transforms_equals_applying_them_in_turn() {
        // Uniform scales, which is where the representation is exact — see
        // `compose`'s own note about the shear it cannot hold.
        let outer = InstanceTransform {
            offset: Vec2(7.0, -3.0),
            rot: 0.4,
            scale: Vec2(1.5, 1.5),
        };
        let inner = InstanceTransform {
            offset: Vec2(-2.0, 6.0),
            rot: -0.9,
            scale: Vec2(2.0, 2.0),
        };
        let composed = InstanceTransform::compose(outer, inner);
        for point in [Vec2(0.0, 0.0), Vec2(1.0, 0.0), Vec2(-4.0, 2.5)] {
            let stepwise = outer.apply(inner.apply(point));
            let direct = composed.apply(point);
            assert!(
                (stepwise.0 - direct.0).abs() < 1e-3 && (stepwise.1 - direct.1).abs() < 1e-3,
                "{point:?}: stepwise {stepwise:?} != composed {direct:?}"
            );
        }
        // The offset rule holds whatever the scales are: a nested instance
        // sits where its parent's placement puts its own origin.
        let skewed = InstanceTransform::compose(
            InstanceTransform {
                scale: Vec2(1.5, 0.5),
                ..outer
            },
            inner,
        );
        let expected = InstanceTransform {
            scale: Vec2(1.5, 0.5),
            ..outer
        }
        .apply(inner.offset);
        assert_eq!(skewed.offset, expected);
    }

    #[test]
    fn the_identity_instance_transform_moves_nothing() {
        let point = Vec2(3.0, -8.0);
        assert_eq!(InstanceTransform::IDENTITY.apply(point), point);
        assert_eq!(InstanceTransform::IDENTITY.apply_vector(point), point);
        assert_eq!(InstanceTransform::IDENTITY.uniform_scale(), 1.0);
    }

    /// A mirrored scale still scales a stroke by a positive width.
    #[test]
    fn a_mirrored_instance_transform_has_a_positive_uniform_scale() {
        let mirrored = InstanceTransform {
            scale: Vec2(-4.0, 2.0),
            ..InstanceTransform::IDENTITY
        };
        assert_eq!(mirrored.uniform_scale(), 3.0);
    }
}
