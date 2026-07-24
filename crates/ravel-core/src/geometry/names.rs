// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reserved standard attribute names (see
//! `docs/specifications/procedural-geometry.md`).

/// Position (Vec2, required on Point/Instance domains).
pub const P: &str = "P";
/// Geometry anchor (Vec2, Detail).
pub const ANCHOR: &str = "anchor";
/// Stable creation-order index (I32, Point/Instance).
pub const INDEX: &str = "index";
/// Instance source selector (I32, Instance).
pub const SOURCE_INDEX: &str = "source_index";
/// Identifier stable across an element's lifetime (I32, sim use).
pub const ID: &str = "id";
/// Rotation in radians (F32, Instance).
pub const ROT: &str = "rot";
/// Scale (Vec2, Instance).
pub const SCALE: &str = "scale";
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
