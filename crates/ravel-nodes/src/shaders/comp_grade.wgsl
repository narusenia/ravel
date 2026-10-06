// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// The colour-adjustment nodes (`comp.brightness_contrast`, ...). One shader,
// one uniform: `params.mode.x` picks the operation and `a`/`b`/`c` carry its
// numbers (what each holds is documented beside `GradeKind::fill` in
// `comp/grade.rs`).
//
// Straight alpha (`premultiplied.wgsl` explains the convention): every
// operation here is per-pixel and mixes no neighbours, so it works on the
// straight RGB as stored and never touches alpha. Luminance weights are
// Rec.709, the same as `color_correct.wgsl`.

struct Params {
    mode: vec4<u32>,
    a: vec4<f32>,
    b: vec4<f32>,
    c: vec4<f32>,
}

@group(0) @binding(0) var input_tex:  texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var<uniform> params: Params;

// a.x = brightness, a.y = contrast, a.z = pivot.
fn brightness_contrast(rgb: vec3<f32>) -> vec3<f32> {
    return (rgb - vec3<f32>(params.a.z)) * params.a.y + vec3<f32>(params.a.z + params.a.x);
}

// a.xyz = (m0, m1, m2): the rotation about the gray axis, as the circulant
// matrix [[m0 m1 m2] [m2 m0 m1] [m1 m2 m0]]; b.x = saturation. Rotate first,
// then scale the distance from Rec.709 luminance.
fn hue_saturation(rgb: vec3<f32>) -> vec3<f32> {
    let m = params.a.xyz;
    let rotated = vec3<f32>(
        m.x * rgb.x + m.y * rgb.y + m.z * rgb.z,
        m.z * rgb.x + m.x * rgb.y + m.y * rgb.z,
        m.y * rgb.x + m.z * rgb.y + m.x * rgb.z,
    );
    let lum = dot(rotated, vec3<f32>(0.2126, 0.7152, 0.0722));
    return mix(vec3<f32>(lum), rotated, params.b.x);
}

// a = (in_black, in_white, gamma), b.xy = (out_black, out_white). The input
// range is clamped to [0, 1] after normalising, so `in_white <= in_black`
// degenerates to a hard threshold at `in_black` rather than dividing by zero.
fn levels(rgb: vec3<f32>) -> vec3<f32> {
    let span = max(params.a.y - params.a.x, 1e-6);
    let t = clamp((rgb - vec3<f32>(params.a.x)) / span, vec3<f32>(0.0), vec3<f32>(1.0));
    let curved = pow(t, vec3<f32>(1.0 / max(params.a.z, 1e-3)));
    return vec3<f32>(params.b.x) + curved * (params.b.y - params.b.x);
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
    switch params.mode.x {
        case 0u: { rgb = brightness_contrast(rgb); }
        case 1u: { rgb = hue_saturation(rgb); }
        case 2u: { rgb = levels(rgb); }
        default: {}
    }
    textureStore(output_tex, coord, vec4<f32>(rgb, src.a));
}
