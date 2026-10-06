// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.key` — a chroma or luminance key. The pixel's alpha is multiplied by
//! how far its colour lies from `key_color`; RGB is never changed. The formula
//! is written out in `comp_key.wgsl`:
//!
//! ```text
//! f = clamp((d - tolerance) / softness, 0, 1)    (a step when softness is 0)
//! alpha' = alpha * f         (alpha * (1 - f) with `invert`)
//! ```
//!
//! `d` is the distance to the key colour: in `chroma` mode the Rec.709 CbCr
//! distance of the colours normalised by their largest channel (so the
//! brightness of the backdrop does not move the key), in `luma` mode the
//! luminance distance. A pixel within `tolerance` is removed; one past
//! `tolerance + softness` is kept. Spill suppression, edge treatment and garbage
//! mattes are not built (`effects-library-plan.md`, non-goals).
//!
//! **Not a shell node**: an ordinary user-placed node; no `Document` access.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool,
};
use std::sync::{Arc, Mutex};

use super::transparent;
use crate::gpu_util;

const SHADER_SRC: &str = include_str!("../shaders/comp_key.wgsl");

pub use ravel_core::registry::builtin::COMP_KEY_MODES as KEY_MODES;

/// Whether `mode` is a `comp.key` mode the processor understands.
pub fn comp_key_mode_is_known(mode: &str) -> bool {
    KEY_MODES.contains(&mode)
}

/// The green-screen default the template and the processor share.
const DEFAULT_KEY: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    mode: [u32; 4],
    key: [f32; 4],
    shape: [f32; 4],
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

/// An unknown `mode` is `None` (the image passes through).
fn fill(p: &ResolvedParams) -> Option<Params> {
    let mode = match p.str_or("mode", "chroma") {
        "chroma" => 0,
        "luma" => 1,
        _ => return None,
    };
    Some(Params {
        mode: [mode, u32::from(p.bool_or("invert", false)), 0, 0],
        key: p.vec4_or("key_color", DEFAULT_KEY),
        shape: [
            finite(p.f32_or("tolerance", 0.15)).max(0.0),
            finite(p.f32_or("softness", 0.1)).max(0.0),
            0.0,
            0.0,
        ],
    })
}

pub struct CompKeyProcessor {
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl CompKeyProcessor {
    pub fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
    ) -> Self {
        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::output_storage_layout_entry(1),
            gpu_util::uniform_layout_entry(2),
        ];
        let pipeline = shaders
            .compute_pipeline(
                "comp_key",
                SHADER_SRC,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_key.wgsl compilation failed");
        Self {
            ctx,
            pipeline,
            pool,
        }
    }
}

impl NodeProcessor for CompKeyProcessor {
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
        let Some(shader_params) = fill(params) else {
            return Ok(input);
        };
        let image = gpu_util::ensure_gpu(&self.ctx, &self.pool, input.as_ref())
            .map_err(|e| anyhow::anyhow!("comp.key: {e}"))?;
        let (width, height) = image.size();
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));
        let input_binding = image.binding();
        let output_binding = output_tex.binding();
        self.ctx.dispatch_compute(&ComputeDispatch {
            label: "comp_key",
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

    fn run(gpu: &GpuContext, values: &[(&str, ResolvedValue)], input: &FrameBuffer) -> FrameBuffer {
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor = CompKeyProcessor::new(gpu.clone(), &mut shaders, pool(gpu));
        let mut params = ResolvedParams::default();
        for (key, v) in values {
            params.set(key, v.clone());
        }
        let out = processor
            .process(
                &Node::new(NodeId::new(1), "comp.key"),
                &ctx((input.width, input.height)),
                &[Some(Arc::new(input.clone()))],
                &params,
                &mut Evaluator::new(),
            )
            .expect("key");
        readback(out.as_ref())
    }

    fn f(v: f32) -> ResolvedValue {
        ResolvedValue::Float(v)
    }

    fn mode(m: &str) -> (&'static str, ResolvedValue) {
        ("mode", ResolvedValue::Str(m.into()))
    }

    fn key(c: [f32; 4]) -> (&'static str, ResolvedValue) {
        ("key_color", ResolvedValue::Vec4(c))
    }

    const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const SKIN: [f32; 4] = [0.8, 0.5, 0.4, 1.0];

    /// A green backdrop with a 4 x 4 foreground of two colours at the centre.
    fn green_screen() -> FrameBuffer {
        let mut pixels = vec![GREEN; 64];
        for y in 2..6 {
            for x in 2..6 {
                pixels[y * 8 + x] = if x < 4 { RED } else { SKIN };
            }
        }
        frame(8, 8, &pixels)
    }

    /// Golden: a single-colour backdrop is keyed out, the foreground is kept
    /// untouched (RGB and alpha), whatever the brightness of the green.
    #[test]
    fn a_single_colour_backdrop_is_removed() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = green_screen();
        let out = run(
            &gpu,
            &[
                mode("chroma"),
                key(GREEN),
                f_pair("tolerance", 0.1),
                f_pair("softness", 0.0),
            ],
            &input,
        );
        for y in 0..8usize {
            for x in 0..8usize {
                let p = &out.as_f32()[(y * 8 + x) * 4..][..4];
                let i = &input.as_f32()[(y * 8 + x) * 4..][..4];
                if (2..6).contains(&x) && (2..6).contains(&y) {
                    assert_eq!(p, i, "foreground ({x},{y}) is untouched");
                } else {
                    assert_eq!(p[3], 0.0, "backdrop ({x},{y}) is keyed out");
                    assert_eq!(p[..3], i[..3], "keying never changes RGB");
                }
            }
        }
        // A dim green (same chroma, lower luminance) is removed too.
        let dim = frame(2, 1, &[[0.0, 0.4, 0.0, 1.0], RED]);
        let out = run(
            &gpu,
            &[
                mode("chroma"),
                key(GREEN),
                f_pair("tolerance", 0.1),
                f_pair("softness", 0.0),
            ],
            &dim,
        );
        assert_eq!(out.as_f32()[3], 0.0);
        assert_eq!(out.as_f32()[7], 1.0);
    }

    fn f_pair(key: &'static str, v: f32) -> (&'static str, ResolvedValue) {
        (key, f(v))
    }

    #[test]
    fn softness_ramps_the_alpha_and_invert_keeps_the_keyed_range() {
        let Some(gpu) = gpu_or_skip() else { return };
        // The chroma distance of SKIN from GREEN, worked out here from the
        // definition; tolerance / softness are then placed around it.
        let cbcr = |c: [f32; 4]| {
            let m = c[0].max(c[1]).max(c[2]);
            let n = [c[0] / m, c[1] / m, c[2] / m];
            let y = 0.2126 * n[0] + 0.7152 * n[1] + 0.0722 * n[2];
            [(n[2] - y) / 1.8556, (n[0] - y) / 1.5748]
        };
        let (a, b) = (cbcr(SKIN), cbcr(GREEN));
        let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
        let input = frame(1, 1, &[SKIN]);
        // d sits a third of the way up the ramp.
        let soft = 0.3;
        let tol = d - soft / 3.0;
        let base = [
            mode("chroma"),
            key(GREEN),
            f_pair("tolerance", tol),
            f_pair("softness", soft),
        ];
        let out = run(&gpu, &base, &input);
        assert!(
            (out.as_f32()[3] - 1.0 / 3.0).abs() < 1e-4,
            "{:?}",
            out.as_f32()
        );
        let mut inverted = base.to_vec();
        inverted.push(("invert", ResolvedValue::Bool(true)));
        let out = run(&gpu, &inverted, &input);
        assert!(
            (out.as_f32()[3] - 2.0 / 3.0).abs() < 1e-4,
            "{:?}",
            out.as_f32()
        );
        // The alpha of the input is multiplied, not replaced.
        let half = frame(1, 1, &[[0.8, 0.5, 0.4, 0.5]]);
        let out = run(&gpu, &base, &half);
        assert!((out.as_f32()[3] - 0.5 / 3.0).abs() < 1e-4);
    }

    #[test]
    fn luma_mode_keys_by_brightness() {
        let Some(gpu) = gpu_or_skip() else { return };
        // Key colour black: pixels within the tolerance of black luminance go.
        let input = frame(
            3,
            1,
            &[
                [0.0, 0.0, 0.0, 1.0],
                [0.05, 0.05, 0.05, 1.0],
                [0.9, 0.9, 0.9, 1.0],
            ],
        );
        let out = run(
            &gpu,
            &[
                mode("luma"),
                key([0.0, 0.0, 0.0, 1.0]),
                f_pair("tolerance", 0.1),
                f_pair("softness", 0.0),
            ],
            &input,
        );
        let alphas: Vec<f32> = out.as_f32().chunks_exact(4).map(|p| p[3]).collect();
        assert_eq!(alphas, [0.0, 0.0, 1.0]);
        // Chroma mode would not tell these grays apart from black (no chroma).
        let out = run(
            &gpu,
            &[
                mode("chroma"),
                key([0.0, 0.0, 0.0, 1.0]),
                f_pair("tolerance", 0.1),
                f_pair("softness", 0.0),
            ],
            &input,
        );
        assert!(out.as_f32().chunks_exact(4).all(|p| p[3] == 0.0));
    }

    #[test]
    fn an_unknown_mode_passes_the_image_through() {
        let Some(gpu) = gpu_or_skip() else { return };
        let input = green_screen();
        let mut shaders = ShaderManager::new(gpu.clone());
        let processor = CompKeyProcessor::new(gpu.clone(), &mut shaders, pool(&gpu));
        let mut params = ResolvedParams::default();
        params.set("mode", ResolvedValue::Str("no_such_mode".into()));
        let out = processor
            .process(
                &Node::new(NodeId::new(1), "comp.key"),
                &ctx((8, 8)),
                &[Some(Arc::new(input.clone()))],
                &params,
                &mut Evaluator::new(),
            )
            .unwrap();
        assert_eq!(
            out.downcast_ref::<FrameBuffer>().unwrap().as_f32(),
            input.as_f32()
        );
        assert!(!comp_key_mode_is_known("no_such_mode"));
        assert!(KEY_MODES.iter().all(|m| comp_key_mode_is_known(m)));
    }
}
