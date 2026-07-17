// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.merge.*` — layer compositing for the shell's merge chain
//! (REQ-LAYER-001/010).
//!
//! Straight-alpha Porter-Duff *over* with per-mode color blending (the W3C
//! compositing model: the foreground color is mixed with `B(Cb, Cf)` by the
//! backdrop's alpha before compositing). `comp.merge.adjustment` instead
//! mixes the adjusted stack over the original background with the layer's
//! opacity as effect strength (REQ-LAYER-010).

use ravel_core::composition::compile::{NodeRole, decode_deterministic_node_id};
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::{FrameBuffer, NodeData};
use std::sync::Arc;

use super::{layer_local_frame, transparent};
use crate::gpu_util::ensure_cpu;

#[derive(Clone, Copy, PartialEq)]
enum MergeMode {
    Normal,
    Add,
    Multiply,
    Screen,
    Overlay,
    Adjustment,
}

fn merge_mode(type_key: &str) -> anyhow::Result<MergeMode> {
    Ok(match type_key {
        "comp.merge.normal" => MergeMode::Normal,
        "comp.merge.add" => MergeMode::Add,
        "comp.merge.multiply" => MergeMode::Multiply,
        "comp.merge.screen" => MergeMode::Screen,
        "comp.merge.overlay" => MergeMode::Overlay,
        "comp.merge.adjustment" => MergeMode::Adjustment,
        other => anyhow::bail!("comp.merge: unknown type key {other}"),
    })
}

/// Per-channel color blend `B(Cb, Cf)` on straight colors.
fn blend(mode: MergeMode, cb: f32, cf: f32) -> f32 {
    match mode {
        MergeMode::Normal => cf,
        MergeMode::Add => cb + cf,
        MergeMode::Multiply => cb * cf,
        MergeMode::Screen => cb + cf - cb * cf,
        MergeMode::Overlay => {
            if cb <= 0.5 {
                2.0 * cb * cf
            } else {
                1.0 - 2.0 * (1.0 - cb) * (1.0 - cf)
            }
        }
        // Adjustment merges mix whole frames; see `mix_frames`.
        MergeMode::Adjustment => cf,
    }
}

pub struct CompMergeProcessor;

impl CompMergeProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for CompMergeProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let mode = merge_mode(&node.type_key)?;
        // inputs[0] = background, inputs[1] = foreground.
        let background = inputs.first().and_then(|i| i.clone());
        let foreground = inputs.get(1).and_then(|i| i.clone());

        if mode == MergeMode::Adjustment {
            return merge_adjustment(node, ctx, background, foreground, scope);
        }

        // One side missing: compositing against transparency is the color
        // identity for every mode, but the output must still be normalized
        // to the composition resolution (a lone video layer may carry the
        // media's native dimensions). Same-size frames pass through.
        let (background, foreground) = match (background, foreground) {
            (None, None) => return Ok(transparent(ctx)),
            (bg, fg) => {
                if let (None, Some(only)) | (Some(only), None) = (&bg, &fg) {
                    match frame_dims(only.as_ref()) {
                        // Undersized/oversized frames are padded/cropped by
                        // the compositing loop below.
                        Some(dims) if dims != ctx.resolution => {}
                        // Right-sized frames — and non-frame values (scalar
                        // probes etc.) — pass through untouched.
                        _ => return Ok(only.clone()),
                    }
                }
                (
                    bg.unwrap_or_else(empty_frame),
                    fg.unwrap_or_else(empty_frame),
                )
            }
        };

        let bg = ensure_cpu(background.as_ref())?;
        let fg = ensure_cpu(foreground.as_ref())?;
        let (width, height) = ctx.resolution;
        let mut pixels = vec![0.0f32; width as usize * height as usize * 4];
        for y in 0..height {
            for x in 0..width {
                let b = pixel_at(&bg, x, y);
                let f = pixel_at(&fg, x, y);
                let out = composite(mode, b, f);
                let idx = ((y * width + x) * 4) as usize;
                pixels[idx..idx + 4].copy_from_slice(&out);
            }
        }
        Ok(Arc::new(FrameBuffer {
            width,
            height,
            data: pixels.into(),
        }))
    }

    fn is_time_dependent(&self) -> bool {
        // Display-interval checks and the adjustment strength read the
        // document per frame.
        true
    }
}

/// Straight-alpha compositing of foreground `f` over background `b` with the
/// mode's color blend applied where the two overlap.
fn composite(mode: MergeMode, b: [f32; 4], f: [f32; 4]) -> [f32; 4] {
    let ab = b[3];
    let af = f[3];
    let ao = af + ab * (1.0 - af);
    if ao <= 0.0 {
        return [0.0; 4];
    }
    let mut out = [0.0f32; 4];
    for c in 0..3 {
        let blended = blend(mode, b[c], f[c]);
        let mixed = (1.0 - ab) * f[c] + ab * blended;
        out[c] = (af * mixed + (1.0 - af) * ab * b[c]) / ao;
    }
    out[3] = ao;
    out
}

/// Adjustment layer merge: `mix(background, adjusted, opacity)` where
/// `adjusted` is the layer network's output over the lower stack and the
/// layer's opacity acts as effect strength. Outside the layer's display
/// interval the background passes through untouched (REQ-LAYER-010).
fn merge_adjustment(
    node: &Node,
    ctx: &EvalContext,
    background: Option<Arc<dyn NodeData>>,
    foreground: Option<Arc<dyn NodeData>>,
    scope: &mut dyn EvalScope,
) -> anyhow::Result<Arc<dyn NodeData>> {
    let background = background.unwrap_or_else(|| transparent(ctx));

    let strength = adjustment_strength(node, ctx, scope);
    let Some(strength) = strength else {
        // Outside the display interval (or the layer vanished): bypass.
        return Ok(background);
    };
    let Some(foreground) = foreground else {
        return Ok(background);
    };
    if strength <= 0.0 {
        return Ok(background);
    }
    if (strength - 1.0).abs() < 1e-6 && frame_dims(foreground.as_ref()) == Some(ctx.resolution) {
        return Ok(foreground);
    }

    let bg = ensure_cpu(background.as_ref())?;
    let fg = ensure_cpu(foreground.as_ref())?;
    let (width, height) = ctx.resolution;
    let mut pixels = vec![0.0f32; width as usize * height as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let b = premultiply(pixel_at(&bg, x, y));
            let f = premultiply(pixel_at(&fg, x, y));
            let mut mixed = [0.0f32; 4];
            for c in 0..4 {
                mixed[c] = b[c] * (1.0 - strength) + f[c] * strength;
            }
            let idx = ((y * width + x) * 4) as usize;
            pixels[idx..idx + 4].copy_from_slice(&unpremultiply(mixed));
        }
    }
    Ok(Arc::new(FrameBuffer {
        width,
        height,
        data: pixels.into(),
    }))
}

/// The adjustment layer's opacity at the current frame, or `None` when the
/// layer is outside its display interval (bypass) or cannot be resolved.
fn adjustment_strength(node: &Node, ctx: &EvalContext, scope: &mut dyn EvalScope) -> Option<f32> {
    let (comp_id, layer_id, role) = decode_deterministic_node_id(node.id)?;
    if role != NodeRole::Merge {
        return None;
    }
    let document = scope.document()?;
    let comp = document.get_composition(comp_id)?;
    let layer = comp.get_layer(layer_id)?;
    let local = ctx.frame as i64 - layer.start_frame + layer.in_frame as i64;
    if local < layer.in_frame as i64 || local >= layer.out_frame as i64 {
        return None;
    }
    let lf = layer_local_frame(layer, ctx);
    Some(layer.opacity.evaluate(lf, ctx).clamp(0.0, 1.0))
}

/// Dimensions of a CPU- or GPU-resident frame, without any readback.
fn frame_dims(value: &dyn NodeData) -> Option<(u32, u32)> {
    if let Some(fb) = value.downcast_ref::<FrameBuffer>() {
        return Some((fb.width, fb.height));
    }
    value
        .downcast_ref::<ravel_gpu::GpuFrameBuffer>()
        .map(|fb| (fb.width(), fb.height()))
}

/// Zero-sized stand-in for a missing merge input: `pixel_at` reads it as
/// fully transparent everywhere.
fn empty_frame() -> Arc<dyn NodeData> {
    Arc::new(FrameBuffer::new_zeroed(0, 0))
}

fn pixel_at(fb: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
    if x >= fb.width || y >= fb.height {
        return [0.0; 4];
    }
    let idx = ((y * fb.width + x) * 4) as usize;
    fb.data[idx..idx + 4].try_into().unwrap_or([0.0; 4])
}

fn premultiply(p: [f32; 4]) -> [f32; 4] {
    [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]
}

fn unpremultiply(p: [f32; 4]) -> [f32; 4] {
    if p[3] > 0.0 {
        [p[0] / p[3], p[1] / p[3], p[2] / p[3], p[3]]
    } else {
        [0.0; 4]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_over_matches_porter_duff() {
        // Opaque red under half-transparent green.
        let out = composite(
            MergeMode::Normal,
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 0.5],
        );
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
        assert!((out[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn add_blend_sums_where_opaque() {
        let out = composite(MergeMode::Add, [0.25, 0.0, 0.0, 1.0], [0.5, 0.0, 0.0, 1.0]);
        assert!((out[0] - 0.75).abs() < 1e-6, "{out:?}");
    }

    #[test]
    fn multiply_blend_darkens() {
        let out = composite(
            MergeMode::Multiply,
            [0.5, 0.5, 0.5, 1.0],
            [0.5, 0.5, 0.5, 1.0],
        );
        assert!((out[0] - 0.25).abs() < 1e-6, "{out:?}");
    }

    #[test]
    fn screen_blend_brightens() {
        let out = composite(
            MergeMode::Screen,
            [0.5, 0.5, 0.5, 1.0],
            [0.5, 0.5, 0.5, 1.0],
        );
        assert!((out[0] - 0.75).abs() < 1e-6, "{out:?}");
    }

    #[test]
    fn overlay_splits_on_backdrop_midpoint() {
        let dark = composite(
            MergeMode::Overlay,
            [0.25, 0.25, 0.25, 1.0],
            [0.5, 0.5, 0.5, 1.0],
        );
        assert!((dark[0] - 0.25).abs() < 1e-6, "{dark:?}");
        let bright = composite(
            MergeMode::Overlay,
            [0.75, 0.75, 0.75, 1.0],
            [0.5, 0.5, 0.5, 1.0],
        );
        assert!((bright[0] - 0.75).abs() < 1e-6, "{bright:?}");
    }

    #[test]
    fn transparent_foreground_keeps_background() {
        let out = composite(MergeMode::Normal, [0.2, 0.4, 0.6, 0.8], [0.0; 4]);
        assert!((out[0] - 0.2).abs() < 1e-6 && (out[3] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn blend_modes_reduce_to_foreground_over_transparent_backdrop() {
        for mode in [
            MergeMode::Add,
            MergeMode::Multiply,
            MergeMode::Screen,
            MergeMode::Overlay,
        ] {
            let out = composite(mode, [0.0; 4], [0.3, 0.6, 0.9, 0.5]);
            assert!(
                (out[0] - 0.3).abs() < 1e-6 && (out[3] - 0.5).abs() < 1e-6,
                "{out:?}"
            );
        }
    }
}
