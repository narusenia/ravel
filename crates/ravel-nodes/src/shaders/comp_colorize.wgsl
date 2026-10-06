// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// `comp.fill` and `comp.tint` — map each pixel's luminance onto the line
// between two colours; the alpha channel is carried through untouched.
//
// `comp.fill` is the degenerate case `color_a == color_b`: the interpolation
// is written as `a + (b - a) * t` rather than `mix`, so `b - a` is exactly 0
// and the result is exactly `a` whatever the luminance.
//
// Straight alpha (`premultiplied.wgsl` explains the convention): this is a
// per-pixel colour operation that mixes no neighbours, so it works on the
// straight RGB as stored and never touches alpha. The luminance weights are
// Rec.709, the same as `color_correct.wgsl`.

struct Params {
    color_a: vec4<f32>,
    color_b: vec4<f32>,
}

@group(0) @binding(0) var input_tex:  texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }

    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(input_tex, coord, 0);
    let t = clamp(dot(c.rgb, vec3<f32>(0.2126, 0.7152, 0.0722)), 0.0, 1.0);
    let rgb = params.color_a.rgb + (params.color_b.rgb - params.color_a.rgb) * t;
    textureStore(output_tex, coord, vec4<f32>(rgb, c.a));
}
