// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Network interface conventions (REQ-LAYER-002).
//!
//! Every layer network (and, by the same mechanism, every subnet) contains
//! exactly one **In** node and one **Out** node, identified by type key:
//!
//! * `net.in` — the shell→network injection point. Fixed outputs
//!   `base_geometry` (GEOMETRY), `t` (SCALAR, layer-local seconds), and `f`
//!   (SCALAR, layer-local frame index), plus user-defined custom parameter
//!   ports, plus `source` (FRAME_BUFFER) on adjustment layers. A
//!   multi-output node: its value is a [`crate::types::PortRecord`] in
//!   output-port order.
//! * `net.out` — the network→shell result. Input port `frame`
//!   (FRAME_BUFFER) is the only port the shell consumes; additional custom
//!   input ports are exposed to Layer Ref (REQ-LAYER-005). Its value is a
//!   `PortRecord` in input-port order.
//!
//! Custom ports are edited through [`add_custom_port`], [`remove_custom_port`]
//! and [`rename_custom_port`], which wrap the generic `Graph` port operations
//! with the two rules only this module knows: which types a
//! [`NetworkContext`] admits, and which ports the shell owns and therefore
//! nobody may remove or rename ([`is_fixed_port`]).

use crate::animation::channel::AnimationChannel;
use crate::graph::{
    Graph, GraphError, InputPort, Node, OutputPort, Parameter, ParameterValue, PortSide,
};
use crate::id::{DataTypeId, InputPortIndex, NodeId, OutputPortIndex};
use std::sync::Arc;
use thiserror::Error;

/// Type key of the network interface input node.
pub const NET_IN_TYPE_KEY: &str = "net.in";
/// Type key of the network interface output node.
pub const NET_OUT_TYPE_KEY: &str = "net.out";

/// In-node output port: the layer's base quad geometry.
pub const PORT_BASE_GEOMETRY: &str = "base_geometry";
/// In-node output port: layer-local time in seconds.
pub const PORT_TIME: &str = "t";
/// In-node output port: layer-local frame index.
pub const PORT_FRAME_INDEX: &str = "f";
/// In-node output port: composited lower stack (adjustment layers only).
pub const PORT_SOURCE: &str = "source";
/// Out-node input port consumed by the shell's compositing chain.
pub const PORT_FRAME: &str = "frame";

/// Whether `node` is the network interface input node.
pub fn is_in_node(node: &Node) -> bool {
    node.type_key == NET_IN_TYPE_KEY
}

/// Whether `node` is the network interface output node.
pub fn is_out_node(node: &Node) -> bool {
    node.type_key == NET_OUT_TYPE_KEY
}

/// Find the In node of a network, if present.
pub fn find_in_node(graph: &Graph) -> Option<&Arc<Node>> {
    graph.nodes().find(|n| is_in_node(n))
}

/// Find the Out node of a network, if present.
pub fn find_out_node(graph: &Graph) -> Option<&Arc<Node>> {
    graph.nodes().find(|n| is_out_node(n))
}

/// Index of the Out node's `frame` input port, if the node declares one.
pub fn frame_port_index(out_node: &Node) -> Option<usize> {
    out_node.inputs.iter().position(|p| p.name == PORT_FRAME)
}

/// Index of the output port named `name` on `node`.
pub fn output_port_index(node: &Node, name: &str) -> Option<OutputPortIndex> {
    node.outputs
        .iter()
        .position(|p| p.name == name)
        .map(|i| OutputPortIndex(i as u32))
}

// ===========================================================================
// Custom port editing (REQ-LAYER-002, REQ-LAYER-003)
// ===========================================================================

/// Where a network sits in the ownership hierarchy. It decides which custom
/// port types an In node may declare, and nothing else.
///
/// The UI addresses a network by `NetworkPath { comp, layer, subnets }`, but
/// that type lives in `ravel-ui` and this crate must work without it (and
/// without a UI at all). Callers therefore collapse the path to this two-value
/// answer — `subnets.is_empty()` — before crossing into the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkContext {
    /// The network a `Layer` owns directly. Its In node is fed by the layer
    /// shell, which supplies values only (REQ-LAYER-002).
    LayerRoot,
    /// The inner graph of a `subnet` node, at any nesting depth. Its In node
    /// is the subnet's input-pin boundary, so anything that flows on a wire
    /// can arrive there (REQ-LAYER-003).
    Subnet,
}

/// A type a user-declared custom port can have.
///
/// This is deliberately *not* [`DataTypeId`]: `Float`, `Int` and `Bool` all
/// travel the wire as `SCALAR`, yet they are three different parameter kinds
/// with three different Properties widgets. The choice a user makes is the
/// pair "wire type + parameter kind", and that pair is what this enum names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustomPortType {
    Float,
    Int,
    Bool,
    Vec2,
    Vec3,
    Color,
    Geometry,
    Field,
    FrameBuffer,
    Text,
}

/// The types the layer shell can supply: values, and nothing else
/// (REQ-LAYER-002).
const VALUE_PORT_TYPES: [CustomPortType; 6] = [
    CustomPortType::Float,
    CustomPortType::Int,
    CustomPortType::Bool,
    CustomPortType::Vec2,
    CustomPortType::Vec3,
    CustomPortType::Color,
];

/// Every custom port type: the value types plus the types that only ever
/// arrive over a wire.
const ALL_PORT_TYPES: [CustomPortType; 10] = [
    CustomPortType::Float,
    CustomPortType::Int,
    CustomPortType::Bool,
    CustomPortType::Vec2,
    CustomPortType::Vec3,
    CustomPortType::Color,
    CustomPortType::Geometry,
    CustomPortType::Field,
    CustomPortType::FrameBuffer,
    CustomPortType::Text,
];

impl CustomPortType {
    /// The types an In node may declare in `context`, in menu order.
    ///
    /// A layer-root In is fed by the shell, which has values and nothing else
    /// to give (REQ-LAYER-002). A subnet's inner In is the node's input-pin
    /// boundary, so it takes whatever a wire can carry (REQ-LAYER-003).
    pub fn allowed_for_in(context: NetworkContext) -> &'static [CustomPortType] {
        match context {
            NetworkContext::LayerRoot => &VALUE_PORT_TYPES,
            NetworkContext::Subnet => &ALL_PORT_TYPES,
        }
    }

    /// The types an Out node may declare — all of them, in every context.
    ///
    /// REQ-LAYER-002 states the Out node's custom ports as "GEOMETRY / FIELD /
    /// SCALAR / COLOR etc., any type": an Out port is an exit toward the shell
    /// and toward Layer Ref (REQ-LAYER-005), and as a subnet's inner Out it is
    /// the output-pin boundary. Nothing on either side restricts it the way the
    /// shell restricts a layer-root In, so the set does not depend on
    /// [`NetworkContext`].
    pub fn allowed_for_out() -> &'static [CustomPortType] {
        &ALL_PORT_TYPES
    }

    /// Whether an In node in `context` may declare a port of this type.
    pub fn is_allowed_for_in(self, context: NetworkContext) -> bool {
        Self::allowed_for_in(context).contains(&self)
    }

    /// The wire type of a port of this type.
    pub fn data_type(self) -> DataTypeId {
        match self {
            Self::Float | Self::Int | Self::Bool => DataTypeId::SCALAR,
            Self::Vec2 => DataTypeId::VEC2,
            Self::Vec3 => DataTypeId::VEC3,
            Self::Color => DataTypeId::COLOR,
            Self::Geometry => DataTypeId::GEOMETRY,
            Self::Field => DataTypeId::FIELD,
            Self::FrameBuffer => DataTypeId::FRAME_BUFFER,
            Self::Text => DataTypeId::PLAIN_TEXT,
        }
    }

    /// Every wire type an *input* port of this type accepts, principal first.
    ///
    /// `Color` also takes `VEC4` for the reason
    /// [`ParameterValue::port_accepted_types`] gives: the two carry the same
    /// four floats, and refusing `VEC4` would leave a colour port undrivable
    /// by `vector.construct.vec4`.
    pub fn accepted_types(self) -> Vec<DataTypeId> {
        match self {
            Self::Color => vec![DataTypeId::COLOR, DataTypeId::VEC4],
            other => vec![other.data_type()],
        }
    }

    /// The parameter an In node gets alongside a custom output port of this
    /// type, or `None` for a type that has no parameter representation.
    ///
    /// The In node's evaluation falls back to the same-named parameter when
    /// nothing is bound to a custom port (REQ-LAYER-002), so a port that can
    /// carry a parameter must be created with one — otherwise the Properties
    /// panel has no row to show and the fallback has no value to read.
    /// `Geometry` / `Field` / `FrameBuffer` / `Text` have no
    /// [`ParameterValue`] with a matching wire type, so their unconnected
    /// fallback is the typed zero instead.
    ///
    /// Scalars and vectors default to *channel-backed* values rather than
    /// `Float`: every custom In parameter is keyframable (REQ-LAYER-004), and
    /// a channel is the representation the keyframe model edits in place.
    /// `Int` and `Bool` stay constant-only in v1, so they have no channel form.
    /// `Color` defaults to opaque black — a transparent default would make a
    /// newly added colour port look like it does nothing.
    pub fn default_parameter(self) -> Option<ParameterValue> {
        let zero = || AnimationChannel::constant(0.0);
        match self {
            Self::Float => Some(ParameterValue::Channel(zero())),
            Self::Int => Some(ParameterValue::Int(0)),
            Self::Bool => Some(ParameterValue::Bool(false)),
            Self::Vec2 => Some(ParameterValue::vec2(0.0, 0.0)),
            Self::Vec3 => Some(ParameterValue::vec3(0.0, 0.0, 0.0)),
            Self::Color => Some(ParameterValue::Channel4([
                zero(),
                zero(),
                zero(),
                AnimationChannel::constant(1.0),
            ])),
            Self::Geometry | Self::Field | Self::FrameBuffer | Self::Text => None,
        }
    }
}

/// Failure of a network-interface port edit.
#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("node {0:?} is not a network interface node")]
    NotInterfaceNode(NodeId),

    #[error("custom port type {port_type:?} is not allowed on an In node in {context:?}")]
    PortTypeNotAllowed {
        context: NetworkContext,
        port_type: CustomPortType,
    },

    #[error("{name:?} is a built-in {side} port name and cannot name a custom port")]
    ReservedPortName { side: PortSide, name: String },

    #[error("the built-in {side} port {name:?} on node {node:?} cannot be removed or renamed")]
    FixedPort {
        node: NodeId,
        side: PortSide,
        name: String,
    },

    #[error(transparent)]
    Graph(#[from] GraphError),
}

/// The port side a network-interface node's custom ports live on: In declares
/// them as outputs, Out as inputs. `None` for any other node.
fn interface_side(node: &Node) -> Option<PortSide> {
    if is_in_node(node) {
        Some(PortSide::Output)
    } else if is_out_node(node) {
        Some(PortSide::Input)
    } else {
        None
    }
}

/// Whether `name` is one of the interface node's built-in port names on
/// `side`, regardless of what the node currently looks like.
///
/// This is the **reserved-name** question — "may a new custom port be called
/// this?" — and it deliberately ignores the legacy `f` exception below: a
/// built-in name is off limits for a port a user is creating today.
pub fn is_builtin_port_name(node: &Node, side: PortSide, name: &str) -> bool {
    match (node.type_key.as_str(), side) {
        (NET_IN_TYPE_KEY, PortSide::Output) => matches!(
            name,
            PORT_BASE_GEOMETRY | PORT_TIME | PORT_FRAME_INDEX | PORT_SOURCE
        ),
        (NET_OUT_TYPE_KEY, PortSide::Input) => name == PORT_FRAME,
        _ => false,
    }
}

/// Whether the `side` port named `name` on `node` is fixed: shell machinery
/// owns it, so it can be neither removed nor renamed.
///
/// This is the **protection** question, and it is not the same as
/// [`is_builtin_port_name`] for exactly one port. A `net.in` output named `f`
/// that carries a same-named *parameter* is a legacy user-defined port that
/// predates the built-in frame index; the evaluator keeps its
/// custom-parameter semantics and load-time normalization leaves it alone
/// (see `NetInProcessor::process` and the node-expansion plan). Reporting it
/// as fixed would leave the user with a port they can neither drive nor
/// delete, so the exception is honoured here too: with the parameter present
/// the port is custom, and editable.
pub fn is_fixed_port(node: &Node, side: PortSide, name: &str) -> bool {
    match (node.type_key.as_str(), side) {
        (NET_IN_TYPE_KEY, PortSide::Output) => match name {
            PORT_BASE_GEOMETRY | PORT_TIME | PORT_SOURCE => true,
            PORT_FRAME_INDEX => !node.parameters.iter().any(|p| p.key == PORT_FRAME_INDEX),
            _ => false,
        },
        (NET_OUT_TYPE_KEY, PortSide::Input) => name == PORT_FRAME,
        _ => false,
    }
}

/// Append a custom port named `name` to the network-interface node `node_id`.
///
/// On an **In** node the port is an output *and* — for the types that have a
/// parameter representation — a same-named parameter, added together so the
/// node is never left with a port whose unconnected fallback has nothing to
/// read (REQ-LAYER-002). On an **Out** node it is an input port only.
///
/// `context` constrains the In node's type choice
/// ([`CustomPortType::allowed_for_in`]); an Out node's set is
/// context-independent, so the argument does not affect it.
///
/// Errors when the node is not an interface node, when the type is not allowed
/// in `context`, when `name` is a built-in port name, or when the node already
/// has a port (or, for a parameter-carrying type, a parameter) under that name.
/// One call = one consistent graph state (the caller's Document commit is the
/// undo unit).
pub fn add_custom_port(
    graph: Graph,
    node_id: NodeId,
    name: &str,
    port_type: CustomPortType,
    context: NetworkContext,
) -> Result<Graph, NetworkError> {
    let node = graph
        .node(node_id)
        .ok_or(GraphError::NodeNotFound(node_id))?
        .clone();
    let side = interface_side(&node).ok_or(NetworkError::NotInterfaceNode(node_id))?;
    if side == PortSide::Output && !port_type.is_allowed_for_in(context) {
        return Err(NetworkError::PortTypeNotAllowed { context, port_type });
    }
    if is_builtin_port_name(&node, side, name) {
        return Err(NetworkError::ReservedPortName {
            side,
            name: name.to_string(),
        });
    }
    let duplicate = match side {
        PortSide::Input => node.inputs.iter().any(|p| p.name == name),
        PortSide::Output => node.outputs.iter().any(|p| p.name == name),
    };
    if duplicate {
        return Err(GraphError::DuplicatePortName {
            node: node_id,
            side,
            name: name.to_string(),
        }
        .into());
    }

    match side {
        PortSide::Input => {
            let port = InputPort {
                name: name.to_string(),
                accepted_types: port_type.accepted_types(),
                is_param: false,
                is_variadic: false,
            };
            Ok(graph.insert_input_port(node_id, node.inputs.len(), port)?)
        }
        PortSide::Output => {
            // Checked for every type, not only the ones that bring a parameter
            // along. An In node's output port falls back to the parameter of
            // its own name, so a wire-only port landing on an existing key
            // would read it and answer with a value of the wrong type — the
            // very fault the typed-zero fallback exists to prevent.
            if node.parameters.iter().any(|p| p.key == name) {
                return Err(GraphError::DuplicateParamKey {
                    node: node_id,
                    key: name.to_string(),
                }
                .into());
            }
            let parameter = port_type.default_parameter();
            let port = OutputPort {
                name: name.to_string(),
                data_type: port_type.data_type(),
            };
            let graph = graph.insert_output_port(node_id, node.outputs.len(), port)?;
            let Some(value) = parameter else {
                return Ok(graph);
            };
            let updated = {
                let node = graph
                    .node(node_id)
                    .expect("the port was inserted on this node a moment ago");
                let mut updated = (**node).clone();
                updated.parameters.push(Parameter {
                    key: name.to_string(),
                    value,
                });
                updated
            };
            Ok(graph.replace_node(Arc::new(updated)))
        }
    }
}

/// Re-add the built-in `f` output if an edit has just taken the name off a
/// **layer-root** In node.
///
/// `f` is the layer-local frame index, a port REQ-LAYER-002 requires a
/// layer-root In to carry. The only thing that supplies a missing one today is
/// `Document::normalize_net_in_ports` → `append_missing_in_ports`, which runs
/// **on load** — so deleting or renaming a legacy custom `f` would leave the
/// layer unable to read the frame number until the project was reopened, at
/// which point it would come back on its own. This is that same append, in the
/// same call as the edit, so no half-applied state exists in between.
///
/// **Not done inside a subnet.** A subnet's inner In defines the enclosing
/// node's pin interface, so a port appearing there changes the shape of a node
/// the user did not touch; the plan's decision is that `f` is auto-added at the
/// layer root only, and `append_missing_in_ports` skips subnets for the same
/// reason.
fn restore_layer_root_frame_index(
    graph: Graph,
    node_id: NodeId,
    context: NetworkContext,
) -> Result<Graph, NetworkError> {
    if context != NetworkContext::LayerRoot {
        return Ok(graph);
    }
    let Some(node) = graph.node(node_id) else {
        return Ok(graph);
    };
    if !is_in_node(node) || node.outputs.iter().any(|p| p.name == PORT_FRAME_INDEX) {
        return Ok(graph);
    }
    // Appending keeps every existing index-addressed edge valid.
    let index = node.outputs.len();
    let port = OutputPort {
        name: PORT_FRAME_INDEX.to_string(),
        data_type: DataTypeId::SCALAR,
    };
    Ok(graph.insert_output_port(node_id, index, port)?)
}

/// Remove the custom port named `name` from the network-interface node
/// `node_id`, together with the same-named parameter an In node's port carries.
///
/// Edges into (or out of) the removed port are deleted and the remaining ports
/// are re-indexed by the underlying graph operation. Removing a legacy custom
/// `f` from a layer-root In puts the **built-in** `f` back in the same call
/// ([`restore_layer_root_frame_index`]).
///
/// Errors when the node is not an interface node, when the port does not
/// exist, or when it is fixed ([`is_fixed_port`]). One call = one consistent
/// graph state.
pub fn remove_custom_port(
    graph: Graph,
    node_id: NodeId,
    name: &str,
    context: NetworkContext,
) -> Result<Graph, NetworkError> {
    let node = graph
        .node(node_id)
        .ok_or(GraphError::NodeNotFound(node_id))?
        .clone();
    let side = interface_side(&node).ok_or(NetworkError::NotInterfaceNode(node_id))?;
    if is_fixed_port(&node, side, name) {
        return Err(NetworkError::FixedPort {
            node: node_id,
            side,
            name: name.to_string(),
        });
    }
    match side {
        PortSide::Input => {
            let index = node.inputs.iter().position(|p| p.name == name).ok_or(
                GraphError::PortNotFound {
                    node: node_id,
                    side,
                    name: name.to_string(),
                },
            )?;
            Ok(graph.remove_input_port(node_id, InputPortIndex(index as u32))?)
        }
        PortSide::Output => {
            let index = node.outputs.iter().position(|p| p.name == name).ok_or(
                GraphError::PortNotFound {
                    node: node_id,
                    side,
                    name: name.to_string(),
                },
            )?;
            let mut graph = graph.remove_output_port(node_id, OutputPortIndex(index as u32))?;
            if node.parameters.iter().any(|p| p.key == name) {
                let updated = {
                    let node = graph
                        .node(node_id)
                        .expect("the port was removed from this node a moment ago");
                    let mut updated = (**node).clone();
                    updated.parameters.retain(|p| p.key != name);
                    updated
                };
                graph = graph.replace_node(Arc::new(updated));
            }
            restore_layer_root_frame_index(graph, node_id, context)
        }
    }
}

/// Rename the custom port `old_name` on the network-interface node `node_id`,
/// carrying an In node's same-named parameter with it
/// ([`Graph::rename_port`]).
///
/// The fixed-port guard lives here rather than inside `Graph::rename_port`:
/// that call is the general name-keyed operation and knows nothing about which
/// ports a shell owns. Renaming a legacy custom `f` away from a layer-root In
/// puts the **built-in** `f` back in the same call
/// ([`restore_layer_root_frame_index`]).
///
/// Errors when the node is not an interface node, when `old_name` is fixed,
/// when `new_name` is a built-in port name, or when `new_name` collides with an
/// existing port or parameter.
pub fn rename_custom_port(
    graph: Graph,
    node_id: NodeId,
    old_name: &str,
    new_name: &str,
    context: NetworkContext,
) -> Result<Graph, NetworkError> {
    let node = graph
        .node(node_id)
        .ok_or(GraphError::NodeNotFound(node_id))?
        .clone();
    let side = interface_side(&node).ok_or(NetworkError::NotInterfaceNode(node_id))?;
    if is_fixed_port(&node, side, old_name) {
        return Err(NetworkError::FixedPort {
            node: node_id,
            side,
            name: old_name.to_string(),
        });
    }
    if is_builtin_port_name(&node, side, new_name) {
        return Err(NetworkError::ReservedPortName {
            side,
            name: new_name.to_string(),
        });
    }
    let graph = graph.rename_port(node_id, side, old_name, new_name)?;
    restore_layer_root_frame_index(graph, node_id, context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::EdgeId;

    fn in_id() -> NodeId {
        NodeId::new(1)
    }

    fn out_id() -> NodeId {
        NodeId::new(2)
    }

    /// A layer-root In node with every fixed port an adjustment layer has.
    fn in_graph() -> Graph {
        let node = Node::new(in_id(), NET_IN_TYPE_KEY)
            .with_output(PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
            .with_output(PORT_TIME, DataTypeId::SCALAR)
            .with_output(PORT_FRAME_INDEX, DataTypeId::SCALAR)
            .with_output(PORT_SOURCE, DataTypeId::FRAME_BUFFER);
        Graph::new().add_node(node).unwrap()
    }

    fn out_graph() -> Graph {
        let node = Node::new(out_id(), NET_OUT_TYPE_KEY)
            .with_input(PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        Graph::new().add_node(node).unwrap()
    }

    fn node_of(graph: &Graph, id: NodeId) -> &Node {
        graph.node(id).expect("node exists")
    }

    #[test]
    fn find_interface_nodes() {
        let in_node = Node::new(NodeId::new(1), NET_IN_TYPE_KEY)
            .with_output(PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
            .with_output(PORT_TIME, DataTypeId::SCALAR);
        let out_node = Node::new(NodeId::new(2), NET_OUT_TYPE_KEY)
            .with_input(PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let other = Node::new(NodeId::new(3), "blur");

        let g = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(out_node)
            .unwrap()
            .add_node(other)
            .unwrap();

        let found_in = find_in_node(&g).unwrap();
        assert_eq!(found_in.id, NodeId::new(1));
        let found_out = find_out_node(&g).unwrap();
        assert_eq!(found_out.id, NodeId::new(2));
        assert_eq!(frame_port_index(found_out), Some(0));
        assert_eq!(
            output_port_index(found_in, PORT_TIME),
            Some(OutputPortIndex(1))
        );
    }

    // ----- allowed types ---------------------------------------------------

    /// The layer shell supplies values, so a layer-root In cannot declare a
    /// wire-only port (REQ-LAYER-002).
    #[test]
    fn layer_root_in_rejects_a_geometry_port() {
        let err = add_custom_port(
            in_graph(),
            in_id(),
            "geo",
            CustomPortType::Geometry,
            NetworkContext::LayerRoot,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                NetworkError::PortTypeNotAllowed {
                    context: NetworkContext::LayerRoot,
                    port_type: CustomPortType::Geometry,
                }
            ),
            "{err}"
        );
        for port_type in [
            CustomPortType::Field,
            CustomPortType::FrameBuffer,
            CustomPortType::Text,
        ] {
            assert!(
                add_custom_port(
                    in_graph(),
                    in_id(),
                    "pin",
                    port_type,
                    NetworkContext::LayerRoot
                )
                .is_err(),
                "{port_type:?} must not be allowed at the layer root"
            );
        }
    }

    /// A subnet's inner In is the node's input-pin boundary, so it takes
    /// anything a wire carries (REQ-LAYER-003).
    #[test]
    fn subnet_in_accepts_a_geometry_port() {
        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "geo",
            CustomPortType::Geometry,
            NetworkContext::Subnet,
        )
        .unwrap();
        let node = node_of(&graph, in_id());
        let port = node.outputs.last().expect("appended port");
        assert_eq!(port.name, "geo");
        assert_eq!(port.data_type, DataTypeId::GEOMETRY);
        // Geometry has no ParameterValue counterpart: the port's unconnected
        // fallback is the typed zero, not a parameter.
        assert!(node.parameters.is_empty());
    }

    /// Every value type is allowed in both contexts, and the parameter each
    /// one creates declares the same wire type as its port.
    #[test]
    fn value_ports_carry_a_parameter_of_the_ports_own_type() {
        for context in [NetworkContext::LayerRoot, NetworkContext::Subnet] {
            for port_type in CustomPortType::allowed_for_in(NetworkContext::LayerRoot) {
                let graph =
                    add_custom_port(in_graph(), in_id(), "amount", *port_type, context).unwrap();
                let node = node_of(&graph, in_id());
                let port = node.outputs.last().expect("appended port");
                let param = node
                    .parameters
                    .iter()
                    .find(|p| p.key == "amount")
                    .unwrap_or_else(|| panic!("{port_type:?} must create a parameter"));
                assert_eq!(
                    param.value.port_data_type(),
                    Some(port.data_type),
                    "{port_type:?} parameter and port disagree on the wire type"
                );
            }
        }
    }

    // ----- fixed ports -----------------------------------------------------

    #[test]
    fn fixed_in_ports_cannot_be_removed_or_renamed() {
        for name in [PORT_BASE_GEOMETRY, PORT_TIME, PORT_FRAME_INDEX, PORT_SOURCE] {
            let removed = remove_custom_port(in_graph(), in_id(), name, NetworkContext::LayerRoot)
                .unwrap_err();
            assert!(
                matches!(removed, NetworkError::FixedPort { .. }),
                "removing {name}: {removed}"
            );
            let renamed = rename_custom_port(
                in_graph(),
                in_id(),
                name,
                "renamed",
                NetworkContext::LayerRoot,
            )
            .unwrap_err();
            assert!(
                matches!(renamed, NetworkError::FixedPort { .. }),
                "renaming {name}: {renamed}"
            );
        }
    }

    #[test]
    fn the_out_frame_port_cannot_be_removed_or_renamed() {
        let removed =
            remove_custom_port(out_graph(), out_id(), PORT_FRAME, NetworkContext::LayerRoot)
                .unwrap_err();
        assert!(
            matches!(removed, NetworkError::FixedPort { .. }),
            "{removed}"
        );
        let renamed = rename_custom_port(
            out_graph(),
            out_id(),
            PORT_FRAME,
            "result",
            NetworkContext::LayerRoot,
        )
        .unwrap_err();
        assert!(
            matches!(renamed, NetworkError::FixedPort { .. }),
            "{renamed}"
        );
    }

    /// A legacy `f` output that carries a same-named parameter is a custom
    /// port, not the builtin frame index, so it stays removable and renamable
    /// (the evaluator honours the same exception).
    #[test]
    fn a_legacy_custom_f_port_stays_editable() {
        let node = Node::new(in_id(), NET_IN_TYPE_KEY)
            .with_output(PORT_TIME, DataTypeId::SCALAR)
            .with_output(PORT_FRAME_INDEX, DataTypeId::SCALAR)
            .with_param(PORT_FRAME_INDEX, ParameterValue::Float(7.5));
        let graph = Graph::new().add_node(node).unwrap();
        assert!(!is_fixed_port(
            node_of(&graph, in_id()),
            PortSide::Output,
            PORT_FRAME_INDEX
        ));

        let renamed = rename_custom_port(
            graph.clone(),
            in_id(),
            PORT_FRAME_INDEX,
            "speed",
            NetworkContext::Subnet,
        )
        .unwrap();
        let node = node_of(&renamed, in_id());
        assert!(node.outputs.iter().any(|p| p.name == "speed"));
        assert!(node.parameters.iter().any(|p| p.key == "speed"));

        let removed =
            remove_custom_port(graph, in_id(), PORT_FRAME_INDEX, NetworkContext::Subnet).unwrap();
        let node = node_of(&removed, in_id());
        assert!(node.outputs.iter().all(|p| p.name != PORT_FRAME_INDEX));
        assert!(node.parameters.is_empty());
    }

    /// An In node whose `f` is a legacy custom port (it has a parameter).
    fn legacy_f_graph() -> Graph {
        let node = Node::new(in_id(), NET_IN_TYPE_KEY)
            .with_output(PORT_TIME, DataTypeId::SCALAR)
            .with_output(PORT_FRAME_INDEX, DataTypeId::SCALAR)
            .with_param(PORT_FRAME_INDEX, ParameterValue::Float(7.5));
        Graph::new().add_node(node).unwrap()
    }

    /// A layer-root In must always be able to report the frame index. Editing
    /// a legacy custom `f` away puts the builtin one back in the same call,
    /// instead of leaving the layer broken until the next load repairs it
    /// (`append_missing_in_ports`).
    #[test]
    fn editing_a_legacy_f_away_restores_the_builtin_at_the_layer_root() {
        let removed = remove_custom_port(
            legacy_f_graph(),
            in_id(),
            PORT_FRAME_INDEX,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let node = node_of(&removed, in_id());
        assert!(
            node.outputs.iter().any(|p| p.name == PORT_FRAME_INDEX),
            "the builtin frame index is back"
        );
        assert!(
            node.parameters.is_empty(),
            "with no parameter it is the builtin, not the custom port again"
        );
        assert!(is_fixed_port(node, PortSide::Output, PORT_FRAME_INDEX));

        let renamed = rename_custom_port(
            legacy_f_graph(),
            in_id(),
            PORT_FRAME_INDEX,
            "speed",
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let node = node_of(&renamed, in_id());
        assert_eq!(
            node.outputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec![PORT_TIME, "speed", PORT_FRAME_INDEX],
            "the renamed port keeps its slot and the builtin is appended"
        );
        assert_eq!(node.parameters.len(), 1);
        assert_eq!(node.parameters[0].key, "speed");
    }

    /// Inside a subnet the In node's ports are the enclosing node's pin
    /// interface, so nothing is auto-added: `f` stays gone (the plan's
    /// decision, and what `append_missing_in_ports` does on load).
    #[test]
    fn editing_a_legacy_f_away_inside_a_subnet_leaves_it_gone() {
        let removed = remove_custom_port(
            legacy_f_graph(),
            in_id(),
            PORT_FRAME_INDEX,
            NetworkContext::Subnet,
        )
        .unwrap();
        let node = node_of(&removed, in_id());
        assert!(node.outputs.iter().all(|p| p.name != PORT_FRAME_INDEX));

        let renamed = rename_custom_port(
            legacy_f_graph(),
            in_id(),
            PORT_FRAME_INDEX,
            "speed",
            NetworkContext::Subnet,
        )
        .unwrap();
        let node = node_of(&renamed, in_id());
        assert_eq!(
            node.outputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec![PORT_TIME, "speed"]
        );
    }

    /// Removing an ordinary custom port from a layer-root In must not grow an
    /// `f` that was already there.
    #[test]
    fn restoring_the_frame_index_does_not_duplicate_an_existing_one() {
        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "amount",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let graph =
            remove_custom_port(graph, in_id(), "amount", NetworkContext::LayerRoot).unwrap();
        let node = node_of(&graph, in_id());
        assert_eq!(
            node.outputs
                .iter()
                .filter(|p| p.name == PORT_FRAME_INDEX)
                .count(),
            1
        );
    }

    #[test]
    fn builtin_names_are_reserved_for_new_and_renamed_ports() {
        let err = add_custom_port(
            in_graph(),
            in_id(),
            PORT_TIME,
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap_err();
        assert!(
            matches!(err, NetworkError::ReservedPortName { .. }),
            "{err}"
        );

        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "amount",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let err = rename_custom_port(
            graph,
            in_id(),
            "amount",
            PORT_FRAME_INDEX,
            NetworkContext::LayerRoot,
        )
        .unwrap_err();
        assert!(
            matches!(err, NetworkError::ReservedPortName { .. }),
            "{err}"
        );
    }

    // ----- add / remove ----------------------------------------------------

    #[test]
    fn removing_an_in_custom_port_drops_its_parameter_and_edges() {
        let sink = Node::new(NodeId::new(9), "blur").with_input("value", &[DataTypeId::SCALAR]);
        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "amount",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap()
        .add_node(sink)
        .unwrap();
        let amount = output_port_index(node_of(&graph, in_id()), "amount").unwrap();
        let graph = graph
            .add_edge(
                EdgeId::new(1),
                in_id(),
                amount,
                NodeId::new(9),
                InputPortIndex(0),
            )
            .unwrap();
        assert_eq!(graph.edge_count(), 1);

        let graph =
            remove_custom_port(graph, in_id(), "amount", NetworkContext::LayerRoot).unwrap();
        let node = node_of(&graph, in_id());
        assert!(node.outputs.iter().all(|p| p.name != "amount"));
        assert!(node.parameters.is_empty());
        assert_eq!(graph.edge_count(), 0, "the port's edge went with it");
    }

    #[test]
    fn renaming_an_in_custom_port_carries_its_parameter() {
        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "amount",
            CustomPortType::Vec2,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let graph = rename_custom_port(
            graph,
            in_id(),
            "amount",
            "offset",
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let node = node_of(&graph, in_id());
        assert!(node.outputs.iter().any(|p| p.name == "offset"));
        assert_eq!(node.parameters.len(), 1);
        assert_eq!(node.parameters[0].key, "offset");
    }

    /// Out custom ports are inputs: adding one appends, removing one deletes
    /// its edge and re-indexes the ports after it.
    #[test]
    fn out_custom_ports_add_and_remove_with_their_edges() {
        let graph = add_custom_port(
            out_graph(),
            out_id(),
            "mask",
            CustomPortType::Geometry,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let graph = add_custom_port(
            graph,
            out_id(),
            "tint",
            CustomPortType::Color,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let node = node_of(&graph, out_id());
        assert_eq!(
            node.inputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec![PORT_FRAME, "mask", "tint"]
        );
        assert_eq!(node.inputs[1].accepted_types, vec![DataTypeId::GEOMETRY]);
        assert_eq!(
            node.inputs[2].accepted_types,
            vec![DataTypeId::COLOR, DataTypeId::VEC4],
            "a colour port also takes the four floats of a vec4"
        );
        assert!(node.inputs.iter().all(|p| !p.is_param && !p.is_variadic));

        let mask_source =
            Node::new(NodeId::new(7), "shape.rect").with_output("out", DataTypeId::GEOMETRY);
        let tint_source =
            Node::new(NodeId::new(8), "constant.color").with_output("out", DataTypeId::COLOR);
        let graph = graph
            .add_node(mask_source)
            .unwrap()
            .add_node(tint_source)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(7),
                OutputPortIndex(0),
                out_id(),
                InputPortIndex(1),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(8),
                OutputPortIndex(0),
                out_id(),
                InputPortIndex(2),
            )
            .unwrap();

        let graph = remove_custom_port(graph, out_id(), "mask", NetworkContext::LayerRoot).unwrap();
        let node = node_of(&graph, out_id());
        assert_eq!(
            node.inputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec![PORT_FRAME, "tint"]
        );
        assert!(
            graph.edge(EdgeId::new(1)).is_none(),
            "the mask edge is gone"
        );
        let tint_edge = graph.edge(EdgeId::new(2)).expect("the tint edge survives");
        assert_eq!(tint_edge.target_port, InputPortIndex(1));
    }

    #[test]
    fn a_duplicate_custom_port_name_is_rejected() {
        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "amount",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let err = add_custom_port(
            graph,
            in_id(),
            "amount",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                NetworkError::Graph(GraphError::DuplicatePortName { .. })
            ),
            "{err}"
        );
    }

    /// A parameter left behind by an earlier port would be silently adopted by
    /// a new port of a different type, so the collision is refused instead.
    #[test]
    fn a_custom_port_will_not_adopt_an_existing_parameter() {
        let node = Node::new(in_id(), NET_IN_TYPE_KEY)
            .with_output(PORT_TIME, DataTypeId::SCALAR)
            .with_param("amount", ParameterValue::Float(1.0));
        let graph = Graph::new().add_node(node).unwrap();
        let err = add_custom_port(
            graph,
            in_id(),
            "amount",
            CustomPortType::Vec3,
            NetworkContext::LayerRoot,
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                NetworkError::Graph(GraphError::DuplicateParamKey { .. })
            ),
            "{err}"
        );
    }

    /// The same refusal for the wire-only types. They bring no parameter of
    /// their own, but the In node's fallback looks a parameter up **by port
    /// name** — so a Geometry port that landed on an orphaned scalar key would
    /// answer with a `Scalar`, which is the fault the typed zero exists to
    /// prevent.
    #[test]
    fn a_wire_only_port_will_not_adopt_an_existing_parameter() {
        for port_type in [
            CustomPortType::Geometry,
            CustomPortType::Field,
            CustomPortType::FrameBuffer,
            CustomPortType::Text,
        ] {
            let node = Node::new(in_id(), NET_IN_TYPE_KEY)
                .with_output(PORT_TIME, DataTypeId::SCALAR)
                .with_param("amount", ParameterValue::Float(1.0));
            let graph = Graph::new().add_node(node).unwrap();
            let err = add_custom_port(graph, in_id(), "amount", port_type, NetworkContext::Subnet)
                .unwrap_err();
            assert!(
                matches!(
                    err,
                    NetworkError::Graph(GraphError::DuplicateParamKey { .. })
                ),
                "{port_type:?}: {err}"
            );
        }
    }

    /// Renaming a wire-only port onto an occupied parameter key is the same
    /// fault reached from the other side, and is refused by `Graph` itself.
    #[test]
    fn renaming_onto_an_occupied_parameter_key_is_refused() {
        let node = Node::new(in_id(), NET_IN_TYPE_KEY)
            .with_output("shape", DataTypeId::GEOMETRY)
            .with_param("intensity", ParameterValue::Float(1.0));
        let graph = Graph::new().add_node(node).unwrap();
        let err = rename_custom_port(graph, in_id(), "shape", "intensity", NetworkContext::Subnet)
            .unwrap_err();
        assert!(
            matches!(
                err,
                NetworkError::Graph(GraphError::DuplicateParamKey { .. })
            ),
            "{err}"
        );
    }

    #[test]
    fn ordinary_nodes_have_no_custom_ports() {
        let graph = Graph::new()
            .add_node(
                Node::new(NodeId::new(3), "blur").with_output("out", DataTypeId::FRAME_BUFFER),
            )
            .unwrap();
        let err = add_custom_port(
            graph.clone(),
            NodeId::new(3),
            "extra",
            CustomPortType::Float,
            NetworkContext::Subnet,
        )
        .unwrap_err();
        assert!(matches!(err, NetworkError::NotInterfaceNode(_)), "{err}");
        assert!(matches!(
            remove_custom_port(graph, NodeId::new(3), "out", NetworkContext::Subnet).unwrap_err(),
            NetworkError::NotInterfaceNode(_)
        ));
    }

    #[test]
    fn removing_a_port_that_does_not_exist_is_an_error() {
        let err =
            remove_custom_port(in_graph(), in_id(), "nope", NetworkContext::LayerRoot).unwrap_err();
        assert!(
            matches!(err, NetworkError::Graph(GraphError::PortNotFound { .. })),
            "{err}"
        );
    }
}
