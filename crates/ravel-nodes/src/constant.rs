// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Constant value generators (CPU-only).

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::{Color, NodeData, Scalar};
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

/// RGB color constant (`constant.color`): emits the animatable `color`
/// parameter as a [`Color`] value, e.g. feeding the rasterize `color` pin in
/// the Solid layer template (REQ-LAYER-008).
pub struct ColorConstantProcessor;

impl ColorConstantProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for ColorConstantProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let [r, g, b, a] = params.vec4_or("color", {
            let [r, g, b] = params.vec3_or("color", [1.0, 1.0, 1.0]);
            [r, g, b, 1.0]
        });
        Ok(Arc::new(Color::new(r, g, b, a)))
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

    #[test]
    fn color_constant_outputs_channel_values() {
        use ravel_core::animation::channel::AnimationChannel;
        let node = Node::new(NodeId::new(1), "constant.color")
            .with_output("color", DataTypeId::COLOR)
            .with_param(
                "color",
                ParameterValue::Channel4([
                    AnimationChannel::constant(0.2),
                    AnimationChannel::constant(0.4),
                    AnimationChannel::constant(0.6),
                    AnimationChannel::constant(0.8),
                ]),
            );
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(ColorConstantProcessor));

        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        let c = out.downcast_ref::<Color>().unwrap();
        assert!((c.r - 0.2).abs() < 1e-6);
        assert!((c.g - 0.4).abs() < 1e-6);
        assert!((c.b - 0.6).abs() < 1e-6);
        assert!((c.a - 0.8).abs() < 1e-6);
    }

    #[test]
    fn color_constant_defaults_to_opaque_white() {
        let node =
            Node::new(NodeId::new(1), "constant.color").with_output("color", DataTypeId::COLOR);
        let mut scope = Evaluator::new();
        let out = ColorConstantProcessor
            .process(&node, &ctx(), &[], &ResolvedParams::default(), &mut scope)
            .unwrap();
        let c = out.downcast_ref::<Color>().unwrap();
        assert!((c.r - 1.0).abs() < 1e-6 && (c.a - 1.0).abs() < 1e-6);
    }
}
