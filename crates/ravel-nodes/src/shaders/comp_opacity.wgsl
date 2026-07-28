// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// `comp.opacity` — the shell's layer opacity (REQ-LAYER-001).
//
// Buffers carry straight alpha, so layer opacity scales the alpha channel
// alone and leaves RGB untouched. This must stay the same arithmetic as the
// CPU reference path (`comp/opacity.rs`): the two are compared pixel-exactly.
//
// The identity case (opacity 1.0) never reaches this shader — the processor
// short-circuits and returns its input unchanged.

struct Params {
    opacity: f32,
    _pad0:   f32,
    _pad1:   f32,
    _pad2:   f32,
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

    textureStore(output_tex, coord, vec4<f32>(c.rgb, c.a * params.opacity));
}
