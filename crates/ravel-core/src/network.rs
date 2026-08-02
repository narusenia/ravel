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
//! Custom ports are edited through [`add_custom_port`], [`remove_custom_port`],
//! [`rename_custom_port`], [`set_custom_port_type`] and [`move_custom_port`],
//! which wrap the generic `Graph` port operations with the two rules only this
//! module knows: which types a [`NetworkContext`] admits, and which ports the
//! shell owns and therefore nobody may remove, rename, retype or reorder
//! ([`is_fixed_port`]).
//!
//! A `subnet` node's own pins are not edited at all: they are **derived** from
//! the In / Out pair of the graph it owns. [`seed_subnet_node`] builds that
//! pair when the node is created and [`sync_subnet_pins`] re-derives the pins
//! after every inner edit and on load (REQ-LAYER-003).

use crate::animation::channel::AnimationChannel;
use crate::graph::{
    Graph, GraphError, InputPort, Node, OutputPort, Parameter, ParameterValue, PortSide,
};
use crate::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
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

/// The types an Out node may declare: everything except `Int` and `Bool`.
///
/// See [`CustomPortType::allowed_for_out`] — the Out side has nowhere to store
/// the parameter kind, so those two are indistinguishable from `Float` the
/// moment they are created.
const OUT_PORT_TYPES: [CustomPortType; 8] = [
    CustomPortType::Float,
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

    /// The types an Out node may declare — every wire type, in every context,
    /// **minus `Int` and `Bool`**.
    ///
    /// REQ-LAYER-002 states the Out node's custom ports as "GEOMETRY / FIELD /
    /// SCALAR / COLOR etc., any type": an Out port is an exit toward the shell
    /// and toward Layer Ref (REQ-LAYER-005), and as a subnet's inner Out it is
    /// the output-pin boundary. Nothing on either side restricts it the way the
    /// shell restricts a layer-root In, so the set does not depend on
    /// [`NetworkContext`].
    ///
    /// `Int` and `Bool` are left out for a different reason: they are not a
    /// wire type but a **parameter kind**, and on this side there is nowhere to
    /// keep one. An In node's custom output pairs with a same-named parameter
    /// that remembers which of the three scalar kinds the user picked; an Out
    /// node's custom port is a bare input, so all three collapse to
    /// `accepted_types == [SCALAR]` and [`custom_port_type`] can only ever read
    /// them back as `Float`. Offering a choice that silently becomes another
    /// one is worse than not offering it, and nothing is lost: `Float` already
    /// names that port exactly. `allowed_for_in` keeps all three, where the
    /// parameter makes the distinction real.
    ///
    /// This is the set to **offer**. [`add_custom_port`] and
    /// [`set_custom_port_type`] do not reject `Int` or `Bool` on an Out node —
    /// there is nothing wrong with the port they build, it is a `[SCALAR]`
    /// input either way — they simply cannot tell it apart from `Float`
    /// afterwards, which is the whole reason not to ask.
    pub fn allowed_for_out() -> &'static [CustomPortType] {
        &OUT_PORT_TYPES
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

    /// The custom port type a bare wire type reads back as, or `None` for a
    /// wire type no custom port can declare (`VEC4`, `SCENE`, `AUDIO_BUFFER`,
    /// …).
    ///
    /// `SCALAR` answers `Float`: the parameter kind that distinguishes
    /// `Float` / `Int` / `Bool` is not in the wire type, so a caller holding a
    /// parameter has to refine the answer itself ([`custom_port_type`] does).
    fn from_data_type(data_type: DataTypeId) -> Option<Self> {
        Some(match data_type {
            DataTypeId::SCALAR => Self::Float,
            DataTypeId::VEC2 => Self::Vec2,
            DataTypeId::VEC3 => Self::Vec3,
            DataTypeId::COLOR => Self::Color,
            DataTypeId::GEOMETRY => Self::Geometry,
            DataTypeId::FIELD => Self::Field,
            DataTypeId::FRAME_BUFFER => Self::FrameBuffer,
            DataTypeId::PLAIN_TEXT => Self::Text,
            _ => return None,
        })
    }
}

/// The [`CustomPortType`] the `side` port named `name` on `node` currently
/// declares, or `None` when the port is missing or carries a wire type no
/// custom port can have.
///
/// The choice a user made is stored in two places, and reading it back needs
/// both. An **In** node's output port holds the wire type while its same-named
/// parameter holds the parameter kind — `Float`, `Int` and `Bool` are three
/// kinds behind one `SCALAR`, so the parameter is what tells them apart. An
/// **Out** node's custom port is an input with no parameter beside it, so a
/// `SCALAR` one always reads back as `Float`: nothing on that side records the
/// difference, which is exactly why [`CustomPortType::allowed_for_out`] does
/// not offer `Int` or `Bool`.
///
/// Fixed ports answer too. The Ports UI lists them read-only next to the
/// custom ones, and a row still has to show a type.
pub fn custom_port_type(node: &Node, side: PortSide, name: &str) -> Option<CustomPortType> {
    match side {
        PortSide::Input => {
            let port = node.inputs.iter().find(|p| p.name == name)?;
            CustomPortType::from_data_type(*port.accepted_types.first()?)
        }
        PortSide::Output => {
            let port = node.outputs.iter().find(|p| p.name == name)?;
            if port.data_type == DataTypeId::SCALAR {
                let param = node.parameters.iter().find(|p| p.key == name);
                return Some(match param.map(|p| &p.value) {
                    Some(ParameterValue::Int(_)) => CustomPortType::Int,
                    Some(ParameterValue::Bool(_)) => CustomPortType::Bool,
                    _ => CustomPortType::Float,
                });
            }
            CustomPortType::from_data_type(port.data_type)
        }
    }
}

/// Failure of a network-interface port edit.
#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("node {0:?} is not a network interface node")]
    NotInterfaceNode(NodeId),

    #[error("node {0:?} is not a subnet node")]
    NotSubnetNode(NodeId),

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

/// Give the custom port `name` on the network-interface node `node_id` a new
/// type, keeping its slot.
///
/// The port keeps its index, so nothing is re-indexed: an **In** node's output
/// port gets the new wire type and its same-named parameter is replaced by the
/// new type's default ([`CustomPortType::default_parameter`]) — added when the
/// old type had none, dropped when the new type has none. An **Out** node's
/// input port gets the new acceptance set.
///
/// **Edges the new type cannot carry are dropped.** This is the same trade
/// [`Graph::set_params`] makes when a retyped parameter's acceptance set
/// changes: a port whose declared type lies about what flows through it is
/// worse than a lost connection, and the loss is undone by the caller's
/// Document commit like every other graph edit. An edge whose other end still
/// accepts the new wire type is kept, so a change that does not move on the
/// wire (`Float` → `Int`, both `SCALAR`) costs nothing — the counterpart of
/// `vec4` ↔ `color` keeping its port there.
///
/// The old parameter's **value is not carried over**. There is no meaning-
/// preserving map between the kinds (what is a `Vec3` as a `Bool`?), so the
/// new type's default is the honest answer; the previous value comes back with
/// undo.
///
/// Errors when the node is not an interface node, when the type is not allowed
/// in `context` ([`CustomPortType::allowed_for_in`]), when the port does not
/// exist, or when it is fixed ([`is_fixed_port`]) — the shell declares those
/// types, so they are not the user's to change. Retyping a port to the type it
/// already has is a no-op that keeps the parameter's value. One call = one
/// consistent graph state.
pub fn set_custom_port_type(
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
    if is_fixed_port(&node, side, name) {
        return Err(NetworkError::FixedPort {
            node: node_id,
            side,
            name: name.to_string(),
        });
    }
    let index = match side {
        PortSide::Input => node.inputs.iter().position(|p| p.name == name),
        PortSide::Output => node.outputs.iter().position(|p| p.name == name),
    }
    .ok_or_else(|| GraphError::PortNotFound {
        node: node_id,
        side,
        name: name.to_string(),
    })?;
    // A wire-only type drops the parameter, and a legacy custom `f` without a
    // parameter is no longer custom — `is_fixed_port` would start reporting it
    // as the built-in frame index, leaving a port nobody can edit or delete.
    // Refuse rather than manufacture that state.
    if side == PortSide::Output
        && port_type.default_parameter().is_none()
        && is_builtin_port_name(&node, side, name)
    {
        return Err(NetworkError::ReservedPortName {
            side,
            name: name.to_string(),
        });
    }
    if custom_port_type(&node, side, name) == Some(port_type) {
        return Ok(graph);
    }

    let doomed: Vec<EdgeId> = graph
        .edges()
        .filter(|edge| match side {
            PortSide::Input => {
                edge.target == node_id && edge.target_port.0 as usize == index && {
                    let accepted = port_type.accepted_types();
                    graph
                        .node(edge.source)
                        .and_then(|n| n.outputs.get(edge.source_port.0 as usize))
                        .is_none_or(|port| !accepted.contains(&port.data_type))
                }
            }
            PortSide::Output => {
                edge.source == node_id && edge.source_port.0 as usize == index && {
                    graph
                        .node(edge.target)
                        .and_then(|n| n.inputs.get(edge.target_port.0 as usize))
                        .is_none_or(|port| !port.accepted_types.contains(&port_type.data_type()))
                }
            }
        })
        .map(|edge| edge.id)
        .collect();
    let mut graph = graph;
    for id in doomed {
        graph = graph.remove_edge(id)?;
    }

    // `replace_node` is safe here — and only here — because the port lists
    // keep their length and their order: no `Edge::source_port` and no
    // `ChannelSource::NodeOutput` binding changes the slot it names.
    let mut updated = (*node).clone();
    match side {
        PortSide::Input => updated.inputs[index].accepted_types = port_type.accepted_types(),
        PortSide::Output => {
            updated.outputs[index].data_type = port_type.data_type();
            let existing = updated.parameters.iter().position(|p| p.key == name);
            match (existing, port_type.default_parameter()) {
                (Some(at), Some(value)) => updated.parameters[at].value = value,
                (Some(at), None) => {
                    updated.parameters.remove(at);
                }
                (None, Some(value)) => updated.parameters.push(Parameter {
                    key: name.to_string(),
                    value,
                }),
                (None, None) => {}
            }
        }
    }
    Ok(graph.replace_node(Arc::new(updated)))
}

/// Move the custom port `name` on the network-interface node `node_id`
/// `offset` slots toward the front (negative) or the back (positive), taking
/// its edges and parameter bindings with it ([`Graph::reorder_ports`]).
///
/// **Fixed ports neither move nor are crossed.** `net.in`'s `base_geometry` /
/// `t` / `f` / `source` and `net.out`'s `frame` sit at the head of every layer
/// network, and that shape is what a user reads a network by; the shell finds
/// them by name so nothing would *break*, but there is no gain in letting one
/// custom port shuffle the common prologue out from under the next reader. A
/// step onto a fixed neighbour therefore stops the move, exactly as a step
/// past either end does — the port travels as far as it can and the call
/// succeeds. Moving a fixed port at all is an error.
///
/// `Graph::reorder_ports` is the raw permutation and knows none of this; the
/// convention lives here with [`is_fixed_port`], the only place that can
/// answer which ports the shell owns.
///
/// Errors when the node is not an interface node, when the port does not
/// exist, or when it is fixed. One call = one consistent graph state.
pub fn move_custom_port(
    graph: Graph,
    node_id: NodeId,
    name: &str,
    offset: i32,
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
    let names: Vec<String> = match side {
        PortSide::Input => node.inputs.iter().map(|p| p.name.clone()).collect(),
        PortSide::Output => node.outputs.iter().map(|p| p.name.clone()).collect(),
    };
    let index = names
        .iter()
        .position(|n| n == name)
        .ok_or_else(|| GraphError::PortNotFound {
            node: node_id,
            side,
            name: name.to_string(),
        })?;
    if offset == 0 {
        return Ok(graph);
    }

    let step: isize = if offset > 0 { 1 } else { -1 };
    // A port cannot travel further than the list is long, so walking more
    // steps than that can only spin against a `break` that has already been
    // reached. Clamping keeps `i32::MIN` from asking for two billion of them
    // and changes no outcome: the stopping conditions below are unchanged.
    let steps = (offset.unsigned_abs() as usize).min(names.len());
    let mut target = index;
    for _ in 0..steps {
        let Some(next) = target.checked_add_signed(step).filter(|n| *n < names.len()) else {
            break;
        };
        if is_fixed_port(&node, side, &names[next]) {
            break;
        }
        target = next;
    }
    if target == index {
        return Ok(graph);
    }

    let mut order = names;
    let moved = order.remove(index);
    order.insert(target, moved);
    Ok(graph.reorder_ports(node_id, side, &order)?)
}

// ===========================================================================
// Subnet pins (REQ-LAYER-003)
// ===========================================================================

/// Type key of the node that owns a nested graph.
pub const SUBNET_TYPE_KEY: &str = "subnet";

/// Whether `node` owns a nested graph by type.
pub fn is_subnet_node(node: &Node) -> bool {
    node.type_key == SUBNET_TYPE_KEY
}

/// The inner graph a freshly created subnet starts with: an In / Out pair and
/// nothing else.
///
/// **The In carries `t` and nothing else.** Of the four ports
/// [`is_fixed_port`] protects on a `net.in`, only `t` means anything here: the
/// evaluator answers `base_geometry` with the layer's base quad and `source`
/// with the composited lower stack, both shell concepts a subnet is not, and
/// `f` is auto-added at the layer root only (the plan's decision — a port
/// appearing on a subnet's inner In changes the shape of the enclosing node).
/// `t` comes from [`crate::eval::EvalContext`], which every nesting level
/// inherits, so it is correct at any depth and it keeps the node from being
/// drawn with no ports at all.
///
/// **The Out carries `frame`.** It is the port [`is_fixed_port`] protects on a
/// `net.out`, and it is what makes a subnet evaluate the moment it is created:
/// with no output pin at all [`crate::eval::EvalScope`] would be asked for a
/// value the inner Out cannot name. Unlike the In's fixed ports it has no
/// context source — `NetOutProcessor` merely collects its inputs — so it
/// becomes an ordinary output pin (see [`sync_subnet_pins`]) and a fresh
/// subnet answers with a transparent frame.
pub fn new_subnet_inner_graph(in_id: NodeId, out_id: NodeId) -> Graph {
    let mut in_node = Node::new(in_id, NET_IN_TYPE_KEY).with_output(PORT_TIME, DataTypeId::SCALAR);
    in_node.metadata.position = (0.0, 0.0);
    let mut out_node =
        Node::new(out_id, NET_OUT_TYPE_KEY).with_input(PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
    out_node.metadata.position = (360.0, 0.0);
    Graph::new()
        .add_node(in_node)
        .and_then(|graph| graph.add_node(out_node))
        .expect("two distinct ids into an empty graph")
}

/// Turn a bare `subnet` node into a working one: give it the inner graph its
/// evaluation requires and the pins that graph implies.
///
/// **Mints two node ids** ([`NodeId::next`]). That is safe wherever a node is
/// being created — the counter is past every id the document holds once
/// `Document::advance_id_counters` has run on load — and is why load-time
/// repair ([`sync_subnet_pins`]) does *not* do it: the normalizers run before
/// the counters are advanced, so a minted id there could collide with a stored
/// one.
///
/// **A minted id equal to `node.id` is skipped.** Node ids are unique
/// *globally*, not per graph: `Evaluator`'s processor table is a flat
/// `HashMap<NodeId, Arc<dyn NodeProcessor>>` with no path in the key, so a
/// subnet node and its own inner `net.in` sharing an id would fight over one
/// entry and one of the two processors would simply vanish. A caller is free
/// to construct the node with an explicit id (`Node::new(NodeId::new(k), …)`,
/// as tests and the palette's connectability probe do), and nothing keeps that
/// `k` away from the counter.
pub fn seed_subnet_node(node: &mut Node) {
    seed_subnet_node_with(node, NodeId::next);
}

/// [`seed_subnet_node`] over an injectable id source, so the collision the
/// owner id can cause is testable without racing the global counter.
///
/// `mint` must be strictly increasing — [`NodeId::next`] is — so the skip
/// below fires at most once and the two inner ids differ from each other.
fn seed_subnet_node_with(node: &mut Node, mut mint: impl FnMut() -> NodeId) {
    let owner = node.id;
    let mut next = move || loop {
        let id = mint();
        if id != owner {
            return id;
        }
    };
    let inner = new_subnet_inner_graph(next(), next());
    let (inputs, outputs) = subnet_pins(&inner).expect("the seeded inner graph has In and Out");
    node.parameters = promote_parameters(&inner, &[]);
    node.inputs = inputs;
    node.outputs = outputs;
    node.subnet = Some(Arc::new(inner));
}

/// The pins a subnet node owning `inner` must declare, or `None` when `inner`
/// is not a network (no In node, or no Out node) and there is nothing to
/// derive from.
///
/// Input pins are the inner In's **custom** output ports; output pins are
/// **every** inner Out input port. The asymmetry is the evaluator's: a fixed
/// In port is answered from the [`crate::eval::EvalContext`] or the enclosing
/// scope's own bindings (`base_geometry`, `t`, `f`, `source`), so exposing it
/// as a pin would offer the outer graph a socket nothing reads. Nothing on the
/// Out side has a source of its own — `NetOutProcessor` collects its inputs
/// and `SubnetProcessor` maps them onto the outer pins by name — so `frame` is
/// a pin like any other custom Out port.
pub fn subnet_pins(inner: &Graph) -> Option<(Vec<InputPort>, Vec<OutputPort>)> {
    let in_node = find_in_node(inner)?;
    let out_node = find_out_node(inner)?;
    let inputs = in_node
        .outputs
        .iter()
        .filter(|port| !is_fixed_port(in_node, PortSide::Output, &port.name))
        .map(|port| InputPort {
            name: port.name.clone(),
            accepted_types: CustomPortType::from_data_type(port.data_type)
                .map(CustomPortType::accepted_types)
                .unwrap_or_else(|| vec![port.data_type]),
            is_param: false,
            is_variadic: false,
        })
        .collect();
    let outputs = out_node
        .inputs
        .iter()
        .map(|port| OutputPort {
            name: port.name.clone(),
            data_type: port
                .accepted_types
                .first()
                .copied()
                .unwrap_or(DataTypeId::SCALAR),
        })
        .collect();
    Some((inputs, outputs))
}

/// Whether `value` is a parameter of the kind a promotion parameter for `ty`
/// has to be. `Float` admits both representations: the default is a channel
/// (custom In parameters are keyframable) but a plain `Float` is a legitimate
/// value for the same port and must not be reset by a sync.
fn promote_parameter_matches(value: &ParameterValue, ty: CustomPortType) -> bool {
    matches!(
        (ty, value),
        (
            CustomPortType::Float,
            ParameterValue::Float(_) | ParameterValue::Channel(_)
        ) | (CustomPortType::Int, ParameterValue::Int(_))
            | (CustomPortType::Bool, ParameterValue::Bool(_))
            | (CustomPortType::Vec2, ParameterValue::Channel2(_))
            | (CustomPortType::Vec3, ParameterValue::Channel3(_))
            | (CustomPortType::Color, ParameterValue::Channel4(_))
    )
}

/// The parameters a subnet node owning `inner` must carry: one per input pin
/// whose type has a parameter representation, and nothing else.
///
/// An **unconnected** input pin reads the subnet node's parameter of the same
/// name (`SubnetProcessor::process`, REQ-LAYER-003), so the set of promotion
/// parameters is a function of the pins — which is why this owns the whole
/// parameter list rather than editing it in place. `subnet` is excluded from
/// [`Node::supports_param_ports`], so nothing else puts a parameter there.
///
/// A surviving parameter keeps its value when its kind still fits the pin;
/// otherwise the pin was retyped and the honest answer is the new type's
/// default, exactly as in [`set_custom_port_type`]. A **new** parameter is
/// seeded from the inner In's own same-named parameter when the kinds agree:
/// that value is the default the inner network already evaluates with when the
/// subnet has no parameter, so promotion must not change what the subnet
/// computes on the frame it appears.
fn promote_parameters(inner: &Graph, existing: &[Parameter]) -> Vec<Parameter> {
    let Some(in_node) = find_in_node(inner) else {
        return Vec::new();
    };
    in_node
        .outputs
        .iter()
        .filter(|port| !is_fixed_port(in_node, PortSide::Output, &port.name))
        .filter_map(|port| {
            let ty = custom_port_type(in_node, PortSide::Output, &port.name)?;
            let value = existing
                .iter()
                .find(|p| p.key == port.name)
                .map(|p| &p.value)
                .filter(|value| promote_parameter_matches(value, ty))
                .or_else(|| {
                    in_node
                        .parameters
                        .iter()
                        .find(|p| p.key == port.name)
                        .map(|p| &p.value)
                        .filter(|value| promote_parameter_matches(value, ty))
                })
                .cloned()
                .or_else(|| ty.default_parameter())?;
            Some(Parameter {
                key: port.name.clone(),
                value,
            })
        })
        .collect()
}

/// Re-derive the pins of the subnet node `subnet_id` from its own inner graph
/// ([`subnet_pins`]), remapping the outer wiring onto the result.
///
/// Pins are matched to the inner declaration **by name**: a pin whose name
/// survives keeps its edges wherever its slot moved to, and a pin whose name
/// is gone takes its edges (and, on the output side, its `NodeOutput`
/// parameter bindings) with it. A pin whose type changed keeps its slot and
/// drops only the outer edges the new wire type cannot carry — the trade
/// [`set_custom_port_type`] makes one level down. Promotion parameters follow
/// the pins ([`promote_parameters`]).
///
/// Call it **after every commit of an inner graph** and **on load**; a graph
/// whose pins already agree with its inner declaration is returned untouched,
/// so both are cheap and idempotent, and the whole re-derivation lands in the
/// caller's Document commit — one inner edit stays one undo step.
///
/// A subnet with **no inner graph at all** is left alone: repairing it means
/// minting node ids, which load-time normalization cannot do safely (see
/// [`seed_subnet_node`]). So is one whose inner graph has no In or no Out —
/// deriving pins from half a network would delete the user's wiring on the
/// strength of a malformed inner graph.
///
/// Errors when `subnet_id` is absent or is not a subnet node.
pub fn sync_subnet_pins(graph: Graph, subnet_id: NodeId) -> Result<Graph, NetworkError> {
    let node = graph
        .node(subnet_id)
        .ok_or(GraphError::NodeNotFound(subnet_id))?
        .clone();
    if !is_subnet_node(&node) {
        return Err(NetworkError::NotSubnetNode(subnet_id));
    }
    let Some(inner) = node.subnet.as_deref() else {
        return Ok(graph);
    };
    let Some((inputs, outputs)) = subnet_pins(inner) else {
        return Ok(graph);
    };
    // Only to answer "is anything out of step?". The list that is actually
    // written is re-derived at the end, from the node the port operations
    // leave behind.
    let settled = promote_parameters(inner, &node.parameters);
    if node.inputs == inputs && node.outputs == outputs && node.parameters == settled {
        return Ok(graph);
    }

    // Names first: drop the pins the inner graph no longer declares (the
    // graph operation deletes their edges and re-indexes the rest), append
    // the new ones, then put the whole list into the declared order. Only
    // after that do the slots line up one-to-one with `inputs` / `outputs`.
    let mut graph = graph;
    for (index, port) in node.inputs.iter().enumerate().rev() {
        if !inputs.iter().any(|p| p.name == port.name) {
            graph = graph.remove_input_port(subnet_id, InputPortIndex(index as u32))?;
        }
    }
    for (index, port) in node.outputs.iter().enumerate().rev() {
        if !outputs.iter().any(|p| p.name == port.name) {
            graph = graph.remove_output_port(subnet_id, OutputPortIndex(index as u32))?;
        }
    }
    for port in &inputs {
        if !node.inputs.iter().any(|p| p.name == port.name) {
            let at = live_node(&graph, subnet_id).inputs.len();
            graph = graph.insert_input_port(subnet_id, at, port.clone())?;
        }
    }
    for port in &outputs {
        if !node.outputs.iter().any(|p| p.name == port.name) {
            let at = live_node(&graph, subnet_id).outputs.len();
            graph = graph.insert_output_port(subnet_id, at, port.clone())?;
        }
    }
    let order: Vec<String> = inputs.iter().map(|p| p.name.clone()).collect();
    graph = graph.reorder_ports(subnet_id, PortSide::Input, &order)?;
    let order: Vec<String> = outputs.iter().map(|p| p.name.clone()).collect();
    graph = graph.reorder_ports(subnet_id, PortSide::Output, &order)?;

    // Retyped pins keep their slot, so nothing is re-indexed; the outer edges
    // the new wire type cannot carry are the only casualties.
    let current = live_node(&graph, subnet_id).clone();
    let mut doomed: Vec<EdgeId> = Vec::new();
    for (index, desired) in inputs.iter().enumerate() {
        if current.inputs[index].accepted_types == desired.accepted_types {
            continue;
        }
        doomed.extend(
            graph
                .edges()
                .filter(|edge| edge.target == subnet_id && edge.target_port.0 as usize == index)
                .filter(|edge| {
                    graph
                        .node(edge.source)
                        .and_then(|n| n.outputs.get(edge.source_port.0 as usize))
                        .is_none_or(|port| !desired.accepted_types.contains(&port.data_type))
                })
                .map(|edge| edge.id),
        );
    }
    for (index, desired) in outputs.iter().enumerate() {
        if current.outputs[index].data_type == desired.data_type {
            continue;
        }
        doomed.extend(
            graph
                .edges()
                .filter(|edge| edge.source == subnet_id && edge.source_port.0 as usize == index)
                .filter(|edge| {
                    graph
                        .node(edge.target)
                        .and_then(|n| n.inputs.get(edge.target_port.0 as usize))
                        .is_none_or(|port| !port.accepted_types.contains(&desired.data_type))
                })
                .map(|edge| edge.id),
        );
    }
    for id in doomed {
        graph = graph.remove_edge(id)?;
    }

    // `replace_node` is safe here: the lists already carry the declared names
    // in the declared order, so no `Edge` port index and no
    // `ChannelSource::NodeOutput` binding changes the slot it names.
    let mut updated = live_node(&graph, subnet_id).clone();
    // Derived from the node the port operations left behind, not from the one
    // this pass started with: `remove_output_port` remaps (and, for a vanished
    // pin, collapses) the `ChannelSource::NodeOutput` bindings a parameter can
    // hold, and writing back a list captured before that would quietly restore
    // the stale indices.
    updated.parameters = promote_parameters(inner, &updated.parameters);
    updated.inputs = inputs;
    updated.outputs = outputs;
    Ok(graph.replace_node(Arc::new(updated)))
}

fn live_node(graph: &Graph, node_id: NodeId) -> Node {
    (**graph
        .node(node_id)
        .expect("the subnet node is only edited, never removed, by this pass"))
    .clone()
}

/// Run [`sync_subnet_pins`] over every subnet node directly inside `graph`.
///
/// One level only. Load-time repair reaches nested subnets by composing this
/// with the document's own subnet walk, which rewrites inner graphs before
/// their owners so a nested repair is never discarded by the outer one.
pub fn sync_subnet_pins_in(graph: &Graph) -> Graph {
    let mut synced = graph.clone();
    let subnets: Vec<NodeId> = synced
        .nodes()
        .filter(|node| is_subnet_node(node))
        .map(|node| node.id)
        .collect();
    for id in subnets {
        synced = sync_subnet_pins(synced.clone(), id).unwrap_or(synced);
    }
    synced
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

    // ----- retype ----------------------------------------------------------

    /// A port that read back as one type reads back as the new one, keeping
    /// its slot, and its parameter follows the kind it now names.
    #[test]
    fn retyping_an_in_port_moves_its_parameter_to_the_new_kind() {
        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "amount",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let before = node_of(&graph, in_id())
            .outputs
            .iter()
            .position(|p| p.name == "amount")
            .unwrap();

        let graph = set_custom_port_type(
            graph,
            in_id(),
            "amount",
            CustomPortType::Vec3,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let node = node_of(&graph, in_id());
        assert_eq!(
            node.outputs.iter().position(|p| p.name == "amount"),
            Some(before),
            "the port keeps its slot"
        );
        assert_eq!(node.outputs[before].data_type, DataTypeId::VEC3);
        assert_eq!(
            custom_port_type(node, PortSide::Output, "amount"),
            Some(CustomPortType::Vec3)
        );
        let param = node.parameters.iter().find(|p| p.key == "amount").unwrap();
        assert_eq!(param.value.port_data_type(), Some(DataTypeId::VEC3));
    }

    /// The three scalar kinds are told apart by the parameter, not the wire
    /// type, and the retype rewrites it.
    #[test]
    fn the_scalar_kinds_round_trip_through_their_parameter() {
        let mut graph = add_custom_port(
            in_graph(),
            in_id(),
            "amount",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        for kind in [
            CustomPortType::Int,
            CustomPortType::Bool,
            CustomPortType::Float,
        ] {
            graph = set_custom_port_type(graph, in_id(), "amount", kind, NetworkContext::LayerRoot)
                .unwrap();
            let node = node_of(&graph, in_id());
            assert_eq!(
                node.outputs
                    .iter()
                    .find(|p| p.name == "amount")
                    .unwrap()
                    .data_type,
                DataTypeId::SCALAR,
                "{kind:?} still travels as a scalar"
            );
            assert_eq!(
                custom_port_type(node, PortSide::Output, "amount"),
                Some(kind)
            );
        }
    }

    /// Retyping to the type the port already has changes nothing — in
    /// particular it does not reset the parameter the user has been editing.
    #[test]
    fn retyping_to_the_current_type_keeps_the_parameter_value() {
        let node = Node::new(in_id(), NET_IN_TYPE_KEY)
            .with_output(PORT_TIME, DataTypeId::SCALAR)
            .with_output("amount", DataTypeId::SCALAR)
            .with_param("amount", ParameterValue::Int(7));
        let graph = Graph::new().add_node(node).unwrap();
        let graph = set_custom_port_type(
            graph,
            in_id(),
            "amount",
            CustomPortType::Int,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        assert_eq!(
            node_of(&graph, in_id())
                .parameters
                .iter()
                .find(|p| p.key == "amount")
                .map(|p| p.value.clone()),
            Some(ParameterValue::Int(7))
        );
    }

    /// A wire-only type takes the parameter away and a parameter-carrying one
    /// brings it back (inside a subnet, where the wire-only types are legal).
    #[test]
    fn retyping_across_the_parameter_boundary_adds_and_drops_the_parameter() {
        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "amount",
            CustomPortType::Float,
            NetworkContext::Subnet,
        )
        .unwrap();
        let graph = set_custom_port_type(
            graph,
            in_id(),
            "amount",
            CustomPortType::Geometry,
            NetworkContext::Subnet,
        )
        .unwrap();
        assert!(
            node_of(&graph, in_id())
                .parameters
                .iter()
                .all(|p| p.key != "amount"),
            "a Geometry port has no parameter representation"
        );

        let graph = set_custom_port_type(
            graph,
            in_id(),
            "amount",
            CustomPortType::Color,
            NetworkContext::Subnet,
        )
        .unwrap();
        let param = node_of(&graph, in_id())
            .parameters
            .iter()
            .find(|p| p.key == "amount")
            .expect("a Color port carries one again");
        assert_eq!(param.value.port_data_type(), Some(DataTypeId::COLOR));
    }

    /// An edge the new wire type cannot travel is dropped; one that still
    /// fits survives, so a retype that does not move on the wire costs
    /// nothing.
    #[test]
    fn retyping_drops_only_the_edges_the_new_type_cannot_carry() {
        let scalar_sink =
            Node::new(NodeId::new(9), "blur").with_input("value", &[DataTypeId::SCALAR]);
        let any_sink = Node::new(NodeId::new(10), "debug")
            .with_input("value", &[DataTypeId::SCALAR, DataTypeId::VEC3]);
        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "amount",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap()
        .add_node(scalar_sink)
        .unwrap()
        .add_node(any_sink)
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
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                in_id(),
                amount,
                NodeId::new(10),
                InputPortIndex(0),
            )
            .unwrap();

        // Float → Int stays SCALAR: both edges still carry what they carried.
        let same_wire = set_custom_port_type(
            graph.clone(),
            in_id(),
            "amount",
            CustomPortType::Int,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        assert_eq!(same_wire.edge_count(), 2);

        // Float → Vec3 keeps only the sink that accepts VEC3.
        let retyped = set_custom_port_type(
            graph,
            in_id(),
            "amount",
            CustomPortType::Vec3,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        assert!(
            retyped.edge(EdgeId::new(1)).is_none(),
            "the scalar-only sink lost its edge"
        );
        assert!(
            retyped.edge(EdgeId::new(2)).is_some(),
            "the sink that also accepts VEC3 kept its edge"
        );
    }

    /// An Out node's custom port is an input: the acceptance set is what
    /// changes, and an incoming edge the new set refuses goes with it.
    #[test]
    fn retyping_an_out_port_replaces_its_acceptance_set() {
        let source =
            Node::new(NodeId::new(7), "shape.rect").with_output("out", DataTypeId::GEOMETRY);
        let graph = add_custom_port(
            out_graph(),
            out_id(),
            "mask",
            CustomPortType::Geometry,
            NetworkContext::LayerRoot,
        )
        .unwrap()
        .add_node(source)
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            NodeId::new(7),
            OutputPortIndex(0),
            out_id(),
            InputPortIndex(1),
        )
        .unwrap();

        let graph = set_custom_port_type(
            graph,
            out_id(),
            "mask",
            CustomPortType::Color,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let node = node_of(&graph, out_id());
        assert_eq!(
            node.inputs[1].accepted_types,
            vec![DataTypeId::COLOR, DataTypeId::VEC4]
        );
        assert_eq!(
            graph.edge_count(),
            0,
            "the GEOMETRY edge cannot feed a colour port"
        );
        assert!(
            node.parameters.is_empty(),
            "an Out port carries no parameter"
        );
    }

    #[test]
    fn a_fixed_port_cannot_be_retyped() {
        let err = set_custom_port_type(
            in_graph(),
            in_id(),
            PORT_BASE_GEOMETRY,
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap_err();
        assert!(matches!(err, NetworkError::FixedPort { .. }), "{err}");
    }

    #[test]
    fn a_layer_root_in_port_cannot_be_retyped_to_a_wire_only_type() {
        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "amount",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let err = set_custom_port_type(
            graph,
            in_id(),
            "amount",
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
    }

    /// Retyping a legacy custom `f` to a parameterless type would leave a
    /// port that `is_fixed_port` reports as the built-in frame index — one
    /// the user could then neither drive nor delete.
    #[test]
    fn a_legacy_f_port_cannot_be_retyped_into_the_builtin() {
        let err = set_custom_port_type(
            legacy_f_graph(),
            in_id(),
            PORT_FRAME_INDEX,
            CustomPortType::Geometry,
            NetworkContext::Subnet,
        )
        .unwrap_err();
        assert!(
            matches!(err, NetworkError::ReservedPortName { .. }),
            "{err}"
        );
        // The parameter-carrying types stay available.
        let graph = set_custom_port_type(
            legacy_f_graph(),
            in_id(),
            PORT_FRAME_INDEX,
            CustomPortType::Vec2,
            NetworkContext::Subnet,
        )
        .unwrap();
        assert!(!is_fixed_port(
            node_of(&graph, in_id()),
            PortSide::Output,
            PORT_FRAME_INDEX
        ));
    }

    // ----- reorder ---------------------------------------------------------

    /// Two custom ports swap and every edge keeps the port it was drawn to.
    #[test]
    fn moving_a_custom_port_carries_its_edges() {
        let sink = Node::new(NodeId::new(9), "blur")
            .with_input("a", &[DataTypeId::SCALAR])
            .with_input("b", &[DataTypeId::SCALAR]);
        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "first",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let graph = add_custom_port(
            graph,
            in_id(),
            "second",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap()
        .add_node(sink)
        .unwrap();
        let first = output_port_index(node_of(&graph, in_id()), "first").unwrap();
        let second = output_port_index(node_of(&graph, in_id()), "second").unwrap();
        let graph = graph
            .add_edge(
                EdgeId::new(1),
                in_id(),
                first,
                NodeId::new(9),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                in_id(),
                second,
                NodeId::new(9),
                InputPortIndex(1),
            )
            .unwrap();

        let graph = move_custom_port(graph, in_id(), "second", -1).unwrap();
        let node = node_of(&graph, in_id());
        assert_eq!(
            node.outputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                PORT_BASE_GEOMETRY,
                PORT_TIME,
                PORT_FRAME_INDEX,
                PORT_SOURCE,
                "second",
                "first",
            ]
        );
        assert_eq!(
            graph.edge(EdgeId::new(1)).unwrap().source_port,
            output_port_index(node, "first").unwrap()
        );
        assert_eq!(
            graph.edge(EdgeId::new(2)).unwrap().source_port,
            output_port_index(node, "second").unwrap()
        );
    }

    /// The fixed prologue is a wall: a custom port stops in front of it
    /// instead of displacing `base_geometry` / `t` / `f` / `source`.
    #[test]
    fn a_custom_port_does_not_step_over_a_fixed_one() {
        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "amount",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let moved = move_custom_port(graph.clone(), in_id(), "amount", -3).unwrap();
        assert_eq!(
            node_of(&moved, in_id())
                .outputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                PORT_BASE_GEOMETRY,
                PORT_TIME,
                PORT_FRAME_INDEX,
                PORT_SOURCE,
                "amount",
            ],
            "the move stops at the fixed prologue and the call still succeeds"
        );
        // Past the end is the same no-op.
        let moved = move_custom_port(graph, in_id(), "amount", 4).unwrap();
        assert_eq!(
            node_of(&moved, in_id()).outputs.last().unwrap().name,
            "amount"
        );
    }

    #[test]
    fn a_fixed_port_cannot_be_moved() {
        let err = move_custom_port(in_graph(), in_id(), PORT_TIME, 1).unwrap_err();
        assert!(matches!(err, NetworkError::FixedPort { .. }), "{err}");
    }

    /// An extreme offset lands where the same walk would with a small one —
    /// the stopping conditions decide the result, not the number asked for,
    /// and the number never costs more steps than the list is long.
    #[test]
    fn an_extreme_offset_stops_at_the_same_place() {
        let graph = add_custom_port(
            in_graph(),
            in_id(),
            "first",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let graph = add_custom_port(
            graph,
            in_id(),
            "second",
            CustomPortType::Float,
            NetworkContext::LayerRoot,
        )
        .unwrap();
        let order = |graph: &Graph| {
            node_of(graph, in_id())
                .outputs
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(&move_custom_port(graph.clone(), in_id(), "second", i32::MIN).unwrap()),
            order(&move_custom_port(graph.clone(), in_id(), "second", -1).unwrap()),
            "both stop in front of the fixed prologue"
        );
        assert_eq!(
            order(&move_custom_port(graph.clone(), in_id(), "first", i32::MAX).unwrap()),
            order(&move_custom_port(graph, in_id(), "first", 1).unwrap()),
            "both stop at the end of the list"
        );
    }

    // ----- the type menus --------------------------------------------------

    /// An Out node's custom port is a bare input with no parameter beside it,
    /// so the three scalar kinds collapse into one `[SCALAR]` port. The menu
    /// therefore offers only `Float`: a choice that silently reads back as
    /// something else is worse than no choice, and `Float` names that port
    /// exactly. An In node keeps all three, where the parameter records which.
    #[test]
    fn the_out_menu_omits_the_kinds_it_cannot_read_back() {
        for port_type in [CustomPortType::Int, CustomPortType::Bool] {
            assert!(
                !CustomPortType::allowed_for_out().contains(&port_type),
                "{port_type:?} is indistinguishable from Float on an Out node"
            );
            assert!(
                CustomPortType::allowed_for_in(NetworkContext::LayerRoot).contains(&port_type),
                "{port_type:?} is a real choice on an In node"
            );
        }
        assert_eq!(CustomPortType::allowed_for_out().len(), 8);

        // The reason, demonstrated: an Out port built from any scalar kind
        // reads back as `Float`, because nothing on that side stored the kind.
        for port_type in [
            CustomPortType::Float,
            CustomPortType::Int,
            CustomPortType::Bool,
        ] {
            let graph = add_custom_port(
                out_graph(),
                out_id(),
                "amount",
                port_type,
                NetworkContext::LayerRoot,
            )
            .unwrap();
            assert_eq!(
                custom_port_type(node_of(&graph, out_id()), PortSide::Input, "amount"),
                Some(CustomPortType::Float),
                "{port_type:?}"
            );
        }
    }

    /// Out custom ports reorder on the input side, past the fixed `frame`.
    #[test]
    fn out_custom_ports_reorder_behind_the_frame_port() {
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
        let graph = move_custom_port(graph, out_id(), "tint", -5).unwrap();
        assert_eq!(
            node_of(&graph, out_id())
                .inputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec![PORT_FRAME, "tint", "mask"]
        );
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

    // -- subnet pins --------------------------------------------------------

    fn subnet_id() -> NodeId {
        NodeId::new(50)
    }

    fn source_id() -> NodeId {
        NodeId::new(60)
    }

    fn sink_id() -> NodeId {
        NodeId::new(61)
    }

    /// A subnet node with a seeded inner graph, sitting alone in a graph.
    fn subnet_graph() -> Graph {
        let inner = new_subnet_inner_graph(in_id(), out_id());
        let (inputs, outputs) = subnet_pins(&inner).unwrap();
        let mut node = Node::new(subnet_id(), SUBNET_TYPE_KEY);
        node.inputs = inputs;
        node.outputs = outputs;
        node.subnet = Some(Arc::new(inner));
        Graph::new().add_node(node).unwrap()
    }

    /// Rewrite the subnet's inner graph with `edit` and re-derive its pins,
    /// exactly as a commit of the inner network does.
    fn edit_inner(graph: Graph, edit: impl FnOnce(Graph) -> Graph) -> Graph {
        let node = node_of(&graph, subnet_id());
        let inner = edit(node.subnet.as_deref().unwrap().clone());
        let mut updated = node.clone();
        updated.subnet = Some(Arc::new(inner));
        let graph = graph.replace_node(Arc::new(updated));
        sync_subnet_pins(graph, subnet_id()).unwrap()
    }

    fn pin_names(graph: &Graph, side: PortSide) -> Vec<String> {
        let node = node_of(graph, subnet_id());
        match side {
            PortSide::Input => node.inputs.iter().map(|p| p.name.clone()).collect(),
            PortSide::Output => node.outputs.iter().map(|p| p.name.clone()).collect(),
        }
    }

    /// Every edge as (source, source port name, target, target port name), so
    /// a comparison across a reorder is about connections rather than indices.
    fn connections(graph: &Graph) -> Vec<(NodeId, String, NodeId, String)> {
        let mut wired: Vec<_> = graph
            .edges()
            .map(|edge| {
                let source = graph.node(edge.source).unwrap();
                let target = graph.node(edge.target).unwrap();
                (
                    edge.source,
                    source.outputs[edge.source_port.0 as usize].name.clone(),
                    edge.target,
                    target.inputs[edge.target_port.0 as usize].name.clone(),
                )
            })
            .collect();
        wired.sort();
        wired
    }

    /// A fresh subnet is a working node: it owns an In / Out pair, exposes the
    /// Out's `frame` as its only pin, and offers no input pin for the In's
    /// context-supplied `t`.
    #[test]
    fn a_seeded_subnet_exposes_the_inner_frame_port_and_nothing_else() {
        let graph = subnet_graph();
        let node = node_of(&graph, subnet_id());
        let inner = node.subnet.as_deref().unwrap();

        assert!(find_in_node(inner).is_some() && find_out_node(inner).is_some());
        assert_eq!(
            find_in_node(inner)
                .unwrap()
                .outputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec![PORT_TIME],
            "`f` is auto-added at the layer root only"
        );
        assert!(
            node.inputs.is_empty(),
            "`t` comes from the context, not a pin"
        );
        assert_eq!(pin_names(&graph, PortSide::Output), vec![PORT_FRAME]);
        assert_eq!(
            node.outputs[0].data_type,
            DataTypeId::FRAME_BUFFER,
            "the pin carries the inner port's wire type"
        );
    }

    /// Node ids are unique **globally**, not per graph: `Evaluator`'s
    /// processor table is keyed by `NodeId` alone, with no ownership path, so
    /// a subnet node sharing an id with its own inner `net.in` would leave one
    /// of the two processors unreachable. An explicit owner id that the
    /// counter is about to hand out again must therefore be skipped.
    #[test]
    fn seeding_never_gives_an_inner_node_the_owner_id() {
        let owner = NodeId::new(7);
        // An id source whose first answer is the owner id — exactly what the
        // global counter does when a caller builds the node with `new(k)` and
        // `k` happens to be where the counter stands.
        let mut handed = [owner, NodeId::new(8), NodeId::new(9)].into_iter();
        let mut node = Node::new(owner, SUBNET_TYPE_KEY);
        seed_subnet_node_with(&mut node, || handed.next().expect("three ids are enough"));

        let inner = node.subnet.as_deref().unwrap();
        assert!(
            !inner.node_ids().any(|id| id == owner),
            "the inner graph reused the subnet node's own id"
        );
        assert_eq!(inner.node_count(), 2);
    }

    /// A custom port on the inner In becomes an input pin; a custom port on
    /// the inner Out becomes an output pin beside `frame`.
    #[test]
    fn inner_custom_ports_become_outer_pins() {
        let graph = edit_inner(subnet_graph(), |inner| {
            let inner = add_custom_port(
                inner,
                in_id(),
                "amount",
                CustomPortType::Float,
                NetworkContext::Subnet,
            )
            .unwrap();
            add_custom_port(
                inner,
                out_id(),
                "mask",
                CustomPortType::Geometry,
                NetworkContext::Subnet,
            )
            .unwrap()
        });

        assert_eq!(pin_names(&graph, PortSide::Input), vec!["amount"]);
        assert_eq!(
            pin_names(&graph, PortSide::Output),
            vec![PORT_FRAME, "mask"]
        );
        assert_eq!(
            node_of(&graph, subnet_id()).inputs[0].accepted_types,
            vec![DataTypeId::SCALAR]
        );
    }

    /// A subnet's inner In may declare wire-only types (REQ-LAYER-003), and
    /// the pin takes the same acceptance set a custom port would.
    #[test]
    fn a_geometry_pin_accepts_geometry_and_carries_no_parameter() {
        let graph = edit_inner(subnet_graph(), |inner| {
            add_custom_port(
                inner,
                in_id(),
                "shape",
                CustomPortType::Geometry,
                NetworkContext::Subnet,
            )
            .unwrap()
        });
        let node = node_of(&graph, subnet_id());
        assert_eq!(node.inputs[0].accepted_types, vec![DataTypeId::GEOMETRY]);
        assert!(
            node.parameters.is_empty(),
            "GEOMETRY has no parameter representation to promote"
        );
    }

    /// Removing an inner In port removes its pin and its outer edge; the pins
    /// that survive keep the connections they had.
    #[test]
    fn removing_an_inner_port_drops_only_its_outer_edge() {
        let graph = edit_inner(subnet_graph(), |inner| {
            let inner = add_custom_port(
                inner,
                in_id(),
                "a",
                CustomPortType::Float,
                NetworkContext::Subnet,
            )
            .unwrap();
            add_custom_port(
                inner,
                in_id(),
                "b",
                CustomPortType::Float,
                NetworkContext::Subnet,
            )
            .unwrap()
        });
        let source = Node::new(source_id(), "constant")
            .with_output("x", DataTypeId::SCALAR)
            .with_output("y", DataTypeId::SCALAR);
        let graph = graph
            .add_node(source)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                source_id(),
                OutputPortIndex(0),
                subnet_id(),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                source_id(),
                OutputPortIndex(1),
                subnet_id(),
                InputPortIndex(1),
            )
            .unwrap();

        let graph = edit_inner(graph, |inner| {
            remove_custom_port(inner, in_id(), "a", NetworkContext::Subnet).unwrap()
        });

        assert_eq!(pin_names(&graph, PortSide::Input), vec!["b"]);
        assert_eq!(
            connections(&graph),
            vec![(source_id(), "y".into(), subnet_id(), "b".into())],
            "the surviving pin keeps its edge at its new index"
        );
    }

    /// Reordering the inner In's ports reorders the pins and moves every outer
    /// edge with the pin it was drawn to.
    #[test]
    fn reordering_inner_ports_carries_the_outer_edges() {
        let graph = edit_inner(subnet_graph(), |inner| {
            let inner = add_custom_port(
                inner,
                in_id(),
                "a",
                CustomPortType::Float,
                NetworkContext::Subnet,
            )
            .unwrap();
            add_custom_port(
                inner,
                in_id(),
                "b",
                CustomPortType::Float,
                NetworkContext::Subnet,
            )
            .unwrap()
        });
        let source = Node::new(source_id(), "constant")
            .with_output("x", DataTypeId::SCALAR)
            .with_output("y", DataTypeId::SCALAR);
        let graph = graph
            .add_node(source)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                source_id(),
                OutputPortIndex(0),
                subnet_id(),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                source_id(),
                OutputPortIndex(1),
                subnet_id(),
                InputPortIndex(1),
            )
            .unwrap();
        let before = connections(&graph);

        let graph = edit_inner(graph, |inner| {
            move_custom_port(inner, in_id(), "b", -1).unwrap()
        });

        assert_eq!(pin_names(&graph, PortSide::Input), vec!["b", "a"]);
        assert_eq!(
            connections(&graph),
            before,
            "every connection survives the permutation"
        );
    }

    /// An output pin that vanishes takes its downstream edge with it, and the
    /// remaining pins keep theirs.
    #[test]
    fn removing_an_inner_out_port_drops_its_downstream_edge() {
        let graph = edit_inner(subnet_graph(), |inner| {
            add_custom_port(
                inner,
                out_id(),
                "mask",
                CustomPortType::Geometry,
                NetworkContext::Subnet,
            )
            .unwrap()
        });
        let sink = Node::new(sink_id(), "merge")
            .with_input("frame", &[DataTypeId::FRAME_BUFFER])
            .with_input("mask", &[DataTypeId::GEOMETRY]);
        let graph = graph
            .add_node(sink)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                subnet_id(),
                OutputPortIndex(0),
                sink_id(),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                subnet_id(),
                OutputPortIndex(1),
                sink_id(),
                InputPortIndex(1),
            )
            .unwrap();

        let graph = edit_inner(graph, |inner| {
            remove_custom_port(inner, out_id(), "mask", NetworkContext::Subnet).unwrap()
        });

        assert_eq!(pin_names(&graph, PortSide::Output), vec![PORT_FRAME]);
        assert_eq!(
            connections(&graph),
            vec![(subnet_id(), PORT_FRAME.into(), sink_id(), "frame".into())]
        );
    }

    /// A promotion parameter appears with the pin, is seeded from the inner
    /// In's own default so the subnet keeps evaluating to the same value, and
    /// disappears with the pin.
    #[test]
    fn promotion_parameters_follow_the_pins() {
        let graph = edit_inner(subnet_graph(), |inner| {
            let inner = add_custom_port(
                inner,
                in_id(),
                "amount",
                CustomPortType::Float,
                NetworkContext::Subnet,
            )
            .unwrap();
            let mut in_node = node_of(&inner, in_id()).clone();
            for param in &mut in_node.parameters {
                if param.key == "amount" {
                    param.value = ParameterValue::Float(4.5);
                }
            }
            inner.replace_node(Arc::new(in_node))
        });
        assert_eq!(
            node_of(&graph, subnet_id()).parameters,
            vec![Parameter {
                key: "amount".into(),
                value: ParameterValue::Float(4.5),
            }],
            "the promoted default is the inner network's own"
        );

        // A value the user has since set on the subnet node survives a sync.
        let mut node = node_of(&graph, subnet_id()).clone();
        node.parameters[0].value = ParameterValue::Float(9.0);
        let graph = sync_subnet_pins(graph.replace_node(Arc::new(node)), subnet_id()).unwrap();
        assert_eq!(
            node_of(&graph, subnet_id()).parameters[0].value,
            ParameterValue::Float(9.0)
        );

        let graph = edit_inner(graph, |inner| {
            remove_custom_port(inner, in_id(), "amount", NetworkContext::Subnet).unwrap()
        });
        assert!(node_of(&graph, subnet_id()).parameters.is_empty());
    }

    /// Retyping an inner In port retypes the pin in place and drops only the
    /// outer edges the new wire type cannot carry.
    #[test]
    fn retyping_an_inner_port_retypes_the_pin_and_drops_the_broken_edge() {
        let graph = edit_inner(subnet_graph(), |inner| {
            add_custom_port(
                inner,
                in_id(),
                "amount",
                CustomPortType::Float,
                NetworkContext::Subnet,
            )
            .unwrap()
        });
        let source = Node::new(source_id(), "constant").with_output("x", DataTypeId::SCALAR);
        let graph = graph
            .add_node(source)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                source_id(),
                OutputPortIndex(0),
                subnet_id(),
                InputPortIndex(0),
            )
            .unwrap();

        // `Float` → `Int` stays on the wire as SCALAR: the edge is kept.
        let graph = edit_inner(graph, |inner| {
            set_custom_port_type(
                inner,
                in_id(),
                "amount",
                CustomPortType::Int,
                NetworkContext::Subnet,
            )
            .unwrap()
        });
        assert_eq!(graph.edges().count(), 1);
        assert_eq!(
            node_of(&graph, subnet_id()).parameters[0].value,
            ParameterValue::Int(0),
            "the promotion parameter follows the kind"
        );

        // `Int` → `Geometry` does not: a SCALAR source cannot feed it.
        let graph = edit_inner(graph, |inner| {
            set_custom_port_type(
                inner,
                in_id(),
                "amount",
                CustomPortType::Geometry,
                NetworkContext::Subnet,
            )
            .unwrap()
        });
        assert_eq!(graph.edges().count(), 0);
        assert_eq!(
            node_of(&graph, subnet_id()).inputs[0].accepted_types,
            vec![DataTypeId::GEOMETRY]
        );
        assert!(node_of(&graph, subnet_id()).parameters.is_empty());
    }

    /// Drift repair: pins that disagree with the inner declaration are
    /// rebuilt, and a second pass changes nothing.
    #[test]
    fn drifted_pins_are_rebuilt_and_the_repair_is_idempotent() {
        let graph = edit_inner(subnet_graph(), |inner| {
            add_custom_port(
                inner,
                in_id(),
                "amount",
                CustomPortType::Float,
                NetworkContext::Subnet,
            )
            .unwrap()
        });

        // Forge the drift a stale archive would hold: a pin the inner In does
        // not declare, no pin for the one it does, and a bogus output.
        let mut drifted = node_of(&graph, subnet_id()).clone();
        drifted.inputs = vec![InputPort {
            name: "stale".into(),
            accepted_types: vec![DataTypeId::SCALAR],
            is_param: false,
            is_variadic: false,
        }];
        drifted.outputs = vec![OutputPort {
            name: "stale_out".into(),
            data_type: DataTypeId::SCALAR,
        }];
        drifted.parameters.clear();
        let drifted = graph.replace_node(Arc::new(drifted));

        let repaired = sync_subnet_pins(drifted, subnet_id()).unwrap();
        assert_eq!(pin_names(&repaired, PortSide::Input), vec!["amount"]);
        assert_eq!(pin_names(&repaired, PortSide::Output), vec![PORT_FRAME]);
        assert_eq!(node_of(&repaired, subnet_id()).parameters.len(), 1);

        let again = sync_subnet_pins(repaired.clone(), subnet_id()).unwrap();
        assert_eq!(
            node_of(&again, subnet_id()),
            node_of(&repaired, subnet_id())
        );
    }

    /// A subnet with no inner graph is left exactly as it is: repairing it
    /// would mean minting node ids, which load-time normalization cannot do.
    #[test]
    fn a_subnet_without_an_inner_graph_is_left_alone() {
        let node = Node::new(subnet_id(), SUBNET_TYPE_KEY).with_output("out", DataTypeId::SCALAR);
        let graph = Graph::new().add_node(node).unwrap();
        let synced = sync_subnet_pins(graph.clone(), subnet_id()).unwrap();
        assert_eq!(node_of(&synced, subnet_id()), node_of(&graph, subnet_id()));
    }

    /// Half a network is not a declaration: deriving pins from it would delete
    /// the wiring the user still has.
    #[test]
    fn an_inner_graph_without_an_out_node_is_left_alone() {
        let inner = Graph::new()
            .add_node(Node::new(in_id(), NET_IN_TYPE_KEY).with_output("a", DataTypeId::SCALAR))
            .unwrap();
        let mut node =
            Node::new(subnet_id(), SUBNET_TYPE_KEY).with_output("out", DataTypeId::SCALAR);
        node.subnet = Some(Arc::new(inner));
        let graph = Graph::new().add_node(node).unwrap();
        let synced = sync_subnet_pins(graph.clone(), subnet_id()).unwrap();
        assert_eq!(node_of(&synced, subnet_id()), node_of(&graph, subnet_id()));
    }

    #[test]
    fn syncing_a_node_that_is_not_a_subnet_is_an_error() {
        let err = sync_subnet_pins(in_graph(), in_id()).unwrap_err();
        assert!(matches!(err, NetworkError::NotSubnetNode(_)), "{err}");
    }
}
