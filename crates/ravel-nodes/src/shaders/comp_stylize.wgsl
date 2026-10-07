// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// The stylize image nodes of `comp/stylize.rs`. `params.mode.x` selects:
//   0 glow         a.x = intensity; colour.rgb = tint; the blurred copy is
//                  `aux_tex`
//   1 drop shadow  a.xy = offset px, a.z = opacity; colour.rgb = shadow colour;
//                  the blurred copy is `aux_tex`
//   2 stroke       a.x = width px, mode.y = search radius (taps), mode.z = 1
//                  for an inside stroke; colour = stroke colour
//   3 emboss       a.xy = light step in px (unit direction x distance),
//                  a.z = amount
//
// Everything that reads a neighbour does so in premultiplied alpha
// (GPUCOMP-4, `premultiplied.wgsl`) and the result goes back to straight alpha.
// **Border definition**: a tap outside the image is clamped to the nearest edge
// texel (as in `comp_distort.wgsl`); only the drop shadow, which is *meant* to
// leave the frame, reads outside as transparent.
//
// Prepend `premultiplied.wgsl` (see `gpu_util::with_premultiplied_helpers`).

struct Params {
    mode:  vec4<u32>,
    a:     vec4<f32>,
    colour: vec4<f32>,
}

@group(0) @binding(0) var input_tex:  texture_2d<f32>;
@group(0) @binding(1) var aux_tex:    texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> params: Params;

fn luma(rgb: vec3<f32>) -> f32 {
    return dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
}

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

/// The relief height of a pixel: luminance plus coverage, so a black shape on
/// transparency has an edge as well as a bright one.
fn height(pos: vec2<f32>) -> f32 {
    let t = tap(input_tex, pos.x, pos.y);
    return luma(t.rgb) + t.a;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let p = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5);
    let c = textureLoad(input_tex, coord, 0);
    let pc = premultiply(c);
    var out = c;

    switch (params.mode.x) {
        case 0u: {
            // Additive: the blurred copy, tinted and scaled, is added to the
            // image. Alpha grows by what the glow covers beyond the image.
            let b = premultiply(textureLoad(aux_tex, coord, 0));
            let g = b * params.a.x * vec4<f32>(params.colour.rgb, 1.0);
            if (params.a.x > 0.0) {
                let alpha = min(pc.a + g.a * (1.0 - pc.a), 1.0);
                out = un_premultiply(vec4<f32>(pc.rgb + g.rgb, alpha));
            }
        }
        case 1u: {
            // The blurred alpha, moved by the offset (outside the frame it is
            // transparent), is the shadow's coverage; it goes behind the image.
            let dims_f = vec2<f32>(f32(dims.x), f32(dims.y));
            let s = sample_premultiplied_bilinear(
                aux_tex, p.x - params.a.x, p.y - params.a.y, dims_f);
            let k = s.a * params.a.z * (1.0 - pc.a);
            if (k > 0.0) {
                out = un_premultiply(vec4<f32>(pc.rgb + params.colour.rgb * k, pc.a + k));
            }
        }
        case 2u: {
            // Distance-weighted dilation (outside) or erosion (inside) of the
            // coverage over a disc of `width`; the weight is 1 up to half a
            // pixel inside the radius and falls to 0 over one pixel, which
            // anti-aliases the outer edge. A width of 0 changes nothing: the
            // weights are at most 0.5 and the ring below is clamped at 0.
            let width = params.a.x;
            let reach = i32(params.mode.y);
            let inside = params.mode.z == 1u;
            var e = select(0.0, 1.0, inside);
            for (var dy = -reach; dy <= reach; dy = dy + 1) {
                for (var dx = -reach; dx <= reach; dx = dx + 1) {
                    let w = clamp(width + 0.5 - length(vec2<f32>(f32(dx), f32(dy))), 0.0, 1.0);
                    if (w <= 0.0) {
                        continue;
                    }
                    let a = clamped_texel(input_tex, p.x + f32(dx), p.y + f32(dy)).a;
                    if (inside) {
                        e = min(e, 1.0 - (1.0 - a) * w);
                    } else {
                        e = max(e, a * w);
                    }
                }
            }
            if (inside) {
                // The ring is the coverage the erosion took away; the stroke
                // colour replaces the image there, alpha unchanged.
                let ring = max(pc.a - e, 0.0);
                if (pc.a > 0.0) {
                    let k = clamp(ring * params.colour.a / pc.a, 0.0, 1.0);
                    out = vec4<f32>(mix(c.rgb, params.colour.rgb, k), c.a);
                }
            } else {
                // The ring is the coverage the dilation added beyond the image,
                // drawn behind it; the output alpha grows by exactly that.
                // (Untouched pixels keep their stored value bit for bit.)
                let k = max(e - pc.a, 0.0) * params.colour.a;
                if (k > 0.0) {
                    out = un_premultiply(vec4<f32>(pc.rgb + params.colour.rgb * k, pc.a + k));
                }
            }
        }
        default: {
            // Slope along the light: a surface whose height falls toward the
            // light faces it and is brightened. A pixel with no coverage keeps
            // what it stores.
            let relief = height(p - params.a.xy) - height(p + params.a.xy);
            if (c.a > 0.0) {
                out = vec4<f32>(c.rgb + params.a.z * relief, c.a);
            }
        }
    }
    textureStore(output_tex, coord, out);
}
