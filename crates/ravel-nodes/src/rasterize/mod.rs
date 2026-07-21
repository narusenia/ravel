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
use ravel_core::geometry::{AttributeSet, Geometry, Primitive, names};
use ravel_core::graph::Node;
use ravel_core::types::{Color, FrameBuffer, NodeData, Vec2};
use ravel_gpu::{
    ComputePipeline, GpuContext, GpuFrameBuffer, RasterPipeline, ShaderManager, TextureKey,
    TexturePool,
};
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;
use zeno::{Cap, Command, Fill, Join, Mask, Stroke, Vector};

use crate::composition_scale;
use crate::gpu_util;

const SHADER_SRC: &str = include_str!("../shaders/rasterize.wgsl");

/// Instance nesting guard: instances-of-instances beyond this depth are
/// skipped rather than recursed (spec limits stateful/sim nesting similarly).
const MAX_INSTANCE_DEPTH: u32 = 4;
const DEFAULT_POINT_RADIUS: f32 = 2.0;

/// Fill/stroke style resolved from the node's parameters for one process call.
#[derive(Clone, Copy)]
struct Style {
    fill: bool,
    stroke_width: f32,
    /// Base color for elements without `Cd`/`alpha` attributes: the `color`
    /// input pin when connected, else the `color` parameter (REQ-LAYER-008;
    /// attribute > pin > parameter priority).
    color: Color,
}

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

        let style = Style {
            fill: params.bool_or("fill", true),
            stroke_width: params.f32_or("stroke_width", 0.0),
            color: base_color(params),
        };

        if let Some(gpu) = &self.gpu {
            return gpu.rasterize(geo, style, ctx);
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

        raster_geometry(
            geo,
            Placement::for_context(ctx),
            0,
            &mut pixels,
            width,
            height,
            style,
        );

        Ok(Arc::new(FrameBuffer {
            width,
            height,
            data: pixels.into(),
        }))
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
    data0: [f32; 4],
    data1: [f32; 4],
}

struct GpuRasterizer {
    ctx: GpuContext,
    raster_pipeline: RasterPipeline,
    unpremultiply_pipeline: ComputePipeline,
    pool: Arc<Mutex<TexturePool>>,
}

impl GpuRasterizer {
    fn new(ctx: GpuContext, shaders: &mut ShaderManager, pool: Arc<Mutex<TexturePool>>) -> Self {
        let shader = shaders
            .compile_source("rasterize", SHADER_SRC)
            .expect("rasterize.wgsl compilation failed");
        let raster_layout = [
            buffer_layout_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT, true),
            buffer_layout_entry(1, wgpu::ShaderStages::FRAGMENT, false),
            buffer_layout_entry(2, wgpu::ShaderStages::VERTEX_FRAGMENT, false),
        ];
        let raster_pipeline = RasterPipeline::new(
            &ctx,
            &shader,
            "raster_vertex",
            "raster_fragment",
            &raster_layout,
            wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba16Float,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            },
        );
        let unpremultiply_layout = [texture_layout_entry(3), storage_texture_layout_entry(4)];
        let unpremultiply_pipeline = ComputePipeline::new(
            &ctx,
            &shader,
            "unpremultiply",
            &unpremultiply_layout,
            gpu_util::WORKGROUP_SIZE,
        );
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
        flatten_geometry(
            geo,
            Placement::for_context(ctx),
            0,
            style,
            &mut vertices,
            &mut items,
        );

        // Empty storage bindings still need a non-zero-sized backing buffer.
        let dummy_vertices = [[0.0f32; 2]];
        let dummy_items = [DrawItem {
            bounds: [0.0; 4],
            color: [0.0; 4],
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
        let device = self.ctx.device();
        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterize params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterize path vertices"),
            contents: vertex_bytes,
            usage: wgpu::BufferUsages::STORAGE,
        });
        let item_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterize draw items"),
            contents: item_bytes,
            usage: wgpu::BufferUsages::STORAGE,
        });

        let premul_key = TextureKey::new(
            width,
            height,
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let (premul_texture, output_texture) = {
            let mut pool = self.pool.lock().expect("texture pool poisoned");
            (
                pool.acquire(premul_key),
                pool.acquire(gpu_util::tex_key_rw(width, height)),
            )
        };
        let premul_view = premul_texture.create_view();
        let output_view = output_texture.create_view();
        let raster_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rasterize draw data"),
            layout: self.raster_pipeline.bind_group_layout(),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: vertex_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: item_buffer.as_entire_binding(),
                },
            ],
        });
        let unpremultiply_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rasterize unpremultiply"),
            layout: self.unpremultiply_pipeline.bind_group_layout(),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&premul_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterize"),
        });
        self.raster_pipeline.draw_quads(
            &mut encoder,
            &raster_bind_group,
            &premul_view,
            items.len() as u32,
        );
        self.unpremultiply_pipeline.dispatch(
            &mut encoder,
            &unpremultiply_bind_group,
            width,
            height,
        );
        self.ctx.queue().submit(Some(encoder.finish()));
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

fn buffer_layout_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    uniform: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: if uniform {
                wgpu::BufferBindingType::Uniform
            } else {
                wgpu::BufferBindingType::Storage { read_only: true }
            },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn storage_texture_layout_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::Rgba32Float,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
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
        let Primitive::Path { verts, closed } = prim;
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
        for position in &positions[verts.clone()] {
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
        let color = tinted(
            element_color(geo.primitive_attrs(), prim_index, style.color),
            element_alpha(geo.primitive_attrs(), prim_index),
            placement.tint,
        );
        items.push(DrawItem {
            bounds,
            color: color_array(color),
            data0: [
                1.0,
                start as f32,
                verts.len() as f32,
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
            data0: [0.0, center.0, center.1, radius],
            data1: [0.0; 4],
        });
    }

    if depth >= MAX_INSTANCE_DEPTH {
        if geo.instance_source().is_some() {
            log::warn!("rasterize: instance nesting deeper than {MAX_INSTANCE_DEPTH}, skipping");
        }
        return;
    }
    let Some(source) = geo.instance_source() else {
        return;
    };
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
        flatten_geometry(
            source,
            compose(placement, local),
            depth + 1,
            style,
            vertices,
            items,
        );
    }
}

/// True for each point referenced by a `Primitive::Path`. Path vertices are
/// already represented by their fill/stroke; only unmarked ("loose") points
/// draw as circle sprites.
fn path_vertex_mask(geo: &Geometry, point_count: usize) -> Vec<bool> {
    let mut mask = vec![false; point_count];
    for prim in geo.primitives() {
        let Primitive::Path { verts, .. } = prim;
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

fn raster_geometry(
    geo: &Geometry,
    placement: Placement,
    depth: u32,
    pixels: &mut [f32],
    width: u32,
    height: u32,
    style: Style,
) {
    let positions = geo
        .points()
        .get(names::P)
        .and_then(|c| c.as_vec2(names::P).ok().map(<[Vec2]>::to_vec))
        .unwrap_or_default();

    raster_paths(geo, &positions, placement, pixels, width, height, style);
    raster_points(geo, &positions, placement, pixels, width, height, style);
    raster_instances(geo, placement, depth, pixels, width, height, style);
}

fn raster_paths(
    geo: &Geometry,
    positions: &[Vec2],
    placement: Placement,
    pixels: &mut [f32],
    width: u32,
    height: u32,
    style: Style,
) {
    for (prim_index, prim) in geo.primitives().iter().enumerate() {
        let Primitive::Path { verts, closed } = prim;
        if verts.len() < 2 || verts.end > positions.len() {
            continue;
        }

        let mut commands = Vec::with_capacity(verts.len() + 1);
        for (i, p) in positions[verts.clone()].iter().enumerate() {
            let v = placement.apply(*p);
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

        let color = tinted(
            element_color(geo.primitive_attrs(), prim_index, style.color),
            element_alpha(geo.primitive_attrs(), prim_index),
            placement.tint,
        );

        let mut coverage = vec![0u8; width as usize * height as usize];
        if style.fill && *closed {
            Mask::new(commands.as_slice())
                .size(width, height)
                .style(Fill::NonZero)
                .render_into(&mut coverage, None);
            blend_coverage(pixels, &coverage, color);
        }
        if style.stroke_width > 0.0 {
            let mut stroke_cov = vec![0u8; width as usize * height as usize];
            // Round caps/joins match the GPU stroke, which is an unsigned
            // distance to the polyline (inherently round at caps and joins).
            let mut stroke = Stroke::new(style.stroke_width * placement.uniform_scale());
            stroke.cap(Cap::Round).join(Join::Round);
            Mask::new(commands.as_slice())
                .size(width, height)
                .style(stroke)
                .render_into(&mut stroke_cov, None);
            blend_coverage(pixels, &stroke_cov, color);
        }
    }
}

fn raster_instances(
    geo: &Geometry,
    placement: Placement,
    depth: u32,
    pixels: &mut [f32],
    width: u32,
    height: u32,
    style: Style,
) {
    if depth >= MAX_INSTANCE_DEPTH {
        log::warn!("rasterize: instance nesting deeper than {MAX_INSTANCE_DEPTH}, skipping");
        return;
    }
    let Some(source) = geo.instance_source() else {
        return;
    };
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
        raster_geometry(source, combined, depth + 1, pixels, width, height, style);
    }
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
    pixels: &mut [f32],
    width: u32,
    height: u32,
    style: Style,
) {
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
                    blend_pixel(&mut pixels[idx..idx + 4], color, cov);
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

fn element_color(set: &AttributeSet, index: usize, fallback: Color) -> Color {
    set.get(names::CD)
        .and_then(|c| {
            c.as_color(names::CD)
                .ok()
                .and_then(|v| v.get(index).copied())
        })
        .unwrap_or(fallback)
}

fn element_alpha(set: &AttributeSet, index: usize) -> f32 {
    set.get(names::ALPHA)
        .and_then(|c| {
            c.as_f32(names::ALPHA)
                .ok()
                .and_then(|v| v.get(index).copied())
        })
        .unwrap_or(1.0)
}

fn tinted(color: Color, alpha: f32, tint: Color) -> Color {
    Color::new(
        color.r * tint.r,
        color.g * tint.g,
        color.b * tint.b,
        color.a * alpha * tint.a,
    )
}

fn blend_coverage(pixels: &mut [f32], coverage: &[u8], color: Color) {
    for (i, cov) in coverage.iter().enumerate() {
        if *cov == 0 {
            continue;
        }
        let idx = i * 4;
        blend_pixel(&mut pixels[idx..idx + 4], color, *cov as f32 / 255.0);
    }
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
    use ravel_core::types::FrameRate;
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
        fb.data[idx..idx + 4].try_into().unwrap()
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
        assert_eq!((cpu.width, cpu.height), (gpu.width, gpu.height));
        let pixel_count = (cpu.width * cpu.height) as usize;
        let matching = cpu
            .data
            .chunks_exact(4)
            .zip(gpu.data.chunks_exact(4))
            .filter(|(a, b)| a.iter().zip(*b).all(|(x, y)| (x - y).abs() < 0.1))
            .count();
        let match_ratio = matching as f32 / pixel_count as f32;
        let cpu_coverage: f32 = cpu.data.iter().skip(3).step_by(4).sum();
        let gpu_coverage: f32 = gpu.data.iter().skip(3).step_by(4).sum();
        let coverage_delta = (cpu_coverage - gpu_coverage).abs() / cpu_coverage.max(1.0);
        eprintln!(
            "{label}: {:.3}% pixels within 0.1, coverage delta {:.3}%",
            match_ratio * 100.0,
            coverage_delta * 100.0
        );
        assert!(
            match_ratio > 0.99,
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

        let scaled_ctx = ctx(20, 20).with_comp_resolution((40, 40));
        let cpu = run_with_ctx(true, 0.0, &bowtie, &scaled_ctx);
        let gpu_frame = run_gpu(&gpu, &pool, &bowtie, true, 0.0, &scaled_ctx);
        assert_equivalent(&cpu, &gpu_frame, "scaled composition coordinates");
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
}
