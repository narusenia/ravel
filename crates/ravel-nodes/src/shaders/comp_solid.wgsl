// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// `comp.solid` — one colour over the whole evaluation frame.
//
// The colour is written as given: frames carry straight alpha, so an RGBA
// colour is stored as-is (no premultiplication). There is no input texture, so
// the extent comes from the uniform.

struct Params {
    color:  vec4<f32>,
    width:  u32,
    height: u32,
    _pad0:  u32,
    _pad1:  u32,
}

@group(0) @binding(0) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(1) var<uniform> params: Params;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }
    textureStore(output_tex, vec2<i32>(i32(gid.x), i32(gid.y)), params.color);
}
