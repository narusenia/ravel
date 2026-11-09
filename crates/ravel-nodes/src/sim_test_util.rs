// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Test processors for the scope-input tests: a value that is its frame, a
//! simulation that sums what it is given, and a time node that reads its
//! input at an earlier frame. Each reads the **frame its input was pulled
//! at**, which is exactly what those tests look at.

use ravel_core::eval::{
    EvalContext, EvalScope, NodeProcessor, ResolvedParams, SimInputs, StatefulProcessor,
};
use ravel_core::graph::Node;
use ravel_core::types::{FrameBuffer, NodeData, Scalar};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The scalar `k * frame`.
pub(crate) struct FrameScalar(pub f32);

impl NodeProcessor for FrameScalar {
    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(Scalar(ctx.frame as f32 * self.0)))
    }

    fn is_time_dependent(&self) -> bool {
        true
    }
}

/// A 1x1 frame whose red channel is `k * frame`.
pub(crate) struct FrameBufferValue(pub f32);

impl NodeProcessor for FrameBufferValue {
    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(FrameBuffer::from_f32(
            1,
            1,
            vec![ctx.frame as f32 * self.0, 0.0, 0.0, 1.0],
        )))
    }

    fn is_time_dependent(&self) -> bool {
        true
    }
}

/// The number an input carries: a scalar, or the red of a frame's first pixel.
pub(crate) fn number_of(data: &dyn NodeData) -> f32 {
    if let Some(scalar) = data.downcast_ref::<Scalar>() {
        return scalar.0;
    }
    let frame = data.downcast_ref::<FrameBuffer>().expect("scalar or frame");
    frame.as_rgba_f32().expect("rgba f32")[0]
}

/// `state(0) = in(0)`, `state(t) = state(t - 1) + in(t)`, the input read at
/// frame `t` — so `state(t)` is the sum of the input over `0..=t` if and only
/// if every step was given its own frame's input. A wrong-frame input shows as
/// a different sum.
#[derive(Clone, Default)]
pub(crate) struct Accumulate {
    /// Output a frame instead of a scalar.
    pub as_frame: bool,
    /// Bytes charged per state (0 counts as 1), to drive checkpoint thinning.
    pub state_bytes: u64,
    /// How many times a track started from nothing.
    pub initials: Arc<AtomicUsize>,
}

fn input_number(inputs: &SimInputs<'_>) -> f32 {
    inputs
        .inputs
        .first()
        .and_then(|slot| slot.as_ref())
        .map_or(0.0, |data| number_of(data.as_ref()))
}

impl StatefulProcessor for Accumulate {
    type State = f32;

    fn initial(&self, _ctx: &EvalContext, inputs: &SimInputs<'_>) -> f32 {
        self.initials.fetch_add(1, Ordering::Relaxed);
        input_number(inputs)
    }

    fn step(&self, prev: &f32, _ctx: &EvalContext, inputs: &SimInputs<'_>) -> f32 {
        prev + input_number(inputs)
    }

    fn output(&self, state: &f32) -> Arc<dyn NodeData> {
        if self.as_frame {
            Arc::new(FrameBuffer::from_f32(1, 1, vec![*state, 0.0, 0.0, 1.0]))
        } else {
            Arc::new(Scalar(*state))
        }
    }

    fn byte_size(&self, _state: &f32) -> u64 {
        self.state_bytes.max(1)
    }
}

/// Reads its input `by` frames earlier than the frame it is processed at.
pub(crate) struct ShiftBack(pub u64);

impl NodeProcessor for ShiftBack {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let earlier = ctx.frame.saturating_sub(self.0);
        scope
            .evaluate_input_at(node, 0, earlier, ctx)?
            .ok_or_else(|| anyhow::anyhow!("nothing wired"))
    }

    fn is_time_dependent(&self) -> bool {
        true
    }

    fn pulls_inputs_itself(&self) -> bool {
        true
    }
}
