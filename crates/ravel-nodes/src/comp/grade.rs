// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The colour-adjustment and grading nodes: `comp.brightness_contrast`,
//! `comp.hue_saturation`.
//!
//! One processor, one shader (`comp_grade.wgsl`), one uniform. [`GradeKind`]
//! says which node a [`CompGradeProcessor`] is, and [`GradeKind::fill`] is the
//! only place that reads a node's parameters into the uniform. Every node is a
//! per-pixel operation on the **straight** RGB as stored; the alpha channel is
//! carried through bit for bit (GPUCOMP-4, see `premultiplied.wgsl`).
//!
//! **Not shell nodes**: like `comp.fill` these are ordinary user-placed nodes
//! and never decode a deterministic id or read the `Document`.

use bytemuck::Zeroable;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool,
};
use std::sync::{Arc, Mutex};

use super::transparent;
use crate::gpu_util;

const SHADER_SRC: &str = include_str!("../shaders/comp_grade.wgsl");

/// Which node a [`CompGradeProcessor`] is. The discriminant is the shader's
/// `mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradeKind {
    BrightnessContrast = 0,
    HueSaturation = 1,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    mode: [u32; 4],
    a: [f32; 4],
    b: [f32; 4],
    c: [f32; 4],
}

impl GradeKind {
    fn label(self) -> &'static str {
        match self {
            Self::BrightnessContrast => "comp.brightness_contrast",
            Self::HueSaturation => "comp.hue_saturation",
        }
    }

    /// Read the node's parameters into the uniform. Defaults are the neutral
    /// value of each parameter (the node does nothing), the same as the
    /// template's.
    fn fill(self, p: &ResolvedParams) -> Params {
        let mut out = Params::zeroed();
        out.mode[0] = self as u32;
        match self {
            // a = (brightness, contrast, pivot)
            Self::BrightnessContrast => {
                out.a = [
                    p.f32_or("brightness", 0.0),
                    p.f32_or("contrast", 1.0),
                    p.f32_or("pivot", 0.5),
                    0.0,
                ];
            }
            // a = the rotation's (m0, m1, m2), b.x = saturation. The hue is
            // a rotation about the gray axis (1, 1, 1) by `hue` degrees
            // (Rodrigues), so it is linear in RGB and HDR-safe, and a gray
            // pixel never moves. 120 degrees turns red into green.
            Self::HueSaturation => {
                let (sin, cos) = p.f32_or("hue", 0.0).to_radians().sin_cos();
                let third = (1.0 - cos) / 3.0;
                let side = sin / 3.0_f32.sqrt();
                out.a = [cos + third, third - side, third + side, 0.0];
                out.b = [p.f32_or("saturation", 1.0), 0.0, 0.0, 0.0];
            }
        }
        out
    }
}

pub struct CompGradeProcessor {
    kind: GradeKind,
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl CompGradeProcessor {
    pub fn new(
        kind: GradeKind,
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
    ) -> Self {
        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::output_storage_layout_entry(1),
            gpu_util::uniform_layout_entry(2),
        ];
        // Shared by every kind: they differ only in the uniform.
        let pipeline = shaders
            .compute_pipeline(
                "comp_grade",
                SHADER_SRC,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_grade.wgsl compilation failed");
        Self {
            kind,
            ctx,
            pipeline,
            pool,
        }
    }
}

impl NodeProcessor for CompGradeProcessor {
    /// Nothing is captured from the node; every value is read from `params`.
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
        let label = self.kind.label();
        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input.as_ref())
            .map_err(|e| anyhow::anyhow!("{label}: {e}"))?;
        let (width, height) = image.size();
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));

        let shader_params = self.kind.fill(params);
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

    /// Run one node over `input` with the given resolved parameters.
    fn run(
        gpu: &GpuContext,
        kind: GradeKind,
        values: &[(&str, ResolvedValue)],
        input: &FrameBuffer,
    ) -> FrameBuffer {
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor = CompGradeProcessor::new(kind, gpu.clone(), &mut shaders, pool(gpu));
        let mut params = ResolvedParams::default();
        for (key, v) in values {
            params.set(key, v.clone());
        }
        let node = Node::new(NodeId::new(1), kind.label());
        let out = processor
            .process(
                &node,
                &ctx((input.width, input.height)),
                &[Some(Arc::new(input.clone()))],
                &params,
                &mut Evaluator::new(),
            )
            .expect("grade");
        readback(out.as_ref())
    }

    fn f(v: f32) -> ResolvedValue {
        ResolvedValue::Float(v)
    }

    /// Alpha is bit-identical and the RGB is `want(rgb)` within `tol`.
    fn assert_matches(
        out: &FrameBuffer,
        input: &FrameBuffer,
        tol: f32,
        want: impl Fn([f32; 3]) -> [f32; 3],
    ) {
        for (i, (o, s)) in out
            .as_f32()
            .chunks_exact(4)
            .zip(input.as_f32().chunks_exact(4))
            .enumerate()
        {
            assert_eq!(o[3].to_bits(), s[3].to_bits(), "alpha of pixel {i}");
            let w = want([s[0], s[1], s[2]]);
            for ch in 0..3 {
                assert!(
                    (o[ch] - w[ch]).abs() <= tol,
                    "pixel {i} channel {ch}: gpu {} vs want {}",
                    o[ch],
                    w[ch]
                );
            }
        }
    }

    #[test]
    fn brightness_contrast_golden_pixels() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = frame(
            3,
            1,
            &[
                [0.0, 0.5, 1.0, 1.0],
                [0.25, 0.75, 0.5, 0.5],
                [0.5, 0.5, 0.5, 0.0],
            ],
        );
        // (x - 0.5) * 2 + 0.5 + 0.125
        let out = run(
            &gpu,
            GradeKind::BrightnessContrast,
            &[("brightness", f(0.125)), ("contrast", f(2.0))],
            &input,
        );
        let want = [[-0.375, 0.625, 1.625], [0.125, 1.125, 0.625], [0.625; 3]];
        for (px, want) in out.as_f32().chunks_exact(4).zip(want) {
            assert_eq!(px[..3], want);
        }
        // Alpha, including the transparent pixel's, is untouched.
        assert_eq!(
            out.as_f32()
                .chunks_exact(4)
                .map(|p| p[3])
                .collect::<Vec<_>>(),
            [1.0, 0.5, 0.0]
        );
    }

    #[test]
    fn brightness_contrast_pivot_is_the_fixed_point() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = frame(2, 1, &[[0.25; 4], [0.5; 4]]);
        let out = run(
            &gpu,
            GradeKind::BrightnessContrast,
            &[("contrast", f(3.0)), ("pivot", f(0.25))],
            &input,
        );
        // The pivot stays; 0.5 moves to 0.25 + 0.25 * 3.
        assert_eq!(out.as_f32()[..3], [0.25; 3]);
        assert_eq!(out.as_f32()[4..7], [1.0; 3]);
    }

    #[test]
    fn defaults_are_the_identity() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(9, 5);
        for kind in [GradeKind::BrightnessContrast, GradeKind::HueSaturation] {
            let out = run(&gpu, kind, &[], &input);
            assert_matches(&out, &input, 1e-6, |rgb| rgb);
        }
    }

    #[test]
    fn hue_of_120_degrees_turns_red_into_green_into_blue() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = frame(
            4,
            1,
            &[
                [1.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 0.5],
                [0.0, 0.0, 1.0, 1.0],
                [0.5, 0.5, 0.5, 1.0],
            ],
        );
        let out = run(&gpu, GradeKind::HueSaturation, &[("hue", f(120.0))], &input);
        let want = [
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            [0.5, 0.5, 0.5],
        ];
        for (px, want) in out.as_f32().chunks_exact(4).zip(want) {
            for ch in 0..3 {
                assert!((px[ch] - want[ch]).abs() < 1e-6, "{px:?} vs {want:?}");
            }
        }
        assert_eq!(out.as_f32()[7], 0.5, "alpha is kept");
    }

    #[test]
    fn saturation_zero_is_rec709_luminance_and_hue_turns_a_full_circle() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(9, 5);
        let out = run(
            &gpu,
            GradeKind::HueSaturation,
            &[("saturation", f(0.0))],
            &input,
        );
        assert_matches(&out, &input, 1e-6, |[r, g, b]| {
            [0.2126 * r + 0.7152 * g + 0.0722 * b; 3]
        });
        let out = run(&gpu, GradeKind::HueSaturation, &[("hue", f(360.0))], &input);
        assert_matches(&out, &input, 1e-5, |rgb| rgb);
    }

    #[test]
    fn a_missing_input_is_a_transparent_frame() {
        let Some(gpu) = gpu_or_skip() else { return };
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor = CompGradeProcessor::new(
            GradeKind::BrightnessContrast,
            gpu.clone(),
            &mut shaders,
            pool(&gpu),
        );
        let node = Node::new(NodeId::new(1), "comp.brightness_contrast");
        let out = processor
            .process(
                &node,
                &ctx((3, 2)),
                &[],
                &ResolvedParams::default(),
                &mut Evaluator::new(),
            )
            .expect("grade");
        let fb = out
            .downcast_ref::<FrameBuffer>()
            .expect("CPU transparent frame");
        assert_eq!((fb.width, fb.height), (3, 2));
        assert!(fb.as_f32().iter().all(|v| *v == 0.0));
    }
}
