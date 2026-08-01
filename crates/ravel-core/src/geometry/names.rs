// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reserved standard attribute names (see
//! `docs/specifications/procedural-geometry.md`).

/// Position (Vec2 in 2D or Vec3 in 3D, required on Point/Instance domains).
/// The dimension is chosen per domain — see "位置の次元" in the procedural
/// geometry spec.
pub const P: &str = "P";
/// Geometry anchor (Vec2, Detail).
pub const ANCHOR: &str = "anchor";
/// Stable creation-order index (I32, Point/Instance).
pub const INDEX: &str = "index";
/// Instance source selector (I32, Instance).
pub const SOURCE_INDEX: &str = "source_index";
/// Identifier stable across an element's lifetime (I32, sim use).
pub const ID: &str = "id";
/// Rotation in radians (F32, Instance). 2D only; the 3D counterpart is
/// [`ORIENT`].
pub const ROT: &str = "rot";
/// Scale (Vec2, Instance). 2D only; the 3D counterpart is [`SCALE3`].
pub const SCALE: &str = "scale";
/// Orientation quaternion (Vec4, Instance), 3D only (REQ-3D-003). Component
/// order is `(x, y, z, w)` — see `geometry::rotation`, which owns the
/// conversions. Not a keyframe target: the unified animation channel
/// interpolates components independently, which does not compose a rotation.
pub const ORIENT: &str = "orient";
/// Scale (Vec3, Instance), 3D only. The 2D counterpart [`SCALE`] stays as it
/// is; which one a 3D consumer reads is decided by the consuming nodes.
pub const SCALE3: &str = "scale3";
/// Normal (Vec3, Point/Primitive), 3D only. Lighting reads it.
pub const N: &str = "N";
/// Color (Color, Point/Instance).
pub const CD: &str = "Cd";
/// Opacity (F32, Point/Instance).
pub const ALPHA: &str = "alpha";
/// Point draw radius (F32, Point).
pub const PSCALE: &str = "pscale";
/// Particle age in frames (F32, Point).
pub const AGE: &str = "age";
/// Particle lifetime in frames (F32, Point).
pub const LIFE: &str = "life";
/// Velocity (Vec2, Point, sim).
pub const VELOCITY: &str = "velocity";
/// Incoming bezier tangent offset (Vec2, Point). The control point of the
/// segment arriving at a point is `P + in_tan`; zero = corner (straight
/// segment). Reserved for pen-drawn paths (REQ-UI-011).
pub const IN_TAN: &str = "in_tan";
/// Outgoing bezier tangent offset (Vec2, Point). The control point of the
/// segment leaving a point is `P + out_tan`; zero = corner (straight
/// segment). Reserved for pen-drawn paths (REQ-UI-011).
pub const OUT_TAN: &str = "out_tan";

#[cfg(test)]
mod tests {
    use super::*;

    /// The spelling of a reserved name is persisted inside every project that
    /// carries the attribute, so renaming one is a migration. Pinning the
    /// strings makes that visible in review.
    #[test]
    fn reserved_names_keep_their_spelling() {
        assert_eq!(
            [
                P,
                ANCHOR,
                INDEX,
                SOURCE_INDEX,
                ID,
                ROT,
                SCALE,
                ORIENT,
                SCALE3,
                N,
                CD,
                ALPHA,
                PSCALE,
                AGE,
                LIFE,
                VELOCITY,
                IN_TAN,
                OUT_TAN,
            ],
            [
                "P",
                "anchor",
                "index",
                "source_index",
                "id",
                "rot",
                "scale",
                "orient",
                "scale3",
                "N",
                "Cd",
                "alpha",
                "pscale",
                "age",
                "life",
                "velocity",
                "in_tan",
                "out_tan",
            ]
        );
    }

    /// The 3D additions are separate names, not replacements: the 2D path
    /// keeps reading `rot` and `scale`.
    #[test]
    fn the_2d_and_3d_transform_names_are_distinct() {
        assert_ne!(ROT, ORIENT);
        assert_ne!(SCALE, SCALE3);
    }
}
