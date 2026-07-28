// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// `comp.merge.{normal,add,multiply,screen,overlay}` — the shell's layer
// compositing (REQ-LAYER-001/010).
//
// Straight-alpha Porter-Duff *over* in two stages, the same order as the CPU
// reference in `comp/merge.rs` (the W3C compositing model):
//
//   1. per-channel blend      `blended = B(Cb, Cf)`
//   2. weight by the backdrop `mixed = (1 - ab) * Cf + ab * blended`
//   3. Porter-Duff over       `Co = (af * mixed + (1 - af) * ab * Cb) / ao`
//
// This is deliberately *not* an extension of the user-facing `merge.wgsl`,
// which writes each mode's composite out as one expression and skips
// Porter-Duff entirely for `add`.
//
// The dispatch covers the output canvas (`ctx.resolution`); either side may be
// a different size, and reads outside a side's own dimensions are transparent,
// which is what pads an undersized layer and crops an oversized one. A side
// that is absent altogether carries dimensions `(0, 0)` — see the stand-in
// texture note in `comp/merge.rs` — so every coordinate reads as transparent,
// matching the CPU reference's 0x0 `empty_frame`.

struct Params {
    bg_width:   u32,
    bg_height:  u32,
    fg_width:   u32,
    fg_height:  u32,
    out_width:  u32,
    out_height: u32,
    // 0 Normal, 1 Add, 2 Multiply, 3 Screen, 4 Overlay.
    mode:       u32,
    _pad0:      u32,
}

@group(0) @binding(0) var bg_tex:     texture_2d<f32>;
@group(0) @binding(1) var fg_tex:     texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> params: Params;

/// One texel, or transparent outside the side's own dimensions.
///
/// **Must stay identical to `comp_merge_adjustment.wgsl`'s copy** — and to the
/// CPU reference's `pixel_at` in `comp/merge.rs`. This one rule is what pads an
/// undersized side, crops an oversized one, and makes a `(0, 0)` side absent;
/// letting the two merge shaders drift is how MED-GPU-02 happened.
fn pixel_at(tex: texture_2d<f32>, x: u32, y: u32, width: u32, height: u32) -> vec4<f32> {
    if (x >= width || y >= height) {
        return vec4<f32>(0.0);
    }
    return textureLoad(tex, vec2<i32>(i32(x), i32(y)), 0);
}

/// Per-channel colour blend `B(Cb, Cf)` on straight colours.
fn blend_rgb(mode: u32, cb: vec3<f32>, cf: vec3<f32>) -> vec3<f32> {
    switch (mode) {
        // Normal
        case 0u: {
            return cf;
        }
        // Add
        case 1u: {
            return cb + cf;
        }
        // Multiply
        case 2u: {
            return cb * cf;
        }
        // Screen
        case 3u: {
            return cb + cf - cb * cf;
        }
        // Overlay — multiply below the backdrop midpoint, screen above it.
        case 4u: {
            let one = vec3<f32>(1.0);
            let dark = 2.0 * cb * cf;
            let light = one - 2.0 * (one - cb) * (one - cf);
            return select(light, dark, cb <= vec3<f32>(0.5));
        }
        default: {
            return cf;
        }
    }
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    // Bounds come from the uniform, not `textureDimensions`: neither input
    // need be the size of the output canvas.
    if (gid.x >= params.out_width || gid.y >= params.out_height) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));

    let b = pixel_at(bg_tex, gid.x, gid.y, params.bg_width, params.bg_height);
    let f = pixel_at(fg_tex, gid.x, gid.y, params.fg_width, params.fg_height);

    let ab = b.a;
    let af = f.a;
    let ao = af + ab * (1.0 - af);
    if (ao <= 0.0) {
        textureStore(output_tex, coord, vec4<f32>(0.0));
        return;
    }

    let blended = blend_rgb(params.mode, b.rgb, f.rgb);
    let mixed = (1.0 - ab) * f.rgb + ab * blended;
    let rgb = (af * mixed + (1.0 - af) * ab * b.rgb) / ao;

    textureStore(output_tex, coord, vec4<f32>(rgb, ao));
}
