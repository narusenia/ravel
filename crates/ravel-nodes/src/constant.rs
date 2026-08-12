// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Constant value generators (CPU-only).

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::id::DataTypeId;
use ravel_core::types::{Color, NodeData, Scalar, Vec2, Vec3, Vec4};
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

/// Vector constants (`constant.vec2` / `constant.vec3` / `constant.vec4`):
/// emit one animatable vector parameter as a value-domain vector.
pub struct VectorConstantProcessor {
    data_type: DataTypeId,
}

impl VectorConstantProcessor {
    pub const fn new(data_type: DataTypeId) -> Self {
        Self { data_type }
    }
}

impl NodeProcessor for VectorConstantProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(match self.data_type {
            DataTypeId::VEC2 => {
                let [x, y] = params.vec2_or("value", [0.0, 0.0]);
                Arc::new(Vec2(x, y)) as Arc<dyn NodeData>
            }
            DataTypeId::VEC3 => {
                let [x, y, z] = params.vec3_or("value", [0.0, 0.0, 0.0]);
                Arc::new(Vec3(x, y, z))
            }
            DataTypeId::VEC4 => {
                let [x, y, z, w] = params.vec4_or("value", [0.0, 0.0, 0.0, 0.0]);
                Arc::new(Vec4(x, y, z, w))
            }
            other => anyhow::bail!("vector constant has unsupported output type {other:?}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::animation::channel::AnimationChannel;
    use ravel_core::animation::curve::KeyframeCurve;
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, NodeId, OutputPortIndex};
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

    fn vector_constant_node(
        id: u64,
        type_key: &str,
        data_type: DataTypeId,
        value: ParameterValue,
    ) -> Node {
        Node::new(NodeId::new(id), type_key)
            .with_output("value", data_type)
            .with_param("value", value)
    }

    fn evaluate_vector(node: Node, data_type: DataTypeId, ctx: &EvalContext) -> Arc<dyn NodeData> {
        let id = node.id;
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(id, Arc::new(VectorConstantProcessor::new(data_type)));
        ev.evaluate(&graph, id, ctx).unwrap()
    }

    #[test]
    fn vector_constants_output_their_declared_types() {
        let ctx = ctx();
        let cases = [
            (
                vector_constant_node(
                    1,
                    "constant.vec2",
                    DataTypeId::VEC2,
                    ParameterValue::vec2(1.0, 2.0),
                ),
                DataTypeId::VEC2,
            ),
            (
                vector_constant_node(
                    2,
                    "constant.vec3",
                    DataTypeId::VEC3,
                    ParameterValue::vec3(1.0, 2.0, 3.0),
                ),
                DataTypeId::VEC3,
            ),
            (
                vector_constant_node(
                    3,
                    "constant.vec4",
                    DataTypeId::VEC4,
                    ParameterValue::Channel4([
                        AnimationChannel::constant(1.0),
                        AnimationChannel::constant(2.0),
                        AnimationChannel::constant(3.0),
                        AnimationChannel::constant(4.0),
                    ]),
                ),
                DataTypeId::VEC4,
            ),
        ];

        for (node, data_type) in cases {
            let out = evaluate_vector(node, data_type, &ctx);
            assert_eq!(out.data_type_id(), data_type);
        }
    }

    fn keyframed_channel(from: f32, to: f32) -> AnimationChannel {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, from, Interpolation::Linear);
        curve.insert(10, to, Interpolation::Linear);
        AnimationChannel::keyframes(curve)
    }

    #[test]
    fn every_vector_constant_component_can_be_keyframed() {
        let ctx = EvalContext::new(5, FrameRate::new(30, 1), (1920, 1080));
        let cases = [
            (
                vector_constant_node(
                    1,
                    "constant.vec2",
                    DataTypeId::VEC2,
                    ParameterValue::Channel2([
                        keyframed_channel(0.0, 10.0),
                        keyframed_channel(10.0, 30.0),
                    ]),
                ),
                DataTypeId::VEC2,
            ),
            (
                vector_constant_node(
                    2,
                    "constant.vec3",
                    DataTypeId::VEC3,
                    ParameterValue::Channel3([
                        keyframed_channel(0.0, 10.0),
                        keyframed_channel(10.0, 30.0),
                        keyframed_channel(20.0, 50.0),
                    ]),
                ),
                DataTypeId::VEC3,
            ),
            (
                vector_constant_node(
                    3,
                    "constant.vec4",
                    DataTypeId::VEC4,
                    ParameterValue::Channel4([
                        keyframed_channel(0.0, 10.0),
                        keyframed_channel(10.0, 30.0),
                        keyframed_channel(20.0, 50.0),
                        keyframed_channel(30.0, 70.0),
                    ]),
                ),
                DataTypeId::VEC4,
            ),
        ];

        for (node, data_type) in cases {
            let out = evaluate_vector(node, data_type, &ctx);
            match data_type {
                DataTypeId::VEC2 => {
                    assert_eq!(*out.downcast_ref::<Vec2>().unwrap(), Vec2(5.0, 20.0))
                }
                DataTypeId::VEC3 => {
                    assert_eq!(*out.downcast_ref::<Vec3>().unwrap(), Vec3(5.0, 20.0, 35.0))
                }
                DataTypeId::VEC4 => assert_eq!(
                    *out.downcast_ref::<Vec4>().unwrap(),
                    Vec4(5.0, 20.0, 35.0, 50.0)
                ),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn vec2_constant_drives_shape_rect_center_port() {
        let source = vector_constant_node(
            1,
            "constant.vec2",
            DataTypeId::VEC2,
            ParameterValue::vec2(40.0, 30.0),
        );
        let rect = Node::new(NodeId::new(2), "shape.rect")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("center", ParameterValue::vec2(0.0, 0.0))
            .with_param("width", ParameterValue::Float(20.0))
            .with_param("height", ParameterValue::Float(10.0));
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(rect)
            .unwrap()
            .expose_param_port(NodeId::new(2), "center")
            .unwrap();
        let center_port = graph
            .node(NodeId::new(2))
            .unwrap()
            .param_port_index("center")
            .unwrap();
        let graph = graph
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                center_port,
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(VectorConstantProcessor::new(DataTypeId::VEC2)),
        );
        ev.register(NodeId::new(2), Arc::new(crate::shape::RectProcessor));

        let out = ev.evaluate(&graph, NodeId::new(2), &ctx()).unwrap();
        let geometry = out
            .downcast_ref::<ravel_core::geometry::Geometry>()
            .unwrap();
        let anchor = geometry
            .detail()
            .get(ravel_core::geometry::names::ANCHOR)
            .unwrap()
            .as_vec2("anchor")
            .unwrap();
        assert_eq!(anchor, &[ravel_core::types::Vec2(40.0, 30.0)]);
    }
}
