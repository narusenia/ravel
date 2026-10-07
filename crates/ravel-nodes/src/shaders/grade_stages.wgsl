// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// Not a module: the colour-grading stages shared by `comp_grade.wgsl` (one
// stage per node) and `color_correct.wgsl` (all of them in one pass), so a
// section of the combined node and the matching standalone node run the same
// code. Prepend this file (`gpu_util::with_grade_stages`); the including
// shader declares `params` with a `table: array<vec4<f32>, N>` field. A stage
// that reads the table takes `base`, the index of its first entry.

// a.x = brightness, a.y = contrast, a.z = pivot.
fn brightness_contrast(rgb: vec3<f32>, a: vec4<f32>) -> vec3<f32> {
    return (rgb - vec3<f32>(a.z)) * a.y + vec3<f32>(a.z + a.x);
}

// Rotation about the gray axis as the circulant matrix
// [[m0 m1 m2] [m2 m0 m1] [m1 m2 m0]].
fn rotate_gray(rgb: vec3<f32>, m: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        m.x * rgb.x + m.y * rgb.y + m.z * rgb.z,
        m.z * rgb.x + m.x * rgb.y + m.y * rgb.z,
        m.y * rgb.x + m.z * rgb.y + m.x * rgb.z,
    );
}

// Scale the distance from Rec.709 luminance.
fn saturate_by(rgb: vec3<f32>, amount: f32) -> vec3<f32> {
    let lum = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    return mix(vec3<f32>(lum), rgb, amount);
}

// a.xyz = (m0, m1, m2); b.x = saturation. Rotate first, then saturate.
fn hue_saturation(rgb: vec3<f32>, a: vec4<f32>, b: vec4<f32>) -> vec3<f32> {
    return saturate_by(rotate_gray(rgb, a.xyz), b.x);
}

// a = (in_black, in_white, gamma), b.xy = (out_black, out_white). The input
// range is clamped to [0, 1] after normalising, so `in_white <= in_black`
// degenerates to a hard threshold at `in_black` rather than dividing by zero.
fn levels(rgb: vec3<f32>, a: vec4<f32>, b: vec4<f32>) -> vec3<f32> {
    let span = max(a.y - a.x, 1e-6);
    let t = clamp((rgb - vec3<f32>(a.x)) / span, vec3<f32>(0.0), vec3<f32>(1.0));
    let curved = pow(t, vec3<f32>(1.0 / max(a.z, 1e-3)));
    return vec3<f32>(b.x) + curved * (b.y - b.x);
}

// Linear lookup of lane `lane` of the table over [0, 1]; `v` is clamped, so a
// value outside the curve's domain takes the curve's end value.
fn table_at(v: f32, lane: u32, base: u32) -> f32 {
    let pos = clamp(v, 0.0, 1.0) * 255.0;
    let i0 = u32(floor(pos));
    let i1 = min(i0 + 1u, 255u);
    return mix(params.table[base + i0][lane], params.table[base + i1][lane], pos - f32(i0));
}

// Lanes: 0 = the RGB curve, 1..3 = the red, green and blue curves. Each
// channel goes through the RGB curve first, then its own.
fn curves(rgb: vec3<f32>, base: u32) -> vec3<f32> {
    return vec3<f32>(
        table_at(table_at(rgb.x, 0u, base), 1u, base),
        table_at(table_at(rgb.y, 0u, base), 2u, base),
        table_at(table_at(rgb.z, 0u, base), 3u, base),
    );
}

// a.xyz = lift, b.xyz = gamma, c.xyz = gain, per channel:
// `lift + (gain - lift) * pow(x, 1 / gamma)`. The gamma bends the 0..1 ramp
// *before* it is stretched between lift and gain, so black is exactly the lift
// and white exactly the gain whatever the gamma. A negative input is clipped to
// black; an input above 1 (HDR) continues past the gain along the same curve.
fn lift_gamma_gain(rgb: vec3<f32>, a: vec4<f32>, b: vec4<f32>, c: vec4<f32>) -> vec3<f32> {
    let t = pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(1.0) / max(b.xyz, vec3<f32>(1e-3)));
    return a.xyz + (c.xyz - a.xyz) * t;
}

// Hue of a colour in [0, 1) (red = 0, green = 1/3, blue = 2/3) and its
// chroma weight in [0, 1] (the HSV saturation: 0 for any gray, so a gray has
// no hue to select on).
fn hue_and_weight(rgb: vec3<f32>) -> vec2<f32> {
    let hi = max(rgb.x, max(rgb.y, rgb.z));
    let d = hi - min(rgb.x, min(rgb.y, rgb.z));
    if (d <= 1e-6) {
        return vec2<f32>(0.0, 0.0);
    }
    var h = 0.0;
    if (hi == rgb.x) {
        h = (rgb.y - rgb.z) / d;
    } else if (hi == rgb.y) {
        h = (rgb.z - rgb.x) / d + 2.0;
    } else {
        h = (rgb.x - rgb.y) / d + 4.0;
    }
    return vec2<f32>(fract(h / 6.0), clamp(d / max(hi, 1e-6), 0.0, 1.0));
}

// Periodic lookup of table lane `lane` over a hue in [0, 1): the entry after
// the last is the first, so the seam interpolates across one entry step.
fn hue_table_at(h: f32, lane: u32, base: u32) -> f32 {
    let pos = h * 256.0;
    let i0 = u32(floor(pos)) % 256u;
    let i1 = (i0 + 1u) % 256u;
    return mix(params.table[base + i0][lane], params.table[base + i1][lane], fract(pos));
}

// Lanes: 0 = hue vs hue (0.5 is no shift, a full unit is 360 degrees),
// 1 = hue vs saturation (a scale of 2y, 0.5 is unchanged), 2 = hue vs
// luminance (a gain of 2y, 0.5 is unchanged, weighted by chroma so grays stay
// put).
fn hsl_curves(rgb: vec3<f32>, base: u32) -> vec3<f32> {
    let hw = hue_and_weight(rgb);
    let angle = (hue_table_at(hw.x, 0u, base) - 0.5) * 6.28318530718;
    let third = (1.0 - cos(angle)) / 3.0;
    let side = sin(angle) / sqrt(3.0);
    let rotated = rotate_gray(rgb, vec3<f32>(cos(angle) + third, third - side, third + side));
    let saturated = saturate_by(rotated, 2.0 * hue_table_at(hw.x, 1u, base));
    return saturated * mix(1.0, 2.0 * hue_table_at(hw.x, 2u, base), hw.y);
}
