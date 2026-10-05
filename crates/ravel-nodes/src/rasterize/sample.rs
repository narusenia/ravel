// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The per-pixel path evaluator: what one pixel knows about a polyline.
//!
//! `rasterize.wgsl` has always walked every segment per fragment to get a
//! distance and a winding number. The CPU path hands the polyline to `zeno`
//! and gets a coverage mask back, which cannot say *which* segment a pixel is
//! near or *which side* of the path it lies on. [`path_sample`] is the CPU
//! twin of the shader's `path_sample`: the same four answers from the same
//! rules, so a feature that needs them (vertex colours, stroke alignment)
//! gives the same picture on either path
//! (`docs/implementation/path-shading-plan.md`).
//!
//! "Same rules" is structural, not a promise. The vertex layout is the one the
//! GPU reads — device-space vertices with [`super::CONTOUR_BREAK`] sentinels
//! between the contours of one run — and [`segments`] is the single statement
//! of which vertex pairs are segments, which `path_sample` consumes. The
//! shader's loop is that statement written out in WGSL;
//! `the_gpu_sample_matches_the_cpu_sample_on_every_pixel` holds the two to each
//! other on every pixel of a probe frame.

/// What a pixel centre knows about a polyline.
///
/// Segment indices are positions in the slice [`path_sample`] was given, and a
/// segment is named by the vertex it leaves (`nearest_segment`) and the vertex
/// it arrives at (`segment_end`) — the closing segment of a closed contour
/// arrives at that contour's **first** vertex, so the two are not always
/// adjacent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PathSample {
    /// Unsigned distance to the nearest segment. `1e20` when the polyline has
    /// no segment at all (the shader's initial value, so an empty path covers
    /// nothing rather than everything).
    pub min_distance: f32,
    /// Non-zero-rule winding number, accumulated across every contour.
    pub winding: i32,
    /// The vertex the nearest segment leaves. The first of equally near
    /// segments wins, as in the shader (`<`, not `<=`).
    pub nearest_segment: u32,
    /// The vertex the nearest segment arrives at.
    pub segment_end: u32,
    /// Where on that segment the nearest point lies, `0..=1`. `0` for a
    /// zero-length segment, which has no direction to project onto.
    pub t_on_segment: f32,
}

/// Whether a vertex is the sentinel between two contours rather than a point.
///
/// `>= 3.0e38` exactly as the shader writes it: false for a NaN, so a
/// malformed position is not read as a break.
fn is_contour_break(v: [f32; 2]) -> bool {
    v[0] >= 3.0e38
}

/// The `(from, to)` vertex indices of every segment of the run.
///
/// Segment `i -> i + 1` exists when both are points of one contour. Where a
/// contour ends — at a break or at the end of the slice — a **closed** run
/// wraps back to **that contour's own** first vertex, so no segment ever
/// bridges two contours, and an **open** run has no such wrap, so its last
/// vertex starts no segment.
fn segments(vertices: &[[f32; 2]], closed: bool) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut contour_start = 0;
    (0..vertices.len()).filter_map(move |i| {
        if is_contour_break(vertices[i]) {
            contour_start = i + 1;
            return None;
        }
        match vertices.get(i + 1) {
            Some(&next) if !is_contour_break(next) => Some((i, i + 1)),
            _ if closed => Some((i, contour_start)),
            _ => None,
        }
    })
}

/// Distance from `p` to the segment `a -> b` and the clamped parameter of the
/// nearest point on it. A degenerate segment (squared length `<= 1e-10`) is
/// the point `a`, with `t = 0`: the shader's rule, not a guess.
fn segment_nearest(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> (f32, f32) {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let denom = ab[0] * ab[0] + ab[1] * ab[1];
    let ap = [p[0] - a[0], p[1] - a[1]];
    if denom <= 1e-10 {
        return (ap[0].hypot(ap[1]), 0.0);
    }
    let t = ((ap[0] * ab[0] + ap[1] * ab[1]) / denom).clamp(0.0, 1.0);
    (
        (p[0] - (a[0] + t * ab[0])).hypot(p[1] - (a[1] + t * ab[1])),
        t,
    )
}

/// Sample the polyline `vertices` (device space, contours separated by
/// [`super::CONTOUR_BREAK`]) at the point `p`.
///
/// `closed` applies to every contour of the run, as it does for a draw item.
/// The winding rule is the shader's: a crossing counts when the segment
/// straddles `p.y` and `p` lies on the matching side of it.
// Unit 2 and 3 are the callers; until then only the tests use it.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn path_sample(vertices: &[[f32; 2]], closed: bool, p: [f32; 2]) -> PathSample {
    let mut sample = PathSample {
        min_distance: 1e20,
        winding: 0,
        nearest_segment: 0,
        segment_end: 0,
        t_on_segment: 0.0,
    };
    for (from, to) in segments(vertices, closed) {
        let (a, b) = (vertices[from], vertices[to]);
        let (distance, t) = segment_nearest(p, a, b);
        if distance < sample.min_distance {
            sample.min_distance = distance;
            sample.nearest_segment = from as u32;
            sample.segment_end = to as u32;
            sample.t_on_segment = t;
        }
        let cross = (b[0] - a[0]) * (p[1] - a[1]) - (p[0] - a[0]) * (b[1] - a[1]);
        if a[1] <= p[1] && b[1] > p[1] && cross > 0.0 {
            sample.winding += 1;
        } else if a[1] > p[1] && b[1] <= p[1] && cross < 0.0 {
            sample.winding -= 1;
        }
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rasterize::CONTOUR_BREAK;

    fn square() -> Vec<[f32; 2]> {
        vec![[2.0, 2.0], [10.0, 2.0], [10.0, 10.0], [2.0, 10.0]]
    }

    #[test]
    fn a_point_inside_a_closed_square_winds_once_and_is_near_the_edge_it_is_closest_to() {
        let sample = path_sample(&square(), true, [3.0, 6.0]);
        // Hand-derived: 1 px from the left edge, which is the closing segment
        // (vertex 3 -> vertex 0, parameter 0.5 up it).
        assert_eq!(sample.min_distance, 1.0);
        assert_eq!(sample.winding, 1);
        assert_eq!(
            (sample.nearest_segment, sample.segment_end),
            (3, 0),
            "the closing segment arrives at the first vertex"
        );
        assert_eq!(sample.t_on_segment, 0.5);
        // Outside: no winding, distance to the right edge.
        let outside = path_sample(&square(), true, [13.0, 6.0]);
        assert_eq!((outside.min_distance, outside.winding), (3.0, 0));
        assert_eq!(
            (outside.nearest_segment, outside.segment_end),
            (1, 2),
            "the right edge"
        );
    }

    #[test]
    fn an_open_paths_last_vertex_starts_no_segment() {
        let open = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
        // Nearest the missing closing edge's midpoint: the open path must
        // report the distance to what exists, 5 px to the vertical edge.
        let sample = path_sample(&open, false, [5.0, 5.0]);
        assert_eq!(sample.min_distance, 5.0);
        assert_eq!(sample.nearest_segment, 0);
        // Closed, the same point is on the third edge's hypotenuse side.
        let closed = path_sample(&open, true, [5.0, 5.0]);
        assert!(closed.min_distance < 1e-5, "{closed:?}");
        assert_eq!((closed.nearest_segment, closed.segment_end), (2, 0));
    }

    #[test]
    fn a_zero_length_segment_is_a_point_with_t_zero() {
        let vertices = [[4.0, 4.0], [4.0, 4.0], [8.0, 4.0]];
        let sample = path_sample(&vertices, false, [4.0, 7.0]);
        // The nearest is the degenerate first segment's point (4,4) at 3 px;
        // the live segment is 3 px away as well, and the first wins the tie.
        assert_eq!(sample.min_distance, 3.0);
        assert_eq!(sample.nearest_segment, 0);
        assert_eq!(sample.t_on_segment, 0.0);
    }

    #[test]
    fn a_single_vertex_is_a_point_when_closed_and_nothing_when_open() {
        let one = [[5.0, 5.0]];
        let closed = path_sample(&one, true, [8.0, 9.0]);
        assert_eq!((closed.min_distance, closed.winding), (5.0, 0));
        let open = path_sample(&one, false, [8.0, 9.0]);
        assert_eq!(open.min_distance, 1e20);
        assert_eq!(open.winding, 0);
    }

    #[test]
    fn a_self_intersecting_path_winds_by_the_non_zero_rule() {
        // A bowtie: the lobes are the triangles above and below the crossing
        // at (10, 10), and they wind against each other.
        let bowtie = [[0.0, 0.0], [20.0, 20.0], [0.0, 20.0], [20.0, 0.0]];
        let below = path_sample(&bowtie, true, [10.0, 3.0]);
        let above = path_sample(&bowtie, true, [10.0, 17.0]);
        assert_eq!((below.winding, above.winding), (-1, 1));
        assert_eq!(
            path_sample(&bowtie, true, [3.0, 10.0]).winding,
            0,
            "left of the crossing is outside both lobes"
        );
    }

    #[test]
    fn the_start_vertex_of_a_closed_path_is_at_distance_zero() {
        let sample = path_sample(&square(), true, [2.0, 2.0]);
        assert_eq!(sample.min_distance, 0.0);
        // Both edges meeting there are at distance 0; the first segment
        // (leaving vertex 0) wins the tie.
        assert_eq!(sample.nearest_segment, 0);
        assert_eq!(sample.t_on_segment, 0.0);
    }

    #[test]
    fn contours_close_onto_their_own_first_vertex_and_never_bridge() {
        // Two squares far apart. A bridge from the first's last vertex to the
        // second's first would pass through (20, 6).
        let mut vertices = square();
        vertices.push(CONTOUR_BREAK);
        vertices.extend([[30.0, 2.0], [38.0, 2.0], [38.0, 10.0], [30.0, 10.0]]);
        let between = path_sample(&vertices, true, [20.0, 6.0]);
        assert_eq!(between.min_distance, 10.0, "nothing lies between the two");
        assert_eq!(between.winding, 0);
        // The second contour's closing segment arrives at ITS first vertex.
        let second = path_sample(&vertices, true, [31.0, 6.0]);
        assert_eq!((second.nearest_segment, second.segment_end), (8, 5));
        // A hole: the same square wound the other way inside the first cancels.
        let mut ring = square();
        ring.push(CONTOUR_BREAK);
        ring.extend([[4.0, 4.0], [4.0, 8.0], [8.0, 8.0], [8.0, 4.0]]);
        assert_eq!(path_sample(&ring, true, [6.0, 6.0]).winding, 0);
        assert_eq!(path_sample(&ring, true, [3.0, 6.0]).winding, 1);
    }

    #[test]
    fn an_empty_run_has_no_segment() {
        let sample = path_sample(&[], true, [1.0, 1.0]);
        assert_eq!(sample.min_distance, 1e20);
        assert_eq!(sample.winding, 0);
    }

    // ---- CPU / GPU agreement ------------------------------------------------

    use crate::gpu_util;
    use crate::rasterize::{DrawItem, RasterParams, SHADER_SRC, raster_layout};
    use ravel_core::types::FrameBuffer;
    use ravel_gpu::{
        BlendMode, ColorTarget, ComputeDispatch, GpuContext, GpuFrameBuffer, QuadDraw, QuadRun,
        RasterPipeline, ShaderManager, TextureFormat, TextureKey, TexturePool, TextureUsage,
    };
    use std::sync::{Arc, Mutex};

    /// A fragment entry point that returns the sample itself instead of a
    /// colour, appended to the production shader so it reaches the very
    /// `path_sample` the rasterizer runs. `params._pad.x` picks which three
    /// numbers come out: the distances and winding, or the segment indices.
    /// Alpha is 1 so the premultiplied pass and the unpremultiply pass hand
    /// the three values back untouched.
    const PROBE_SRC: &str = "
@fragment
fn probe_fragment(
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) item_index: u32,
) -> @location(0) vec4<f32> {
    let found = path_sample(draw_items[item_index], position.xy);
    if params._pad.x > 0.5 {
        return vec4<f32>(f32(found.nearest_segment), f32(found.segment_end), found.t_on_segment, 1.0);
    }
    return vec4<f32>(found.min_distance, f32(found.winding), found.t_on_segment, 1.0);
}
";

    /// Run the shader's `path_sample` at every pixel centre of a `size` x
    /// `size` frame. `indices` selects the segment-index readout.
    fn gpu_probe(
        gpu: &GpuContext,
        vertices: &[[f32; 2]],
        closed: bool,
        size: u32,
        indices: bool,
    ) -> FrameBuffer {
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 16 * 1024 * 1024)));
        let mut shaders = ShaderManager::new(gpu.clone());
        let source = format!("{SHADER_SRC}\n{PROBE_SRC}");
        let shader = shaders
            .compile_source("rasterize_probe", &source)
            .expect("probe shader compiles");
        let pipeline = RasterPipeline::new(
            gpu,
            &shader,
            "raster_vertex",
            "probe_fragment",
            &raster_layout(),
            ColorTarget::new(TextureFormat::Rgba16Float, BlendMode::PremultipliedOver),
        );
        let unpremultiply = shaders
            .compute_pipeline(
                "rasterize_probe",
                &source,
                "unpremultiply",
                &[
                    gpu_util::input_texture_layout_entry(0),
                    gpu_util::output_storage_layout_entry(1),
                ],
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("unpremultiply compiles");

        let item = DrawItem {
            bounds: [0.0, 0.0, size as f32, size as f32],
            color: [0.0; 4],
            stroke_color: [0.0; 4],
            data0: [1.0, 0.0, vertices.len() as f32, f32::from(u8::from(closed))],
            data1: [0.0; 4],
        };
        let params = RasterParams {
            resolution: [size as f32, size as f32],
            _pad: [f32::from(u8::from(indices)), 0.0],
        };
        let (premul, output, placeholder) = {
            let mut pool = pool.lock().unwrap();
            (
                pool.acquire(TextureKey::new(
                    size,
                    size,
                    TextureFormat::Rgba16Float,
                    TextureUsage::RENDER_ATTACHMENT | TextureUsage::TEXTURE_BINDING,
                )),
                pool.acquire(gpu_util::tex_key_rw(size, size)),
                pool.acquire(TextureKey::new(
                    1,
                    1,
                    TextureFormat::Rgba32Float,
                    TextureUsage::TEXTURE_BINDING,
                )),
            )
        };
        let (premul_binding, output_binding, placeholder_binding) =
            (premul.binding(), output.binding(), placeholder.binding());
        gpu.draw_quads(&QuadDraw {
            label: "path sample probe",
            pipeline: &pipeline,
            uniform: bytemuck::bytes_of(&params),
            storage: &[bytemuck::cast_slice(vertices), bytemuck::bytes_of(&item)],
            target: &premul_binding,
            runs: &[QuadRun {
                texture: &placeholder_binding,
                instances: 0..1,
            }],
        });
        gpu.dispatch_compute(&ComputeDispatch {
            label: "path sample probe unpremultiply",
            pipeline: &unpremultiply,
            inputs: std::slice::from_ref(&premul_binding),
            output: &output_binding,
            uniform: &[],
            width: size,
            height: size,
        });
        GpuFrameBuffer::new(gpu.clone(), &pool, output, size, size)
            .to_frame_buffer()
            .expect("probe readback")
    }

    /// The shader and `path_sample` agree on every pixel of a frame, for the
    /// shapes the plan names: a plain closed path, a self-intersection, a
    /// contour with a counter, an open path with a zero-length segment and a
    /// bare point. Distances come back through an `Rgba16Float` attachment, so
    /// they are compared to half a sixteenth of a pixel — far finer than any
    /// rule difference (a wrong tie-break, a segment bridging two contours, a
    /// flipped crossing) would show up as.
    #[test]
    fn the_gpu_sample_matches_the_cpu_sample_on_every_pixel() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let size = 32u32;
        let mut ring = vec![[3.3, 3.1], [28.7, 3.9], [27.2, 28.4], [4.6, 27.7]];
        ring.push(CONTOUR_BREAK);
        ring.extend([[10.2, 10.6], [10.9, 21.3], [21.4, 20.8], [20.7, 11.2]]);
        let cases: [(&str, Vec<[f32; 2]>, bool); 6] = [
            (
                "closed triangle",
                vec![[4.5, 5.5], [27.5, 9.5], [12.5, 26.5]],
                true,
            ),
            (
                "bowtie",
                vec![[3.3, 3.1], [28.7, 28.9], [3.1, 28.7], [28.9, 3.3]],
                true,
            ),
            ("ring with a counter", ring, true),
            (
                "open path with a zero-length segment",
                vec![[5.5, 6.5], [5.5, 6.5], [26.5, 8.5], [20.5, 26.5]],
                false,
            ),
            ("one vertex, closed", vec![[16.5, 15.5]], true),
            (
                "start vertex on a pixel centre",
                vec![[8.5, 8.5], [24.5, 8.5], [24.5, 24.5], [8.5, 24.5]],
                true,
            ),
        ];
        for (label, vertices, closed) in cases {
            let metrics = gpu_probe(&gpu, &vertices, closed, size, false);
            let indices = gpu_probe(&gpu, &vertices, closed, size, true);
            let (metrics, indices) = (metrics.as_f32(), indices.as_f32());
            let mut index_mismatches = 0;
            for y in 0..size {
                for x in 0..size {
                    let at = [x as f32 + 0.5, y as f32 + 0.5];
                    let cpu = path_sample(&vertices, closed, at);
                    let i = ((y * size + x) * 4) as usize;
                    let ctx = format!("{label} at ({x},{y}): CPU {cpu:?}");
                    assert!(
                        (metrics[i] - cpu.min_distance).abs() < 0.05,
                        "{ctx}, GPU distance {}",
                        metrics[i]
                    );
                    assert_eq!(metrics[i + 1] as i32, cpu.winding, "{ctx}");
                    assert!(
                        (metrics[i + 2] - cpu.t_on_segment).abs() < 5e-3
                            || (indices[i] as u32, indices[i + 1] as u32)
                                != (cpu.nearest_segment, cpu.segment_end),
                        "{ctx}, GPU t {}",
                        metrics[i + 2]
                    );
                    let gpu_segment = (indices[i] as u32, indices[i + 1] as u32);
                    if gpu_segment != (cpu.nearest_segment, cpu.segment_end) {
                        // Two segments equally near (a corner) may be picked
                        // either way by arithmetic that differs in the last
                        // bit. That is only acceptable when the GPU's choice
                        // is genuinely as near as the CPU's.
                        index_mismatches += 1;
                        let (a, b) = (
                            vertices[gpu_segment.0 as usize],
                            vertices[gpu_segment.1 as usize],
                        );
                        let (distance, _) = segment_nearest(at, a, b);
                        assert!(
                            (distance - cpu.min_distance).abs() < 1e-3,
                            "{ctx}, GPU picked segment {gpu_segment:?} at {distance}"
                        );
                    }
                }
            }
            assert!(
                index_mismatches * 50 < size * size,
                "{label}: {index_mismatches} pixels pick another equally near segment"
            );
        }
    }
}
