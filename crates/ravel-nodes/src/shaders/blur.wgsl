// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// Separable Gaussian blur — one dispatch per axis.
// The same shader handles both horizontal and vertical passes via `params.horizontal`.

struct Params {
    radius:     i32,
    horizontal: u32,   // 1 = horizontal, 0 = vertical
    sigma:      f32,
    _pad:       f32,
}

@group(0) @binding(0) var input_tex:  texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var<uniform> params: Params;

fn gaussian_weight(x: i32, sigma: f32) -> f32 {
    let xf = f32(x);
    return exp(-(xf * xf) / (2.0 * sigma * sigma));
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }

    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let r = params.radius;
    let sigma = params.sigma;

    var total = vec4<f32>(0.0);
    var weight_sum: f32 = 0.0;

    for (var i: i32 = -r; i <= r; i = i + 1) {
        var sample_coord: vec2<i32>;
        if (params.horizontal == 1u) {
            sample_coord = vec2<i32>(clamp(coord.x + i, 0, i32(dims.x) - 1), coord.y);
        } else {
            sample_coord = vec2<i32>(coord.x, clamp(coord.y + i, 0, i32(dims.y) - 1));
        }
        let w = gaussian_weight(i, sigma);
        total = total + textureLoad(input_tex, sample_coord, 0) * w;
        weight_sum = weight_sum + w;
    }

    textureStore(output_tex, coord, total / weight_sum);
}
