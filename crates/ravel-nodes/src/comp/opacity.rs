// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.opacity` — the shell's layer opacity (REQ-LAYER-001).
//!
//! Multiplies the frame's alpha channel by the owning layer's animatable
//! opacity, evaluated at the layer's local frame. The layer is read from the
//! [`Document`] at process time (never captured at construction).

use ravel_core::composition::compile::NodeRole;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::{FrameBuffer, NodeData};
use std::sync::Arc;

use super::{layer_local_frame, shell_layer, transparent};
use crate::gpu_util::ensure_cpu;

pub struct CompOpacityProcessor;

impl CompOpacityProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for CompOpacityProcessor {
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

        let (comp, layer_id) = shell_layer(node, scope, NodeRole::Opacity)?;
        let layer = comp
            .get_layer(layer_id)
            .ok_or_else(|| anyhow::anyhow!("comp.opacity: layer {layer_id:?} missing"))?;

        let lf = layer_local_frame(layer, ctx);
        let opacity = layer.opacity.evaluate(lf, ctx).clamp(0.0, 1.0);
        if (opacity - 1.0).abs() < 1e-6 {
            return Ok(input);
        }

        let source = ensure_cpu(input.as_ref())?;
        let mut pixels = source.data.to_vec();
        for px in pixels.chunks_exact_mut(4) {
            px[3] *= opacity;
        }
        Ok(Arc::new(FrameBuffer {
            width: source.width,
            height: source.height,
            data: pixels.into(),
        }))
    }

    fn is_time_dependent(&self) -> bool {
        // The layer opacity channel is a hidden (document-side) dependency.
        true
    }
}
