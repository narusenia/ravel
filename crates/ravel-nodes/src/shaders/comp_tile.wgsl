// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// `comp.mirror` and `comp.tile`. `params.mode.x` selects:
//   0 flip horizontally    out(x, y) = in(w - 1 - x, y)
//   1 flip vertically      out(x, y) = in(x, h - 1 - y)
//   2 flip both
//   3 tile                 `params.mode.yz` = columns, rows
//
// The flips are texel copies (straight alpha and RGB bit for bit), so a flip
// applied twice is the input. **Tile** shrinks the image to 1/columns by
// 1/rows and repeats it: the pixel centre (x + 0.5, y + 0.5) reads the source
// at ((x + 0.5) * columns, (y + 0.5) * rows). A tap past the source wraps
// around (repeat), so copies meet without a seam and 1 x 1 reads every texel
// at its own centre, which is the input. The read is premultiplied bilinear
// (GPUCOMP-4). ponytail: bilinear is 2 x 2 taps, so a large shrink aliases;
// a box filter over columns x rows taps is the upgrade if that shows.
//
// Prepend `premultiplied.wgsl` (see `gpu_util::with_premultiplied_helpers`).

struct Params {
    mode: vec4<u32>,
}

@group(0) @binding(0) var input_tex:  texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var<uniform> params: Params;

fn wrapped_texel(x: f32, y: f32, dims: vec2<i32>) -> vec4<f32> {
    let p = vec2<i32>(
        ((i32(x) % dims.x) + dims.x) % dims.x,
        ((i32(y) % dims.y) + dims.y) % dims.y,
    );
    return premultiply(textureLoad(input_tex, p, 0));
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let d = vec2<i32>(dims);
    var out = vec4<f32>(0.0);
    switch (params.mode.x) {
        case 0u: {
            out = textureLoad(input_tex, vec2<i32>(d.x - 1 - coord.x, coord.y), 0);
        }
        case 1u: {
            out = textureLoad(input_tex, vec2<i32>(coord.x, d.y - 1 - coord.y), 0);
        }
        case 2u: {
            out = textureLoad(input_tex, vec2<i32>(d.x - 1 - coord.x, d.y - 1 - coord.y), 0);
        }
        default: {
            let sx = (f32(gid.x) + 0.5) * f32(params.mode.y) - 0.5;
            let sy = (f32(gid.y) + 0.5) * f32(params.mode.z) - 0.5;
            let x0 = floor(sx);
            let y0 = floor(sy);
            let tx = sx - x0;
            let ty = sy - y0;
            var acc = vec4<f32>(0.0);
            acc = acc + (1.0 - tx) * (1.0 - ty) * wrapped_texel(x0, y0, d);
            acc = acc + tx * (1.0 - ty) * wrapped_texel(x0 + 1.0, y0, d);
            acc = acc + (1.0 - tx) * ty * wrapped_texel(x0, y0 + 1.0, d);
            acc = acc + tx * ty * wrapped_texel(x0 + 1.0, y0 + 1.0, d);
            out = un_premultiply(acc);
        }
    }
    textureStore(output_tex, coord, out);
}
