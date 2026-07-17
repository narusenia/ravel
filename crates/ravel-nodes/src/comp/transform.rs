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

use ravel_core::composition::compile::NodeRole;
use ravel_core::composition::{Composition, Layer};
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::{FrameBuffer, NodeData};
use std::sync::Arc;

use super::{layer_local_frame, shell_layer, transparent};
use crate::gpu_util::ensure_cpu;

// ===========================================================================
// 2D affine matrix
// ===========================================================================

/// Row-major 2×3 affine matrix: `x' = m0·x + m1·y + m2`, `y' = m3·x + m4·y + m5`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Affine(pub [f32; 6]);

impl Affine {
    pub const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);

    /// `self ∘ other`: apply `other` first, then `self`.
    pub fn mul(self, other: Affine) -> Affine {
        let a = self.0;
        let b = other.0;
        Affine([
            a[0] * b[0] + a[1] * b[3],
            a[0] * b[1] + a[1] * b[4],
            a[0] * b[2] + a[1] * b[5] + a[2],
            a[3] * b[0] + a[4] * b[3],
            a[3] * b[1] + a[4] * b[4],
            a[3] * b[2] + a[4] * b[5] + a[5],
        ])
    }

    pub fn apply(self, x: f32, y: f32) -> (f32, f32) {
        let m = self.0;
        (m[0] * x + m[1] * y + m[2], m[3] * x + m[4] * y + m[5])
    }

    /// Inverse, or `None` when the matrix is singular (e.g. zero scale).
    pub fn inverse(self) -> Option<Affine> {
        let m = self.0;
        let det = m[0] * m[4] - m[1] * m[3];
        if det.abs() < 1e-10 {
            return None;
        }
        let inv_det = 1.0 / det;
        let a = m[4] * inv_det;
        let b = -m[1] * inv_det;
        let d = -m[3] * inv_det;
        let e = m[0] * inv_det;
        Some(Affine([
            a,
            b,
            -(a * m[2] + b * m[5]),
            d,
            e,
            -(d * m[2] + e * m[5]),
        ]))
    }

    pub fn is_identity(self) -> bool {
        let m = self.0;
        let i = Affine::IDENTITY.0;
        m.iter().zip(i).all(|(a, b)| (a - b).abs() < 1e-6)
    }
}

/// The layer's local transform matrix at its local frame `lf`:
/// `T(position) · R(rotation°) · S(scale) · T(-anchor)`.
pub(crate) fn layer_matrix(layer: &Layer, lf: u64, ctx: &EvalContext) -> Affine {
    let t = &layer.transform;
    let ax = t.anchor_point[0].evaluate(lf, ctx);
    let ay = t.anchor_point[1].evaluate(lf, ctx);
    let px = t.position[0].evaluate(lf, ctx);
    let py = t.position[1].evaluate(lf, ctx);
    let sx = t.scale[0].evaluate(lf, ctx);
    let sy = t.scale[1].evaluate(lf, ctx);
    let rot = t.rotation.evaluate(lf, ctx).to_radians();
    let (sin, cos) = rot.sin_cos();

    // T(px, py) · R · S · T(-ax, -ay), composed directly.
    Affine([
        cos * sx,
        -sin * sy,
        px - (cos * sx * ax - sin * sy * ay),
        sin * sx,
        cos * sy,
        py - (sin * sx * ax + cos * sy * ay),
    ])
}

/// The layer's world matrix: the parent chain composed onto the layer's own
/// matrix. Every ancestor's channels are evaluated at that ancestor's own
/// local frame. Parent cycles are rejected by validation; a visited guard
/// keeps evaluation robust regardless.
pub(crate) fn world_matrix(comp: &Composition, layer: &Layer, ctx: &EvalContext) -> Affine {
    let mut matrix = layer_matrix(layer, layer_local_frame(layer, ctx), ctx);
    let mut visited = vec![layer.id];
    let mut current = layer.parent;
    while let Some(parent_id) = current {
        if visited.contains(&parent_id) {
            break;
        }
        let Some(parent) = comp.get_layer(parent_id) else {
            break;
        };
        visited.push(parent_id);
        matrix = layer_matrix(parent, layer_local_frame(parent, ctx), ctx).mul(matrix);
        current = parent.parent;
    }
    matrix
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_inverse_roundtrip() {
        let m = Affine([1.5, 0.2, 10.0, -0.3, 2.0, -4.0]);
        let inv = m.inverse().unwrap();
        let (x, y) = m.apply(3.0, 7.0);
        let (rx, ry) = inv.apply(x, y);
        assert!((rx - 3.0).abs() < 1e-4 && (ry - 7.0).abs() < 1e-4);
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        assert!(Affine([0.0; 6]).inverse().is_none());
    }

    #[test]
    fn identity_detection() {
        assert!(Affine::IDENTITY.is_identity());
        assert!(!Affine([1.0, 0.0, 5.0, 0.0, 1.0, 0.0]).is_identity());
    }
}
