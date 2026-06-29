// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// 2D affine transform with inverse-mapping and bilinear interpolation.

struct Params {
    // Inverse affine matrix (2x3, stored as 6 floats row-major).
    inv_m00: f32, inv_m01: f32, inv_m02: f32,
    inv_m10: f32, inv_m11: f32, inv_m12: f32,
    width:   f32,
    height:  f32,
}

@group(0) @binding(0) var input_tex:  texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var<uniform> params: Params;

fn bilinear_sample(tex: texture_2d<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {
    let px = uv.x - 0.5;
    let py = uv.y - 0.5;
    let x0 = i32(floor(px));
    let y0 = i32(floor(py));
    let fx = px - floor(px);
    let fy = py - floor(py);

    let w = i32(dims.x);
    let h = i32(dims.y);
    let x0c = clamp(x0, 0, w - 1);
    let y0c = clamp(y0, 0, h - 1);
    let x1c = clamp(x0 + 1, 0, w - 1);
    let y1c = clamp(y0 + 1, 0, h - 1);

    let c00 = textureLoad(tex, vec2<i32>(x0c, y0c), 0);
    let c10 = textureLoad(tex, vec2<i32>(x1c, y0c), 0);
    let c01 = textureLoad(tex, vec2<i32>(x0c, y1c), 0);
    let c11 = textureLoad(tex, vec2<i32>(x1c, y1c), 0);

    return mix(mix(c00, c10, fx), mix(c01, c11, fx), fy);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }

    let dst_x = f32(gid.x) + 0.5;
    let dst_y = f32(gid.y) + 0.5;

    // Apply inverse affine to find source coordinate.
    let src_x = params.inv_m00 * dst_x + params.inv_m01 * dst_y + params.inv_m02;
    let src_y = params.inv_m10 * dst_x + params.inv_m11 * dst_y + params.inv_m12;

    let dims_f = vec2<f32>(params.width, params.height);

    // Out-of-bounds → transparent.
    if (src_x < 0.0 || src_x >= dims_f.x || src_y < 0.0 || src_y >= dims_f.y) {
        textureStore(output_tex, vec2<i32>(i32(gid.x), i32(gid.y)), vec4<f32>(0.0));
        return;
    }

    let color = bilinear_sample(input_tex, vec2<f32>(src_x, src_y), dims_f);
    textureStore(output_tex, vec2<i32>(i32(gid.x), i32(gid.y)), color);
}
