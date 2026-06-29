// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// Per-pixel brightness / contrast / saturation adjustment.

struct Params {
    brightness: f32,
    contrast:   f32,
    saturation: f32,
    _pad:       f32,
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
    var c = textureLoad(input_tex, coord, 0);

    // Brightness: additive offset.
    c = vec4<f32>(c.rgb + vec3<f32>(params.brightness), c.a);

    // Contrast: scale around mid-grey (0.5).
    c = vec4<f32>((c.rgb - vec3<f32>(0.5)) * params.contrast + vec3<f32>(0.5), c.a);

    // Saturation: lerp towards luminance.
    let lum = dot(c.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    c = vec4<f32>(mix(vec3<f32>(lum), c.rgb, params.saturation), c.a);

    textureStore(output_tex, coord, c);
}
