// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Rotation math: quaternions, Euler angles, and 3x3 rotation matrices.
//!
//! This module owns rotation for the whole repository — Euler conversion,
//! quaternion composition, slerp, and the matrix form. Anything that needs to
//! turn an authored rotation into an applied one goes through here instead of
//! open-coding the trigonometry, so the conventions below hold in one place.
//!
//! Conventions, all fixed by the "回転の表現" section of
//! `docs/specifications/procedural-geometry.md` (REQ-3D-003):
//!
//! - **Euler order is ZYX, extrinsic (fixed axes): Z is applied first, then Y,
//!   then X.** As a matrix product on column vectors that is `Rx * Ry * Rz`.
//!   The equivalent intrinsic sequence is X, then Y', then Z''. This is pinned
//!   by tests and **must not change later** — the pose of every already saved
//!   project depends on it.
//! - **Angles are radians**, and a `Vec3` of Euler angles is `(x, y, z)`:
//!   component `i` is the rotation about axis `i`, not the order of
//!   application.
//! - **Right-handed**, so a positive angle rotates counter-clockwise looking
//!   down the positive axis toward the origin. A 90° rotation about Y maps
//!   `+x` onto `-z`.
//! - [`Mat3`] is **row-major** and multiplies **column vectors** on the left:
//!   `m * v` rotates `v`.
//!
//! No 4x4 transform type lives here on purpose: rotation stops at 3x3, and the
//! full affine/camera transform belongs to the scene layer.

use crate::types::{Vec3, Vec4};

/// Below this the cosine of the Y (pitch) angle counts as zero and the X / Z
/// split of a ZYX Euler decomposition is degenerate. `1e-6` on a cosine is
/// about 0.00006° away from ±90°, which is where the row the split is read
/// from has decayed into `f32` rounding noise.
const GIMBAL_EPSILON: f32 = 1e-6;

/// Above this dot product two quaternions are close enough that `slerp` falls
/// back to normalized linear interpolation, where `sin(theta)` would otherwise
/// divide away the precision.
const SLERP_LINEAR_DOT: f32 = 0.999_5;

/// Whether a magnitude can be divided by. Only an exact zero and the
/// non-finite values are unusable: `1.0 / 1e-8` is an ordinary `f32`, so a tiny
/// but non-zero length still carries a real direction. Comparing against
/// `f32::EPSILON` instead would throw away every magnitude below 1.2e-7 — and,
/// applied to a squared length, below 3.5e-4 — which are rotations, not
/// degeneracies. The three guards in this module share this one criterion.
fn is_divisible(magnitude: f32) -> bool {
    magnitude != 0.0 && magnitude.is_finite()
}

/// A rotation quaternion, stored **`(x, y, z, w)`** — the vector part first and
/// the scalar part last.
///
/// The order matches the `orient` attribute's `Vec4` column element for
/// element (see [`Quat::from_vec4`]), so moving between the attribute and this
/// type never permutes components. Rotation operations assume a unit
/// quaternion; [`Quat::normalized`] restores that after accumulating products.
///
/// Limitation: [`Quat::length`] and [`Quat::length_squared`] sum squares
/// without rescaling, so a component beyond about 1e19 overflows to infinity
/// and the magnitude-dependent operations fall back to the identity. Values
/// that large are not rotations, and an `orient` column holds unit
/// quaternions, so nothing real is given up by not rescaling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quat(pub f32, pub f32, pub f32, pub f32);

impl Default for Quat {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Quat {
    /// The identity rotation, `(0, 0, 0, 1)`.
    pub const IDENTITY: Self = Self(0.0, 0.0, 0.0, 1.0);

    /// Reads a quaternion out of an `orient` column element (`x, y, z, w`).
    pub fn from_vec4(v: Vec4) -> Self {
        Self(v.0, v.1, v.2, v.3)
    }

    /// Writes the quaternion into an `orient` column element (`x, y, z, w`).
    pub fn to_vec4(self) -> Vec4 {
        Vec4(self.0, self.1, self.2, self.3)
    }

    /// Builds a rotation from ZYX Euler angles in radians (`(x, y, z)`
    /// components, applied Z then Y then X).
    pub fn from_euler_zyx(euler: Vec3) -> Self {
        let (sx, cx) = (euler.0 * 0.5).sin_cos();
        let (sy, cy) = (euler.1 * 0.5).sin_cos();
        let (sz, cz) = (euler.2 * 0.5).sin_cos();
        // qx * qy * qz, expanded.
        Self(
            sx * cy * cz + cx * sy * sz,
            cx * sy * cz - sx * cy * sz,
            cx * cy * sz + sx * sy * cz,
            cx * cy * cz - sx * sy * sz,
        )
    }

    /// Recovers ZYX Euler angles in radians. See [`Mat3::to_euler_zyx`] for
    /// the behaviour near the ±90° Y degeneracy, which this shares by going
    /// through the matrix form.
    pub fn to_euler_zyx(self) -> Vec3 {
        self.to_mat3().to_euler_zyx()
    }

    /// Builds a rotation of `angle` radians about `axis`. The axis is
    /// normalized, so only an exactly zero-length one yields the identity —
    /// there is no direction to rotate about. Axis components beyond about
    /// 1e19 overflow the sum of squares and also fall back to the identity.
    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Self {
        let length = (axis.0 * axis.0 + axis.1 * axis.1 + axis.2 * axis.2).sqrt();
        if !is_divisible(length) {
            return Self::IDENTITY;
        }
        let (s, c) = (angle * 0.5).sin_cos();
        let k = s / length;
        Self(axis.0 * k, axis.1 * k, axis.2 * k, c)
    }

    /// Composes two rotations: `self * rhs` applies `rhs` first, then `self`.
    pub fn mul_quat(self, rhs: Self) -> Self {
        let (x1, y1, z1, w1) = (self.0, self.1, self.2, self.3);
        let (x2, y2, z2, w2) = (rhs.0, rhs.1, rhs.2, rhs.3);
        Self(
            w1 * x2 + x1 * w2 + y1 * z2 - z1 * y2,
            w1 * y2 - x1 * z2 + y1 * w2 + z1 * x2,
            w1 * z2 + x1 * y2 - y1 * x2 + z1 * w2,
            w1 * w2 - x1 * x2 - y1 * y2 - z1 * z2,
        )
    }

    /// Four-component dot product. Its sign says which of the two antipodal
    /// representations of `rhs` is on the short arc from `self`.
    pub fn dot(self, rhs: Self) -> f32 {
        self.0 * rhs.0 + self.1 * rhs.1 + self.2 * rhs.2 + self.3 * rhs.3
    }

    /// Squared magnitude; `1.0` for a unit quaternion.
    pub fn length_squared(self) -> f32 {
        self.dot(self)
    }

    /// Magnitude; `1.0` for a unit quaternion.
    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }

    /// Returns the unit quaternion with the same rotation. Only an exactly
    /// zero-length quaternion (or one whose length is not finite) normalizes to
    /// the identity, so the result is never `NaN` and no accumulated product
    /// can poison a whole column. Any non-zero length, however small, keeps its
    /// direction.
    pub fn normalized(self) -> Self {
        let length = self.length();
        if !is_divisible(length) {
            return Self::IDENTITY;
        }
        let k = length.recip();
        Self(self.0 * k, self.1 * k, self.2 * k, self.3 * k)
    }

    /// Negates the vector part. For a unit quaternion this is the inverse.
    pub fn conjugate(self) -> Self {
        Self(-self.0, -self.1, -self.2, self.3)
    }

    /// The inverse rotation, valid for a non-unit quaternion too. Only an
    /// exactly zero-length quaternion (or one whose squared length is not
    /// finite) inverts to the identity.
    pub fn inverse(self) -> Self {
        let squared = self.length_squared();
        if !is_divisible(squared) {
            return Self::IDENTITY;
        }
        let k = squared.recip();
        Self(-self.0 * k, -self.1 * k, -self.2 * k, self.3 * k)
    }

    /// Negates every component. The result is the same rotation, taken the
    /// other way round the sphere.
    pub fn negated(self) -> Self {
        Self(-self.0, -self.1, -self.2, -self.3)
    }

    /// Spherical linear interpolation along the **short arc**: when the two
    /// quaternions are more than a quarter turn apart in four-space, `rhs` is
    /// negated first, so the result never takes the long way round. `t` is not
    /// clamped; the inputs are treated as unit quaternions and the output is
    /// normalized.
    pub fn slerp(self, rhs: Self, t: f32) -> Self {
        let mut end = rhs;
        let mut dot = self.dot(rhs);
        if dot < 0.0 {
            end = end.negated();
            dot = -dot;
        }
        if dot > SLERP_LINEAR_DOT {
            // Nearly parallel: the sines below lose all precision, and the
            // chord is indistinguishable from the arc at this separation.
            return Self(
                self.0 + (end.0 - self.0) * t,
                self.1 + (end.1 - self.1) * t,
                self.2 + (end.2 - self.2) * t,
                self.3 + (end.3 - self.3) * t,
            )
            .normalized();
        }
        let theta = dot.clamp(-1.0, 1.0).acos();
        let sin_theta = theta.sin();
        let a = ((1.0 - t) * theta).sin() / sin_theta;
        let b = (t * theta).sin() / sin_theta;
        Self(
            self.0 * a + end.0 * b,
            self.1 * a + end.1 * b,
            self.2 * a + end.2 * b,
            self.3 * a + end.3 * b,
        )
        .normalized()
    }

    /// The matrix form of the rotation. Assumes a unit quaternion.
    pub fn to_mat3(self) -> Mat3 {
        let (x, y, z, w) = (self.0, self.1, self.2, self.3);
        let (x2, y2, z2) = (x + x, y + y, z + z);
        let (xx, xy, xz) = (x * x2, x * y2, x * z2);
        let (yy, yz, zz) = (y * y2, y * z2, z * z2);
        let (wx, wy, wz) = (w * x2, w * y2, w * z2);
        Mat3::from_rows([
            [1.0 - (yy + zz), xy - wz, xz + wy],
            [xy + wz, 1.0 - (xx + zz), yz - wx],
            [xz - wy, yz + wx, 1.0 - (xx + yy)],
        ])
    }

    /// Rotates a vector. Assumes a unit quaternion.
    pub fn rotate(self, v: Vec3) -> Vec3 {
        self.to_mat3().mul_vec3(v)
    }
}

impl std::ops::Mul for Quat {
    type Output = Self;

    /// Rotation composition; see [`Quat::mul_quat`].
    fn mul(self, rhs: Self) -> Self {
        self.mul_quat(rhs)
    }
}

/// A 3x3 rotation matrix, **row-major** (`[m00, m01, m02, m10, …]`), acting on
/// **column vectors** from the left: [`Mat3::mul_vec3`] computes `m * v`.
///
/// Rotation stops at 3x3 here by design. A 4x4 transform is the scene layer's
/// concern, and defining one in two places would let the two disagree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat3([f32; 9]);

impl Default for Mat3 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Mat3 {
    /// The identity rotation.
    pub const IDENTITY: Self = Self([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);

    /// Builds a matrix from its rows.
    pub fn from_rows(rows: [[f32; 3]; 3]) -> Self {
        Self([
            rows[0][0], rows[0][1], rows[0][2], rows[1][0], rows[1][1], rows[1][2], rows[2][0],
            rows[2][1], rows[2][2],
        ])
    }

    /// The rows, in the same shape [`Mat3::from_rows`] takes.
    pub fn rows(&self) -> [[f32; 3]; 3] {
        let m = &self.0;
        [[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], m[8]]]
    }

    /// Row-major elements, for handing the matrix to a buffer.
    pub fn as_array(&self) -> &[f32; 9] {
        &self.0
    }

    /// One element. `row` and `col` are 0-based and must be below 3.
    pub fn get(&self, row: usize, col: usize) -> f32 {
        self.0[row * 3 + col]
    }

    /// Builds the rotation for ZYX Euler angles in radians: `Rx * Ry * Rz`,
    /// so Z is applied to a vector first and X last.
    pub fn from_euler_zyx(euler: Vec3) -> Self {
        let (sx, cx) = euler.0.sin_cos();
        let (sy, cy) = euler.1.sin_cos();
        let (sz, cz) = euler.2.sin_cos();
        Self::from_rows([
            [cy * cz, -cy * sz, sy],
            [cx * sz + sx * sy * cz, cx * cz - sx * sy * sz, -sx * cy],
            [sx * sz - cx * sy * cz, sx * cz + cx * sy * sz, cx * cy],
        ])
    }

    /// Recovers ZYX Euler angles in radians. Y lands in `[-pi/2, pi/2]`, which
    /// is the range that makes the decomposition unique away from the
    /// degeneracy.
    ///
    /// **Near the ±90° Y degeneracy (gimbal lock)** only the sum (at `+90°`)
    /// or difference (at `-90°`) of the X and Z angles is recoverable from the
    /// matrix, so a choice is required. The choice is fixed: **X is set to
    /// zero and the whole coupled rotation is reported on Z.** Feeding the
    /// result back through [`Mat3::from_euler_zyx`] reproduces the same
    /// rotation — it is the Euler triple that changes, not the pose.
    ///
    /// Accuracy limit just outside that window: X and Z come from ratios of
    /// entries whose size is `cos(y)`, so a matrix carrying about `1e-7` of
    /// rounding — which every quaternion-derived one does — loses roughly
    /// `1e-7 / cos(y)` radians on them, and the pose rebuilt from the triple
    /// loses the same. A matrix built by [`Mat3::from_euler_zyx`] is exempt,
    /// because there `cos(y)` is a common factor that cancels in the ratios.
    pub fn to_euler_zyx(&self) -> Vec3 {
        let sy = self.get(0, 2);
        // `cos(y)` comes from the length of the row it survives in, which also
        // makes it non-negative and so keeps `y` inside [-pi/2, pi/2]. Reading
        // `y` from `atan2` rather than `asin(sy)` matters: `asin` of a value
        // within an f32 epsilon of 1 loses about three digits, which is
        // exactly the region a pitched-up pose lives in.
        let cy = (self.get(0, 0) * self.get(0, 0) + self.get(0, 1) * self.get(0, 1)).sqrt();
        let y = sy.atan2(cy);
        if cy < GIMBAL_EPSILON {
            // Degenerate: row 1 reduces to (sin(z ± x), cos(z ± x), 0).
            return Vec3(0.0, y, self.get(1, 0).atan2(self.get(1, 1)));
        }
        Vec3(
            (-self.get(1, 2)).atan2(self.get(2, 2)),
            y,
            (-self.get(0, 1)).atan2(self.get(0, 0)),
        )
    }

    /// The quaternion form of the rotation. Assumes an orthonormal matrix.
    pub fn to_quat(&self) -> Quat {
        // Pick the branch whose divisor is largest, so the square root never
        // runs into a near-zero denominator.
        let (m00, m11, m22) = (self.get(0, 0), self.get(1, 1), self.get(2, 2));
        let trace = m00 + m11 + m22;
        let quat = if trace > 0.0 {
            let s = (trace + 1.0).sqrt();
            let k = 0.5 / s;
            Quat(
                (self.get(2, 1) - self.get(1, 2)) * k,
                (self.get(0, 2) - self.get(2, 0)) * k,
                (self.get(1, 0) - self.get(0, 1)) * k,
                0.5 * s,
            )
        } else if m00 >= m11 && m00 >= m22 {
            let s = (1.0 + m00 - m11 - m22).sqrt();
            let k = 0.5 / s;
            Quat(
                0.5 * s,
                (self.get(0, 1) + self.get(1, 0)) * k,
                (self.get(0, 2) + self.get(2, 0)) * k,
                (self.get(2, 1) - self.get(1, 2)) * k,
            )
        } else if m11 >= m22 {
            let s = (1.0 - m00 + m11 - m22).sqrt();
            let k = 0.5 / s;
            Quat(
                (self.get(0, 1) + self.get(1, 0)) * k,
                0.5 * s,
                (self.get(1, 2) + self.get(2, 1)) * k,
                (self.get(0, 2) - self.get(2, 0)) * k,
            )
        } else {
            let s = (1.0 - m00 - m11 + m22).sqrt();
            let k = 0.5 / s;
            Quat(
                (self.get(0, 2) + self.get(2, 0)) * k,
                (self.get(1, 2) + self.get(2, 1)) * k,
                0.5 * s,
                (self.get(1, 0) - self.get(0, 1)) * k,
            )
        };
        quat.normalized()
    }

    /// Rotates a column vector: `self * v`.
    pub fn mul_vec3(&self, v: Vec3) -> Vec3 {
        let m = &self.0;
        Vec3(
            m[0] * v.0 + m[1] * v.1 + m[2] * v.2,
            m[3] * v.0 + m[4] * v.1 + m[5] * v.2,
            m[6] * v.0 + m[7] * v.1 + m[8] * v.2,
        )
    }

    /// Composes two rotations: `self * rhs` applies `rhs` to a vector first.
    pub fn mul_mat3(&self, rhs: &Self) -> Self {
        let mut out = [0.0f32; 9];
        for row in 0..3 {
            for col in 0..3 {
                out[row * 3 + col] = (0..3).map(|k| self.get(row, k) * rhs.get(k, col)).sum();
            }
        }
        Self(out)
    }

    /// The transpose, which for a rotation matrix is the inverse rotation.
    pub fn transposed(&self) -> Self {
        let m = &self.0;
        Self([m[0], m[3], m[6], m[1], m[4], m[7], m[2], m[5], m[8]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};

    const TOLERANCE: f32 = 1e-5;

    fn assert_vec3_close(actual: Vec3, expected: Vec3, what: &str) {
        assert!(
            (actual.0 - expected.0).abs() < TOLERANCE
                && (actual.1 - expected.1).abs() < TOLERANCE
                && (actual.2 - expected.2).abs() < TOLERANCE,
            "{what}: {actual:?} != {expected:?}"
        );
    }

    fn assert_mat3_close(actual: Mat3, expected: Mat3, what: &str) {
        for i in 0..9 {
            assert!(
                (actual.as_array()[i] - expected.as_array()[i]).abs() < TOLERANCE,
                "{what}: {actual:?} != {expected:?}"
            );
        }
    }

    /// Same rotation up to sign: the two antipodal quaternions are the same
    /// pose, so comparisons align the signs first.
    fn assert_same_rotation(actual: Quat, expected: Quat, what: &str) {
        let aligned = if actual.dot(expected) < 0.0 {
            actual.negated()
        } else {
            actual
        };
        assert!(
            (aligned.0 - expected.0).abs() < TOLERANCE
                && (aligned.1 - expected.1).abs() < TOLERANCE
                && (aligned.2 - expected.2).abs() < TOLERANCE
                && (aligned.3 - expected.3).abs() < TOLERANCE,
            "{what}: {actual:?} != {expected:?}"
        );
    }

    fn sample_eulers() -> Vec<Vec3> {
        vec![
            Vec3(0.0, 0.0, 0.0),
            Vec3(0.3, -0.7, 1.1),
            Vec3(-1.2, 0.4, -0.9),
            Vec3(FRAC_PI_4, FRAC_PI_4, FRAC_PI_4),
            Vec3(0.0, 0.0, PI * 0.75),
            Vec3(-0.5, 1.4, 0.2),
        ]
    }

    #[test]
    fn euler_and_quaternion_round_trip() {
        for euler in sample_eulers() {
            let quat = Quat::from_euler_zyx(euler);
            assert!(
                (quat.length() - 1.0).abs() < TOLERANCE,
                "euler conversion has to produce a unit quaternion: {quat:?}"
            );
            assert_vec3_close(quat.to_euler_zyx(), euler, "euler -> quat -> euler");
        }
    }

    #[test]
    fn quaternion_round_trips_through_euler() {
        for euler in sample_eulers() {
            let quat = Quat::from_euler_zyx(euler);
            let round_tripped = Quat::from_euler_zyx(quat.to_euler_zyx());
            assert_same_rotation(round_tripped, quat, "quat -> euler -> quat");
        }
    }

    /// The spec fixes ZYX (Z applied first), which as a matrix product is
    /// `Rx * Ry * Rz`. The reversed product is a different rotation, so this
    /// pins the order and not merely the axes.
    #[test]
    fn euler_order_is_zyx() {
        let euler = Vec3(0.3, -0.7, 1.1);
        let rx = Mat3::from_euler_zyx(Vec3(euler.0, 0.0, 0.0));
        let ry = Mat3::from_euler_zyx(Vec3(0.0, euler.1, 0.0));
        let rz = Mat3::from_euler_zyx(Vec3(0.0, 0.0, euler.2));

        assert_mat3_close(
            Mat3::from_euler_zyx(euler),
            rx.mul_mat3(&ry).mul_mat3(&rz),
            "ZYX euler is Rx * Ry * Rz",
        );
        let reversed = rz.mul_mat3(&ry).mul_mat3(&rx);
        assert!(
            Mat3::from_euler_zyx(euler) != reversed,
            "the reversed order has to be a different rotation, or the test pins nothing"
        );
    }

    /// Known angles, known matrix. Also the handedness check: 90° about Y
    /// takes +x onto -z, matching `geometry.transform`.
    #[test]
    fn known_angles_produce_known_matrices() {
        assert_mat3_close(
            Mat3::from_euler_zyx(Vec3(0.0, FRAC_PI_2, 0.0)),
            Mat3::from_rows([[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]]),
            "90 degrees about y",
        );
        assert_vec3_close(
            Mat3::from_euler_zyx(Vec3(0.0, FRAC_PI_2, 0.0)).mul_vec3(Vec3(1.0, 0.0, 0.0)),
            Vec3(0.0, 0.0, -1.0),
            "+x rotates onto -z",
        );
        assert_mat3_close(
            Mat3::from_euler_zyx(Vec3(FRAC_PI_2, 0.0, 0.0)),
            Mat3::from_rows([[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]]),
            "90 degrees about x",
        );
        assert_mat3_close(
            Mat3::from_euler_zyx(Vec3(0.0, 0.0, FRAC_PI_2)),
            Mat3::from_rows([[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]),
            "90 degrees about z",
        );
        // Z first, then X: (1,0,0) -> (0,1,0) -> (0,0,1).
        assert_mat3_close(
            Mat3::from_euler_zyx(Vec3(FRAC_PI_2, 0.0, FRAC_PI_2)),
            Mat3::from_rows([[0.0, -1.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]]),
            "90 about z then 90 about x",
        );
    }

    #[test]
    fn quaternion_and_matrix_agree() {
        for euler in sample_eulers() {
            assert_mat3_close(
                Quat::from_euler_zyx(euler).to_mat3(),
                Mat3::from_euler_zyx(euler),
                "quaternion matrix equals the euler matrix",
            );
            assert_same_rotation(
                Mat3::from_euler_zyx(euler).to_quat(),
                Quat::from_euler_zyx(euler),
                "matrix -> quat",
            );
        }
    }

    /// Every branch of `to_quat` has to be reachable and correct, including the
    /// three negative-trace ones.
    ///
    /// The axes are deliberately **not** the basis vectors: about a basis axis
    /// the off-diagonal terms the branch reads are all zero, so a sign error in
    /// them would still round-trip. A tilted axis puts every term to work. The
    /// angles bracket the half turn, which is where the trace goes negative.
    #[test]
    fn matrix_to_quaternion_covers_the_negative_trace_branches() {
        let mut negative_traces = 0;
        for axis in [
            Vec3(1.0, 1.0, 0.0),
            Vec3(0.0, 1.0, 1.0),
            Vec3(1.0, 0.0, 1.0),
            Vec3(2.0, -1.0, 0.5),
            Vec3(-0.3, 0.9, -1.7),
            Vec3(1.0, 0.0, 0.0),
            Vec3(0.0, 1.0, 0.0),
            Vec3(0.0, 0.0, 1.0),
        ] {
            for angle in [PI * 0.9, PI, PI * 1.1, 0.7] {
                let quat = Quat::from_axis_angle(axis, angle);
                let matrix = quat.to_mat3();
                if matrix.get(0, 0) + matrix.get(1, 1) + matrix.get(2, 2) <= 0.0 {
                    negative_traces += 1;
                }
                assert_same_rotation(
                    matrix.to_quat(),
                    quat,
                    &format!("round trip about {axis:?} by {angle}"),
                );
                // The rotation itself, not just the quaternion, has to survive.
                assert_mat3_close(
                    matrix.to_quat().to_mat3(),
                    matrix,
                    "matrix -> quat -> matrix",
                );
            }
        }
        assert!(
            negative_traces >= 3,
            "the fixtures have to reach the negative-trace branches, hit {negative_traces}"
        );
    }

    /// The largest-diagonal choice inside `to_quat` has to pick each of the
    /// three vector branches for some input, or one of them is dead code that
    /// no round-trip test can reach.
    #[test]
    fn matrix_to_quaternion_reaches_each_vector_branch() {
        let mut largest = [false; 3];
        for axis in [
            Vec3(1.0, 0.2, 0.1),
            Vec3(0.2, 1.0, 0.1),
            Vec3(0.1, 0.2, 1.0),
        ] {
            let quat = Quat::from_axis_angle(axis, PI);
            let matrix = quat.to_mat3();
            let diagonal = [matrix.get(0, 0), matrix.get(1, 1), matrix.get(2, 2)];
            assert!(
                diagonal[0] + diagonal[1] + diagonal[2] <= 0.0,
                "a half turn has a non-positive trace: {diagonal:?}"
            );
            let index = (0..3)
                .max_by(|a, b| diagonal[*a].total_cmp(&diagonal[*b]))
                .expect("three elements");
            largest[index] = true;
            assert_same_rotation(matrix.to_quat(), quat, "half turn about a tilted axis");
        }
        assert_eq!(largest, [true; 3], "each vector branch has to be exercised");
    }

    /// The component order is `(x, y, z, w)`, and the `orient` column shares it
    /// element for element. Each axis is checked on its own: a rotation about
    /// one axis fills that component and leaves the other two at zero, so a
    /// swapped pair cannot slip through.
    #[test]
    fn quaternion_component_order_is_xyzw() {
        let (sin_half, cos_half) = (FRAC_PI_4.sin(), FRAC_PI_4.cos());
        let cases = [
            (Vec3(FRAC_PI_2, 0.0, 0.0), [sin_half, 0.0, 0.0], "x"),
            (Vec3(0.0, FRAC_PI_2, 0.0), [0.0, sin_half, 0.0], "y"),
            (Vec3(0.0, 0.0, FRAC_PI_2), [0.0, 0.0, sin_half], "z"),
        ];
        for (euler, expected, axis) in cases {
            let quat = Quat::from_euler_zyx(euler);
            assert!(
                (quat.0 - expected[0]).abs() < TOLERANCE
                    && (quat.1 - expected[1]).abs() < TOLERANCE
                    && (quat.2 - expected[2]).abs() < TOLERANCE
                    && (quat.3 - cos_half).abs() < TOLERANCE,
                "a {axis} rotation only fills the {axis} and w components: {quat:?}"
            );
        }

        // The `Vec4` mapping is positional and has to stay that way, so pin it
        // on four values that are all different from one another.
        let asymmetric = Quat(0.1, 0.2, 0.3, 0.4);
        let vec4 = asymmetric.to_vec4();
        assert_eq!((vec4.0, vec4.1, vec4.2, vec4.3), (0.1, 0.2, 0.3, 0.4));
        assert_eq!(Quat::from_vec4(Vec4(0.1, 0.2, 0.3, 0.4)), asymmetric);
        assert_eq!(
            Quat::from_vec4(Quat::from_euler_zyx(Vec3(0.3, -0.7, 1.1)).to_vec4()),
            Quat::from_euler_zyx(Vec3(0.3, -0.7, 1.1)),
            "Vec4 round trip preserves order"
        );
    }

    #[test]
    fn composition_applies_the_right_hand_side_first() {
        let first = Quat::from_euler_zyx(Vec3(0.0, 0.0, FRAC_PI_2));
        let then = Quat::from_euler_zyx(Vec3(FRAC_PI_2, 0.0, 0.0));
        let composed = then * first;
        assert_vec3_close(
            composed.rotate(Vec3(1.0, 0.0, 0.0)),
            Vec3(0.0, 0.0, 1.0),
            "z rotation then x rotation",
        );
        assert_mat3_close(
            composed.to_mat3(),
            then.to_mat3().mul_mat3(&first.to_mat3()),
            "quaternion product matches the matrix product",
        );
    }

    #[test]
    fn inverse_and_conjugate_undo_the_rotation() {
        let quat = Quat::from_euler_zyx(Vec3(0.3, -0.7, 1.1));
        assert_same_rotation(quat * quat.inverse(), Quat::IDENTITY, "q * q^-1");
        assert_same_rotation(quat * quat.conjugate(), Quat::IDENTITY, "unit q * conj q");

        // The conjugate is only an inverse for a unit quaternion; the inverse
        // has to hold for a scaled one too.
        let scaled = Quat(quat.0 * 3.0, quat.1 * 3.0, quat.2 * 3.0, quat.3 * 3.0);
        let product = scaled * scaled.inverse();
        assert_same_rotation(product, Quat::IDENTITY, "scaled q * q^-1");

        assert_eq!(Quat(0.0, 0.0, 0.0, 0.0).inverse(), Quat::IDENTITY);
        assert_eq!(Quat(0.0, 0.0, 0.0, 0.0).normalized(), Quat::IDENTITY);
        assert_eq!(
            Quat::from_axis_angle(Vec3(0.0, 0.0, 0.0), 1.0),
            Quat::IDENTITY
        );
    }

    /// Only an exact zero is degenerate. A guard written against `f32::EPSILON`
    /// discards magnitudes down to 1.2e-7 — and, compared against a *squared*
    /// length as `inverse` does, everything below 3.5e-4 — which are ordinary
    /// rotations that divide perfectly well.
    #[test]
    fn tiny_but_non_zero_magnitudes_are_not_treated_as_degenerate() {
        let tiny = Quat(0.0, 0.0, 1e-4, 0.0);
        let inverted = tiny.inverse();
        assert_ne!(inverted, Quat::IDENTITY, "1e-4 is not a degenerate length");
        // q^-1 = -v / |q|^2 for a pure quaternion: -1e-4 / 1e-8 = -1e4.
        assert!(
            (inverted.2 + 1e4).abs() < 1.0 && inverted.3 == 0.0,
            "{inverted:?}"
        );
        assert_same_rotation(
            (tiny * tiny.inverse()).normalized(),
            Quat::IDENTITY,
            "a tiny quaternion still has a real inverse",
        );

        let very_tiny = Quat(0.0, 0.0, 1e-8, 0.0);
        assert_eq!(
            very_tiny.normalized(),
            Quat(0.0, 0.0, 1.0, 0.0),
            "1e-8 still carries a direction to normalize onto"
        );

        let quat = Quat::from_axis_angle(Vec3(1e-8, 0.0, 0.0), FRAC_PI_2);
        assert_same_rotation(
            quat,
            Quat::from_axis_angle(Vec3(1.0, 0.0, 0.0), FRAC_PI_2),
            "a short axis vector still points along x",
        );
    }

    /// The documented limits: a non-finite or overflowing magnitude falls back
    /// to the identity instead of propagating `NaN` into a whole column.
    #[test]
    fn non_finite_and_overflowing_magnitudes_fall_back_to_the_identity() {
        assert_eq!(Quat(f32::NAN, 0.0, 0.0, 1.0).normalized(), Quat::IDENTITY);
        assert_eq!(
            Quat(f32::INFINITY, 0.0, 0.0, 1.0).normalized(),
            Quat::IDENTITY
        );
        assert_eq!(Quat(f32::NAN, 0.0, 0.0, 1.0).inverse(), Quat::IDENTITY);
        // 1e20 squared overflows f32, which is the stated limitation.
        assert_eq!(Quat(1e20, 0.0, 0.0, 0.0).normalized(), Quat::IDENTITY);
        assert_eq!(
            Quat::from_axis_angle(Vec3(1e20, 0.0, 0.0), FRAC_PI_2),
            Quat::IDENTITY
        );
        assert_eq!(
            Quat::from_axis_angle(Vec3(f32::NAN, 0.0, 0.0), FRAC_PI_2),
            Quat::IDENTITY
        );
    }

    #[test]
    fn normalized_rescales_to_unit_length() {
        let quat = Quat(0.0, 0.0, 2.0, 2.0);
        let unit = quat.normalized();
        assert!((unit.length() - 1.0).abs() < TOLERANCE, "{unit:?}");
        assert_vec3_close(
            unit.to_euler_zyx(),
            Vec3(0.0, 0.0, FRAC_PI_2),
            "normalizing keeps the rotation",
        );
    }

    /// 350° about Z is 10° the other way. Slerp has to interpolate through the
    /// short arc, so halfway is -5°, not +175°.
    #[test]
    fn slerp_takes_the_short_arc() {
        let z = Vec3(0.0, 0.0, 1.0);
        let start = Quat::IDENTITY;
        let end = Quat::from_axis_angle(z, 350.0f32.to_radians());
        let middle = start.slerp(end, 0.5).to_euler_zyx();
        assert_vec3_close(
            middle,
            Vec3(0.0, 0.0, -5.0f32.to_radians()),
            "halfway along the short arc",
        );

        // Negating an endpoint is the same rotation, so it must not change
        // the path.
        assert_same_rotation(
            start.slerp(end.negated(), 0.5),
            start.slerp(end, 0.5),
            "an antipodal endpoint takes the same arc",
        );

        // A genuinely long rotation still interpolates monotonically.
        let quarter = Quat::from_axis_angle(z, FRAC_PI_2);
        for (t, expected) in [
            (0.25, FRAC_PI_2 * 0.25),
            (0.5, FRAC_PI_4),
            (0.75, FRAC_PI_2 * 0.75),
        ] {
            assert_vec3_close(
                start.slerp(quarter, t).to_euler_zyx(),
                Vec3(0.0, 0.0, expected),
                "constant angular velocity along the arc",
            );
        }
    }

    #[test]
    fn slerp_reaches_its_endpoints_and_stays_normalized() {
        let start = Quat::from_euler_zyx(Vec3(0.2, 0.4, -0.6));
        let end = Quat::from_euler_zyx(Vec3(-1.0, 0.9, 2.4));
        assert_same_rotation(start.slerp(end, 0.0), start, "t = 0");
        assert_same_rotation(start.slerp(end, 1.0), end, "t = 1");
        for step in 0..=10 {
            let quat = start.slerp(end, step as f32 / 10.0);
            assert!((quat.length() - 1.0).abs() < TOLERANCE, "{quat:?}");
        }
    }

    /// Nearly identical endpoints take the linear fallback; it still has to
    /// land on the endpoints and stay normalized.
    #[test]
    fn slerp_falls_back_to_the_linear_path_when_nearly_parallel() {
        let start = Quat::from_euler_zyx(Vec3(0.2, 0.4, -0.6));
        let end = Quat::from_euler_zyx(Vec3(0.2, 0.4, -0.599_99));
        assert!(
            start.dot(end) > SLERP_LINEAR_DOT,
            "the fixture has to be inside the linear window"
        );
        let middle = start.slerp(end, 0.5);
        assert!((middle.length() - 1.0).abs() < TOLERANCE, "{middle:?}");
        assert_same_rotation(start.slerp(end, 1.0), end, "t = 1 in the linear window");
    }

    /// The two branches have to agree **across the threshold**, otherwise the
    /// crossover is a visible kink in an interpolated rotation.
    ///
    /// The pair above the threshold sits just above it — a dot of about 0.99955
    /// against a cut of 0.9995 — rather than at a dot that rounds to 1.0, where
    /// any cut value whatsoever would select the linear branch and the test
    /// would pin nothing. The pair below it is 0.07 rad apart and takes the
    /// trigonometric branch. Both are checked against the analytic answer, so
    /// moving `SLERP_LINEAR_DOT` past either fixture makes one of them fail.
    #[test]
    fn slerp_agrees_with_the_analytic_arc_on_both_sides_of_the_threshold() {
        let z = Vec3(0.0, 0.0, 1.0);
        for (separation, expect_linear) in [(0.06f32, true), (0.07f32, false)] {
            let start = Quat::IDENTITY;
            let end = Quat::from_axis_angle(z, separation);
            let dot = start.dot(end);
            assert_eq!(
                dot > SLERP_LINEAR_DOT,
                expect_linear,
                "a separation of {separation} rad has to land on the intended \
                 branch, dot {dot}"
            );
            assert!(dot < 1.0, "a dot that rounds to 1.0 pins no threshold");
            for t in [0.25f32, 0.5, 0.75] {
                assert_vec3_close(
                    start.slerp(end, t).to_euler_zyx(),
                    Vec3(0.0, 0.0, separation * t),
                    "both branches follow the analytic arc",
                );
            }
        }
    }

    /// The declared degeneracy: at Y = +90° only `x + z` is recoverable, so X
    /// is reported as zero and the sum lands on Z.
    #[test]
    fn gimbal_lock_puts_the_coupled_rotation_on_z() {
        let euler = Vec3(30.0f32.to_radians(), FRAC_PI_2, 20.0f32.to_radians());
        let recovered = Mat3::from_euler_zyx(euler).to_euler_zyx();
        assert_vec3_close(
            recovered,
            Vec3(0.0, FRAC_PI_2, 50.0f32.to_radians()),
            "x collapses onto z at y = +90 degrees",
        );
        assert_mat3_close(
            Mat3::from_euler_zyx(recovered),
            Mat3::from_euler_zyx(euler),
            "the pose survives even though the euler triple changed",
        );
    }

    /// At Y = -90° the recoverable quantity is `z - x` instead, and the same
    /// choice (X = 0) applies.
    #[test]
    fn gimbal_lock_at_negative_ninety_reports_the_difference() {
        let euler = Vec3(30.0f32.to_radians(), -FRAC_PI_2, 20.0f32.to_radians());
        let recovered = Mat3::from_euler_zyx(euler).to_euler_zyx();
        assert_vec3_close(
            recovered,
            Vec3(0.0, -FRAC_PI_2, -10.0f32.to_radians()),
            "x subtracts from z at y = -90 degrees",
        );
        assert_mat3_close(
            Mat3::from_euler_zyx(recovered),
            Mat3::from_euler_zyx(euler),
            "the pose survives even though the euler triple changed",
        );
    }

    /// Just outside the degenerate window the ordinary branch still runs and
    /// round-trips, so the epsilon does not swallow usable angles.
    #[test]
    fn near_gimbal_lock_still_round_trips() {
        let euler = Vec3(0.4, FRAC_PI_2 - 0.01, -0.8);
        let recovered = Mat3::from_euler_zyx(euler).to_euler_zyx();
        assert_vec3_close(recovered, euler, "0.57 degrees away from the pole");
        assert_vec3_close(
            Quat::from_euler_zyx(euler).to_euler_zyx(),
            euler,
            "the quaternion path agrees near the pole",
        );
    }

    /// `cos(y)` of a matrix, read back the way `to_euler_zyx` reads it.
    fn cos_y_of(matrix: &Mat3) -> f32 {
        (matrix.get(0, 0) * matrix.get(0, 0) + matrix.get(0, 1) * matrix.get(0, 1)).sqrt()
    }

    /// The band immediately above `GIMBAL_EPSILON` is where the ordinary branch
    /// would be expected to fall apart, and for a matrix built from Euler
    /// angles it does not: X and Z are read from `atan2` ratios in which
    /// `cos(y)` appears in **both** arguments and therefore cancels, so
    /// shrinking it does not degrade them. Two ulps' worth of margin above the
    /// threshold is all this needs to hold.
    #[test]
    fn the_ordinary_branch_survives_just_above_the_gimbal_threshold() {
        for offset in [2e-6f32, 5e-6, 1e-5, 1e-4, 1e-2] {
            let euler = Vec3(0.4, FRAC_PI_2 - offset, -0.8);
            let matrix = Mat3::from_euler_zyx(euler);
            let cos_y = cos_y_of(&matrix);
            assert!(
                cos_y >= GIMBAL_EPSILON,
                "the fixture has to stay on the ordinary branch, cos(y) {cos_y}"
            );
            assert_vec3_close(
                matrix.to_euler_zyx(),
                euler,
                "round trip a hair off the pole",
            );
        }
    }

    /// Going through a quaternion is the accuracy floor near the pole, not the
    /// threshold. `to_mat3` builds the first row out of sums and differences of
    /// near-equal products, so its entries carry an absolute error of about
    /// `1e-7` whatever `cos(y)` is; the X / Z split is then read from a ratio of
    /// entries of size `cos(y)`, and the error scales as `1e-7 / cos(y)`.
    ///
    /// The quaternion's own pose is exact; what degrades is the Euler triple
    /// read out of it, and with it the pose rebuilt from that triple. This is a
    /// property of Euler extraction from a rounded near-degenerate matrix, not
    /// of the threshold: raising `GIMBAL_EPSILON` would relabel the band rather
    /// than recover the information, which is why the constant is left alone.
    /// Beyond a milliradian from the pole the split is usable again.
    #[test]
    fn the_quaternion_path_loses_the_x_z_split_before_the_matrix_path_does() {
        for offset in [2e-6f32, 1e-5, 1e-4, 1e-3, 1e-2] {
            let euler = Vec3(0.4, FRAC_PI_2 - offset, -0.8);
            let matrix = Quat::from_euler_zyx(euler).to_mat3();
            let cos_y = cos_y_of(&matrix);
            assert!(cos_y >= GIMBAL_EPSILON, "cos(y) {cos_y}");

            let recovered = matrix.to_euler_zyx();
            let error = (recovered.0 - euler.0)
                .abs()
                .max((recovered.2 - euler.2).abs());
            assert!(
                error * cos_y < 2e-7,
                "the split error has to stay within the 1e-7 / cos(y) law: \
                 error {error} at cos(y) {cos_y}"
            );
            // The pose rebuilt from the triple obeys the same law, so it is
            // held to a bound that scales with it rather than a fixed one.
            let rebuilt = Mat3::from_euler_zyx(recovered);
            let bound = (2e-7 / cos_y).max(TOLERANCE);
            for i in 0..9 {
                assert!(
                    (rebuilt.as_array()[i] - matrix.as_array()[i]).abs() < bound,
                    "the rebuilt pose has to stay inside the same law: \
                     {rebuilt:?} vs {matrix:?} at cos(y) {cos_y}"
                );
            }
            if cos_y >= 1e-3 {
                assert!(
                    error < 1e-4,
                    "beyond a milliradian from the pole the split is usable: {error}"
                );
            }
        }
    }

    /// Where the boundary actually falls is worth pinning, because it is not
    /// where the arithmetic suggests: `f32` rounds `pi/2 - 1e-6` to a value
    /// whose cosine is 9.1e-7, not 1e-6, so an offset of exactly one epsilon is
    /// already inside the degenerate window. The rule that holds either way is
    /// that the **pose** survives; only the Euler triple is redistributed.
    #[test]
    fn an_offset_of_one_epsilon_is_already_degenerate() {
        let euler = Vec3(0.4, FRAC_PI_2 - 1e-6, -0.8);
        let recovered = Mat3::from_euler_zyx(euler).to_euler_zyx();
        assert_vec3_close(
            recovered,
            Vec3(0.0, recovered.1, euler.0 + euler.2),
            "the degenerate rule applies: x = 0 and the sum lands on z",
        );
        assert_mat3_close(
            Mat3::from_euler_zyx(recovered),
            Mat3::from_euler_zyx(euler),
            "the pose survives the redistribution",
        );
    }

    /// The quaternion path shares the matrix degeneracy rule, since it routes
    /// through the matrix.
    #[test]
    fn quaternion_euler_extraction_shares_the_gimbal_rule() {
        let euler = Vec3(30.0f32.to_radians(), FRAC_PI_2, 20.0f32.to_radians());
        assert_vec3_close(
            Quat::from_euler_zyx(euler).to_euler_zyx(),
            Vec3(0.0, FRAC_PI_2, 50.0f32.to_radians()),
            "same collapse through the quaternion",
        );
    }

    #[test]
    fn matrix_helpers_are_consistent() {
        let matrix = Mat3::from_euler_zyx(Vec3(0.3, -0.7, 1.1));
        assert_eq!(matrix.rows()[1][2], matrix.get(1, 2));
        assert_eq!(matrix.as_array()[5], matrix.get(1, 2));
        assert_mat3_close(
            matrix.mul_mat3(&matrix.transposed()),
            Mat3::IDENTITY,
            "a rotation times its transpose is the identity",
        );
        assert_eq!(Mat3::default(), Mat3::IDENTITY);
        assert_eq!(Quat::default(), Quat::IDENTITY);
        assert_vec3_close(
            Mat3::IDENTITY.mul_vec3(Vec3(1.0, 2.0, 3.0)),
            Vec3(1.0, 2.0, 3.0),
            "identity leaves a vector alone",
        );
    }
}
