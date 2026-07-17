// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `layer.ref` — cross-layer reference inside a composition (REQ-LAYER-005).
//!
//! References another layer's `net.out` port (`frame` or a custom port) in
//! the **same** composition, returning the target network's raw
//! pre-transform output with the target shell's time placement applied: the
//! target network is evaluated at the target's layer-local time derived from
//! the shared composition time. Outside the target's display interval the
//! node yields a typed zero (transparent frame / empty geometry / zero).
//!
//! Cycles are rejected at validation time
//! ([`ravel_core::composition::validate::validate_layer_ref_cycles`]); the
//! evaluator's scope re-entry guard additionally fails cyclic pulls at
//! runtime instead of recursing forever.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, PathSegment, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::id::LayerId;
use ravel_core::network as net;
use ravel_core::types::{NodeData, PortRecord};
use std::sync::Arc;

use crate::net::zero_value;

pub struct LayerRefProcessor;

impl LayerRefProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for LayerRefProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let target_raw = params.i32_or("layer", -1);
        anyhow::ensure!(target_raw >= 0, "layer.ref: no target layer set");
        let target_id = LayerId::new(target_raw as u64);
        let port_name = params.str_or("port", net::PORT_FRAME).to_string();

        // The enclosing layer scope identifies "the same composition"
        // (REQ-LAYER-005: same-comp references only in v1).
        let Some(&PathSegment::Layer(comp_id, source_id)) = scope
            .path()
            .iter()
            .rev()
            .find(|s| matches!(s, PathSegment::Layer(..)))
        else {
            anyhow::bail!("layer.ref: not evaluated inside a layer network");
        };

        let document = scope
            .document()
            .ok_or_else(|| anyhow::anyhow!("layer.ref: no document set on the evaluator"))?;
        let comp = document
            .get_composition(comp_id)
            .ok_or_else(|| anyhow::anyhow!("layer.ref: composition {comp_id:?} missing"))?
            .clone();
        let source = comp
            .get_layer(source_id)
            .ok_or_else(|| anyhow::anyhow!("layer.ref: layer {source_id:?} missing"))?;
        let target = comp.get_layer(target_id).ok_or_else(|| {
            anyhow::anyhow!("layer.ref: target layer {target_id:?} not in composition {comp_id:?}")
        })?;

        // ctx is the source layer's local time; map back to composition
        // time, then into the target's local time (REQ-LAYER-006).
        let comp_frame = ctx.frame as i64 + source.start_frame - source.in_frame as i64;
        let target_local = comp_frame - target.start_frame + target.in_frame as i64;

        let zero = || zero_value(node.outputs.first().map(|p| &p.data_type), ctx);
        if target_local < target.in_frame as i64 || target_local >= target.out_frame as i64 {
            return Ok(zero());
        }

        let out_node = net::find_out_node(&target.network).ok_or_else(|| {
            anyhow::anyhow!("layer.ref: target layer {target_id:?} has no net.out node")
        })?;
        let port_index = out_node
            .inputs
            .iter()
            .position(|p| p.name == port_name)
            .ok_or_else(|| {
                anyhow::anyhow!("layer.ref: target has no out port named {port_name:?}")
            })?;

        let local = EvalContext {
            frame: target_local as u64,
            time: target_local as f64 / comp.frame_rate.as_f64(),
            fps: comp.frame_rate,
            resolution: comp.resolution,
        };

        let value = scope.evaluate_sub(
            PathSegment::Layer(comp_id, target_id),
            &target.network,
            out_node.id,
            &local,
            Vec::new(),
        )?;
        value
            .downcast_ref::<PortRecord>()
            .and_then(|record| record.0.get(port_index).cloned())
            .ok_or_else(|| anyhow::anyhow!("layer.ref: target net.out produced no port record"))
    }

    fn is_time_dependent(&self) -> bool {
        true
    }
}
