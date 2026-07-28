// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// 2D affine transform with inverse-mapping and bilinear interpolation.
// Output dimensions equal input dimensions; the shell's transform, which maps
// a layer onto a differently sized canvas, lives in `comp_transform.wgsl`.
//
// Filtering runs in premultiplied alpha via the shared helpers (issue
// MED-GPU-02) so this node and the shell transform produce the same edges.
// Prepend `premultiplied.wgsl` (see `gpu_util::with_premultiplied_helpers`).

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

    // Out-of-bounds → transparent. Taps that individually fall outside the
    // source are transparent too (`premultiplied_texel`), so a scaled layer
    // does not smear its border row outwards.
    let color = sample_premultiplied_bilinear(input_tex, src_x, src_y, dims_f);
    textureStore(output_tex, vec2<i32>(i32(gid.x), i32(gid.y)), color);
}
