// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// Time nodes' frame mix: out = a * (1 - t) + b * t, where `a` is the earlier
// source frame and `b` the later one. The mix runs in premultiplied alpha
// (GPUCOMP-4), so transparent pixels do not bleed their RGB into the result;
// at t = 0.5 with equal alpha the result is the arithmetic mean of both
// frames' straight RGB.
//
// Prepend `premultiplied.wgsl` (see `gpu_util::with_premultiplied_helpers`).

struct Params {
    t:     f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var a_tex:      texture_2d<f32>;
@group(0) @binding(1) var b_tex:      texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> params: Params;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(a_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let a = premultiply(textureLoad(a_tex, coord, 0));
    let b = premultiply(textureLoad(b_tex, coord, 0));
    textureStore(output_tex, coord, un_premultiply(mix(a, b, params.t)));
}
