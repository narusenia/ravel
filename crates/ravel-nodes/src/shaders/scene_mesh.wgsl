// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The triangle pipeline of `scene.render`. Bind contract: see
// `ravel_gpu::scene_pass`. The object record layout is `ObjectRecord` in
// `scene_render/gpu.rs`.

struct ObjectData {
    // Object space -> clip space, column-major.
    mvp: mat4x4<f32>,
    // Object space -> scene space; carries the normal to scene space.
    model: mat4x4<f32>,
    // Straight-alpha tint multiplied into every fragment.
    color: vec4<f32>,
    // x: the mesh has vertex colours, y: sample the bound texture.
    flags: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> objects: array<ObjectData>;
@group(1) @binding(0) var<storage, read> vertex_colors: array<vec4<f32>>;
@group(2) @binding(0) var albedo: texture_2d<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) normal: vec3<f32>,
    @location(3) @interpolate(flat) object: u32,
}

@vertex
fn mesh_vertex(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) object: u32,
) -> VertexOutput {
    let record = objects[object];
    var output: VertexOutput;
    output.position = record.mvp * vec4<f32>(position, 1.0);
    output.uv = uv;
    output.color = select(
        vec4<f32>(1.0),
        vertex_colors[vertex_index],
        record.flags.x > 0.5,
    );
    // Not normalised and not inverse-transposed: nothing shades with it yet,
    // and lighting owns the choice (a non-uniform scale needs the latter).
    output.normal = (record.model * vec4<f32>(normal, 0.0)).xyz;
    output.object = object;
    return output;
}

/// Bilinear sample of `albedo` at a source-pixel coordinate, `(0.5, 0.5)` the
/// centre of the first texel, returned premultiplied.
///
/// The CPU reference (`sample_bilinear` in `rasterize/mod.rs`) weights the four
/// texels premultiplied and clamps to the edge texel; this is the same
/// accumulator, so a transparent texel cannot bleed its colour.
fn sample_albedo(texel_coord: vec2<f32>, dimensions: vec2<i32>) -> vec4<f32> {
    let t = texel_coord - vec2<f32>(0.5);
    let base = floor(t);
    let fraction = t - base;
    let corner = vec2<i32>(base);
    var accumulated = vec4<f32>(0.0);
    for (var row = 0; row < 2; row += 1) {
        let weight_y = select(1.0 - fraction.y, fraction.y, row == 1);
        for (var column = 0; column < 2; column += 1) {
            let weight = select(1.0 - fraction.x, fraction.x, column == 1) * weight_y;
            if weight == 0.0 {
                continue;
            }
            let coord = clamp(
                corner + vec2<i32>(column, row),
                vec2<i32>(0),
                dimensions - vec2<i32>(1),
            );
            let texel = textureLoad(albedo, coord, 0);
            accumulated += vec4<f32>(texel.rgb * texel.a, texel.a) * weight;
        }
    }
    return accumulated;
}

@fragment
fn mesh_fragment(input: VertexOutput) -> @location(0) vec4<f32> {
    let record = objects[input.object];
    let tint = input.color * record.color;
    if record.flags.y > 0.5 {
        let dimensions = textureDimensions(albedo);
        let sampled = sample_albedo(input.uv * vec2<f32>(dimensions), vec2<i32>(dimensions));
        // The tint multiplies the straight-alpha texel; premultiplied that is
        // the accumulator scaled by the tint's colour and its alpha.
        return vec4<f32>(sampled.rgb * tint.rgb * tint.a, sampled.a * tint.a);
    }
    return vec4<f32>(tint.rgb * tint.a, tint.a);
}
