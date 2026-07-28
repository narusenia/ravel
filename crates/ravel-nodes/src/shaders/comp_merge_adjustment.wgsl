// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// `comp.merge.adjustment` — an adjustment layer's effect strength
// (REQ-LAYER-010).
//
// This is not a per-pixel composite: the adjusted stack (`fg`) already
// contains the lower stack with the layer's effects applied, so the merge is a
// straight `mix(background, adjusted, strength)` over the whole frame, with
// the layer's opacity acting as strength. Hence a separate shader from
// `comp_merge.wgsl` rather than a sixth mode in its `switch` — none of the
// blend/Porter-Duff machinery applies here.
//
// The mix happens in **premultiplied alpha** and converts back, matching the
// CPU reference in `comp/merge.rs`. Mixing straight alpha instead — what the
// user-facing `merge.wgsl` does for its `mix_val` — pulls a transparent
// texel's RGB (usually 0) into a visible one and darkens the result wherever
// the two sides disagree on alpha.
//
// Dimensions follow `comp_merge.wgsl`: the dispatch covers the output canvas,
// reads outside a side's own dimensions are transparent, and dimensions
// `(0, 0)` mark a side that is absent altogether.
//
// Prepend `premultiplied.wgsl` (see `gpu_util::with_premultiplied_helpers`).

struct Params {
    bg_width:   u32,
    bg_height:  u32,
    fg_width:   u32,
    fg_height:  u32,
    out_width:  u32,
    out_height: u32,
    strength:   f32,
    _pad0:      u32,
}

@group(0) @binding(0) var bg_tex:     texture_2d<f32>;
@group(0) @binding(1) var fg_tex:     texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> params: Params;

/// One texel, or transparent outside the side's own dimensions.
fn pixel_at(tex: texture_2d<f32>, x: u32, y: u32, width: u32, height: u32) -> vec4<f32> {
    if (x >= width || y >= height) {
        return vec4<f32>(0.0);
    }
    return textureLoad(tex, vec2<i32>(i32(x), i32(y)), 0);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.out_width || gid.y >= params.out_height) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));

    let b = premultiply(pixel_at(bg_tex, gid.x, gid.y, params.bg_width, params.bg_height));
    let f = premultiply(pixel_at(fg_tex, gid.x, gid.y, params.fg_width, params.fg_height));

    // All four channels, alpha included — the CPU reference mixes the whole
    // premultiplied vector.
    let mixed = b * (1.0 - params.strength) + f * params.strength;

    textureStore(output_tex, coord, un_premultiply(mixed));
}
