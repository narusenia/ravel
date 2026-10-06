// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// `comp.key` — chroma / luminance key. Per pixel, on the straight RGB as
// stored; the RGB is never changed, only the alpha:
//
//   alpha' = alpha * f          f = clamp((d - tolerance) / softness, 0, 1)
//                               (a step at `tolerance` when softness is 0)
//   alpha' = alpha * (1 - f)    when `invert` is set (keep only the keyed range)
//
// `d` is the distance to the key colour, so a pixel within `tolerance` of it is
// removed (f = 0) and one beyond tolerance + softness is kept (f = 1):
//   chroma   d = |(Cb, Cr) - (Cb, Cr) of the key|, Rec.709 CbCr of the colour
//            normalised by its largest channel (Cb = (B - Y) / 1.8556,
//            Cr = (R - Y) / 1.5748, both within +-0.5), so the brightness of
//            the backdrop does not move the key: a dim green screen keys like
//            a bright one. Black (largest channel ~0) has no chroma.
//   luma     d = |Y - Y of the key|, Rec.709 luminance
//
// params.mode.x: 0 chroma, 1 luma; params.mode.y: invert (0 / 1).
// params.key.rgb: the key colour; params.shape.x: tolerance, .y: softness.

struct Params {
    mode:  vec4<u32>,
    key:   vec4<f32>,
    shape: vec4<f32>,
}

@group(0) @binding(0) var input_tex:  texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var<uniform> params: Params;

fn luma(rgb: vec3<f32>) -> f32 {
    return dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn chroma(color: vec3<f32>) -> vec2<f32> {
    let m = max(color.r, max(color.g, color.b));
    let rgb = select(vec3<f32>(0.0), color / m, m > 1e-4);
    let y = luma(rgb);
    return vec2<f32>((rgb.b - y) / 1.8556, (rgb.r - y) / 1.5748);
}

fn distance_to_key(rgb: vec3<f32>) -> f32 {
    if (params.mode.x == 0u) {
        return length(chroma(rgb) - chroma(params.key.rgb));
    }
    return abs(luma(rgb) - luma(params.key.rgb));
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(input_tex, coord, 0);
    let d = distance_to_key(c.rgb);
    var f = select(0.0, 1.0, d > params.shape.x);
    if (params.shape.y > 0.0) {
        f = clamp((d - params.shape.x) / params.shape.y, 0.0, 1.0);
    }
    if (params.mode.y != 0u) {
        f = 1.0 - f;
    }
    textureStore(output_tex, coord, vec4<f32>(c.rgb, c.a * f));
}
