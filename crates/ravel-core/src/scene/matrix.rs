// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! 4×4 matrices for scene object transforms and camera projection.
//!
//! Rotation itself lives in [`crate::geometry::rotation`]; this module lifts
//! its 3×3 form into the affine 4×4 one and adds translation, scale and
//! projection. The Euler convention is not restated here — that module owns it.
//!
//! # Scene space
//!
//! Scene space is composition space extended with depth: `+X` right, `+Y`
//! **down** (the pixel convention every 2D node already uses — the
//! composition origin is its top-left corner), and `+Z` **away from the
//! viewer**, into the screen. `X × Y = Z` holds, so the basis is
//! right-handed.
//!
//! glTF and OBJ are `+Y` up with `+Z` toward the viewer, so model loading
//! (REQ-3D-008) has to convert on import rather than assume this basis.
//!
//! # Storage
//!
//! Elements are stored **column-major**, matching
//! [`Transform2D`](crate::types::Transform2D): `cols[col * 4 + row]`. A
//! matrix acts on a column vector from the left (`M * v`), so composing a
//! parent transform with a child's reads `parent * child` and
//! [`Mat4::mul`] is not commutative.
//!
//! These matrices are deliberately local to [`crate::scene`]. Element-level
//! rotation (the `orient` quaternion attribute, slerp, look-at on points)
//! belongs to `crate::geometry`, which does not depend on this module.

use crate::geometry::rotation;
use crate::types::Vec3;

/// A 4×4 matrix in column-major storage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat4 {
    /// Column-major elements: `cols[col * 4 + row]`.
    pub cols: [f32; 16],
}

impl Default for Mat4 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Mat4 {
    /// The multiplicative identity.
    pub const IDENTITY: Self = Self {
        cols: [
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 1.0,
        ],
    };

    /// Build from four rows, written the way the mathematics reads.
    ///
    /// `rows[r][c]` is the element at row `r`, column `c`; storage stays
    /// column-major, so this is a transpose of the argument's memory layout.
    pub const fn from_rows(rows: [[f32; 4]; 4]) -> Self {
        let mut cols = [0.0; 16];
        let mut row = 0;
        while row < 4 {
            let mut col = 0;
            while col < 4 {
                cols[col * 4 + row] = rows[row][col];
                col += 1;
            }
            row += 1;
        }
        Self { cols }
    }

    /// Pure translation.
    pub const fn from_translation(t: [f32; 3]) -> Self {
        Self::from_rows([
            [1.0, 0.0, 0.0, t[0]],
            [0.0, 1.0, 0.0, t[1]],
            [0.0, 0.0, 1.0, t[2]],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    /// Pure non-uniform scale.
    pub const fn from_scale(s: [f32; 3]) -> Self {
        Self::from_rows([
            [s[0], 0.0, 0.0, 0.0],
            [0.0, s[1], 0.0, 0.0],
            [0.0, 0.0, s[2], 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    /// Rotation from Euler angles in **degrees**, applied about the fixed
    /// axes in the `Z → Y → X` order of the procedural geometry specification
    /// (an extrinsic ZYX rotation).
    pub fn from_euler_zyx_degrees(euler_degrees: [f32; 3]) -> Self {
        euler_zyx_rotation(euler_degrees)
    }

    /// Element at `row`, `col`.
    ///
    /// # Panics
    /// Panics if `row` or `col` is 4 or greater.
    pub fn element(&self, row: usize, col: usize) -> f32 {
        assert!(row < 4 && col < 4, "Mat4 index out of range");
        self.cols[col * 4 + row]
    }

    /// Matrix product `self * rhs`.
    pub fn mul(&self, rhs: &Self) -> Self {
        let mut cols = [0.0f32; 16];
        for col in 0..4 {
            for row in 0..4 {
                let mut sum = 0.0;
                for k in 0..4 {
                    sum += self.cols[k * 4 + row] * rhs.cols[col * 4 + k];
                }
                cols[col * 4 + row] = sum;
            }
        }
        Self { cols }
    }

    /// Transform a homogeneous 4-vector.
    pub fn transform_vec4(&self, v: [f32; 4]) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for (row, slot) in out.iter_mut().enumerate() {
            *slot = (0..4).map(|k| self.cols[k * 4 + row] * v[k]).sum();
        }
        out
    }

    /// Transform a point (`w = 1`) and project back by the resulting `w`.
    ///
    /// A zero `w` cannot be divided out, so the unprojected components are
    /// returned as-is; callers that care about the difference use
    /// [`Mat4::transform_vec4`].
    pub fn transform_point3(&self, p: [f32; 3]) -> [f32; 3] {
        let v = self.transform_vec4([p[0], p[1], p[2], 1.0]);
        if v[3] == 0.0 {
            return [v[0], v[1], v[2]];
        }
        [v[0] / v[3], v[1] / v[3], v[2] / v[3]]
    }
}

/// Euler angles in degrees → a 4×4 rotation matrix, extrinsic `Z → Y → X`.
///
/// **This is the only place in the crate that turns Euler angles into a
/// matrix**, and it does not do the trigonometry itself: the rotation order is
/// owned by [`crate::geometry::rotation`], which is where the convention is
/// documented and pinned. This function only lifts that 3×3 rotation into the
/// affine 4×4 form the scene layer composes with.
///
/// The order is fixed by `docs/specifications/procedural-geometry.md`: the
/// object turns about the fixed Z axis first, then the fixed Y, then the fixed
/// X, so as matrices acting on a column vector the product is `Rx * Ry * Rz`.
/// Turning about fixed rather than carried axes is what makes this an
/// *extrinsic* ZYX rotation.
fn euler_zyx_rotation(euler_degrees: [f32; 3]) -> Mat4 {
    let rows = rotation::Mat3::from_euler_zyx(Vec3(
        euler_degrees[0].to_radians(),
        euler_degrees[1].to_radians(),
        euler_degrees[2].to_radians(),
    ))
    .rows();

    Mat4::from_rows([
        [rows[0][0], rows[0][1], rows[0][2], 0.0],
        [rows[1][0], rows[1][1], rows[1][2], 0.0],
        [rows[2][0], rows[2][1], rows[2][2], 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

/// Vector helpers. Scene space is only ever three-dimensional here, so these
/// stay plain arrays rather than growing a vector type the crate already has
/// two of.
pub(super) mod vec3 {
    pub fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }

    pub fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    pub fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    pub fn length(a: [f32; 3]) -> f32 {
        dot(a, a).sqrt()
    }

    /// Normalize `a`, or return `fallback` when `a` is degenerate (zero or
    /// non-finite). Cameras take user-authored, animated vectors, so a
    /// coincident position/target must produce a usable matrix rather than
    /// NaNs.
    pub fn normalize_or(a: [f32; 3], fallback: [f32; 3]) -> [f32; 3] {
        let len = length(a);
        if len.is_finite() && len > 1e-6 {
            [a[0] / len, a[1] / len, a[2] / len]
        } else {
            fallback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-5,
            "{what}: expected {expected}, got {actual}"
        );
    }

    #[test]
    fn from_rows_stores_column_major() {
        let m = Mat4::from_rows([
            [0.0, 1.0, 2.0, 3.0],
            [4.0, 5.0, 6.0, 7.0],
            [8.0, 9.0, 10.0, 11.0],
            [12.0, 13.0, 14.0, 15.0],
        ]);
        // Row 0 is spread across the four columns, one element each.
        assert_eq!(m.cols[0], 0.0);
        assert_eq!(m.cols[4], 1.0);
        assert_eq!(m.cols[8], 2.0);
        assert_eq!(m.cols[12], 3.0);
        assert_eq!(m.element(1, 2), 6.0);
        assert_eq!(m.element(3, 0), 12.0);
    }

    #[test]
    fn identity_leaves_points_untouched() {
        assert_eq!(
            Mat4::IDENTITY.transform_point3([3.0, -4.0, 5.0]),
            [3.0, -4.0, 5.0]
        );
    }

    #[test]
    fn translation_then_scale_composes_left_to_right() {
        let t = Mat4::from_translation([10.0, 0.0, 0.0]);
        let s = Mat4::from_scale([2.0, 2.0, 2.0]);
        // `t * s` scales first, then translates.
        assert_eq!(
            t.mul(&s).transform_point3([1.0, 0.0, 0.0]),
            [12.0, 0.0, 0.0]
        );
        // `s * t` translates first, then scales the whole thing.
        assert_eq!(
            s.mul(&t).transform_point3([1.0, 0.0, 0.0]),
            [22.0, 0.0, 0.0]
        );
    }

    /// A 90° Z rotation takes `+X` to `+Y`, which in scene space (Y down) is
    /// the same sense the existing 2D `geometry.transform` rotation has.
    #[test]
    fn z_rotation_matches_the_two_dimensional_convention() {
        let r = Mat4::from_euler_zyx_degrees([0.0, 0.0, 90.0]);
        let p = r.transform_point3([1.0, 0.0, 0.0]);
        assert_close(p[0], 0.0, "x");
        assert_close(p[1], 1.0, "y");
        assert_close(p[2], 0.0, "z");
    }

    /// Independent reference for the rotation order: turn `v` about the fixed
    /// Z axis, then the fixed Y, then the fixed X, one axis at a time with
    /// scalar trigonometry.
    ///
    /// Deliberately shares no code with [`Mat4`] — the point is to check the
    /// matrix product against a separate expression of the specification, not
    /// against itself.
    fn rotate_zyx_reference(euler_degrees: [f32; 3], v: [f32; 3]) -> [f32; 3] {
        let [ax, ay, az] = euler_degrees.map(f32::to_radians);
        let (sz, cz) = az.sin_cos();
        let v = [v[0] * cz - v[1] * sz, v[0] * sz + v[1] * cz, v[2]];
        let (sy, cy) = ay.sin_cos();
        let v = [v[0] * cy + v[2] * sy, v[1], -v[0] * sy + v[2] * cy];
        let (sx, cx) = ax.sin_cos();
        [v[0], v[1] * cx - v[2] * sx, v[1] * sx + v[2] * cx]
    }

    /// The `Z → Y → X` order is fixed, checked with **all three angles
    /// non-zero and different from each other** — the case that actually
    /// separates the six orderings. A single zero angle, or two equal ones,
    /// leaves several wrong products agreeing with the right one.
    #[test]
    fn euler_order_is_extrinsic_zyx() {
        let probes = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [3.0, -5.0, 7.0],
        ];
        for angles in [
            [17.0f32, 43.0, 71.0],
            [-30.0, 60.0, -120.0],
            [90.0, 180.0, 90.0],
            [10.0, 20.0, 30.0],
        ] {
            let matrix = Mat4::from_euler_zyx_degrees(angles);
            for probe in probes {
                let expected = rotate_zyx_reference(angles, probe);
                let actual = matrix.transform_point3(probe);
                for axis in 0..3 {
                    assert_close(
                        actual[axis],
                        expected[axis],
                        &format!("{angles:?} on {probe:?} component {axis}"),
                    );
                }
            }
        }
    }

    /// Only `Rx · Ry · Rz` reproduces the conversion. The other five orderings
    /// of the same three single-axis rotations must all disagree, so a
    /// transposed or permuted product cannot pass.
    #[test]
    fn no_other_ordering_of_the_axis_rotations_matches() {
        let angles = [17.0f32, 43.0, 71.0];
        let rx = Mat4::from_euler_zyx_degrees([angles[0], 0.0, 0.0]);
        let ry = Mat4::from_euler_zyx_degrees([0.0, angles[1], 0.0]);
        let rz = Mat4::from_euler_zyx_degrees([0.0, 0.0, angles[2]]);
        let zyx = Mat4::from_euler_zyx_degrees(angles);

        assert_eq!(zyx, rx.mul(&ry).mul(&rz), "the fixed order is Rx · Ry · Rz");
        for (label, wrong) in [
            ("Rx·Rz·Ry", rx.mul(&rz).mul(&ry)),
            ("Ry·Rx·Rz", ry.mul(&rx).mul(&rz)),
            ("Ry·Rz·Rx", ry.mul(&rz).mul(&rx)),
            ("Rz·Rx·Ry", rz.mul(&rx).mul(&ry)),
            ("Rz·Ry·Rx", rz.mul(&ry).mul(&rx)),
        ] {
            assert_ne!(zyx, wrong, "{label} must not match the ZYX conversion");
        }
    }

    /// The single-axis cases stay pinned separately, so a failure says whether
    /// the per-axis matrices or their composition order is wrong.
    #[test]
    fn a_single_non_zero_angle_rotates_about_that_axis_only() {
        let zyx = Mat4::from_euler_zyx_degrees([0.0, 90.0, 90.0]);
        let ry = Mat4::from_euler_zyx_degrees([0.0, 90.0, 0.0]);
        let rz = Mat4::from_euler_zyx_degrees([0.0, 0.0, 90.0]);
        assert_eq!(zyx, ry.mul(&rz));
        assert_ne!(zyx, rz.mul(&ry));
    }

    #[test]
    fn euler_rotations_compose_per_axis() {
        // 90° about X takes +Y to +Z.
        let rx = Mat4::from_euler_zyx_degrees([90.0, 0.0, 0.0]);
        let p = rx.transform_point3([0.0, 1.0, 0.0]);
        assert_close(p[1], 0.0, "y");
        assert_close(p[2], 1.0, "z");

        // 90° about Y takes +Z to +X.
        let ry = Mat4::from_euler_zyx_degrees([0.0, 90.0, 0.0]);
        let q = ry.transform_point3([0.0, 0.0, 1.0]);
        assert_close(q[0], 1.0, "x");
        assert_close(q[2], 0.0, "z");
    }

    #[test]
    fn zero_rotation_is_the_identity() {
        assert_eq!(
            Mat4::from_euler_zyx_degrees([0.0, 0.0, 0.0]),
            Mat4::IDENTITY
        );
    }

    #[test]
    fn transform_point_divides_by_w() {
        let m = Mat4::from_rows([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 2.0],
        ]);
        assert_eq!(m.transform_point3([4.0, 6.0, 8.0]), [2.0, 3.0, 4.0]);
    }

    #[test]
    fn degenerate_vectors_normalize_to_the_fallback() {
        assert_eq!(
            vec3::normalize_or([0.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            [0.0, 0.0, 1.0]
        );
        assert_eq!(
            vec3::normalize_or([f32::NAN, 0.0, 0.0], [1.0, 0.0, 0.0]),
            [1.0, 0.0, 0.0]
        );
        assert_eq!(
            vec3::normalize_or([0.0, 4.0, 0.0], [1.0, 0.0, 0.0]),
            [0.0, 1.0, 0.0]
        );
    }
}
