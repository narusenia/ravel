// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Value-domain vector nodes (CPU-only).
//!
//! These operate on Scalar / Vec *values* on wires, not on fields. The field
//! counterparts (`field.compose` and friends) transform `Field -> Field` and
//! share no implementation with these because the output types differ.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::registry::builtin::VECTOR_COMPONENT_KEYS;
use ravel_core::types::{NodeData, Vec2, Vec3, Vec4};
use std::sync::Arc;

/// Component count of a `vector.construct` node, one per `type_key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorArity {
    Vec2,
    Vec3,
    Vec4,
}

impl VectorArity {
    /// Number of component parameters this arity reads.
    pub const fn components(self) -> usize {
        match self {
            VectorArity::Vec2 => 2,
            VectorArity::Vec3 => 3,
            VectorArity::Vec4 => 4,
        }
    }
}

/// `vector.construct.vec2` / `vec3` / `vec4`: combines the `x`, `y`, `z` and
/// `w` Float parameters into one vector value.
///
/// Unset components are zero, which keeps an unconnected node evaluable
/// instead of failing (`Vec2(0, 0)` matches the typed zero an unconnected
/// vector port produces).
pub struct VectorConstructProcessor {
    arity: VectorArity,
}

impl VectorConstructProcessor {
    pub const fn new(arity: VectorArity) -> Self {
        Self { arity }
    }
}

impl NodeProcessor for VectorConstructProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let mut c = [0.0f32; 4];
        for (slot, key) in c
            .iter_mut()
            .zip(VECTOR_COMPONENT_KEYS)
            .take(self.arity.components())
        {
            *slot = params.f32_or(key, 0.0);
        }
        Ok(match self.arity {
            VectorArity::Vec2 => Arc::new(Vec2(c[0], c[1])) as Arc<dyn NodeData>,
            VectorArity::Vec3 => Arc::new(Vec3(c[0], c[1], c[2])),
            VectorArity::Vec4 => Arc::new(Vec4(c[0], c[1], c[2], c[3])),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, NodeId, OutputPortIndex};
    use ravel_core::registry::builtin::{
        VECTOR_CONSTRUCT_VEC2, VECTOR_CONSTRUCT_VEC3, VECTOR_CONSTRUCT_VEC4,
    };
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (64, 64))
    }

    /// A node of the given arity with its component parameters set.
    fn construct_node(
        id: u64,
        type_key: &str,
        data_type: DataTypeId,
        values: &[(&str, f32)],
    ) -> Node {
        let mut node = Node::new(NodeId::new(id), type_key).with_output("vector", data_type);
        for (key, value) in values {
            node = node.with_param(*key, ParameterValue::Float(*value));
        }
        node
    }

    fn eval(node: Node, arity: VectorArity) -> Arc<dyn NodeData> {
        let id = node.id;
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(id, Arc::new(VectorConstructProcessor::new(arity)));
        ev.evaluate(&graph, id, &ctx()).unwrap()
    }

    #[test]
    fn vec2_combines_its_components() {
        let node = construct_node(
            1,
            VECTOR_CONSTRUCT_VEC2,
            DataTypeId::VEC2,
            &[("x", 1.5), ("y", -2.5)],
        );
        let out = eval(node, VectorArity::Vec2);
        assert_eq!(out.data_type_id(), DataTypeId::VEC2);
        assert_eq!(*out.downcast_ref::<Vec2>().unwrap(), Vec2(1.5, -2.5));
    }

    #[test]
    fn vec3_combines_its_components() {
        let node = construct_node(
            1,
            VECTOR_CONSTRUCT_VEC3,
            DataTypeId::VEC3,
            &[("x", 1.0), ("y", 2.0), ("z", 3.0)],
        );
        let out = eval(node, VectorArity::Vec3);
        assert_eq!(out.data_type_id(), DataTypeId::VEC3);
        assert_eq!(*out.downcast_ref::<Vec3>().unwrap(), Vec3(1.0, 2.0, 3.0));
    }

    #[test]
    fn vec4_combines_its_components() {
        let node = construct_node(
            1,
            VECTOR_CONSTRUCT_VEC4,
            DataTypeId::VEC4,
            &[("x", 1.0), ("y", 2.0), ("z", 3.0), ("w", 4.0)],
        );
        let out = eval(node, VectorArity::Vec4);
        assert_eq!(out.data_type_id(), DataTypeId::VEC4);
        assert_eq!(
            *out.downcast_ref::<Vec4>().unwrap(),
            Vec4(1.0, 2.0, 3.0, 4.0)
        );
    }

    /// Missing component parameters read as zero rather than failing.
    #[test]
    fn unset_components_are_zero() {
        let node = construct_node(1, VECTOR_CONSTRUCT_VEC3, DataTypeId::VEC3, &[("y", 7.0)]);
        let out = eval(node, VectorArity::Vec3);
        assert_eq!(*out.downcast_ref::<Vec3>().unwrap(), Vec3(0.0, 7.0, 0.0));
    }

    /// A lower arity ignores the components it does not declare, so a stale
    /// `z` parameter cannot leak into a Vec2.
    #[test]
    fn vec2_ignores_higher_components() {
        let node = construct_node(
            1,
            VECTOR_CONSTRUCT_VEC2,
            DataTypeId::VEC2,
            &[("x", 1.0), ("y", 2.0), ("z", 9.0), ("w", 9.0)],
        );
        let out = eval(node, VectorArity::Vec2);
        assert_eq!(*out.downcast_ref::<Vec2>().unwrap(), Vec2(1.0, 2.0));
    }

    #[test]
    fn is_not_time_dependent() {
        assert!(!VectorConstructProcessor::new(VectorArity::Vec2).is_time_dependent());
    }

    /// The component parameters are exposable as Scalar ports, which is what
    /// the `_x` / `_y` parameter migration relies on: two separately driven
    /// scalar edges keep driving one vector parameter through this node.
    #[test]
    fn components_are_drivable_through_exposed_param_ports() {
        let a = Node::new(NodeId::new(1), "constant")
            .with_output("value", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(4.0));
        let b = Node::new(NodeId::new(2), "constant")
            .with_output("value", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(-6.0));
        let construct = construct_node(
            3,
            VECTOR_CONSTRUCT_VEC2,
            DataTypeId::VEC2,
            &[("x", 0.0), ("y", 0.0)],
        );

        let graph = Graph::new()
            .add_node(a)
            .unwrap()
            .add_node(b)
            .unwrap()
            .add_node(construct)
            .unwrap()
            .expose_param_port(NodeId::new(3), "x")
            .unwrap()
            .expose_param_port(NodeId::new(3), "y")
            .unwrap();
        let construct = graph.node(NodeId::new(3)).unwrap();
        let x_port = construct.param_port_index("x").unwrap();
        let y_port = construct.param_port_index("y").unwrap();

        let graph = graph
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(3),
                x_port,
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                y_port,
            )
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(crate::constant::ConstantProcessor));
        ev.register(NodeId::new(2), Arc::new(crate::constant::ConstantProcessor));
        ev.register(
            NodeId::new(3),
            Arc::new(VectorConstructProcessor::new(VectorArity::Vec2)),
        );

        let out = ev.evaluate(&graph, NodeId::new(3), &ctx()).unwrap();
        assert_eq!(*out.downcast_ref::<Vec2>().unwrap(), Vec2(4.0, -6.0));
    }
}
