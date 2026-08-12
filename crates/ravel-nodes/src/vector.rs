// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Value-domain vector nodes (CPU-only).
//!
//! These operate on Scalar / Vec *values* on wires, not on fields. The field
//! counterparts (`field.compose` and friends) transform `Field -> Field` and
//! share no implementation with these because the output types differ.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::registry::builtin::{VECTOR_COMPONENT_KEYS, VECTOR_SWIZZLE_PATTERN};
use ravel_core::types::{NodeData, PortRecord, Scalar, Vec2, Vec3, Vec4};
use std::sync::Arc;

/// Component count of a value-domain vector node, one per `type_key`.
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

    /// Builds the vector value of this arity from `c`, ignoring the
    /// components beyond it.
    fn value(self, c: [f32; 4]) -> Arc<dyn NodeData> {
        match self {
            VectorArity::Vec2 => Arc::new(Vec2(c[0], c[1])) as Arc<dyn NodeData>,
            VectorArity::Vec3 => Arc::new(Vec3(c[0], c[1], c[2])),
            VectorArity::Vec4 => Arc::new(Vec4(c[0], c[1], c[2], c[3])),
        }
    }
}

/// Arity and components of a value-domain vector value, zero-padded to four.
fn components_of(value: &Arc<dyn NodeData>) -> Option<(usize, [f32; 4])> {
    if let Some(v) = value.downcast_ref::<Vec2>() {
        return Some((2, [v.0, v.1, 0.0, 0.0]));
    }
    if let Some(v) = value.downcast_ref::<Vec3>() {
        return Some((3, [v.0, v.1, v.2, 0.0]));
    }
    if let Some(v) = value.downcast_ref::<Vec4>() {
        return Some((4, [v.0, v.1, v.2, v.3]));
    }
    None
}

/// Reads vector input `slot`, or `None` when the port is unconnected.
///
/// Edge creation is type-filtered, so a non-vector value here means the port
/// declaration and the wired node disagree — an error, not a fallback.
fn vector_at(
    inputs: &[Option<Arc<dyn NodeData>>],
    slot: usize,
    port: &str,
) -> anyhow::Result<Option<(usize, [f32; 4])>> {
    match inputs.get(slot).and_then(Option::as_ref) {
        None => Ok(None),
        Some(value) => components_of(value).map(Some).ok_or_else(|| {
            anyhow::anyhow!(
                "`{port}` expects a vector value, got {:?}",
                value.data_type_id()
            )
        }),
    }
}

/// Reads vector input `slot` and requires it to be `arity` components wide,
/// treating an unconnected port as the zero vector of that arity.
fn vector_of_arity(
    inputs: &[Option<Arc<dyn NodeData>>],
    slot: usize,
    port: &str,
    arity: VectorArity,
) -> anyhow::Result<[f32; 4]> {
    let want = arity.components();
    match vector_at(inputs, slot, port)? {
        None => Ok([0.0; 4]),
        Some((got, c)) if got == want => Ok(c),
        Some((got, _)) => {
            anyhow::bail!("`{port}` expects a {want}-component vector, got {got} components")
        }
    }
}

/// `vector.split.vec2` / `vec3` / `vec4`: one vector in, one Scalar per
/// component out.
///
/// Multi-output, so the value is a [`PortRecord`] in output-port order — the
/// same convention `net.in` and `subnet` follow. An unconnected input splits
/// into zeros rather than failing.
pub struct VectorSplitProcessor {
    arity: VectorArity,
}

impl VectorSplitProcessor {
    pub const fn new(arity: VectorArity) -> Self {
        Self { arity }
    }
}

impl NodeProcessor for VectorSplitProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let c = vector_of_arity(inputs, 0, "vector", self.arity)?;
        let record: Vec<Arc<dyn NodeData>> = c[..self.arity.components()]
            .iter()
            .map(|v| Arc::new(Scalar(*v)) as Arc<dyn NodeData>)
            .collect();
        Ok(Arc::new(PortRecord(record)))
    }
}

/// `vector.swizzle.vec2` / `vec3` / `vec4`: reorders (and repeats) the
/// components named by the `pattern` parameter.
///
/// The output arity is the node's, so the pattern has to name exactly that
/// many components; naming one the wired vector does not have is an error.
/// An unconnected input reads as a zero Vec4, which keeps every pattern
/// evaluable until something is wired in.
pub struct VectorSwizzleProcessor {
    arity: VectorArity,
}

impl VectorSwizzleProcessor {
    pub const fn new(arity: VectorArity) -> Self {
        Self { arity }
    }
}

impl NodeProcessor for VectorSwizzleProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let (arity, c) = vector_at(inputs, 0, "vector")?.unwrap_or((4, [0.0; 4]));
        let pattern = params.str_or(VECTOR_SWIZZLE_PATTERN, "");
        let want = self.arity.components();
        if pattern.chars().count() != want {
            anyhow::bail!("`pattern` {pattern:?} must name exactly {want} components");
        }
        let mut out = [0.0f32; 4];
        for (slot, name) in out.iter_mut().zip(pattern.chars()) {
            let index = VECTOR_COMPONENT_KEYS
                .iter()
                .position(|key| key.starts_with(name))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "`pattern` {pattern:?} names {name:?}, which is not one of x, y, z, w"
                    )
                })?;
            if index >= arity {
                anyhow::bail!(
                    "`pattern` {pattern:?} names {name:?}, which a {arity}-component vector does not have"
                );
            }
            *slot = c[index];
        }
        Ok(self.arity.value(out))
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
        Ok(self.arity.value(c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::InputPortIndex;
    use ravel_core::id::{DataTypeId, EdgeId, NodeId, OutputPortIndex};
    use ravel_core::registry::NodeRegistry;
    use ravel_core::registry::builtin::{
        VECTOR_CONSTRUCT_VEC2, VECTOR_CONSTRUCT_VEC3, VECTOR_CONSTRUCT_VEC4, VECTOR_SPLIT_VEC2,
        VECTOR_SPLIT_VEC3, VECTOR_SWIZZLE_VEC2, VECTOR_SWIZZLE_VEC3, register_builtins,
    };
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (64, 64))
    }

    fn registry() -> NodeRegistry {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        reg
    }

    /// A template-created node with `params` overwritten in place (the
    /// builder appends, which would leave the template's default behind).
    fn node_of(
        reg: &NodeRegistry,
        type_key: &str,
        id: u64,
        params: &[(&str, ParameterValue)],
    ) -> Node {
        let mut node = reg
            .create_node(type_key, NodeId::new(id))
            .unwrap_or_else(|| panic!("{type_key} is registered"));
        for (key, value) in params {
            let slot = node
                .parameters
                .iter_mut()
                .find(|p| p.key == *key)
                .unwrap_or_else(|| panic!("{type_key} declares {key}"));
            slot.value = value.clone();
        }
        node
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

    // -----------------------------------------------------------------
    // vector.split
    // -----------------------------------------------------------------

    /// `construct` then `split` returns the components unchanged, and the
    /// split value is a `PortRecord` in output-port order.
    #[test]
    fn split_recovers_the_components_construct_combined() {
        let reg = registry();
        let construct = node_of(
            &reg,
            VECTOR_CONSTRUCT_VEC3,
            1,
            &[
                ("x", ParameterValue::Float(1.5)),
                ("y", ParameterValue::Float(-2.5)),
                ("z", ParameterValue::Float(0.25)),
            ],
        );
        let split = node_of(&reg, VECTOR_SPLIT_VEC3, 2, &[]);
        let graph = Graph::new()
            .add_node(construct)
            .unwrap()
            .add_node(split)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(VectorConstructProcessor::new(VectorArity::Vec3)),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(VectorSplitProcessor::new(VectorArity::Vec3)),
        );

        let out = ev.evaluate(&graph, NodeId::new(2), &ctx()).unwrap();
        let record = out
            .downcast_ref::<PortRecord>()
            .expect("a multi-output node yields a PortRecord");
        let components: Vec<f32> = record
            .0
            .iter()
            .map(|v| v.downcast_ref::<Scalar>().unwrap().0)
            .collect();
        assert_eq!(components, vec![1.5, -2.5, 0.25]);
    }

    /// Each split output drives its own downstream edge: the `y` port alone
    /// reaches a consumer's `x`, so the record is indexed per port rather
    /// than collapsed to output 0.
    #[test]
    fn each_split_output_pulls_on_its_own() {
        let reg = registry();
        let source = node_of(
            &reg,
            VECTOR_CONSTRUCT_VEC3,
            1,
            &[
                ("x", ParameterValue::Float(7.0)),
                ("y", ParameterValue::Float(8.0)),
                ("z", ParameterValue::Float(9.0)),
            ],
        );
        let split = node_of(&reg, VECTOR_SPLIT_VEC3, 2, &[]);
        // Rebuild a Vec2 from split's `z` and `x`, in that order.
        let rebuild = node_of(&reg, VECTOR_CONSTRUCT_VEC2, 3, &[]);
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(split)
            .unwrap()
            .add_node(rebuild)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap()
            .expose_param_port(NodeId::new(3), "x")
            .unwrap()
            .expose_param_port(NodeId::new(3), "y")
            .unwrap();
        let rebuild_node = graph.node(NodeId::new(3)).unwrap();
        let x_port = rebuild_node.param_port_index("x").unwrap();
        let y_port = rebuild_node.param_port_index("y").unwrap();
        let graph = graph
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(2),
                NodeId::new(3),
                x_port,
            )
            .unwrap()
            .add_edge(
                EdgeId::new(3),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                y_port,
            )
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(VectorConstructProcessor::new(VectorArity::Vec3)),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(VectorSplitProcessor::new(VectorArity::Vec3)),
        );
        ev.register(
            NodeId::new(3),
            Arc::new(VectorConstructProcessor::new(VectorArity::Vec2)),
        );

        let out = ev.evaluate(&graph, NodeId::new(3), &ctx()).unwrap();
        assert_eq!(*out.downcast_ref::<Vec2>().unwrap(), Vec2(9.0, 7.0));
    }

    /// An unconnected input splits into zeros instead of failing.
    #[test]
    fn an_unconnected_split_yields_zeros() {
        let out = VectorSplitProcessor::new(VectorArity::Vec2)
            .process(
                &Node::new(NodeId::new(1), VECTOR_SPLIT_VEC2),
                &ctx(),
                &[None],
                &ResolvedParams::default(),
                &mut Evaluator::new(),
            )
            .unwrap();
        let record = out.downcast_ref::<PortRecord>().unwrap();
        assert_eq!(record.0.len(), 2);
        assert!(
            record
                .0
                .iter()
                .all(|v| v.downcast_ref::<Scalar>().unwrap().0 == 0.0)
        );
    }

    /// Narrowing a `vector.split.vec3` to two components is an output-port
    /// removal: the edge out of `z` goes, and the edges out of the ports
    /// before it keep their indices (the unit-1 re-index of
    /// `network-interface-editing-plan.md`). Arity is a `type_key`, so this
    /// is what "changing arity" means for a split — there is no in-place
    /// retype of an output port.
    #[test]
    fn narrowing_a_split_drops_only_the_edge_of_the_removed_component() {
        let reg = registry();
        let split = node_of(&reg, VECTOR_SPLIT_VEC3, 1, &[]);
        let sink = node_of(&reg, VECTOR_CONSTRUCT_VEC3, 2, &[]);
        let graph = Graph::new()
            .add_node(split)
            .unwrap()
            .add_node(sink)
            .unwrap()
            .expose_param_port(NodeId::new(2), "x")
            .unwrap()
            .expose_param_port(NodeId::new(2), "z")
            .unwrap();
        let sink_node = graph.node(NodeId::new(2)).unwrap();
        let x_port = sink_node.param_port_index("x").unwrap();
        let z_port = sink_node.param_port_index("z").unwrap();
        let graph = graph
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                x_port,
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(1),
                OutputPortIndex(2),
                NodeId::new(2),
                z_port,
            )
            .unwrap();

        let graph = graph
            .remove_output_port(NodeId::new(1), OutputPortIndex(2))
            .unwrap();

        assert_eq!(graph.node(NodeId::new(1)).unwrap().outputs.len(), 2);
        let edges: Vec<_> = graph.edges().collect();
        assert_eq!(edges.len(), 1, "only the `z` edge is dropped");
        assert_eq!(edges[0].id, EdgeId::new(1));
        assert_eq!(edges[0].source_port, OutputPortIndex(0));
    }

    // -----------------------------------------------------------------
    // vector.swizzle
    // -----------------------------------------------------------------

    /// Evaluates a swizzle node of `type_key` on `input` with `pattern`.
    fn swizzle(
        type_key: &str,
        arity: VectorArity,
        pattern: &str,
        input: Option<Arc<dyn NodeData>>,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let reg = registry();
        let node = node_of(
            &reg,
            type_key,
            1,
            &[(
                VECTOR_SWIZZLE_PATTERN,
                ParameterValue::String(pattern.into()),
            )],
        );
        let mut params = ResolvedParams::default();
        params.set(
            VECTOR_SWIZZLE_PATTERN,
            ravel_core::eval::ResolvedValue::Str(pattern.into()),
        );
        VectorSwizzleProcessor::new(arity).process(
            &node,
            &ctx(),
            &[input],
            &params,
            &mut Evaluator::new(),
        )
    }

    /// The error message of a swizzle that must not evaluate.
    fn swizzle_err(
        type_key: &str,
        arity: VectorArity,
        pattern: &str,
        input: Option<Arc<dyn NodeData>>,
    ) -> String {
        match swizzle(type_key, arity, pattern, input) {
            Ok(_) => panic!("{pattern:?} evaluated but should not have"),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn swizzle_reorders_and_repeats_components() {
        let input: Arc<dyn NodeData> = Arc::new(Vec3(1.0, 2.0, 3.0));
        let out = swizzle(
            VECTOR_SWIZZLE_VEC3,
            VectorArity::Vec3,
            "zyx",
            Some(input.clone()),
        )
        .unwrap();
        assert_eq!(out.data_type_id(), DataTypeId::VEC3);
        assert_eq!(*out.downcast_ref::<Vec3>().unwrap(), Vec3(3.0, 2.0, 1.0));

        let out = swizzle(VECTOR_SWIZZLE_VEC3, VectorArity::Vec3, "xxx", Some(input)).unwrap();
        assert_eq!(*out.downcast_ref::<Vec3>().unwrap(), Vec3(1.0, 1.0, 1.0));
    }

    /// A swizzle may narrow: a Vec3 in, a Vec2 out.
    #[test]
    fn swizzle_narrows_a_vec3_to_a_vec2() {
        let out = swizzle(
            VECTOR_SWIZZLE_VEC2,
            VectorArity::Vec2,
            "yx",
            Some(Arc::new(Vec3(1.0, 2.0, 3.0))),
        )
        .unwrap();
        assert_eq!(*out.downcast_ref::<Vec2>().unwrap(), Vec2(2.0, 1.0));
    }

    /// Naming a component the wired vector does not have is an error, not a
    /// silent zero.
    #[test]
    fn swizzle_rejects_a_component_the_input_lacks() {
        let err = swizzle_err(
            VECTOR_SWIZZLE_VEC2,
            VectorArity::Vec2,
            "xz",
            Some(Arc::new(Vec2(1.0, 2.0))),
        );
        assert!(err.contains("2-component"), "{err}");
    }

    /// The pattern has to name exactly as many components as the node's
    /// declared output arity — the output port type is fixed by the
    /// `type_key`, so a shorter or longer pattern has no valid answer.
    #[test]
    fn swizzle_rejects_a_pattern_of_the_wrong_length() {
        for pattern in ["x", "xyz"] {
            let err = swizzle_err(
                VECTOR_SWIZZLE_VEC2,
                VectorArity::Vec2,
                pattern,
                Some(Arc::new(Vec3(1.0, 2.0, 3.0))),
            );
            assert!(err.contains("exactly 2 components"), "{pattern}: {err}");
        }
    }

    #[test]
    fn swizzle_rejects_a_name_that_is_not_a_component() {
        let err = swizzle_err(
            VECTOR_SWIZZLE_VEC2,
            VectorArity::Vec2,
            "xq",
            Some(Arc::new(Vec2(1.0, 2.0))),
        );
        assert!(err.contains("not one of x, y, z, w"), "{err}");
    }

    /// An unconnected swizzle stays evaluable: the input reads as a zero
    /// Vec4, so any pattern resolves to zeros.
    #[test]
    fn an_unconnected_swizzle_yields_zeros() {
        let out = swizzle(VECTOR_SWIZZLE_VEC2, VectorArity::Vec2, "zw", None).unwrap();
        assert_eq!(*out.downcast_ref::<Vec2>().unwrap(), Vec2(0.0, 0.0));
    }

    #[test]
    fn split_and_swizzle_are_not_time_dependent() {
        assert!(!VectorSplitProcessor::new(VectorArity::Vec3).is_time_dependent());
        assert!(!VectorSwizzleProcessor::new(VectorArity::Vec3).is_time_dependent());
    }
}
