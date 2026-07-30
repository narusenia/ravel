// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.background` — the composition background at the bottom of the shell chain.

use ravel_core::composition::compile::{NodeRole, decode_deterministic_node_id};
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::{FrameBuffer, NodeData};
use std::sync::Arc;

use crate::scaled_resolution;

pub struct CompBackgroundProcessor;

impl CompBackgroundProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for CompBackgroundProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let (comp_id, _, role) = decode_deterministic_node_id(node.id)
            .ok_or_else(|| anyhow::anyhow!("comp.background: non-deterministic node id"))?;
        anyhow::ensure!(
            role == NodeRole::Background,
            "comp.background: node id role mismatch"
        );
        let document = scope
            .document()
            .ok_or_else(|| anyhow::anyhow!("comp.background: no document set on the evaluator"))?;
        let comp = document
            .get_composition(comp_id)
            .ok_or_else(|| anyhow::anyhow!("comp.background: composition {comp_id:?} missing"))?;
        let resolution = scaled_resolution(ctx, comp.resolution);
        let color = comp.background_color;
        let mut data = Vec::with_capacity(resolution.0 as usize * resolution.1 as usize * 4);
        for _ in 0..resolution.0 as usize * resolution.1 as usize {
            data.extend_from_slice(&[color.r, color.g, color.b, color.a]);
        }
        Ok(Arc::new(FrameBuffer {
            width: resolution.0,
            height: resolution.1,
            data: Arc::from(data),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::compile::compile_composition;
    use ravel_core::composition::{Composition, Document};
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::Graph;
    use ravel_core::id::CompId;
    use ravel_core::types::{Color, FrameRate};

    fn evaluate_background(color: Color) -> FrameBuffer {
        let mut comp = Composition::new(
            CompId::new(7),
            "Background",
            (2, 1),
            FrameRate::new(30, 1),
            30,
        );
        comp.background_color = color;
        let compiled = compile_composition(&comp, Graph::new()).unwrap();
        let mut evaluator = Evaluator::new();
        let node = compiled.graph.node(compiled.output_node).unwrap();
        evaluator.register(node.id, Arc::new(CompBackgroundProcessor::from_node(node)));
        evaluator.set_document(Arc::new(Document::default().with_composition(comp)));
        let output = evaluator
            .evaluate(
                &compiled.graph,
                compiled.output_node,
                &EvalContext::new(0, FrameRate::new(30, 1), (2, 1)),
            )
            .unwrap();
        output.downcast_ref::<FrameBuffer>().unwrap().clone()
    }

    #[test]
    fn composition_color_fills_the_evaluation_background() {
        let frame = evaluate_background(Color::new(0.25, 0.5, 0.75, 1.0));
        assert_eq!(&*frame.data, &[0.25, 0.5, 0.75, 1.0, 0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn transparent_composition_background_preserves_zero_alpha() {
        let frame = evaluate_background(Color::new(0.2, 0.4, 0.6, 0.0));
        assert_eq!(frame.data[3], 0.0);
        assert_eq!(frame.data[7], 0.0);
    }
}
