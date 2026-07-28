// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// `comp.transform` — the shell's layer transform (REQ-LAYER-001).
//
// Inverse mapping from the **output canvas** to the layer's source frame, so
// the dispatch covers `ctx.resolution` and the source dimensions arrive in the
// uniform. This is why it is a separate shader from `transform.wgsl`, whose
// output is the same size as its input.
//
// The inverse matrix is computed on the CPU from
// `ravel_core::composition::transform::world_matrix` — the same matrix the
// viewer's bbox and hit test use — and never re-derived here.
//
// Prepend `premultiplied.wgsl` (see `gpu_util::with_premultiplied_helpers`).

struct Params {
    // Inverse affine matrix (2x3, row-major).
    inv_m00: f32, inv_m01: f32, inv_m02: f32,
    inv_m10: f32, inv_m11: f32, inv_m12: f32,
    src_width:  f32,
    src_height: f32,
    out_width:  f32,
    out_height: f32,
    _pad0:      f32,
    _pad1:      f32,
}

@group(0) @binding(0) var input_tex:  texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    // Bounds come from the uniform, not `textureDimensions(input_tex)`: the
    // output canvas and the source frame need not be the same size.
    if (gid.x >= u32(params.out_width) || gid.y >= u32(params.out_height)) {
        return;
    }

    let dst_x = f32(gid.x) + 0.5;
    let dst_y = f32(gid.y) + 0.5;
    let src_x = params.inv_m00 * dst_x + params.inv_m01 * dst_y + params.inv_m02;
    let src_y = params.inv_m10 * dst_x + params.inv_m11 * dst_y + params.inv_m12;

    let dims = vec2<f32>(params.src_width, params.src_height);
    let color = sample_premultiplied_bilinear(input_tex, src_x, src_y, dims);
    textureStore(output_tex, vec2<i32>(i32(gid.x), i32(gid.y)), color);
}
