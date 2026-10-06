// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.fill` and `comp.tint` — re-colour an image, keeping its alpha.
//!
//! * `comp.fill` replaces the RGB with one colour (the alpha is untouched).
//! * `comp.tint` maps each pixel's luminance onto the line from `map_black` to
//!   `map_white` (luminance clamped to `[0, 1]`).
//!
//! Both are one shader (`comp_colorize.wgsl`); fill is tint with both ends the
//! same colour. The colours' own alpha is ignored: these nodes never change the
//! alpha channel. Alpha convention: straight, as every frame buffer is (see
//! `premultiplied.wgsl`) — the RGB written is the colour itself, never scaled by
//! the pixel's alpha. Luminance is Rec.709, as in `color_correct`.
//!
//! **Not shell nodes**: unlike `comp.opacity` these are ordinary user-placed
//! nodes and never decode a deterministic id or read the `Document`.
//! `comp.fill` is also unrelated to the geometry node `style.fill`, which
//! writes an attribute and does not rasterize.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool,
};
use std::sync::{Arc, Mutex};

use super::transparent;
use crate::gpu_util;

const SHADER_SRC: &str = include_str!("../shaders/comp_colorize.wgsl");

const DEFAULT_FILL: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
const DEFAULT_BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const DEFAULT_WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// Which of the two nodes a [`CompColorizeProcessor`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorizeKind {
    Fill,
    Tint,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    color_a: [f32; 4],
    color_b: [f32; 4],
}

pub struct CompColorizeProcessor {
    kind: ColorizeKind,
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl CompColorizeProcessor {
    pub fn new(
        kind: ColorizeKind,
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
    ) -> Self {
        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::output_storage_layout_entry(1),
            gpu_util::uniform_layout_entry(2),
        ];
        // Shared by fill and tint: they differ only in the uniform.
        let pipeline = shaders
            .compute_pipeline(
                "comp_colorize",
                SHADER_SRC,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_colorize.wgsl compilation failed");
        Self {
            kind,
            ctx,
            pipeline,
            pool,
        }
    }

    fn colors(&self, params: &ResolvedParams) -> (&'static str, [f32; 4], [f32; 4]) {
        match self.kind {
            ColorizeKind::Fill => {
                let c = params.vec4_or("color", DEFAULT_FILL);
                ("comp.fill", c, c)
            }
            ColorizeKind::Tint => (
                "comp.tint",
                params.vec4_or("map_black", DEFAULT_BLACK),
                params.vec4_or("map_white", DEFAULT_WHITE),
            ),
        }
    }
}

impl NodeProcessor for CompColorizeProcessor {
    /// Nothing is captured from the node; the colours are read from `params`.
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
        let Some(input) = inputs.first().and_then(|i| i.clone()) else {
            return Ok(transparent(ctx));
        };
        let (label, color_a, color_b) = self.colors(params);
        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input.as_ref())
            .map_err(|e| anyhow::anyhow!("{label}: {e}"))?;
        let (width, height) = image.size();
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));

        let shader_params = Params { color_a, color_b };
        let input_binding = image.binding();
        let output_binding = output_tex.binding();
        self.ctx.dispatch_compute(&ComputeDispatch {
            label,
            pipeline: &self.pipeline,
            inputs: std::slice::from_ref(&input_binding),
            output: &output_binding,
            uniform: bytemuck::bytes_of(&shader_params),
            width,
            height,
        });
        image.release(&self.pool);
        Ok(Arc::new(GpuFrameBuffer::new(
            self.ctx.clone(),
            &self.pool,
            output_tex,
            width,
            height,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::super::fx_test_util::*;
    use super::*;
    use ravel_core::eval::{Evaluator, ResolvedValue};
    use ravel_core::id::NodeId;
    use ravel_core::types::FrameBuffer;

    /// CPU reference: the arithmetic of `comp_colorize.wgsl`.
    fn reference(src: &FrameBuffer, a: [f32; 4], b: [f32; 4]) -> Vec<f32> {
        let mut out = src.as_f32().to_vec();
        for px in out.chunks_exact_mut(4) {
            let t = (0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2]).clamp(0.0, 1.0);
            for ch in 0..3 {
                px[ch] = a[ch] + (b[ch] - a[ch]) * t;
            }
        }
        out
    }

    fn run(
        gpu: &GpuContext,
        kind: ColorizeKind,
        colors: &[(&str, [f32; 4])],
        input: &FrameBuffer,
    ) -> FrameBuffer {
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor = CompColorizeProcessor::new(kind, gpu.clone(), &mut shaders, pool(gpu));
        let mut params = ResolvedParams::default();
        for (key, c) in colors {
            params.set(key, ResolvedValue::Vec4(*c));
        }
        let node = Node::new(NodeId::new(1), "comp.fill");
        let out = processor
            .process(
                &node,
                &ctx((input.width, input.height)),
                &[Some(Arc::new(input.clone()))],
                &params,
                &mut Evaluator::new(),
            )
            .expect("colorize");
        readback(out.as_ref())
    }

    #[test]
    fn fill_keeps_alpha_bit_for_bit_and_sets_rgb() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(9, 5);
        let color = [0.125, 0.5, 0.875, 0.3];
        let out = run(&gpu, ColorizeKind::Fill, &[("color", color)], &input);
        assert!(
            input
                .as_f32()
                .chunks_exact(4)
                .any(|p| p[3] > 0.0 && p[3] < 1.0),
            "the input must exercise partial alpha"
        );
        for (i, (o, s)) in out
            .as_f32()
            .chunks_exact(4)
            .zip(input.as_f32().chunks_exact(4))
            .enumerate()
        {
            assert_eq!(o[3].to_bits(), s[3].to_bits(), "alpha of pixel {i}");
            // Straight alpha: the colour itself, not scaled by the alpha.
            assert_eq!(o[..3], color[..3], "rgb of pixel {i}");
        }
    }

    #[test]
    fn fill_of_a_transparent_pixel_stays_transparent() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = frame(2, 1, &[[0.9, 0.2, 0.1, 0.0], [0.0, 0.0, 0.0, 1.0]]);
        let out = run(
            &gpu,
            ColorizeKind::Fill,
            &[("color", [1.0, 0.0, 0.0, 1.0])],
            &input,
        );
        assert_eq!(out.as_f32()[3], 0.0);
        assert_eq!(out.as_f32()[7], 1.0);
    }

    #[test]
    fn tint_maps_black_to_a_and_white_to_b() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = frame(
            3,
            1,
            &[
                [0.0, 0.0, 0.0, 1.0],
                [1.0, 1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0, 0.5],
            ],
        );
        let a = [0.25, 0.0, 0.5, 1.0];
        let b = [1.0, 0.75, 0.0, 1.0];
        let out = run(
            &gpu,
            ColorizeKind::Tint,
            &[("map_black", a), ("map_white", b)],
            &input,
        );
        let data = out.as_f32();
        let px: Vec<&[f32]> = data.chunks_exact(4).collect();
        for ch in 0..3 {
            assert!((px[0][ch] - a[ch]).abs() < 1e-6, "black -> A, channel {ch}");
            assert!((px[1][ch] - b[ch]).abs() < 1e-6, "white -> B, channel {ch}");
            // Straight alpha: half-transparent white still maps to B, and
            // keeps its alpha.
            assert!((px[2][ch] - b[ch]).abs() < 1e-6, "channel {ch}");
        }
        assert_eq!(px[2][3], 0.5);
    }

    #[test]
    fn gpu_matches_the_cpu_reference() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(11, 6);
        let (a, b) = ([0.1, 0.2, 0.3, 1.0], [0.9, 0.6, 0.2, 1.0]);
        let out = run(
            &gpu,
            ColorizeKind::Tint,
            &[("map_black", a), ("map_white", b)],
            &input,
        );
        let want = reference(&input, a, b);
        for (i, (g, w)) in out.as_f32().iter().zip(&want).enumerate() {
            assert!((g - w).abs() < 1e-5, "element {i}: gpu {g} vs cpu {w}");
        }
    }

    #[test]
    fn a_missing_input_is_a_transparent_frame() {
        let Some(gpu) = gpu_or_skip() else { return };
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor =
            CompColorizeProcessor::new(ColorizeKind::Fill, gpu.clone(), &mut shaders, pool(&gpu));
        let node = Node::new(NodeId::new(1), "comp.fill");
        let out = processor
            .process(
                &node,
                &ctx((3, 2)),
                &[],
                &ResolvedParams::default(),
                &mut Evaluator::new(),
            )
            .expect("fill");
        let fb = out
            .downcast_ref::<FrameBuffer>()
            .expect("CPU transparent frame");
        assert_eq!((fb.width, fb.height), (3, 2));
        assert!(fb.as_f32().iter().all(|v| *v == 0.0));
    }
}
