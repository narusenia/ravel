// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// The viewer's display transform (CM-7): linear working-space RGBA f32 in,
// display-encoded BGRA8 out, in one pass before the readback.
//
// Two things are deliberate here.
//
// * **BGRA, not RGBA.** The bytes are read back straight into the image the
//   UI toolkit draws, and that image is BGRA. Swizzling costs nothing here and
//   a full pass over the frame on the CPU.
// * **The code is computed, then written as `f32(code) / 255.0`.** Letting the
//   `rgba8unorm` store do the float -> byte conversion would put the rounding
//   rule in the driver's hands; `k / 255.0` round-trips through the store
//   exactly, so the byte is the one this shader decided on.
//
// The CPU definition is `ravel_core::color::to_display_rgba8`. This shader
// evaluates the same transform in f32 where that one uses f64, so the two
// agree to within one 8-bit code rather than bit for bit — see
// `docs/specifications/color-management.md`.

struct Params {
    // Only `.xyz` is read; a `vec4` so the uniform layout needs no padding
    // rules spelled out.
    domain_min: vec4<f32>,
    domain_max: vec4<f32>,
    // Edge length of the 3D LUT, or 0 when there is none and the built-in
    // transfer function applies.
    lut_size: u32,
    // Texels per row of the LUT atlas (see `lut_texel`).
    lut_row: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var input_tex: texture_2d<f32>;
// The LUT atlas. When `lut_size` is 0 this slot is bound to `input_tex` — a
// binding cannot be left empty and nothing reads it.
@group(0) @binding(1) var lut_tex: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(3) var<uniform> params: Params;

const SRGB_PHI: f32 = 12.92;
const SRGB_LINEAR_BREAK: f32 = 0.0031308;
const SRGB_ALPHA: f32 = 0.055;
const SRGB_GAMMA: f32 = 2.4;

// Smallest span the LUT domain is taken to have; matches `f32::EPSILON` in
// `CubeLut::sample`, which the same guard protects from dividing by zero.
const F32_EPSILON: f32 = 1.1920929e-7;

/// Linear light -> sRGB encoded, extended as an odd function so negative
/// values stay monotonic instead of turning into NaN under `pow`.
fn srgb_encode(v: f32) -> f32 {
    let a = abs(v);
    let encoded = select(
        (1.0 + SRGB_ALPHA) * pow(a, 1.0 / SRGB_GAMMA) - SRGB_ALPHA,
        SRGB_PHI * a,
        a <= SRGB_LINEAR_BREAK,
    );
    return select(encoded, -encoded, v < 0.0);
}

/// One encoded channel -> its display code, as the unorm value that stores it
/// back exactly. `quantize_u8`'s rule: clamp, scale, round half up.
fn display_code(v: f32) -> f32 {
    return floor(clamp(v, 0.0, 1.0) * 255.0 + 0.5) / 255.0;
}

/// One entry of the LUT atlas.
///
/// The table is a cube of `size^3` entries laid out linearly in the file's own
/// order (red fastest) and wrapped into `lut_row` texels per row, because a
/// `size = 256` table is 16.7M entries and no 2D texture is that wide.
fn lut_texel(index: u32) -> vec3<f32> {
    let coord = vec2<i32>(i32(index % params.lut_row), i32(index / params.lut_row));
    return textureLoad(lut_tex, coord, 0).rgb;
}

/// Trilinear interpolation over the table — the same arithmetic as
/// `CubeLut::sample`, including the clamp to the cube's own extent.
fn lut_sample(rgb: vec3<f32>) -> vec3<f32> {
    let size = params.lut_size;
    let last = f32(size - 1u);
    let domain_min = params.domain_min.xyz;
    let span = params.domain_max.xyz - domain_min;
    // A degenerate axis normalises to 0 rather than dividing by zero.
    let usable = abs(span) >= vec3<f32>(F32_EPSILON);
    let divisor = select(vec3<f32>(1.0), span, usable);
    let normalized = select(vec3<f32>(0.0), (rgb - domain_min) / divisor, usable);
    let coord = clamp(clamp(normalized, vec3<f32>(0.0), vec3<f32>(1.0)) * last, vec3<f32>(0.0), vec3<f32>(last));
    // `base + 1` must be a real grid point, so the cell index stops one short
    // of the edge and the fraction is measured from there.
    let base = min(vec3<u32>(coord), vec3<u32>(size - 2u));
    let frac = coord - vec3<f32>(base);

    var out = vec3<f32>(0.0);
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let step = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, (corner >> 2u) & 1u);
        let axis_weight = select(vec3<f32>(1.0) - frac, frac, step == vec3<u32>(1u));
        let weight = axis_weight.x * axis_weight.y * axis_weight.z;
        let grid = base + step;
        let index = grid.x + size * (grid.y + size * grid.z);
        out = out + weight * lut_texel(index);
    }
    return out;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let linear = textureLoad(input_tex, coord, 0);

    var encoded: vec3<f32>;
    if (params.lut_size >= 2u) {
        // A display LUT replaces the transfer function: it takes the linear
        // working value and yields the display-encoded one.
        encoded = lut_sample(linear.rgb);
    } else {
        encoded = vec3<f32>(
            srgb_encode(linear.r),
            srgb_encode(linear.g),
            srgb_encode(linear.b),
        );
    }

    // Alpha is coverage, not light: quantised, never encoded.
    textureStore(output_tex, coord, vec4<f32>(
        display_code(encoded.b),
        display_code(encoded.g),
        display_code(encoded.r),
        display_code(linear.a),
    ));
}
