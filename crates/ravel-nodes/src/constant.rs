// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Constant value generator (CPU-only).

use ravel_core::eval::{EvalContext, NodeProcessor};
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::types::{NodeData, Scalar};

pub struct ConstantProcessor {
    value: f32,
}

impl ConstantProcessor {
    pub fn from_node(node: &Node) -> Self {
        let value = node
            .parameters
            .iter()
            .find(|p| p.key == "value")
            .and_then(|p| match &p.value {
                ParameterValue::Float(v) => Some(*v),
                _ => None,
            })
            .unwrap_or(0.0);
        Self { value }
    }
}

impl NodeProcessor for ConstantProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        _inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        Ok(Box::new(Scalar(self.value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::Graph;
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::types::{FrameRate, Scalar};
    use std::sync::Arc;

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
        let proc = ConstantProcessor::from_node(&node);

        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(proc));

        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        let s = out.downcast_ref::<Scalar>().unwrap();
        assert!((s.0 - 42.5).abs() < f32::EPSILON);
    }

    #[test]
    fn default_value_is_zero() {
        let node = Node::new(NodeId::new(1), "constant").with_output("value", DataTypeId::SCALAR);
        let proc = ConstantProcessor::from_node(&node);
        let result = proc.process(&ctx(), &[]).unwrap();
        let s = result.downcast_ref::<Scalar>().unwrap();
        assert!((s.0).abs() < f32::EPSILON);
    }

    #[test]
    fn is_not_time_dependent() {
        let node = make_constant_node(1, 1.0);
        let proc = ConstantProcessor::from_node(&node);
        assert!(!proc.is_time_dependent());
    }
}
