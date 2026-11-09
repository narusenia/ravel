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
    // x: the mesh has vertex colours, y: sample the bound texture,
    // z: lights shade it (it has normals).
    flags: vec4<f32>,
}

// One light in scene space. `vector.w` is the kind: 0 ambient, 1 directional
// (`vector.xyz` is the unit vector toward the light), 2 point (`vector.xyz` is
// its position). `color.rgb` is colour times intensity, `color.w` a point
// light's half-strength radius. All-zero padding is a dark ambient light.
struct LightData {
    vector: vec4<f32>,
    color: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> objects: array<ObjectData>;
@group(0) @binding(1) var<storage, read> lights: array<LightData>;
@group(1) @binding(0) var<storage, read> vertex_colors: array<vec4<f32>>;
@group(2) @binding(0) var albedo: texture_2d<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) normal: vec3<f32>,
    @location(3) @interpolate(flat) object: u32,
    @location(4) world: vec3<f32>,
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
    // The inverse transpose of the model's upper 3x3, so a non-uniform scale
    // keeps the normal perpendicular: for columns a, b, c it is
    // [b x c, c x a, a x b] / det, and `det` is only needed for its sign (a
    // mirroring model flips it). Normalised per fragment, after interpolation.
    let a = record.model[0].xyz;
    let b = record.model[1].xyz;
    let c = record.model[2].xyz;
    let handedness = select(-1.0, 1.0, dot(a, cross(b, c)) >= 0.0);
    output.normal = (cross(b, c) * normal.x + cross(c, a) * normal.y + cross(a, b) * normal.z)
        * handedness;
    output.world = (record.model * vec4<f32>(position, 1.0)).xyz;
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

/// What the lights deliver to a surface point, per channel: Lambert, summed
/// over every light. See `ravel_core::scene::light` for the model. The CPU
/// reference is `light_factor` in `scene_render/cpu.rs`.
fn light_factor(world: vec3<f32>, interpolated_normal: vec3<f32>) -> vec3<f32> {
    let length_n = length(interpolated_normal);
    var n = vec3<f32>(0.0);
    if length_n > 1e-12 {
        n = interpolated_normal / length_n;
    }
    var total = vec3<f32>(0.0);
    for (var i = 0u; i < arrayLength(&lights); i += 1u) {
        let light = lights[i];
        if light.vector.w < 0.5 {
            total += light.color.rgb;
        } else if light.vector.w < 1.5 {
            total += light.color.rgb * max(dot(n, light.vector.xyz), 0.0);
        } else {
            let to_light = light.vector.xyz - world;
            let distance = length(to_light);
            if distance > 1e-6 {
                let ratio = distance / light.color.w;
                let falloff = 1.0 / (1.0 + ratio * ratio);
                total += light.color.rgb * falloff * max(dot(n, to_light / distance), 0.0);
            }
        }
    }
    return total;
}

@fragment
fn mesh_fragment(input: VertexOutput) -> @location(0) vec4<f32> {
    let record = objects[input.object];
    var tint = input.color * record.color;
    if record.flags.z > 0.5 {
        tint = vec4<f32>(tint.rgb * light_factor(input.world, input.normal), tint.a);
    }
    if record.flags.y > 0.5 {
        let dimensions = textureDimensions(albedo);
        let sampled = sample_albedo(input.uv * vec2<f32>(dimensions), vec2<i32>(dimensions));
        // The tint multiplies the straight-alpha texel; premultiplied that is
        // the accumulator scaled by the tint's colour and its alpha.
        return vec4<f32>(sampled.rgb * tint.rgb * tint.a, sampled.a * tint.a);
    }
    return vec4<f32>(tint.rgb * tint.a, tint.a);
}
