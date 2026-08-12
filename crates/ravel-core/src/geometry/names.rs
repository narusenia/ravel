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
/// Fill flag (Bool, Primitive/Instance). Absent means the `rasterize`
/// node's `fill` parameter decides.
pub const FILL: &str = "fill";
/// Stroke width in composition pixels (F32, Primitive/Instance);
/// 0 draws no stroke. Absent means the `rasterize` node's `stroke_width`
/// parameter decides.
pub const STROKE_WIDTH: &str = "stroke_width";
/// Stroke color (Color, Primitive/Instance). Absent falls back to
/// [`CD`], which is the fill color, so an unset stroke color draws the way it
/// did before strokes had one.
pub const STROKE_COLOR: &str = "stroke_color";
/// Dash pattern (Str, Detail): alternating on/off run lengths in composition
/// pixels, `"4,2"` style. Empty (or absent) draws a solid stroke. Detail
/// rather than per element: a dash costs the rasterizer an arc-length walk,
/// and one pattern for the geometry is what the GPU path can still decide on.
pub const DASH: &str = "dash";
/// Where the dash pattern starts, in composition pixels (F32, Detail).
pub const DASH_OFFSET: &str = "dash_offset";
/// Stroke end shape (I32, Detail): [`CAP_BUTT`] / [`CAP_ROUND`] /
/// [`CAP_SQUARE`]. Absent means round, which is what the rasterizer drew
/// before the attribute existed.
pub const CAP: &str = "cap";
/// Stroke corner shape (I32, Detail): [`JOIN_MITER`] / [`JOIN_ROUND`] /
/// [`JOIN_BEVEL`]. Absent means round, as for [`CAP`].
pub const JOIN: &str = "join";

/// Flat cap, ending the stroke at the end point ([`CAP`]).
pub const CAP_BUTT: i32 = 0;
/// Rounded cap of radius half the stroke width ([`CAP`]). The default.
pub const CAP_ROUND: i32 = 1;
/// Square cap extending half the stroke width past the end point ([`CAP`]).
pub const CAP_SQUARE: i32 = 2;
/// Corners extended to their natural intersection ([`JOIN`]).
pub const JOIN_MITER: i32 = 0;
/// Arc between the segments ([`JOIN`]). The default.
pub const JOIN_ROUND: i32 = 1;
/// Straight line between the segments ([`JOIN`]).
pub const JOIN_BEVEL: i32 = 2;

/// Particle age in frames (F32, Point).
pub const AGE: &str = "age";
/// Particle lifetime in frames (F32, Point).
pub const LIFE: &str = "life";
/// Velocity (Vec2, Point, sim).
pub const VELOCITY: &str = "velocity";
/// Path parameter (F32, Point): where a point sits along its own path
/// primitive, `0..1`. Houdini's `curveu`. Written by `attribute.curveu`, and
/// read like any other column — `field.attribute("u")` is what turns it into
/// a gradient along a line.
pub const U: &str = "u";
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
                FILL,
                STROKE_WIDTH,
                STROKE_COLOR,
                DASH,
                DASH_OFFSET,
                CAP,
                JOIN,
                AGE,
                LIFE,
                VELOCITY,
                U,
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
                "fill",
                "stroke_width",
                "stroke_color",
                "dash",
                "dash_offset",
                "cap",
                "join",
                "age",
                "life",
                "velocity",
                "u",
                "in_tan",
                "out_tan",
            ]
        );
    }

    /// The cap and join codes travel inside a project's geometry the same way
    /// the names do: a renumbering would silently restyle every stroke that
    /// stored the old value.
    #[test]
    fn cap_and_join_codes_keep_their_numbering() {
        assert_eq!([CAP_BUTT, CAP_ROUND, CAP_SQUARE], [0, 1, 2]);
        assert_eq!([JOIN_MITER, JOIN_ROUND, JOIN_BEVEL], [0, 1, 2]);
    }

    /// The 3D additions are separate names, not replacements: the 2D path
    /// keeps reading `rot` and `scale`.
    #[test]
    fn the_2d_and_3d_transform_names_are_distinct() {
        assert_ne!(ROT, ORIENT);
        assert_ne!(SCALE, SCALE3);
    }
}
