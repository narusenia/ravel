// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// The neighbourhood image nodes of `comp/distort.rs`. `params.mode.x` selects:
//   0 directional blur   a.xy = full blur vector in device px
//   1 radial blur, spin  a = (centre px, total angle rad)
//   2 radial blur, zoom  a = (centre px, zoom fraction)
//   3 sharpen            a.x = amount; the blurred copy is `aux_tex`
//   4 warp               a.xy = displacement at full map value, device px;
//                        the displacement map is `aux_tex`
//   5 lens distortion    a = (centre px, k, 1 / half-diagonal px)
//   6 ripple             a = (centre px, amplitude px, wavelength px),
//                        b.x = phase in cycles
// `params.mode.y` is the tap count of the two blurs.
//
// Every tap is read in premultiplied alpha and the sum is converted back
// (GPUCOMP-4, `premultiplied.wgsl`). **Border definition**: a tap outside the
// image is clamped to the nearest edge texel (edge replicate), so a uniform
// opaque image stays uniform up to its border and nothing darkens or fades at
// the edge.
//
// Prepend `premultiplied.wgsl` (see `gpu_util::with_premultiplied_helpers`).

struct Params {
    mode: vec4<u32>,
    a:    vec4<f32>,
    b:    vec4<f32>,
}

@group(0) @binding(0) var input_tex:  texture_2d<f32>;
@group(0) @binding(1) var aux_tex:    texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> params: Params;

const TAU: f32 = 6.283185307179586;

fn clamped_texel(tex: texture_2d<f32>, x: f32, y: f32) -> vec4<f32> {
    let d = vec2<i32>(textureDimensions(tex));
    let p = vec2<i32>(clamp(i32(x), 0, d.x - 1), clamp(i32(y), 0, d.y - 1));
    return premultiply(textureLoad(tex, p, 0));
}

/// Premultiplied bilinear tap at pixel-space `(sx, sy)` with edge clamping.
fn tap(tex: texture_2d<f32>, sx: f32, sy: f32) -> vec4<f32> {
    let fx = sx - 0.5;
    let fy = sy - 0.5;
    let x0 = floor(fx);
    let y0 = floor(fy);
    let tx = fx - x0;
    let ty = fy - y0;
    var acc = vec4<f32>(0.0);
    acc = acc + (1.0 - tx) * (1.0 - ty) * clamped_texel(tex, x0, y0);
    acc = acc + tx * (1.0 - ty) * clamped_texel(tex, x0 + 1.0, y0);
    acc = acc + (1.0 - tx) * ty * clamped_texel(tex, x0, y0 + 1.0);
    acc = acc + tx * ty * clamped_texel(tex, x0 + 1.0, y0 + 1.0);
    return acc;
}

fn tap_at(pos: vec2<f32>) -> vec4<f32> {
    return tap(input_tex, pos.x, pos.y);
}

/// Position of tap `i` of `n` along [0, 1]; a single tap sits at 0.
fn tap_t(i: u32, n: u32) -> f32 {
    if (n < 2u) {
        return 0.0;
    }
    return f32(i) / f32(n - 1u);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let p = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5);
    let n = max(params.mode.y, 1u);
    var acc = vec4<f32>(0.0);
    var out = vec4<f32>(0.0);

    switch (params.mode.x) {
        case 0u: {
            // Centred on the pixel: taps span -1/2 .. +1/2 of the vector.
            for (var i = 0u; i < n; i = i + 1u) {
                let t = select(0.0, tap_t(i, n) - 0.5, n > 1u);
                acc = acc + tap_at(p + params.a.xy * t);
            }
            out = un_premultiply(acc / f32(n));
        }
        case 1u: {
            let c = params.a.xy;
            let d = p - c;
            for (var i = 0u; i < n; i = i + 1u) {
                let t = select(0.0, tap_t(i, n) - 0.5, n > 1u);
                let theta = params.a.z * t;
                let cs = cos(theta);
                let sn = sin(theta);
                acc = acc + tap_at(c + vec2<f32>(d.x * cs - d.y * sn, d.x * sn + d.y * cs));
            }
            out = un_premultiply(acc / f32(n));
        }
        case 2u: {
            let c = params.a.xy;
            for (var i = 0u; i < n; i = i + 1u) {
                acc = acc + tap_at(c + (p - c) * (1.0 - params.a.z * tap_t(i, n)));
            }
            out = un_premultiply(acc / f32(n));
        }
        case 3u: {
            let c = textureLoad(input_tex, coord, 0);
            let pc = premultiply(c);
            let pb = premultiply(textureLoad(aux_tex, coord, 0));
            let sharp = pc.rgb + params.a.x * (pc.rgb - pb.rgb);
            // Alpha is the input's; a transparent pixel keeps its stored RGB.
            if (c.a > 0.0) {
                out = vec4<f32>(sharp / c.a, c.a);
            } else {
                out = c;
            }
        }
        case 4u: {
            // The map is stretched over the image (UV of the pixel centre) and
            // read over mid-gray, so a transparent map texel displaces nothing.
            let md = vec2<f32>(textureDimensions(aux_tex));
            let uv = p / vec2<f32>(f32(dims.x), f32(dims.y)) * md;
            let m = tap(aux_tex, uv.x, uv.y);
            let eff = m.rgb + vec3<f32>(0.5) * (1.0 - m.a);
            let d = (eff.rg - vec2<f32>(0.5)) * params.a.xy;
            out = un_premultiply(tap_at(p + d));
        }
        case 5u: {
            let c = params.a.xy;
            let d = p - c;
            let r = length(d) * params.a.w;
            out = un_premultiply(tap_at(c + d * (1.0 + params.a.z * r * r)));
        }
        default: {
            let c = params.a.xy;
            let d = p - c;
            let r = length(d);
            var dir = vec2<f32>(0.0);
            if (r > 0.0) {
                dir = d / r;
            }
            let shift = params.a.z * sin(TAU * (r / params.a.w - params.b.x));
            out = un_premultiply(tap_at(p + dir * shift));
        }
    }
    textureStore(output_tex, coord, out);
}
