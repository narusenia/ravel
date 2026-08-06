// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The built-in functions and constants of the expression language.
//!
//! # The semantics are WGSL's
//!
//! Every function here means what the WGSL builtin of the same name means:
//! `round` breaks ties to even, `mix` does not clamp its factor, `clamp` is
//! `min(max(x, lo), hi)` including when the bounds are reversed, `atan2` takes
//! `y` first, `%` follows the sign of the dividend.
//!
//! That is a deliberate choice rather than an inherited accident. REQ-CORE-015
//! keeps the door open to compiling field expressions into shaders, and an
//! expression that means one thing on the CPU and another on the GPU would
//! change the picture the moment that door was used — silently, on projects
//! that were finished years earlier. Matching WGSL now costs a `round` that
//! surprises people who expect `0.5` to round up; matching it later would cost
//! a migration nobody can write.
//!
//! # Totality
//!
//! Every function is **total**: it returns an `f64` for every input, for every
//! argument count the parser can hand it. It never panics and there is no
//! error channel to return. That property is what lets `ChannelSource::evaluate`
//! keep a signature that returns a value rather than a `Result`.
//!
//! Totality is not the same as being defined mathematically. Out-of-domain
//! input produces the IEEE 754 answer and that answer propagates unchanged:
//! `sqrt(-1)` is `NaN`, `1/0` is `inf`, `log(0)` is `-inf`, `pow(-1, 0.5)` is
//! `NaN`. **This layer deliberately does not sanitize them.** REQ-CORE-014
//! puts that at the channel boundary, in one place, so that "a non-finite
//! result falls back to the default" is stated once instead of being spread
//! across twenty function bodies each inventing its own substitute.
//!
//! One function needs care to stay total: [`f64::clamp`] panics when
//! `min > max` or when a bound is `NaN`, so `clamp` is written out as
//! `min(max(x, lo), hi)` — which is also exactly what WGSL specifies.

use smol_str::SmolStr;

use super::noise::perlin_noise_3d;

/// How many arguments a built-in accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arity {
    /// Exactly this many.
    Exact(u8),
    /// Any count in `min..=max`.
    Range(u8, u8),
}

impl Arity {
    /// Whether a call passing `count` arguments is well-formed.
    pub const fn accepts(self, count: usize) -> bool {
        match self {
            Arity::Exact(n) => count == n as usize,
            Arity::Range(min, max) => count >= min as usize && count <= max as usize,
        }
    }

    /// Human-readable description for an error message.
    pub fn describe(self) -> SmolStr {
        match self {
            Arity::Exact(1) => SmolStr::new_static("1 argument"),
            Arity::Exact(n) => SmolStr::from(format!("{n} arguments")),
            Arity::Range(min, max) => SmolStr::from(format!("{min} to {max} arguments")),
        }
    }
}

/// A built-in function of the expression language.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Builtin {
    /// `sin(x)` — sine of `x` in radians.
    Sin,
    /// `cos(x)` — cosine of `x` in radians.
    Cos,
    /// `tan(x)` — tangent of `x` in radians.
    Tan,
    /// `asin(x)` — arc sine in radians; `NaN` outside `[-1, 1]`.
    Asin,
    /// `acos(x)` — arc cosine in radians; `NaN` outside `[-1, 1]`.
    Acos,
    /// `atan(x)` — arc tangent in radians.
    Atan,
    /// `atan2(y, x)` — angle of the vector `(x, y)`. `y` comes first, as in
    /// WGSL and C.
    Atan2,
    /// `exp(x)` — `e` raised to `x`.
    Exp,
    /// `exp2(x)` — 2 raised to `x`.
    Exp2,
    /// `log(x)` — **natural** logarithm; `-inf` at zero, `NaN` below it.
    Log,
    /// `log2(x)` — base-2 logarithm.
    Log2,
    /// `pow(a, b)` — `a` raised to `b`. The language has no `^` operator.
    Pow,
    /// `sqrt(x)` — square root; `NaN` for negative `x`.
    Sqrt,
    /// `abs(x)` — magnitude.
    Abs,
    /// `sign(x)` — `-1`, `0` or `1`; `NaN` stays `NaN`.
    Sign,
    /// `floor(x)` — largest integer not greater than `x`.
    Floor,
    /// `ceil(x)` — smallest integer not less than `x`.
    Ceil,
    /// `round(x)` — nearest integer, **ties to even** (`0.5` → `0`,
    /// `1.5` → `2`), as WGSL specifies.
    Round,
    /// `fract(x)` — `x - floor(x)`.
    Fract,
    /// `min(a, b)` — smaller value; the non-`NaN` one if exactly one is `NaN`.
    Min,
    /// `max(a, b)` — larger value; the non-`NaN` one if exactly one is `NaN`.
    Max,
    /// `clamp(x, lo, hi)` — `min(max(x, lo), hi)`, reversed bounds included.
    Clamp,
    /// `mix(a, b, t)` — `a + (b - a) * t`. **`t` is not clamped**, so values
    /// outside `[0, 1]` extrapolate.
    Mix,
    /// `step(edge, x)` — `0` below `edge`, `1` from `edge` up.
    Step,
    /// `smoothstep(e0, e1, x)` — Hermite ramp from 0 at `e0` to 1 at `e1`.
    Smoothstep,
    /// `noise(x)` / `noise(x, y)` / `noise(x, y, z)` — deterministic improved
    /// Perlin noise in `[-1, 1]` (see [`super::noise`]).
    Noise,
}

impl Builtin {
    /// Every built-in, in the order the specification tabulates them.
    pub const ALL: &'static [Builtin] = &[
        Builtin::Sin,
        Builtin::Cos,
        Builtin::Tan,
        Builtin::Asin,
        Builtin::Acos,
        Builtin::Atan,
        Builtin::Atan2,
        Builtin::Exp,
        Builtin::Exp2,
        Builtin::Log,
        Builtin::Log2,
        Builtin::Pow,
        Builtin::Sqrt,
        Builtin::Abs,
        Builtin::Sign,
        Builtin::Floor,
        Builtin::Ceil,
        Builtin::Round,
        Builtin::Fract,
        Builtin::Min,
        Builtin::Max,
        Builtin::Clamp,
        Builtin::Mix,
        Builtin::Step,
        Builtin::Smoothstep,
        Builtin::Noise,
    ];

    /// Look a built-in up by the name written in the source.
    pub fn from_name(name: &str) -> Option<Self> {
        let builtin = match name {
            "sin" => Builtin::Sin,
            "cos" => Builtin::Cos,
            "tan" => Builtin::Tan,
            "asin" => Builtin::Asin,
            "acos" => Builtin::Acos,
            "atan" => Builtin::Atan,
            "atan2" => Builtin::Atan2,
            "exp" => Builtin::Exp,
            "exp2" => Builtin::Exp2,
            "log" => Builtin::Log,
            "log2" => Builtin::Log2,
            "pow" => Builtin::Pow,
            "sqrt" => Builtin::Sqrt,
            "abs" => Builtin::Abs,
            "sign" => Builtin::Sign,
            "floor" => Builtin::Floor,
            "ceil" => Builtin::Ceil,
            "round" => Builtin::Round,
            "fract" => Builtin::Fract,
            "min" => Builtin::Min,
            "max" => Builtin::Max,
            "clamp" => Builtin::Clamp,
            "mix" => Builtin::Mix,
            "step" => Builtin::Step,
            "smoothstep" => Builtin::Smoothstep,
            "noise" => Builtin::Noise,
            _ => return None,
        };
        Some(builtin)
    }

    /// The name written in the source.
    pub const fn name(self) -> &'static str {
        match self {
            Builtin::Sin => "sin",
            Builtin::Cos => "cos",
            Builtin::Tan => "tan",
            Builtin::Asin => "asin",
            Builtin::Acos => "acos",
            Builtin::Atan => "atan",
            Builtin::Atan2 => "atan2",
            Builtin::Exp => "exp",
            Builtin::Exp2 => "exp2",
            Builtin::Log => "log",
            Builtin::Log2 => "log2",
            Builtin::Pow => "pow",
            Builtin::Sqrt => "sqrt",
            Builtin::Abs => "abs",
            Builtin::Sign => "sign",
            Builtin::Floor => "floor",
            Builtin::Ceil => "ceil",
            Builtin::Round => "round",
            Builtin::Fract => "fract",
            Builtin::Min => "min",
            Builtin::Max => "max",
            Builtin::Clamp => "clamp",
            Builtin::Mix => "mix",
            Builtin::Step => "step",
            Builtin::Smoothstep => "smoothstep",
            Builtin::Noise => "noise",
        }
    }

    /// How many arguments the built-in accepts.
    pub const fn arity(self) -> Arity {
        match self {
            Builtin::Sin
            | Builtin::Cos
            | Builtin::Tan
            | Builtin::Asin
            | Builtin::Acos
            | Builtin::Atan
            | Builtin::Exp
            | Builtin::Exp2
            | Builtin::Log
            | Builtin::Log2
            | Builtin::Sqrt
            | Builtin::Abs
            | Builtin::Sign
            | Builtin::Floor
            | Builtin::Ceil
            | Builtin::Round
            | Builtin::Fract => Arity::Exact(1),
            Builtin::Atan2 | Builtin::Pow | Builtin::Min | Builtin::Max | Builtin::Step => {
                Arity::Exact(2)
            }
            Builtin::Clamp | Builtin::Mix | Builtin::Smoothstep => Arity::Exact(3),
            Builtin::Noise => Arity::Range(1, 3),
        }
    }

    /// Apply the built-in.
    ///
    /// Total for every `args`, including counts the arity rejects: a missing
    /// argument reads as `0.0`. The compiler checks arity, so that path is
    /// unreachable in practice — it exists so that no arrangement of this
    /// function's inputs can panic.
    pub(crate) fn call(self, args: &[f64]) -> f64 {
        let arg = |index: usize| args.get(index).copied().unwrap_or(0.0);
        match self {
            Builtin::Sin => arg(0).sin(),
            Builtin::Cos => arg(0).cos(),
            Builtin::Tan => arg(0).tan(),
            Builtin::Asin => arg(0).asin(),
            Builtin::Acos => arg(0).acos(),
            Builtin::Atan => arg(0).atan(),
            Builtin::Atan2 => arg(0).atan2(arg(1)),
            Builtin::Exp => arg(0).exp(),
            Builtin::Exp2 => arg(0).exp2(),
            Builtin::Log => arg(0).ln(),
            Builtin::Log2 => arg(0).log2(),
            Builtin::Pow => arg(0).powf(arg(1)),
            Builtin::Sqrt => arg(0).sqrt(),
            Builtin::Abs => arg(0).abs(),
            Builtin::Sign => sign(arg(0)),
            Builtin::Floor => arg(0).floor(),
            Builtin::Ceil => arg(0).ceil(),
            Builtin::Round => arg(0).round_ties_even(),
            Builtin::Fract => arg(0) - arg(0).floor(),
            Builtin::Min => arg(0).min(arg(1)),
            Builtin::Max => arg(0).max(arg(1)),
            Builtin::Clamp => clamp(arg(0), arg(1), arg(2)),
            Builtin::Mix => mix(arg(0), arg(1), arg(2)),
            Builtin::Step => step(arg(0), arg(1)),
            Builtin::Smoothstep => smoothstep(arg(0), arg(1), arg(2)),
            Builtin::Noise => perlin_noise_3d(arg(0), arg(1), arg(2)),
        }
    }
}

fn sign(x: f64) -> f64 {
    if x.is_nan() {
        // `f64::signum` answers 1.0 here, which would be the one place this
        // layer invented a value instead of propagating IEEE.
        f64::NAN
    } else if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// `min(max(x, lo), hi)`, which is what WGSL's `clamp` is defined as.
///
/// Written out rather than delegated to [`f64::clamp`], which panics when the
/// bounds are reversed or `NaN`. Reversed bounds are not an error here: the
/// expression above simply yields `hi`.
fn clamp(x: f64, lo: f64, hi: f64) -> f64 {
    x.max(lo).min(hi)
}

fn mix(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn step(edge: f64, x: f64) -> f64 {
    if x < edge { 0.0 } else { 1.0 }
}

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The language's named constants.
///
/// These belong to the language rather than to a [`Scope`](super::Scope), so
/// that constant folding can collapse `2 * pi` before evaluation ever runs. A
/// scope that declares a variable of the same name is shadowed by the
/// constant.
pub const CONSTANTS: &[(&str, f64)] = &[("pi", std::f64::consts::PI), ("e", std::f64::consts::E)];

/// The value of the named constant, if the name is one.
pub fn constant(name: &str) -> Option<f64> {
    CONSTANTS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, value)| *value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(builtin: Builtin, args: &[f64]) -> f64 {
        builtin.call(args)
    }

    #[test]
    fn every_builtin_is_reachable_by_name() {
        for builtin in Builtin::ALL {
            assert_eq!(
                Builtin::from_name(builtin.name()),
                Some(*builtin),
                "`{}` does not round-trip through from_name",
                builtin.name()
            );
        }
        assert_eq!(Builtin::ALL.len(), 26);
        assert_eq!(Builtin::from_name("nope"), None);
        // Case sensitive, per the lexical rules.
        assert_eq!(Builtin::from_name("Sin"), None);
    }

    #[test]
    fn trigonometric_values() {
        assert!((call(Builtin::Sin, &[0.0])).abs() < 1e-12);
        assert!((call(Builtin::Sin, &[std::f64::consts::FRAC_PI_2]) - 1.0).abs() < 1e-12);
        assert!((call(Builtin::Cos, &[0.0]) - 1.0).abs() < 1e-12);
        assert!((call(Builtin::Tan, &[std::f64::consts::FRAC_PI_4]) - 1.0).abs() < 1e-12);
        assert!((call(Builtin::Asin, &[1.0]) - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!((call(Builtin::Acos, &[1.0])).abs() < 1e-12);
        assert!((call(Builtin::Atan, &[1.0]) - std::f64::consts::FRAC_PI_4).abs() < 1e-12);
    }

    #[test]
    fn atan2_takes_y_before_x() {
        // The WGSL argument order. Swapping them mirrors the angle, which is
        // the kind of bug that only shows up as a rotation going backwards.
        assert!((call(Builtin::Atan2, &[1.0, 0.0]) - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!(call(Builtin::Atan2, &[0.0, 1.0]).abs() < 1e-12);
    }

    #[test]
    fn exponential_and_logarithmic_values() {
        assert!((call(Builtin::Exp, &[1.0]) - std::f64::consts::E).abs() < 1e-12);
        assert_eq!(call(Builtin::Exp2, &[10.0]), 1024.0);
        assert!(
            (call(Builtin::Log, &[std::f64::consts::E]) - 1.0).abs() < 1e-12,
            "log is the natural logarithm"
        );
        assert_eq!(call(Builtin::Log2, &[1024.0]), 10.0);
        assert_eq!(call(Builtin::Pow, &[2.0, 10.0]), 1024.0);
        assert_eq!(call(Builtin::Sqrt, &[144.0]), 12.0);
    }

    #[test]
    fn rounding_follows_wgsl() {
        assert_eq!(call(Builtin::Abs, &[-3.5]), 3.5);
        assert_eq!(call(Builtin::Floor, &[-1.5]), -2.0);
        assert_eq!(call(Builtin::Ceil, &[-1.5]), -1.0);
        // Ties to even, not away from zero: this is the one place the WGSL
        // choice contradicts the intuition most people bring.
        assert_eq!(call(Builtin::Round, &[0.5]), 0.0);
        assert_eq!(call(Builtin::Round, &[1.5]), 2.0);
        assert_eq!(call(Builtin::Round, &[2.5]), 2.0);
        assert_eq!(call(Builtin::Round, &[-0.5]), -0.0);
        assert_eq!(call(Builtin::Round, &[-1.5]), -2.0);
        assert_eq!(call(Builtin::Fract, &[1.25]), 0.25);
        assert_eq!(call(Builtin::Fract, &[-0.25]), 0.75);
    }

    #[test]
    fn sign_values() {
        assert_eq!(call(Builtin::Sign, &[-4.0]), -1.0);
        assert_eq!(call(Builtin::Sign, &[0.0]), 0.0);
        assert_eq!(call(Builtin::Sign, &[-0.0]), 0.0);
        assert_eq!(call(Builtin::Sign, &[4.0]), 1.0);
        assert_eq!(call(Builtin::Sign, &[f64::INFINITY]), 1.0);
        assert!(call(Builtin::Sign, &[f64::NAN]).is_nan());
    }

    #[test]
    fn interpolation_values() {
        assert_eq!(call(Builtin::Min, &[3.0, -1.0]), -1.0);
        assert_eq!(call(Builtin::Max, &[3.0, -1.0]), 3.0);
        assert_eq!(call(Builtin::Clamp, &[5.0, 0.0, 1.0]), 1.0);
        assert_eq!(call(Builtin::Clamp, &[-5.0, 0.0, 1.0]), 0.0);
        assert_eq!(call(Builtin::Clamp, &[0.25, 0.0, 1.0]), 0.25);
        assert_eq!(call(Builtin::Mix, &[10.0, 20.0, 0.25]), 12.5);
        assert_eq!(
            call(Builtin::Mix, &[10.0, 20.0, 2.0]),
            30.0,
            "mix extrapolates: t is not clamped"
        );
        assert_eq!(call(Builtin::Step, &[1.0, 0.5]), 0.0);
        assert_eq!(call(Builtin::Step, &[1.0, 1.0]), 1.0);
        assert_eq!(call(Builtin::Step, &[1.0, 2.0]), 1.0);
        assert_eq!(call(Builtin::Smoothstep, &[0.0, 1.0, 0.5]), 0.5);
        assert_eq!(call(Builtin::Smoothstep, &[0.0, 1.0, -1.0]), 0.0);
        assert_eq!(call(Builtin::Smoothstep, &[0.0, 1.0, 2.0]), 1.0);
        assert!(
            call(Builtin::Smoothstep, &[0.0, 1.0, 0.25]) < 0.25,
            "eased in"
        );
    }

    #[test]
    fn clamp_is_total_where_the_std_one_panics() {
        // `f64::clamp` panics on all of these. WGSL defines clamp as
        // min(max(x, lo), hi), so reversed bounds collapse onto `hi`.
        assert_eq!(call(Builtin::Clamp, &[0.5, 1.0, 0.0]), 0.0);
        assert_eq!(call(Builtin::Clamp, &[5.0, 1.0, 0.0]), 0.0);
        assert!(call(Builtin::Clamp, &[0.5, f64::NAN, 1.0]).is_finite());
        assert!(call(Builtin::Clamp, &[0.5, 0.0, f64::NAN]).is_finite());
    }

    #[test]
    fn smoothstep_survives_coincident_edges() {
        // (x - e) / 0 is ±inf or NaN; the clamp turns the first two into the
        // step function the limit implies, and NaN into 0 via `max`.
        assert_eq!(call(Builtin::Smoothstep, &[1.0, 1.0, 0.0]), 0.0);
        assert_eq!(call(Builtin::Smoothstep, &[1.0, 1.0, 2.0]), 1.0);
        assert!(call(Builtin::Smoothstep, &[1.0, 1.0, 1.0]).is_finite());
    }

    #[test]
    fn out_of_domain_input_yields_the_ieee_answer() {
        // REQ-CORE-014 fixes these; the channel boundary, not this layer, is
        // where a non-finite result becomes a default.
        assert!(call(Builtin::Sqrt, &[-1.0]).is_nan());
        assert_eq!(call(Builtin::Log, &[0.0]), f64::NEG_INFINITY);
        assert!(call(Builtin::Log, &[-1.0]).is_nan());
        assert_eq!(call(Builtin::Log2, &[0.0]), f64::NEG_INFINITY);
        assert!(call(Builtin::Pow, &[-1.0, 0.5]).is_nan());
        assert!(call(Builtin::Asin, &[2.0]).is_nan());
        assert!(call(Builtin::Acos, &[-2.0]).is_nan());
        assert!(call(Builtin::Exp, &[1e308]).is_infinite());
    }

    #[test]
    fn every_builtin_returns_a_value_for_every_hostile_input() {
        let hostile = [
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.5,
            1e308,
            -1e308,
            f64::MIN_POSITIVE,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ];
        for builtin in Builtin::ALL {
            for a in hostile {
                for b in hostile {
                    for c in hostile {
                        // The contract is "returns", not "returns something
                        // particular": reaching the assert is the test.
                        let value = builtin.call(&[a, b, c]);
                        assert!(value.is_nan() || value.is_finite() || value.is_infinite());
                    }
                }
            }
            // Arity mismatches are unreachable after compilation, but must
            // still not panic.
            let _ = builtin.call(&[]);
            let _ = builtin.call(&[1.0; 8]);
        }
    }

    #[test]
    fn noise_is_deterministic_ranged_and_dimension_nested() {
        assert_eq!(Builtin::Noise.arity(), Arity::Range(1, 3));
        for i in 0..128 {
            let x = i as f64 * 0.31;
            let one = Builtin::Noise.call(&[x]);
            assert_eq!(
                one,
                Builtin::Noise.call(&[x]),
                "noise must be deterministic"
            );
            assert_eq!(one, Builtin::Noise.call(&[x, 0.0]), "1D is 2D at y = 0");
            assert_eq!(
                one,
                Builtin::Noise.call(&[x, 0.0, 0.0]),
                "2D is 3D at z = 0"
            );
            assert!((-1.0..=1.0).contains(&one));
        }
    }

    #[test]
    fn arity_descriptions_read_as_english() {
        assert_eq!(Arity::Exact(1).describe(), "1 argument");
        assert_eq!(Arity::Exact(3).describe(), "3 arguments");
        assert_eq!(Arity::Range(1, 3).describe(), "1 to 3 arguments");
        assert!(Arity::Exact(2).accepts(2));
        assert!(!Arity::Exact(2).accepts(3));
        assert!(Arity::Range(1, 3).accepts(2));
        assert!(!Arity::Range(1, 3).accepts(4));
        assert!(!Arity::Range(1, 3).accepts(0));
    }

    #[test]
    fn the_named_constants_are_the_std_ones() {
        assert_eq!(constant("pi"), Some(std::f64::consts::PI));
        assert_eq!(constant("e"), Some(std::f64::consts::E));
        assert_eq!(constant("tau"), None);
        assert_eq!(CONSTANTS.len(), 2);
    }
}
