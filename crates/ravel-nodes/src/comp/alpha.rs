// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.alpha` — alpha operations on an image (`mode` parameter):
//!
//! | mode | result |
//! |---|---|
//! | `invert` | `a' = 1 - a`, RGB kept |
//! | `luma_to_alpha` | `a' = luma(rgb) * a`, RGB kept |
//! | `alpha_to_luma` | `rgb' = a` (gray), `a' = 1` |
//! | `matte_alpha` | `a' = a * matte.a` |
//! | `matte_luma` | `a' = a * luma(matte)` |
//!
//! Frames carry straight alpha, so RGB is never scaled by alpha here.
//! Luminance is Rec.709, as in `color_correct`; the matte's luminance is taken
//! from its *premultiplied* colour, so a transparent matte pixel counts as 0.
//!
//! **Matte resolution.** The matte input (`matte`) is resampled onto the main
//! image's pixel grid: the centre of main pixel `(x, y)` maps to the same UV in
//! the matte, read with bilinear filtering in premultiplied alpha and edge
//! clamping. A matte of another size therefore stretches to cover the whole
//! frame and never leaves a transparent border; an equal-size matte is read
//! texel for texel. The output always has the main image's size. With no matte
//! connected, the matte modes return the image unchanged; an unknown `mode`
//! string does too.
//!
//! **Not a shell node**: an ordinary user-placed node, no deterministic id and
//! no `Document` access.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool,
};
use std::sync::{Arc, Mutex};

use super::transparent;
use crate::gpu_util;

const SHADER_SRC: &str = include_str!("../shaders/comp_alpha.wgsl");

/// The `mode` parameter. The discriminant is the shader's `mode` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
enum Mode {
    Invert = 0,
    LumaToAlpha = 1,
    AlphaToLuma = 2,
    MatteAlpha = 3,
    MatteLuma = 4,
}

impl Mode {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "invert" => Self::Invert,
            "luma_to_alpha" => Self::LumaToAlpha,
            "alpha_to_luma" => Self::AlphaToLuma,
            "matte_alpha" => Self::MatteAlpha,
            "matte_luma" => Self::MatteLuma,
            _ => return None,
        })
    }

    fn needs_matte(self) -> bool {
        matches!(self, Self::MatteAlpha | Self::MatteLuma)
    }
}

/// Whether `mode` is a value the processor understands (the template's
/// dropdown is tested against this).
pub fn comp_alpha_mode_is_known(mode: &str) -> bool {
    Mode::parse(mode).is_some()
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    mode: u32,
    _pad: [u32; 3],
}

pub struct CompAlphaProcessor {
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl CompAlphaProcessor {
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
        _node: &Node,
    ) -> Self {
        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::input_texture_layout_entry(1),
            gpu_util::output_storage_layout_entry(2),
            gpu_util::uniform_layout_entry(3),
        ];
        let source = gpu_util::with_premultiplied_helpers(SHADER_SRC);
        let pipeline = shaders
            .compute_pipeline(
                "comp_alpha",
                &source,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_alpha.wgsl compilation failed");
        Self {
            ctx,
            pipeline,
            pool,
        }
    }
}

impl NodeProcessor for CompAlphaProcessor {
    /// Nothing is captured from the node; `mode` is read from `params`.
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
        let matte = inputs.get(1).and_then(|i| i.clone());
        let Some(mode) = Mode::parse(params.str_or("mode", "invert")) else {
            return Ok(input);
        };
        if mode.needs_matte() && matte.is_none() {
            return Ok(input);
        }

        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input.as_ref())
            .map_err(|e| anyhow::anyhow!("comp.alpha: {e}"))?;
        // Modes that read no matte bind the image in its slot; the shader
        // never samples it.
        let matte_image = match (&matte, mode.needs_matte()) {
            (Some(m), true) => Some(
                gpu_util::ensure_gpu(&self.ctx, &self.pool, m.as_ref())
                    .map_err(|e| anyhow::anyhow!("comp.alpha matte: {e}"))?,
            ),
            _ => None,
        };
        let (width, height) = image.size();
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));

        let bindings = [
            image.binding(),
            matte_image
                .as_ref()
                .map_or_else(|| image.binding(), |m| m.binding()),
        ];
        let output_binding = output_tex.binding();
        let shader_params = Params {
            mode: mode as u32,
            _pad: [0; 3],
        };
        self.ctx.dispatch_compute(&ComputeDispatch {
            label: "comp_alpha",
            pipeline: &self.pipeline,
            inputs: &bindings,
            output: &output_binding,
            uniform: bytemuck::bytes_of(&shader_params),
            width,
            height,
        });
        image.release(&self.pool);
        if let Some(m) = matte_image {
            m.release(&self.pool);
        }
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

    const W: [f32; 3] = [0.2126, 0.7152, 0.0722];

    fn luma(p: &[f32]) -> f32 {
        W[0] * p[0] + W[1] * p[1] + W[2] * p[2]
    }

    /// Premultiplied bilinear matte sample with edge clamping, at the centre
    /// of main pixel `(x, y)` of a `main` sized frame: the definition from the
    /// module docs, written independently of the shader.
    fn matte_at(m: &FrameBuffer, main: (u32, u32), x: u32, y: u32) -> [f32; 4] {
        let sx = (x as f32 + 0.5) / main.0 as f32 * m.width as f32 - 0.5;
        let sy = (y as f32 + 0.5) / main.1 as f32 * m.height as f32 - 0.5;
        let (x0, y0) = (sx.floor(), sy.floor());
        let (tx, ty) = (sx - x0, sy - y0);
        let tap = |ix: f32, iy: f32| {
            let cx = (ix as i32).clamp(0, m.width as i32 - 1) as usize;
            let cy = (iy as i32).clamp(0, m.height as i32 - 1) as usize;
            let p = &m.as_f32()[(cy * m.width as usize + cx) * 4..][..4];
            [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]
        };
        let mut acc = [0.0; 4];
        for (dx, dy, w) in [
            (0.0, 0.0, (1.0 - tx) * (1.0 - ty)),
            (1.0, 0.0, tx * (1.0 - ty)),
            (0.0, 1.0, (1.0 - tx) * ty),
            (1.0, 1.0, tx * ty),
        ] {
            let p = tap(x0 + dx, y0 + dy);
            for c in 0..4 {
                acc[c] += w * p[c];
            }
        }
        acc
    }

    /// CPU reference for every mode.
    fn reference(mode: Mode, src: &FrameBuffer, matte: Option<&FrameBuffer>) -> Vec<f32> {
        let mut out = src.as_f32().to_vec();
        for (i, px) in out.chunks_exact_mut(4).enumerate() {
            let a = px[3];
            let (x, y) = (i as u32 % src.width, i as u32 / src.width);
            match mode {
                Mode::Invert => px[3] = 1.0 - a,
                Mode::LumaToAlpha => px[3] = luma(px) * a,
                Mode::AlphaToLuma => px.copy_from_slice(&[a, a, a, 1.0]),
                Mode::MatteAlpha => {
                    px[3] = a * matte_at(matte.unwrap(), (src.width, src.height), x, y)[3]
                }
                Mode::MatteLuma => {
                    let m = matte_at(matte.unwrap(), (src.width, src.height), x, y);
                    px[3] = a * luma(&m);
                }
            }
        }
        out
    }

    fn run(
        gpu: &GpuContext,
        mode: &str,
        input: &FrameBuffer,
        matte: Option<&FrameBuffer>,
    ) -> Arc<dyn NodeData> {
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = Node::new(NodeId::new(1), "comp.alpha");
        let processor = CompAlphaProcessor::new(gpu.clone(), &mut shaders, pool(gpu), &node);
        let mut params = ResolvedParams::default();
        params.set("mode", ResolvedValue::Str(mode.into()));
        processor
            .process(
                &node,
                &ctx((input.width, input.height)),
                &[
                    Some(Arc::new(input.clone())),
                    matte.map(|m| Arc::new(m.clone()) as Arc<dyn NodeData>),
                ],
                &params,
                &mut Evaluator::new(),
            )
            .expect("alpha")
    }

    fn assert_close(got: &[f32], want: &[f32], what: &str) {
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert!(
                (g - w).abs() < 1e-5,
                "{what}: element {i}: gpu {g} vs cpu {w}"
            );
        }
    }

    #[test]
    fn inverting_twice_restores_the_input_exactly() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(9, 5);
        let once = run(&gpu, "invert", &input, None);
        let twice = run(&gpu, "invert", &readback(once.as_ref()), None);
        assert_eq!(readback(twice.as_ref()).as_f32(), input.as_f32());
        // And one inversion does something.
        assert_ne!(readback(once.as_ref()).as_f32(), input.as_f32());
    }

    #[test]
    fn straight_alpha_modes_leave_rgb_alone() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = frame(2, 1, &[[1.0, 1.0, 1.0, 0.5], [0.5, 0.25, 0.75, 0.25]]);
        for mode in ["invert", "luma_to_alpha"] {
            let out = readback(run(&gpu, mode, &input, None).as_ref());
            for (o, s) in out
                .as_f32()
                .chunks_exact(4)
                .zip(input.as_f32().chunks_exact(4))
            {
                assert_eq!(o[..3], s[..3], "{mode} must not scale RGB by alpha");
            }
        }
        let out = readback(run(&gpu, "luma_to_alpha", &input, None).as_ref());
        // White at half alpha: luma 1 (weights sum to 1 within rounding) * 0.5.
        assert!((out.as_f32()[3] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn alpha_to_luma_is_opaque_gray_of_the_alpha() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = frame(2, 1, &[[1.0, 0.0, 0.0, 0.25], [0.0, 1.0, 0.0, 1.0]]);
        let out = readback(run(&gpu, "alpha_to_luma", &input, None).as_ref());
        assert_eq!(
            out.as_f32()[..],
            [0.25, 0.25, 0.25, 1.0, 1.0, 1.0, 1.0, 1.0]
        );
    }

    #[test]
    fn every_mode_matches_the_cpu_reference() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(7, 5);
        let matte = ramp(3, 4);
        for (name, mode) in [
            ("invert", Mode::Invert),
            ("luma_to_alpha", Mode::LumaToAlpha),
            ("alpha_to_luma", Mode::AlphaToLuma),
            ("matte_alpha", Mode::MatteAlpha),
            ("matte_luma", Mode::MatteLuma),
        ] {
            let got = readback(run(&gpu, name, &input, Some(&matte)).as_ref());
            assert_eq!((got.width, got.height), (7, 5), "{name}: main size");
            assert_close(&got.as_f32(), &reference(mode, &input, Some(&matte)), name);
        }
    }

    /// The defined handling of a matte whose size differs from the image.
    #[test]
    fn a_matte_of_another_size_stretches_with_bilinear_and_edge_clamp() {
        let Some(gpu) = gpu_or_skip() else { return };
        let opaque = |a: f32| [1.0, 1.0, 1.0, a];
        let input = frame(4, 1, &[opaque(1.0); 4]);

        // 2x1 matte alpha [0, 1] over 4 pixels: UV centres sit at 0.25 / 0.75 /
        // 1.25 / 1.75 matte pixels -> clamped, 0.25, 0.75, clamped.
        let matte = frame(2, 1, &[opaque(0.0), opaque(1.0)]);
        let out = readback(run(&gpu, "matte_alpha", &input, Some(&matte)).as_ref());
        let alphas: Vec<f32> = out.as_f32().chunks_exact(4).map(|p| p[3]).collect();
        assert_close(&alphas, &[0.0, 0.25, 0.75, 1.0], "2x1 -> 4x1");

        // A 1x1 matte covers the whole frame: no transparent border.
        let one = frame(1, 1, &[opaque(0.5)]);
        let out = readback(run(&gpu, "matte_alpha", &input, Some(&one)).as_ref());
        assert!(out.as_f32().chunks_exact(4).all(|p| p[3] == 0.5));

        // The output keeps the main image's size, not the matte's.
        let big = frame(8, 2, &[opaque(1.0); 16]);
        let out = readback(run(&gpu, "matte_alpha", &input, Some(&big)).as_ref());
        assert_eq!((out.width, out.height), (4, 1));
    }

    /// Matte luma follows the premultiplied convention: a transparent texel
    /// that stores white contributes nothing, so the blend between it and an
    /// opaque white texel is 0.5 (straight weighting would give 1.0).
    #[test]
    fn matte_luma_filters_in_premultiplied_alpha() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = frame(4, 1, &[[1.0, 1.0, 1.0, 1.0]; 4]);
        let matte = frame(2, 1, &[[1.0, 1.0, 1.0, 0.0], [1.0, 1.0, 1.0, 1.0]]);
        let out = readback(run(&gpu, "matte_luma", &input, Some(&matte)).as_ref());
        let a = out.as_f32();
        assert!((a[4 + 3] - 0.25).abs() < 1e-5, "got {}", a[4 + 3]);
        assert!((a[8 + 3] - 0.75).abs() < 1e-5, "got {}", a[8 + 3]);
    }

    #[test]
    fn a_matte_mode_without_a_matte_returns_the_input() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = ramp(3, 3);
        for mode in ["matte_alpha", "matte_luma", "no_such_mode"] {
            let out = run(&gpu, mode, &input, None);
            let fb = out
                .downcast_ref::<FrameBuffer>()
                .expect("passed through unchanged");
            assert_eq!(fb.as_f32(), input.as_f32(), "{mode}");
        }
    }
}
