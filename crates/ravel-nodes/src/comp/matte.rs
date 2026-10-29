// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.matte.*` — the shell's track-matte stage.
//!
//! Sits between a layer's Opacity and Merge nodes and cuts the layer out by
//! its matte source's result (the source layer's network → transform →
//! opacity chain at the same composition frame):
//!
//! | type key | matte value `m` |
//! |---|---|
//! | `comp.matte.alpha` | `a` |
//! | `comp.matte.alpha_inverted` | `1 - a` |
//! | `comp.matte.luma` | `luma(rgb) * a` |
//! | `comp.matte.luma_inverted` | `1 - luma(rgb) * a` |
//!
//! The output is the input with `a' = a * m`; RGB is kept, because frames
//! carry straight alpha. Luma is Rec.709 of the matte's **straight** colour
//! multiplied by its alpha, which is the same as the luma of its premultiplied
//! colour — so a transparent matte pixel is 0 whatever colour it carries, and
//! the number agrees with `comp.alpha`'s `matte_luma`.
//!
//! The matte kind is encoded in the node's type key, so editing it recompiles
//! the shell chain like a blend-mode change does; the processor reads nothing
//! from the `Document`. A missing matte input passes the layer through
//! unchanged. This is the CPU path; the frame is read back if it is
//! GPU-resident.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::{FrameBuffer, NodeData};
use std::sync::Arc;

use super::transparent;
use crate::gpu_util::ensure_cpu;

/// What the matte stage reads off the matte pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatteMode {
    Alpha,
    Luma,
}

/// Parse a `comp.matte.*` type key into `(mode, inverted)`.
fn matte_kind(type_key: &str) -> Option<(MatteMode, bool)> {
    match type_key {
        "comp.matte.alpha" => Some((MatteMode::Alpha, false)),
        "comp.matte.alpha_inverted" => Some((MatteMode::Alpha, true)),
        "comp.matte.luma" => Some((MatteMode::Luma, false)),
        "comp.matte.luma_inverted" => Some((MatteMode::Luma, true)),
        _ => None,
    }
}

/// The matte value of one straight-alpha RGBA pixel.
fn matte_value(mode: MatteMode, inverted: bool, px: &[f32]) -> f32 {
    let m = match mode {
        MatteMode::Alpha => px[3],
        MatteMode::Luma => (0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2]) * px[3],
    }
    .clamp(0.0, 1.0);
    if inverted { 1.0 - m } else { m }
}

pub struct CompMatteProcessor {
    mode: MatteMode,
    inverted: bool,
}

impl CompMatteProcessor {
    /// `None` for a type key that is not a `comp.matte.*` kind.
    pub fn from_node(node: &Node) -> Option<Self> {
        let (mode, inverted) = matte_kind(&node.type_key)?;
        Some(Self { mode, inverted })
    }
}

impl NodeProcessor for CompMatteProcessor {
    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let Some(input) = inputs.first().and_then(|i| i.clone()) else {
            return Ok(transparent(ctx));
        };
        let Some(matte) = inputs.get(1).and_then(|i| i.clone()) else {
            return Ok(input);
        };

        let source = ensure_cpu(input.as_ref())?;
        let matte = ensure_cpu(matte.as_ref())?;
        anyhow::ensure!(
            (source.width, source.height) == (matte.width, matte.height),
            "comp.matte: the matte is {}x{} but the layer is {}x{}",
            matte.width,
            matte.height,
            source.width,
            source.height
        );
        let mut pixels = source.as_rgba_f32()?.into_owned();
        let matte_px = matte.as_rgba_f32()?;
        for (px, m) in pixels.chunks_exact_mut(4).zip(matte_px.chunks_exact(4)) {
            px[3] *= matte_value(self.mode, self.inverted, m);
        }
        Ok(Arc::new(FrameBuffer::from_f32(
            source.width,
            source.height,
            pixels,
        )))
    }

    fn is_time_dependent(&self) -> bool {
        // A pure function of its inputs, but the inputs move with time.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_keys_map_to_modes() {
        assert_eq!(
            matte_kind("comp.matte.alpha"),
            Some((MatteMode::Alpha, false))
        );
        assert_eq!(
            matte_kind("comp.matte.luma_inverted"),
            Some((MatteMode::Luma, true))
        );
        assert_eq!(matte_kind("comp.matte.nope"), None);
    }

    #[test]
    fn matte_values() {
        let px = [1.0, 1.0, 1.0, 0.5];
        assert_eq!(matte_value(MatteMode::Alpha, false, &px), 0.5);
        assert_eq!(matte_value(MatteMode::Alpha, true, &px), 0.5);
        assert!((matte_value(MatteMode::Luma, false, &px) - 0.5).abs() < 1e-6);
        // A transparent pixel is 0 as a luma matte whatever colour it holds.
        assert_eq!(
            matte_value(MatteMode::Luma, false, &[1.0, 1.0, 1.0, 0.0]),
            0.0
        );
        assert_eq!(
            matte_value(MatteMode::Luma, true, &[1.0, 1.0, 1.0, 0.0]),
            1.0
        );
        // Pure green is brighter than pure blue in Rec.709.
        let green = matte_value(MatteMode::Luma, false, &[0.0, 1.0, 0.0, 1.0]);
        let blue = matte_value(MatteMode::Luma, false, &[0.0, 0.0, 1.0, 1.0]);
        assert!(green > 0.7 && blue < 0.1);
    }
}
