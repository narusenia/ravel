// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The [`Scene`] data type: what `scene.add` / `scene.merge` /
//! `scene.camera` build and `scene.render` consumes (REQ-3D-001).
//!
//! A scene is a list of [`SceneObject`]s and a list of [`Camera`]s. An object
//! is a piece of content — a [`Geometry`], a frame buffer placed as a
//! textured rectangle, or a **nested scene** — paired with a
//! [`Transform3D`]. Nesting is how a transform hierarchy is expressed: a
//! child scene handed to a parent's `scene.add` follows the parent's
//! transform, which is the Null / parenting idiom of C4D and After Effects.
//!
//! Lights are not part of a scene yet; they arrive with `scene.light`.
//!
//! # Not persisted
//!
//! A `Scene` deliberately implements neither `Serialize` nor `Deserialize`.
//! It is an evaluated value flowing between ports, exactly like
//! [`Geometry`] and `FrameBuffer`; `.ravprj` stores nodes, edges, and
//! parameters. That is also why the object model can be extended later
//! without a file migration (`docs/implementation/3d-scene-plan.md`).
//!
//! # Structural sharing
//!
//! The object and camera lists are [`im::Vector`]s and the content behind an
//! object is `Arc`-shared, so adding one object to a scene of a thousand
//! copies neither the list spine nor any geometry (REQ-CORE-004).

pub mod camera;
pub mod matrix;

pub use camera::{Camera, Projection};
pub use matrix::Mat4;

use crate::geometry::Geometry;
use crate::id::DataTypeId;
use crate::types::{NodeData, Rect};
use std::fmt;
use std::sync::Arc;

/// Why a value cannot be placed in a scene.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SceneError {
    /// The value handed to [`SceneImage::new`] is not a frame buffer.
    #[error("a scene image must be a frame buffer, but the value is data type {data_type}")]
    NotAFrameBuffer {
        /// Raw [`DataTypeId`] of the offending value.
        data_type: u32,
    },

    /// A frame buffer with no pixels has no rectangle to be drawn on.
    #[error("a scene image must have a non-zero resolution, but this one is {width}x{height}")]
    EmptyImage {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
    },
}

// ===========================================================================
// Transform3D
// ===========================================================================

/// A 3D placement authored as component channels.
///
/// Rotation is **Euler angles in degrees**, applied in the fixed extrinsic
/// `Z → Y → X` order of `docs/specifications/procedural-geometry.md` — each
/// angle turns about a fixed scene axis, not about the axes the previous
/// rotation left behind. It is
/// deliberately not a quaternion: the unified animation channel interpolates
/// components independently (REQ-CORE-007), and component-wise interpolation
/// of a quaternion is not a rotation. Per-element orientation — which does
/// not pass through a channel — is the `orient` attribute's job instead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform3D {
    /// Offset in composition units.
    pub translate: [f32; 3],
    /// Euler angles in degrees, applied about the fixed axes Z → Y → X.
    pub rotate: [f32; 3],
    /// Scale factor per axis.
    pub scale: [f32; 3],
    /// Point held fixed by the rotation and the scale, in the object's own
    /// space.
    pub pivot: [f32; 3],
}

impl Default for Transform3D {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform3D {
    /// The transform that changes nothing.
    pub const IDENTITY: Self = Self {
        translate: [0.0, 0.0, 0.0],
        rotate: [0.0, 0.0, 0.0],
        scale: [1.0, 1.0, 1.0],
        pivot: [0.0, 0.0, 0.0],
    };

    /// A pure translation.
    pub const fn from_translation(translate: [f32; 3]) -> Self {
        Self {
            translate,
            ..Self::IDENTITY
        }
    }

    /// The matrix form: `T(translate) · T(pivot) · R · S · T(-pivot)`.
    ///
    /// Scale and rotation act about `pivot`, which therefore stays fixed when
    /// `translate` is zero — the same order `geometry.transform` applies in
    /// 2D.
    pub fn to_matrix(&self) -> Mat4 {
        let negative_pivot = [-self.pivot[0], -self.pivot[1], -self.pivot[2]];
        Mat4::from_translation(self.translate)
            .mul(&Mat4::from_translation(self.pivot))
            .mul(&Mat4::from_euler_zyx_degrees(self.rotate))
            .mul(&Mat4::from_scale(self.scale))
            .mul(&Mat4::from_translation(negative_pivot))
    }
}

// ===========================================================================
// SceneImage
// ===========================================================================

/// A frame buffer placed in a scene as a textured rectangle.
///
/// The frame is held as an `Arc<dyn NodeData>` rather than a concrete
/// `FrameBuffer` so a GPU-resident frame passes through without a readback:
/// both representations are tagged [`DataTypeId::FRAME_BUFFER`] and only the
/// crate that owns the GPU one can name it. The resolution is captured
/// alongside, which is what lets `ravel-core` size the rectangle without
/// knowing which representation it holds.
#[derive(Clone)]
pub struct SceneImage {
    frame: Arc<dyn NodeData>,
    width: u32,
    height: u32,
}

impl SceneImage {
    /// Place `frame` as a rectangle of `width` × `height` composition units.
    ///
    /// The size is the image's own pixel resolution, so the rectangle keeps
    /// the source's aspect ratio by construction (REQ-3D-001).
    pub fn new(frame: Arc<dyn NodeData>, width: u32, height: u32) -> Result<Self, SceneError> {
        if frame.data_type_id() != DataTypeId::FRAME_BUFFER {
            return Err(SceneError::NotAFrameBuffer {
                data_type: frame.data_type_id().raw(),
            });
        }
        if width == 0 || height == 0 {
            return Err(SceneError::EmptyImage { width, height });
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

    /// The rectangle this image occupies in the object's own space, centred
    /// on the object origin and sized from the image resolution.
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

impl fmt::Debug for SceneImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SceneImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("gpu_resident", &self.frame.is_gpu_resident())
            .finish()
    }
}

// ===========================================================================
// SceneObject
// ===========================================================================

/// What a scene object draws.
#[derive(Clone, Debug)]
pub enum SceneContent {
    /// A geometry — `Primitive::Path` or `Primitive::Mesh`, with a `P` column
    /// of either dimension.
    Geometry(Arc<Geometry>),
    /// A frame buffer as a textured rectangle.
    Image(SceneImage),
    /// A nested scene. Its objects compose under this object's transform;
    /// its cameras are **not** promoted into the parent — a camera belongs to
    /// the scene it was merged into.
    Scene(Arc<Scene>),
}

/// One placement of one piece of content in a scene.
#[derive(Clone, Debug)]
pub struct SceneObject {
    /// What is drawn.
    pub content: SceneContent,
    /// Where it is drawn.
    pub transform: Transform3D,
}

impl SceneObject {
    /// Place `content` with `transform`.
    pub fn new(content: SceneContent, transform: Transform3D) -> Self {
        Self { content, transform }
    }

    /// Place a geometry.
    pub fn geometry(geometry: Arc<Geometry>, transform: Transform3D) -> Self {
        Self::new(SceneContent::Geometry(geometry), transform)
    }

    /// Place a frame buffer as a textured rectangle.
    pub fn image(image: SceneImage, transform: Transform3D) -> Self {
        Self::new(SceneContent::Image(image), transform)
    }

    /// Place a nested scene.
    pub fn scene(scene: Arc<Scene>, transform: Transform3D) -> Self {
        Self::new(SceneContent::Scene(scene), transform)
    }
}

// ===========================================================================
// FlatObject
// ===========================================================================

/// A drawable leaf of a scene with every enclosing transform composed in.
///
/// Produced by [`Scene::flatten`]. Nested scenes are gone: what remains is
/// content plus one scene-space matrix, which is the form a renderer wants.
#[derive(Clone, Debug)]
pub struct FlatObject {
    /// The content — never a nested scene.
    pub content: FlatContent,
    /// Object space → scene space.
    pub world_transform: Mat4,
}

/// The content of a [`FlatObject`]: [`SceneContent`] minus the nesting case.
#[derive(Clone, Debug)]
pub enum FlatContent {
    /// A geometry.
    Geometry(Arc<Geometry>),
    /// A frame buffer as a textured rectangle.
    Image(SceneImage),
}

// ===========================================================================
// Scene
// ===========================================================================

/// A 3D scene flowing through the node graph ([`DataTypeId::SCENE`]).
#[derive(Clone, Debug, Default)]
pub struct Scene {
    objects: im::Vector<SceneObject>,
    cameras: im::Vector<Camera>,
}

impl Scene {
    /// An empty scene: no objects, no cameras.
    pub fn new() -> Self {
        Self::default()
    }

    /// A scene holding one camera and nothing else — what `scene.camera`
    /// produces.
    pub fn from_camera(camera: Camera) -> Self {
        let mut cameras = im::Vector::new();
        cameras.push_back(camera);
        Self {
            objects: im::Vector::new(),
            cameras,
        }
    }

    /// This scene with `object` appended.
    ///
    /// Returns a new value; the receiver is untouched and the two share their
    /// list spine and all their content.
    pub fn with_object(&self, object: SceneObject) -> Self {
        let mut next = self.clone();
        next.objects.push_back(object);
        next
    }

    /// This scene with `camera` appended.
    pub fn with_camera(&self, camera: Camera) -> Self {
        let mut next = self.clone();
        next.cameras.push_back(camera);
        next
    }

    /// The union of two scenes: `self`'s objects and cameras first, then
    /// `other`'s. This is what `scene.merge` computes.
    pub fn merged(&self, other: &Self) -> Self {
        let mut objects = self.objects.clone();
        objects.append(other.objects.clone());
        let mut cameras = self.cameras.clone();
        cameras.append(other.cameras.clone());
        Self { objects, cameras }
    }

    /// The objects of this scene, in insertion order. Nested scenes appear as
    /// single [`SceneContent::Scene`] objects; use [`Scene::flatten`] for the
    /// drawable leaves.
    pub fn objects(&self) -> &im::Vector<SceneObject> {
        &self.objects
    }

    /// The cameras of this scene, in insertion order.
    pub fn cameras(&self) -> &im::Vector<Camera> {
        &self.cameras
    }

    /// Number of objects at this level (nested scenes count as one).
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Number of cameras at this level.
    pub fn camera_count(&self) -> usize {
        self.cameras.len()
    }

    /// The camera a renderer uses when it is not told which one: the first
    /// that was merged in.
    pub fn primary_camera(&self) -> Option<&Camera> {
        self.cameras.front()
    }

    /// Every drawable leaf, with the nested transforms composed into one
    /// scene-space matrix each.
    ///
    /// Order is depth-first in insertion order, so a renderer's own sorting
    /// starts from a deterministic list.
    ///
    /// The traversal assumes the nesting is **acyclic**, and the public API
    /// cannot produce anything else: a `Scene` is immutable with no interior
    /// mutability, so [`SceneContent::Scene`] can only ever wrap an
    /// already-constructed value. A cycle would need a scene to be edited
    /// after it was embedded, which no method offers. The same assumption
    /// holds for `NodeData::is_gpu_resident` and `NodeData::byte_size`, which
    /// walk the tree the same way.
    pub fn flatten(&self) -> Vec<FlatObject> {
        let mut out = Vec::new();
        self.collect_flat(&Mat4::IDENTITY, &mut out);
        out
    }

    fn collect_flat(&self, parent: &Mat4, out: &mut Vec<FlatObject>) {
        for object in &self.objects {
            let world = parent.mul(&object.transform.to_matrix());
            match &object.content {
                SceneContent::Geometry(geometry) => out.push(FlatObject {
                    content: FlatContent::Geometry(Arc::clone(geometry)),
                    world_transform: world,
                }),
                SceneContent::Image(image) => out.push(FlatObject {
                    content: FlatContent::Image(image.clone()),
                    world_transform: world,
                }),
                SceneContent::Scene(nested) => nested.collect_flat(&world, out),
            }
        }
    }

    /// Whether any content in this scene, at any nesting depth, is a
    /// GPU-resident value.
    fn holds_gpu_resident(&self) -> bool {
        self.objects.iter().any(|object| match &object.content {
            SceneContent::Geometry(geometry) => geometry.is_gpu_resident(),
            SceneContent::Image(image) => image.frame.is_gpu_resident(),
            SceneContent::Scene(nested) => nested.holds_gpu_resident(),
        })
    }
}

impl NodeData for Scene {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::SCENE
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    /// A scene reports GPU residency when any frame buffer it holds is a
    /// texture, because that is what the flag documents: the value is then
    /// not wholly CPU-readable and cannot be persisted or inspected without
    /// a readback through the owning crate.
    ///
    /// The cache tier is a single choice per value, so a scene that mixes CPU
    /// geometry with one resident texture is charged to VRAM in full. That
    /// over-charge is deliberate — evicting a scene sooner is cheap, and the
    /// alternative (calling a scene CPU-resident while it pins a texture)
    /// under-reports VRAM, which is the failure mode that actually breaks a
    /// render.
    fn is_gpu_resident(&self) -> bool {
        self.holds_gpu_resident()
    }

    fn byte_size(&self) -> u64 {
        // Content is counted through the values themselves, nested scenes
        // included: a scene can hold an arbitrarily large geometry and that
        // is exactly what the budget has to see. Shared `Arc`s are counted
        // once per holder, the same convention `Geometry` uses for its
        // instance sources.
        //
        // The additions saturate. `byte_size` is an approximation an arbitrary
        // `NodeData` implementation supplies, so a hostile or simply wrong one
        // must not overflow the accounting — a debug panic or a release wrap
        // would turn a bad estimate into a broken budget, and `u64::MAX` is a
        // perfectly good answer for "more than the budget will ever hold".
        let content = self.objects.iter().fold(0u64, |total, object| {
            let bytes = match &object.content {
                SceneContent::Geometry(geometry) => geometry.byte_size(),
                SceneContent::Image(image) => image.frame.byte_size(),
                SceneContent::Scene(nested) => nested.byte_size(),
            };
            total.saturating_add(bytes)
        });
        (size_of::<Self>() as u64)
            .saturating_add(
                (self.objects.len() as u64).saturating_mul(size_of::<SceneObject>() as u64),
            )
            .saturating_add((self.cameras.len() as u64).saturating_mul(size_of::<Camera>() as u64))
            .saturating_add(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FrameBuffer, Vec2};

    fn geometry() -> Arc<Geometry> {
        Arc::new(Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]))
    }

    fn image(width: u32, height: u32) -> SceneImage {
        SceneImage::new(
            Arc::new(FrameBuffer::new_zeroed(width, height)),
            width,
            height,
        )
        .expect("a zeroed frame buffer is a valid scene image")
    }

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-4,
            "{what}: expected {expected}, got {actual}"
        );
    }

    #[test]
    fn an_empty_scene_holds_nothing() {
        let scene = Scene::new();
        assert_eq!(scene.object_count(), 0);
        assert_eq!(scene.camera_count(), 0);
        assert!(scene.primary_camera().is_none());
        assert!(scene.flatten().is_empty());
    }

    /// Objects go in and come back out, in order, and adding one leaves the
    /// scene it was added to alone.
    #[test]
    fn objects_are_added_and_read_back_in_order() {
        let first =
            SceneObject::geometry(geometry(), Transform3D::from_translation([1.0, 0.0, 0.0]));
        let second = SceneObject::image(image(16, 8), Transform3D::IDENTITY);

        let empty = Scene::new();
        let one = empty.with_object(first);
        let two = one.with_object(second);

        assert_eq!(empty.object_count(), 0, "the original is untouched");
        assert_eq!(one.object_count(), 1);
        assert_eq!(two.object_count(), 2);

        assert!(matches!(
            two.objects()[0].content,
            SceneContent::Geometry(_)
        ));
        assert_eq!(two.objects()[0].transform.translate, [1.0, 0.0, 0.0]);
        assert!(matches!(two.objects()[1].content, SceneContent::Image(_)));
    }

    #[test]
    fn cameras_are_added_and_read_back_in_order() {
        let near = Camera::looking_at([0.0, 0.0, -100.0], [0.0, 0.0, 0.0]);
        let far = Camera::looking_at([0.0, 0.0, -900.0], [0.0, 0.0, 0.0]);
        let scene = Scene::from_camera(near).with_camera(far);
        assert_eq!(scene.camera_count(), 2);
        assert_eq!(scene.primary_camera(), Some(&near));
        assert_eq!(scene.cameras()[1], far);
    }

    #[test]
    fn merge_concatenates_objects_and_cameras() {
        let a = Scene::new()
            .with_object(SceneObject::geometry(geometry(), Transform3D::IDENTITY))
            .with_camera(Camera::default());
        let b = Scene::new()
            .with_object(SceneObject::image(image(4, 4), Transform3D::IDENTITY))
            .with_object(SceneObject::image(image(8, 8), Transform3D::IDENTITY));

        let merged = a.merged(&b);
        assert_eq!(merged.object_count(), 3);
        assert_eq!(merged.camera_count(), 1);
        assert_eq!(a.object_count(), 1, "operands are untouched");
        assert_eq!(b.object_count(), 2);
        assert!(matches!(
            merged.objects()[0].content,
            SceneContent::Geometry(_)
        ));
    }

    /// A child scene handed to a parent's `scene.add` inherits the parent's
    /// transform: moving the parent moves the child.
    #[test]
    fn nested_transforms_compose() {
        let child = Scene::new().with_object(SceneObject::geometry(
            geometry(),
            Transform3D::from_translation([10.0, 0.0, 0.0]),
        ));
        let parent = Scene::new().with_object(SceneObject::scene(
            Arc::new(child),
            Transform3D::from_translation([0.0, 5.0, 0.0]),
        ));

        let flat = parent.flatten();
        assert_eq!(flat.len(), 1, "nesting flattens to its leaves");
        assert_eq!(
            flat[0].world_transform.transform_point3([0.0, 0.0, 0.0]),
            [10.0, 5.0, 0.0]
        );

        // Moving the parent moves the child with it.
        let moved = Scene::new().with_object(SceneObject::scene(
            Arc::new(Scene::new().with_object(SceneObject::geometry(
                geometry(),
                Transform3D::from_translation([10.0, 0.0, 0.0]),
            ))),
            Transform3D::from_translation([100.0, 5.0, 0.0]),
        ));
        assert_eq!(
            moved.flatten()[0]
                .world_transform
                .transform_point3([0.0, 0.0, 0.0]),
            [110.0, 5.0, 0.0]
        );
    }

    /// A parent rotation rotates the child's *offset*, which is the whole
    /// point of a hierarchy — it is not the same as rotating the child in
    /// place.
    #[test]
    fn a_parent_rotation_carries_the_child_offset_around() {
        let child = Arc::new(Scene::new().with_object(SceneObject::geometry(
            geometry(),
            Transform3D::from_translation([100.0, 0.0, 0.0]),
        )));
        let parent = Scene::new().with_object(SceneObject::scene(
            Arc::clone(&child),
            Transform3D {
                rotate: [0.0, 0.0, 90.0],
                ..Transform3D::IDENTITY
            },
        ));

        let point = parent.flatten()[0]
            .world_transform
            .transform_point3([0.0, 0.0, 0.0]);
        assert_close(point[0], 0.0, "x");
        assert_close(point[1], 100.0, "y");
        assert_close(point[2], 0.0, "z");
    }

    #[test]
    fn nesting_goes_deeper_than_one_level() {
        let leaf = Arc::new(Scene::new().with_object(SceneObject::geometry(
            geometry(),
            Transform3D::from_translation([1.0, 0.0, 0.0]),
        )));
        let middle = Arc::new(Scene::new().with_object(SceneObject::scene(
            leaf,
            Transform3D::from_translation([0.0, 2.0, 0.0]),
        )));
        let root = Scene::new().with_object(SceneObject::scene(
            middle,
            Transform3D::from_translation([0.0, 0.0, 4.0]),
        ));

        let flat = root.flatten();
        assert_eq!(flat.len(), 1);
        assert_eq!(
            flat[0].world_transform.transform_point3([0.0, 0.0, 0.0]),
            [1.0, 2.0, 4.0]
        );
    }

    #[test]
    fn flatten_visits_every_leaf_in_depth_first_order() {
        let nested = Arc::new(
            Scene::new()
                .with_object(SceneObject::image(image(2, 2), Transform3D::IDENTITY))
                .with_object(SceneObject::image(image(3, 3), Transform3D::IDENTITY)),
        );
        let scene = Scene::new()
            .with_object(SceneObject::geometry(geometry(), Transform3D::IDENTITY))
            .with_object(SceneObject::scene(nested, Transform3D::IDENTITY))
            .with_object(SceneObject::geometry(geometry(), Transform3D::IDENTITY));

        let flat = scene.flatten();
        assert_eq!(flat.len(), 4);
        assert!(matches!(flat[0].content, FlatContent::Geometry(_)));
        assert!(matches!(flat[1].content, FlatContent::Image(_)));
        assert!(matches!(flat[2].content, FlatContent::Image(_)));
        assert!(matches!(flat[3].content, FlatContent::Geometry(_)));
    }

    #[test]
    fn a_nested_scene_camera_is_not_promoted_to_the_parent() {
        let child = Arc::new(Scene::from_camera(Camera::default()));
        let parent = Scene::new().with_object(SceneObject::scene(child, Transform3D::IDENTITY));
        assert_eq!(parent.camera_count(), 0);
        assert!(parent.primary_camera().is_none());
    }

    /// The rectangle a frame buffer occupies is its pixel resolution, so its
    /// aspect ratio is the image's (REQ-3D-001).
    #[test]
    fn an_image_object_keeps_the_source_aspect_ratio() {
        let wide = image(1920, 1080);
        let rect = wide.rect();
        assert_eq!(rect.width, 1920.0);
        assert_eq!(rect.height, 1080.0);
        assert_close(rect.width / rect.height, 16.0 / 9.0, "aspect ratio");
        assert_close(wide.aspect_ratio(), 16.0 / 9.0, "aspect_ratio()");
        // Centred on the object origin.
        assert_eq!(rect.x, -960.0);
        assert_eq!(rect.y, -540.0);

        let tall = image(600, 800);
        assert_close(tall.aspect_ratio(), 0.75, "portrait aspect ratio");
        assert_close(
            tall.rect().width / tall.rect().height,
            0.75,
            "portrait rect aspect ratio",
        );
    }

    /// A scaled image object still has the source's aspect ratio, because
    /// the rectangle carries it and the transform is uniform.
    #[test]
    fn a_scaled_image_object_still_has_the_source_aspect_ratio() {
        let object = SceneObject::image(
            image(1920, 1080),
            Transform3D {
                scale: [0.25, 0.25, 1.0],
                ..Transform3D::IDENTITY
            },
        );
        let SceneContent::Image(placed) = &object.content else {
            panic!("content is an image");
        };
        let matrix = object.transform.to_matrix();
        let rect = placed.rect();
        let left = matrix.transform_point3([rect.x, rect.y, 0.0]);
        let right = matrix.transform_point3([rect.x + rect.width, rect.y + rect.height, 0.0]);
        let width = right[0] - left[0];
        let height = right[1] - left[1];
        assert_close(width, 480.0, "scaled width");
        assert_close(height, 270.0, "scaled height");
        assert_close(width / height, 16.0 / 9.0, "aspect ratio after scaling");
    }

    #[test]
    fn a_non_frame_buffer_cannot_be_placed_as_an_image() {
        let error =
            SceneImage::new(geometry(), 4, 4).expect_err("a geometry is not a frame buffer");
        assert_eq!(
            error,
            SceneError::NotAFrameBuffer {
                data_type: DataTypeId::GEOMETRY.raw()
            }
        );
    }

    #[test]
    fn an_empty_frame_buffer_cannot_be_placed_as_an_image() {
        let frame: Arc<dyn NodeData> = Arc::new(FrameBuffer::new_zeroed(0, 4));
        let error = SceneImage::new(frame, 0, 4).expect_err("an empty image has no rectangle");
        assert_eq!(
            error,
            SceneError::EmptyImage {
                width: 0,
                height: 4
            }
        );
    }

    #[test]
    fn transform_pivot_stays_fixed_under_rotation_and_scale() {
        let transform = Transform3D {
            rotate: [0.0, 0.0, 90.0],
            scale: [3.0, 3.0, 3.0],
            pivot: [50.0, 50.0, 0.0],
            translate: [0.0, 0.0, 0.0],
        };
        let fixed = transform.to_matrix().transform_point3([50.0, 50.0, 0.0]);
        assert_close(fixed[0], 50.0, "pivot x");
        assert_close(fixed[1], 50.0, "pivot y");
        assert_close(fixed[2], 0.0, "pivot z");
    }

    #[test]
    fn the_identity_transform_is_the_identity_matrix() {
        assert_eq!(Transform3D::IDENTITY.to_matrix(), Mat4::IDENTITY);
        assert_eq!(Transform3D::default(), Transform3D::IDENTITY);
    }

    #[test]
    fn data_type_id_is_scene() {
        let scene: Arc<dyn NodeData> = Arc::new(Scene::new());
        assert_eq!(scene.data_type_id(), DataTypeId::SCENE);
        assert!(scene.downcast_ref::<Scene>().is_some());
    }

    /// A scene of CPU values is CPU-resident, so the cache charges it to host
    /// memory.
    #[test]
    fn a_cpu_scene_is_not_gpu_resident() {
        let scene = Scene::new()
            .with_object(SceneObject::geometry(geometry(), Transform3D::IDENTITY))
            .with_object(SceneObject::image(image(8, 8), Transform3D::IDENTITY));
        assert!(!scene.is_gpu_resident());
    }

    /// A resident frame anywhere in the tree — nested scenes included — makes
    /// the whole scene GPU-resident.
    #[test]
    fn a_resident_frame_makes_the_whole_scene_gpu_resident() {
        struct ResidentFrame;
        impl NodeData for ResidentFrame {
            fn data_type_id(&self) -> DataTypeId {
                DataTypeId::FRAME_BUFFER
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn is_gpu_resident(&self) -> bool {
                true
            }
            fn byte_size(&self) -> u64 {
                4096
            }
        }

        let resident = SceneImage::new(Arc::new(ResidentFrame), 32, 32).expect("frame buffer");
        let scene =
            Scene::new().with_object(SceneObject::image(resident.clone(), Transform3D::IDENTITY));
        assert!(scene.is_gpu_resident());

        let nested =
            Scene::new().with_object(SceneObject::scene(Arc::new(scene), Transform3D::IDENTITY));
        assert!(nested.is_gpu_resident(), "residency propagates upward");
    }

    #[test]
    fn byte_size_counts_the_content_and_the_nesting() {
        let empty = Scene::new().byte_size();
        let with_image = Scene::new()
            .with_object(SceneObject::image(image(64, 64), Transform3D::IDENTITY))
            .byte_size();
        // 64 * 64 RgbaF32 pixels is 64 KiB of pixel bytes.
        assert!(
            with_image >= empty + 64 * 64 * 16,
            "an image object must account for its pixels: {with_image} vs {empty}"
        );

        let nested = Scene::new()
            .with_object(SceneObject::scene(
                Arc::new(
                    Scene::new()
                        .with_object(SceneObject::image(image(64, 64), Transform3D::IDENTITY)),
                ),
                Transform3D::IDENTITY,
            ))
            .byte_size();
        assert!(
            nested >= 64 * 64 * 16,
            "nested content must be accounted too: {nested}"
        );
    }

    /// A hostile `byte_size` must not overflow the accounting into a debug
    /// panic or a release wrap.
    #[test]
    fn byte_size_saturates_instead_of_overflowing() {
        struct HugeFrame;
        impl NodeData for HugeFrame {
            fn data_type_id(&self) -> DataTypeId {
                DataTypeId::FRAME_BUFFER
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn byte_size(&self) -> u64 {
                u64::MAX
            }
        }

        let huge = || {
            SceneObject::image(
                SceneImage::new(Arc::new(HugeFrame), 1, 1).expect("frame buffer"),
                Transform3D::IDENTITY,
            )
        };
        let scene = Scene::new().with_object(huge()).with_object(huge());
        assert_eq!(scene.byte_size(), u64::MAX);

        // And through a nesting level, which recurses into the same addition.
        let nested =
            Scene::new().with_object(SceneObject::scene(Arc::new(scene), Transform3D::IDENTITY));
        assert_eq!(nested.byte_size(), u64::MAX);
    }

    /// Structural sharing: appending to a scene of many objects clones the
    /// spine, not the content.
    #[test]
    fn adding_an_object_shares_the_existing_content() {
        let shared = geometry();
        let mut scene = Scene::new();
        for _ in 0..64 {
            scene = scene.with_object(SceneObject::geometry(
                Arc::clone(&shared),
                Transform3D::IDENTITY,
            ));
        }
        // 64 holders plus the local binding.
        assert_eq!(Arc::strong_count(&shared), 65);

        let extended = scene.with_object(SceneObject::geometry(
            Arc::clone(&shared),
            Transform3D::IDENTITY,
        ));
        assert_eq!(scene.object_count(), 64);
        assert_eq!(extended.object_count(), 65);
    }
}
