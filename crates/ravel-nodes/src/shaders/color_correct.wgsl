// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// The all-in-one grading node. Prepend `grade_stages.wgsl`
// (`gpu_util::with_grade_stages`): the stages that have a `comp.*` counterpart
// come from there, so a section run alone gives the same pixel as that node.
//
// Stages, in order, each skipped when its bit in `flags` is clear (the host
// clears it for a neutral parameter set, so an untouched stage is a bit-exact
// pass-through even for out-of-range values the curve tables would clamp):
//   0 white balance   1 exposure   2 brightness / contrast   3 tonal
//   4 saturation   5 vibrance   6 curves   7 HSL curves   8 wheels
//   9 fade   10 vignette
// Straight alpha, never touched.

struct Params {
    flags: vec4<u32>,
    // x = temperature, y = tint, z = exposure (stops), w = vibrance.
    wb: vec4<f32>,
    // = `brightness_contrast`'s `a`: brightness already scaled by contrast.
    bc: vec4<f32>,
    // x = saturation.
    sat: vec4<f32>,
    // highlights, shadows, whites, blacks.
    tone: vec4<f32>,
    lift: vec4<f32>,
    gamma: vec4<f32>,
    gain: vec4<f32>,
    // x = fade, y = vignette amount, z = midpoint, w = feather.
    creative: vec4<f32>,
    // Entries 0..255: RGB / red / green / blue curves; 256..511: HSL curves.
    table: array<vec4<f32>, 512>,
}

@group(0) @binding(0) var input_tex:  texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var<uniform> params: Params;

fn luma(rgb: vec3<f32>) -> f32 {
    return dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Per-channel gains of 2^(+-t / 2): warm raises red and lowers blue, a
// positive tint lowers green (towards magenta).
fn white_balance(rgb: vec3<f32>) -> vec3<f32> {
    return rgb * exp2(0.5 * vec3<f32>(params.wb.x, -params.wb.y, -params.wb.x));
}

// Four luminance bands, each smoothstep-weighted, added to every channel as
// `0.25 * amount * weight`: shadows 1 -> 0 over luma 0..0.5, highlights 0 -> 1
// over 0.5..1, blacks 1 -> 0 over 0..0.25, whites 0 -> 1 over 0.75..1.
fn tonal(rgb: vec3<f32>) -> vec3<f32> {
    let l = clamp(luma(rgb), 0.0, 1.0);
    let w = params.tone.x * smoothstep(0.5, 1.0, l)
        + params.tone.y * (1.0 - smoothstep(0.0, 0.5, l))
        + params.tone.z * smoothstep(0.75, 1.0, l)
        + params.tone.w * (1.0 - smoothstep(0.0, 0.25, l));
    return rgb + vec3<f32>(0.25 * w);
}

// Saturation scale of `1 + amount * (1 - chroma)`, chroma being the HSV
// saturation, so the dull colours move most and a gray not at all.
fn vibrance(rgb: vec3<f32>) -> vec3<f32> {
    let hw = hue_and_weight(rgb);
    return saturate_by(rgb, max(1.0 + params.wb.w * (1.0 - hw.y), 0.0));
}

// Lifts black to `fade` and leaves white where it is.
fn fade(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(params.creative.x) + rgb * (1.0 - params.creative.x);
}

// Distance from the frame centre, 0 there and 1 at the corners, over an
// ellipse so the same fractions work at any aspect. Starts at `midpoint`, over
// `feather`. A negative amount multiplies by `1 + amount * t` (darkens), a
// positive one mixes towards white by `amount * t`.
fn vignette(rgb: vec3<f32>, coord: vec2<i32>, dims: vec2<u32>) -> vec3<f32> {
    let p = ((vec2<f32>(coord) + vec2<f32>(0.5)) / vec2<f32>(dims) - vec2<f32>(0.5)) * 2.0;
    let r = length(p) * 0.70710678118;
    let t = smoothstep(params.creative.z, params.creative.z + max(params.creative.w, 1e-3), r);
    let amount = params.creative.y * t;
    if (amount < 0.0) {
        return rgb * (1.0 + amount);
    }
    return mix(rgb, vec3<f32>(1.0), amount);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }

    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let src = textureLoad(input_tex, coord, 0);
    var rgb = src.rgb;
    let f = params.flags.x;
    if ((f & 1u) != 0u)    { rgb = white_balance(rgb); }
    if ((f & 2u) != 0u)    { rgb = rgb * exp2(params.wb.z); }
    if ((f & 4u) != 0u)    { rgb = brightness_contrast(rgb, params.bc); }
    if ((f & 8u) != 0u)    { rgb = tonal(rgb); }
    if ((f & 16u) != 0u)   { rgb = saturate_by(rgb, params.sat.x); }
    if ((f & 32u) != 0u)   { rgb = vibrance(rgb); }
    if ((f & 64u) != 0u)   { rgb = curves(rgb, 0u); }
    if ((f & 128u) != 0u)  { rgb = hsl_curves(rgb, 256u); }
    if ((f & 256u) != 0u)  { rgb = lift_gamma_gain(rgb, params.lift, params.gamma, params.gain); }
    if ((f & 512u) != 0u)  { rgb = fade(rgb); }
    if ((f & 1024u) != 0u) { rgb = vignette(rgb, coord, dims); }
    textureStore(output_tex, coord, vec4<f32>(rgb, src.a));
}
