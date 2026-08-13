// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Geometry → FrameBuffer rasterization (GPU and CPU reference paths).
//!
//! Paths are filled/stroked through `zeno` with antialiased coverage; loose
//! points (those not referenced by a `Primitive::Path`) draw as analytic-AA
//! circle sprites. Instances expand their source geometry
//! with per-instance `P`/`rot`/`scale` and optional `Cd`/`alpha` tint.
//! The GPU path flattens those attributes into instanced-quad draw records;
//! its fragment shader evaluates non-zero winding and edge distance directly,
//! so concave and self-intersecting paths do not require triangulation.
//! Geometry positions are interpreted in output pixel space (origin top-left).
//! Output is straight-alpha RGBA f32, composited src-over to match the
//! existing merge convention.

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{AttributeSet, Domain, Geometry, Primitive, names};
use ravel_core::graph::Node;
use ravel_core::types::{Color, FrameBuffer, NodeData, Vec2};
use ravel_gpu::{
    BindingDesc, BindingKind, BlendMode, ColorTarget, ComputeDispatch, ComputePipeline, GpuContext,
    GpuFrameBuffer, QuadDraw, RasterPipeline, ShaderManager, ShaderVisibility, TextureFormat,
    TextureKey, TexturePool, TextureUsage,
};
use std::ops::Range;
use std::sync::{Arc, Mutex};
use zeno::{Cap, Command, Fill, Join, Mask, Stroke, Vector};

use crate::composition_scale;
use crate::flatten;
use crate::gpu_util;

const SHADER_SRC: &str = include_str!("../shaders/rasterize.wgsl");

/// Instance nesting guard: instances-of-instances beyond this depth are
/// skipped rather than recursed (spec limits stateful/sim nesting similarly).
const MAX_INSTANCE_DEPTH: u32 = 4;
const DEFAULT_POINT_RADIUS: f32 = 2.0;

/// Fill/stroke style in effect for one element.
///
/// It starts as the node's parameters and is narrowed per element by the
/// `fill` / `stroke_width` / `stroke_color` attributes
/// ([`element_style`]): attribute > parameter > hard-coded default. An
/// instance narrows it for everything it expands, so a `scatter` that
/// modulates `stroke_width` on the Instance domain reaches the source
/// geometry's paths.
#[derive(Clone, Copy)]
struct Style<'a> {
    fill: bool,
    stroke_width: f32,
    /// Base color for elements without `Cd`/`alpha` attributes: the `color`
    /// input pin when connected, else the `color` parameter (REQ-LAYER-008;
    /// attribute > pin > parameter priority).
    color: Color,
    /// Stroke color, when something set `stroke_color`. `None` strokes in the
    /// element's own fill color (`Cd`, else `color`), which is what the
    /// rasterizer did before the stroke had a color of its own.
    stroke_color: Option<Color>,
    /// Cap, join and dash. Unlike the fields above these are Detail
    /// attributes, so they are read once from the rasterized geometry and
    /// apply to everything it draws, instance sources included.
    shape: StrokeShape<'a>,
}

/// The outline shape of a stroke: the `cap` / `join` / `dash` / `dash_offset`
/// Detail attributes, already resolved to what zeno wants and scaled to
/// device pixels.
#[derive(Clone, Copy)]
struct StrokeShape<'a> {
    cap: Cap,
    join: Join,
    /// Alternating on/off run lengths; empty draws a solid stroke.
    dashes: &'a [f32],
    dash_offset: f32,
}

impl StrokeShape<'_> {
    /// Whether the GPU fragment shader can draw this stroke.
    ///
    /// It measures the unsigned distance to the polyline, which is round at
    /// every cap and join and knows nothing of arc length — so a square cap, a
    /// miter join or a dash is CPU-only. Approximating them in WGSL would
    /// break the CPU/GPU agreement the equivalence tests hold to, which is why
    /// `style-attributes-plan.md` unit 3 allows this fallback.
    fn drawable_on_gpu(&self) -> bool {
        self.cap == Cap::Round && self.join == Join::Round && self.dashes.is_empty()
    }
}

/// How far past the path a stroke of `width` can reach, joins included: a
/// miter spike runs out to `miter_limit` half-widths, and zeno writes those
/// pixels into the shared coverage mask whether or not the blend rectangle
/// covers them.
fn stroke_margin(width: f32, join: Join) -> f32 {
    let half_widths = if join == Join::Miter {
        ZENO_MITER_LIMIT
    } else {
        1.0
    };
    width * 0.5 * half_widths + 1.0
}

/// zeno's default miter limit (`zeno::Stroke::default`), which the rasterizer
/// does not change.
///
/// An upper bound on the reach, not the reach itself: zeno also bevels any
/// turn sharper than a right angle, so a miter never actually exceeds √2
/// half-widths. Sizing the blend rectangle by the declared limit costs a few
/// pixels of scan and does not depend on that second rule staying true.
const ZENO_MITER_LIMIT: f32 = 4.0;

/// Per-element placement accumulated while expanding instances.
#[derive(Clone, Copy)]
struct Placement {
    offset: Vec2,
    rot: f32,
    scale: Vec2,
    tint: Color,
}

impl Placement {
    fn identity() -> Self {
        Self {
            offset: Vec2(0.0, 0.0),
            rot: 0.0,
            scale: Vec2(1.0, 1.0),
            tint: Color::new(1.0, 1.0, 1.0, 1.0),
        }
    }

    fn for_context(ctx: &EvalContext) -> Self {
        let (scale_x, scale_y) = composition_scale(ctx);
        Self {
            scale: Vec2(scale_x as f32, scale_y as f32),
            ..Self::identity()
        }
    }

    fn apply(&self, p: Vec2) -> Vec2 {
        let scaled = Vec2(p.0 * self.scale.0, p.1 * self.scale.1);
        let (sin, cos) = self.rot.sin_cos();
        Vec2(
            self.offset.0 + scaled.0 * cos - scaled.1 * sin,
            self.offset.1 + scaled.0 * sin + scaled.1 * cos,
        )
    }

    fn uniform_scale(&self) -> f32 {
        (self.scale.0.abs() + self.scale.1.abs()) * 0.5
    }
}

pub struct RasterizeProcessor {
    gpu: Option<GpuRasterizer>,
}

impl RasterizeProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self { gpu: None }
    }

    /// Construct the GPU render-pass implementation used by graph evaluation.
    /// [`Self::from_node`] remains the CPU reference/fallback constructor.
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
        _node: &Node,
    ) -> Self {
        Self {
            gpu: Some(GpuRasterizer::new(ctx, shaders, pool)),
        }
    }
}

impl NodeProcessor for RasterizeProcessor {
    /// Nothing here comes off the node: the constructor takes `&Node` only to
    /// match the registry's signature and ignores it, and every value used is
    /// read from `params` at dispatch. Rebuilding on a parameter edit would
    /// recompile the shader and recreate the pipeline for no change at all.
    fn rebuild_on_node_change(&self) -> bool {
        false
    }

    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geo = inputs
            .first()
            .and_then(|input| input.as_ref())
            .and_then(|input| input.downcast_ref::<Geometry>())
            .context("rasterize expects a Geometry input")?;
        ensure_planar_paths(geo, 0)?;

        // Dash lengths are authored in composition pixels like `stroke_width`,
        // so they scale with the render resolution the same way.
        let scale = Placement::for_context(ctx).uniform_scale();
        let dashes = dash_pattern(geo.detail(), scale);
        let style = Style {
            fill: params.bool_or("fill", true),
            stroke_width: params.f32_or("stroke_width", 0.0),
            color: base_color(params),
            stroke_color: None,
            shape: StrokeShape {
                cap: detail_cap(geo.detail()),
                join: detail_join(geo.detail()),
                dashes: &dashes,
                dash_offset: attr_f32(geo.detail(), names::DASH_OFFSET, 0).unwrap_or(0.0) * scale,
            },
        };

        if let Some(gpu) = &self.gpu {
            if style.shape.drawable_on_gpu() {
                return gpu.rasterize(geo, style, ctx);
            }
            tracing::debug!(
                cap = ?style.shape.cap,
                join = ?style.shape.join,
                dashed = !style.shape.dashes.is_empty(),
                "rasterize: stroke shape has no GPU form, drawing on the CPU"
            );
        }

        let (width, height) = ctx.resolution;
        let span = tracing::debug_span!(
            "cpu_rasterize",
            width,
            height,
            points = geo.points().element_count(),
            instances = geo.instances().element_count()
        );
        let _guard = span.enter();
        let mut pixels = vec![0.0f32; width as usize * height as usize * 4];
        // One mask for the whole geometry, instances included. Allocating it
        // per primitive — twice per primitive, once more for the stroke — is
        // what made this path cost O(primitives x resolution) in allocation
        // alone (issue MED-GPU-04).
        let mut coverage = vec![0u8; width as usize * height as usize];

        raster_geometry(
            geo,
            Placement::for_context(ctx),
            0,
            &mut Canvas {
                pixels: &mut pixels,
                coverage: &mut coverage,
                width,
                height,
            },
            style,
        );

        Ok(Arc::new(FrameBuffer::from_f32(width, height, pixels)))
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RasterParams {
    resolution: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawItem {
    bounds: [f32; 4],
    color: [f32; 4],
    stroke_color: [f32; 4],
    data0: [f32; 4],
    data1: [f32; 4],
}

struct GpuRasterizer {
    ctx: GpuContext,
    raster_pipeline: RasterPipeline,
    unpremultiply_pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl GpuRasterizer {
    fn new(ctx: GpuContext, shaders: &mut ShaderManager, pool: Arc<Mutex<TexturePool>>) -> Self {
        let shader = shaders
            .compile_source("rasterize", SHADER_SRC)
            .expect("rasterize.wgsl compilation failed");
        let raster_layout = [
            BindingDesc::new(
                0,
                BindingKind::UniformBuffer,
                ShaderVisibility::VERTEX_FRAGMENT,
            ),
            BindingDesc::new(
                1,
                BindingKind::ReadOnlyStorageBuffer,
                ShaderVisibility::FRAGMENT,
            ),
            BindingDesc::new(
                2,
                BindingKind::ReadOnlyStorageBuffer,
                ShaderVisibility::VERTEX_FRAGMENT,
            ),
        ];
        let raster_pipeline = RasterPipeline::new(
            &ctx,
            &shader,
            "raster_vertex",
            "raster_fragment",
            &raster_layout,
            // Must stay the format `premul_key` asks the pool for. The pass
            // blends premultiplied coverage, which the `unpremultiply` compute
            // pass below converts back to straight alpha.
            ColorTarget::new(TextureFormat::Rgba16Float, BlendMode::PremultipliedOver),
        );
        let unpremultiply_layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::output_storage_layout_entry(1),
        ];
        // Shared across every rasterize node (the raster pipeline above still
        // belongs to this processor; only the compute pass is cached).
        let unpremultiply_pipeline = shaders
            .compute_pipeline(
                "rasterize",
                SHADER_SRC,
                "unpremultiply",
                &unpremultiply_layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("rasterize.wgsl compilation failed");
        Self {
            ctx,
            raster_pipeline,
            unpremultiply_pipeline,
            pool,
        }
    }

    fn rasterize(
        &self,
        geo: &Geometry,
        style: Style,
        ctx: &EvalContext,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let (width, height) = ctx.resolution;
        anyhow::ensure!(
            width > 0 && height > 0,
            "rasterize resolution must be non-zero"
        );
        let span = tracing::debug_span!(
            "gpu_rasterize",
            width,
            height,
            points = geo.points().element_count(),
            instances = geo.instances().element_count()
        );
        let _guard = span.enter();

        let mut vertices = Vec::new();
        let mut items = Vec::new();
        {
            // Split out so the geometry-scaling baseline can attribute cost to
            // the CPU flatten separately from the upload and the submit
            // (`gpu-resident-geometry-plan.md` phase 0).
            let flatten = tracing::debug_span!("raster_flatten");
            let _flatten_guard = flatten.enter();
            flatten_geometry(
                geo,
                Placement::for_context(ctx),
                0,
                style,
                &mut vertices,
                &mut items,
            );
        }

        // Empty storage bindings still need a non-zero-sized backing buffer.
        let dummy_vertices = [[0.0f32; 2]];
        let dummy_items = [DrawItem {
            bounds: [0.0; 4],
            color: [0.0; 4],
            stroke_color: [0.0; 4],
            data0: [0.0; 4],
            data1: [0.0; 4],
        }];
        let vertex_bytes: &[u8] = if vertices.is_empty() {
            bytemuck::cast_slice(&dummy_vertices)
        } else {
            bytemuck::cast_slice(&vertices)
        };
        let item_bytes: &[u8] = if items.is_empty() {
            bytemuck::cast_slice(&dummy_items)
        } else {
            bytemuck::cast_slice(&items)
        };
        let params = RasterParams {
            resolution: [width as f32, height as f32],
            _pad: [0.0; 2],
        };

        let premul_key = TextureKey::new(
            width,
            height,
            TextureFormat::Rgba16Float,
            TextureUsage::RENDER_ATTACHMENT | TextureUsage::TEXTURE_BINDING,
        );
        let (premul_texture, output_texture) = {
            let mut pool = self.pool.lock().expect("texture pool poisoned");
            (
                pool.acquire(premul_key),
                pool.acquire(gpu_util::tex_key_rw(width, height)),
            )
        };
        let premul_binding = premul_texture.binding();
        let output_binding = output_texture.binding();

        {
            // Both passes are recorded into the frame's shared encoder; the
            // submit happens at the next flush point (the viewer readback in
            // the application), not here.
            let submit = tracing::debug_span!("raster_submit", draws = items.len());
            let _submit_guard = submit.enter();
            self.ctx.draw_quads(&QuadDraw {
                label: "rasterize draw data",
                pipeline: &self.raster_pipeline,
                uniform: bytemuck::bytes_of(&params),
                storage: &[vertex_bytes, item_bytes],
                target: &premul_binding,
                instance_count: items.len() as u32,
            });
            self.ctx.dispatch_compute(&ComputeDispatch {
                label: "rasterize unpremultiply",
                pipeline: &self.unpremultiply_pipeline,
                inputs: std::slice::from_ref(&premul_binding),
                output: &output_binding,
                // The pass reads its extent from the output texture.
                uniform: &[],
                width,
                height,
            });
        }
        // Safe before the batch is submitted: the recorded draw and dispatch
        // put this texture in the batch's used set, and the pool refuses to
        // hand out a texture the pending batch still touches.
        self.pool
            .lock()
            .expect("texture pool poisoned")
            .release(premul_texture);

        Ok(Arc::new(GpuFrameBuffer::new(
            self.ctx.clone(),
            &self.pool,
            output_texture,
            width,
            height,
        )))
    }
}

/// Rejects three-dimensional positions and mesh primitives before any drawing
/// happens.
///
/// This rasterizer is analytic and planar: it evaluates each fragment's
/// distance to the path segments in composition space, with no vertex buffer
/// and no depth attachment. 3D geometry is drawn through `scene.render`
/// instead, so the input has to say so rather than disappear — every read of
/// `P` below falls back to an empty slice, which would otherwise render a
/// blank frame with no explanation.
///
/// Meshes are refused for the same reason and at the same place. The draw
/// walks below iterate primitives and would simply not match a mesh, so a
/// mesh-only geometry would rasterize to an empty frame and a mixed one would
/// silently drop its solid surfaces. Triangle drawing belongs to
/// `scene.render`.
fn ensure_planar_paths(geo: &Geometry, depth: u32) -> anyhow::Result<()> {
    for domain in [Domain::Point, Domain::Instance] {
        if let Some(positions) = geo.positions(domain) {
            positions?.require_planar("rasterize")?;
        }
    }
    geo.require_paths("rasterize")?;
    // Nesting past the limit is dropped by the draw walk with a warning, so
    // there is nothing below it left to validate.
    if depth < MAX_INSTANCE_DEPTH {
        for source in geo.instance_sources() {
            ensure_planar_paths(source, depth + 1)?;
        }
    }
    Ok(())
}

/// The draw-ready polyline of a path primitive: the control polygon, or —
/// when the points carry `in_tan` / `out_tan` attributes — the shared bezier
/// flattening of its curved segments (REQ-UI-011 unit 6). The CPU and GPU
/// paths both consume this, so curves render identically on either.
fn path_polyline(
    geo: &Geometry,
    positions: &[Vec2],
    verts: &Range<usize>,
    closed: bool,
) -> Vec<Vec2> {
    let column = |name: &str| {
        geo.points()
            .get(name)
            .and_then(|c| c.as_vec2(name).ok())
            .and_then(|values| (verts.end <= values.len()).then(|| &values[verts.clone()]))
    };
    let in_tans = column(names::IN_TAN);
    let out_tans = column(names::OUT_TAN);
    if in_tans.is_none() && out_tans.is_none() {
        return positions[verts.clone()].to_vec();
    }
    flatten::flatten_path(&positions[verts.clone()], in_tans, out_tans, closed)
}

fn flatten_geometry(
    geo: &Geometry,
    placement: Placement,
    depth: u32,
    style: Style,
    vertices: &mut Vec<[f32; 2]>,
    items: &mut Vec<DrawItem>,
) {
    let positions = geo
        .points()
        .get(names::P)
        .and_then(|c| c.as_vec2(names::P).ok())
        .unwrap_or_default();

    for (prim_index, prim) in geo.primitives().iter().enumerate() {
        // `ensure_planar_paths` refused meshes at the node entry, so this skip
        // never fires; it keeps the walk total without a panic.
        let Primitive::Path { verts, closed } = prim else {
            continue;
        };
        let style = element_style(style, geo.primitive_attrs(), prim_index);
        if verts.len() < 2
            || verts.end > positions.len()
            || (!style.fill && style.stroke_width <= 0.0)
        {
            continue;
        }
        let start = vertices.len();
        let mut bounds = [
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        ];
        let polyline = path_polyline(geo, positions, verts, *closed);
        for position in &polyline {
            let point = placement.apply(*position);
            vertices.push([point.0, point.1]);
            bounds[0] = bounds[0].min(point.0);
            bounds[1] = bounds[1].min(point.1);
            bounds[2] = bounds[2].max(point.0);
            bounds[3] = bounds[3].max(point.1);
        }
        let scaled_stroke = style.stroke_width * placement.uniform_scale();
        let padding = if scaled_stroke > 0.0 {
            scaled_stroke * 0.5 + 1.0
        } else {
            1.0
        };
        expand_bounds(&mut bounds, padding);
        let (color, stroke_color) =
            element_colors(style, geo.primitive_attrs(), prim_index, placement.tint);
        items.push(DrawItem {
            bounds,
            color: color_array(color),
            stroke_color: color_array(stroke_color),
            data0: [
                1.0,
                start as f32,
                polyline.len() as f32,
                u32::from(*closed) as f32,
            ],
            data1: [u32::from(style.fill) as f32, scaled_stroke, 0.0, 0.0],
        });
    }

    let radii = float_column(geo.points(), names::PSCALE);
    let sprite_mask = path_vertex_mask(geo, positions.len());
    for (index, position) in positions.iter().enumerate() {
        if sprite_mask[index] {
            continue;
        }
        let center = placement.apply(*position);
        let radius =
            radii.as_ref().map_or(DEFAULT_POINT_RADIUS, |r| r[index]) * placement.uniform_scale();
        if radius <= 0.0 {
            continue;
        }
        let color = tinted(
            element_color(geo.points(), index, style.color),
            element_alpha(geo.points(), index),
            placement.tint,
        );
        items.push(DrawItem {
            bounds: [
                center.0 - radius - 1.0,
                center.1 - radius - 1.0,
                center.0 + radius + 1.0,
                center.1 + radius + 1.0,
            ],
            color: color_array(color),
            // Sprites do not stroke; the shader never reads this slot for them.
            stroke_color: [0.0; 4],
            data0: [0.0, center.0, center.1, radius],
            data1: [0.0; 4],
        });
    }

    if depth >= MAX_INSTANCE_DEPTH {
        if !geo.instance_sources().is_empty() {
            log::warn!("rasterize: instance nesting deeper than {MAX_INSTANCE_DEPTH}, skipping");
        }
        return;
    }
    let sources = geo.instance_sources();
    if sources.is_empty() {
        return;
    }
    let instances = geo.instances();
    let Some(offsets) = instances
        .get(names::P)
        .and_then(|c| c.as_vec2(names::P).ok())
    else {
        return;
    };
    let rotations = float_column(instances, names::ROT);
    let scales = instances
        .get(names::SCALE)
        .and_then(|c| c.as_vec2(names::SCALE).ok());
    let source_indices = instances
        .get(names::SOURCE_INDEX)
        .and_then(|c| c.as_i32(names::SOURCE_INDEX).ok());
    for (index, offset) in offsets.iter().enumerate() {
        let local = Placement {
            offset: *offset,
            rot: rotations.as_ref().map_or(0.0, |values| values[index]),
            scale: scales.map_or(Vec2(1.0, 1.0), |values| values[index]),
            // Instance tint is multiplicative: fall back to neutral white so
            // the base color applies once, at the leaf elements.
            tint: tinted(
                element_color(instances, index, Color::new(1.0, 1.0, 1.0, 1.0)),
                element_alpha(instances, index),
                Color::new(1.0, 1.0, 1.0, 1.0),
            ),
        };
        let source = select_instance_source(sources, source_indices, index);
        flatten_geometry(
            source,
            compose(placement, local),
            depth + 1,
            element_style(style, instances, index),
            vertices,
            items,
        );
    }
}

/// True for each point referenced by a primitive. Those vertices are already
/// represented by their fill/stroke; only unmarked ("loose") points draw as
/// circle sprites.
///
/// Reading `verts()` keeps this kind-agnostic: a mesh covers its vertices for
/// the same reason a path does. Meshes cannot reach here today, but the rule
/// does not depend on which variant supplied the run.
fn path_vertex_mask(geo: &Geometry, point_count: usize) -> Vec<bool> {
    let mut mask = vec![false; point_count];
    for prim in geo.primitives() {
        let verts = prim.verts();
        let end = verts.end.min(point_count);
        let start = verts.start.min(end);
        for covered in &mut mask[start..end] {
            *covered = true;
        }
    }
    mask
}

fn expand_bounds(bounds: &mut [f32; 4], amount: f32) {
    bounds[0] -= amount;
    bounds[1] -= amount;
    bounds[2] += amount;
    bounds[3] += amount;
}

fn color_array(color: Color) -> [f32; 4] {
    [color.r, color.g, color.b, color.a]
}

/// The frame being drawn into, plus the single coverage mask every primitive
/// of one `process` call shares.
struct Canvas<'a> {
    pixels: &'a mut [f32],
    /// **Zero on entry to every draw and zero again afterwards.** zeno writes
    /// only the spans a shape covers and never clears the rest, so a mask
    /// handed over dirty would stamp the previous primitive's silhouette;
    /// [`Canvas::blend_coverage`] restores the invariant as it reads.
    coverage: &'a mut [u8],
    width: u32,
    height: u32,
}

/// A half-open pixel rectangle, clamped to the canvas.
#[derive(Clone, Copy)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Canvas<'_> {
    /// Composite the mask zeno just rendered — restricted to `rect`, the only
    /// pixels it can have written — and zero those pixels again.
    ///
    /// The restriction is what keeps a primitive's cost proportional to its
    /// own size instead of the frame's: a scatter of a hundred small shapes
    /// used to walk the whole canvas a hundred times.
    fn blend_coverage(&mut self, rect: Rect, color: Color) {
        for y in rect.y0..rect.y1 {
            let row = (y * self.width) as usize;
            for i in row + rect.x0 as usize..row + rect.x1 as usize {
                let cov = std::mem::take(&mut self.coverage[i]);
                if cov != 0 {
                    blend_pixel(
                        &mut self.pixels[i * 4..i * 4 + 4],
                        color,
                        cov as f32 / 255.0,
                    );
                }
            }
        }
        // The rectangle has to bound what zeno wrote, or the leftovers become
        // the next primitive's phantom coverage. Checked on every CPU
        // rasterize the test suite runs rather than argued about in a comment.
        debug_assert!(
            self.coverage.iter().all(|&c| c == 0),
            "coverage outside the primitive's bounds was not clean"
        );
    }
}

/// The pixels a mask over `bounds` can write: the path's device-space extent
/// grown by `margin` (the antialiasing feather, plus half the stroke width
/// when stroking) and clamped to the canvas.
fn coverage_rect(bounds: (Vec2, Vec2), margin: f32, width: u32, height: u32) -> Rect {
    let (min, max) = bounds;
    Rect {
        x0: (min.0 - margin).floor().clamp(0.0, width as f32) as u32,
        y0: (min.1 - margin).floor().clamp(0.0, height as f32) as u32,
        x1: (max.0 + margin).ceil().clamp(0.0, width as f32) as u32,
        y1: (max.1 + margin).ceil().clamp(0.0, height as f32) as u32,
    }
}

fn raster_geometry(
    geo: &Geometry,
    placement: Placement,
    depth: u32,
    canvas: &mut Canvas<'_>,
    style: Style,
) {
    let positions = geo
        .points()
        .get(names::P)
        .and_then(|c| c.as_vec2(names::P).ok().map(<[Vec2]>::to_vec))
        .unwrap_or_default();

    raster_paths(geo, &positions, placement, canvas, style);
    raster_points(geo, &positions, placement, canvas, style);
    raster_instances(geo, placement, depth, canvas, style);
}

fn raster_paths(
    geo: &Geometry,
    positions: &[Vec2],
    placement: Placement,
    canvas: &mut Canvas<'_>,
    style: Style,
) {
    let (width, height) = (canvas.width, canvas.height);
    for (prim_index, prim) in geo.primitives().iter().enumerate() {
        // Unreachable for meshes: see the twin walk in `flatten_geometry`.
        let Primitive::Path { verts, closed } = prim else {
            continue;
        };
        if verts.len() < 2 || verts.end > positions.len() {
            continue;
        }

        let polyline = path_polyline(geo, positions, verts, *closed);
        let mut commands = Vec::with_capacity(polyline.len() + 1);
        let mut min = Vec2(f32::INFINITY, f32::INFINITY);
        let mut max = Vec2(f32::NEG_INFINITY, f32::NEG_INFINITY);
        for (i, p) in polyline.iter().enumerate() {
            let v = placement.apply(*p);
            min = Vec2(min.0.min(v.0), min.1.min(v.1));
            max = Vec2(max.0.max(v.0), max.1.max(v.1));
            let v = Vector::new(v.0, v.1);
            commands.push(if i == 0 {
                Command::MoveTo(v)
            } else {
                Command::LineTo(v)
            });
        }
        if *closed {
            commands.push(Command::Close);
        }

        let style = element_style(style, geo.primitive_attrs(), prim_index);
        let (color, stroke_color) =
            element_colors(style, geo.primitive_attrs(), prim_index, placement.tint);

        if style.fill && *closed {
            Mask::new(commands.as_slice())
                .size(width, height)
                .style(Fill::NonZero)
                .render_into(canvas.coverage, None);
            // One pixel of margin: the fill is antialiased, so the outermost
            // covered pixel is the one the boundary passes through.
            canvas.blend_coverage(coverage_rect((min, max), 1.0, width, height), color);
        }
        if style.stroke_width > 0.0 {
            // Round caps/joins are the default because they match the GPU
            // stroke, which is an unsigned distance to the polyline
            // (inherently round at caps and joins). Anything else the `cap` /
            // `join` / `dash` attributes ask for is drawn here, on the CPU
            // path the node routes to for exactly that reason.
            let stroke_width = style.stroke_width * placement.uniform_scale();
            let mut stroke = Stroke::new(stroke_width);
            stroke.cap(style.shape.cap).join(style.shape.join);
            if !style.shape.dashes.is_empty() {
                stroke.dash(style.shape.dashes, style.shape.dash_offset);
            }
            Mask::new(commands.as_slice())
                .size(width, height)
                .style(stroke)
                .render_into(canvas.coverage, None);
            // The stroke straddles the path: half its width on each side, and
            // a cap or a miter spike reaches further still. The rectangle has
            // to bound every pixel zeno wrote, or the leftovers become the
            // next primitive's coverage.
            canvas.blend_coverage(
                coverage_rect(
                    (min, max),
                    stroke_margin(stroke_width, style.shape.join),
                    width,
                    height,
                ),
                stroke_color,
            );
        }
    }
}

fn raster_instances(
    geo: &Geometry,
    placement: Placement,
    depth: u32,
    canvas: &mut Canvas<'_>,
    style: Style,
) {
    if depth >= MAX_INSTANCE_DEPTH {
        log::warn!("rasterize: instance nesting deeper than {MAX_INSTANCE_DEPTH}, skipping");
        return;
    }
    let sources = geo.instance_sources();
    if sources.is_empty() {
        return;
    }
    let inst = geo.instances();
    let Some(offsets) = inst.get(names::P).and_then(|c| c.as_vec2(names::P).ok()) else {
        return;
    };
    let offsets = offsets.to_vec();
    let rots = float_column(inst, names::ROT);
    let scales = inst
        .get(names::SCALE)
        .and_then(|c| c.as_vec2(names::SCALE).ok())
        .map(<[Vec2]>::to_vec);
    let source_indices = inst
        .get(names::SOURCE_INDEX)
        .and_then(|c| c.as_i32(names::SOURCE_INDEX).ok());

    for (i, offset) in offsets.iter().enumerate() {
        let local = Placement {
            offset: *offset,
            rot: rots.as_ref().map_or(0.0, |r| r[i]),
            scale: scales.as_ref().map_or(Vec2(1.0, 1.0), |s| s[i]),
            // Instance tint is multiplicative: fall back to neutral white so
            // the base color applies once, at the leaf elements.
            tint: tinted(
                element_color(inst, i, Color::new(1.0, 1.0, 1.0, 1.0)),
                element_alpha(inst, i),
                Color::new(1.0, 1.0, 1.0, 1.0),
            ),
        };
        let combined = compose(placement, local);
        let source = select_instance_source(sources, source_indices, i);
        raster_geometry(
            source,
            combined,
            depth + 1,
            canvas,
            element_style(style, inst, i),
        );
    }
}

fn select_instance_source<'a>(
    sources: &'a [Arc<Geometry>],
    source_indices: Option<&[i32]>,
    instance_index: usize,
) -> &'a Geometry {
    let source_index = source_indices.map_or(0, |indices| indices[instance_index].max(0) as usize);
    sources[source_index.min(sources.len() - 1)].as_ref()
}

/// Composes an outer placement with an instance-local one (outer ∘ local).
fn compose(outer: Placement, local: Placement) -> Placement {
    Placement {
        offset: outer.apply(local.offset),
        rot: outer.rot + local.rot,
        scale: Vec2(outer.scale.0 * local.scale.0, outer.scale.1 * local.scale.1),
        tint: Color::new(
            outer.tint.r * local.tint.r,
            outer.tint.g * local.tint.g,
            outer.tint.b * local.tint.b,
            outer.tint.a * local.tint.a,
        ),
    }
}

fn raster_points(
    geo: &Geometry,
    positions: &[Vec2],
    placement: Placement,
    canvas: &mut Canvas<'_>,
    style: Style,
) {
    let (width, height) = (canvas.width, canvas.height);
    let points = geo.points();
    let radii = float_column(points, names::PSCALE);
    let sprite_mask = path_vertex_mask(geo, positions.len());

    for (i, p) in positions.iter().enumerate() {
        if sprite_mask[i] {
            continue;
        }
        let center = placement.apply(*p);
        let radius =
            radii.as_ref().map_or(DEFAULT_POINT_RADIUS, |r| r[i]) * placement.uniform_scale();
        if radius <= 0.0 {
            continue;
        }
        let color = tinted(
            element_color(points, i, style.color),
            element_alpha(points, i),
            placement.tint,
        );

        let min_x = (center.0 - radius - 1.0).floor().max(0.0) as u32;
        let max_x = ((center.0 + radius + 1.0).ceil() as u32).min(width);
        let min_y = (center.1 - radius - 1.0).floor().max(0.0) as u32;
        let max_y = ((center.1 + radius + 1.0).ceil() as u32).min(height);

        for y in min_y..max_y {
            for x in min_x..max_x {
                let dx = x as f32 + 0.5 - center.0;
                let dy = y as f32 + 0.5 - center.1;
                let dist = (dx * dx + dy * dy).sqrt();
                // Analytic 1px-feather coverage.
                let cov = (radius - dist + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let idx = ((y * width + x) * 4) as usize;
                    blend_pixel(&mut canvas.pixels[idx..idx + 4], color, cov);
                }
            }
        }
    }
}

fn float_column(set: &AttributeSet, name: &str) -> Option<Vec<f32>> {
    set.get(name)
        .and_then(|c| c.as_f32(name).ok().map(<[f32]>::to_vec))
}

/// Resolve the node's base color: `Cd`/`alpha` attributes still win per
/// element; this is only the fallback for elements without them. Priority:
/// connected `color` input pin > `color` parameter > opaque white.
fn base_color(params: &ResolvedParams) -> Color {
    // The `color` pin is an `is_param` port: a connected color is already
    // overlaid onto this parameter by the evaluator (attribute > pin >
    // parameter, REQ-LAYER-008).
    let [r, g, b, a] = params.vec4_or("color", {
        let [r, g, b] = params.vec3_or("color", [1.0, 1.0, 1.0]);
        [r, g, b, 1.0]
    });
    Color::new(r, g, b, a)
}

/// The `dash` Detail attribute as zeno's alternating on/off run lengths,
/// scaled to device pixels. Empty means a solid stroke.
///
/// A pattern with a token that is not a number is refused whole rather than
/// filtered: dropping one entry swaps every following on for an off, which
/// looks like a bug in the dash rather than a typo in the pattern.
/// Upper bound on the runs a dash pattern may declare.
///
/// zeno walks the pattern for every stroked segment, so an unbounded list is
/// unbounded work on the render thread. Real patterns are a handful of runs;
/// anything past this is a paste accident.
const MAX_DASH_RUNS: usize = 64;

fn dash_pattern(detail: &AttributeSet, scale: f32) -> Vec<f32> {
    let Some(spec) = detail
        .get(names::DASH)
        .and_then(|column| column.as_str(names::DASH).ok())
        .and_then(<[String]>::first)
    else {
        return Vec::new();
    };
    let mut lengths = Vec::new();
    for token in spec.split([',', ' ', '\t']).filter(|t| !t.is_empty()) {
        let Ok(length) = token.parse::<f32>() else {
            tracing::warn!(
                pattern = spec,
                token,
                "dash pattern is not a list of numbers"
            );
            return Vec::new();
        };
        // A negative or non-finite run has no drawing meaning, and clamping
        // it to zero would silently turn `"-1,4"` into a *different* dash
        // than the one written. The whole pattern is dropped for the same
        // reason a non-numeric token drops it: a partly applied pattern
        // inverts the on/off runs and reads as a rasterizer bug.
        if !length.is_finite() || length < 0.0 {
            tracing::warn!(
                pattern = spec,
                token,
                "dash pattern has a run that is negative or not finite"
            );
            return Vec::new();
        }
        // A pattern long enough to matter is a mistake, not a design: zeno
        // walks it per stroke segment, so an unbounded list is an unbounded
        // cost on the render thread.
        if lengths.len() >= MAX_DASH_RUNS {
            tracing::warn!(
                pattern = spec,
                limit = MAX_DASH_RUNS,
                "dash pattern has more runs than the rasterizer accepts"
            );
            return Vec::new();
        }
        lengths.push(length * scale);
    }
    // An all-zero pattern has no "on" run at all; zeno would draw nothing,
    // where the user plainly meant "no dash".
    if lengths.iter().all(|length| *length <= 0.0) {
        return Vec::new();
    }
    lengths
}

fn detail_cap(detail: &AttributeSet) -> Cap {
    match attr_i32(detail, names::CAP, 0) {
        Some(names::CAP_BUTT) => Cap::Butt,
        Some(names::CAP_SQUARE) => Cap::Square,
        _ => Cap::Round,
    }
}

fn detail_join(detail: &AttributeSet) -> Join {
    match attr_i32(detail, names::JOIN, 0) {
        Some(names::JOIN_MITER) => Join::Miter,
        Some(names::JOIN_BEVEL) => Join::Bevel,
        _ => Join::Round,
    }
}

fn attr_i32(set: &AttributeSet, name: &str, index: usize) -> Option<i32> {
    set.get(name)?.as_i32(name).ok()?.get(index).copied()
}

fn attr_f32(set: &AttributeSet, name: &str, index: usize) -> Option<f32> {
    set.get(name)?.as_f32(name).ok()?.get(index).copied()
}

fn attr_color(set: &AttributeSet, name: &str, index: usize) -> Option<Color> {
    set.get(name)?.as_color(name).ok()?.get(index).copied()
}

fn attr_bool(set: &AttributeSet, name: &str, index: usize) -> Option<bool> {
    set.get(name)?.as_bool(name).ok()?.get(index).copied()
}

fn element_color(set: &AttributeSet, index: usize, fallback: Color) -> Color {
    attr_color(set, names::CD, index).unwrap_or(fallback)
}

fn element_alpha(set: &AttributeSet, index: usize) -> f32 {
    attr_f32(set, names::ALPHA, index).unwrap_or(1.0)
}

/// The style one element draws with: its own `fill` / `stroke_width` /
/// `stroke_color` attributes, falling back to the style it inherits (the
/// node's parameters, or an enclosing instance's attributes).
fn element_style<'a>(inherited: Style<'a>, set: &AttributeSet, index: usize) -> Style<'a> {
    Style {
        fill: attr_bool(set, names::FILL, index).unwrap_or(inherited.fill),
        stroke_width: attr_f32(set, names::STROKE_WIDTH, index).unwrap_or(inherited.stroke_width),
        color: inherited.color,
        stroke_color: attr_color(set, names::STROKE_COLOR, index).or(inherited.stroke_color),
        // Detail attributes: not narrowed per element, carried through.
        shape: inherited.shape,
    }
}

/// The two colors one element draws with: the fill color (`Cd` > the style's
/// base color) and the stroke color (`stroke_color` > the fill color), both
/// tinted by the element's `alpha` and the enclosing instances' tint.
fn element_colors(style: Style, set: &AttributeSet, index: usize, tint: Color) -> (Color, Color) {
    let alpha = element_alpha(set, index);
    let fill = tinted(element_color(set, index, style.color), alpha, tint);
    let stroke = style
        .stroke_color
        .map_or(fill, |color| tinted(color, alpha, tint));
    (fill, stroke)
}

fn tinted(color: Color, alpha: f32, tint: Color) -> Color {
    Color::new(
        color.r * tint.r,
        color.g * tint.g,
        color.b * tint.b,
        color.a * alpha * tint.a,
    )
}

/// Straight-alpha Porter-Duff src-over, matching the merge node convention.
fn blend_pixel(dst: &mut [f32], color: Color, coverage: f32) {
    let sa = color.a * coverage;
    if sa <= 0.0 {
        return;
    }
    let da = dst[3];
    let out_a = sa + da * (1.0 - sa);
    if out_a > 0.0 {
        for c in 0..3 {
            let s = [color.r, color.g, color.b][c];
            dst[c] = (s * sa + dst[c] * da * (1.0 - sa)) / out_a;
        }
    }
    dst[3] = out_a;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::geometry::AttributeArray;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::{FrameRate, Vec3};
    use ravel_gpu::ShaderManager;
    use std::sync::Arc;

    fn ctx(w: u32, h: u32) -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (w, h))
    }

    fn make_node(fill: bool, stroke_width: f32) -> Node {
        Node::new(NodeId::new(1), "rasterize")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_output("frame", DataTypeId::FRAME_BUFFER)
            .with_param("fill", ParameterValue::Bool(fill))
            .with_param("stroke_width", ParameterValue::Float(stroke_width))
    }

    fn pixel(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
        let idx = ((y * fb.width + x) * 4) as usize;
        fb.as_f32()[idx..idx + 4].try_into().unwrap()
    }

    /// Emits a fixed Geometry; stands in for upstream nodes.
    struct GeoSource(Geometry);

    impl NodeProcessor for GeoSource {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(self.0.clone()))
        }
    }

    /// Evaluate a rasterize node fed by `geo` through a real evaluator.
    fn evaluate(
        node: &Node,
        proc: Arc<dyn NodeProcessor>,
        geo: &Geometry,
        ctx: &EvalContext,
    ) -> Arc<dyn NodeData> {
        let graph = Graph::new()
            .add_node(
                Node::new(NodeId::new(2), "test.source").with_output("out", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_node(node.clone())
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(2),
                OutputPortIndex(0),
                node.id,
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(2), Arc::new(GeoSource(geo.clone())));
        ev.register(node.id, proc);
        ev.evaluate(&graph, node.id, ctx).unwrap()
    }

    fn run(fill: bool, stroke_width: f32, geo: &Geometry, w: u32, h: u32) -> FrameBuffer {
        run_with_ctx(fill, stroke_width, geo, &ctx(w, h))
    }

    fn run_with_ctx(
        fill: bool,
        stroke_width: f32,
        geo: &Geometry,
        ctx: &EvalContext,
    ) -> FrameBuffer {
        let node = make_node(fill, stroke_width);
        let out = evaluate(
            &node,
            Arc::new(RasterizeProcessor::from_node(&node)),
            geo,
            ctx,
        );
        out.downcast_ref::<FrameBuffer>().unwrap().clone()
    }

    /// The CPU rasterizer is analytic and planar. A 3D geometry has to fail
    /// loudly — every `P` read below defaults to an empty slice, so without
    /// the guard it would quietly produce a blank frame.
    #[test]
    fn three_dimensional_positions_are_an_explicit_error() {
        let node = make_node(true, 0.0);

        let mut geo = Geometry::from_points3(vec![
            Vec3(0.0, 0.0, 0.0),
            Vec3(8.0, 0.0, 4.0),
            Vec3(8.0, 8.0, 4.0),
        ]);
        geo.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: true,
        });
        let error = rasterize_error(&node, geo);
        assert!(
            error.contains("rasterize requires 2D positions") && error.contains("Vec3"),
            "the message has to name the operation and the dimension: {error}"
        );

        // The same applies to an instance source nested under 2D instances.
        let mut placed = Geometry::new();
        placed
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))
            .unwrap();
        placed.set_instance_source(Some(Arc::new(Geometry::from_points3(vec![Vec3(
            0.0, 0.0, 1.0,
        )]))));
        assert!(rasterize_error(&node, placed).contains("rasterize requires 2D positions"));
    }

    /// Triangles belong to `scene.render`. This rasterizer would match no
    /// primitive and emit a blank frame, so a mesh is refused the same way a
    /// 3D position is — including one hidden in an instance source.
    #[test]
    fn mesh_primitives_are_an_explicit_error() {
        let node = make_node(true, 0.0);

        let mut geo = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(8.0, 0.0),
            Vec2(8.0, 8.0),
            Vec2(0.0, 8.0),
        ]);
        geo.push_mesh(0..4, &[0, 1, 2, 0, 2, 3]);
        let error = rasterize_error(&node, geo);
        assert!(
            error.contains("rasterize requires path primitives"),
            "the message has to name the operation and the primitive kind: {error}"
        );

        let mut placed = Geometry::new();
        placed
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))
            .unwrap();
        let mut source =
            Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0), Vec2(1.0, 1.0)]);
        source.push_mesh(0..3, &[0, 1, 2]);
        placed.set_instance_source(Some(Arc::new(source)));
        assert!(
            rasterize_error(&node, placed).contains("rasterize requires path primitives"),
            "a mesh nested in an instance source is refused too"
        );
    }

    fn rasterize_error(node: &Node, geo: Geometry) -> String {
        let graph = Graph::new()
            .add_node(
                Node::new(NodeId::new(2), "test.source").with_output("out", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_node(node.clone())
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(2),
                OutputPortIndex(0),
                node.id,
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(2), Arc::new(GeoSource(geo)));
        ev.register(
            node.id,
            Arc::new(RasterizeProcessor::from_node(node)) as Arc<dyn NodeProcessor>,
        );
        let Err(error) = ev.evaluate(&graph, node.id, &ctx(16, 16)) else {
            panic!("3D positions must not rasterize");
        };
        // The evaluator wraps the processor's error, so the reason a user sees
        // is the whole chain.
        let mut chain = vec![error.to_string()];
        let mut source = std::error::Error::source(&error);
        while let Some(current) = source {
            chain.push(current.to_string());
            source = current.source();
        }
        chain.join(": ")
    }

    fn run_gpu(
        gpu: &GpuContext,
        pool: &Arc<Mutex<TexturePool>>,
        geo: &Geometry,
        fill: bool,
        stroke_width: f32,
        ctx: &EvalContext,
    ) -> FrameBuffer {
        let node = make_node(fill, stroke_width);
        let mut shaders = ShaderManager::new(gpu.clone());
        let proc = RasterizeProcessor::new(gpu.clone(), &mut shaders, pool.clone(), &node);
        let out = evaluate(&node, Arc::new(proc), geo, ctx);
        out.downcast_ref::<GpuFrameBuffer>()
            .expect("GPU rasterize output stays resident")
            .to_frame_buffer()
            .expect("GPU readback")
    }

    fn assert_equivalent(cpu: &FrameBuffer, gpu: &FrameBuffer, label: &str) {
        assert_equivalent_with(cpu, gpu, label, 0.99);
    }

    fn assert_equivalent_with(
        cpu: &FrameBuffer,
        gpu: &FrameBuffer,
        label: &str,
        min_match_ratio: f32,
    ) {
        assert_eq!((cpu.width, cpu.height), (gpu.width, gpu.height));
        let pixel_count = (cpu.width * cpu.height) as usize;
        let cpu_data = cpu.as_f32();
        let gpu_data = gpu.as_f32();
        let matching = cpu_data
            .chunks_exact(4)
            .zip(gpu_data.chunks_exact(4))
            .filter(|(a, b)| a.iter().zip(*b).all(|(x, y)| (x - y).abs() < 0.1))
            .count();
        let match_ratio = matching as f32 / pixel_count as f32;
        let cpu_coverage: f32 = cpu_data.iter().skip(3).step_by(4).sum();
        let gpu_coverage: f32 = gpu_data.iter().skip(3).step_by(4).sum();
        let coverage_delta = (cpu_coverage - gpu_coverage).abs() / cpu_coverage.max(1.0);
        eprintln!(
            "{label}: {:.3}% pixels within 0.1, coverage delta {:.3}%",
            match_ratio * 100.0,
            coverage_delta * 100.0
        );
        assert!(
            match_ratio > min_match_ratio,
            "{label}: only {:.3}% pixels within tolerance",
            match_ratio * 100.0
        );
        assert!(
            coverage_delta < 0.02,
            "{label}: coverage differs by {:.3}% (CPU {cpu_coverage}, GPU {gpu_coverage})",
            coverage_delta * 100.0
        );
    }

    fn square_geo(color: Color) -> Geometry {
        let mut geo = Geometry::from_points(vec![
            Vec2(4.0, 4.0),
            Vec2(12.0, 4.0),
            Vec2(12.0, 12.0),
            Vec2(4.0, 12.0),
        ]);
        geo.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        geo.primitive_attrs_mut()
            .insert(names::CD, AttributeArray::Color(vec![color]))
            .unwrap();
        geo
    }

    #[test]
    fn filled_path_covers_interior_not_exterior() {
        let geo = square_geo(Color::new(1.0, 0.0, 0.0, 1.0));
        let fb = run(true, 0.0, &geo, 16, 16);

        let inside = pixel(&fb, 8, 8);
        assert!(inside[3] > 0.9, "interior should be covered: {inside:?}");
        assert!(inside[0] > 0.9 && inside[1] < 0.1, "fill uses Cd");

        let outside = pixel(&fb, 1, 1);
        assert!(outside[3] < 1e-6, "exterior stays transparent");
    }

    /// The CPU path reuses one coverage mask for every primitive, so a
    /// primitive that leaves the mask dirty would stamp its own silhouette,
    /// in the *next* primitive's colour, wherever the next one does not
    /// reach. Two disjoint squares of different colours catch exactly that:
    /// each keeps its own colour and the gap between them stays empty.
    #[test]
    fn a_reused_coverage_mask_does_not_leak_between_primitives() {
        let mut geo = Geometry::from_points(vec![
            Vec2(2.0, 2.0),
            Vec2(6.0, 2.0),
            Vec2(6.0, 6.0),
            Vec2(2.0, 6.0),
            Vec2(10.0, 10.0),
            Vec2(14.0, 10.0),
            Vec2(14.0, 14.0),
            Vec2(10.0, 14.0),
        ]);
        geo.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        geo.push_primitive(Primitive::Path {
            verts: 4..8,
            closed: true,
        });
        geo.primitive_attrs_mut()
            .insert(
                names::CD,
                AttributeArray::Color(vec![
                    Color::new(1.0, 0.0, 0.0, 1.0),
                    Color::new(0.0, 0.0, 1.0, 1.0),
                ]),
            )
            .unwrap();

        let fb = run(true, 0.0, &geo, 16, 16);
        let red = pixel(&fb, 4, 4);
        assert!(
            red[0] > 0.9 && red[2] < 0.1,
            "the first square keeps its own colour: {red:?}"
        );
        let blue = pixel(&fb, 12, 12);
        assert!(
            blue[2] > 0.9 && blue[0] < 0.1,
            "the second square keeps its own colour: {blue:?}"
        );
        for (x, y) in [(4, 12), (12, 4), (8, 8)] {
            let gap = pixel(&fb, x, y);
            assert!(gap[3] < 1e-6, "nothing is drawn at ({x},{y}): {gap:?}");
        }
    }

    #[test]
    fn composition_coordinates_scale_position_size_stroke_and_pscale() {
        let scaled_ctx = ctx(64, 64).with_comp_resolution((128, 128));
        let mut rect = Geometry::from_points(vec![
            Vec2(48.0, 48.0),
            Vec2(80.0, 48.0),
            Vec2(80.0, 80.0),
            Vec2(48.0, 80.0),
        ]);
        rect.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });

        let fill = run_with_ctx(true, 0.0, &rect, &scaled_ctx);
        assert!(
            pixel(&fill, 32, 32)[3] > 0.9,
            "rect center lands at canvas center"
        );
        assert!(
            pixel(&fill, 25, 32)[3] > 0.9,
            "scaled rect interior is covered"
        );
        assert!(
            pixel(&fill, 15, 32)[3] < 0.01,
            "rect position and size are scaled"
        );

        let stroke = run_with_ctx(false, 8.0, &rect, &scaled_ctx);
        assert!(
            pixel(&stroke, 22, 32)[3] > 0.5,
            "8 comp-pixel stroke scales to 4 pixels"
        );
        assert!(
            pixel(&stroke, 20, 32)[3] < 0.1,
            "stroke does not retain comp-space width"
        );

        let mut point = Geometry::from_points(vec![Vec2(96.0, 64.0)]);
        point
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![8.0]))
            .unwrap();
        let sprite = run_with_ctx(true, 0.0, &point, &scaled_ctx);
        assert!(pixel(&sprite, 48, 32)[3] > 0.9, "point position is scaled");
        assert!(pixel(&sprite, 51, 32)[3] > 0.5, "pscale radius is scaled");
        assert!(
            pixel(&sprite, 53, 32)[3] < 0.1,
            "pscale does not retain comp-space radius"
        );
    }

    #[test]
    fn stroke_only_leaves_interior_empty() {
        let geo = square_geo(Color::new(0.0, 1.0, 0.0, 1.0));
        let fb = run(false, 2.0, &geo, 16, 16);

        let edge = pixel(&fb, 8, 4);
        assert!(edge[3] > 0.5, "stroke covers the edge: {edge:?}");
        let inside = pixel(&fb, 8, 8);
        assert!(inside[3] < 0.1, "interior not filled: {inside:?}");
    }

    /// Two 8x8 squares side by side: primitive 0 spans (4,4)-(12,12),
    /// primitive 1 spans (20,4)-(28,12). Per-primitive style attributes
    /// address them independently.
    fn two_squares() -> Geometry {
        let mut geo = Geometry::from_points(vec![
            Vec2(4.0, 4.0),
            Vec2(12.0, 4.0),
            Vec2(12.0, 12.0),
            Vec2(4.0, 12.0),
            Vec2(20.0, 4.0),
            Vec2(28.0, 4.0),
            Vec2(28.0, 12.0),
            Vec2(20.0, 12.0),
        ]);
        geo.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        geo.push_primitive(Primitive::Path {
            verts: 4..8,
            closed: true,
        });
        geo
    }

    /// The point of the whole unit: one `rasterize` node, two elements, two
    /// stroke widths. The parameter is the fallback, not the width.
    #[test]
    fn per_element_stroke_width_overrides_the_parameter() {
        let mut geo = two_squares();
        geo.primitive_attrs_mut()
            .insert(names::STROKE_WIDTH, AttributeArray::F32(vec![2.0, 6.0]))
            .unwrap();

        let fb = run(false, 2.0, &geo, 32, 16);
        assert!(pixel(&fb, 4, 8)[3] > 0.9, "the 2px stroke covers its edge");
        assert!(
            pixel(&fb, 2, 8)[3] < 0.1,
            "the 2px stroke stops 2px short of the edge"
        );
        assert!(
            pixel(&fb, 18, 8)[3] > 0.9,
            "the 6px stroke reaches 2px outside its edge"
        );
        assert!(
            pixel(&fb, 16, 8)[3] < 0.1,
            "and no further than half its width"
        );
    }

    #[test]
    fn per_element_fill_attribute_overrides_the_parameter() {
        let mut geo = two_squares();
        geo.primitive_attrs_mut()
            .insert(names::FILL, AttributeArray::Bool(vec![true, false]))
            .unwrap();

        let fb = run(true, 0.0, &geo, 32, 16);
        assert!(pixel(&fb, 8, 8)[3] > 0.9, "the first square still fills");
        assert!(
            pixel(&fb, 24, 8)[3] < 1e-6,
            "the second one is switched off by its attribute"
        );
    }

    #[test]
    fn fill_and_stroke_take_different_colors() {
        let mut geo = square_geo(Color::new(1.0, 0.0, 0.0, 1.0));
        geo.primitive_attrs_mut()
            .insert(
                names::STROKE_COLOR,
                AttributeArray::Color(vec![Color::new(0.0, 0.0, 1.0, 1.0)]),
            )
            .unwrap();

        let fb = run(true, 4.0, &geo, 16, 16);
        let edge = pixel(&fb, 4, 8);
        assert!(
            edge[2] > 0.9 && edge[0] < 0.1,
            "the outline takes stroke_color: {edge:?}"
        );
        let inside = pixel(&fb, 8, 8);
        assert!(
            inside[0] > 0.9 && inside[2] < 0.1,
            "the interior keeps Cd: {inside:?}"
        );
    }

    /// Without `stroke_color` the outline draws in `Cd`, which is what the
    /// rasterizer did before the stroke had a color of its own.
    #[test]
    fn stroke_without_stroke_color_falls_back_to_cd() {
        let geo = square_geo(Color::new(0.0, 1.0, 0.0, 1.0));

        let fb = run(false, 4.0, &geo, 16, 16);
        let edge = pixel(&fb, 4, 8);
        assert!(
            edge[1] > 0.9 && edge[0] < 0.1 && edge[2] < 0.1,
            "the outline is Cd green: {edge:?}"
        );
    }

    /// An instance narrows the style for everything it expands, so a
    /// `scatter` that modulates `stroke_width` on the Instance domain reaches
    /// the source geometry's paths.
    #[test]
    fn instance_style_attributes_reach_the_expanded_source() {
        let mut source = Geometry::from_points(vec![
            Vec2(-4.0, -4.0),
            Vec2(4.0, -4.0),
            Vec2(4.0, 4.0),
            Vec2(-4.0, 4.0),
        ]);
        source.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });

        let mut geo = Geometry::new();
        geo.set_instance_source(Some(Arc::new(source)));
        geo.instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(8.0, 8.0), Vec2(24.0, 8.0)]),
            )
            .unwrap();
        geo.instances_mut()
            .insert(names::STROKE_WIDTH, AttributeArray::F32(vec![0.0, 6.0]))
            .unwrap();

        let fb = run(false, 0.0, &geo, 32, 16);
        assert!(
            pixel(&fb, 4, 8)[3] < 1e-6,
            "the first instance keeps the parameter's zero width"
        );
        assert!(
            pixel(&fb, 18, 8)[3] > 0.9,
            "the second instance strokes with its own width"
        );
    }

    #[test]
    fn point_sprite_uses_pscale_cd_alpha() {
        let mut geo = Geometry::from_points(vec![Vec2(8.0, 8.0)]);
        geo.points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![3.0]))
            .unwrap();
        geo.points_mut()
            .insert(
                names::CD,
                AttributeArray::Color(vec![Color::new(0.0, 0.0, 1.0, 1.0)]),
            )
            .unwrap();
        geo.points_mut()
            .insert(names::ALPHA, AttributeArray::F32(vec![0.5]))
            .unwrap();
        let fb = run(true, 0.0, &geo, 16, 16);

        let center = pixel(&fb, 8, 8);
        assert!(center[2] > 0.9, "sprite uses Cd: {center:?}");
        assert!(
            (center[3] - 0.5).abs() < 0.05,
            "alpha attribute respected: {center:?}"
        );
        let outside = pixel(&fb, 14, 8);
        assert!(outside[3] < 1e-6, "outside radius transparent");
    }

    #[test]
    fn path_vertices_do_not_draw_sprites() {
        // Square path over verts 0..4 plus one loose point: the path fills,
        // the loose point draws a sprite, and the path corners get no dots.
        let mut geo = Geometry::from_points(vec![
            Vec2(4.0, 4.0),
            Vec2(12.0, 4.0),
            Vec2(12.0, 12.0),
            Vec2(4.0, 12.0),
            Vec2(14.0, 14.0),
        ]);
        geo.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        let fb = run(true, 0.0, &geo, 16, 16);

        assert!(pixel(&fb, 8, 8)[3] > 0.9, "path fill intact");
        assert!(pixel(&fb, 14, 14)[3] > 0.5, "loose point still draws");
        // Just outside the top-left corner: a vertex sprite (r=2 at (4,4))
        // would cover this pixel; the fill does not.
        assert!(
            pixel(&fb, 2, 2)[3] < 1e-6,
            "no sprite at path vertex: {:?}",
            pixel(&fb, 2, 2)
        );
    }

    #[test]
    fn instances_expand_source_with_transform() {
        let mut source = Geometry::from_points(vec![Vec2(0.0, 0.0)]);
        source
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![2.0]))
            .unwrap();

        let mut geo = Geometry::new();
        geo.set_instance_source(Some(Arc::new(source)));
        geo.instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(4.0, 4.0), Vec2(12.0, 12.0)]),
            )
            .unwrap();

        let fb = run(true, 0.0, &geo, 16, 16);
        assert!(pixel(&fb, 4, 4)[3] > 0.5, "first instance drawn");
        assert!(pixel(&fb, 12, 12)[3] > 0.5, "second instance drawn");
        assert!(pixel(&fb, 8, 8)[3] < 1e-6, "no stray coverage between");
    }

    fn colored_point_source(color: Color) -> Arc<Geometry> {
        let mut source = Geometry::from_points(vec![Vec2(0.0, 0.0)]);
        source
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![2.0]))
            .unwrap();
        source
            .points_mut()
            .insert(names::CD, AttributeArray::Color(vec![color]))
            .unwrap();
        Arc::new(source)
    }

    fn two_source_instances(source_indices: Option<Vec<i32>>) -> Geometry {
        let red = colored_point_source(Color::new(1.0, 0.0, 0.0, 1.0));
        let blue = colored_point_source(Color::new(0.0, 0.0, 1.0, 1.0));
        let mut geo = Geometry::new();
        geo.set_instance_sources(vec![red, blue]);
        geo.instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(5.0, 8.0), Vec2(15.0, 8.0), Vec2(25.0, 8.0)]),
            )
            .unwrap();
        if let Some(source_indices) = source_indices {
            geo.instances_mut()
                .insert(names::SOURCE_INDEX, AttributeArray::I32(source_indices))
                .unwrap();
        }
        geo
    }

    #[test]
    fn instances_select_source_per_source_index() {
        let geo = two_source_instances(Some(vec![0, 1, 0]));
        let fb = run(true, 0.0, &geo, 32, 16);

        let first = pixel(&fb, 5, 8);
        let second = pixel(&fb, 15, 8);
        let third = pixel(&fb, 25, 8);
        assert!(
            first[0] > 0.9 && first[2] < 0.1,
            "first uses source 0: {first:?}"
        );
        assert!(
            second[2] > 0.9 && second[0] < 0.1,
            "second uses source 1: {second:?}"
        );
        assert!(
            third[0] > 0.9 && third[2] < 0.1,
            "third uses source 0: {third:?}"
        );
    }

    #[test]
    fn missing_source_index_matches_single_source_path() {
        let plural = two_source_instances(None);
        let mut single = plural.clone();
        single.set_instance_source(plural.instance_source().cloned());

        let plural_frame = run(true, 0.0, &plural, 32, 16);
        let single_frame = run(true, 0.0, &single, 32, 16);
        assert_eq!(plural_frame.as_f32(), single_frame.as_f32());
    }

    #[test]
    fn source_index_clamps_to_valid_range() {
        let geo = two_source_instances(Some(vec![-7, 99, 0]));
        let fb = run(true, 0.0, &geo, 32, 16);

        let below = pixel(&fb, 5, 8);
        let above = pixel(&fb, 15, 8);
        assert!(
            below[0] > 0.9 && below[2] < 0.1,
            "negative clamps to source 0: {below:?}"
        );
        assert!(
            above[2] > 0.9 && above[0] < 0.1,
            "large index clamps to last source: {above:?}"
        );
    }

    #[test]
    fn instance_scale_grows_sprite_radius() {
        let source = Geometry::from_points(vec![Vec2(0.0, 0.0)]);

        let mut geo = Geometry::new();
        geo.set_instance_source(Some(Arc::new(source)));
        geo.instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(8.0, 8.0)]))
            .unwrap();
        geo.instances_mut()
            .insert(names::SCALE, AttributeArray::Vec2(vec![Vec2(3.0, 3.0)]))
            .unwrap();

        // Default radius 2.0 × scale 3.0 = 6.0 → pixel at distance 5 covered.
        let fb = run(true, 0.0, &geo, 16, 16);
        assert!(pixel(&fb, 13, 8)[3] > 0.5, "scaled sprite reaches r=5");
    }

    #[test]
    fn over_blend_is_straight_alpha() {
        let mut geo = Geometry::from_points(vec![Vec2(8.0, 8.0), Vec2(8.0, 8.0)]);
        geo.points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![4.0, 4.0]))
            .unwrap();
        geo.points_mut()
            .insert(
                names::CD,
                AttributeArray::Color(vec![
                    Color::new(1.0, 0.0, 0.0, 1.0),
                    Color::new(0.0, 1.0, 0.0, 1.0),
                ]),
            )
            .unwrap();
        geo.points_mut()
            .insert(names::ALPHA, AttributeArray::F32(vec![1.0, 0.5]))
            .unwrap();

        let fb = run(true, 0.0, &geo, 16, 16);
        let c = pixel(&fb, 8, 8);
        // Second (green, a=0.5) over first (red, a=1) → half red, half green.
        assert!(
            (c[0] - 0.5).abs() < 0.05 && (c[1] - 0.5).abs() < 0.05,
            "{c:?}"
        );
        assert!((c[3] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn gpu_matches_cpu_for_paths_points_and_nested_instances() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));

        // Non-zero winding on a self-intersecting closed path.
        let mut bowtie = Geometry::from_points(vec![
            Vec2(8.0, 8.0),
            Vec2(32.0, 32.0),
            Vec2(8.0, 32.0),
            Vec2(32.0, 8.0),
        ]);
        bowtie.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        bowtie
            .primitive_attrs_mut()
            .insert(
                names::CD,
                AttributeArray::Color(vec![Color::new(0.8, 0.2, 0.1, 1.0)]),
            )
            .unwrap();
        let cpu = run(true, 0.0, &bowtie, 40, 40);
        let gpu_frame = run_gpu(&gpu, &pool, &bowtie, true, 0.0, &ctx(40, 40));
        assert_equivalent(&cpu, &gpu_frame, "self-intersecting path");

        // Closed paths fill; open paths only stroke.
        let mut paths = Geometry::from_points(vec![
            Vec2(6.0, 6.0),
            Vec2(26.0, 6.0),
            Vec2(26.0, 26.0),
            Vec2(6.0, 26.0),
            Vec2(36.0, 8.0),
            Vec2(56.0, 16.0),
            Vec2(40.0, 28.0),
        ]);
        paths.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        paths.push_primitive(Primitive::Path {
            verts: 4..7,
            closed: false,
        });
        paths
            .primitive_attrs_mut()
            .insert(
                names::CD,
                AttributeArray::Color(vec![
                    Color::new(0.1, 0.7, 0.2, 1.0),
                    Color::new(0.2, 0.3, 0.9, 1.0),
                ]),
            )
            .unwrap();
        let cpu = run(true, 2.0, &paths, 64, 36);
        let gpu_frame = run_gpu(&gpu, &pool, &paths, true, 2.0, &ctx(64, 36));
        assert_equivalent(&cpu, &gpu_frame, "closed and open paths");

        // Two instance levels exercise P/rot/scale/Cd/alpha while the source
        // point varies pscale/Cd/alpha.
        let mut point = Geometry::from_points(vec![Vec2(0.0, 0.0)]);
        point
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![3.0]))
            .unwrap();
        point
            .points_mut()
            .insert(
                names::CD,
                AttributeArray::Color(vec![Color::new(0.5, 0.8, 1.0, 1.0)]),
            )
            .unwrap();
        point
            .points_mut()
            .insert(names::ALPHA, AttributeArray::F32(vec![0.8]))
            .unwrap();
        let mut inner = Geometry::new();
        inner.set_instance_source(Some(Arc::new(point)));
        inner
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(-5.0, 0.0), Vec2(5.0, 0.0)]),
            )
            .unwrap();
        inner
            .instances_mut()
            .insert(names::ROT, AttributeArray::F32(vec![0.2, -0.3]))
            .unwrap();
        inner
            .instances_mut()
            .insert(
                names::SCALE,
                AttributeArray::Vec2(vec![Vec2(1.0, 1.0), Vec2(1.5, 0.75)]),
            )
            .unwrap();
        inner
            .instances_mut()
            .insert(
                names::CD,
                AttributeArray::Color(vec![
                    Color::new(1.0, 0.5, 0.5, 1.0),
                    Color::new(0.5, 1.0, 0.5, 1.0),
                ]),
            )
            .unwrap();
        inner
            .instances_mut()
            .insert(names::ALPHA, AttributeArray::F32(vec![0.7, 1.0]))
            .unwrap();
        let mut outer = Geometry::new();
        outer.set_instance_source(Some(Arc::new(inner)));
        outer
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(18.0, 20.0), Vec2(46.0, 40.0)]),
            )
            .unwrap();
        outer
            .instances_mut()
            .insert(names::ROT, AttributeArray::F32(vec![0.0, 0.6]))
            .unwrap();
        outer
            .instances_mut()
            .insert(
                names::SCALE,
                AttributeArray::Vec2(vec![Vec2(1.0, 1.0), Vec2(0.8, 1.2)]),
            )
            .unwrap();
        let cpu = run(true, 0.0, &outer, 64, 64);
        let gpu_frame = run_gpu(&gpu, &pool, &outer, true, 0.0, &ctx(64, 64));
        assert_equivalent(&cpu, &gpu_frame, "nested instances");

        let multiple_sources = two_source_instances(Some(vec![0, 1, 0]));
        let cpu = run(true, 0.0, &multiple_sources, 32, 16);
        let gpu_frame = run_gpu(&gpu, &pool, &multiple_sources, true, 0.0, &ctx(32, 16));
        assert_equivalent(&cpu, &gpu_frame, "multiple instance sources");

        let scaled_ctx = ctx(20, 20).with_comp_resolution((40, 40));
        let cpu = run_with_ctx(true, 0.0, &bowtie, &scaled_ctx);
        let gpu_frame = run_gpu(&gpu, &pool, &bowtie, true, 0.0, &scaled_ctx);
        assert_equivalent(&cpu, &gpu_frame, "scaled composition coordinates");
    }

    /// Per-element style has to hold the CPU/GPU agreement the rest of the
    /// rasterizer keeps: the two paths draw the fill and the stroke in
    /// different orders (CPU blends twice, the shader composites once), so a
    /// second color is exactly where they could drift apart.
    #[test]
    fn gpu_matches_cpu_for_per_element_style() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));

        let mut geo = two_squares();
        geo.primitive_attrs_mut()
            .insert(
                names::CD,
                AttributeArray::Color(vec![
                    Color::new(0.9, 0.2, 0.1, 1.0),
                    Color::new(0.1, 0.8, 0.3, 1.0),
                ]),
            )
            .unwrap();
        geo.primitive_attrs_mut()
            .insert(
                names::STROKE_COLOR,
                AttributeArray::Color(vec![
                    Color::new(0.1, 0.3, 1.0, 1.0),
                    Color::new(1.0, 1.0, 0.2, 0.6),
                ]),
            )
            .unwrap();
        geo.primitive_attrs_mut()
            .insert(names::STROKE_WIDTH, AttributeArray::F32(vec![2.0, 6.0]))
            .unwrap();
        geo.primitive_attrs_mut()
            .insert(names::FILL, AttributeArray::Bool(vec![true, false]))
            .unwrap();

        let cpu = run(true, 1.0, &geo, 64, 32);
        let gpu_frame = run_gpu(&gpu, &pool, &geo, true, 1.0, &ctx(64, 32));
        assert_equivalent(&cpu, &gpu_frame, "per-element style");
    }

    /// A closed smooth blob (fill) plus an open mixed corner/smooth path
    /// (stroke), carrying `in_tan` / `out_tan` point attributes
    /// (REQ-UI-011 unit 6).
    fn curved_geo() -> Geometry {
        let mut geo = Geometry::from_points(vec![
            // Closed blob: apex + two base points, all smooth.
            Vec2(20.0, 6.0),
            Vec2(34.0, 20.0),
            Vec2(6.0, 20.0),
            // Open stroke: straight corner segment into a curve.
            Vec2(6.0, 30.0),
            Vec2(20.0, 30.0),
            Vec2(34.0, 30.0),
        ]);
        geo.points_mut()
            .insert(
                names::IN_TAN,
                AttributeArray::Vec2(vec![
                    Vec2(-10.0, 0.0),
                    Vec2(0.0, -8.0),
                    Vec2(0.0, -8.0),
                    Vec2(0.0, 0.0),
                    Vec2(-6.0, -6.0),
                    Vec2(0.0, 0.0),
                ]),
            )
            .unwrap();
        geo.points_mut()
            .insert(
                names::OUT_TAN,
                AttributeArray::Vec2(vec![
                    Vec2(10.0, 0.0),
                    Vec2(0.0, 8.0),
                    Vec2(0.0, 8.0),
                    Vec2(0.0, 0.0),
                    Vec2(6.0, 6.0),
                    Vec2(0.0, 0.0),
                ]),
            )
            .unwrap();
        geo.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: true,
        });
        geo.push_primitive(Primitive::Path {
            verts: 3..6,
            closed: false,
        });
        geo
    }

    #[test]
    fn tangent_attributes_curve_the_rendered_path() {
        let coverage = |fb: &FrameBuffer| fb.as_f32().iter().skip(3).step_by(4).sum::<f32>();

        let curved = curved_geo();
        let curved_fb = run(true, 0.0, &curved, 40, 40);

        // Same control polygon without tangent attributes renders straight.
        let mut straight = Geometry::from_points(vec![
            Vec2(20.0, 6.0),
            Vec2(34.0, 20.0),
            Vec2(6.0, 20.0),
            Vec2(6.0, 30.0),
            Vec2(20.0, 30.0),
            Vec2(34.0, 30.0),
        ]);
        straight.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: true,
        });
        straight.push_primitive(Primitive::Path {
            verts: 3..6,
            closed: false,
        });
        let straight_fb = run(true, 0.0, &straight, 40, 40);

        let curved_cov = coverage(&curved_fb);
        let straight_cov = coverage(&straight_fb);
        let delta = (curved_cov - straight_cov).abs() / straight_cov.max(1.0);
        assert!(
            delta > 0.05,
            "tangents must change coverage (curved {curved_cov}, straight {straight_cov})"
        );
        // The blob's center is filled.
        assert!(pixel(&curved_fb, 20, 14)[3] > 0.5);
    }

    #[test]
    fn gpu_matches_cpu_for_curved_paths() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
        let geo = curved_geo();
        let cpu = run(true, 2.0, &geo, 40, 40);
        let gpu_frame = run_gpu(&gpu, &pool, &geo, true, 2.0, &ctx(40, 40));
        // A flattened curve concentrates many short edges, so the zeno vs.
        // analytic-shader per-edge AA divergence shows on more pixels than
        // for straight paths; total coverage stays pinned to 2%.
        assert_equivalent_with(&cpu, &gpu_frame, "curved paths", 0.98);
    }

    /// Diagnostic twin of `gpu_matches_cpu_for_curved_paths`: the same
    /// content as an explicit polyline (no tangent attributes), isolating
    /// per-edge AA divergence from actual flattening mismatches.
    #[test]
    fn gpu_matches_cpu_for_flattened_polyline() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
        let source = curved_geo();
        let positions = source
            .points()
            .get(names::P)
            .and_then(|c| c.as_vec2(names::P).ok())
            .unwrap()
            .to_vec();
        let in_tans = source
            .points()
            .get(names::IN_TAN)
            .and_then(|c| c.as_vec2(names::IN_TAN).ok())
            .unwrap()
            .to_vec();
        let out_tans = source
            .points()
            .get(names::OUT_TAN)
            .and_then(|c| c.as_vec2(names::OUT_TAN).ok())
            .unwrap()
            .to_vec();
        let mut all_points = Vec::new();
        let mut ranges = Vec::new();
        for (verts, closed) in [(0..3, true), (3..6, false)] {
            let start = all_points.len();
            let flat = crate::flatten::flatten_path(
                &positions[verts.clone()],
                Some(&in_tans[verts.clone()]),
                Some(&out_tans[verts.clone()]),
                closed,
            );
            all_points.extend(flat);
            ranges.push((start..all_points.len(), closed));
        }
        let mut geo = Geometry::from_points(all_points);
        for (verts, closed) in ranges {
            geo.push_primitive(Primitive::Path { verts, closed });
        }
        let cpu = run(true, 2.0, &geo, 40, 40);
        let gpu_frame = run_gpu(&gpu, &pool, &geo, true, 2.0, &ctx(40, 40));
        // Same looser per-pixel threshold as the tangent-attribute case.
        assert_equivalent_with(&cpu, &gpu_frame, "flattened polyline", 0.98);
    }

    /// Square path with no `Cd`/`alpha` attributes.
    fn plain_square_geo() -> Geometry {
        let mut geo = Geometry::from_points(vec![
            Vec2(4.0, 4.0),
            Vec2(12.0, 4.0),
            Vec2(12.0, 12.0),
            Vec2(4.0, 12.0),
        ]);
        geo.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        geo
    }

    /// Evaluate a rasterize node (with a `color` input port) fed by `geo`
    /// and optionally a Color on the `color` pin.
    fn run_with_color_pin(geo: &Geometry, pin: Option<Color>, node: &Node) -> FrameBuffer {
        struct ColorSource(Color);
        impl NodeProcessor for ColorSource {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn NodeData>>],
                _params: &ResolvedParams,
                _scope: &mut dyn EvalScope,
            ) -> anyhow::Result<Arc<dyn NodeData>> {
                Ok(Arc::new(self.0))
            }
        }

        let mut graph = Graph::new()
            .add_node(
                Node::new(NodeId::new(2), "test.source").with_output("out", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_node(node.clone())
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(2),
                OutputPortIndex(0),
                node.id,
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(2), Arc::new(GeoSource(geo.clone())));
        if let Some(color) = pin {
            graph = graph
                .add_node(
                    Node::new(NodeId::new(3), "test.color").with_output("out", DataTypeId::COLOR),
                )
                .unwrap()
                .add_edge(
                    EdgeId::new(2),
                    NodeId::new(3),
                    OutputPortIndex(0),
                    node.id,
                    InputPortIndex(1),
                )
                .unwrap();
            ev.register(NodeId::new(3), Arc::new(ColorSource(color)));
        }
        ev.register(node.id, Arc::new(RasterizeProcessor::from_node(node)));
        let out = ev.evaluate(&graph, node.id, &ctx(16, 16)).unwrap();
        out.downcast_ref::<FrameBuffer>().unwrap().clone()
    }

    /// Rasterize node with the template's `color` wiring: an `is_param`
    /// COLOR input backed by the `color` parameter (the evaluator overlays
    /// a connected pin onto the parameter).
    fn color_node_with(rgba: [f32; 4]) -> Node {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::graph::InputPort;
        let mut node = make_node(true, 0.0).with_param(
            "color",
            ParameterValue::Channel4(rgba.map(AnimationChannel::constant)),
        );
        node.inputs.push(InputPort {
            name: "color".into(),
            accepted_types: vec![DataTypeId::COLOR],
            is_param: true,
            is_variadic: false,
        });
        node
    }

    fn color_node() -> Node {
        color_node_with([1.0, 1.0, 1.0, 1.0])
    }

    #[test]
    fn color_pin_fills_geometry_without_cd() {
        let fb = run_with_color_pin(
            &plain_square_geo(),
            Some(Color::new(0.0, 0.25, 1.0, 0.5)),
            &color_node(),
        );
        let p = pixel(&fb, 8, 8);
        assert!(
            p[0] < 0.05 && (p[1] - 0.25).abs() < 0.05 && p[2] > 0.9,
            "{p:?}"
        );
        assert!((p[3] - 0.5).abs() < 0.05, "pin alpha applies: {p:?}");
    }

    #[test]
    fn color_parameter_used_when_pin_unconnected() {
        let node = color_node_with([0.0, 1.0, 0.0, 1.0]);
        let fb = run_with_color_pin(&plain_square_geo(), None, &node);
        let p = pixel(&fb, 8, 8);
        assert!(p[0] < 0.05 && p[1] > 0.9 && p[2] < 0.05, "{p:?}");
        assert!(p[3] > 0.9, "{p:?}");
    }

    #[test]
    fn cd_attribute_wins_over_color_pin() {
        let fb = run_with_color_pin(
            &square_geo(Color::new(1.0, 0.0, 0.0, 1.0)),
            Some(Color::new(0.0, 0.0, 1.0, 1.0)),
            &color_node(),
        );
        let p = pixel(&fb, 8, 8);
        assert!(p[0] > 0.9 && p[2] < 0.05, "Cd beats the pin: {p:?}");
    }

    #[test]
    fn default_color_stays_white_without_pin_or_parameter() {
        let fb = run_with_color_pin(&plain_square_geo(), None, &color_node());
        let p = pixel(&fb, 8, 8);
        assert!(
            p[0] > 0.9 && p[1] > 0.9 && p[2] > 0.9 && p[3] > 0.9,
            "{p:?}"
        );
    }

    #[test]
    fn gpu_output_is_resident_until_explicit_readback() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 64 * 1024 * 1024)));
        let node = Node::new(NodeId::new(2), "rasterize");
        let mut shaders = ShaderManager::new(gpu.clone());
        let proc = RasterizeProcessor::new(gpu.clone(), &mut shaders, pool, &node);
        let geo: Arc<dyn NodeData> = Arc::new(Geometry::from_points(vec![Vec2(8.0, 8.0)]));
        let before = gpu.transfer_stats();
        let mut scope = Evaluator::new();
        let out = proc
            .process(
                &node,
                &ctx(16, 16),
                &[Some(geo)],
                &ResolvedParams::default(),
                &mut scope,
            )
            .unwrap();
        assert!(out.downcast_ref::<GpuFrameBuffer>().is_some());
        let resident = gpu.transfer_stats();
        assert_eq!(resident.uploads, before.uploads);
        assert_eq!(resident.readbacks, before.readbacks);
        out.downcast_ref::<GpuFrameBuffer>()
            .unwrap()
            .to_frame_buffer()
            .unwrap();
        assert_eq!(gpu.transfer_stats().readbacks, before.readbacks + 1);
    }

    /// The real stale-viewer chain: rasterize draws, the upstream geometry
    /// node is deleted (a structural document edit that also strips its
    /// edges), the pull now fails with the missing-geometry error, and
    /// restoring the source draws again. The evaluator is rebuilt around
    /// each edit, mirroring the app's structural-hint handling.
    #[test]
    fn deleting_the_geometry_source_fails_rasterize_until_restored() {
        let geo = square_geo(Color::new(1.0, 1.0, 1.0, 1.0));
        let node = make_node(true, 0.0);
        let source =
            Node::new(NodeId::new(2), "test.source").with_output("out", DataTypeId::GEOMETRY);
        let graph = Graph::new()
            .add_node(source.clone())
            .unwrap()
            .add_node(node.clone())
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                source.id,
                OutputPortIndex(0),
                node.id,
                InputPortIndex(0),
            )
            .unwrap();
        let register = |ev: &mut Evaluator| {
            ev.register(source.id, Arc::new(GeoSource(geo.clone())));
            ev.register(node.id, Arc::new(RasterizeProcessor::from_node(&node)));
        };

        let mut ev = Evaluator::new();
        register(&mut ev);
        ev.evaluate(&graph, node.id, &ctx(16, 16))
            .expect("the connected graph draws");

        let edited = graph.remove_node(source.id).unwrap();
        let mut ev = Evaluator::new();
        ev.register(node.id, Arc::new(RasterizeProcessor::from_node(&node)));
        let err = match ev.evaluate(&edited, node.id, &ctx(16, 16)) {
            Ok(_) => panic!("the orphaned rasterize must fail, not draw stale content"),
            Err(err) => err,
        };
        match &err {
            ravel_core::eval::EvalError::ProcessFailed { source, .. } => assert!(
                format!("{source:#}").contains("Geometry input"),
                "unexpected process failure: {source:#}"
            ),
            other => panic!("expected a process failure, got {other}"),
        }

        let restored = edited
            .add_node(source.clone())
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                source.id,
                OutputPortIndex(0),
                node.id,
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        register(&mut ev);
        ev.evaluate(&restored, node.id, &ctx(16, 16))
            .expect("restoring the source draws again");
    }

    // ---- cap / join / dash (Detail attributes) -----------------------------

    /// A horizontal open path, so the caps sit at known pixels.
    fn horizontal_path(from: Vec2, to: Vec2) -> Geometry {
        let mut geo = Geometry::from_points(vec![from, to]);
        geo.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        geo
    }

    fn with_detail_i32(mut geo: Geometry, name: &str, value: i32) -> Geometry {
        geo.detail_mut()
            .insert(name, AttributeArray::I32(vec![value]))
            .unwrap();
        geo
    }

    fn alpha(fb: &FrameBuffer, x: u32, y: u32) -> f32 {
        pixel(fb, x, y)[3]
    }

    /// The three caps differ exactly where they are supposed to: past the end
    /// point on the axis (butt stops, round and square reach) and off the axis
    /// at the corner of the square (only the square covers it).
    #[test]
    fn each_cap_shapes_the_end_of_the_stroke() {
        let path = horizontal_path(Vec2(10.0, 16.0), Vec2(30.0, 16.0));
        let draw = |cap: i32| {
            run(
                false,
                6.0,
                &with_detail_i32(path.clone(), names::CAP, cap),
                40,
                32,
            )
        };

        let butt = draw(names::CAP_BUTT);
        let round = draw(names::CAP_ROUND);
        let square = draw(names::CAP_SQUARE);

        // On the axis, past the end point at (30, 16).
        assert!(
            alpha(&butt, 31, 16) < 0.01,
            "a butt cap stops at the end point"
        );
        assert!(alpha(&round, 31, 16) > 0.9, "a round cap reaches past it");
        assert!(alpha(&square, 31, 16) > 0.9, "so does a square cap");

        // Off the axis, at the corner of the square: outside the cap radius.
        assert!(alpha(&round, 32, 18) < 0.1, "the round cap has no corner");
        assert!(alpha(&square, 32, 18) > 0.9, "the square cap does");

        // All three draw the same body, so the difference really is the cap.
        for x in [16, 20, 24] {
            assert!(alpha(&butt, x, 16) > 0.9 && alpha(&round, x, 16) > 0.9);
        }
    }

    /// The three joins differ at the outside of a right-angle corner: the
    /// miter fills it to the point, the round arc cuts it back, and the bevel
    /// cuts it back furthest.
    #[test]
    fn each_join_shapes_the_corner_of_the_stroke() {
        let mut path =
            Geometry::from_points(vec![Vec2(10.0, 26.0), Vec2(10.0, 10.0), Vec2(26.0, 10.0)]);
        path.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: false,
        });
        let draw = |join: i32| {
            run(
                false,
                6.0,
                &with_detail_i32(path.clone(), names::JOIN, join),
                40,
                40,
            )
        };

        let miter = draw(names::JOIN_MITER);
        let round = draw(names::JOIN_ROUND);
        let bevel = draw(names::JOIN_BEVEL);

        // The miter point of a 90° corner is at (7, 7); the arc of radius 3
        // and the bevel chord both stop short of it.
        assert!(alpha(&miter, 7, 7) > 0.9, "the miter reaches its point");
        assert!(alpha(&round, 7, 7) < 0.1, "the arc does not");
        assert!(alpha(&bevel, 7, 7) < 0.1, "nor does the bevel");

        // Between the two that both cut the corner, the arc keeps more of it.
        let corner_coverage = |fb: &FrameBuffer| {
            (6..11)
                .flat_map(|y| (6..11).map(move |x| (x, y)))
                .map(|(x, y)| alpha(fb, x, y))
                .sum::<f32>()
        };
        assert!(
            corner_coverage(&round) > corner_coverage(&bevel) + 0.5,
            "round {} vs bevel {}",
            corner_coverage(&round),
            corner_coverage(&bevel)
        );
    }

    /// A miter reaches `half_width / sin(half_angle)` past its vertex — up to
    /// √2 half-widths before zeno bevels the corner — while the path's own
    /// bounds stop at the vertex. The CPU path blends a rectangle rather than
    /// the whole canvas, so a rectangle sized for the half-width alone leaves
    /// the tip of the spike in the shared coverage mask, where it becomes the
    /// next primitive's phantom silhouette (`Canvas::blend_coverage` asserts
    /// on exactly that).
    #[test]
    fn a_miter_spike_stays_inside_the_blended_rectangle() {
        // A right-angle corner whose bisector points along +x: the spike runs
        // 14px past a vertex the path bounds end at, against a 10px
        // half-width.
        let mut spike =
            Geometry::from_points(vec![Vec2(16.0, 6.0), Vec2(30.0, 20.0), Vec2(16.0, 34.0)]);
        spike.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: false,
        });
        let fb = run(
            false,
            20.0,
            &with_detail_i32(spike, names::JOIN, names::JOIN_MITER),
            60,
            40,
        );
        assert!(
            alpha(&fb, 42, 20) > 0.9,
            "the miter reaches past its vertex, or this test proves nothing"
        );
        assert!(alpha(&fb, 46, 20) < 0.01, "and it does end");
    }

    fn with_dash(mut geo: Geometry, pattern: &str, offset: f32) -> Geometry {
        geo.detail_mut()
            .insert(names::DASH, AttributeArray::Str(vec![pattern.to_owned()]))
            .unwrap();
        geo.detail_mut()
            .insert(names::DASH_OFFSET, AttributeArray::F32(vec![offset]))
            .unwrap();
        geo
    }

    /// The dash pattern turns the stroke on and off along the path, and the
    /// offset slides that rhythm.
    #[test]
    fn a_dash_pattern_breaks_the_stroke_into_runs() {
        let path = horizontal_path(Vec2(4.0, 16.0), Vec2(36.0, 16.0));
        let solid = run(false, 4.0, &path, 40, 32);
        let dashed = run(false, 4.0, &with_dash(path.clone(), "8,8", 0.0), 40, 32);
        let shifted = run(false, 4.0, &with_dash(path.clone(), "8,8", 8.0), 40, 32);

        assert!(
            alpha(&solid, 16, 16) > 0.9,
            "the solid stroke covers it all"
        );
        assert!(alpha(&dashed, 8, 16) > 0.9, "the first run is on");
        assert!(alpha(&dashed, 16, 16) < 0.01, "the second run is off");
        assert!(alpha(&dashed, 24, 16) > 0.9, "the third run is on again");
        assert_ne!(
            dashed.as_f32(),
            shifted.as_f32(),
            "the dash offset has to move the pattern"
        );

        // A pattern that is not a list of numbers is refused whole: a typo
        // draws the stroke it would have drawn, not a half-applied rhythm.
        let broken = run(false, 4.0, &with_dash(path.clone(), "8,x", 0.0), 40, 32);
        assert_eq!(broken.as_f32(), solid.as_f32());
        // So is an all-zero pattern, which zeno would otherwise draw as
        // nothing at all.
        let zeroed = run(false, 4.0, &with_dash(path.clone(), "0,0", 0.0), 40, 32);
        assert_eq!(zeroed.as_f32(), solid.as_f32());
    }

    /// A run that cannot be drawn drops the whole pattern rather than
    /// becoming a different one.
    ///
    /// Clamping `-1` to `0` would turn `"-1,4"` into `"0,4"` — a pattern the
    /// user did not write, drawn without a word. `NaN` and an infinity reach
    /// zeno unexamined otherwise, and an unbounded run count is unbounded
    /// work per stroked segment.
    #[test]
    fn a_dash_run_that_cannot_be_drawn_drops_the_whole_pattern() {
        let path = horizontal_path(Vec2(4.0, 16.0), Vec2(36.0, 16.0));
        let solid = run(false, 4.0, &path, 40, 32);
        // `"-1,4"` clamped to `"0,4"` is a real dash: it would *not* equal
        // the solid stroke, which is what makes this assertion bite.
        let huge = std::iter::repeat_n("1", MAX_DASH_RUNS + 1)
            .collect::<Vec<_>>()
            .join(",");
        for pattern in ["-1,4", "NaN,4", "inf,4", huge.as_str()] {
            let drawn = run(false, 4.0, &with_dash(path.clone(), pattern, 0.0), 40, 32);
            assert_eq!(
                drawn.as_f32(),
                solid.as_f32(),
                "pattern {pattern:?} must not draw a different dash"
            );
        }
    }

    /// A stroke shape the fragment shader cannot express routes the whole
    /// draw to the CPU rather than drawing something else on the GPU. The
    /// picture is therefore the CPU one, pixel for pixel.
    #[test]
    fn a_shape_the_shader_cannot_draw_falls_back_to_the_cpu() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let pool = Arc::new(Mutex::new(TexturePool::new(gpu.clone(), 16 * 1024 * 1024)));
        let path = horizontal_path(Vec2(4.0, 16.0), Vec2(36.0, 16.0));

        for (label, geo) in [
            ("dash", with_dash(path.clone(), "8,8", 0.0)),
            (
                "square cap",
                with_detail_i32(path.clone(), names::CAP, names::CAP_SQUARE),
            ),
            (
                "miter join",
                with_detail_i32(path.clone(), names::JOIN, names::JOIN_MITER),
            ),
        ] {
            let node = make_node(false, 4.0);
            let mut shaders = ShaderManager::new(gpu.clone());
            let proc = RasterizeProcessor::new(gpu.clone(), &mut shaders, pool.clone(), &node);
            let out = evaluate(&node, Arc::new(proc), &geo, &ctx(40, 32));
            let fallback = out
                .downcast_ref::<FrameBuffer>()
                .unwrap_or_else(|| panic!("{label}: the GPU rasterizer had to fall back"));
            assert_eq!(
                fallback.as_f32(),
                run(false, 4.0, &geo, 40, 32).as_f32(),
                "{label}: the fallback is the CPU picture"
            );
        }

        // A round stroke with no dash still takes the GPU path — the fallback
        // is the exception, not the new normal.
        let node = make_node(false, 4.0);
        let mut shaders = ShaderManager::new(gpu.clone());
        let proc = RasterizeProcessor::new(gpu.clone(), &mut shaders, pool.clone(), &node);
        let out = evaluate(&node, Arc::new(proc), &path, &ctx(40, 32));
        assert!(
            out.downcast_ref::<GpuFrameBuffer>().is_some(),
            "an unstyled stroke stays on the GPU"
        );
    }
}
