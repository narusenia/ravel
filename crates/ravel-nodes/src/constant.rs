// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Constant value generator (CPU-only).

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::{NodeData, Scalar};
use std::sync::Arc;

pub struct ConstantProcessor;

impl ConstantProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for ConstantProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(Scalar(params.f32_or("value", 0.0))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::types::FrameRate;

    fn make_constant_node(id: u64, value: f32) -> Node {
        Node::new(NodeId::new(id), "constant")
            .with_output("value", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(value))
    }

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    #[test]
    fn outputs_configured_value() {
        let node = make_constant_node(1, 42.5);
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(ConstantProcessor));

        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        let s = out.downcast_ref::<Scalar>().unwrap();
        assert!((s.0 - 42.5).abs() < f32::EPSILON);
    }

    #[test]
    fn default_value_is_zero() {
        let node = Node::new(NodeId::new(1), "constant").with_output("value", DataTypeId::SCALAR);
        let mut scope = Evaluator::new();
        let result = ConstantProcessor
            .process(&node, &ctx(), &[], &ResolvedParams::default(), &mut scope)
            .unwrap();
        let s = result.downcast_ref::<Scalar>().unwrap();
        assert!((s.0).abs() < f32::EPSILON);
    }

    #[test]
    fn is_not_time_dependent() {
        assert!(!ConstantProcessor.is_time_dependent());
    }
}
