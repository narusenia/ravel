// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

struct RasterParams {
    resolution: vec2<f32>,
    _pad: vec2<f32>,
}

struct DrawItem {
    bounds: vec4<f32>,
    color: vec4<f32>,
    // kind, path vertex start / point center x, path vertex count / point
    // center y, closed flag / point radius
    data0: vec4<f32>,
    // fill flag, stroke width, unused, unused
    data1: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: RasterParams;
@group(0) @binding(1) var<storage, read> path_vertices: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read> draw_items: array<DrawItem>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) item_index: u32,
}

@vertex
fn raster_vertex(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let item = draw_items[instance_index];
    let pixel = mix(item.bounds.xy, item.bounds.zw, corners[vertex_index]);
    let ndc = vec2<f32>(
        pixel.x * 2.0 / params.resolution.x - 1.0,
        1.0 - pixel.y * 2.0 / params.resolution.y,
    );
    var output: VertexOutput;
    output.position = vec4<f32>(ndc, 0.0, 1.0);
    output.item_index = instance_index;
    return output;
}

fn segment_distance(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let denom = dot(ab, ab);
    if denom <= 1e-10 {
        return distance(p, a);
    }
    let t = clamp(dot(p - a, ab) / denom, 0.0, 1.0);
    return distance(p, a + t * ab);
}

fn path_coverage(item: DrawItem, p: vec2<f32>) -> f32 {
    let start = u32(item.data0.y);
    let count = u32(item.data0.z);
    let closed = item.data0.w > 0.5;
    let fill = item.data1.x > 0.5 && closed;
    let stroke_width = item.data1.y;
    let segment_count = select(count - 1u, count, closed);

    var winding = 0i;
    var min_distance = 1e20;
    for (var i = 0u; i < segment_count; i += 1u) {
        let next = select(i + 1u, 0u, i + 1u == count);
        let a = path_vertices[start + i];
        let b = path_vertices[start + next];
        min_distance = min(min_distance, segment_distance(p, a, b));

        let cross = (b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y);
        if a.y <= p.y && b.y > p.y && cross > 0.0 {
            winding += 1;
        } else if a.y > p.y && b.y <= p.y && cross < 0.0 {
            winding -= 1;
        }
    }

    var fill_coverage = 0.0;
    if fill {
        if winding != 0 {
            fill_coverage = clamp(min_distance + 0.5, 0.0, 1.0);
        } else {
            fill_coverage = clamp(0.5 - min_distance, 0.0, 1.0);
        }
    }
    var stroke_coverage = 0.0;
    if stroke_width > 0.0 {
        stroke_coverage = clamp(stroke_width * 0.5 - min_distance + 0.5, 0.0, 1.0);
    }
    // CPU draws the stroke over the fill with the same color.
    return fill_coverage + stroke_coverage * (1.0 - fill_coverage);
}

@fragment
fn raster_fragment(
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) item_index: u32,
) -> @location(0) vec4<f32> {
    let item = draw_items[item_index];
    var coverage = 0.0;
    if item.data0.x < 0.5 {
        let center = item.data0.yz;
        let radius = item.data0.w;
        coverage = clamp(radius - distance(position.xy, center) + 0.5, 0.0, 1.0);
    } else {
        coverage = path_coverage(item, position.xy);
    }
    let alpha = item.color.a * coverage;
    return vec4<f32>(item.color.rgb * alpha, alpha);
}

@group(0) @binding(3) var premul_input: texture_2d<f32>;
@group(0) @binding(4) var straight_output: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(8, 8, 1)
fn unpremultiply(@builtin(global_invocation_id) gid: vec3<u32>) {
    let size = textureDimensions(straight_output);
    if gid.x >= size.x || gid.y >= size.y {
        return;
    }
    let coord = vec2<i32>(gid.xy);
    let value = textureLoad(premul_input, coord, 0);
    var straight = vec4<f32>(0.0);
    if value.a > 1e-7 {
        straight = vec4<f32>(value.rgb / value.a, value.a);
    }
    textureStore(straight_output, coord, straight);
}
