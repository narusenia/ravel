// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.transform` — the shell's built-in layer transform (REQ-LAYER-001).
//!
//! Applies the owning layer's animatable transform channels (anchor point,
//! position, scale, rotation in degrees) to the layer's frame, composing the
//! parent chain's transforms on top (P/R/S inheritance, REQ-LAYER-001).
//! Channel values are read from the [`Document`] at process time — nothing
//! is captured at construction — and evaluated at the owning layer's local
//! frame (keyframes live in layer-local frames, REQ-LAYER-006).
//!
//! The matrix math lives in [`ravel_core::composition::transform`] so the
//! viewer's bbox and hit test compose the parent chain exactly the way these
//! pixels do.

use ravel_core::composition::compile::NodeRole;
use ravel_core::composition::transform::world_matrix;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::{FrameBuffer, NodeData};
use std::sync::Arc;

use super::{shell_layer, transparent};
use crate::gpu_util::ensure_cpu;

// ===========================================================================
// Processor
// ===========================================================================

/// Applies the owning layer's (and its parent chain's) transform to the
/// frame via inverse mapping with premultiplied bilinear sampling.
/// Tolerates a missing input so null layers — which keep a Transform node
/// for parenting — evaluate cleanly.
pub struct CompTransformProcessor;

impl CompTransformProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for CompTransformProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let Some(input) = inputs.first().and_then(|i| i.clone()) else {
            return Ok(transparent(ctx));
        };

        let (comp, layer_id) = shell_layer(node, scope, NodeRole::Transform)?;
        let layer = comp
            .get_layer(layer_id)
            .ok_or_else(|| anyhow::anyhow!("comp.transform: layer {layer_id:?} missing"))?;

        let matrix = world_matrix(&comp, layer, ctx);
        if matrix.is_identity() {
            return Ok(input);
        }
        let Some(inverse) = matrix.inverse() else {
            // Degenerate transform (zero scale) collapses the layer.
            return Ok(transparent(ctx));
        };

        let source = ensure_cpu(input.as_ref())?;
        let (width, height) = ctx.resolution;
        let mut pixels = vec![0.0f32; width as usize * height as usize * 4];
        for y in 0..height {
            for x in 0..width {
                let (sx, sy) = inverse.apply(x as f32 + 0.5, y as f32 + 0.5);
                let rgba = sample_bilinear(&source, sx, sy);
                let idx = ((y * width + x) * 4) as usize;
                pixels[idx..idx + 4].copy_from_slice(&rgba);
            }
        }
        Ok(Arc::new(FrameBuffer {
            width,
            height,
            data: pixels.into(),
        }))
    }

    fn is_time_dependent(&self) -> bool {
        // Layer transform channels are hidden (document-side) dependencies.
        true
    }
}

/// Bilinear sample at pixel-space `(sx, sy)`; interpolation happens in
/// premultiplied alpha to avoid fringing, and the result is converted back
/// to the straight-alpha buffer convention. Outside the source: transparent.
fn sample_bilinear(fb: &FrameBuffer, sx: f32, sy: f32) -> [f32; 4] {
    let fx = sx - 0.5;
    let fy = sy - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = fx - x0;
    let ty = fy - y0;

    let mut acc = [0.0f32; 4];
    for (dx, dy, w) in [
        (0.0, 0.0, (1.0 - tx) * (1.0 - ty)),
        (1.0, 0.0, tx * (1.0 - ty)),
        (0.0, 1.0, (1.0 - tx) * ty),
        (1.0, 1.0, tx * ty),
    ] {
        if w <= 0.0 {
            continue;
        }
        let p = premultiplied_at(fb, x0 + dx, y0 + dy);
        for (a, v) in acc.iter_mut().zip(p) {
            *a += w * v;
        }
    }
    if acc[3] > 0.0 {
        [acc[0] / acc[3], acc[1] / acc[3], acc[2] / acc[3], acc[3]]
    } else {
        [0.0; 4]
    }
}

fn premultiplied_at(fb: &FrameBuffer, x: f32, y: f32) -> [f32; 4] {
    if x < 0.0 || y < 0.0 || x >= fb.width as f32 || y >= fb.height as f32 {
        return [0.0; 4];
    }
    let idx = ((y as u32 * fb.width + x as u32) * 4) as usize;
    let p = &fb.data[idx..idx + 4];
    [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]
}
