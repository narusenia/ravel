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
/// A non-vector value means the wired node's output type is not one the port
/// declares. The node editor filters connections by `accepted_types`, but
/// `Graph::add_edge` does not, so this stays an error rather than a fallback.
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

/// `vector.length`: magnitude of a vector of any arity. An unconnected input
/// reads as the zero vector, so the length is 0.
pub struct VectorLengthProcessor;

/// Euclidean length of the first `arity` components of `c`.
fn magnitude(arity: usize, c: [f32; 4]) -> f32 {
    c[..arity].iter().map(|v| v * v).sum::<f32>().sqrt()
}

impl NodeProcessor for VectorLengthProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let (arity, c) = vector_at(inputs, 0, "vector")?.unwrap_or((2, [0.0; 4]));
        Ok(Arc::new(Scalar(magnitude(arity, c))))
    }
}

/// `vector.normalize.vec2` / `vec3` / `vec4`: the input scaled to unit
/// length.
///
/// The zero vector normalizes to the zero vector: dividing by its length
/// would be `0/0`, and a NaN propagates silently through every downstream
/// node. A non-finite length (an infinite component) takes the same branch
/// for the same reason.
pub struct VectorNormalizeProcessor {
    arity: VectorArity,
}

impl VectorNormalizeProcessor {
    pub const fn new(arity: VectorArity) -> Self {
        Self { arity }
    }
}

impl NodeProcessor for VectorNormalizeProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let c = vector_of_arity(inputs, 0, "vector", self.arity)?;
        let length = magnitude(self.arity.components(), c);
        let mut out = [0.0f32; 4];
        if length.is_finite() && length > 0.0 {
            for (slot, value) in out.iter_mut().zip(c) {
                *slot = value / length;
            }
        }
        Ok(self.arity.value(out))
    }
}

/// The two operands of a binary vector node, with their shared arity.
/// Unconnected ports read as the zero vector of the other side's arity, so
/// a half-wired node does not report a spurious mismatch.
fn binary_operands(
    inputs: &[Option<Arc<dyn NodeData>>],
    node: &str,
) -> anyhow::Result<(usize, [f32; 4], [f32; 4])> {
    let a = vector_at(inputs, 0, "a")?;
    let b = vector_at(inputs, 1, "b")?;
    match (a, b) {
        (Some((a_arity, _)), Some((b_arity, _))) if a_arity != b_arity => {
            anyhow::bail!(
                "{node} needs two vectors of the same arity, got {a_arity} and {b_arity} components"
            )
        }
        (a, b) => {
            let arity = a.or(b).map(|(n, _)| n).unwrap_or(2);
            Ok((
                arity,
                a.map(|(_, c)| c).unwrap_or([0.0; 4]),
                b.map(|(_, c)| c).unwrap_or([0.0; 4]),
            ))
        }
    }
}

/// `vector.dot`: the dot product of two vectors of the same arity.
///
/// Both input ports accept every arity, so a Vec2 × Vec3 pair is
/// connectable; the mismatch is reported here.
pub struct VectorDotProcessor;

impl NodeProcessor for VectorDotProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let (arity, a, b) = binary_operands(inputs, "`vector.dot`")?;
        let dot: f32 = a[..arity].iter().zip(&b[..arity]).map(|(x, y)| x * y).sum();
        Ok(Arc::new(Scalar(dot)))
    }
}

/// `vector.cross.vec2` / `vec3`: the cross product, which is a Scalar in 2D
/// and a Vec3 in 3D — hence two templates rather than one.
pub struct VectorCrossProcessor {
    arity: VectorArity,
}

impl VectorCrossProcessor {
    pub const fn new(arity: VectorArity) -> Self {
        Self { arity }
    }
}

impl NodeProcessor for VectorCrossProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let a = vector_of_arity(inputs, 0, "a", self.arity)?;
        let b = vector_of_arity(inputs, 1, "b", self.arity)?;
        Ok(match self.arity {
            VectorArity::Vec2 => Arc::new(Scalar(a[0] * b[1] - a[1] * b[0])) as Arc<dyn NodeData>,
            VectorArity::Vec3 => Arc::new(Vec3(
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            )),
            VectorArity::Vec4 => {
                anyhow::bail!("the cross product is undefined for 4-component vectors")
            }
        })
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
        VECTOR_CONSTRUCT_VEC2, VECTOR_CONSTRUCT_VEC3, VECTOR_CONSTRUCT_VEC4, VECTOR_CROSS_VEC2,
        VECTOR_CROSS_VEC3, VECTOR_DOT, VECTOR_LENGTH, VECTOR_NORMALIZE_VEC2, VECTOR_NORMALIZE_VEC3,
        VECTOR_NORMALIZE_VEC4, VECTOR_SPLIT_VEC2, VECTOR_SPLIT_VEC3, VECTOR_SWIZZLE_VEC2,
        VECTOR_SWIZZLE_VEC3, register_builtins,
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

    // -----------------------------------------------------------------
    // vector.length / normalize / dot / cross
    // -----------------------------------------------------------------

    /// Runs `processor` on the given operands with no parameters.
    fn run(
        processor: &dyn NodeProcessor,
        type_key: &str,
        inputs: Vec<Option<Arc<dyn NodeData>>>,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        processor.process(
            &Node::new(NodeId::new(1), type_key),
            &ctx(),
            &inputs,
            &ResolvedParams::default(),
            &mut Evaluator::new(),
        )
    }

    fn scalar_of(value: &Arc<dyn NodeData>) -> f32 {
        value.downcast_ref::<Scalar>().expect("a Scalar").0
    }

    #[test]
    fn length_is_the_magnitude_at_every_arity() {
        for (input, expected) in [
            (Arc::new(Vec2(3.0, 4.0)) as Arc<dyn NodeData>, 5.0),
            (Arc::new(Vec3(2.0, 3.0, 6.0)), 7.0),
            (Arc::new(Vec4(1.0, 1.0, 1.0, 1.0)), 2.0),
        ] {
            let out = run(&VectorLengthProcessor, VECTOR_LENGTH, vec![Some(input)]).unwrap();
            assert_eq!(out.data_type_id(), DataTypeId::SCALAR);
            assert!((scalar_of(&out) - expected).abs() < 1e-6, "{expected}");
        }
    }

    #[test]
    fn an_unconnected_length_is_zero() {
        let out = run(&VectorLengthProcessor, VECTOR_LENGTH, vec![None]).unwrap();
        assert_eq!(scalar_of(&out), 0.0);
    }

    #[test]
    fn normalize_scales_to_unit_length() {
        let out = run(
            &VectorNormalizeProcessor::new(VectorArity::Vec2),
            VECTOR_NORMALIZE_VEC2,
            vec![Some(Arc::new(Vec2(3.0, 4.0)))],
        )
        .unwrap();
        assert_eq!(*out.downcast_ref::<Vec2>().unwrap(), Vec2(0.6, 0.8));

        let out = run(
            &VectorNormalizeProcessor::new(VectorArity::Vec3),
            VECTOR_NORMALIZE_VEC3,
            vec![Some(Arc::new(Vec3(0.0, 0.0, -2.0)))],
        )
        .unwrap();
        assert_eq!(*out.downcast_ref::<Vec3>().unwrap(), Vec3(0.0, 0.0, -1.0));
    }

    /// The zero vector normalizes to zero, not to NaN — dividing by a zero
    /// length would poison every downstream value.
    #[test]
    fn normalize_of_a_zero_vector_is_zero_not_nan() {
        for (arity, type_key, input) in [
            (
                VectorArity::Vec2,
                VECTOR_NORMALIZE_VEC2,
                Arc::new(Vec2(0.0, 0.0)) as Arc<dyn NodeData>,
            ),
            (
                VectorArity::Vec3,
                VECTOR_NORMALIZE_VEC3,
                Arc::new(Vec3(0.0, 0.0, 0.0)),
            ),
            (
                VectorArity::Vec4,
                VECTOR_NORMALIZE_VEC4,
                Arc::new(Vec4(0.0, 0.0, 0.0, 0.0)),
            ),
        ] {
            let out = run(
                &VectorNormalizeProcessor::new(arity),
                type_key,
                vec![Some(input)],
            )
            .unwrap();
            let (_, c) = components_of(&out).expect("a vector");
            assert!(
                c.iter().all(|v| *v == 0.0),
                "{type_key} produced {c:?}, expected zeros"
            );
        }
    }

    /// An unconnected input is the zero vector, which takes the same branch.
    #[test]
    fn an_unconnected_normalize_is_zero() {
        let out = run(
            &VectorNormalizeProcessor::new(VectorArity::Vec2),
            VECTOR_NORMALIZE_VEC2,
            vec![None, None],
        )
        .unwrap();
        assert_eq!(*out.downcast_ref::<Vec2>().unwrap(), Vec2(0.0, 0.0));
    }

    #[test]
    fn dot_multiplies_componentwise_and_sums() {
        for (a, b, expected) in [
            (
                Arc::new(Vec2(1.0, 2.0)) as Arc<dyn NodeData>,
                Arc::new(Vec2(3.0, 4.0)) as Arc<dyn NodeData>,
                11.0,
            ),
            (
                Arc::new(Vec3(1.0, 0.0, 0.0)),
                Arc::new(Vec3(0.0, 1.0, 0.0)),
                0.0,
            ),
            (
                Arc::new(Vec4(1.0, 2.0, 3.0, 4.0)),
                Arc::new(Vec4(-1.0, 1.0, -1.0, 1.0)),
                2.0,
            ),
        ] {
            let out = run(&VectorDotProcessor, VECTOR_DOT, vec![Some(a), Some(b)]).unwrap();
            assert_eq!(out.data_type_id(), DataTypeId::SCALAR);
            assert!((scalar_of(&out) - expected).abs() < 1e-6, "{expected}");
        }
    }

    /// `vector.dot` takes any arity on both ports, so a Vec2 × Vec3 pair is
    /// connectable — and has to be reported when it evaluates.
    #[test]
    fn dot_rejects_mismatched_arities() {
        let err = run(
            &VectorDotProcessor,
            VECTOR_DOT,
            vec![
                Some(Arc::new(Vec2(1.0, 2.0))),
                Some(Arc::new(Vec3(1.0, 2.0, 3.0))),
            ],
        )
        .err()
        .expect("Vec2 . Vec3 is not a dot product")
        .to_string();
        assert!(err.contains("same arity"), "{err}");
    }

    /// One connected side is not a mismatch: the other reads as that arity's
    /// zero vector.
    #[test]
    fn a_half_wired_dot_is_zero() {
        let out = run(
            &VectorDotProcessor,
            VECTOR_DOT,
            vec![Some(Arc::new(Vec3(1.0, 2.0, 3.0))), None],
        )
        .unwrap();
        assert_eq!(scalar_of(&out), 0.0);
    }

    #[test]
    fn cross_of_two_vec2_is_the_scalar_wedge() {
        let out = run(
            &VectorCrossProcessor::new(VectorArity::Vec2),
            VECTOR_CROSS_VEC2,
            vec![
                Some(Arc::new(Vec2(1.0, 0.0))),
                Some(Arc::new(Vec2(0.0, 1.0))),
            ],
        )
        .unwrap();
        assert_eq!(out.data_type_id(), DataTypeId::SCALAR);
        assert_eq!(scalar_of(&out), 1.0);

        // Antisymmetric: swapping the operands negates the result.
        let out = run(
            &VectorCrossProcessor::new(VectorArity::Vec2),
            VECTOR_CROSS_VEC2,
            vec![
                Some(Arc::new(Vec2(0.0, 1.0))),
                Some(Arc::new(Vec2(1.0, 0.0))),
            ],
        )
        .unwrap();
        assert_eq!(scalar_of(&out), -1.0);
    }

    #[test]
    fn cross_of_two_vec3_is_perpendicular_to_both() {
        let a = Vec3(1.0, 0.0, 0.0);
        let b = Vec3(0.0, 1.0, 0.0);
        let out = run(
            &VectorCrossProcessor::new(VectorArity::Vec3),
            VECTOR_CROSS_VEC3,
            vec![Some(Arc::new(a)), Some(Arc::new(b))],
        )
        .unwrap();
        assert_eq!(out.data_type_id(), DataTypeId::VEC3);
        assert_eq!(*out.downcast_ref::<Vec3>().unwrap(), Vec3(0.0, 0.0, 1.0));

        let c = Vec3(2.0, -3.0, 1.0);
        let d = Vec3(-1.0, 0.5, 4.0);
        let out = run(
            &VectorCrossProcessor::new(VectorArity::Vec3),
            VECTOR_CROSS_VEC3,
            vec![Some(Arc::new(c)), Some(Arc::new(d))],
        )
        .unwrap();
        let r = *out.downcast_ref::<Vec3>().unwrap();
        assert_eq!(r, Vec3(-12.5, -9.0, -2.0));
        // Orthogonal to both operands.
        assert!((r.0 * c.0 + r.1 * c.1 + r.2 * c.2).abs() < 1e-5);
        assert!((r.0 * d.0 + r.1 * d.1 + r.2 * d.2).abs() < 1e-5);
    }

    /// The `cross` input ports declare only their own arity, so the node
    /// editor will not draw a Vec2 × Vec3 pair — but `Graph::add_edge` does
    /// not type-check (the filter lives in the editor), so the processor has
    /// to report the mismatch rather than read a stale component.
    #[test]
    fn cross_rejects_a_mismatched_arity() {
        let reg = registry();
        let cross = reg.create_node(VECTOR_CROSS_VEC2, NodeId::new(1)).unwrap();
        assert_eq!(cross.inputs[0].accepted_types, [DataTypeId::VEC2]);
        assert_eq!(cross.inputs[1].accepted_types, [DataTypeId::VEC2]);

        let err = run(
            &VectorCrossProcessor::new(VectorArity::Vec2),
            VECTOR_CROSS_VEC2,
            vec![
                Some(Arc::new(Vec3(1.0, 2.0, 3.0))),
                Some(Arc::new(Vec2(1.0, 0.0))),
            ],
        )
        .err()
        .expect("a Vec3 operand is not a 2D cross product")
        .to_string();
        assert!(err.contains("2-component vector"), "{err}");
    }

    /// There is no 4-component cross product, and no template that would
    /// build this processor — the guard keeps the arity enum total.
    #[test]
    fn cross_has_no_four_component_form() {
        let err = run(
            &VectorCrossProcessor::new(VectorArity::Vec4),
            VECTOR_CROSS_VEC3,
            vec![None, None],
        )
        .err()
        .expect("4-component cross is undefined")
        .to_string();
        assert!(err.contains("undefined"), "{err}");
    }

    /// `length` and `normalize` agree: normalizing scales the input by the
    /// reciprocal of what `length` reports.
    #[test]
    fn normalize_divides_by_the_length_node_s_answer() {
        let input = Vec3(2.0, -3.0, 6.0);
        let length = scalar_of(
            &run(
                &VectorLengthProcessor,
                VECTOR_LENGTH,
                vec![Some(Arc::new(input))],
            )
            .unwrap(),
        );
        let out = run(
            &VectorNormalizeProcessor::new(VectorArity::Vec3),
            VECTOR_NORMALIZE_VEC3,
            vec![Some(Arc::new(input))],
        )
        .unwrap();
        let unit = *out.downcast_ref::<Vec3>().unwrap();
        assert!((length - 7.0).abs() < 1e-6, "{length}");
        assert!((unit.0 - input.0 / length).abs() < 1e-6);
        assert!((unit.1 - input.1 / length).abs() < 1e-6);
        assert!((unit.2 - input.2 / length).abs() < 1e-6);
    }

    /// `dot` and `length` agree: `a . a` is the squared magnitude.
    #[test]
    fn dot_with_itself_is_the_squared_length() {
        let a: Arc<dyn NodeData> = Arc::new(Vec4(1.0, -2.0, 3.0, -4.0));
        let dot = scalar_of(
            &run(
                &VectorDotProcessor,
                VECTOR_DOT,
                vec![Some(a.clone()), Some(a.clone())],
            )
            .unwrap(),
        );
        let length = scalar_of(&run(&VectorLengthProcessor, VECTOR_LENGTH, vec![Some(a)]).unwrap());
        assert!((dot - length * length).abs() < 1e-4, "{dot} vs {length}");
    }

    #[test]
    fn the_arithmetic_nodes_are_not_time_dependent() {
        assert!(!VectorLengthProcessor.is_time_dependent());
        assert!(!VectorNormalizeProcessor::new(VectorArity::Vec3).is_time_dependent());
        assert!(!VectorDotProcessor.is_time_dependent());
        assert!(!VectorCrossProcessor::new(VectorArity::Vec3).is_time_dependent());
    }
}
