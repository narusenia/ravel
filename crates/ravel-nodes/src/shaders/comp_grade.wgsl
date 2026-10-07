// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT
//
// The colour-adjustment nodes (`comp.brightness_contrast`, ...). One shader,
// one uniform: `params.mode.x` picks the operation and `a`/`b`/`c` carry its
// numbers (what each holds is documented beside `GradeKind::fill` in
// `comp/grade.rs`).
//
// Straight alpha (`premultiplied.wgsl` explains the convention): every
// operation here is per-pixel and mixes no neighbours, so it works on the
// straight RGB as stored and never touches alpha. Luminance weights are
// Rec.709, the same as `color_correct.wgsl`.

struct Params {
    mode: vec4<u32>,
    a: vec4<f32>,
    b: vec4<f32>,
    c: vec4<f32>,
    // Baked 1D tables, `TABLE_LEN` entries (`comp/grade.rs`); what the four
    // lanes of an entry hold depends on the mode.
    table: array<vec4<f32>, 256>,
}

@group(0) @binding(0) var input_tex:  texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var<uniform> params: Params;

// Prepend `grade_stages.wgsl` (`gpu_util::with_grade_stages`): the stage
// functions live there and are shared with `color_correct.wgsl`.

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(input_tex);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }

    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let src = textureLoad(input_tex, coord, 0);
    var rgb = src.rgb;
    switch params.mode.x {
        case 0u: { rgb = brightness_contrast(rgb, params.a); }
        case 1u: { rgb = hue_saturation(rgb, params.a, params.b); }
        case 2u: { rgb = levels(rgb, params.a, params.b); }
        case 3u: { rgb = curves(rgb, 0u); }
        case 4u: { rgb = lift_gamma_gain(rgb, params.a, params.b, params.c); }
        case 5u: { rgb = hsl_curves(rgb, 0u); }
        default: {}
    }
    textureStore(output_tex, coord, vec4<f32>(rgb, src.a));
}
