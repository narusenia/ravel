// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scene cameras and their view / projection matrices (REQ-3D-002).
//!
//! A camera only ever *computes* matrices. Projection never rewrites the `P`
//! attribute of a geometry — the position of a point has exactly one source
//! (`docs/specifications/procedural-geometry.md`), and turning scene space
//! into pixels is `scene.render`'s job.

use crate::eval::EvalContext;
use crate::scene::matrix::{Mat4, vec3};

/// How a camera maps scene space onto the image plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Projection {
    /// Perspective projection with a vertical field of view in degrees.
    Perspective { fov_y_degrees: f32 },
    /// Orthographic projection covering `height` composition units
    /// vertically. The horizontal extent follows the aspect ratio.
    Orthographic { height: f32 },
}

/// Projection kind as it is spelled in the `scene.camera` `projection`
/// parameter and in `.ravprj`.
pub const PROJECTION_PERSPECTIVE: &str = "perspective";
/// See [`PROJECTION_PERSPECTIVE`].
pub const PROJECTION_ORTHOGRAPHIC: &str = "orthographic";
/// Every accepted value of the `scene.camera` `projection` parameter, in the
/// order the Properties dropdown lists them.
pub const PROJECTION_KINDS: [&str; 2] = [PROJECTION_PERSPECTIVE, PROJECTION_ORTHOGRAPHIC];

/// A camera positioned in scene space and looking at a target point.
///
/// Every field is a plain value: the animation of the underlying parameters
/// happens in the graph, and the camera is what one frame's resolved
/// parameters evaluate to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Eye position in scene space.
    pub position: [f32; 3],
    /// Point the camera looks at, in scene space.
    pub target: [f32; 3],
    /// Scene-space direction that maps to the **top** of the rendered image.
    /// Scene space is Y-down, so the default is `-Y`.
    pub up: [f32; 3],
    /// Perspective or orthographic.
    pub projection: Projection,
    /// Near clip distance, measured along the view direction.
    pub near: f32,
    /// Far clip distance, measured along the view direction.
    pub far: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            position: [0.0, 0.0, -DEFAULT_DISTANCE],
            target: [0.0, 0.0, 0.0],
            up: [0.0, -1.0, 0.0],
            projection: Projection::Perspective {
                fov_y_degrees: DEFAULT_FOV_Y_DEGREES,
            },
            near: DEFAULT_NEAR,
            far: DEFAULT_FAR,
        }
    }
}

/// Default eye distance from the composition plane, in composition units.
/// Scene coordinates are pixel-scaled, so the default has to be of the order
/// of a composition's own size rather than of order 1.
pub const DEFAULT_DISTANCE: f32 = 1000.0;
/// Default vertical field of view, in degrees.
pub const DEFAULT_FOV_Y_DEGREES: f32 = 50.0;
/// Default orthographic vertical extent, in composition units.
pub const DEFAULT_ORTHOGRAPHIC_HEIGHT: f32 = 1080.0;
/// Smallest clip-range thickness a projection is built with.
///
/// `near` and `far` are independent parameters, so an inverted or empty range
/// is reachable from the UI (and from a hand-edited project). Widening to this
/// keeps the depth mapping oriented near → 0, far → 1 instead of silently
/// reversing it.
pub const MIN_CLIP_SPAN: f32 = 1e-4;
/// Default near clip distance.
pub const DEFAULT_NEAR: f32 = 1.0;
/// Default far clip distance.
pub const DEFAULT_FAR: f32 = 10_000.0;

/// Aspect ratio (width / height) a scene is projected with.
///
/// Taken from [`EvalContext::comp_resolution`], not `resolution`: scene
/// coordinates are composition coordinates, so the projection has to describe
/// the composition's shape. `resolution` is the output canvas, and rendering
/// to a half-size preview must not restretch the image — that scale is
/// applied when pixels are produced
/// ([`EvalContext::comp_to_canvas_scale`]).
///
/// A degenerate composition height falls back to `1.0` so a projection matrix
/// never carries an infinity.
pub fn aspect_ratio(ctx: &EvalContext) -> f32 {
    let (width, height) = ctx.comp_resolution;
    if width == 0 || height == 0 {
        return 1.0;
    }
    width as f32 / height as f32
}

impl Camera {
    /// A camera looking at `target` from `position` with default optics.
    pub fn looking_at(position: [f32; 3], target: [f32; 3]) -> Self {
        Self {
            position,
            target,
            ..Self::default()
        }
    }

    /// Replace the projection.
    pub fn with_projection(mut self, projection: Projection) -> Self {
        self.projection = projection;
        self
    }

    /// Replace the clip range.
    pub fn with_clip_range(mut self, near: f32, far: f32) -> Self {
        self.near = near;
        self.far = far;
        self
    }

    /// Scene space → view space.
    ///
    /// The camera basis is `x` right, `y` up in the image, `z` along the view
    /// direction, so a point in front of the camera has a **positive** view-space
    /// `z` equal to its distance along that direction. Together with the
    /// projection matrices below that is the standard left-handed camera on a
    /// right-handed world, which is what maps directly onto wgpu's clip space.
    ///
    /// A coincident `position` and `target`, or an `up` parallel to the view
    /// direction, are resolved to a usable basis rather than producing NaNs.
    pub fn view_matrix(&self) -> Mat4 {
        let forward = vec3::normalize_or(
            vec3::sub(self.target, self.position),
            [0.0, 0.0, 1.0], // looking into the screen
        );
        let up = vec3::normalize_or(self.up, [0.0, -1.0, 0.0]);
        let mut right = vec3::cross(forward, up);
        if vec3::length(right) <= 1e-6 {
            // `up` is parallel to the view direction; any perpendicular axis
            // gives a valid basis, and Z is perpendicular to every up vector
            // that could have caused this except one, which X then covers.
            let alternative = if forward[2].abs() < 0.9 {
                [0.0, 0.0, 1.0]
            } else {
                [1.0, 0.0, 0.0]
            };
            right = vec3::cross(forward, alternative);
        }
        let right = vec3::normalize_or(right, [1.0, 0.0, 0.0]);
        let image_up = vec3::cross(right, forward);

        Mat4::from_rows([
            [
                right[0],
                right[1],
                right[2],
                -vec3::dot(right, self.position),
            ],
            [
                image_up[0],
                image_up[1],
                image_up[2],
                -vec3::dot(image_up, self.position),
            ],
            [
                forward[0],
                forward[1],
                forward[2],
                -vec3::dot(forward, self.position),
            ],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    /// The clip range this camera actually projects with: `near` as authored,
    /// `far` guaranteed to sit beyond it by at least [`MIN_CLIP_SPAN`].
    ///
    /// `near` and `far` are independent parameters with independent ranges, so
    /// `near >= far` is reachable — a user drags `near` past `far`, or a
    /// hand-edited project says so. Left alone that **inverts the depth
    /// mapping**: the far plane lands at NDC 0 and the near plane at 1, so
    /// every depth comparison downstream is backwards. `near` is kept as
    /// authored because it is the eye-side plane being positioned; only `far`
    /// moves. The widening is relative so it survives a `near` large enough
    /// that adding an absolute epsilon would round back to `near`.
    ///
    /// `scene.render` reads the range through this so the depth attachment and
    /// the projection matrix cannot disagree.
    pub fn clip_range(&self) -> (f32, f32) {
        let near = if self.near.is_finite() {
            self.near
        } else {
            DEFAULT_NEAR
        };
        let far = if self.far.is_finite() {
            self.far
        } else {
            DEFAULT_FAR
        };
        let span = MIN_CLIP_SPAN.max(near.abs() * f32::EPSILON * 8.0);
        (near, far.max(near + span))
    }

    /// View space → clip space for the given `aspect` (width / height).
    ///
    /// Clip space is wgpu's: NDC `x` and `y` in `[-1, 1]` with `y` up, and
    /// depth in `[0, 1]` with `near` at 0 and `far` at 1.
    ///
    /// Degenerate optics are made harmless rather than propagated: a
    /// non-positive or non-finite aspect becomes 1, a field of view at or past
    /// 180° is clamped, an empty or inverted clip range is widened
    /// ([`Camera::clip_range`]), and the depth divisor is floored at
    /// [`MIN_CLIP_SPAN`] so it can never be zero or negative. Every element of
    /// the result is finite for every input.
    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        let aspect = if aspect.is_finite() && aspect > 1e-6 {
            aspect
        } else {
            1.0
        };
        let (near, far) = self.clip_range();
        // `clip_range` already guarantees `far > near`; the floor closes the
        // residual case where the difference underflows to zero anyway.
        let depth = (far - near).max(MIN_CLIP_SPAN);

        match self.projection {
            Projection::Perspective { fov_y_degrees } => {
                let fov = fov_y_degrees.clamp(1e-3, 179.999);
                let focal = 1.0 / (fov.to_radians() * 0.5).tan();
                Mat4::from_rows([
                    [focal / aspect, 0.0, 0.0, 0.0],
                    [0.0, focal, 0.0, 0.0],
                    [0.0, 0.0, far / depth, -(near * far) / depth],
                    [0.0, 0.0, 1.0, 0.0],
                ])
            }
            Projection::Orthographic { height } => {
                let half_height = (height * 0.5).abs().max(1e-6);
                let half_width = half_height * aspect;
                Mat4::from_rows([
                    [1.0 / half_width, 0.0, 0.0, 0.0],
                    [0.0, 1.0 / half_height, 0.0, 0.0],
                    [0.0, 0.0, 1.0 / depth, -near / depth],
                    [0.0, 0.0, 0.0, 1.0],
                ])
            }
        }
    }

    /// [`Camera::projection_matrix`] with the aspect ratio the composition
    /// implies ([`aspect_ratio`]).
    pub fn projection_matrix_for(&self, ctx: &EvalContext) -> Mat4 {
        self.projection_matrix(aspect_ratio(ctx))
    }

    /// `projection * view`: scene space → clip space in one matrix.
    pub fn view_projection_matrix(&self, aspect: f32) -> Mat4 {
        self.projection_matrix(aspect).mul(&self.view_matrix())
    }

    /// [`Camera::view_projection_matrix`] with the composition's aspect ratio.
    pub fn view_projection_matrix_for(&self, ctx: &EvalContext) -> Mat4 {
        self.view_projection_matrix(aspect_ratio(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FrameRate;

    fn ctx(comp: (u32, u32)) -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), comp)
    }

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-4,
            "{what}: expected {expected}, got {actual}"
        );
    }

    /// The default camera sits in front of the composition plane and looks
    /// into the screen, so the plane lands at its eye distance in view space.
    #[test]
    fn default_view_matrix_puts_the_composition_plane_ahead() {
        let view = Camera::default().view_matrix();
        let origin = view.transform_point3([0.0, 0.0, 0.0]);
        assert_close(origin[0], 0.0, "x");
        assert_close(origin[1], 0.0, "y");
        assert_close(origin[2], DEFAULT_DISTANCE, "depth ahead of the camera");

        // Image right is scene `+X`; image up is scene `-Y`, because scene
        // space is Y-down.
        let right = view.transform_point3([100.0, 0.0, 0.0]);
        assert_close(right[0], 100.0, "scene +X is image right");
        let below = view.transform_point3([0.0, 100.0, 0.0]);
        assert_close(below[1], -100.0, "scene +Y is image down");
    }

    #[test]
    fn view_matrix_survives_a_coincident_position_and_target() {
        let camera = Camera::looking_at([5.0, 5.0, 5.0], [5.0, 5.0, 5.0]);
        for element in camera.view_matrix().cols {
            assert!(element.is_finite(), "view matrix must stay finite");
        }
    }

    #[test]
    fn view_matrix_survives_an_up_parallel_to_the_view_direction() {
        let mut camera = Camera::looking_at([0.0, 0.0, -10.0], [0.0, 0.0, 0.0]);
        camera.up = [0.0, 0.0, 1.0];
        let view = camera.view_matrix();
        for element in view.cols {
            assert!(element.is_finite(), "view matrix must stay finite");
        }
        // Still a distance-preserving basis: the target is `near`-ward at 10.
        assert_close(view.transform_point3([0.0, 0.0, 0.0])[2], 10.0, "depth");
    }

    /// Perspective matrix elements against hand-computed values, and the
    /// depth range mapping near → 0, far → 1.
    #[test]
    fn perspective_projection_matches_known_values() {
        let camera = Camera::default()
            .with_projection(Projection::Perspective {
                fov_y_degrees: 90.0,
            })
            .with_clip_range(1.0, 10_000.0);
        let p = camera.projection_matrix(16.0 / 9.0);

        // focal = 1 / tan(45°) = 1.
        assert_close(p.element(0, 0), 9.0 / 16.0, "m00");
        assert_close(p.element(1, 1), 1.0, "m11");
        assert_close(p.element(2, 2), 10_000.0 / 9_999.0, "m22");
        assert_close(p.element(2, 3), -10_000.0 / 9_999.0, "m23");
        assert_close(p.element(3, 2), 1.0, "m32 (w picks up view-space z)");
        assert_close(p.element(3, 3), 0.0, "m33");

        // near → 0, far → 1 in NDC depth.
        assert_close(p.transform_point3([0.0, 0.0, 1.0])[2], 0.0, "near depth");
        assert_close(
            p.transform_point3([0.0, 0.0, 10_000.0])[2],
            1.0,
            "far depth",
        );
        // A 90° vertical field of view puts view-space (0, z, z) on the top
        // edge of the frame.
        assert_close(
            p.transform_point3([0.0, 100.0, 100.0])[1],
            1.0,
            "frame top edge",
        );
    }

    #[test]
    fn orthographic_projection_matches_known_values() {
        let camera = Camera::default()
            .with_projection(Projection::Orthographic { height: 1000.0 })
            .with_clip_range(1.0, 1001.0);
        let p = camera.projection_matrix(2.0);

        // half height 500, half width 1000.
        assert_close(p.element(0, 0), 0.001, "m00");
        assert_close(p.element(1, 1), 0.002, "m11");
        assert_close(p.element(2, 2), 0.001, "m22");
        assert_close(p.element(2, 3), -0.001, "m23");
        assert_close(p.element(3, 2), 0.0, "m32 (no perspective divide)");
        assert_close(p.element(3, 3), 1.0, "m33");

        assert_close(p.transform_point3([0.0, 0.0, 1.0])[2], 0.0, "near depth");
        assert_close(p.transform_point3([0.0, 0.0, 1001.0])[2], 1.0, "far depth");
        // Depth does not shrink an orthographic image.
        let near_edge = p.transform_point3([1000.0, 500.0, 1.0]);
        let far_edge = p.transform_point3([1000.0, 500.0, 1001.0]);
        assert_close(near_edge[0], 1.0, "right edge at the near plane");
        assert_close(far_edge[0], 1.0, "right edge at the far plane");
        assert_close(near_edge[1], 1.0, "top edge at the near plane");
    }

    /// The aspect ratio comes from the composition resolution, so the same
    /// camera projects differently in a 16:9 and a 1:1 composition — and the
    /// output resolution does not enter into it.
    #[test]
    fn aspect_ratio_follows_the_composition_resolution() {
        assert_close(aspect_ratio(&ctx((1920, 1080))), 16.0 / 9.0, "16:9");
        assert_close(aspect_ratio(&ctx((1000, 1000))), 1.0, "square");
        assert_close(aspect_ratio(&ctx((0, 0))), 1.0, "degenerate fallback");

        let camera = Camera::default();
        let wide = camera.projection_matrix_for(&ctx((1920, 1080)));
        let square = camera.projection_matrix_for(&ctx((1080, 1080)));
        assert_ne!(wide, square);
        assert_close(
            wide.element(0, 0) * (16.0 / 9.0),
            square.element(0, 0),
            "horizontal scale is divided by the aspect ratio",
        );
        assert_close(
            wide.element(1, 1),
            square.element(1, 1),
            "vertical field of view is independent of the aspect ratio",
        );

        // A half-size output canvas is a rendering scale, not a reprojection.
        let preview = ctx((1920, 1080));
        let half = EvalContext::new(0, FrameRate::new(30, 1), (960, 540))
            .with_comp_resolution((1920, 1080));
        assert_eq!(
            camera.projection_matrix_for(&preview),
            camera.projection_matrix_for(&half)
        );
    }

    #[test]
    fn view_projection_is_the_projection_times_the_view() {
        let camera = Camera::looking_at([0.0, 0.0, -500.0], [0.0, 0.0, 0.0]);
        let aspect = 16.0 / 9.0;
        assert_eq!(
            camera.view_projection_matrix(aspect),
            camera.projection_matrix(aspect).mul(&camera.view_matrix())
        );
        assert_eq!(
            camera.view_projection_matrix_for(&ctx((1920, 1080))),
            camera.view_projection_matrix(16.0 / 9.0)
        );
    }

    /// An inverted or empty clip range is widened instead of reversing the
    /// depth mapping. `near` stays as authored; only `far` moves.
    #[test]
    fn an_inverted_clip_range_is_widened_rather_than_reversed() {
        for (near, far) in [(100.0f32, 10.0f32), (5.0, 5.0), (1.0, -100.0)] {
            let camera = Camera::default().with_clip_range(near, far);
            let (effective_near, effective_far) = camera.clip_range();
            assert_eq!(effective_near, near, "near is authoritative");
            assert!(
                effective_far > effective_near,
                "far must sit beyond near: {effective_near} .. {effective_far}"
            );

            for projection in [
                Projection::Perspective {
                    fov_y_degrees: 60.0,
                },
                Projection::Orthographic { height: 1000.0 },
            ] {
                let matrix = camera.with_projection(projection).projection_matrix(2.0);
                for element in matrix.cols {
                    assert!(
                        element.is_finite(),
                        "{near}..{far} produced a non-finite element"
                    );
                }
                // The near plane still maps to 0 and the far plane to 1 — the
                // orientation a depth test downstream relies on.
                let at_near = matrix.transform_point3([0.0, 0.0, effective_near])[2];
                let at_far = matrix.transform_point3([0.0, 0.0, effective_far])[2];
                assert!(
                    at_far > at_near,
                    "depth must increase with distance: {at_near} then {at_far}"
                );
            }
        }
    }

    /// A very large `near` still leaves room for `far`: widening by an absolute
    /// epsilon would round straight back to `near` and divide by zero.
    #[test]
    fn a_large_near_still_yields_a_positive_clip_span() {
        let camera = Camera::default().with_clip_range(1e9, 1e9);
        let (near, far) = camera.clip_range();
        assert!(far > near, "{near} .. {far}");
        for element in camera.projection_matrix(2.0).cols {
            assert!(element.is_finite(), "large near produced {element}");
        }
    }

    /// Every corner of the declared parameter ranges produces a finite matrix,
    /// in both projections. The registry hard ranges are `near`/`far` in
    /// `1e-4..=1e9`, `fov` in `1e-3..=179.999`, `ortho_height` in `1e-3..=1e9`.
    #[test]
    fn the_declared_parameter_extremes_stay_finite() {
        for near in [1e-4f32, 1.0, 1e9] {
            for far in [1e-4f32, 1.0, 1e9] {
                for aspect in [1e-6f32, 1.0, 1e9, f32::NAN, 0.0, -2.0] {
                    for projection in [
                        Projection::Perspective {
                            fov_y_degrees: 1e-3,
                        },
                        Projection::Perspective {
                            fov_y_degrees: 179.999,
                        },
                        Projection::Orthographic { height: 1e-3 },
                        Projection::Orthographic { height: 1e9 },
                    ] {
                        let matrix = Camera::default()
                            .with_clip_range(near, far)
                            .with_projection(projection)
                            .projection_matrix(aspect);
                        for element in matrix.cols {
                            assert!(
                                element.is_finite(),
                                "near={near} far={far} aspect={aspect} \
                                 projection={projection:?} produced {element}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn degenerate_optics_stay_finite() {
        let camera = Camera::default()
            .with_projection(Projection::Perspective { fov_y_degrees: 0.0 })
            .with_clip_range(5.0, 5.0);
        for element in camera.projection_matrix(0.0).cols {
            assert!(element.is_finite(), "perspective must stay finite");
        }
        let ortho = Camera::default().with_projection(Projection::Orthographic { height: 0.0 });
        for element in ortho.projection_matrix(f32::NAN).cols {
            assert!(element.is_finite(), "orthographic must stay finite");
        }
    }
}
