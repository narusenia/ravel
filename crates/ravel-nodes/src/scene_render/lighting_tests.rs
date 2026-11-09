// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Tests of lighting in `scene.render` (REQ-3D-006).
//!
//! Every expected brightness is **derived by hand** in the test's comment from
//! the light's definition (`ravel_core::scene::light`), not computed with the
//! renderer's own formula. The set-up throughout is `pixel_camera(64, 64)` — an
//! orthographic camera at `z = -100` looking along `+Z` — and a quad facing it,
//! whose normal is therefore `(0, 0, -1)` and whose direction *to* the camera is
//! `(0, 0, -1)` as well. A light that travels along `+Z` hits it head on.
//!
//! Each shading test runs on both renderers: the CPU reference and the GPU
//! pass must each land on the hand-derived number, not merely on each other.

use std::sync::Arc;

use ravel_core::geometry::{AttributeArray, Geometry, names};
use ravel_core::scene::{Camera, Light, LightKind, Scene, SceneObject, Transform3D};
use ravel_core::types::{Color, FrameBuffer, Vec3};

use super::prepare::{KIND_AMBIENT, KIND_DIRECTIONAL, KIND_POINT, LightRecord};
use super::tests::{
    RED, Rig, agreement, assert_color, covered, cpu_frame, flat_quad, path_square, pixel_camera,
    prepare_default, px, scene_of,
};

const BASE: Color = Color::new(0.8, 0.8, 0.8, 1.0);

/// Half-float colour attachment (`~5e-4`) with room.
const TOLERANCE: f32 = 2e-3;

fn facing(normal: Vec3, geometry: &mut Geometry) {
    let count = geometry.point_count();
    geometry
        .points_mut()
        .insert(names::N, AttributeArray::Vec3(vec![normal; count]))
        .unwrap();
}

/// A quad over `[x0, x1] x [y0, y1]` at depth `z` with `N` writen on it.
fn lit_quad(x0: f32, y0: f32, x1: f32, y1: f32, z: f32, color: Color, normal: Vec3) -> Geometry {
    let mut quad = flat_quad(x0, y0, x1, y1, z, color);
    facing(normal, &mut quad);
    quad
}

const TOWARD_CAMERA: Vec3 = Vec3(0.0, 0.0, -1.0);

fn lights(scene: Scene, lights: &[Light]) -> Scene {
    lights
        .iter()
        .fold(scene, |scene, light| scene.with_light(*light))
}

fn white(kind: LightKind, intensity: f32) -> Light {
    Light {
        kind,
        color: [1.0, 1.0, 1.0],
        intensity,
    }
}

fn sun(direction: [f32; 3], intensity: f32) -> Light {
    white(LightKind::Directional { direction }, intensity)
}

fn ambient(intensity: f32) -> Light {
    white(LightKind::Ambient, intensity)
}

fn point(position: [f32; 3], radius: f32) -> Light {
    white(LightKind::Point { position, radius }, 1.0)
}

/// The full-canvas quad facing the camera under `under`.
fn quad_under(under: &[Light]) -> Scene {
    lights(
        scene_of(
            pixel_camera(64, 64),
            vec![lit_quad(0.0, 0.0, 64.0, 64.0, 0.0, BASE, TOWARD_CAMERA)],
        ),
        under,
    )
}

fn both(scene: &Scene, size: u32) -> [(&'static str, FrameBuffer); 2] {
    [
        ("cpu", cpu_frame(scene, size, size)),
        ("gpu", Rig::new().frame(scene, size, size)),
    ]
}

/// The pixel `(x, y)` of both renderers is `BASE` scaled per channel by
/// `factor`, with the alpha untouched.
fn assert_shade_at(scene: &Scene, x: u32, y: u32, factor: [f32; 3], alpha: f32) {
    for (renderer, frame) in both(scene, 64) {
        assert_color(
            px(&frame, x, y),
            Color::new(
                BASE.r * factor[0],
                BASE.g * factor[1],
                BASE.b * factor[2],
                alpha,
            ),
            TOLERANCE,
            &format!("{renderer} at ({x}, {y})"),
        );
    }
}

fn assert_shade(scene: &Scene, factor: f32) {
    assert_shade_at(scene, 32, 32, [factor; 3], 1.0);
}

// ---------------------------------------------------------------------------
// The three kinds
// ---------------------------------------------------------------------------

/// Head on, `N · L = 1`, so a directional light delivers exactly its colour
/// times its intensity, per channel: `(1, 0.5, 0.25) * 0.5`.
#[test]
fn a_directional_light_head_on_delivers_its_colour_times_intensity() {
    let light = Light {
        kind: LightKind::Directional {
            direction: [0.0, 0.0, 1.0],
        },
        color: [1.0, 0.5, 0.25],
        intensity: 0.5,
    };
    assert_shade_at(&quad_under(&[light]), 32, 32, [0.5, 0.25, 0.125], 1.0);
}

/// Travelling along `(1.2, 0, 1.6)` — length 2, so unit `(0.6, 0, 0.8)` — the
/// light arrives from `(-0.6, 0, -0.8)`, and `N · L = (0, 0, -1) · (-0.6, 0, -0.8)
/// = 0.8`. The direction is not required to be unit.
#[test]
fn a_directional_light_follows_the_cosine_and_normalises_its_direction() {
    assert_shade(&quad_under(&[sun([1.2, 0.0, 1.6], 1.0)]), 0.8);
}

/// A light travelling along `-Z` shines from behind the quad, onto its back:
/// `N · L = -1`, clamped to 0. This is the pin on the sign of the direction —
/// a renderer that took the direction as pointing *at* the light would light
/// this and darken the head-on case above.
#[test]
fn a_directional_light_from_behind_the_surface_delivers_nothing() {
    for direction in [[0.0, 0.0, -1.0], [1.0, 0.0, 0.0]] {
        // The second is edge-on (`N · L = 0`).
        assert_shade_at(&quad_under(&[sun(direction, 1.0)]), 32, 32, [0.0; 3], 1.0);
    }
}

/// Ambient delivers `color * intensity` whatever the normal: the same `0.25` on
/// a quad facing the camera and on one facing away.
#[test]
fn an_ambient_light_ignores_the_normal() {
    for normal in [TOWARD_CAMERA, Vec3(0.0, 0.0, 1.0), Vec3(1.0, 0.0, 0.0)] {
        let scene = lights(
            scene_of(
                pixel_camera(64, 64),
                vec![lit_quad(0.0, 0.0, 64.0, 64.0, 0.0, BASE, normal)],
            ),
            &[ambient(0.25)],
        );
        assert_shade(&scene, 0.25);
    }
}

/// Lights add: ambient `0.25` plus a head-on sun of `0.5` is `0.75`.
#[test]
fn lights_add_up() {
    assert_shade(
        &quad_under(&[ambient(0.25), sun([0.0, 0.0, 1.0], 0.5)]),
        0.75,
    );
}

/// A lit surface with lights but no ambient light is dark where nothing
/// reaches it: the ambient default applies only to a scene with **no** light.
#[test]
fn the_ambient_default_applies_only_when_there_is_no_light_at_all() {
    assert_shade(&quad_under(&[sun([0.0, 0.0, -1.0], 1.0)]), 0.0);
}

/// A point light at `(32, 32, -30)` over the quad at `z = 0`, half strength at
/// `radius = 30`.
///
/// By hand, at the pixel whose centre is `(0.5, 0.5)`: the vector to the light is
/// `(31.5, 31.5, -30)`, of length `sqrt(2 * 31.5² + 30²) = 53.7075`, so
/// `N · L = 30 / 53.7075 = 0.55858` and the falloff is
/// `1 / (1 + (53.7075 / 30)²) = 0.23781`; the product is `0.13284`. At
/// `(31.5, 31.5)` the light is `30.0083` away, almost straight on: `N · L` and
/// the falloff are `0.99972` and `0.49986`, `0.4997` together.
#[test]
fn a_point_light_falls_off_with_distance_and_with_angle() {
    let scene = quad_under(&[point([32.0, 32.0, -30.0], 30.0)]);
    assert_shade_at(&scene, 31, 31, [0.4997; 3], 1.0);
    assert_shade_at(&scene, 0, 0, [0.13284; 3], 1.0);
    assert_shade_at(&scene, 63, 63, [0.13284; 3], 1.0);
}

/// Behind the surface a point light delivers nothing: `N · L < 0`.
#[test]
fn a_point_light_behind_the_surface_delivers_nothing() {
    assert_shade(&quad_under(&[point([32.0, 32.0, 30.0], 30.0)]), 0.0);
}

// ---------------------------------------------------------------------------
// Defaults and what is not lit
// ---------------------------------------------------------------------------

/// With no light the quad is drawn in its own colour: the white ambient default,
/// which scales by exactly 1.
#[test]
fn a_scene_with_no_light_shows_the_mesh_in_its_own_colour() {
    assert_shade(&quad_under(&[]), 1.0);
}

/// `prepare` hands the renderers a white ambient record when the scene has no
/// light — the rule, as data — and the records of the lights otherwise.
#[test]
fn prepare_supplies_white_ambient_for_a_scene_without_light() {
    let prepared = prepare_default(&quad_under(&[]), 64, 64);
    assert_eq!(
        prepared.lights,
        vec![LightRecord {
            vector: [0.0, 0.0, 0.0, KIND_AMBIENT],
            color: [1.0, 1.0, 1.0, 0.0],
        }]
    );
}

/// The records are in scene space and in the form the shading reads: a
/// directional light is stored as the unit vector **toward** the light (the
/// negated, normalised direction of travel), colour is premultiplied by
/// intensity, and a point light carries its position and half-strength radius.
#[test]
fn prepare_records_each_light_in_the_form_the_shading_reads() {
    let scene = quad_under(&[
        Light {
            kind: LightKind::Directional {
                direction: [0.0, 3.0, 4.0],
            },
            color: [1.0, 0.5, 0.0],
            intensity: 2.0,
        },
        Light {
            kind: LightKind::Point {
                position: [1.0, 2.0, 3.0],
                radius: 0.0,
            },
            color: [0.0, 1.0, 0.0],
            intensity: -1.0,
        },
        ambient(0.5),
    ]);
    let records = prepare_default(&scene, 64, 64).lights;
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].vector, [0.0, -0.6, -0.8, KIND_DIRECTIONAL]);
    assert_eq!(records[0].color, [2.0, 1.0, 0.0, 0.0]);
    assert_eq!(records[1].vector, [1.0, 2.0, 3.0, KIND_POINT]);
    assert_eq!(
        &records[1].color[..3],
        &[0.0, 0.0, 0.0],
        "a negative intensity delivers nothing rather than taking light away"
    );
    assert!(
        records[1].color[3] > 0.0,
        "a zero radius is lifted so the falloff cannot divide by zero"
    );
    assert_eq!(records[2].vector[3], KIND_AMBIENT);
    assert_eq!(records[2].color, [0.5, 0.5, 0.5, 0.0]);
}

/// A mesh with no `N` column is not lit: a sun from behind would make it black
/// if it were, and a normal invented for it would shade it arbitrarily.
#[test]
fn a_mesh_without_normals_is_drawn_in_its_own_colour() {
    let scene = lights(
        scene_of(
            pixel_camera(64, 64),
            vec![flat_quad(0.0, 0.0, 64.0, 64.0, 0.0, BASE)],
        ),
        &[sun([0.0, 0.0, -1.0], 1.0), ambient(0.1)],
    );
    assert_shade(&scene, 1.0);
}

/// Lighting does not touch Paths: they carry no `N`, are drawn by the analytic
/// rasterizer, and render the same picture with a light as without.
#[test]
fn a_path_is_unchanged_by_lighting() {
    let paths = || {
        scene_of(
            pixel_camera(64, 64),
            vec![path_square(8.0, 8.0, 56.0, 56.0, 0.0, RED)],
        )
    };
    let lit = lights(paths(), &[sun([0.0, 0.0, -1.0], 1.0), ambient(0.0)]);
    let lit_frames = both(&lit, 64);
    let plain_frames = both(&paths(), 64);
    for ((renderer, lit), (_, plain)) in lit_frames.iter().zip(&plain_frames) {
        assert!(
            covered(plain) > 2000,
            "{renderer}: the path draws something"
        );
        let (ratio, worst) = agreement(lit, plain, 0.0);
        assert_eq!(
            (ratio, worst),
            (1.0, 0.0),
            "{renderer}: a light changed a Path"
        );
        assert_color(px(lit, 32, 32), RED, TOLERANCE, renderer);
    }
}

/// Lighting multiplies colour, not coverage: a half-transparent quad stays
/// half transparent.
#[test]
fn lighting_leaves_alpha_alone() {
    let glass = Color::new(0.8, 0.8, 0.8, 0.5);
    let scene = lights(
        scene_of(
            pixel_camera(64, 64),
            vec![lit_quad(0.0, 0.0, 64.0, 64.0, 0.0, glass, TOWARD_CAMERA)],
        ),
        &[sun([1.2, 0.0, 1.6], 1.0)],
    );
    assert_shade_at(&scene, 32, 32, [0.8; 3], 0.5);
}

// ---------------------------------------------------------------------------
// The normal under a transform
// ---------------------------------------------------------------------------

/// A slanted quad in the plane spanned by the Y axis and `(1, 0, 1)`, whose
/// outward (camera-side) normal is `(1, 0, -1) / sqrt 2`.
fn slanted() -> Arc<Geometry> {
    let mut geometry = Geometry::from_points3(vec![
        Vec3(0.0, 0.0, 0.0),
        Vec3(20.0, 0.0, 20.0),
        Vec3(20.0, 64.0, 20.0),
        Vec3(0.0, 64.0, 0.0),
    ]);
    let s = std::f32::consts::FRAC_1_SQRT_2;
    facing(Vec3(s, 0.0, -s), &mut geometry);
    geometry
        .points_mut()
        .insert(names::CD, AttributeArray::Color(vec![BASE; 4]))
        .unwrap();
    geometry.push_mesh(0..4, &[0, 1, 2, 0, 2, 3]);
    Arc::new(geometry)
}

fn slanted_scene(transform: Transform3D) -> Scene {
    lights(
        Scene::from_camera(pixel_camera(64, 64))
            .with_object(SceneObject::geometry(slanted(), transform)),
        &[sun([0.0, 0.0, 1.0], 1.0)],
    )
}

/// Stretching `x` by 2 turns the slanted plane's direction `(1, 0, 1)` into
/// `(2, 0, 1)`, so its true normal becomes `(1, 0, -2) / sqrt 5` and a light
/// travelling along `+Z` meets it at `N · L = 2 / sqrt 5 = 0.8944`.
///
/// Moving the stored normal by the model matrix itself would give
/// `(2, 0, -1) / sqrt 5` and `0.4472`: the normal has to move by the inverse
/// transpose to stay perpendicular.
#[test]
fn a_non_uniform_scale_keeps_the_normal_perpendicular_to_the_surface() {
    let scene = slanted_scene(Transform3D {
        scale: [2.0, 1.0, 1.0],
        ..Transform3D::IDENTITY
    });
    // The quad now spans screen x in [0, 40]; pixel (20, 32) is inside it.
    assert_shade_at(&scene, 20, 32, [0.8944; 3], 1.0);
}

/// Mirroring in `x` turns the plane into the one through `(-1, 0, 1)`, whose
/// camera-side normal is `(-1, 0, -1) / sqrt 2`: `N · L = 0.7071`. A mirror
/// flips the sign of the matrix's determinant, and a normal that ignored that
/// would come out pointing away from the camera and light nothing.
#[test]
fn a_mirroring_transform_keeps_the_normal_on_the_camera_side() {
    let scene = slanted_scene(Transform3D {
        translate: [30.0, 0.0, 0.0],
        scale: [-1.0, 1.0, 1.0],
        ..Transform3D::IDENTITY
    });
    // The quad now spans screen x in [10, 30].
    assert_shade_at(&scene, 20, 32, [std::f32::consts::FRAC_1_SQRT_2; 3], 1.0);
}

// ---------------------------------------------------------------------------
// CPU / GPU parity
// ---------------------------------------------------------------------------

/// Two perspective meshes with per-vertex normals that differ across the
/// surface, under an ambient, a directional and a coloured point light, so the
/// interpolation of normal and position and every branch of the shading run.
fn lit_perspective_scene(with_lights: bool) -> Scene {
    let mut bulge = flat_quad(
        -60.0,
        -50.0,
        60.0,
        50.0,
        20.0,
        Color::new(1.0, 0.6, 0.4, 1.0),
    );
    bulge
        .points_mut()
        .insert(
            names::N,
            AttributeArray::Vec3(vec![
                Vec3(-0.5, -0.5, -0.7),
                Vec3(0.5, -0.5, -0.7),
                Vec3(0.5, 0.5, -0.7),
                Vec3(-0.5, 0.5, -0.7),
            ]),
        )
        .unwrap();
    let mut slab = Geometry::from_points3(vec![
        Vec3(-40.0, -40.0, -60.0),
        Vec3(70.0, -30.0, 60.0),
        Vec3(70.0, 40.0, 60.0),
        Vec3(-40.0, 30.0, -60.0),
    ]);
    slab.points_mut()
        .insert(
            names::CD,
            AttributeArray::Color(vec![Color::new(0.3, 0.8, 1.0, 1.0); 4]),
        )
        .unwrap();
    facing(Vec3(0.3, 0.0, -0.95), &mut slab);
    slab.push_mesh(0..4, &[0, 1, 2, 0, 2, 3]);
    // 300 units away: about 0.43 px per unit on a 160 x 120 canvas.
    let camera = Camera::looking_at([0.0, 0.0, -300.0], [0.0, 0.0, 0.0]);
    let scene = scene_of(camera, vec![bulge, slab]);
    if !with_lights {
        return scene;
    }
    lights(
        scene,
        &[
            Light {
                kind: LightKind::Ambient,
                color: [0.2, 0.2, 0.3],
                intensity: 0.5,
            },
            Light {
                kind: LightKind::Directional {
                    direction: [0.5, 0.5, 1.0],
                },
                color: [1.0, 0.9, 0.8],
                intensity: 1.0,
            },
            Light {
                kind: LightKind::Point {
                    position: [80.0, -80.0, -100.0],
                    radius: 150.0,
                },
                color: [0.2, 0.6, 1.0],
                intensity: 1.5,
            },
        ],
    )
}

/// Same bound as `gpu_matches_cpu_for_every_kind_of_piece` — the half-float
/// attachment, fused multiply-add and the edge-flip pixels — with the smooth
/// shading adding nothing measurable of its own.
#[test]
fn gpu_matches_cpu_for_lit_meshes() {
    let scene = lit_perspective_scene(true);
    let cpu = cpu_frame(&scene, 160, 120);
    let gpu = Rig::new().frame(&scene, 160, 120);
    let (ratio, worst) = agreement(&cpu, &gpu, 0.02);
    let mean = cpu
        .as_f32()
        .iter()
        .zip(gpu.as_f32().iter())
        .map(|(a, b)| (a - b).abs())
        .sum::<f32>()
        / cpu.as_f32().len() as f32;
    eprintln!(
        "lit scene: {:.3}% within 0.02, worst {worst:.4}, mean {mean:.5}",
        ratio * 100.0
    );
    assert!(covered(&cpu) > 1500, "the scene actually draws something");
    assert!(worst < 0.25, "the worst pixel differs by {worst}");
    assert!(ratio >= 0.999, "{:.3}% within tolerance", ratio * 100.0);
    assert!(mean < 1e-3, "mean difference {mean}");

    // The parity is not vacuous: the lights changed the picture by far more
    // than the two renderers differ.
    let unlit = cpu_frame(&lit_perspective_scene(false), 160, 120);
    let moved = cpu
        .as_f32()
        .iter()
        .zip(unlit.as_f32().iter())
        .map(|(a, b)| (a - b).abs())
        .sum::<f32>()
        / cpu.as_f32().len() as f32;
    assert!(
        moved > 0.005 && moved > 100.0 * mean,
        "lighting changed the mean by only {moved}"
    );
}
