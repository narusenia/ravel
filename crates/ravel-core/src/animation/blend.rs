// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Blend modes for combining two scalar channel sources.
//!
//! Every blend mode first computes a "full" combination of `a` and `b`, then
//! interpolates from `a` toward that result by `factor ∈ [0, 1]`. This gives a
//! consistent meaning to `factor` across all modes: `0.0` yields `a`, `1.0`
//! yields the full blend, and intermediate values cross-fade between them.

/// How two channel values are combined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BlendMode {
    /// Cross-fade: `lerp(a, b, factor)`.
    #[default]
    Mix,
    /// Additive: full result is `a + b`.
    Add,
    /// Subtractive: full result is `a - b`.
    Subtract,
    /// Multiplicative: full result is `a * b`.
    Multiply,
    /// Maximum: full result is `max(a, b)`.
    Max,
    /// Minimum: full result is `min(a, b)`.
    Min,
    /// Arithmetic mean: full result is `(a + b) / 2`.
    Average,
}

impl BlendMode {
    /// Blend `a` and `b`. `factor` is clamped to `[0, 1]`.
    pub fn blend(self, a: f32, b: f32, factor: f32) -> f32 {
        let factor = factor.clamp(0.0, 1.0);
        let full = match self {
            BlendMode::Mix => b,
            BlendMode::Add => a + b,
            BlendMode::Subtract => a - b,
            BlendMode::Multiply => a * b,
            BlendMode::Max => a.max(b),
            BlendMode::Min => a.min(b),
            BlendMode::Average => 0.5 * (a + b),
        };
        a + (full - a) * factor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_is_lerp() {
        assert!((BlendMode::Mix.blend(10.0, 20.0, 0.0) - 10.0).abs() < f32::EPSILON);
        assert!((BlendMode::Mix.blend(10.0, 20.0, 0.5) - 15.0).abs() < f32::EPSILON);
        assert!((BlendMode::Mix.blend(10.0, 20.0, 1.0) - 20.0).abs() < f32::EPSILON);
    }

    #[test]
    fn add_full_and_partial() {
        assert!((BlendMode::Add.blend(10.0, 20.0, 1.0) - 30.0).abs() < f32::EPSILON);
        assert!((BlendMode::Add.blend(10.0, 20.0, 0.5) - 20.0).abs() < f32::EPSILON);
        assert!((BlendMode::Add.blend(10.0, 20.0, 0.0) - 10.0).abs() < f32::EPSILON);
    }

    #[test]
    fn subtract_full() {
        assert!((BlendMode::Subtract.blend(10.0, 4.0, 1.0) - 6.0).abs() < f32::EPSILON);
    }

    #[test]
    fn multiply_full() {
        assert!((BlendMode::Multiply.blend(3.0, 4.0, 1.0) - 12.0).abs() < f32::EPSILON);
    }

    #[test]
    fn max_and_min() {
        assert!((BlendMode::Max.blend(3.0, 7.0, 1.0) - 7.0).abs() < f32::EPSILON);
        assert!((BlendMode::Min.blend(3.0, 7.0, 1.0) - 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn average_full() {
        assert!((BlendMode::Average.blend(10.0, 20.0, 1.0) - 15.0).abs() < f32::EPSILON);
    }

    #[test]
    fn factor_is_clamped() {
        // factor > 1 behaves like 1, factor < 0 behaves like 0.
        assert!((BlendMode::Mix.blend(0.0, 10.0, 5.0) - 10.0).abs() < f32::EPSILON);
        assert!((BlendMode::Mix.blend(0.0, 10.0, -5.0) - 0.0).abs() < f32::EPSILON);
    }
}
