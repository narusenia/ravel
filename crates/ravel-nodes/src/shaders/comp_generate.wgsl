// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// The generator image nodes of `comp/generate.rs`: no input, one colour per
// pixel from the pixel's position alone. `params.mode.x` selects:
//   0 gradient     a = (start px, end px); mode.w = 1 radial; table = ramp
//   1 noise        a.x = feature size px, a.y = roughness, a.zw = offset px;
//                  mode.y = octaves, mode.z = seed; c = colour A, d = colour B
//   2 fractal      a = (centre re, im, view height in units); b.xy = Julia c;
//                  mode.y = iterations, mode.w = 1 Julia; table = ramp;
//                  c = colour of points that never escape
//   3 checkerboard a.xy = cell size px, b.xy = offset px; c / d = the colours
// Everything position-like arrives already in device pixels (composition value
// x scale) or as a frame fraction, so the picture does not depend on the
// resolution. The colours are written as given (straight alpha).
//
// Noise is this file's own: integer-hash gradient noise, nothing shared with
// the CPU `field.noise`.

struct Params {
    mode:  vec4<u32>,
    a:     vec4<f32>,
    b:     vec4<f32>,
    c:     vec4<f32>,
    d:     vec4<f32>,
    table: array<vec4<f32>, 256>,
}

@group(0) @binding(0) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(1) var<uniform> params: Params;

const TAU: f32 = 6.283185307179586;

/// The ramp baked on the CPU (`RampParam::evaluate` at i / 255), interpolated.
fn ramp_at(t: f32) -> vec4<f32> {
    let x = clamp(t, 0.0, 1.0) * 255.0;
    let i = u32(floor(x));
    let j = min(i + 1u, 255u);
    return mix(params.table[i], params.table[j], x - f32(i));
}

fn hash(ix: i32, iy: i32, seed: u32) -> u32 {
    var h = bitcast<u32>(ix) * 747796405u + bitcast<u32>(iy) * 2891336453u + seed * 277803737u;
    h = (h ^ (h >> 16u)) * 2246822519u;
    h = (h ^ (h >> 13u)) * 3266489917u;
    return h ^ (h >> 16u);
}

/// Unit gradient of the lattice point.
fn lattice_gradient(ix: i32, iy: i32, seed: u32) -> vec2<f32> {
    let angle = f32(hash(ix, iy, seed) >> 8u) * (TAU / 16777216.0);
    return vec2<f32>(cos(angle), sin(angle));
}

/// Gradient noise in about [-1, 1].
fn gradient_noise(p: vec2<f32>, seed: u32) -> f32 {
    let i = floor(p);
    let f = p - i;
    let ix = i32(i.x);
    let iy = i32(i.y);
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let n00 = dot(lattice_gradient(ix, iy, seed), f);
    let n10 = dot(lattice_gradient(ix + 1, iy, seed), f - vec2<f32>(1.0, 0.0));
    let n01 = dot(lattice_gradient(ix, iy + 1, seed), f - vec2<f32>(0.0, 1.0));
    let n11 = dot(lattice_gradient(ix + 1, iy + 1, seed), f - vec2<f32>(1.0, 1.0));
    return mix(mix(n00, n10, u.x), mix(n01, n11, u.x), u.y) * 1.4142135;
}

fn fractal_noise(p: vec2<f32>) -> f32 {
    var sum = 0.0;
    var norm = 0.0;
    var amp = 1.0;
    var freq = 1.0;
    for (var o = 0u; o < params.mode.y; o = o + 1u) {
        sum = sum + amp * gradient_noise(p * freq, params.mode.z + o * 101u);
        norm = norm + amp;
        amp = amp * params.a.y;
        freq = freq * 2.0;
    }
    return sum / max(norm, 1e-6);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let size = vec2<f32>(f32(dims.x), f32(dims.y));
    let p = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5);
    var out = vec4<f32>(0.0);

    switch (params.mode.x) {
        case 0u: {
            let s = params.a.xy;
            let e = params.a.zw;
            let v = e - s;
            let len2 = dot(v, v);
            var t = 0.0;
            if (len2 > 0.0) {
                if (params.mode.w == 1u) {
                    t = length(p - s) / sqrt(len2);
                } else {
                    t = dot(p - s, v) / len2;
                }
            }
            out = ramp_at(t);
        }
        case 1u: {
            let q = (p - params.a.zw) / max(params.a.x, 1e-3);
            let v = clamp(0.5 + 0.5 * fractal_noise(q), 0.0, 1.0);
            out = mix(params.c, params.d, v);
        }
        case 2u: {
            let q = (p - 0.5 * size) / size.y * params.a.z;
            var z = vec2<f32>(0.0);
            var k = vec2<f32>(0.0);
            if (params.mode.w == 1u) {
                z = vec2<f32>(params.a.x, params.a.y) + q;
                k = params.b.xy;
            } else {
                k = vec2<f32>(params.a.x, params.a.y) + q;
            }
            var n = 0u;
            var escaped = false;
            for (; n < params.mode.y; n = n + 1u) {
                z = vec2<f32>(z.x * z.x - z.y * z.y, 2.0 * z.x * z.y) + k;
                if (dot(z, z) > 256.0) {
                    escaped = true;
                    break;
                }
            }
            if (escaped) {
                // Smooth escape time (no banding), as a fraction of the iteration cap;
                // the square root spreads the cheap outer escapes over the ramp.
                let mu = f32(n) + 1.0 - log2(max(log2(dot(z, z)) * 0.5, 1e-6));
                out = ramp_at(sqrt(clamp(mu / f32(params.mode.y), 0.0, 1.0)));
            } else {
                out = params.c;
            }
        }
        default: {
            let cell = floor((p - params.b.xy) / max(params.a.xy, vec2<f32>(1e-3)));
            let odd = (i32(cell.x) + i32(cell.y)) & 1;
            out = select(params.c, params.d, odd != 0);
        }
    }
    textureStore(output_tex, vec2<i32>(i32(gid.x), i32(gid.y)), out);
}
