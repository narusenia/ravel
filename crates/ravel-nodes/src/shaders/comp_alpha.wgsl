// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// `comp.alpha` — alpha operations. `mode` selects:
//   0 invert         a' = 1 - a, RGB kept
//   1 luma_to_alpha  a' = luma(rgb) * a, RGB kept
//   2 alpha_to_luma  rgb' = a (gray), a' = 1
//   3 matte_alpha    a' = a * matte coverage (the matte's alpha)
//   4 matte_luma     a' = a * matte coverage (the matte's luminance)
//
// The matte is resampled onto the main image's pixel grid: the centre of each
// main pixel maps to the same UV in the matte, which is read with bilinear
// filtering in premultiplied alpha and edge clamping (a matte of another size
// stretches to cover the frame; it never leaves a transparent border). Luma
// is taken from the premultiplied colour, so a transparent matte pixel has
// luma 0 whatever RGB it stores. When the matte has the main image's size the
// sample lands on a texel centre and is exact. When the processor has no matte
// it never reaches this shader.
//
// Prepend `premultiplied.wgsl` (see `gpu_util::with_premultiplied_helpers`).
// Luminance weights are Rec.709, as in `color_correct.wgsl`.

struct Params {
    mode:  u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var input_tex:  texture_2d<f32>;
@group(0) @binding(1) var matte_tex:  texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> params: Params;

fn luma(rgb: vec3<f32>) -> f32 {
    return dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn matte_texel(x: f32, y: f32, dims: vec2<i32>) -> vec4<f32> {
    let p = vec2<i32>(clamp(i32(x), 0, dims.x - 1), clamp(i32(y), 0, dims.y - 1));
    return premultiply(textureLoad(matte_tex, p, 0));
}

/// Premultiplied bilinear sample of the matte at the main pixel `(x, y)`.
fn sample_matte(x: u32, y: u32, main_dims: vec2<u32>) -> vec4<f32> {
    let md = vec2<i32>(textureDimensions(matte_tex));
    let sx = (f32(x) + 0.5) / f32(main_dims.x) * f32(md.x);
    let sy = (f32(y) + 0.5) / f32(main_dims.y) * f32(md.y);
    let fx = sx - 0.5;
    let fy = sy - 0.5;
    let x0 = floor(fx);
    let y0 = floor(fy);
    let tx = fx - x0;
    let ty = fy - y0;
    var acc = vec4<f32>(0.0);
    acc = acc + (1.0 - tx) * (1.0 - ty) * matte_texel(x0, y0, md);
    acc = acc + tx * (1.0 - ty) * matte_texel(x0 + 1.0, y0, md);
    acc = acc + (1.0 - tx) * ty * matte_texel(x0, y0 + 1.0, md);
    acc = acc + tx * ty * matte_texel(x0 + 1.0, y0 + 1.0, md);
    return acc;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }

    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(input_tex, coord, 0);
    var out = c;
    switch (params.mode) {
        case 0u: {
            out = vec4<f32>(c.rgb, 1.0 - c.a);
        }
        case 1u: {
            out = vec4<f32>(c.rgb, luma(c.rgb) * c.a);
        }
        case 2u: {
            out = vec4<f32>(vec3<f32>(c.a), 1.0);
        }
        case 3u: {
            out = vec4<f32>(c.rgb, c.a * sample_matte(gid.x, gid.y, dims).a);
        }
        default: {
            out = vec4<f32>(c.rgb, c.a * luma(sample_matte(gid.x, gid.y, dims).rgb));
        }
    }
    textureStore(output_tex, coord, out);
}
