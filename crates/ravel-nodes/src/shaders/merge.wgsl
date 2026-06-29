// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// Alpha compositing blend modes: over, add, multiply.

struct Params {
    operation: u32,  // 0 = over, 1 = add, 2 = multiply
    mix_val:   f32,
    _pad0:     f32,
    _pad1:     f32,
}

@group(0) @binding(0) var tex_a:     texture_2d<f32>;
@group(0) @binding(1) var tex_b:     texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(3) var<uniform> params: Params;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(tex_a);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }

    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let a = textureLoad(tex_a, coord, 0);
    let b = textureLoad(tex_b, coord, 0);

    var result: vec4<f32>;

    switch (params.operation) {
        // over: A over B (Porter-Duff)
        case 0u: {
            let out_a = a.a + b.a * (1.0 - a.a);
            if (out_a > 0.0) {
                let rgb = (a.rgb * a.a + b.rgb * b.a * (1.0 - a.a)) / out_a;
                result = vec4<f32>(rgb, out_a);
            } else {
                result = vec4<f32>(0.0);
            }
        }
        // add
        case 1u: {
            result = vec4<f32>(a.rgb + b.rgb, clamp(a.a + b.a, 0.0, 1.0));
        }
        // multiply
        case 2u: {
            result = vec4<f32>(a.rgb * b.rgb, a.a * b.a);
        }
        default: {
            result = a;
        }
    }

    // Mix between B (original) and result.
    result = mix(b, result, params.mix_val);

    textureStore(output_tex, coord, result);
}
