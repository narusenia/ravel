// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Geometry → FrameBuffer rasterization (CPU path).
//!
//! Paths are filled/stroked through `zeno` with antialiased coverage; points
//! draw as analytic-AA circle sprites. Instances expand their source geometry
//! with per-instance `P`/`rot`/`scale` and optional `Cd`/`alpha` tint.
//! Geometry positions are interpreted in output pixel space (origin top-left).
//! Output is straight-alpha RGBA f32, composited src-over to match the
//! existing merge convention.

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, NodeProcessor};
use ravel_core::geometry::{AttributeSet, Geometry, Primitive, names};
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::types::{Color, FrameBuffer, NodeData, Vec2};
use zeno::{Command, Fill, Mask, Stroke, Vector};

/// Instance nesting guard: instances-of-instances beyond this depth are
/// skipped rather than recursed (spec limits stateful/sim nesting similarly).
const MAX_INSTANCE_DEPTH: u32 = 4;
const DEFAULT_POINT_RADIUS: f32 = 2.0;

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
    fill: bool,
    stroke_width: f32,
}

impl RasterizeProcessor {
    pub fn from_node(node: &Node) -> Self {
        let mut fill = true;
        let mut stroke_width = 0.0;
        for p in &node.parameters {
            match (p.key.as_str(), &p.value) {
                ("fill", ParameterValue::Bool(v)) => fill = *v,
                ("stroke_width", ParameterValue::Float(v)) => stroke_width = *v,
                _ => {}
            }
        }
        Self { fill, stroke_width }
    }
}

impl NodeProcessor for RasterizeProcessor {
    fn process(
        &self,
        ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let geo = inputs
            .first()
            .and_then(|d| d.downcast_ref::<Geometry>())
            .context("rasterize expects a Geometry input")?;

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

        self.raster_geometry(geo, Placement::identity(), 0, &mut pixels, width, height);

        Ok(Box::new(FrameBuffer {
            width,
            height,
            data: pixels.into(),
        }))
    }
}

impl RasterizeProcessor {
    fn raster_geometry(
        &self,
        geo: &Geometry,
        placement: Placement,
        depth: u32,
        pixels: &mut [f32],
        width: u32,
        height: u32,
    ) {
        let positions = geo
            .points()
            .get(names::P)
            .and_then(|c| c.as_vec2(names::P).ok().map(<[Vec2]>::to_vec))
            .unwrap_or_default();

        self.raster_paths(geo, &positions, placement, pixels, width, height);
        raster_points(geo, &positions, placement, pixels, width, height);
        self.raster_instances(geo, placement, depth, pixels, width, height);
    }

    fn raster_paths(
        &self,
        geo: &Geometry,
        positions: &[Vec2],
        placement: Placement,
        pixels: &mut [f32],
        width: u32,
        height: u32,
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
                element_color(geo.primitive_attrs(), prim_index),
                element_alpha(geo.primitive_attrs(), prim_index),
                placement.tint,
            );

            let mut coverage = vec![0u8; width as usize * height as usize];
            if self.fill && *closed {
                Mask::new(commands.as_slice())
                    .size(width, height)
                    .style(Fill::NonZero)
                    .render_into(&mut coverage, None);
                blend_coverage(pixels, &coverage, color);
            }
            if self.stroke_width > 0.0 {
                let mut stroke_cov = vec![0u8; width as usize * height as usize];
                Mask::new(commands.as_slice())
                    .size(width, height)
                    .style(Stroke::new(self.stroke_width * placement.uniform_scale()))
                    .render_into(&mut stroke_cov, None);
                blend_coverage(pixels, &stroke_cov, color);
            }
        }
    }

    fn raster_instances(
        &self,
        geo: &Geometry,
        placement: Placement,
        depth: u32,
        pixels: &mut [f32],
        width: u32,
        height: u32,
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
                tint: tinted(
                    element_color(inst, i),
                    element_alpha(inst, i),
                    Color::new(1.0, 1.0, 1.0, 1.0),
                ),
            };
            let combined = compose(placement, local);
            self.raster_geometry(source, combined, depth + 1, pixels, width, height);
        }
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
) {
    let points = geo.points();
    let radii = float_column(points, names::PSCALE);

    for (i, p) in positions.iter().enumerate() {
        let center = placement.apply(*p);
        let radius =
            radii.as_ref().map_or(DEFAULT_POINT_RADIUS, |r| r[i]) * placement.uniform_scale();
        if radius <= 0.0 {
            continue;
        }
        let color = tinted(
            element_color(points, i),
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

fn element_color(set: &AttributeSet, index: usize) -> Color {
    set.get(names::CD)
        .and_then(|c| {
            c.as_color(names::CD)
                .ok()
                .and_then(|v| v.get(index).copied())
        })
        .unwrap_or(Color::new(1.0, 1.0, 1.0, 1.0))
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
    use ravel_core::geometry::AttributeArray;
    use ravel_core::graph::Node;
    use ravel_core::id::NodeId;
    use ravel_core::types::FrameRate;
    use std::sync::Arc;

    fn ctx(w: u32, h: u32) -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (w, h))
    }

    fn proc_with(fill: bool, stroke_width: f32) -> RasterizeProcessor {
        let node = Node::new(NodeId::new(1), "rasterize")
            .with_param("fill", ParameterValue::Bool(fill))
            .with_param("stroke_width", ParameterValue::Float(stroke_width));
        RasterizeProcessor::from_node(&node)
    }

    fn pixel(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
        let idx = ((y * fb.width + x) * 4) as usize;
        fb.data[idx..idx + 4].try_into().unwrap()
    }

    fn run(proc: &RasterizeProcessor, geo: &Geometry, w: u32, h: u32) -> FrameBuffer {
        let refs: Vec<&dyn NodeData> = vec![geo];
        let out = proc.process(&ctx(w, h), &refs).unwrap();
        out.downcast_ref::<FrameBuffer>().unwrap().clone()
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
        let fb = run(&proc_with(true, 0.0), &geo, 16, 16);

        let inside = pixel(&fb, 8, 8);
        assert!(inside[3] > 0.9, "interior should be covered: {inside:?}");
        assert!(inside[0] > 0.9 && inside[1] < 0.1, "fill uses Cd");

        let outside = pixel(&fb, 1, 1);
        assert!(outside[3] < 1e-6, "exterior stays transparent");
    }

    #[test]
    fn stroke_only_leaves_interior_empty() {
        let geo = square_geo(Color::new(0.0, 1.0, 0.0, 1.0));
        let fb = run(&proc_with(false, 2.0), &geo, 16, 16);

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
        let fb = run(&proc_with(true, 0.0), &geo, 16, 16);

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

        let fb = run(&proc_with(true, 0.0), &geo, 16, 16);
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
        let fb = run(&proc_with(true, 0.0), &geo, 16, 16);
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

        let fb = run(&proc_with(true, 0.0), &geo, 16, 16);
        let c = pixel(&fb, 8, 8);
        // Second (green, a=0.5) over first (red, a=1) → half red, half green.
        assert!(
            (c[0] - 0.5).abs() < 0.05 && (c[1] - 0.5).abs() < 0.05,
            "{c:?}"
        );
        assert!((c[3] - 1.0).abs() < 1e-3);
    }
}
