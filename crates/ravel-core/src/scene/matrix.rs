// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! 4×4 matrices for scene object transforms and camera projection.
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
/// matrix.** The rotation order is fixed by
/// `docs/specifications/procedural-geometry.md` and pinned by tests: the
/// object is rotated about the fixed Z axis first, then the fixed Y, then the
/// fixed X, so as matrices acting on a column vector the product is
/// `Rx * Ry * Rz`. Turning about fixed rather than carried axes is what makes
/// this an *extrinsic* ZYX rotation.
fn euler_zyx_rotation(euler_degrees: [f32; 3]) -> Mat4 {
    let (sx, cx) = euler_degrees[0].to_radians().sin_cos();
    let (sy, cy) = euler_degrees[1].to_radians().sin_cos();
    let (sz, cz) = euler_degrees[2].to_radians().sin_cos();

    let rx = Mat4::from_rows([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, cx, -sx, 0.0],
        [0.0, sx, cx, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);
    let ry = Mat4::from_rows([
        [cy, 0.0, sy, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [-sy, 0.0, cy, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);
    let rz = Mat4::from_rows([
        [cz, -sz, 0.0, 0.0],
        [sz, cz, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]);
    rx.mul(&ry).mul(&rz)
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

    /// The `Z → Y → X` order is fixed. With 90° on Z and Y the two orders
    /// disagree, so this pins the extrinsic ZYX composition against the
    /// reverse (`Rz * Ry * Rx`).
    #[test]
    fn euler_order_is_extrinsic_zyx() {
        let zyx = Mat4::from_euler_zyx_degrees([0.0, 90.0, 90.0]);
        let p = zyx.transform_point3([1.0, 0.0, 0.0]);
        // Z first: (1,0,0) → (0,1,0). Then Y: (0,1,0) is on the Y axis and is
        // unchanged by a Y rotation. X last: no rotation about X here.
        assert_close(p[0], 0.0, "x");
        assert_close(p[1], 1.0, "y");
        assert_close(p[2], 0.0, "z");

        let ry = Mat4::from_euler_zyx_degrees([0.0, 90.0, 0.0]);
        let rz = Mat4::from_euler_zyx_degrees([0.0, 0.0, 90.0]);
        // Extrinsic ZYX means `Ry * Rz` for these two, not `Rz * Ry`.
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
