// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Tests of `scene.render`.
//!
//! Expected pixel positions are **computed by hand** from the camera set-up in
//! each test's comment, never from the renderer's own matrices: a projection
//! with a flipped axis or a wrong depth range draws a consistent but wrong
//! picture, and only an independent expectation sees it.

use std::sync::{Arc, Mutex};

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{
    AttributeArray, Geometry, InstanceImage, InstanceSource, Primitive, names,
};
use ravel_core::graph::Node;
use ravel_core::id::NodeId;
use ravel_core::scene::{Camera, Projection, Scene, SceneObject, Transform3D};
use ravel_core::types::{Color, FrameBuffer, FrameRate, NodeData, Vec2, Vec3};
use ravel_gpu::{GpuContext, GpuFrameBuffer, ShaderManager, TexturePool};

use super::prepare::{Blended, SceneParams, prepare};
use super::{SceneRenderProcessor, cpu};

pub(super) const RED: Color = Color::new(1.0, 0.0, 0.0, 1.0);
pub(super) const GREEN: Color = Color::new(0.0, 1.0, 0.0, 1.0);
pub(super) const BLUE: Color = Color::new(0.0, 0.0, 1.0, 1.0);

pub(super) fn ctx(width: u32, height: u32) -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), (width, height))
}

/// An orthographic camera under which scene `(x, y)` **is** pixel `(x, y)` of a
/// `width` x `height` canvas, and `z` is the distance from the camera minus
/// 100 (so `z = 0` is 100 units away, clip depth `(100 - 1) / 999`).
///
/// By hand: the camera looks down `+Z` with image-up `-Y`, so view `y` is the
/// scene `y` measured from the camera's, and a half-height of `height / 2`
/// maps it onto `[-1, 1]` exactly.
pub(super) fn pixel_camera(width: u32, height: u32) -> Camera {
    let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
    Camera::looking_at([cx, cy, -100.0], [cx, cy, 0.0])
        .with_projection(Projection::Orthographic {
            height: height as f32,
        })
        .with_clip_range(1.0, 1000.0)
}

/// A mesh quad (two triangles) through `corners`, coloured by a `Cd` column.
fn mesh(corners: [[f32; 3]; 4], color: Color) -> Geometry {
    let mut geometry =
        Geometry::from_points3(corners.iter().map(|c| Vec3(c[0], c[1], c[2])).collect());
    geometry
        .points_mut()
        .insert(names::CD, AttributeArray::Color(vec![color; 4]))
        .unwrap();
    geometry.push_mesh(0..4, &[0, 1, 2, 0, 2, 3]);
    geometry
}

/// An axis-aligned quad `[x0, x1] x [y0, y1]` at depth `z`.
pub(super) fn flat_quad(x0: f32, y0: f32, x1: f32, y1: f32, z: f32, color: Color) -> Geometry {
    mesh([[x0, y0, z], [x1, y0, z], [x1, y1, z], [x0, y1, z]], color)
}

pub(super) fn scene_of(camera: Camera, objects: Vec<Geometry>) -> Scene {
    objects
        .into_iter()
        .fold(Scene::from_camera(camera), |scene, geometry| {
            scene.with_object(SceneObject::geometry(
                Arc::new(geometry),
                Transform3D::IDENTITY,
            ))
        })
}

pub(super) fn node() -> Node {
    Node::new(NodeId::new(1), "scene.render")
}

fn run(processor: &SceneRenderProcessor, scene: &Scene, ctx: &EvalContext) -> Arc<dyn NodeData> {
    let input: Arc<dyn NodeData> = Arc::new(scene.clone());
    processor
        .process(
            &node(),
            ctx,
            &[Some(input)],
            &ResolvedParams::default(),
            &mut Evaluator::new(),
        )
        .expect("scene.render succeeds")
}

fn frame(out: &Arc<dyn NodeData>) -> FrameBuffer {
    if let Some(gpu) = out.downcast_ref::<GpuFrameBuffer>() {
        return gpu.to_frame_buffer().expect("GPU readback");
    }
    out.downcast_ref::<FrameBuffer>()
        .expect("a frame buffer")
        .clone()
}

pub(super) fn cpu_frame(scene: &Scene, width: u32, height: u32) -> FrameBuffer {
    frame(&run(
        &SceneRenderProcessor::from_node(&node()),
        scene,
        &ctx(width, height),
    ))
}

pub(super) struct Rig {
    gpu: GpuContext,
    pool: Arc<Mutex<TexturePool>>,
}

impl Rig {
    pub(super) fn new() -> Self {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 128 * 1024 * 1024)));
        Self { gpu, pool }
    }

    fn processor(&self) -> SceneRenderProcessor {
        let mut shaders = ShaderManager::new(self.gpu.clone());
        SceneRenderProcessor::new(self.gpu.clone(), &mut shaders, self.pool.clone(), &node())
    }

    pub(super) fn frame(&self, scene: &Scene, width: u32, height: u32) -> FrameBuffer {
        frame(&run(&self.processor(), scene, &ctx(width, height)))
    }
}

pub(super) fn px(frame: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
    let index = ((y * frame.width + x) * 4) as usize;
    frame.as_f32()[index..index + 4].try_into().unwrap()
}

pub(super) fn assert_color(actual: [f32; 4], expected: Color, tolerance: f32, label: &str) {
    let expected = [expected.r, expected.g, expected.b, expected.a];
    for channel in 0..4 {
        assert!(
            (actual[channel] - expected[channel]).abs() <= tolerance,
            "{label}: expected {expected:?}, got {actual:?}"
        );
    }
}

pub(super) fn covered(frame: &FrameBuffer) -> usize {
    frame
        .as_f32()
        .chunks_exact(4)
        .filter(|p| p[3] > 0.5)
        .count()
}

/// Per-pixel agreement between two frames: the fraction within `tolerance` on
/// every channel, and the largest channel difference seen.
pub(super) fn agreement(a: &FrameBuffer, b: &FrameBuffer, tolerance: f32) -> (f32, f32) {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mut within = 0usize;
    let mut worst = 0.0f32;
    for (p, q) in a.as_f32().chunks_exact(4).zip(b.as_f32().chunks_exact(4)) {
        let diff = p
            .iter()
            .zip(q)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max);
        worst = worst.max(diff);
        if diff <= tolerance {
            within += 1;
        }
    }
    (within as f32 / (a.width * a.height) as f32, worst)
}

// ---------------------------------------------------------------------------
// prepare: classification, ordering, identity
// ---------------------------------------------------------------------------

pub(super) fn prepare_default(scene: &Scene, width: u32, height: u32) -> super::prepare::Prepared {
    let camera = scene.primary_camera().copied().unwrap_or_default();
    prepare(
        scene,
        &camera,
        &ctx(width, height),
        &SceneParams {
            fill: true,
            stroke_width: 0.0,
            color: Color::WHITE,
        },
    )
    .expect("prepare succeeds")
}

#[test]
fn an_opaque_mesh_goes_to_the_first_pass_and_a_translucent_one_to_the_second() {
    let scene = scene_of(
        pixel_camera(64, 64),
        vec![
            flat_quad(0.0, 0.0, 8.0, 8.0, 0.0, RED),
            flat_quad(0.0, 0.0, 8.0, 8.0, 0.0, Color::new(0.0, 1.0, 0.0, 0.5)),
        ],
    );
    let prepared = prepare_default(&scene, 64, 64);
    assert_eq!(prepared.opaque, vec![0], "the opaque quad is pass 1");
    assert_eq!(
        prepared.blended,
        vec![Blended::Mesh(1)],
        "the half-alpha quad is pass 2"
    );
}

#[test]
fn the_second_pass_is_ordered_back_to_front_by_view_space_depth() {
    // Camera at z = -100 looking along +Z: a larger scene z is farther. The
    // quads are added near, far, middle, so neither insertion order nor the
    // reverse of it is the answer.
    let translucent = |z| flat_quad(0.0, 0.0, 8.0, 8.0, z, Color::new(1.0, 1.0, 1.0, 0.5));
    let scene = scene_of(
        pixel_camera(64, 64),
        vec![translucent(-10.0), translucent(10.0), translucent(0.0)],
    );
    let prepared = prepare_default(&scene, 64, 64);
    assert_eq!(
        prepared.blended,
        vec![Blended::Mesh(1), Blended::Mesh(2), Blended::Mesh(0)],
        "far (z = +10) first, near (z = -10) last"
    );
}

#[test]
fn the_sort_follows_the_camera_not_the_scene_z_axis() {
    // The same three quads seen from the other side: now the camera is at
    // z = +100 looking along -Z, so the quad at z = -10 is the *farthest*.
    let translucent = |z| flat_quad(0.0, 0.0, 8.0, 8.0, z, Color::new(1.0, 1.0, 1.0, 0.5));
    let camera = Camera::looking_at([4.0, 4.0, 100.0], [4.0, 4.0, 0.0])
        .with_projection(Projection::Orthographic { height: 64.0 })
        .with_clip_range(1.0, 1000.0);
    let scene = scene_of(
        camera,
        vec![translucent(-10.0), translucent(10.0), translucent(0.0)],
    );
    let prepared = prepare_default(&scene, 64, 64);
    assert_eq!(
        prepared.blended,
        vec![Blended::Mesh(0), Blended::Mesh(2), Blended::Mesh(1)],
        "seen from +Z the quad at z = -10 is farthest"
    );
}

#[test]
fn equal_centroid_depth_is_broken_by_the_farthest_corner() {
    // Both quads have centroid z = 0. The first is flat; the second tilts so
    // one corner is at z = +20 (and the opposite at -20): it reaches farther,
    // so it is drawn first even though it was added second.
    let translucent = Color::new(1.0, 1.0, 1.0, 0.5);
    let flat = flat_quad(0.0, 0.0, 8.0, 8.0, 0.0, translucent);
    let tilted = mesh(
        [
            [0.0, 0.0, 20.0],
            [8.0, 0.0, 20.0],
            [8.0, 8.0, -20.0],
            [0.0, 8.0, -20.0],
        ],
        translucent,
    );
    let prepared = prepare_default(&scene_of(pixel_camera(64, 64), vec![flat, tilted]), 64, 64);
    assert_eq!(prepared.blended, vec![Blended::Mesh(1), Blended::Mesh(0)]);
}

#[test]
fn a_mesh_key_survives_a_transform_change_and_not_a_column_edit() {
    let geometry = Arc::new(flat_quad(0.0, 0.0, 8.0, 8.0, 0.0, RED));
    let camera = pixel_camera(64, 64);
    let at = |x: f32| {
        Scene::from_camera(camera).with_object(SceneObject::geometry(
            Arc::clone(&geometry),
            Transform3D::from_translation([x, 0.0, 0.0]),
        ))
    };
    let first = prepare_default(&at(0.0), 64, 64);
    let second = prepare_default(&at(20.0), 64, 64);
    assert_eq!(
        first.meshes[0].key, second.meshes[0].key,
        "moving an object keeps its static data"
    );
    assert_ne!(
        first.meshes[0].mvp, second.meshes[0].mvp,
        "the matrix is what changed"
    );

    let mut edited = (*geometry).clone();
    if let AttributeArray::Vec3(positions) = edited.points_mut().make_mut(names::P).unwrap() {
        positions[0].0 += 1.0;
    }
    let third = prepare_default(
        &Scene::from_camera(camera).with_object(SceneObject::geometry(
            Arc::new(edited),
            Transform3D::IDENTITY,
        )),
        64,
        64,
    );
    assert_ne!(
        first.meshes[0].key, third.meshes[0].key,
        "an edited column is new static data"
    );
}

#[test]
fn an_image_instance_is_a_textured_unit_quad_scaled_to_the_image() {
    let image = FrameBuffer::from_f32(8, 6, vec![1.0; 8 * 6 * 4]);
    let geometry = image_geometry(&image, &[Vec2(20.0, 15.0)]);
    let scene = Scene::from_camera(pixel_camera(64, 64)).with_object(SceneObject::geometry(
        Arc::new(geometry),
        Transform3D::IDENTITY,
    ));
    let prepared = prepare_default(&scene, 64, 64);
    assert_eq!(prepared.meshes.len(), 1);
    let object = &prepared.meshes[0];
    assert!(object.texture.is_some());
    // The unit quad's corners land on the 8 x 6 rectangle centred on (20, 15):
    // x in [16, 24], y in [12, 18].
    let model = &object.model;
    assert_eq!(model.transform_point3([-0.5, -0.5, 0.0]), [16.0, 12.0, 0.0]);
    assert_eq!(model.transform_point3([0.5, 0.5, 0.0]), [24.0, 18.0, 0.0]);
}

#[test]
fn a_scene_without_a_camera_is_drawn_through_the_default_one() {
    // Looking at the origin from z = -1000 with a 50 degree lens, a 100-unit
    // quad at the origin is visible: this must neither fail nor draw nothing.
    let scene = scene_of_without_camera(vec![flat_quad(-100.0, -100.0, 100.0, 100.0, 0.0, RED)]);
    let out = cpu_frame(&scene, 64, 64);
    assert_color(
        px(&out, 32, 32),
        RED,
        1e-6,
        "the centre of the default view",
    );
}

pub(super) fn scene_of_without_camera(objects: Vec<Geometry>) -> Scene {
    objects.into_iter().fold(Scene::new(), |scene, geometry| {
        scene.with_object(SceneObject::geometry(
            Arc::new(geometry),
            Transform3D::IDENTITY,
        ))
    })
}

#[test]
fn a_non_scene_input_is_an_error() {
    let processor = SceneRenderProcessor::from_node(&node());
    let geometry: Arc<dyn NodeData> = Arc::new(Geometry::new());
    let result = processor.process(
        &node(),
        &ctx(8, 8),
        &[Some(geometry)],
        &ResolvedParams::default(),
        &mut Evaluator::new(),
    );
    let message = format!("{:#}", result.err().expect("a geometry is not a scene"));
    assert!(message.contains("Scene"), "{message}");
}

// ---------------------------------------------------------------------------
// What is drawn where (CPU reference, hand-computed)
// ---------------------------------------------------------------------------

/// A right triangle `(10,10) (30,10) (10,30)` under the pixel camera covers
/// exactly the pixels whose centre lies inside it. The count is derived here
/// from the half-plane `x + y < 40` on the centre `(i + 0.5, j + 0.5)`, with
/// the hypotenuse offset by a quarter pixel (`40.25`) so no centre sits on an
/// edge and the fill rule cannot matter.
#[test]
fn a_triangle_covers_exactly_the_pixels_whose_centres_it_contains() {
    let mut geometry = Geometry::from_points3(vec![
        Vec3(10.0, 10.0, 0.0),
        Vec3(30.25, 10.0, 0.0),
        Vec3(10.0, 30.25, 0.0),
    ]);
    geometry
        .points_mut()
        .insert(names::CD, AttributeArray::Color(vec![RED; 3]))
        .unwrap();
    geometry.push_mesh(0..3, &[0, 1, 2]);
    let scene = scene_of(pixel_camera(48, 48), vec![geometry]);
    let out = cpu_frame(&scene, 48, 48);

    let mut expected = 0;
    for j in 0..48 {
        for i in 0..48 {
            let (x, y) = (i as f32 + 0.5, j as f32 + 0.5);
            let inside = x > 10.0 && y > 10.0 && x + y < 40.5;
            assert_eq!(px(&out, i, j)[3] > 0.5, inside, "pixel ({i}, {j})");
            expected += usize::from(inside);
        }
    }
    assert_eq!(covered(&out), expected);
    assert_color(px(&out, 12, 12), RED, 1e-6, "inside");
}

#[test]
fn the_image_is_not_mirrored_and_scene_y_grows_downwards() {
    // A quad whose top-left corner is the scene origin: scene +Y is image down,
    // so it occupies the top-left of the frame, not the bottom-left.
    let scene = scene_of(
        pixel_camera(32, 32),
        vec![flat_quad(0.0, 0.0, 8.0, 4.0, 0.0, RED)],
    );
    let out = cpu_frame(&scene, 32, 32);
    assert_eq!(covered(&out), 32);
    assert_color(px(&out, 0, 0), RED, 1e-6, "top-left");
    assert_color(px(&out, 7, 3), RED, 1e-6, "the far corner of the quad");
    assert_eq!(px(&out, 8, 0)[3], 0.0, "right of the quad");
    assert_eq!(px(&out, 0, 4)[3], 0.0, "below the quad");
    assert_eq!(px(&out, 0, 31)[3], 0.0, "the bottom-left stays empty");
}

/// The default camera sits at `(0, 0, -1000)` looking at the origin with a
/// 50 degree vertical lens, so in a square canvas of side 200 a point at
/// `(x, y, 0)` lands at pixel `100 + x * k`, `100 + y * k` with
/// `k = 100 / (1000 * tan 25 degrees) = 0.2144507`.
///
/// A 200-unit square (`+-100`) therefore spans `[78.55, 121.45]` on both
/// axes: pixel 78 (centre 78.5) is outside, 79 inside, 120 inside, 121
/// (centre 121.5) outside.
#[test]
fn a_perspective_quad_lands_where_the_lens_puts_it() {
    let scene = scene_of_without_camera(vec![flat_quad(-100.0, -100.0, 100.0, 100.0, 0.0, RED)]);
    let out = cpu_frame(&scene, 200, 200);
    assert_edges(&out);
}

fn assert_edges(out: &FrameBuffer) {
    for (x, y, inside) in [
        (79, 100, true),
        (120, 100, true),
        (78, 100, false),
        (121, 100, false),
        (100, 79, true),
        (100, 120, true),
        (100, 78, false),
        (100, 121, false),
    ] {
        assert_eq!(px(out, x, y)[3] > 0.5, inside, "pixel ({x}, {y})");
    }
    // 42 pixels per side (79..=120), by hand.
    assert_eq!(covered(out), 42 * 42);
}

#[test]
fn a_nearer_quad_is_larger_under_perspective() {
    // 1/z scaling: the same quad 500 units nearer (z = -500, so 500 away
    // instead of 1000) is twice the size, k = 0.4289014, spanning
    // 100 +- 42.89 = [57.11, 142.89].
    let scene = scene_of_without_camera(vec![flat_quad(-100.0, -100.0, 100.0, 100.0, -500.0, RED)]);
    let out = cpu_frame(&scene, 200, 200);
    // Pixel 57 has its centre (57.5) inside, pixel 56 (56.5) outside; likewise
    // 142 (142.5) inside and 143 (143.5) outside.
    assert_eq!(px(&out, 56, 100)[3], 0.0);
    assert!(px(&out, 57, 100)[3] > 0.5);
    assert!(px(&out, 142, 100)[3] > 0.5);
    assert_eq!(px(&out, 143, 100)[3], 0.0);
}

#[test]
fn a_mesh_crossing_the_near_plane_is_clipped_not_dropped() {
    // A floor quad running from far in front of the camera to far behind it.
    // Its near edge is clipped at the near plane; the visible part must still
    // fill the lower half of the view rather than vanishing or smearing.
    let floor = mesh(
        [
            [-500.0, 200.0, -3000.0],
            [500.0, 200.0, -3000.0],
            [500.0, 200.0, 5000.0],
            [-500.0, 200.0, 5000.0],
        ],
        GREEN,
    );
    let out = cpu_frame(&scene_of_without_camera(vec![floor]), 100, 100);
    assert!(covered(&out) > 500, "the clipped floor is drawn");
    assert_eq!(
        px(&out, 50, 5)[3],
        0.0,
        "nothing above the horizon of the floor"
    );
    assert_color(px(&out, 50, 95), GREEN, 1e-6, "the floor in front");
}

// ---------------------------------------------------------------------------
// Self-occlusion
// ---------------------------------------------------------------------------

/// Two opaque quads cross: `A` is flat at z = 0 (red), `B` tilts from z = -20 at
/// its left edge to z = +20 at its right (green). They are equally deep at the
/// middle (x = 30), so left of it `B` is nearer and right of it `A` is.
fn crossing_pair() -> (Geometry, Geometry) {
    let a = flat_quad(10.0, 10.0, 50.0, 30.0, 0.0, RED);
    let b = mesh(
        [
            [10.0, 10.0, -20.0],
            [50.0, 10.0, 20.0],
            [50.0, 30.0, 20.0],
            [10.0, 30.0, -20.0],
        ],
        GREEN,
    );
    (a, b)
}

fn assert_crossing(out: &FrameBuffer) {
    assert_color(
        px(out, 15, 20),
        GREEN,
        1e-3,
        "left of the crossing: B is nearer",
    );
    assert_color(
        px(out, 45, 20),
        RED,
        1e-3,
        "right of the crossing: A is nearer",
    );
}

#[test]
fn overlapping_opaque_meshes_occlude_by_depth_in_either_draw_order() {
    let (a, b) = crossing_pair();
    for (label, objects) in [
        ("A then B", vec![a.clone(), b.clone()]),
        ("B then A", vec![b, a]),
    ] {
        let scene = scene_of(pixel_camera(64, 40), objects);
        let out = cpu_frame(&scene, 64, 40);
        assert!(!label.is_empty());
        assert_crossing(&out);
    }
}

#[test]
fn gpu_overlapping_opaque_meshes_occlude_by_depth_in_either_draw_order() {
    let rig = Rig::new();
    let (a, b) = crossing_pair();
    for objects in [vec![a.clone(), b.clone()], vec![b, a]] {
        let out = rig.frame(&scene_of(pixel_camera(64, 40), objects), 64, 40);
        assert_crossing(&out);
    }
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

/// Geometry stamping `image` at each of `positions` (composition units).
fn image_geometry(image: &FrameBuffer, positions: &[Vec2]) -> Geometry {
    let frame: Arc<dyn NodeData> = Arc::new(image.clone());
    let source = InstanceSource::Image(
        InstanceImage::new(frame, image.width, image.height).expect("a frame buffer"),
    );
    let mut geometry = Geometry::new();
    geometry
        .instances_mut()
        .insert(names::P, AttributeArray::Vec2(positions.to_vec()))
        .unwrap();
    geometry.set_sources(vec![source]);
    geometry
}

/// An 8 x 6 image whose every pixel is distinct, so a shifted, mirrored or
/// stretched copy cannot match by accident.
fn gradient_image() -> FrameBuffer {
    let mut pixels = Vec::new();
    for y in 0..6 {
        for x in 0..8 {
            pixels.extend_from_slice(&[x as f32 / 7.0, y as f32 / 5.0, 0.25, 1.0]);
        }
    }
    FrameBuffer::from_f32(8, 6, pixels)
}

fn assert_copy(out: &FrameBuffer, image: &FrameBuffer, left: u32, top: u32, tolerance: f32) {
    for y in 0..image.height {
        for x in 0..image.width {
            let expected = px(image, x, y);
            let actual = px(out, left + x, top + y);
            for channel in 0..4 {
                assert!(
                    (actual[channel] - expected[channel]).abs() <= tolerance,
                    "texel ({x}, {y}) channel {channel}: expected {expected:?}, got {actual:?}"
                );
            }
        }
    }
}

/// An unscaled image seen front-on through the pixel camera is the image,
/// texel for texel, in exactly `width x height` pixels.
fn image_scene(image: &FrameBuffer) -> Scene {
    scene_of(
        pixel_camera(32, 24),
        vec![image_geometry(image, &[Vec2(16.0, 12.0)])],
    )
}

#[test]
fn a_front_facing_image_instance_reproduces_the_source_on_the_cpu() {
    let image = gradient_image();
    let out = cpu_frame(&image_scene(&image), 32, 24);
    // Centred on (16, 12): x in [12, 20), y in [9, 15).
    assert_copy(&out, &image, 12, 9, 1e-5);
    assert_eq!(
        covered(&out),
        8 * 6,
        "the aspect ratio is the image's: 8 x 6, nothing more"
    );
    assert_eq!(px(&out, 11, 10)[3], 0.0);
    assert_eq!(px(&out, 20, 10)[3], 0.0);
    assert_eq!(px(&out, 14, 8)[3], 0.0);
    assert_eq!(px(&out, 14, 15)[3], 0.0);
}

#[test]
fn a_front_facing_image_instance_reproduces_the_source_on_the_gpu() {
    let image = gradient_image();
    let out = Rig::new().frame(&image_scene(&image), 32, 24);
    // The attachment is half-float, so allow its rounding.
    assert_copy(&out, &image, 12, 9, 2e-3);
    assert_eq!(covered(&out), 8 * 6);
}

/// Four colours in a 2 x 2 texture: red, green / blue, white.
fn quad_texture() -> FrameBuffer {
    FrameBuffer::from_f32(
        2,
        2,
        vec![
            1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, //
            0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
        ],
    )
}

/// A 16 x 16 mesh quad at `(8, 8)` whose `uv` column is `uvs`, textured with
/// [`quad_texture`] (a mesh reaches a texture through its own `uv` and nothing
/// else — the rectangle has no implicit coordinates).
fn textured_prepared(uvs: [Vec2; 4]) -> super::prepare::Prepared {
    let mut geometry = flat_quad(8.0, 8.0, 24.0, 24.0, 0.0, Color::WHITE);
    geometry
        .points_mut()
        .insert(names::UV, AttributeArray::Vec2(uvs.to_vec()))
        .unwrap();
    let scene = scene_of(pixel_camera(32, 32), vec![geometry]);
    let mut prepared = prepare_default(&scene, 32, 32);
    prepared.meshes[0].texture = Some(Arc::new(quad_texture()));
    prepared
}

const NATURAL_UV: [Vec2; 4] = [
    Vec2(0.0, 0.0),
    Vec2(1.0, 0.0),
    Vec2(1.0, 1.0),
    Vec2(0.0, 1.0),
];
/// The same quad with `u` mirrored.
const MIRRORED_UV: [Vec2; 4] = [
    Vec2(1.0, 0.0),
    Vec2(0.0, 0.0),
    Vec2(0.0, 1.0),
    Vec2(1.0, 1.0),
];

/// One probe near each corner of the 16 x 16 quad, with the colour it must show
/// under `NATURAL_UV`. A 2 x 2 texture magnified 8x is flat (the edge texel
/// repeats) outside the band within half a texel of the seam, so a probe a few
/// pixels in from a corner reads one texel's colour exactly.
fn assert_quadrants(out: &FrameBuffer, expected: [Color; 4], tolerance: f32) {
    let probes = [(9, 9), (22, 9), (9, 22), (22, 22)];
    for (index, (x, y)) in probes.into_iter().enumerate() {
        assert_color(
            px(out, x, y),
            expected[index],
            tolerance,
            &format!("quadrant {index}"),
        );
    }
}

#[test]
fn the_sampler_reads_the_uv_attribute_on_the_cpu() {
    let natural = cpu::render(&textured_prepared(NATURAL_UV)).unwrap();
    assert_quadrants(&natural, [RED, GREEN, BLUE, Color::WHITE], 1e-5);
    let mirrored = cpu::render(&textured_prepared(MIRRORED_UV)).unwrap();
    assert_quadrants(&mirrored, [GREEN, RED, Color::WHITE, BLUE], 1e-5);
}

#[test]
fn the_sampler_reads_the_uv_attribute_on_the_gpu() {
    let rig = Rig::new();
    let renderer = rig.processor();
    let gpu = renderer.gpu.as_ref().expect("GPU processor");
    let render = |prepared| frame(&gpu.render(&prepared).unwrap());
    assert_quadrants(
        &render(textured_prepared(NATURAL_UV)),
        [RED, GREEN, BLUE, Color::WHITE],
        2e-3,
    );
    assert_quadrants(
        &render(textured_prepared(MIRRORED_UV)),
        [GREEN, RED, Color::WHITE, BLUE],
        2e-3,
    );
}

#[test]
fn a_changed_image_is_drawn_the_next_frame_on_the_gpu() {
    let rig = Rig::new();
    let processor = rig.processor();
    let solid = |color: [f32; 4]| FrameBuffer::from_f32(8, 6, color.repeat(8 * 6));
    for (index, color) in [
        [1.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
    ]
    .into_iter()
    .enumerate()
    {
        let scene = image_scene(&solid(color));
        let out = frame(&run(&processor, &scene, &ctx(32, 24)));
        let expected = Color::new(color[0], color[1], color[2], color[3]);
        assert_color(px(&out, 15, 11), expected, 2e-3, &format!("frame {index}"));
    }
}

#[test]
fn a_gpu_resident_image_is_drawn_without_a_readback() {
    let rig = Rig::new();
    // The image is the output of an earlier GPU draw: resident, never on the
    // CPU. Drawing it must not read it back.
    let source = rig.processor();
    let red = frame_resident(&rig, &source, RED);
    let geometry = {
        let image = InstanceImage::new(Arc::clone(&red), 16, 16).unwrap();
        let mut geometry = Geometry::new();
        geometry
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(16.0, 12.0)]))
            .unwrap();
        geometry.set_sources(vec![InstanceSource::Image(image)]);
        geometry
    };
    let scene = scene_of(pixel_camera(32, 24), vec![geometry]);
    let before = rig.gpu.transfer_stats();
    let out = run(&rig.processor(), &scene, &ctx(32, 24));
    assert_eq!(before.delta(&rig.gpu.transfer_stats()).readbacks, 0);
    assert_color(
        px(&frame(&out), 16, 12),
        RED,
        2e-3,
        "the resident texture was sampled",
    );
}

/// A resident 16 x 16 frame of `color`, made by rendering a scene on the GPU.
fn frame_resident(rig: &Rig, processor: &SceneRenderProcessor, color: Color) -> Arc<dyn NodeData> {
    let scene = scene_of(
        pixel_camera(16, 16),
        vec![flat_quad(0.0, 0.0, 16.0, 16.0, 0.0, color)],
    );
    let out = run(processor, &scene, &ctx(16, 16));
    assert!(out.downcast_ref::<GpuFrameBuffer>().is_some(), "resident");
    let _ = rig;
    out
}

// ---------------------------------------------------------------------------
// Translucency and Path objects
// ---------------------------------------------------------------------------

/// A filled square path `[x0, x1] x [y0, y1]` at depth `z`, coloured by `Cd`.
pub(super) fn path_square(x0: f32, y0: f32, x1: f32, y1: f32, z: f32, color: Color) -> Geometry {
    let mut geometry = Geometry::from_points3(vec![
        Vec3(x0, y0, z),
        Vec3(x1, y0, z),
        Vec3(x1, y1, z),
        Vec3(x0, y1, z),
    ]);
    geometry.push_primitive(Primitive::Path {
        verts: 0..4,
        closed: true,
    });
    geometry
        .primitive_attrs_mut()
        .insert(names::CD, AttributeArray::Color(vec![color]))
        .unwrap();
    geometry
}

const HALF_GREEN: Color = Color::new(0.0, 1.0, 0.0, 0.5);

/// An opaque red quad over `x in [10, 30)`, and a half-transparent green Path
/// square over `x in [20, 40)` behind (`z = +20`) or in front of
/// (`z = -20`) it.
fn path_and_mesh(path_z: f32) -> Scene {
    scene_of(
        pixel_camera(48, 24),
        vec![
            flat_quad(10.0, 4.0, 30.0, 20.0, 0.0, RED),
            path_square(20.0, 4.0, 40.0, 20.0, path_z, HALF_GREEN),
        ],
    )
}

fn assert_path_behind_mesh(out: &FrameBuffer, tolerance: f32) {
    assert_color(px(out, 15, 12), RED, tolerance, "the mesh alone");
    assert_color(
        px(out, 25, 12),
        RED,
        tolerance,
        "the Path behind the mesh is hidden",
    );
    assert_color(px(out, 35, 12), HALF_GREEN, tolerance, "the Path alone");
}

fn assert_path_in_front_of_mesh(out: &FrameBuffer, tolerance: f32) {
    assert_color(px(out, 15, 12), RED, tolerance, "the mesh alone");
    // Half green over opaque red: premultiplied (0, 0.5, 0) + (1, 0, 0) * 0.5,
    // fully opaque.
    assert_color(
        px(out, 25, 12),
        Color::new(0.5, 0.5, 0.0, 1.0),
        tolerance,
        "the Path over the mesh",
    );
}

#[test]
fn a_translucent_path_behind_an_opaque_mesh_is_hidden_by_it_on_the_cpu() {
    let out = cpu_frame(&path_and_mesh(20.0), 48, 24);
    assert_path_behind_mesh(&out, 1e-5);
}

#[test]
fn a_translucent_path_behind_an_opaque_mesh_is_hidden_by_it_on_the_gpu() {
    let out = Rig::new().frame(&path_and_mesh(20.0), 48, 24);
    assert_path_behind_mesh(&out, 4e-3);
}

#[test]
fn a_translucent_path_in_front_of_an_opaque_mesh_blends_over_it() {
    assert_path_in_front_of_mesh(&cpu_frame(&path_and_mesh(-20.0), 48, 24), 1e-5);
    assert_path_in_front_of_mesh(&Rig::new().frame(&path_and_mesh(-20.0), 48, 24), 4e-3);
}

/// A Path whose left edge sits half-way through pixel 20 (`x = 20.5`) has
/// coverage 0.5 there. Over an opaque mesh that pixel must be the half-and-half
/// blend; if the Path wrote depth or clipped the pixel, the mesh (or nothing)
/// would show instead.
fn antialiased_edge_scene() -> Scene {
    scene_of(
        pixel_camera(48, 24),
        vec![
            flat_quad(0.0, 0.0, 48.0, 24.0, 5.0, BLUE),
            path_square(20.5, 4.0, 40.0, 20.0, 0.0, Color::new(1.0, 1.0, 1.0, 1.0)),
        ],
    )
}

fn assert_edge_blends(out: &FrameBuffer, tolerance: f32) {
    assert_color(px(out, 10, 12), BLUE, tolerance, "the background mesh");
    assert_color(
        px(out, 30, 12),
        Color::WHITE,
        tolerance,
        "the Path interior",
    );
    let edge = px(out, 20, 12);
    assert!(
        (edge[3] - 1.0).abs() <= tolerance,
        "no hole: the edge pixel stays opaque, got {edge:?}"
    );
    assert!(
        edge[0] > 0.3 && edge[0] < 0.7 && edge[2] > 0.99,
        "half white over blue is (0.5, 0.5, 1), got {edge:?}"
    );
}

#[test]
fn an_antialiased_path_edge_does_not_cut_a_hole_in_the_background_on_the_cpu() {
    assert_edge_blends(&cpu_frame(&antialiased_edge_scene(), 48, 24), 1e-5);
}

#[test]
fn an_antialiased_path_edge_does_not_cut_a_hole_in_the_background_on_the_gpu() {
    assert_edge_blends(&Rig::new().frame(&antialiased_edge_scene(), 48, 24), 4e-3);
}

/// REQ-3D-007 draws the line here: two translucent objects are ordered whole,
/// by centroid depth, so crossing ones are wrong where they cross.
///
/// `Far` is a flat quad at z = 0 (red); `Near` tilts from z = -30 on the left
/// to z = +30 on the right, so its centroid is also z = 0 and its farthest
/// corner is behind `Far`'s plane: by the tie-break it is drawn first, then
/// `Far` over it. Truth is the opposite on the left of the crossing, where
/// `Near` is nearer — and the pinned pixel there shows `Far` over `Near`.
///
/// This test asserts the **wrong** answer on purpose. If translucent sorting
/// ever becomes exact (per fragment, or by splitting objects), this fails, and
/// the failure is the cue to update REQ-3D-007 and this expectation together.
fn crossing_translucent_scene() -> Scene {
    let red = Color::new(1.0, 0.0, 0.0, 0.5);
    let green = Color::new(0.0, 1.0, 0.0, 0.5);
    let far = flat_quad(10.0, 10.0, 50.0, 30.0, 0.0, red);
    let near = mesh(
        [
            [10.0, 10.0, -30.0],
            [50.0, 10.0, 30.0],
            [50.0, 30.0, 30.0],
            [10.0, 30.0, -30.0],
        ],
        green,
    );
    scene_of(pixel_camera(64, 40), vec![far, near])
}

fn assert_crossing_is_inexact(out: &FrameBuffer, tolerance: f32) {
    // Left of the crossing (x = 15): green is nearer, so exact blending would
    // be green over red = premultiplied 0.5 green + 0.5 * (0.5 red) = (0.25,
    // 0.5, 0), alpha 0.75. The whole-object order draws red last instead:
    // 0.5 red + 0.5 * (0.5 green) = (0.5, 0.25, 0), alpha 0.75.
    let left = px(out, 15, 20);
    let premultiplied = [left[0] * left[3], left[1] * left[3], left[2] * left[3]];
    assert!((left[3] - 0.75).abs() <= tolerance, "alpha {left:?}");
    assert!(
        (premultiplied[0] - 0.5).abs() <= tolerance && (premultiplied[1] - 0.25).abs() <= tolerance,
        "the whole-object order draws the farther-centroid object last on the left: {left:?}"
    );
}

#[test]
fn two_crossing_translucent_quads_are_drawn_whole_in_centroid_order() {
    let scene = crossing_translucent_scene();
    let prepared = prepare_default(&scene, 64, 40);
    assert_eq!(
        prepared.blended,
        vec![Blended::Mesh(1), Blended::Mesh(0)],
        "the tilted quad reaches farther, so it is drawn first"
    );
    assert_crossing_is_inexact(&cpu_frame(&scene, 64, 40), 1e-5);
    assert_crossing_is_inexact(&Rig::new().frame(&scene, 64, 40), 4e-3);
}

/// Depth writes stay off for the second pass: a translucent object drawn later
/// is not rejected where it lies behind one drawn earlier. `Behind` is at
/// z = +10 on the right and z = -10... — the tilted translucent quad's centroid
/// is nearer than the flat one, so it is drawn later, yet its right half lies
/// behind the flat quad. With depth written by the flat quad that half would
/// vanish.
#[test]
fn translucent_surfaces_do_not_write_depth() {
    let flat = flat_quad(10.0, 10.0, 50.0, 30.0, 0.0, Color::new(1.0, 0.0, 0.0, 0.5));
    let tilted = mesh(
        [
            [10.0, 10.0, -40.0],
            [50.0, 10.0, 20.0],
            [50.0, 30.0, 20.0],
            [10.0, 30.0, -40.0],
        ],
        Color::new(0.0, 1.0, 0.0, 0.5),
    );
    let scene = scene_of(pixel_camera(64, 40), vec![flat, tilted]);
    for (label, out) in [
        ("cpu", cpu_frame(&scene, 64, 40)),
        ("gpu", Rig::new().frame(&scene, 64, 40)),
    ] {
        // x = 45: the tilted quad is at z ~= 15, behind the flat one (z = 0)
        // yet still drawn: both contribute, so alpha is 0.75, not 0.5.
        let right = px(&out, 45, 20);
        assert!((right[3] - 0.75).abs() <= 4e-3, "{label}: {right:?}");
    }
}

// ---------------------------------------------------------------------------
// CPU / GPU agreement
// ---------------------------------------------------------------------------

/// A scene that exercises every kind of piece at once: two crossing opaque
/// meshes seen in perspective with a vertex colour ramp, a translucent mesh, a
/// Path with a stroke, loose point sprites, and two image instances.
fn mixed_scene() -> Scene {
    let mut ramp = Geometry::from_points3(vec![
        Vec3(-60.0, -50.0, 20.0),
        Vec3(60.0, -50.0, 20.0),
        Vec3(60.0, 50.0, 20.0),
        Vec3(-60.0, 50.0, 20.0),
    ]);
    ramp.points_mut()
        .insert(
            names::CD,
            AttributeArray::Color(vec![RED, GREEN, BLUE, Color::WHITE]),
        )
        .unwrap();
    ramp.push_mesh(0..4, &[0, 1, 2, 0, 2, 3]);
    let slab = mesh(
        [
            [-40.0, -40.0, -60.0],
            [70.0, -30.0, 60.0],
            [70.0, 40.0, 60.0],
            [-40.0, 30.0, -60.0],
        ],
        Color::new(1.0, 0.8, 0.1, 1.0),
    );
    let glass = flat_quad(
        -30.0,
        -20.0,
        50.0,
        40.0,
        -30.0,
        Color::new(0.2, 0.4, 1.0, 0.5),
    );
    let mut path = path_square(
        -50.0,
        -45.0,
        20.0,
        10.0,
        -45.0,
        Color::new(1.0, 0.0, 0.5, 0.8),
    );
    path.primitive_attrs_mut()
        .insert(names::STROKE_WIDTH, AttributeArray::F32(vec![4.0]))
        .unwrap();
    let mut sprites =
        Geometry::from_points3(vec![Vec3(30.0, -40.0, -20.0), Vec3(45.0, -35.0, -20.0)]);
    sprites
        .points_mut()
        .insert(names::PSCALE, AttributeArray::F32(vec![6.0, 9.0]))
        .unwrap();
    let image = image_geometry(&big_gradient(), &[Vec2(-30.0, 30.0), Vec2(-10.0, 38.0)]);
    // 300 units from the origin: about 0.43 px per unit on a 160 x 120 canvas,
    // so the 120-unit ramp spans ~50 px.
    let camera = Camera::looking_at([0.0, 0.0, -300.0], [0.0, 0.0, 0.0]);
    scene_of(camera, vec![ramp, slab, glass, path, sprites, image])
}

/// A 40 x 30 gradient, so an image instance covers a visible area.
fn big_gradient() -> FrameBuffer {
    let mut pixels = Vec::new();
    for y in 0..30 {
        for x in 0..40 {
            pixels.extend_from_slice(&[x as f32 / 39.0, y as f32 / 29.0, 0.5, 1.0]);
        }
    }
    FrameBuffer::from_f32(40, 30, pixels)
}

/// The two renderers share every decision (`prepare`); what remains is how a
/// pixel is computed. Differences come from three places: the half-float
/// colour attachment (`~5e-4` per channel), fused multiply-add in the GPU's
/// edge and interpolation arithmetic (a pixel exactly on a triangle edge can
/// flip, and its neighbours move by one ulp-scale step), and the Path pixels'
/// coverage, which is analytic on the GPU and `zeno`'s on the CPU (the same
/// tolerance the 2D rasterizer's own parity tests use).
///
/// So the bound has three parts, measured at 99.97% / 0.086 / 2e-5 and set
/// with room: at least 99.9% of pixels agree within 0.02 on every channel, no
/// pixel is off by more than 0.25 (an edge pixel that flipped), and the mean
/// absolute difference stays below 5e-4. A wrong projection, a flipped axis
/// or a dropped pass moves far more than any of them.
#[test]
fn gpu_matches_cpu_for_every_kind_of_piece() {
    let rig = Rig::new();
    let scene = mixed_scene();
    let cpu = cpu_frame(&scene, 160, 120);
    let gpu = rig.frame(&scene, 160, 120);
    let (ratio, worst) = agreement(&cpu, &gpu, 0.02);
    let mean = cpu
        .as_f32()
        .iter()
        .zip(gpu.as_f32().iter())
        .map(|(a, b)| (a - b).abs())
        .sum::<f32>()
        / cpu.as_f32().len() as f32;
    eprintln!(
        "mixed scene: {:.3}% within 0.02, worst {worst:.4}, mean {mean:.5}",
        ratio * 100.0
    );
    assert!(covered(&cpu) > 2000, "the scene actually draws something");
    assert!(worst < 0.25, "the worst pixel differs by {worst}");
    assert!(
        ratio >= 0.999,
        "{:.3}% of pixels within tolerance (worst {worst})",
        ratio * 100.0
    );
    assert!(mean < 5e-4, "mean difference {mean}");
}

#[test]
fn gpu_matches_cpu_for_perspective_edges() {
    let scene = scene_of_without_camera(vec![flat_quad(-100.0, -100.0, 100.0, 100.0, 0.0, RED)]);
    assert_edges(&Rig::new().frame(&scene, 200, 200));
}

#[test]
fn an_inverted_clip_range_draws_the_same_picture_as_its_normalised_one() {
    // `near > far` is reachable from the parameters. The projection and the
    // depth attachment must agree about the range they use, which they do
    // because both come from `Camera::clip_range`: the picture is the one the
    // normalised range gives (near stays, far is pushed beyond it).
    let objects = || {
        let (a, b) = crossing_pair();
        vec![a, b]
    };
    let normal = pixel_camera(64, 40);
    let inverted = normal.with_clip_range(1.0, 0.5);
    let reference = cpu_frame(
        &scene_of(normal.with_clip_range(1.0, 1.0 + 1e-4), objects()),
        64,
        40,
    );
    let rig = Rig::new();
    for out in [
        cpu_frame(&scene_of(inverted, objects()), 64, 40),
        rig.frame(&scene_of(inverted, objects()), 64, 40),
    ] {
        // Both quads sit 100 units from the camera, far beyond the widened
        // range's far plane at ~1.0001, so nothing is drawn — identically.
        assert_eq!(covered(&out), covered(&reference));
    }
    assert_eq!(
        covered(&reference),
        0,
        "the normalised range really is empty here, so \"the same picture\" is a blank one: \
         an inverted mapping (far at depth 0) would draw both quads"
    );
}

// ---------------------------------------------------------------------------
// Static buffers
// ---------------------------------------------------------------------------

fn moving_scene(geometry: &Arc<Geometry>, x: f32) -> Scene {
    Scene::from_camera(pixel_camera(64, 40)).with_object(SceneObject::geometry(
        Arc::clone(geometry),
        Transform3D::from_translation([x, 0.0, 0.0]),
    ))
}

#[test]
fn an_unchanged_mesh_is_not_reallocated_or_reuploaded_on_the_next_frame() {
    let rig = Rig::new();
    let processor = rig.processor();
    let geometry = Arc::new(flat_quad(0.0, 8.0, 16.0, 24.0, 0.0, RED));

    let first = run(&processor, &moving_scene(&geometry, 0.0), &ctx(64, 40));
    assert_eq!(
        processor.static_buffer_allocations(),
        1,
        "the first frame uploads the mesh"
    );
    let ids = {
        let renderer = processor.gpu.as_ref().unwrap();
        renderer.buffer_ids(&prepare_default(&moving_scene(&geometry, 0.0), 64, 40))
    };
    // The static buffers' three uploads went through the context's counters,
    // so an upload on a cache hit would show up as a change in them.
    let uploaded = rig.gpu.transfer_stats();
    assert!(
        uploaded.uploads >= 3 && uploaded.upload_bytes > 0,
        "the first frame uploads vertices, indices and colours: {uploaded:?}"
    );

    // Four more frames with the object animated: a new scene value each time,
    // the same mesh columns.
    for step in 1..=4 {
        let x = step as f32 * 10.0;
        let out = frame(&run(&processor, &moving_scene(&geometry, x), &ctx(64, 40)));
        assert_color(
            px(&out, x as u32 + 2, 12),
            RED,
            4e-3,
            "the quad followed its transform",
        );
    }
    assert_eq!(
        processor.static_buffer_allocations(),
        1,
        "moving an object must not rebuild its mesh buffers"
    );
    let later = rig.gpu.transfer_stats();
    assert_eq!(
        (later.uploads, later.upload_bytes),
        (uploaded.uploads, uploaded.upload_bytes),
        "a cache hit must not write into the static buffers again"
    );
    let again = processor.gpu.as_ref().unwrap().buffer_ids(&prepare_default(
        &moving_scene(&geometry, 5.0),
        64,
        40,
    ));
    assert_eq!(
        ids, again,
        "the later frames drew through the very same buffers"
    );
    drop(first);
}

#[test]
fn an_aged_out_mesh_returns_its_buffers_to_the_pool_and_a_new_mesh_reuses_them() {
    let rig = Rig::new();
    let processor = rig.processor();
    let gpu = processor.gpu.as_ref().unwrap();
    let geometry = Arc::new(flat_quad(0.0, 8.0, 16.0, 24.0, 0.0, RED));
    run(&processor, &moving_scene(&geometry, 0.0), &ctx(64, 40));
    assert_eq!(
        gpu.pool_stats(),
        (3, 0),
        "vertices, indices, colours leased"
    );

    // Meshless frames until the entry ages out (16 unused frames).
    for _ in 0..20 {
        run(
            &processor,
            &scene_of(pixel_camera(64, 40), vec![]),
            &ctx(64, 40),
        );
    }
    assert_eq!(gpu.live_buffer_sets(), 0, "the entry aged out");
    assert_eq!(gpu.pool_stats(), (3, 3), "its three buffers went back idle");

    // A different mesh of the same size class leases them again. The first
    // frame's pass may still sit in the unsubmitted batch, and the pool skips
    // a buffer that batch reads, so submit it first.
    rig.gpu.flush();
    let mut edited = (*geometry).clone();
    if let AttributeArray::Vec3(positions) = edited.points_mut().make_mut(names::P).unwrap() {
        positions[2].0 = 20.0;
    }
    let out = frame(&run(
        &processor,
        &moving_scene(&Arc::new(edited), 0.0),
        &ctx(64, 40),
    ));
    assert_eq!(gpu.pool_stats(), (3, 0), "no new buffer was created");
    assert_color(
        px(&out, 10, 12),
        RED,
        4e-3,
        "drawn through the recycled buffers",
    );
}

#[test]
fn a_changed_mesh_is_uploaded_again() {
    let rig = Rig::new();
    let processor = rig.processor();
    let geometry = Arc::new(flat_quad(0.0, 8.0, 16.0, 24.0, 0.0, RED));
    run(&processor, &moving_scene(&geometry, 0.0), &ctx(64, 40));
    assert_eq!(processor.static_buffer_allocations(), 1);

    let mut edited = (*geometry).clone();
    if let AttributeArray::Vec3(positions) = edited.points_mut().make_mut(names::P).unwrap() {
        positions[2].0 = 20.0;
    }
    run(
        &processor,
        &moving_scene(&Arc::new(edited), 0.0),
        &ctx(64, 40),
    );
    assert_eq!(
        processor.static_buffer_allocations(),
        2,
        "an edited column is a new mesh"
    );
}

#[test]
fn buffers_nobody_asks_for_any_more_are_released() {
    let rig = Rig::new();
    let processor = rig.processor();
    let renderer = processor.gpu.as_ref().unwrap();
    // A mesh rebuilt every frame (fresh columns each time) must not pile up.
    for index in 0..40 {
        let fresh = Arc::new(flat_quad(0.0, 0.0, 8.0 + index as f32, 8.0, 0.0, RED));
        run(&processor, &moving_scene(&fresh, 0.0), &ctx(64, 40));
    }
    assert_eq!(processor.static_buffer_allocations(), 40);
    assert!(
        renderer.live_buffer_sets() <= 17,
        "{} buffer sets are still held",
        renderer.live_buffer_sets()
    );
}

/// A pixel exactly on the shared diagonal of two triangles belongs to exactly
/// one of them (the top-left rule), so a half-transparent quad is uniformly
/// half transparent: a pixel drawn by both would be 0.75, one drawn by neither
/// would be 0. The diagonal `y = x` passes through the pixel centres.
fn assert_uniform_half_alpha(out: &FrameBuffer, label: &str) {
    for y in 4..12 {
        for x in 4..12 {
            let alpha = px(out, x, y)[3];
            assert!(
                (alpha - 0.5).abs() <= 2e-3,
                "{label}: pixel ({x}, {y}) has alpha {alpha}"
            );
        }
    }
}

#[test]
fn a_shared_triangle_edge_is_drawn_once_on_both_renderers() {
    let scene = scene_of(
        pixel_camera(16, 16),
        vec![flat_quad(
            4.0,
            4.0,
            12.0,
            12.0,
            0.0,
            Color::new(1.0, 1.0, 1.0, 0.5),
        )],
    );
    assert_uniform_half_alpha(&cpu_frame(&scene, 16, 16), "cpu");
    assert_uniform_half_alpha(&Rig::new().frame(&scene, 16, 16), "gpu");
}

// ---------------------------------------------------------------------------
// Paths crossing the near plane
// ---------------------------------------------------------------------------

/// The camera for these tests sits at `(0, 0, -100)` looking along `+Z` with
/// the default 50 degree lens and `near = 1`, so a point with `z <= -99` is
/// nearer than the near plane and `z < -100` is behind the camera. On a
/// 200 x 200 canvas a point at distance `d` from the camera and lateral offset
/// `x` lands at `100 + x * 100 * 2.1445 / d`, by hand
/// (`1 / tan 25 degrees = 2.1445`).
fn near_plane_camera() -> Camera {
    Camera::looking_at([0.0, 0.0, -100.0], [0.0, 0.0, 0.0])
}

/// A line along the view direction at `x = 20`, from 100 behind the camera
/// (`z = -200`) to 200 in front of it (`z = 100`), stroked 1 unit wide.
///
/// Its far end is at distance 200, i.e. pixel `100 + 20 * 2.1445 * 100 / 200 =
/// 121.4`, and it runs outwards to the right from there as it nears the
/// camera, off the right edge. Projected without clipping, its behind-camera
/// end lands on the *other* side of the screen (the divide by a negative `w`
/// mirrors it), so the unclipped picture has a line across `x in [79, 121]`.
fn line_through_the_near_plane() -> Geometry {
    let mut geometry =
        Geometry::from_points3(vec![Vec3(20.0, 0.0, -200.0), Vec3(20.0, 0.0, 100.0)]);
    geometry.push_primitive(Primitive::Path {
        verts: 0..2,
        closed: false,
    });
    geometry
        .primitive_attrs_mut()
        .insert(names::CD, AttributeArray::Color(vec![RED]))
        .unwrap();
    geometry
        .primitive_attrs_mut()
        .insert(names::STROKE_WIDTH, AttributeArray::F32(vec![1.0]))
        .unwrap();
    geometry
}

fn assert_only_the_visible_part_of_the_line(out: &FrameBuffer, label: &str) {
    for x in 84..116 {
        assert_eq!(
            px(out, x, 100)[3],
            0.0,
            "{label}: ({x}, 100) is where the mirrored part would be"
        );
    }
    assert!(
        px(out, 150, 100)[3] > 0.9,
        "{label}: the visible part runs out to the right"
    );
    assert!(px(out, 190, 100)[3] > 0.9, "{label}: ... to the edge");
}

#[test]
fn an_open_path_crossing_the_near_plane_draws_only_its_visible_part() {
    let scene = scene_of(near_plane_camera(), vec![line_through_the_near_plane()]);
    assert_only_the_visible_part_of_the_line(&cpu_frame(&scene, 200, 200), "cpu");
    assert_only_the_visible_part_of_the_line(&Rig::new().frame(&scene, 200, 200), "gpu");
}

/// A floor quad `y = 20` (below the camera: `+Y` is down) running from 100
/// behind the camera to 200 in front of it, `x in [-50, 50]`. Its far edge is
/// at distance 200: pixel row `100 + 20 * 2.1445 * 100 / 200 = 121.4`, below
/// the horizon at row 100, and the visible polygon fills the screen below that
/// row. Projected unclipped, the behind-camera corners land *above* the
/// horizon and the fill covers the sky.
fn floor_through_the_near_plane() -> Geometry {
    let mut geometry = Geometry::from_points3(vec![
        Vec3(-50.0, 20.0, -200.0),
        Vec3(50.0, 20.0, -200.0),
        Vec3(50.0, 20.0, 100.0),
        Vec3(-50.0, 20.0, 100.0),
    ]);
    geometry.push_primitive(Primitive::Path {
        verts: 0..4,
        closed: true,
    });
    geometry
        .primitive_attrs_mut()
        .insert(names::CD, AttributeArray::Color(vec![GREEN]))
        .unwrap();
    geometry
}

fn assert_floor_below_the_horizon(out: &FrameBuffer, label: &str) {
    for y in [5, 50, 100, 115] {
        assert_eq!(
            px(out, 100, y)[3],
            0.0,
            "{label}: row {y} is above the floor's far edge"
        );
    }
    assert!(
        px(out, 100, 130).a_over(0.9),
        "{label}: just below the far edge"
    );
    assert!(
        px(out, 100, 190).a_over(0.9),
        "{label}: the near part fills the bottom"
    );
}

trait AlphaOver {
    fn a_over(&self, threshold: f32) -> bool;
}

impl AlphaOver for [f32; 4] {
    fn a_over(&self, threshold: f32) -> bool {
        self[3] > threshold
    }
}

#[test]
fn a_closed_path_crossing_the_near_plane_is_clipped_to_a_polygon() {
    let scene = scene_of(near_plane_camera(), vec![floor_through_the_near_plane()]);
    assert_floor_below_the_horizon(&cpu_frame(&scene, 200, 200), "cpu");
    assert_floor_below_the_horizon(&Rig::new().frame(&scene, 200, 200), "gpu");
}

#[test]
fn a_loose_point_behind_the_camera_is_not_drawn() {
    let mut sprites = Geometry::from_points3(vec![Vec3(0.0, 0.0, -300.0), Vec3(0.0, 0.0, 100.0)]);
    sprites
        .points_mut()
        .insert(names::PSCALE, AttributeArray::F32(vec![40.0, 2.0]))
        .unwrap();
    let scene = scene_of(near_plane_camera(), vec![sprites]);
    // Only the point 200 away (a sprite of radius 2 * 100 * 2.1445 / 200 =
    // 2.1 px at the centre) is drawn; the one behind the camera, whose 40-unit
    // radius would be huge, is not.
    let out = cpu_frame(&scene, 200, 200);
    assert!(
        covered(&out) > 0 && covered(&out) < 100,
        "{}",
        covered(&out)
    );
}

// ---------------------------------------------------------------------------
// Curved paths
// ---------------------------------------------------------------------------

/// A closed path of four bezier segments approximating the circle of `radius`
/// about `(cx, cy, z)`: points at 0, 90, 180 and 270 degrees, tangents of
/// `0.5523 * radius` along the circle's own direction (the standard cubic
/// circle), so the curve is within 0.03% of round and its control polygon is
/// a square far outside it.
fn bezier_circle(cx: f32, cy: f32, z: f32, radius: f32) -> Geometry {
    let k = 0.552_284_8 * radius;
    let mut points = Vec::new();
    let (mut out_tans, mut in_tans) = (Vec::new(), Vec::new());
    for quarter in 0..4 {
        let angle = quarter as f32 * std::f32::consts::FRAC_PI_2;
        let (sin, cos) = angle.sin_cos();
        points.push(Vec3(cx + radius * cos, cy + radius * sin, z));
        out_tans.push(Vec2(-k * sin, k * cos));
        in_tans.push(Vec2(k * sin, -k * cos));
    }
    let mut geometry = Geometry::from_points3(points);
    geometry
        .points_mut()
        .insert(names::OUT_TAN, AttributeArray::Vec2(out_tans))
        .unwrap();
    geometry
        .points_mut()
        .insert(names::IN_TAN, AttributeArray::Vec2(in_tans))
        .unwrap();
    geometry.push_primitive(Primitive::Path {
        verts: 0..4,
        closed: true,
    });
    geometry
        .primitive_attrs_mut()
        .insert(names::CD, AttributeArray::Color(vec![RED]))
        .unwrap();
    geometry
}

/// Pixels `inside` the circle of pixel radius `radius` about `(cx, cy)` must
/// be covered and pixels `outside` it must not, at every 15 degrees. The
/// control polygon of the circle (its four points joined, or the square of its
/// handles) fails both: at 45 degrees it is 28 pixels out where the circle is
/// 40.
fn assert_round(frame: &FrameBuffer, cx: f32, cy: f32, radius: f32, margin: f32, label: &str) {
    for step in 0..24 {
        let angle = step as f32 * 15.0_f32.to_radians();
        let at = |r: f32| {
            let (x, y) = (cx + r * angle.cos(), cy + r * angle.sin());
            px(frame, x.floor() as u32, y.floor() as u32)[3]
        };
        assert!(
            at(radius - margin) > 0.9,
            "{label}: inside at {} degrees",
            step * 15
        );
        assert!(
            at(radius + margin) < 0.1,
            "{label}: outside at {} degrees",
            step * 15
        );
    }
}

#[test]
fn a_bezier_circle_in_3d_renders_round() {
    let scene = scene_of(
        pixel_camera(100, 100),
        vec![bezier_circle(50.0, 50.0, 0.0, 40.0)],
    );
    assert_round(&cpu_frame(&scene, 100, 100), 50.0, 50.0, 40.0, 1.5, "cpu");
    assert_round(
        &Rig::new().frame(&scene, 100, 100),
        50.0,
        50.0,
        40.0,
        1.5,
        "gpu",
    );
}

#[test]
fn a_bezier_circle_under_perspective_renders_round_at_the_projected_radius() {
    // The facing circle at distance 100 on a 200 x 200 canvas: 30 units are
    // 30 * 2.1445 = 64.3 pixels.
    let scene = scene_of(
        near_plane_camera(),
        vec![bezier_circle(0.0, 0.0, 0.0, 30.0)],
    );
    assert_round(&cpu_frame(&scene, 200, 200), 100.0, 100.0, 64.3, 1.5, "cpu");
    assert_round(
        &Rig::new().frame(&scene, 200, 200),
        100.0,
        100.0,
        64.3,
        1.5,
        "gpu",
    );
}

/// The straight line of `line_through_the_near_plane` bent into a curve (both
/// handles pull toward `+y`). Unclipped, its behind-camera end mirrors across
/// the screen centre into the left half, so the left of the picture must stay
/// empty; the end at distance 200 is at pixel `x = 121.4`.
fn curve_through_the_near_plane() -> Geometry {
    let mut geometry = line_through_the_near_plane();
    geometry
        .points_mut()
        .insert(
            names::OUT_TAN,
            AttributeArray::Vec2(vec![Vec2(0.0, 30.0), Vec2(0.0, 0.0)]),
        )
        .unwrap();
    geometry
        .points_mut()
        .insert(
            names::IN_TAN,
            AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(0.0, 30.0)]),
        )
        .unwrap();
    geometry
}

fn assert_curve_only_in_front(frame: &FrameBuffer, label: &str) {
    for y in 0..200 {
        for x in 0..118 {
            assert_eq!(
                px(frame, x, y)[3],
                0.0,
                "{label}: ({x}, {y}) is behind the camera"
            );
        }
    }
    let drawn = (118..200)
        .flat_map(|x| (0..200).map(move |y| (x, y)))
        .filter(|&(x, y)| px(frame, x, y)[3] > 0.1)
        .count();
    assert!(
        drawn > 20,
        "{label}: the front of the curve is drawn ({drawn})"
    );
}

#[test]
fn a_curve_crossing_the_near_plane_is_clipped_as_a_curve() {
    let scene = scene_of(near_plane_camera(), vec![curve_through_the_near_plane()]);
    assert_curve_only_in_front(&cpu_frame(&scene, 200, 200), "cpu");
    assert_curve_only_in_front(&Rig::new().frame(&scene, 200, 200), "gpu");
}

// ---------------------------------------------------------------------------
// Concave closed paths cut by the near plane
// ---------------------------------------------------------------------------

/// A U-shaped floor polygon at `y = 20` whose two prongs run from behind the
/// camera to `z = 100` and whose notch bottom (`z = -150`) is behind it. The
/// near plane (`near = 90`, so at `z = -10`) cuts it into two pieces, which
/// the clipped ring joins by edges along the plane.
///
/// On a 200 x 200 canvas the plane is 90 away, so those edges land on pixel
/// row `100 + 20 * 214.45 / 90 = 147.7`, spanning the whole width; the prongs
/// (`x in [-60, -30]` and `[30, 60]`) never reach `x in [80, 120]` between
/// rows 140 and 156 (they are at most `100 - 30 * 214.45 / 90 = 28.5` and at
/// least `100 + 30 * 214.45 / 200 = 132` pixels out there).
fn u_shaped_floor() -> Geometry {
    let points = [
        (-60.0, -200.0),
        (-60.0, 100.0),
        (-30.0, 100.0),
        (-30.0, -150.0),
        (30.0, -150.0),
        (30.0, 100.0),
        (60.0, 100.0),
        (60.0, -200.0),
    ];
    let mut geometry =
        Geometry::from_points3(points.iter().map(|&(x, z)| Vec3(x, 20.0, z)).collect());
    geometry.push_primitive(Primitive::Path {
        verts: 0..points.len(),
        closed: true,
    });
    geometry
        .primitive_attrs_mut()
        .insert(names::CD, AttributeArray::Color(vec![GREEN]))
        .unwrap();
    geometry
        .primitive_attrs_mut()
        .insert(names::STROKE_WIDTH, AttributeArray::F32(vec![6.0]))
        .unwrap();
    geometry
}

fn assert_u_floor_has_no_bridge(out: &FrameBuffer, label: &str) {
    for y in 140..156 {
        for x in 80..120 {
            assert_eq!(
                px(out, x, y)[3],
                0.0,
                "{label}: ({x}, {y}) is on the edge along the near plane"
            );
        }
    }
    // The fill is the prongs, unchanged: well inside the left one at row 130
    // (pixels 10 to 55 there) and the right one (145 to 190).
    assert!(px(out, 30, 130)[3] > 0.9, "{label}: left prong filled");
    assert!(px(out, 170, 130)[3] > 0.9, "{label}: right prong filled");
    assert_eq!(px(out, 100, 130)[3], 0.0, "{label}: the notch stays empty");
    // The outline of the shape is stroked (6 units, about 9 pixels here, so
    // 57 is outside the fill, which ends at 55)...
    assert!(
        px(out, 57, 130)[3] > 0.5,
        "{label}: the real edge is stroked"
    );
    // ...but the cut along the near plane is not an edge of the shape: the
    // fill ends at row 147.7, and a stroke along the cut would reach under it
    // (row 150) across the whole prong, out to the screen edge.
    for x in [10, 190] {
        assert_eq!(
            px(out, x, 150)[3],
            0.0,
            "{label}: ({x}, 150) strokes the cut"
        );
    }
}

#[test]
fn a_concave_path_cut_by_the_near_plane_strokes_no_bridge_between_its_pieces() {
    let camera =
        Camera::looking_at([0.0, 0.0, -100.0], [0.0, 0.0, 0.0]).with_clip_range(90.0, 1000.0);
    let scene = scene_of(camera, vec![u_shaped_floor()]);
    assert_u_floor_has_no_bridge(&cpu_frame(&scene, 200, 200), "cpu");
    assert_u_floor_has_no_bridge(&Rig::new().frame(&scene, 200, 200), "gpu");
}

#[test]
fn a_scaled_object_flattens_its_curves_to_the_same_pixel_tolerance() {
    // Radius 0.4 object units scaled 100x about the origin, then moved to the
    // canvas centre: a circle of 40 pixels. Flattened at the object's own
    // 0.25 units the four quarter arcs would be chords 25 pixels off.
    let mut transform = Transform3D::from_translation([50.0, 50.0, 0.0]);
    transform.scale = [100.0, 100.0, 100.0];
    let scene = Scene::from_camera(pixel_camera(100, 100)).with_object(SceneObject::geometry(
        Arc::new(bezier_circle(0.0, 0.0, 0.0, 0.4)),
        transform,
    ));
    assert_round(&cpu_frame(&scene, 100, 100), 50.0, 50.0, 40.0, 1.5, "cpu");
    assert_round(
        &Rig::new().frame(&scene, 100, 100),
        50.0,
        50.0,
        40.0,
        1.5,
        "gpu",
    );
}

/// The primitives' `uv` is what this sampler reads: a `mesh.plane` textured
/// with the 2 x 2 quad shows red, green / blue, white at its top-left,
/// top-right, bottom-left and bottom-right, the same as a hand-built quad with
/// `NATURAL_UV`.
#[test]
fn a_mesh_plane_shows_a_texture_the_right_way_up() {
    let mut params = ResolvedParams::default();
    params.set("size", ravel_core::eval::ResolvedValue::Vec2([16.0, 16.0]));
    params.set(
        "center",
        ravel_core::eval::ResolvedValue::Vec3([16.0, 16.0, 0.0]),
    );
    let plane = crate::mesh_primitives::PlaneProcessor::from_node(&node())
        .process(&node(), &ctx(32, 32), &[], &params, &mut Evaluator::new())
        .unwrap();
    let plane = plane.downcast_ref::<Geometry>().unwrap().clone();
    let scene = scene_of(pixel_camera(32, 32), vec![plane]);
    let mut prepared = prepare_default(&scene, 32, 32);
    prepared.meshes[0].texture = Some(Arc::new(quad_texture()));
    let out = cpu::render(&prepared).unwrap();
    assert_quadrants(&out, [RED, GREEN, BLUE, Color::WHITE], 1e-5);
}
