// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The [`Scene`] data type: what `scene.add` / `scene.merge` /
//! `scene.camera` build and `scene.render` consumes (REQ-3D-001).
//!
//! A scene is a list of [`SceneObject`]s and a list of [`Camera`]s. An object
//! is a piece of content — a [`Geometry`] or a **nested scene** — paired with
//! a [`Transform3D`]. Nesting is how a transform hierarchy is expressed: a
//! child scene handed to a parent's `scene.add` follows the parent's
//! transform, which is the Null / parenting idiom of C4D and After Effects.
//!
//! An image is not a content kind of its own: a frame buffer reaches a scene
//! as a [`Geometry`] whose instance source carries it, so one placement
//! mechanism serves both the 2D repeater and the 3D renderer
//! (`docs/implementation/done/image-instancing-plan.md`).
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
use crate::types::NodeData;
use std::sync::Arc;

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
// SceneObject
// ===========================================================================

/// What a scene object draws.
///
/// There is no image variant: a frame buffer becomes a [`Geometry`] that
/// carries it as an instance source, so the scene never has to know how a
/// picture is held (`docs/implementation/done/image-instancing-plan.md`).
#[derive(Clone, Debug)]
pub enum SceneContent {
    /// A geometry — `Primitive::Path` or `Primitive::Mesh`, with a `P` column
    /// of either dimension.
    Geometry(Arc<Geometry>),
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
/// a geometry plus one scene-space matrix, which is the form a renderer
/// wants.
///
/// The content is a [`Geometry`] outright rather than a one-variant enum,
/// because [`SceneContent`] minus the nesting case *is* a geometry: images
/// travel inside a geometry's instance sources, and meshes and paths are
/// primitive kinds within one container. A second drawable kind would have
/// to reintroduce the enum, and `scene.render` is unwritten, so nothing is
/// paying for that choice today.
#[derive(Clone, Debug)]
pub struct FlatObject {
    /// What is drawn — never a nested scene.
    pub geometry: Arc<Geometry>,
    /// Object space → scene space.
    pub world_transform: Mat4,
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
                    geometry: Arc::clone(geometry),
                    world_transform: world,
                }),
                SceneContent::Scene(nested) => nested.collect_flat(&world, out),
            }
        }
    }

    /// Whether any content in this scene, at any nesting depth, is a
    /// GPU-resident value.
    ///
    /// A `Geometry` reports `false` today — it holds attribute columns in
    /// host memory and does not override the flag. The recursion is what
    /// makes that answer follow the content once a geometry can carry a
    /// resident frame (`docs/implementation/done/image-instancing-plan.md`);
    /// nothing here needs to change then.
    fn holds_gpu_resident(&self) -> bool {
        self.objects.iter().any(|object| match &object.content {
            SceneContent::Geometry(geometry) => geometry.is_gpu_resident(),
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

    /// A scene reports GPU residency when any content it holds is resident,
    /// because that is what the flag documents: the value is then not wholly
    /// CPU-readable and cannot be persisted or inspected without a readback
    /// through the owning crate.
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
        // The additions saturate. Nothing reachable can overflow them today —
        // both arms bottom out in concrete `ravel-core` types, so no caller
        // can hand this an arbitrary estimate — but `NodeData::byte_size` is
        // a contract that returns whatever an implementation says, and
        // `IMG-2` puts `Arc<dyn NodeData>` images back under `Geometry`. A
        // debug panic or a release wrap would turn an overestimate into a
        // broken budget, while `u64::MAX` is a perfectly good answer for
        // "more than the budget will ever hold".
        let content = self.objects.iter().fold(0u64, |total, object| {
            let bytes = match &object.content {
                SceneContent::Geometry(geometry) => geometry.byte_size(),
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
    use crate::geometry::{InstanceImage, InstanceSource};
    use crate::types::Vec2;

    fn geometry() -> Arc<Geometry> {
        Arc::new(Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]))
    }

    /// A geometry of `points` points, so leaves and byte sizes can be told
    /// apart.
    fn geometry_of(points: usize) -> Arc<Geometry> {
        Arc::new(Geometry::from_points(vec![Vec2(0.0, 0.0); points]))
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
        let second = SceneObject::scene(
            Arc::new(
                Scene::new().with_object(SceneObject::geometry(geometry(), Transform3D::IDENTITY)),
            ),
            Transform3D::IDENTITY,
        );

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
        assert!(matches!(two.objects()[1].content, SceneContent::Scene(_)));
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
            .with_object(SceneObject::geometry(geometry(), Transform3D::IDENTITY))
            .with_object(SceneObject::geometry(geometry(), Transform3D::IDENTITY));

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

    /// Three levels of nesting where the transforms **do not commute**: the
    /// root rotates about Z, the middle rotates about X and scales
    /// non-uniformly, the leaf translates. Translation-only nesting commutes,
    /// so it cannot tell `parent · child` from `child · parent` or from any
    /// other regrouping — these do.
    ///
    /// Hand-derived, applying `root · (middle · leaf)` to a point:
    /// `(0,1,0)` → leaf `+(10,0,0)` = `(10,1,0)` → middle scale `(2,3,4)` =
    /// `(20,3,0)` → middle Rx 90° `(x,-z,y)` = `(20,0,3)` → root Rz 90°
    /// `(-y,x,z)` = `(0,20,3)`.
    #[test]
    fn nesting_goes_deeper_than_one_level() {
        let leaf = Arc::new(Scene::new().with_object(SceneObject::geometry(
            geometry(),
            Transform3D::from_translation([10.0, 0.0, 0.0]),
        )));
        let middle = Arc::new(Scene::new().with_object(SceneObject::scene(
            leaf,
            Transform3D {
                rotate: [90.0, 0.0, 0.0],
                scale: [2.0, 3.0, 4.0],
                ..Transform3D::IDENTITY
            },
        )));
        let root = Scene::new().with_object(SceneObject::scene(
            middle,
            Transform3D {
                rotate: [0.0, 0.0, 90.0],
                ..Transform3D::IDENTITY
            },
        ));

        let flat = root.flatten();
        assert_eq!(flat.len(), 1);
        let world = &flat[0].world_transform;

        // The leaf origin: (0,0,0) → (10,0,0) → (20,0,0) → (20,0,0) → (0,20,0).
        let origin = world.transform_point3([0.0, 0.0, 0.0]);
        assert_close(origin[0], 0.0, "origin x");
        assert_close(origin[1], 20.0, "origin y");
        assert_close(origin[2], 0.0, "origin z");

        // A point off the axis, which is what separates the orderings: the
        // middle X rotation has to move the leaf's y into z *before* the root
        // Z rotation acts.
        let offset = world.transform_point3([0.0, 1.0, 0.0]);
        assert_close(offset[0], 0.0, "offset x");
        assert_close(offset[1], 20.0, "offset y");
        assert_close(offset[2], 3.0, "offset z");

        // And a third point, so a coincidental agreement on two cannot pass.
        // (0,0,1) → (10,0,1) → (20,0,4) → (20,-4,0) → (4,20,0).
        let depth = world.transform_point3([0.0, 0.0, 1.0]);
        assert_close(depth[0], 4.0, "depth x");
        assert_close(depth[1], 20.0, "depth y");
        assert_close(depth[2], 0.0, "depth z");

        // The composition is exactly root · middle · leaf, and the reversed
        // grouping is a different matrix — so the assertions above are not
        // accidentally order-insensitive.
        let reversed = Transform3D::from_translation([10.0, 0.0, 0.0])
            .to_matrix()
            .mul(
                &Transform3D {
                    rotate: [90.0, 0.0, 0.0],
                    scale: [2.0, 3.0, 4.0],
                    ..Transform3D::IDENTITY
                }
                .to_matrix(),
            )
            .mul(
                &Transform3D {
                    rotate: [0.0, 0.0, 90.0],
                    ..Transform3D::IDENTITY
                }
                .to_matrix(),
            );
        assert_ne!(*world, reversed, "child-first composition must differ");
    }

    /// A value reused at two places in the nesting tree is counted once per
    /// placement, which is the convention that makes the accounting an
    /// over-estimate rather than an under-estimate — and the reason the sum
    /// saturates instead of wrapping.
    #[test]
    fn byte_size_counts_a_shared_value_once_per_holder() {
        let shared = Arc::new(Scene::new().with_object(SceneObject::geometry(
            geometry_of(4096),
            Transform3D::IDENTITY,
        )));
        let once = Scene::new()
            .with_object(SceneObject::scene(
                Arc::clone(&shared),
                Transform3D::IDENTITY,
            ))
            .byte_size();
        let twice = Scene::new()
            .with_object(SceneObject::scene(
                Arc::clone(&shared),
                Transform3D::IDENTITY,
            ))
            .with_object(SceneObject::scene(shared, Transform3D::IDENTITY))
            .byte_size();
        assert!(
            twice >= once + 4096 * 12,
            "the second placement must be charged too: {twice} vs {once}"
        );
    }

    #[test]
    fn flatten_visits_every_leaf_in_depth_first_order() {
        let nested = Arc::new(
            Scene::new()
                .with_object(SceneObject::geometry(geometry_of(2), Transform3D::IDENTITY))
                .with_object(SceneObject::geometry(geometry_of(3), Transform3D::IDENTITY)),
        );
        let scene = Scene::new()
            .with_object(SceneObject::geometry(geometry_of(1), Transform3D::IDENTITY))
            .with_object(SceneObject::scene(nested, Transform3D::IDENTITY))
            .with_object(SceneObject::geometry(geometry_of(4), Transform3D::IDENTITY));

        let flat = scene.flatten();
        let visited: Vec<usize> = flat
            .iter()
            .map(|leaf| leaf.geometry.point_count())
            .collect();
        assert_eq!(visited, vec![1, 2, 3, 4], "depth-first in insertion order");
    }

    #[test]
    fn a_nested_scene_camera_is_not_promoted_to_the_parent() {
        let child = Arc::new(Scene::from_camera(Camera::default()));
        let parent = Scene::new().with_object(SceneObject::scene(child, Transform3D::IDENTITY));
        assert_eq!(parent.camera_count(), 0);
        assert!(parent.primary_camera().is_none());
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
    /// memory. Every content kind reachable today is CPU-resident: a
    /// `Geometry` holds host columns, and a nested scene can only hold more
    /// of the same. Residency becomes reachable again when a geometry can
    /// carry a resident frame
    /// (`docs/implementation/done/image-instancing-plan.md`), which is why
    /// `holds_gpu_resident` still recurses.
    #[test]
    fn a_cpu_scene_is_not_gpu_resident() {
        let scene = Scene::new()
            .with_object(SceneObject::geometry(geometry(), Transform3D::IDENTITY))
            .with_object(SceneObject::scene(
                Arc::new(
                    Scene::new()
                        .with_object(SceneObject::geometry(geometry(), Transform3D::IDENTITY)),
                ),
                Transform3D::IDENTITY,
            ));
        assert!(!scene.is_gpu_resident());
    }

    /// A geometry that stamps a GPU-resident image makes every scene holding
    /// it resident, nesting included.
    ///
    /// This is what keeps [`Scene::holds_gpu_resident`]'s recursion honest:
    /// before a geometry could carry a frame there was no reachable value that
    /// made it answer `true`, so flattening it to a constant `false` would
    /// have gone unnoticed.
    #[test]
    fn a_resident_image_in_a_geometry_makes_the_whole_scene_gpu_resident() {
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

        let mut carrier = Geometry::new();
        carrier.set_sources(vec![InstanceSource::Image(
            InstanceImage::new(Arc::new(ResidentFrame), 32, 32).expect("frame buffer"),
        )]);
        let carrier = Arc::new(carrier);

        let scene = Scene::new().with_object(SceneObject::geometry(
            Arc::clone(&carrier),
            Transform3D::IDENTITY,
        ));
        assert!(scene.is_gpu_resident());

        let nested =
            Scene::new().with_object(SceneObject::scene(Arc::new(scene), Transform3D::IDENTITY));
        assert!(nested.is_gpu_resident(), "residency propagates upward");

        // A scene of the same shape without the image stays CPU-resident, so
        // the assertions above are not passing on the nesting alone.
        let plain = Scene::new().with_object(SceneObject::scene(
            Arc::new(
                Scene::new().with_object(SceneObject::geometry(geometry(), Transform3D::IDENTITY)),
            ),
            Transform3D::IDENTITY,
        ));
        assert!(!plain.is_gpu_resident());
    }

    /// A `byte_size` an image source is free to invent must not overflow the
    /// scene's accounting either.
    #[test]
    fn byte_size_saturates_through_a_geometry_that_holds_a_huge_image() {
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

        let mut carrier = Geometry::new();
        carrier.set_sources(vec![InstanceSource::Image(
            InstanceImage::new(Arc::new(HugeFrame), 1, 1).expect("frame buffer"),
        )]);
        // The second object is deliberately small: two `u64::MAX` objects wrap
        // back to `u64::MAX - 1` and then saturate in the outer sum, which
        // would let a wrapping fold pass unnoticed.
        let scene = Scene::new()
            .with_object(SceneObject::geometry(
                Arc::new(carrier),
                Transform3D::IDENTITY,
            ))
            .with_object(SceneObject::geometry(
                geometry_of(64),
                Transform3D::IDENTITY,
            ));
        assert_eq!(scene.byte_size(), u64::MAX);

        let nested =
            Scene::new().with_object(SceneObject::scene(Arc::new(scene), Transform3D::IDENTITY));
        assert_eq!(nested.byte_size(), u64::MAX);
    }

    #[test]
    fn byte_size_counts_the_content_and_the_nesting() {
        let empty = Scene::new().byte_size();
        let with_geometry = Scene::new()
            .with_object(SceneObject::geometry(
                geometry_of(4096),
                Transform3D::IDENTITY,
            ))
            .byte_size();
        // `P` (Vec2) plus `index` (i32) per point, at minimum.
        assert!(
            with_geometry >= empty + 4096 * 12,
            "a geometry object must account for its columns: {with_geometry} vs {empty}"
        );

        let nested = Scene::new()
            .with_object(SceneObject::scene(
                Arc::new(Scene::new().with_object(SceneObject::geometry(
                    geometry_of(4096),
                    Transform3D::IDENTITY,
                ))),
                Transform3D::IDENTITY,
            ))
            .byte_size();
        assert!(
            nested >= 4096 * 12,
            "nested content must be accounted too: {nested}"
        );
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
