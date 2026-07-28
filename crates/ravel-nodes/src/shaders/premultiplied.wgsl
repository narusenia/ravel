// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// Shared alpha-convention helpers, prepended to the shaders that filter
// (`crate::gpu_util::with_premultiplied_helpers`). WGSL has no include, so the
// snippet is concatenated in Rust rather than duplicated per shader.
//
// Frame buffers carry **straight alpha** (`rasterize/mod.rs`, `merge.wgsl`).
// Any filter that mixes neighbouring texels — bilinear interpolation, a blur
// convolution — must weight the texels in premultiplied alpha and convert the
// result back, or a transparent texel's RGB (usually 0) darkens its opaque
// neighbours and leaves a halo along alpha boundaries (issue MED-GPU-02).
//
// `sample_premultiplied_bilinear` mirrors the CPU reference in
// `comp/transform.rs` (`sample_bilinear` / `premultiplied_at`) operation for
// operation, including the taps outside the source reading as transparent
// rather than being clamped to the edge.

fn premultiply(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * c.a, c.a);
}

/// Back to straight alpha. A fully transparent result carries no colour.
fn un_premultiply(c: vec4<f32>) -> vec4<f32> {
    if (c.a > 0.0) {
        return vec4<f32>(c.rgb / c.a, c.a);
    }
    return vec4<f32>(0.0);
}

/// One texel in premultiplied alpha; outside `dims` the source is transparent.
///
/// Clamping to the edge instead would extend the border colour outwards, so a
/// scaled-up layer would grow an opaque fringe the CPU path does not have.
fn premultiplied_texel(tex: texture_2d<f32>, x: f32, y: f32, dims: vec2<f32>) -> vec4<f32> {
    if (x < 0.0 || y < 0.0 || x >= dims.x || y >= dims.y) {
        return vec4<f32>(0.0);
    }
    return premultiply(textureLoad(tex, vec2<i32>(i32(x), i32(y)), 0));
}

/// Bilinear sample at pixel-space `(sx, sy)`, interpolated in premultiplied
/// alpha and returned in straight alpha.
fn sample_premultiplied_bilinear(
    tex: texture_2d<f32>,
    sx: f32,
    sy: f32,
    dims: vec2<f32>,
) -> vec4<f32> {
    let fx = sx - 0.5;
    let fy = sy - 0.5;
    let x0 = floor(fx);
    let y0 = floor(fy);
    let tx = fx - x0;
    let ty = fy - y0;

    var acc = vec4<f32>(0.0);
    acc = acc + (1.0 - tx) * (1.0 - ty) * premultiplied_texel(tex, x0, y0, dims);
    acc = acc + tx * (1.0 - ty) * premultiplied_texel(tex, x0 + 1.0, y0, dims);
    acc = acc + (1.0 - tx) * ty * premultiplied_texel(tex, x0, y0 + 1.0, dims);
    acc = acc + tx * ty * premultiplied_texel(tex, x0 + 1.0, y0 + 1.0, dims);
    return un_premultiply(acc);
}
