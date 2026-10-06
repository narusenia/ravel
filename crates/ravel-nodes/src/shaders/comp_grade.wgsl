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
        default: {}
    }
    textureStore(output_tex, coord, vec4<f32>(rgb, src.a));
}
