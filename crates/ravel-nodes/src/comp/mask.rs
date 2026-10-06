// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.mask` — keeps an image where a geometry covers it and clears the
//! alpha everywhere else.
//!
//! **Built from existing parts, not a new rasterizer.** The geometry is drawn
//! by the `rasterize` processor itself (`RasterizeProcessor`, the GPU path, so
//! coverage, anti-aliasing, instances and the composition scale are exactly
//! `rasterize`'s; nothing about coverage is computed here), with fill on and no
//! stroke. The resulting frame is the matte, and `comp.alpha`'s `matte_alpha`
//! (`CompAlphaProcessor`) multiplies the image's alpha by the matte's. So the
//! node is the pre-wired `rasterize` -> `comp.alpha` pair in one place. With
//! `invert` the matte's alpha is first inverted with `comp.alpha`'s `invert`,
//! so the image is kept *outside* the geometry.
//!
//! The matte's alpha is the geometry's own alpha times its coverage: a
//! geometry colour (`Cd` / `alpha` attributes) with alpha below 1 is a partial
//! mask. No feathering and no multi-path boolean operations (non-goals).
//! Without a geometry connected the image passes through, as `comp.alpha` does
//! with no matte. A matte whose size differs from the image (a preview) is
//! stretched over it by `comp.alpha`'s own rule.
//!
//! **Not a shell node**: an ordinary user-placed node; no `Document` access.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams, ResolvedValue};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use ravel_gpu::{GpuContext, ShaderManager, TexturePool};
use std::sync::{Arc, Mutex};

use super::{CompAlphaProcessor, transparent};
use crate::rasterize::RasterizeProcessor;

pub struct CompMaskProcessor {
    rasterize: RasterizeProcessor,
    alpha: CompAlphaProcessor,
}

impl CompMaskProcessor {
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
        node: &Node,
    ) -> Self {
        Self {
            rasterize: RasterizeProcessor::new(ctx.clone(), shaders, pool.clone(), node),
            alpha: CompAlphaProcessor::new(ctx, shaders, pool, node),
        }
    }
}

fn alpha_params(mode: &str) -> ResolvedParams {
    let mut p = ResolvedParams::default();
    p.set("mode", ResolvedValue::Str(mode.into()));
    p
}

impl NodeProcessor for CompMaskProcessor {
    /// Nothing is captured from the node; `invert` is read from `params`.
    fn rebuild_on_node_change(&self) -> bool {
        false
    }

    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let Some(image) = inputs.first().and_then(|i| i.clone()) else {
            return Ok(transparent(ctx));
        };
        let Some(geometry) = inputs.get(1).and_then(|i| i.clone()) else {
            return Ok(image);
        };
        // Fill on, no stroke, the default colour: `rasterize`'s own defaults
        // (an empty parameter set), so the matte is the geometry's coverage.
        let mut matte = self.rasterize.process(
            node,
            ctx,
            &[Some(geometry)],
            &ResolvedParams::default(),
            scope,
        )?;
        if params.bool_or("invert", false) {
            matte =
                self.alpha
                    .process(node, ctx, &[Some(matte)], &alpha_params("invert"), scope)?;
        }
        self.alpha.process(
            node,
            ctx,
            &[Some(image), Some(matte)],
            &alpha_params("matte_alpha"),
            scope,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::fx_test_util::*;
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::geometry::{Geometry, Primitive};
    use ravel_core::id::NodeId;
    use ravel_core::types::{FrameBuffer, Vec2};

    /// A closed square from `(4, 4)` to `(12, 12)`: pixel edges, so coverage is
    /// exactly 1 inside and 0 outside.
    fn square() -> Geometry {
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

    fn run(
        gpu: &GpuContext,
        values: &[(&str, ResolvedValue)],
        image: &FrameBuffer,
        geometry: Option<Geometry>,
    ) -> Arc<dyn NodeData> {
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = Node::new(NodeId::new(1), "comp.mask");
        let processor = CompMaskProcessor::new(gpu.clone(), &mut shaders, pool(gpu), &node);
        let mut params = ResolvedParams::default();
        for (key, v) in values {
            params.set(key, v.clone());
        }
        processor
            .process(
                &node,
                &ctx((image.width, image.height)),
                &[
                    Some(Arc::new(image.clone())),
                    geometry.map(|g| Arc::new(g) as Arc<dyn NodeData>),
                ],
                &params,
                &mut Evaluator::new(),
            )
            .expect("mask")
    }

    fn px(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
        fb.as_f32()[((y * fb.width + x) * 4) as usize..][..4]
            .try_into()
            .unwrap()
    }

    /// Golden: an opaque image through a square matte: alpha 1 inside, exactly
    /// 0 outside, RGB untouched everywhere.
    #[test]
    fn alpha_outside_the_geometry_is_zero() {
        let Some(gpu) = gpu_or_skip() else { return };
        let colour = [0.2, 0.4, 0.6, 1.0];
        let image = frame(16, 16, &[colour; 256]);
        let out = readback(run(&gpu, &[], &image, Some(square())).as_ref());
        for y in 0..16 {
            for x in 0..16 {
                let p = px(&out, x, y);
                assert_eq!(p[..3], colour[..3], "RGB is untouched at ({x},{y})");
                if (4..12).contains(&x) && (4..12).contains(&y) {
                    assert!(p[3] > 0.999, "inside ({x},{y}) keeps its alpha: {p:?}");
                } else {
                    assert_eq!(p[3], 0.0, "outside ({x},{y}) is cleared");
                }
            }
        }
    }

    #[test]
    fn invert_keeps_the_image_outside_the_geometry() {
        let Some(gpu) = gpu_or_skip() else { return };
        let image = frame(16, 16, &[[0.2, 0.4, 0.6, 1.0]; 256]);
        let on = [("invert", ResolvedValue::Bool(true))];
        let out = readback(run(&gpu, &on, &image, Some(square())).as_ref());
        for y in 0..16 {
            for x in 0..16 {
                let a = px(&out, x, y)[3];
                if (4..12).contains(&x) && (4..12).contains(&y) {
                    assert!(a < 1e-3, "inside ({x},{y}) is cleared: {a}");
                } else {
                    assert_eq!(a, 1.0, "outside ({x},{y}) is kept");
                }
            }
        }
    }

    /// The coverage is `rasterize`'s: the mask's alpha equals the alpha of that
    /// processor's own output for the same geometry, edges included.
    #[test]
    fn the_matte_is_the_rasterize_coverage() {
        let Some(gpu) = gpu_or_skip() else { return };
        // A triangle: slanted edges have partial coverage.
        let mut tri = Geometry::from_points(vec![Vec2(1.0, 1.0), Vec2(14.0, 3.0), Vec2(6.0, 13.0)]);
        tri.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: true,
        });
        let image = frame(16, 16, &[[1.0, 1.0, 1.0, 1.0]; 256]);
        let masked = readback(run(&gpu, &[], &image, Some(tri.clone())).as_ref());

        let mut shaders = ShaderManager::new(gpu.clone());
        let node = Node::new(NodeId::new(1), "rasterize");
        let raster = RasterizeProcessor::new(gpu.clone(), &mut shaders, pool(&gpu), &node);
        let coverage = raster
            .process(
                &node,
                &ctx((16, 16)),
                &[Some(Arc::new(tri))],
                &ResolvedParams::default(),
                &mut Evaluator::new(),
            )
            .unwrap();
        let coverage = readback(coverage.as_ref());
        let mut partial = 0;
        for y in 0..16 {
            for x in 0..16 {
                let (m, c) = (px(&masked, x, y)[3], px(&coverage, x, y)[3]);
                assert!((m - c).abs() < 1e-5, "({x},{y}): mask {m} vs rasterize {c}");
                partial += usize::from(c > 0.01 && c < 0.99);
            }
        }
        assert!(
            partial > 0,
            "the triangle must have anti-aliased edge pixels"
        );
    }

    #[test]
    fn without_a_geometry_the_image_passes_through() {
        let Some(gpu) = gpu_or_skip() else { return };
        let image = ramp(4, 4);
        let out = run(&gpu, &[], &image, None);
        assert_eq!(
            out.downcast_ref::<FrameBuffer>()
                .expect("passed through")
                .as_f32(),
            image.as_f32()
        );
    }
}
