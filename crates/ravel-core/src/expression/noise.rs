// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The deterministic gradient noise behind the `noise` built-in.
//!
//! # What this is
//!
//! **Improved Perlin noise** (gradient noise) in three dimensions. Each
//! lattice point gets one of the twelve cube-edge gradients from a hash of its
//! coordinates; the value at a point is the interpolation of the eight corner
//! gradients dotted with the offsets to them, under the improved fade curve
//! `6t⁵ - 15t⁴ + 10t³`. `noise(x)` and `noise(x, y)` are the same function
//! with the unused coordinates at zero, so the 1D result is a slice of the 2D
//! result is a slice of the 3D one.
//!
//! Not simplex noise: that construction carries patent claims, and nothing
//! here needs its properties.
//!
//! # Why the values are pinned
//!
//! **The output of this function is picture.** An author who writes
//! `noise(time)` into a position parameter is looking at the result and
//! keeping it, so changing the hash later would silently re-render every
//! saved project. The concrete values are therefore part of the specification
//! (`docs/specifications/expression-language.md`) and a golden test asserts
//! them: replacing this implementation must break CI rather than break
//! somebody's shot.
//!
//! # Guarantees
//!
//! * **Deterministic.** No seed, no state, no time dependence. The same
//!   coordinates always give the same value, in this process and the next —
//!   which REQ-CORE-014 requires of every expression, because the three-tier
//!   cache keys results on inputs alone.
//! * **Ranged.** Every result is in `[-1, 1]`.
//! * **Zero on the lattice.** Gradient noise vanishes at integer coordinates.
//! * **Total.** Non-finite coordinates give `0.0` rather than letting a `NaN`
//!   leak out of a hash.

/// Scales the raw gradient-noise range to `[-1, 1]`.
///
/// With the twelve edge gradients the extreme value of 3D Perlin noise is
/// `√3 / 2`, so this is `2 / √3`.
const NORMALIZE: f64 = 1.154_700_538_379_251_5;

/// Improved Perlin noise sampled at `(x, y, z)`, in `[-1, 1]`.
pub(crate) fn perlin_noise_3d(x: f64, y: f64, z: f64) -> f64 {
    if !x.is_finite() || !y.is_finite() || !z.is_finite() {
        return 0.0;
    }

    let (ix, tx) = split(x);
    let (iy, ty) = split(y);
    let (iz, tz) = split(z);

    let (u, v, w) = (fade(tx), fade(ty), fade(tz));

    // `saturating_add`, because `split` saturates a huge coordinate onto
    // `i64::MAX` and the far corner of that cell would otherwise overflow.
    let corner = |dx: i64, dy: i64, dz: i64| {
        gradient_dot(
            hash(
                ix.saturating_add(dx),
                iy.saturating_add(dy),
                iz.saturating_add(dz),
            ),
            tx - dx as f64,
            ty - dy as f64,
            tz - dz as f64,
        )
    };

    let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), u);
    let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), u);
    let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), u);
    let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), u);

    let raw = lerp(lerp(x00, x10, v), lerp(x01, x11, v), w);

    // The normalized value is within [-1, 1] by construction; the clamp makes
    // the documented range exact rather than "up to rounding".
    (raw * NORMALIZE).clamp(-1.0, 1.0)
}

/// Split a coordinate into its lattice cell and the position within it.
///
/// `as i64` saturates in Rust, so a coordinate beyond the lattice range stays
/// defined (it flattens into one cell) instead of wrapping.
fn split(value: f64) -> (i64, f64) {
    let floor = value.floor();
    (floor as i64, (value - floor).clamp(0.0, 1.0))
}

/// Improved fade `6t⁵ - 15t⁴ + 10t³`: zero first and second derivatives at
/// both ends, so neighbouring cells join without a visible crease.
fn fade(t: f64) -> f64 {
    t * t * t * t.mul_add(t.mul_add(6.0, -15.0), 10.0)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Ken Perlin's twelve gradients — the midpoints of the cube's edges — chosen
/// by the low bits of the lattice hash.
fn gradient_dot(hash: u64, x: f64, y: f64, z: f64) -> f64 {
    match hash % 12 {
        0 => x + y,
        1 => -x + y,
        2 => x - y,
        3 => -x - y,
        4 => x + z,
        5 => -x + z,
        6 => x - z,
        7 => -x - z,
        8 => y + z,
        9 => -y + z,
        10 => y - z,
        _ => -y - z,
    }
}

/// Hash a lattice point.
///
/// A hash rather than Perlin's 256-entry permutation table: the table only
/// exists to be small, it repeats every 256 units, and this has neither
/// problem. The exact function is part of the specification — see the module
/// documentation.
fn hash(x: i64, y: i64, z: i64) -> u64 {
    let mut hash = 0x9e37_79b9_7f4a_7c15_u64;
    hash ^= (x as u64).wrapping_mul(0xd6e8_feb8_6659_fd93);
    hash = hash.rotate_left(29).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    hash ^= (y as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
    hash = hash.rotate_left(31).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    hash ^= (z as u64).wrapping_mul(0x1656_67b1_9e37_79f9);

    // splitmix64 finalizer: cheap, and good enough that neighbouring lattice
    // points do not correlate visibly.
    hash ^= hash >> 30;
    hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    hash ^= hash >> 27;
    hash = hash.wrapping_mul(0x94d0_49bb_1331_11eb);
    hash ^= hash >> 31;
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_coordinates_always_give_the_same_value() {
        for i in 0..64 {
            let x = i as f64 * 0.37;
            assert_eq!(
                perlin_noise_3d(x, 1.5, -2.25),
                perlin_noise_3d(x, 1.5, -2.25)
            );
        }
    }

    #[test]
    fn results_stay_inside_the_documented_range() {
        for i in -400..400 {
            let t = i as f64 * 0.117;
            for value in [
                perlin_noise_3d(t, 0.0, 0.0),
                perlin_noise_3d(t * 0.5, t * 1.5, 0.0),
                perlin_noise_3d(t, -t, t * 2.0),
                perlin_noise_3d(t * 31.7, t * -13.3, t * 7.1),
            ] {
                assert!((-1.0..=1.0).contains(&value), "{value} left [-1, 1]");
            }
        }
    }

    #[test]
    fn gradient_noise_vanishes_on_the_lattice() {
        for x in -4..4 {
            for y in -4..4 {
                for z in -4..4 {
                    let value = perlin_noise_3d(x as f64, y as f64, z as f64);
                    assert_eq!(value, 0.0, "({x}, {y}, {z}) is a lattice point");
                }
            }
        }
    }

    #[test]
    fn the_field_is_continuous_across_a_lattice_boundary() {
        let epsilon = 1e-6;
        let left = perlin_noise_3d(1.0 - epsilon, 0.25, 0.5);
        let right = perlin_noise_3d(1.0 + epsilon, 0.25, 0.5);
        assert!(
            (left - right).abs() < 1e-4,
            "a crease at the cell boundary: {left} vs {right}"
        );
    }

    #[test]
    fn the_field_actually_varies() {
        let samples: Vec<f64> = (0..32)
            .map(|i| perlin_noise_3d(i as f64 * 0.25 + 0.125, 0.0, 0.0))
            .collect();
        let spread = samples.iter().fold(f64::NEG_INFINITY, |a, b| a.max(*b))
            - samples.iter().fold(f64::INFINITY, |a, b| a.min(*b));
        assert!(spread > 0.5, "noise is too flat to be useful: {spread}");
    }

    #[test]
    fn non_finite_coordinates_are_total() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(perlin_noise_3d(value, 0.0, 0.0), 0.0);
            assert_eq!(perlin_noise_3d(0.0, value, 0.0), 0.0);
            assert_eq!(perlin_noise_3d(0.0, 0.0, value), 0.0);
        }
    }

    #[test]
    fn enormous_coordinates_stay_defined() {
        // `as i64` saturates rather than wrapping, so this must not panic and
        // must stay in range.
        let value = perlin_noise_3d(1e300, -1e300, 1e18);
        assert!((-1.0..=1.0).contains(&value));
    }

    /// The pinned values.
    ///
    /// These are not a sanity check on the maths — the other tests do that.
    /// They exist so that **changing this implementation fails CI**. The
    /// numbers `noise` returns end up in somebody's saved render, so the
    /// function is a compatibility surface: a better hash would silently move
    /// every keyframe that ever sampled it.
    #[test]
    fn golden_values_are_pinned() {
        let cases: [(f64, f64, f64, f64); 9] = [
            (0.5, 0.0, 0.0, 0.577_350_269_189_625_7),
            (1.5, 0.0, 0.0, -0.288_675_134_594_812_87),
            (-0.25, 0.0, 0.0, -0.169_145_586_676_648_17),
            (12.375, 0.0, 0.0, 0.198_613_919_355_470_46),
            (0.5, 0.5, 0.0, -0.144_337_567_297_406_43),
            (2.25, -3.75, 0.0, 0.320_600_262_871_783_6),
            (0.5, 0.5, 0.5, 0.0),
            (1.1, 2.2, 3.3, -0.014_533_187_181_875_866),
            (-4.5, 9.25, -0.125, 0.283_780_281_145_086_9),
        ];
        for (x, y, z, expected) in cases {
            assert_eq!(
                perlin_noise_3d(x, y, z),
                expected,
                "noise({x}, {y}, {z}) changed — see the module documentation \
                 before touching this"
            );
        }
    }
}
