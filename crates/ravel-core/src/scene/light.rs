// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scene lights (REQ-3D-006).
//!
//! A light is plain data carried inside the [`Scene`](super::Scene), the way a
//! [`Camera`](super::Camera) is: `scene.light` builds a one-light scene and
//! `scene.merge` combines it with the rest. Evaluating it is `scene.render`'s
//! job; this module only says what a light *is*.
//!
//! The shading model is Lambert with no material: a surface point's colour is
//! its own colour (`Cd` / tint) times the sum of what each light delivers.
//!
//! | Kind | Delivers |
//! |---|---|
//! | [`LightKind::Ambient`] | `color * intensity`, everywhere, regardless of the normal |
//! | [`LightKind::Directional`] | `color * intensity * max(N·L, 0)` with `L` opposite the direction of travel |
//! | [`LightKind::Point`] | `color * intensity * max(N·L, 0) / (1 + (d / radius)²)`, `L` toward the light, `d` the distance to it |
//!
//! `N` is the geometry's `N` attribute carried to scene space (by the inverse
//! transpose of the object's matrix, so a non-uniform scale keeps it
//! perpendicular). Geometry without `N` — every Path, and a mesh that never
//! wrote one — is **not lit**: it is drawn in its own colour, because a
//! normal invented for it would shade it arbitrarily.

/// `scene.light` `type` spellings, in the order the Properties dropdown lists
/// them.
pub const LIGHT_AMBIENT: &str = "ambient";
/// See [`LIGHT_AMBIENT`].
pub const LIGHT_DIRECTIONAL: &str = "directional";
/// See [`LIGHT_AMBIENT`].
pub const LIGHT_POINT: &str = "point";
/// Every accepted `type` value.
pub const LIGHT_KINDS: [&str; 3] = [LIGHT_AMBIENT, LIGHT_DIRECTIONAL, LIGHT_POINT];

/// Default direction of travel of a directional light: from the upper left
/// and front of the scene, into it (scene space is +Y down, +Z away).
pub const DEFAULT_DIRECTION: [f32; 3] = [0.3, 0.5, 1.0];
/// Default position of a point light, in front of and above the origin.
pub const DEFAULT_POSITION: [f32; 3] = [0.0, -500.0, -500.0];
/// Default distance at which a point light has fallen to half strength.
pub const DEFAULT_RADIUS: f32 = 1000.0;
/// Smallest `radius` a point light is evaluated with, so the falloff never
/// divides by zero.
pub const MIN_RADIUS: f32 = 1e-3;

/// The strongest a light gets. The render target is half float (max 65504),
/// so a lit colour must stay well inside it; a keyframe or expression beyond
/// this is clamped the same way the parameter is.
pub const MAX_INTENSITY: f32 = 1000.0;
const _: () = assert!(MAX_INTENSITY < 65504.0, "a lit colour must fit half float");

/// What a light does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LightKind {
    /// Uniform light from nowhere.
    Ambient,
    /// Parallel rays travelling along `direction` (scene space, need not be
    /// unit length; a zero vector delivers nothing).
    Directional { direction: [f32; 3] },
    /// A point source at `position` that has fallen to half strength at
    /// `radius`.
    Point { position: [f32; 3], radius: f32 },
}

/// One light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    /// What it does.
    pub kind: LightKind,
    /// Linear RGB, each channel nominally in `[0, 1]`.
    pub color: [f32; 3],
    /// Multiplier on `color`; above 1 over-exposes, clamped to
    /// `[0, MAX_INTENSITY]`.
    pub intensity: f32,
}

impl Light {
    /// What a scene with no light is rendered under: white ambient, which
    /// leaves every surface at its own colour.
    pub const DEFAULT_AMBIENT: Self = Self {
        kind: LightKind::Ambient,
        color: [1.0, 1.0, 1.0],
        intensity: 1.0,
    };

    /// `color * intensity`, the light's strength per channel.
    pub fn radiance(&self) -> [f32; 3] {
        let k = self.intensity.clamp(0.0, MAX_INTENSITY);
        self.color.map(|c| c * k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white(intensity: f32) -> Light {
        Light {
            intensity,
            ..Light::DEFAULT_AMBIENT
        }
    }

    #[test]
    fn radiance_clamps_intensity_to_what_the_render_target_holds() {
        assert_eq!(white(-2.0).radiance(), [0.0; 3]);
        assert_eq!(white(2.5).radiance(), [2.5; 3]);
        assert_eq!(white(1e6).radiance(), [MAX_INTENSITY; 3]);
    }
}
