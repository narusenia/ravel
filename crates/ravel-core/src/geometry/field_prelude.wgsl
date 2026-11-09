// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Helpers shared by every WGSL field function (`field_wgsl.rs`).
//
// Every field parameter reaches the shader as a raw 32-bit word of
// `field_params` (declared by `FieldProgram::wgsl`), so the *source* depends on
// the shape of the field tree and never on a number: animating a noise
// frequency or the clock re-uploads words and recompiles nothing. Integers
// read the word as is, floats go through `pf`.
//
// Each helper mirrors a CPU function of `ravel-core`'s `geometry/field.rs` or a
// parameter curve; the name of the original is in the comment. The CPU
// version is the reference, and the parity tests hold the two together.

fn pf(k: u32) -> f32 {
    return bitcast<f32>(field_params[k]);
}

// NaN and infinity are tested on the bits: WGSL lets a compiler assume neither
// occurs, which would fold `x != x` and `isInf` away. The CPU handles both
// explicitly (`CurveParam::evaluate`, `RampParam::evaluate`, `unit_vector`).
fn is_nan_bits(x: f32) -> bool {
    return (bitcast<u32>(x) & 0x7fffffffu) > 0x7f800000u;
}

fn is_finite_bits(x: f32) -> bool {
    return (bitcast<u32>(x) & 0x7f800000u) != 0x7f800000u;
}

// ---- noise: `simplex_2d` / `hash_lattice` / `NoiseField::sample` -------------

var<private> SIMPLEX_GRADIENTS: array<vec2<f32>, 8> = array<vec2<f32>, 8>(
    vec2<f32>(1.0, 1.0),
    vec2<f32>(-1.0, 1.0),
    vec2<f32>(1.0, -1.0),
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(1.0, 0.0),
    vec2<f32>(-1.0, 0.0),
    vec2<f32>(0.0, 1.0),
    vec2<f32>(0.0, -1.0),
);

// Integer hash: the same wrapping 32-bit multiplies and xor-shifts as the CPU
// side, so a lattice point picks the same gradient on both.
fn hash_lattice(x: i32, y: i32, seed: u32) -> u32 {
    var h = seed ^ (bitcast<u32>(x) * 0x9e3779b9u);
    h = h ^ (bitcast<u32>(y) * 0x85ebca6bu);
    h = h ^ (h >> 16u);
    h = h * 0x7feb352du;
    h = h ^ (h >> 15u);
    h = h * 0x846ca68bu;
    return h ^ (h >> 16u);
}

fn simplex_corner(dx: f32, dy: f32, lattice_x: i32, lattice_y: i32, seed: u32) -> f32 {
    let attenuation = 0.5 - dx * dx - dy * dy;
    if (attenuation <= 0.0) {
        return 0.0;
    }
    let gradient = SIMPLEX_GRADIENTS[hash_lattice(lattice_x, lattice_y, seed) & 7u];
    let attenuation2 = attenuation * attenuation;
    return attenuation2 * attenuation2 * (gradient.x * dx + gradient.y * dy);
}

fn simplex_2d(x: f32, y: f32, seed: u32) -> f32 {
    let f2 = 0.36602542;
    let g2 = 0.21132487;
    let skew = (x + y) * f2;
    let i = i32(floor(x + skew));
    let j = i32(floor(y + skew));
    let unskew = f32(i + j) * g2;
    let x0 = x - (f32(i) - unskew);
    let y0 = y - (f32(j) - unskew);
    var i1 = 0;
    var j1 = 1;
    if (x0 > y0) {
        i1 = 1;
        j1 = 0;
    }
    let x1 = x0 - f32(i1) + g2;
    let y1 = y0 - f32(j1) + g2;
    let x2 = x0 - 1.0 + 2.0 * g2;
    let y2 = y0 - 1.0 + 2.0 * g2;
    return 70.0 * (simplex_corner(x0, y0, i, j, seed)
        + simplex_corner(x1, y1, i + i1, j + j1, seed)
        + simplex_corner(x2, y2, i + 1, j + 1, seed));
}

// `octaves` is already at least 1 (the CPU side applies `.max(1)`).
fn fbm_noise(p: vec2<f32>, seed: u32, frequency: f32, octaves: u32) -> f32 {
    var amplitude = 1.0;
    var freq = frequency;
    var total = 0.0;
    var normalization = 0.0;
    for (var octave = 0u; octave < octaves; octave++) {
        total += amplitude * simplex_2d(p.x * freq, p.y * freq, seed + octave);
        normalization += amplitude;
        amplitude *= 0.5;
        freq *= 2.0;
    }
    return total / normalization;
}

// ---- geometry: `unit_vector` / `smooth_falloff` -----------------------------

fn unit_vector(v: vec2<f32>) -> vec2<f32> {
    let len = length(v);
    if (len > 0.0 && is_finite_bits(len)) {
        return v / len;
    }
    return vec2<f32>(0.0, 0.0);
}

fn smooth_falloff(dist: f32, inner: f32, outer: f32) -> f32 {
    if (outer <= inner) {
        if (dist <= inner) {
            return 1.0;
        }
        return 0.0;
    }
    let t = clamp((dist - inner) / (outer - inner), 0.0, 1.0);
    let smooth_t = t * t * (3.0 - 2.0 * t);
    return 1.0 - smooth_t;
}

// `f32::atan2` for the zero vector: `atan2(+-0, +0)` is `+-0` and
// `atan2(+-0, -0)` is `+-pi`, which a raw WGSL `atan2` leaves unspecified. A
// radial field's tangent at its own centre is `(-0, 0)`, so this is reachable.
fn atan2_ieee(y: f32, x: f32) -> f32 {
    if (x == 0.0 && y == 0.0) {
        let x_negative = (bitcast<u32>(x) >> 31u) != 0u;
        let y_negative = (bitcast<u32>(y) >> 31u) != 0u;
        var angle = 0.0;
        if (x_negative) {
            angle = 3.1415927;
        }
        if (y_negative) {
            return -angle;
        }
        return angle;
    }
    return atan2(y, x);
}

// ---- `CurveParam::evaluate` --------------------------------------------------
//
// `base` indexes `field_params`: a point count, then seven words per point
// (x, y, mode, tangent_out.xy, tangent_in.xy), mode 0 bezier / 1 linear /
// 2 step. A scan stands in for the CPU binary search; the point count of a
// transfer curve is small and the result is the same first point at or after x.

fn curve_cubic(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let u = 1.0 - t;
    return u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3;
}

fn curve_eval(base: u32, x: f32) -> f32 {
    let count = field_params[base];
    if (count == 0u) {
        return x;
    }
    let first = base + 1u;
    let last = first + (count - 1u) * 7u;
    // NaN reads the last output, as on the CPU (it would otherwise reach the
    // scan below and interpolate between the wrong points).
    if (is_nan_bits(x) || count == 1u) {
        return pf(last + 1u);
    }
    if (x <= pf(first)) {
        return pf(first + 1u);
    }
    if (x >= pf(last)) {
        return pf(last + 1u);
    }
    var idx = 1u;
    loop {
        if (idx >= count || !(pf(first + idx * 7u) < x)) {
            break;
        }
        idx++;
    }
    let right = first + idx * 7u;
    let left = right - 7u;
    if (pf(right) == x) {
        return pf(right + 1u);
    }
    let mode = field_params[left + 2u];
    let x0 = pf(left);
    let v0 = pf(left + 1u);
    let x1 = pf(right);
    let v1 = pf(right + 1u);
    if (mode == 2u) {
        return v0;
    }
    if (x1 <= x0) {
        return v1;
    }
    if (mode == 1u) {
        return v0 + (v1 - v0) * ((x - x0) / (x1 - x0));
    }
    let cx1 = clamp(x0 + pf(left + 3u), x0, x1);
    let cx2 = clamp(x1 + pf(right + 5u), x0, x1);
    let cy1 = v0 + pf(left + 4u);
    let cy2 = v1 + pf(right + 6u);
    var lo = 0.0;
    var hi = 1.0;
    var mid = 0.5;
    for (var iteration = 0u; iteration < 60u; iteration++) {
        mid = 0.5 * (lo + hi);
        if (curve_cubic(x0, cx1, cx2, x1, mid) < x) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    return curve_cubic(v0, cy1, cy2, v1, mid);
}

// ---- `RampParam::evaluate` ---------------------------------------------------
//
// `base` indexes `field_params`: a stop count, then five words per stop
// (position, r, g, b, a). `mode` is 0 linear / 1 smooth / 2 constant.

fn ramp_stop_color(at: u32) -> vec4<f32> {
    return vec4<f32>(pf(at + 1u), pf(at + 2u), pf(at + 3u), pf(at + 4u));
}

fn ramp_eval(base: u32, mode: u32, position: f32) -> vec4<f32> {
    let count = field_params[base];
    let first = base + 1u;
    let last = first + (count - 1u) * 5u;
    if (is_nan_bits(position) || count == 1u) {
        return ramp_stop_color(last);
    }
    if (position <= pf(first)) {
        return ramp_stop_color(first);
    }
    if (position >= pf(last)) {
        return ramp_stop_color(last);
    }
    var idx = 1u;
    loop {
        if (idx >= count || !(pf(first + idx * 5u) < position)) {
            break;
        }
        idx++;
    }
    let right = first + idx * 5u;
    let left = right - 5u;
    if (pf(right) == position) {
        return ramp_stop_color(right);
    }
    let span = pf(right) - pf(left);
    var t = 0.0;
    if (span > 0.0) {
        t = (position - pf(left)) / span;
    }
    let a = ramp_stop_color(left);
    let b = ramp_stop_color(right);
    if (mode == 2u) {
        return a;
    }
    if (mode == 1u) {
        t = t * t * (3.0 - 2.0 * t);
    }
    return a * (1.0 - t) + b * t;
}
