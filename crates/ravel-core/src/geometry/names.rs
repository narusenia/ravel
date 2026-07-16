// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reserved standard attribute names (see
//! `docs/specifications/procedural-geometry.md`).

/// Position (Vec2, required on Point/Instance domains).
pub const P: &str = "P";
/// Stable creation-order index (I32, Point/Instance).
pub const INDEX: &str = "index";
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
