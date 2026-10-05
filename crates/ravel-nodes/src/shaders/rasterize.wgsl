// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

struct RasterParams {
    resolution: vec2<f32>,
    _pad: vec2<f32>,
}

struct DrawItem {
    bounds: vec4<f32>,
    // fill color (`Cd`, else the node's color); the instance tint for an image
    color: vec4<f32>,
    // stroke color (`stroke_color`, else the fill color)
    stroke_color: vec4<f32>,
    // kind (0 sprite, 1 path, 2 image), then per kind:
    //   path:   vertex start, vertex count, closed flag — the count spans
    //           every contour of the item and the `CONTOUR_BREAK` sentinels
    //           between them
    //   sprite: center x, center y, radius
    //   image:  placement offset x, offset y, unused
    data0: vec4<f32>,
    // path:  fill flag, stroke width, unused, unused
    // image: unused, unused, rectangle half width, half height (the inverse
    //        of the placement's linear part rides in `stroke_color`)
    data1: vec4<f32>,
}

@group(0) @binding(0) var<uniform> params: RasterParams;
@group(0) @binding(1) var<storage, read> path_vertices: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read> draw_items: array<DrawItem>;
// The instance source the current run of image quads samples. Rebound between
// runs; a run of paths and sprites binds a placeholder it never reads.
@group(0) @binding(3) var image_source: texture_2d<f32>;

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

/// Distance to the segment `a -> b` in `x` and the clamped parameter of the
/// nearest point on it in `y`. A degenerate segment is the point `a` with
/// `t = 0` (`segment_nearest` in `sample.rs` is the CPU twin).
fn segment_nearest(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    let ab = b - a;
    let denom = dot(ab, ab);
    if denom <= 1e-10 {
        return vec2<f32>(distance(p, a), 0.0);
    }
    let t = clamp(dot(p - a, ab) / denom, 0.0, 1.0);
    return vec2<f32>(distance(p, a + t * ab), t);
}

/// Whether a vertex is the sentinel that separates two contours of one draw
/// item rather than a point on a path (`CONTOUR_BREAK`, `f32::MAX`). False for
/// a NaN, so a malformed position is not read as a break.
fn is_contour_break(v: vec2<f32>) -> bool {
    return v.x >= 3.0e38;
}

/// What one fragment knows about a draw item's polyline: the CPU's
/// `PathSample` (`sample.rs`), field for field. Segment indices are relative to
/// the item's first vertex.
struct PathSample {
    min_distance: f32,
    winding: i32,
    nearest_segment: u32,
    segment_end: u32,
    t_on_segment: f32,
}

/// Distance, winding, and where the nearest segment is, at `p`.
///
/// One item can hold several contours — a glyph's outer outline plus its
/// counters — laid out back to back in `path_vertices` and separated by
/// `CONTOUR_BREAK`. The winding number is accumulated across all of them,
/// which is what makes the counter of an `o` a hole rather than more ink, and
/// every contour closes onto **its own** first vertex, so no segment ever
/// bridges two of them. `segments` in `sample.rs` states the same rule for the
/// CPU.
fn path_sample(item: DrawItem, p: vec2<f32>) -> PathSample {
    let start = u32(item.data0.y);
    let count = u32(item.data0.z);
    let closed = item.data0.w > 0.5;

    var found = PathSample(1e20, 0i, 0u, 0u, 0.0);
    // Where the contour being walked began, relative to `start`.
    var contour_start = 0u;
    var i = 0u;
    loop {
        if i >= count {
            break;
        }
        let a = path_vertices[start + i];
        if is_contour_break(a) {
            i += 1u;
            contour_start = i;
            continue;
        }
        let seg_from = i;
        let next = i + 1u;
        // The segment leaving `a`: the next vertex, or — at the end of the
        // contour — back to its first vertex when the run is closed. An open
        // path has no such wrap, so its last vertex starts no segment.
        var b = a;
        var seg_to = seg_from;
        var has_segment = false;
        if next < count {
            let candidate = path_vertices[start + next];
            if is_contour_break(candidate) {
                if closed {
                    b = path_vertices[start + contour_start];
                    seg_to = contour_start;
                    has_segment = true;
                }
            } else {
                b = candidate;
                seg_to = next;
                has_segment = true;
            }
        } else if closed {
            b = path_vertices[start + contour_start];
            seg_to = contour_start;
            has_segment = true;
        }
        i = next;
        if !has_segment {
            continue;
        }
        let nearest = segment_nearest(p, a, b);
        // Strictly nearer: the first of equally near segments wins.
        if nearest.x < found.min_distance {
            found.min_distance = nearest.x;
            found.nearest_segment = seg_from;
            found.segment_end = seg_to;
            found.t_on_segment = nearest.y;
        }

        let cross = (b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y);
        if a.y <= p.y && b.y > p.y && cross > 0.0 {
            found.winding += 1;
        } else if a.y > p.y && b.y <= p.y && cross < 0.0 {
            found.winding -= 1;
        }
    }
    return found;
}

/// Fill coverage in `x`, stroke coverage in `y`: the two carry different
/// colors now, so they cannot be unioned here any more.
fn path_coverage(item: DrawItem, p: vec2<f32>) -> vec2<f32> {
    let closed = item.data0.w > 0.5;
    let fill = item.data1.x > 0.5 && closed;
    let stroke_width = item.data1.y;
    let found = path_sample(item, p);
    let winding = found.winding;
    let min_distance = found.min_distance;

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
    return vec2<f32>(fill_coverage, stroke_coverage);
}

/// Bilinear sample of `image_source` at a source-pixel coordinate, where
/// `(0.5, 0.5)` is the centre of the first texel, returned **premultiplied**.
///
/// The CPU reference (`sample_bilinear`) weights the four texels in
/// premultiplied form so a transparent one cannot bleed its colour, clamps to
/// the edge texel, and only then converts back to straight alpha. This is the
/// same accumulator; the conversion is skipped because the attachment blends
/// premultiplied colour anyway.
fn sample_image_source(uv: vec2<f32>, dimensions: vec2<i32>) -> vec4<f32> {
    let t = uv - vec2<f32>(0.5);
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
            let texel = textureLoad(image_source, coord, 0);
            accumulated += vec4<f32>(texel.rgb * texel.a, texel.a) * weight;
        }
    }
    return accumulated;
}

/// One image instance: the placement inverted per fragment, then a texel.
///
/// Edges are hard, and the containment test is the CPU path's verbatim — the
/// interval half-open on the far side — so abutting copies do not blend twice
/// along the edge they share.
fn image_color(item: DrawItem, position: vec2<f32>) -> vec4<f32> {
    let half_size = item.data1.zw;
    let delta = position - item.data0.yz;
    // `Placement::apply` inverted: `stroke_color` carries the inverse of the
    // placement's linear part, row-major `(a, b, c, d)`.
    let inverse = item.stroke_color;
    let local = vec2<f32>(
        inverse.x * delta.x + inverse.y * delta.y,
        inverse.z * delta.x + inverse.w * delta.y,
    );
    if local.x < -half_size.x || local.x >= half_size.x
        || local.y < -half_size.y || local.y >= half_size.y {
        return vec4<f32>(0.0);
    }
    let dimensions = textureDimensions(image_source);
    // Source texels per composition unit: exactly 1 when the image is stamped
    // at its own resolution, which is what makes an unscaled copy sample texel
    // centres exactly.
    let texel_scale = vec2<f32>(dimensions) / (half_size * 2.0);
    let sampled = sample_image_source((local + half_size) * texel_scale, vec2<i32>(dimensions));
    // The tint multiplies the straight-alpha texel; premultiplied that is the
    // accumulator scaled by the tint's colour and its alpha.
    return vec4<f32>(
        sampled.rgb * item.color.rgb * item.color.a,
        sampled.a * item.color.a,
    );
}

@fragment
fn raster_fragment(
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) item_index: u32,
) -> @location(0) vec4<f32> {
    let item = draw_items[item_index];
    if item.data0.x > 1.5 {
        return image_color(item, position.xy);
    }
    if item.data0.x < 0.5 {
        let center = item.data0.yz;
        let radius = item.data0.w;
        let coverage = clamp(radius - distance(position.xy, center) + 0.5, 0.0, 1.0);
        let alpha = item.color.a * coverage;
        return vec4<f32>(item.color.rgb * alpha, alpha);
    }
    // The CPU path blends the fill first and the stroke over it
    // (`raster_paths`); this is that composite in premultiplied form, which
    // reduces to the old single-color union when the two colors are equal.
    let coverage = path_coverage(item, position.xy);
    let fill_alpha = item.color.a * coverage.x;
    let stroke_alpha = item.stroke_color.a * coverage.y;
    let alpha = stroke_alpha + fill_alpha * (1.0 - stroke_alpha);
    let premultiplied = item.stroke_color.rgb * stroke_alpha
        + item.color.rgb * fill_alpha * (1.0 - stroke_alpha);
    return vec4<f32>(premultiplied, alpha);
}

// The unpremultiply pass is a separate pipeline with its own bind group
// layout, so its slots start at 0 again rather than continuing the draw pass's
// numbering above. Binding numbers only have to be unique among the globals
// one entry point reaches, and that is what lets this pass follow the
// declarative dispatch contract (inputs at `@binding(0..N)`, the output
// storage texture at `@binding(N)`).
@group(0) @binding(0) var premul_input: texture_2d<f32>;
@group(0) @binding(1) var straight_output: texture_storage_2d<rgba32float, write>;

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
