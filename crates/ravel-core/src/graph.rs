// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Immutable node graph (DAG) with `im` persistent data structures.
//!
//! The graph stores nodes and edges in [`im::HashMap`] / [`im::Vector`] so
//! that structural sharing makes undo (version switching) cheap. All mutations
//! return a **new** `Graph`; the original is untouched.

use crate::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

// ===========================================================================
// Error
// ===========================================================================

#[derive(Debug, Error)]
pub enum GraphError {
    #[error("node {0:?} not found")]
    NodeNotFound(NodeId),

    #[error("edge {0:?} not found")]
    EdgeNotFound(EdgeId),

    #[error("adding edge {from:?} -> {to:?} would create a cycle")]
    CycleDetected { from: NodeId, to: NodeId },

    #[error("duplicate edge from {from:?}:{from_port:?} to {to:?}:{to_port:?}")]
    DuplicateEdge {
        from: NodeId,
        from_port: OutputPortIndex,
        to: NodeId,
        to_port: InputPortIndex,
    },

    #[error("duplicate node id {0:?}")]
    DuplicateNode(NodeId),

    #[error("node {node:?} has no parameter {key:?}")]
    ParamNotFound { node: NodeId, key: String },

    #[error("parameter {key:?} on node {node:?} has a type that cannot be exposed as a port")]
    UnsupportedParamType { node: NodeId, key: String },

    #[error("parameter {key:?} on node {node:?} is already exposed as a port")]
    ParamAlreadyExposed { node: NodeId, key: String },

    #[error("parameter {key:?} on node {node:?} is not exposed as a port")]
    ParamNotExposed { node: NodeId, key: String },

    #[error("node {0:?} does not support parameter ports (synthetic or network-interface node)")]
    ParamPortsUnsupported(NodeId),

    #[error("input port {port:?} on node {node:?} is not a variadic input port")]
    VariadicInputPortNotFound { node: NodeId, port: InputPortIndex },

    #[error("node {0:?} has no variadic input group")]
    VariadicInputGroupNotFound(NodeId),

    #[error("variadic input port {port:?} on node {node:?} is still connected")]
    VariadicInputPortConnected { node: NodeId, port: InputPortIndex },
}

// ===========================================================================
// Port descriptors
// ===========================================================================

/// Descriptor for an input port on a node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputPort {
    pub name: String,
    pub accepted_types: Vec<DataTypeId>,
    /// An exposed parameter port (`Graph::expose_param_port`): named after
    /// the parameter it drives. The evaluator strips its input before
    /// `process` and overlays the converted value onto the resolved
    /// parameters (attribute > pin > parameter, REQ-LAYER-008).
    /// Additive field — `default` only, never `skip_serializing_if` (the
    /// bincode journal depends on a stable field layout; the layout change
    /// itself is covered by the journal format version bump).
    #[serde(default)]
    pub is_param: bool,
    /// A slot in the node's variadic input group. Variadic slots form one
    /// contiguous trailing region; the final slot is kept disconnected so
    /// editors can grow the group without shifting existing edge indices.
    /// Additive field — `default` only, never `skip_serializing_if` (the
    /// bincode journal depends on a stable field layout; the layout change
    /// itself is covered by the journal format version bump).
    #[serde(default)]
    pub is_variadic: bool,
}

/// Display name for the one-based `slot` in a variadic input group.
pub(crate) fn variadic_input_name(base_name: &str, slot: usize) -> String {
    if slot == 1 {
        base_name.to_string()
    } else {
        format!("{base_name}_{slot}")
    }
}

/// Descriptor for an output port on a node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputPort {
    pub name: String,
    pub data_type: DataTypeId,
}

// ===========================================================================
// Node
// ===========================================================================

/// Value of a node parameter.
///
/// Scalar static values are stored directly; animatable values are stored as
/// [`AnimationChannel`]s (per component for vectors/colors) so any parameter
/// can carry keyframes, expressions, node-output bindings, or blends
/// (REQ-LAYER-004). `Int` / `Bool` remain constant-only in v1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ParameterValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
    /// Animatable single-component value.
    Channel(crate::animation::channel::AnimationChannel),
    /// Animatable 2-component value (x, y).
    Channel2([crate::animation::channel::AnimationChannel; 2]),
    /// Animatable 3-component value (e.g. RGB).
    Channel3([crate::animation::channel::AnimationChannel; 3]),
    /// Animatable 4-component value (e.g. RGBA).
    Channel4([crate::animation::channel::AnimationChannel; 4]),
}

impl ParameterValue {
    /// Static float value, if this is a `Float`.
    pub fn as_float(&self) -> Option<f32> {
        match self {
            ParameterValue::Float(v) => Some(*v),
            _ => None,
        }
    }

    /// Static int value, if this is an `Int`.
    pub fn as_int(&self) -> Option<i32> {
        match self {
            ParameterValue::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// Static bool value, if this is a `Bool`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ParameterValue::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// Static string value, if this is a `String`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ParameterValue::String(v) => Some(v),
            _ => None,
        }
    }

    /// The wire type a parameter port for this value accepts, or `None`
    /// for types that cannot be exposed as a port in v1 (`String` has no
    /// driving node; `Channel3` has no 3-component wire type).
    pub fn port_data_type(&self) -> Option<DataTypeId> {
        match self {
            ParameterValue::Float(_)
            | ParameterValue::Int(_)
            | ParameterValue::Bool(_)
            | ParameterValue::Channel(_) => Some(DataTypeId::SCALAR),
            ParameterValue::Channel2(_) => Some(DataTypeId::VEC2),
            ParameterValue::Channel4(_) => Some(DataTypeId::COLOR),
            ParameterValue::String(_) | ParameterValue::Channel3(_) => None,
        }
    }
}

/// A user-facing parameter on a node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub key: String,
    pub value: ParameterValue,
}

/// Metadata attached to a node for the graph editor UI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeMetadata {
    pub label: Option<String>,
    pub color: Option<[f32; 4]>,
    pub position: (f32, f32),
    pub collapsed: bool,
    /// Nodes generated by the Composition compiler are marked synthetic.
    /// Excluded from persistence (.ravprj) and hidden in the node editor UI.
    #[serde(default)]
    pub synthetic: bool,
    /// Bypassed nodes pass their first type-matching input value through to
    /// each output port unchanged (no `process` call); see the bypass notes
    /// in `eval.rs`. Additive field — `default` only, never
    /// `skip_serializing_if` (the bincode journal depends on a stable field
    /// layout; the layout change itself is covered by the journal format
    /// version bump).
    #[serde(default)]
    pub bypassed: bool,
    /// Editor stacking order: nodes with a higher `z` paint (and hit-test)
    /// in front of lower ones. Values are assigned monotonically by the
    /// editor when a node is created or raised (grabbed for a drag); ties
    /// fall back to graph iteration order. Additive field — `default` only,
    /// never `skip_serializing_if` (see `bypassed`); covered by the journal
    /// format version bump to v4.
    #[serde(default)]
    pub z: u64,
}

impl Default for NodeMetadata {
    fn default() -> Self {
        Self {
            label: None,
            color: None,
            position: (0.0, 0.0),
            collapsed: false,
            synthetic: false,
            bypassed: false,
            z: 0,
        }
    }
}

/// A node in the DAG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    /// Registered node type key, e.g. `"blur"`, `"color_correct"`.
    pub type_key: String,
    pub inputs: Vec<InputPort>,
    pub outputs: Vec<OutputPort>,
    pub parameters: Vec<Parameter>,
    pub metadata: NodeMetadata,
    /// The inner graph of a subnet node (REQ-LAYER-003): the node owns its
    /// nested network, mirroring `Layer::network` ownership (REQ-LAYER-009).
    /// `Arc`-shared so cloning the node (immutable graph edits) stays cheap;
    /// editing the inner graph replaces the whole node via
    /// [`Graph::replace_node`]. `None` for every non-subnet node.
    // `skip_serializing_if` would desync bincode's field layout (the undo
    // journal); the None is always written.
    #[serde(default, with = "subnet_serde")]
    pub subnet: Option<Arc<Graph>>,
}

/// Serde adapter for `Option<Arc<Graph>>` (serde's `Arc` support needs the
/// `rc` feature; the graph is never shared across nodes on disk anyway).
mod subnet_serde {
    use super::Graph;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;

    pub fn serialize<S: Serializer>(
        value: &Option<Arc<Graph>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_deref().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Arc<Graph>>, D::Error> {
        Ok(Option::<Graph>::deserialize(deserializer)?.map(Arc::new))
    }
}

impl Node {
    pub fn new(id: NodeId, type_key: impl Into<String>) -> Self {
        Self {
            id,
            type_key: type_key.into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            parameters: Vec::new(),
            metadata: NodeMetadata::default(),
            subnet: None,
        }
    }

    /// Builder: add an input port.
    pub fn with_input(mut self, name: impl Into<String>, accepted: &[DataTypeId]) -> Self {
        self.inputs.push(InputPort {
            name: name.into(),
            accepted_types: accepted.to_vec(),
            is_param: false,
            is_variadic: false,
        });
        self
    }

    /// Builder: add an output port.
    pub fn with_output(mut self, name: impl Into<String>, data_type: DataTypeId) -> Self {
        self.outputs.push(OutputPort {
            name: name.into(),
            data_type,
        });
        self
    }

    /// Builder: set label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.metadata.label = Some(label.into());
        self
    }

    /// Builder: add a parameter.
    pub fn with_param(mut self, key: impl Into<String>, value: ParameterValue) -> Self {
        self.parameters.push(Parameter {
            key: key.into(),
            value,
        });
        self
    }

    /// Builder: set editor position.
    pub fn with_position(mut self, x: f32, y: f32) -> Self {
        self.metadata.position = (x, y);
        self
    }

    /// Builder: attach an inner graph, making this a subnet node
    /// (REQ-LAYER-003).
    pub fn with_subnet(mut self, graph: Graph) -> Self {
        self.subnet = Some(Arc::new(graph));
        self
    }

    /// Index of the exposed parameter port named `key`, if any.
    pub fn param_port_index(&self, key: &str) -> Option<InputPortIndex> {
        self.inputs
            .iter()
            .position(|p| p.is_param && p.name == key)
            .map(|i| InputPortIndex(i as u32))
    }

    /// Whether this node can expose parameters as input ports. Synthetic
    /// (Composition-compiler) nodes are regenerated on every compile,
    /// network-interface nodes (`net.in` / `net.out`) already have dynamic
    /// port semantics, and subnet promotion is the inverse mechanism —
    /// all are excluded in v1.
    pub fn supports_param_ports(&self) -> bool {
        !self.metadata.synthetic
            && self.subnet.is_none()
            && self.type_key != crate::network::NET_IN_TYPE_KEY
            && self.type_key != crate::network::NET_OUT_TYPE_KEY
            && self.type_key != "subnet"
    }

    /// Whether this node can be bypassed: every output port has at least
    /// one non-parameter input port that accepts its data type, so the
    /// evaluator can pass a connected input value through to each output
    /// unchanged (see the bypass notes in `eval.rs`). Pure generators — no
    /// input matching any output type — and multi-output nodes where only
    /// some outputs match cannot be bypassed: the evaluator would fall
    /// back to normal processing for them anyway.
    pub fn is_bypassable(&self) -> bool {
        !self.outputs.is_empty()
            && self.outputs.iter().all(|output| {
                self.inputs.iter().any(|input| {
                    !input.is_param
                        && (input.accepted_types.is_empty()
                            || input.accepted_types.contains(&output.data_type))
                })
            })
    }
}

// ===========================================================================
// Edge
// ===========================================================================

/// A directed edge connecting one output port to one input port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub id: EdgeId,
    pub source: NodeId,
    pub source_port: OutputPortIndex,
    pub target: NodeId,
    pub target_port: InputPortIndex,
}

// ===========================================================================
// Graph
// ===========================================================================

/// An immutable directed acyclic graph of nodes and edges.
///
/// All mutating methods consume `self` and return a new `Graph`, enabling
/// structural sharing via the `im` crate for zero-cost undo.
#[derive(Clone, Debug)]
pub struct Graph {
    nodes: im::HashMap<NodeId, Arc<Node>>,
    edges: im::HashMap<EdgeId, Edge>,
}

impl Graph {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self {
            nodes: im::HashMap::new(),
            edges: im::HashMap::new(),
        }
    }

    /// Deep-copy this graph hierarchy with globally fresh node and edge ids.
    ///
    /// The returned map contains every node in this graph and its nested
    /// subnet graphs. Edge endpoints and node-output parameter bindings are
    /// rewritten to the corresponding fresh node ids; references to nodes
    /// outside the copied hierarchy are left unchanged.
    pub fn duplicate_with_fresh_ids(&self) -> (Graph, HashMap<NodeId, NodeId>) {
        let mut id_map = HashMap::new();
        self.allocate_duplicate_node_ids(&mut id_map);
        (self.duplicate_with_id_map(&id_map), id_map)
    }

    fn allocate_duplicate_node_ids(&self, id_map: &mut HashMap<NodeId, NodeId>) {
        for node in self.nodes.values() {
            id_map.insert(node.id, NodeId::next());
            if let Some(subnet) = &node.subnet {
                subnet.allocate_duplicate_node_ids(id_map);
            }
        }
    }

    fn duplicate_with_id_map(&self, id_map: &HashMap<NodeId, NodeId>) -> Graph {
        let nodes = self
            .nodes
            .values()
            .map(|node| {
                let mut duplicate = (**node).clone();
                duplicate.id = id_map[&node.id];
                for parameter in &mut duplicate.parameters {
                    remap_parameter_node_outputs(&mut parameter.value, id_map);
                }
                duplicate.subnet = node
                    .subnet
                    .as_ref()
                    .map(|subnet| Arc::new(subnet.duplicate_with_id_map(id_map)));
                (duplicate.id, Arc::new(duplicate))
            })
            .collect();
        let edges = self
            .edges
            .values()
            .map(|edge| {
                let duplicate = Edge {
                    id: EdgeId::next(),
                    source: id_map[&edge.source],
                    source_port: edge.source_port,
                    target: id_map[&edge.target],
                    target_port: edge.target_port,
                };
                (duplicate.id, duplicate)
            })
            .collect();
        Graph { nodes, edges }
    }

    // ----- queries ---------------------------------------------------------

    /// Number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Look up a node by id.
    pub fn node(&self, id: NodeId) -> Option<&Arc<Node>> {
        self.nodes.get(&id)
    }

    /// Look up an edge by id.
    pub fn edge(&self, id: EdgeId) -> Option<&Edge> {
        self.edges.get(&id)
    }

    /// Iterate over all node ids.
    pub fn node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.keys().copied()
    }

    /// Iterate over all nodes (shared references).
    pub fn nodes(&self) -> impl Iterator<Item = &Arc<Node>> + '_ {
        self.nodes.values()
    }

    /// Iterate over all edges.
    pub fn edges(&self) -> impl Iterator<Item = &Edge> + '_ {
        self.edges.values()
    }

    /// Return the ids of nodes that feed **into** `node_id` (upstream
    /// neighbours).
    pub fn inputs_of(&self, node_id: NodeId) -> Vec<NodeId> {
        self.edges
            .values()
            .filter(|e| e.target == node_id)
            .map(|e| e.source)
            .collect()
    }

    /// Return the ids of nodes that `node_id` feeds **into** (downstream
    /// neighbours).
    pub fn outputs_of(&self, node_id: NodeId) -> Vec<NodeId> {
        self.edges
            .values()
            .filter(|e| e.source == node_id)
            .map(|e| e.target)
            .collect()
    }

    // ----- construction from parts -----------------------------------------

    /// Rebuild a graph from a flat list of nodes and edges.
    ///
    /// Intended for deserialization: nodes are inserted first, then every edge
    /// is validated through [`Graph::add_edge`], so a malformed (cyclic or
    /// dangling) edge set is rejected with a [`GraphError`] instead of producing
    /// an invalid graph. Insertion order does not affect the result.
    pub fn from_parts(
        nodes: impl IntoIterator<Item = Node>,
        edges: impl IntoIterator<Item = Edge>,
    ) -> Result<Self, GraphError> {
        let mut graph = Graph::new();
        for node in nodes {
            graph = graph.add_node(node)?;
        }
        for edge in edges {
            graph = graph.add_edge(
                edge.id,
                edge.source,
                edge.source_port,
                edge.target,
                edge.target_port,
            )?;
        }
        Ok(graph)
    }

    // ----- mutations (return new Graph) ------------------------------------

    /// Insert a node. Returns an error if a node with the same id already exists.
    pub fn add_node(mut self, node: Node) -> Result<Self, GraphError> {
        if self.nodes.contains_key(&node.id) {
            return Err(GraphError::DuplicateNode(node.id));
        }
        self.nodes.insert(node.id, Arc::new(node));
        Ok(self)
    }

    /// Replace a node in-place (same id, new data). Structural sharing means
    /// only the single entry is updated; all other nodes keep their `Arc`.
    ///
    /// Parameter ports whose backing parameter is gone from the replacement
    /// are pruned (with their edges, later ports re-indexed) so the
    /// document-level invariant "every `is_param` port has a same-named
    /// parameter" survives arbitrary replacements.
    pub fn replace_node(mut self, node: Arc<Node>) -> Self {
        let orphaned: Vec<String> = node
            .inputs
            .iter()
            .filter(|port| port.is_param && !node.parameters.iter().any(|p| p.key == port.name))
            .map(|port| port.name.clone())
            .collect();
        let id = node.id;
        self.nodes.insert(id, node);
        for key in orphaned {
            // The port was just observed on the inserted node; removal only
            // fails if a caller razes it concurrently, which the immutable
            // model precludes.
            self = self
                .remove_param_port(id, &key)
                .expect("orphaned param port exists on the just-inserted node");
        }
        self
    }

    /// Remove a node and all its connected edges.
    pub fn remove_node(mut self, id: NodeId) -> Result<Self, GraphError> {
        if !self.nodes.contains_key(&id) {
            return Err(GraphError::NodeNotFound(id));
        }
        self.nodes.remove(&id);
        // Remove edges touching this node.
        self.edges.retain(|_, e| e.source != id && e.target != id);
        Ok(self)
    }

    /// Add an edge. Returns `Err` if the edge would create a cycle or if
    /// either endpoint node does not exist.
    pub fn add_edge(
        mut self,
        id: EdgeId,
        source: NodeId,
        source_port: OutputPortIndex,
        target: NodeId,
        target_port: InputPortIndex,
    ) -> Result<Self, GraphError> {
        // Validate endpoints exist.
        if !self.nodes.contains_key(&source) {
            return Err(GraphError::NodeNotFound(source));
        }
        if !self.nodes.contains_key(&target) {
            return Err(GraphError::NodeNotFound(target));
        }

        // Check for duplicate.
        for e in self.edges.values() {
            if e.source == source
                && e.source_port == source_port
                && e.target == target
                && e.target_port == target_port
            {
                return Err(GraphError::DuplicateEdge {
                    from: source,
                    from_port: source_port,
                    to: target,
                    to_port: target_port,
                });
            }
        }

        // Cycle detection: would adding source→target introduce a cycle?
        // A cycle exists iff `target` can already reach `source`.
        if self.can_reach(target, source) {
            return Err(GraphError::CycleDetected {
                from: source,
                to: target,
            });
        }

        self.edges.insert(
            id,
            Edge {
                id,
                source,
                source_port,
                target,
                target_port,
            },
        );
        Ok(self)
    }

    /// Remove an edge by id.
    pub fn remove_edge(mut self, id: EdgeId) -> Result<Self, GraphError> {
        if self.edges.remove(&id).is_none() {
            return Err(GraphError::EdgeNotFound(id));
        }
        Ok(self)
    }

    /// Expose the parameter `key` on `node_id` as an input port
    /// (node-driven parameters). The port is appended to the node's inputs
    /// so existing edge indices stay valid, accepts the wire type derived
    /// from the parameter's value type, and is marked `is_param`.
    ///
    /// Errors when the node does not support parameter ports (synthetic /
    /// network-interface / subnet), the parameter does not exist, its type
    /// cannot be exposed, or an input port with that name already exists
    /// (exposed or built-in, e.g. the rasterize `color` pin).
    pub fn expose_param_port(mut self, node_id: NodeId, key: &str) -> Result<Self, GraphError> {
        let node = self
            .nodes
            .get(&node_id)
            .ok_or(GraphError::NodeNotFound(node_id))?;
        if !node.supports_param_ports() {
            return Err(GraphError::ParamPortsUnsupported(node_id));
        }
        let param = node
            .parameters
            .iter()
            .find(|p| p.key == key)
            .ok_or_else(|| GraphError::ParamNotFound {
                node: node_id,
                key: key.to_string(),
            })?;
        let data_type =
            param
                .value
                .port_data_type()
                .ok_or_else(|| GraphError::UnsupportedParamType {
                    node: node_id,
                    key: key.to_string(),
                })?;
        if node.inputs.iter().any(|p| p.name == key) {
            return Err(GraphError::ParamAlreadyExposed {
                node: node_id,
                key: key.to_string(),
            });
        }
        let mut updated = (**node).clone();
        updated.inputs.push(InputPort {
            name: key.to_string(),
            accepted_types: vec![data_type],
            is_param: true,
            is_variadic: false,
        });
        self.nodes.insert(node_id, Arc::new(updated));
        Ok(self)
    }

    /// Remove the exposed parameter port `key` from `node_id`, atomically:
    /// edges into the removed port are deleted and edges into later ports
    /// of the node have their `target_port` re-indexed to compensate for
    /// the shift. One call = one consistent graph state (the caller's
    /// Document commit is the undo unit).
    pub fn remove_param_port(mut self, node_id: NodeId, key: &str) -> Result<Self, GraphError> {
        let node = self
            .nodes
            .get(&node_id)
            .ok_or(GraphError::NodeNotFound(node_id))?;
        let index = node
            .inputs
            .iter()
            .position(|p| p.is_param && p.name == key)
            .ok_or_else(|| GraphError::ParamNotExposed {
                node: node_id,
                key: key.to_string(),
            })?;
        self.remove_input_port_and_reindex(node_id, index);
        Ok(self)
    }

    /// Insert the empty trailing slot for `node_id`'s variadic input group.
    /// The insertion occurs only when every current group slot is connected,
    /// so repeated calls preserve exactly one empty slot. Exposed parameter
    /// ports that follow the group are shifted atomically with their edges.
    pub fn grow_variadic_input_group(mut self, node_id: NodeId) -> Result<Self, GraphError> {
        let node = self
            .nodes
            .get(&node_id)
            .ok_or(GraphError::NodeNotFound(node_id))?;
        let Some((group_start, base_port)) = node
            .inputs
            .iter()
            .enumerate()
            .find(|(_, port)| port.is_variadic)
        else {
            return Err(GraphError::VariadicInputGroupNotFound(node_id));
        };
        let group_end = node.inputs[group_start..]
            .iter()
            .take_while(|port| port.is_variadic)
            .count()
            + group_start;
        let all_connected = (group_start..group_end).all(|index| {
            self.edges.values().any(|edge| {
                edge.target == node_id && edge.target_port == InputPortIndex(index as u32)
            })
        });
        if !all_connected {
            return Ok(self);
        }

        let mut appended = base_port.clone();
        appended.name = variadic_input_name(&base_port.name, group_end - group_start + 1);
        appended.is_param = false;
        appended.is_variadic = true;
        self.insert_input_port_and_reindex(node_id, group_end, appended);
        Ok(self)
    }

    /// Normalize a loaded node's variadic inputs into one contiguous group
    /// after its fixed inputs and before exposed parameter ports. Connected
    /// source slots retain their order, parameter ports retain their order,
    /// all affected edge indices are remapped, and one empty source slot is
    /// appended to the group.
    pub(crate) fn normalize_variadic_input_group(
        mut self,
        node_id: NodeId,
        fixed_input_count: usize,
        base_port: &InputPort,
    ) -> Result<Self, GraphError> {
        let node = self
            .nodes
            .get(&node_id)
            .ok_or(GraphError::NodeNotFound(node_id))?;
        if node.inputs.len() < fixed_input_count {
            return Ok(self);
        }

        let connected: std::collections::HashSet<_> = self
            .edges
            .values()
            .filter(|edge| edge.target == node_id)
            .map(|edge| edge.target_port.0 as usize)
            .collect();
        let mut remap = HashMap::new();
        let mut inputs = Vec::with_capacity(node.inputs.len() + 1);
        for (index, input) in node.inputs[..fixed_input_count].iter().cloned().enumerate() {
            remap.insert(index, inputs.len());
            inputs.push(input);
        }

        let connected_sources: Vec<_> = node.inputs[fixed_input_count..]
            .iter()
            .cloned()
            .enumerate()
            .filter_map(|(offset, input)| {
                let old_index = fixed_input_count + offset;
                (!input.is_param && connected.contains(&old_index)).then_some((old_index, input))
            })
            .collect();
        for (slot, (old_index, mut input)) in connected_sources.into_iter().enumerate() {
            input.name = variadic_input_name(&base_port.name, slot + 1);
            input.is_param = false;
            input.is_variadic = true;
            remap.insert(old_index, inputs.len());
            inputs.push(input);
        }
        let mut empty = base_port.clone();
        empty.name = variadic_input_name(&base_port.name, inputs.len() - fixed_input_count + 1);
        empty.is_param = false;
        empty.is_variadic = true;
        inputs.push(empty);

        for (offset, input) in node.inputs[fixed_input_count..].iter().cloned().enumerate() {
            if input.is_param {
                remap.insert(fixed_input_count + offset, inputs.len());
                inputs.push(input);
            }
        }

        let mut updated = (**node).clone();
        updated.inputs = inputs;
        self.nodes.insert(node_id, Arc::new(updated));
        let shifts: Vec<_> = self
            .edges
            .values()
            .filter(|edge| edge.target == node_id)
            .filter_map(|edge| {
                let mut shifted = edge.clone();
                shifted.target_port =
                    InputPortIndex(*remap.get(&(edge.target_port.0 as usize))? as u32);
                Some(shifted)
            })
            .collect();
        for edge in shifts {
            self.edges.insert(edge.id, edge);
        }
        Ok(self)
    }

    /// Remove a disconnected slot from `node_id`'s variadic input group.
    /// Edges targeting later ports are shifted down by one and remaining
    /// group slots are renamed from their base name. The sole empty group
    /// slot is retained instead of being removed.
    pub fn compact_variadic_input_group(
        mut self,
        node_id: NodeId,
        port: InputPortIndex,
    ) -> Result<Self, GraphError> {
        let node = self
            .nodes
            .get(&node_id)
            .ok_or(GraphError::NodeNotFound(node_id))?;
        let index = port.0 as usize;
        if !node
            .inputs
            .get(index)
            .is_some_and(|input| input.is_variadic)
        {
            return Err(GraphError::VariadicInputPortNotFound {
                node: node_id,
                port,
            });
        }
        if self
            .edges
            .values()
            .any(|edge| edge.target == node_id && edge.target_port == port)
        {
            return Err(GraphError::VariadicInputPortConnected {
                node: node_id,
                port,
            });
        }
        let group_start = node
            .inputs
            .iter()
            .position(|input| input.is_variadic)
            .expect("checked variadic input above");
        let group_end = node.inputs[group_start..]
            .iter()
            .take_while(|input| input.is_variadic)
            .count()
            + group_start;
        if group_end - group_start == 1 || index == group_end - 1 {
            return Ok(self);
        }
        let base_name = node.inputs[group_start].name.clone();
        self.remove_input_port_and_reindex(node_id, index);
        let node = self
            .nodes
            .get(&node_id)
            .expect("node retained while compacting variadic inputs");
        let mut updated = (**node).clone();
        for (slot, input) in updated.inputs[group_start..group_end - 1]
            .iter_mut()
            .enumerate()
        {
            input.name = variadic_input_name(&base_name, slot + 1);
        }
        self.nodes.insert(node_id, Arc::new(updated));
        Ok(self)
    }

    fn remove_input_port_and_reindex(&mut self, node_id: NodeId, index: usize) {
        let node = self
            .nodes
            .get(&node_id)
            .expect("input-port removal requires an existing node");
        let mut updated = (**node).clone();
        updated.inputs.remove(index);
        self.nodes.insert(node_id, Arc::new(updated));

        let removed_port = InputPortIndex(index as u32);
        let mut removals: Vec<EdgeId> = Vec::new();
        let mut shifts: Vec<Edge> = Vec::new();
        for edge in self.edges.values() {
            if edge.target != node_id {
                continue;
            }
            if edge.target_port == removed_port {
                removals.push(edge.id);
            } else if edge.target_port.0 > removed_port.0 {
                let mut shifted = edge.clone();
                shifted.target_port = InputPortIndex(edge.target_port.0 - 1);
                shifts.push(shifted);
            }
        }
        for id in removals {
            self.edges.remove(&id);
        }
        for edge in shifts {
            self.edges.insert(edge.id, edge);
        }
    }

    fn insert_input_port_and_reindex(&mut self, node_id: NodeId, index: usize, port: InputPort) {
        let node = self
            .nodes
            .get(&node_id)
            .expect("input-port insertion requires an existing node");
        let mut updated = (**node).clone();
        updated.inputs.insert(index, port);
        self.nodes.insert(node_id, Arc::new(updated));

        let shifts: Vec<_> = self
            .edges
            .values()
            .filter_map(|edge| {
                if edge.target == node_id && edge.target_port.0 >= index as u32 {
                    let mut shifted = edge.clone();
                    shifted.target_port = InputPortIndex(edge.target_port.0 + 1);
                    Some(shifted)
                } else {
                    None
                }
            })
            .collect();
        for edge in shifts {
            self.edges.insert(edge.id, edge);
        }
    }

    // ----- algorithms ------------------------------------------------------

    /// Build a forward adjacency list: source → [targets].
    fn adjacency_list(&self) -> std::collections::HashMap<NodeId, Vec<NodeId>> {
        let mut adj: std::collections::HashMap<NodeId, Vec<NodeId>> =
            std::collections::HashMap::new();
        for e in self.edges.values() {
            adj.entry(e.source).or_default().push(e.target);
        }
        adj
    }

    /// Test whether `from` can reach `to` via directed edges (BFS). O(V+E).
    fn can_reach(&self, from: NodeId, to: NodeId) -> bool {
        if from == to {
            return true;
        }
        let adj = self.adjacency_list();
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(from);
        while let Some(current) = queue.pop_front() {
            if let Some(neighbors) = adj.get(&current) {
                for &next in neighbors {
                    if next == to {
                        return true;
                    }
                    if visited.insert(next) {
                        queue.push_back(next);
                    }
                }
            }
        }
        false
    }

    /// Kahn's algorithm for topological sort. O(V+E).
    ///
    /// Returns nodes in evaluation order (sources first, sinks last).
    /// Returns `Err` if the graph contains a cycle (should be impossible if
    /// edges are only added through [`add_edge`], which rejects cycles).
    pub fn topological_sort(&self) -> Result<Vec<NodeId>, GraphError> {
        let adj = self.adjacency_list();

        let mut in_degree: std::collections::HashMap<NodeId, usize> =
            self.nodes.keys().map(|&id| (id, 0)).collect();
        for e in self.edges.values() {
            *in_degree.entry(e.target).or_default() += 1;
        }

        let mut queue: std::collections::BinaryHeap<std::cmp::Reverse<NodeId>> = in_degree
            .iter()
            .filter(|entry| *entry.1 == 0)
            .map(|entry| std::cmp::Reverse(*entry.0))
            .collect();

        let mut order = Vec::with_capacity(self.nodes.len());

        while let Some(std::cmp::Reverse(current)) = queue.pop() {
            order.push(current);
            if let Some(neighbors) = adj.get(&current) {
                for &next in neighbors {
                    if let Some(deg) = in_degree.get_mut(&next) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push(std::cmp::Reverse(next));
                        }
                    }
                }
            }
        }

        if order.len() == self.nodes.len() {
            Ok(order)
        } else {
            let visited: std::collections::HashSet<_> = order.iter().copied().collect();
            let remaining: Vec<_> = self
                .nodes
                .keys()
                .filter(|id| !visited.contains(id))
                .copied()
                .collect();
            Err(GraphError::CycleDetected {
                from: remaining[0],
                to: remaining.get(1).copied().unwrap_or(remaining[0]),
            })
        }
    }
}

fn remap_parameter_node_outputs(value: &mut ParameterValue, id_map: &HashMap<NodeId, NodeId>) {
    match value {
        ParameterValue::Channel(channel) => remap_channel_source(&mut channel.source, id_map),
        ParameterValue::Channel2(channels) => {
            for channel in channels {
                remap_channel_source(&mut channel.source, id_map);
            }
        }
        ParameterValue::Channel3(channels) => {
            for channel in channels {
                remap_channel_source(&mut channel.source, id_map);
            }
        }
        ParameterValue::Channel4(channels) => {
            for channel in channels {
                remap_channel_source(&mut channel.source, id_map);
            }
        }
        ParameterValue::Float(_)
        | ParameterValue::Int(_)
        | ParameterValue::Bool(_)
        | ParameterValue::String(_) => {}
    }
}

fn remap_channel_source(
    source: &mut crate::animation::channel::ChannelSource,
    id_map: &HashMap<NodeId, NodeId>,
) {
    use crate::animation::channel::ChannelSource;
    match source {
        ChannelSource::NodeOutput(node, _) => {
            if let Some(duplicate) = id_map.get(node) {
                *node = *duplicate;
            }
        }
        ChannelSource::Blend(a, b, _, _) => {
            remap_channel_source(a, id_map);
            remap_channel_source(b, id_map);
        }
        ChannelSource::Constant(_)
        | ChannelSource::Keyframes(_)
        | ChannelSource::Expression(_)
        | ChannelSource::AudioReactive(_) => {}
    }
}

/// Serialized shape of a [`Graph`]: id-sorted node/edge lists, matching the
/// diff-friendly on-disk projection. Deserialization re-validates through
/// [`Graph::from_parts`], so malformed subnet graphs are rejected.
#[derive(Serialize, Deserialize)]
struct GraphParts {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Serialize for Graph {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut nodes: Vec<Node> = self.nodes.values().map(|n| (**n).clone()).collect();
        nodes.sort_by_key(|n| n.id);
        let mut edges: Vec<Edge> = self.edges.values().cloned().collect();
        edges.sort_by_key(|e| e.id);
        GraphParts { nodes, edges }.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Graph {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let parts = GraphParts::deserialize(deserializer)?;
        Graph::from_parts(parts.nodes, parts.edges).map_err(serde::de::Error::custom)
    }
}

impl PartialEq for Graph {
    fn eq(&self, other: &Self) -> bool {
        self.nodes.len() == other.nodes.len()
            && self.edges.len() == other.edges.len()
            && self.nodes.iter().all(|(k, v)| {
                other
                    .nodes
                    .get(k)
                    .is_some_and(|ov| v.as_ref() == ov.as_ref())
            })
            && self.edges == other.edges
    }
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl Graph {
    /// Test-only: insert an edge **without** cycle validation.
    ///
    /// The public [`Graph::add_edge`] rejects cycles, so cyclic graphs cannot
    /// be built through the normal API. This escape hatch lets evaluator
    /// robustness tests construct pathological (cyclic) graphs to verify that
    /// evaluation fails gracefully instead of panicking or looping forever.
    pub(crate) fn add_edge_unchecked(
        mut self,
        id: EdgeId,
        source: NodeId,
        source_port: OutputPortIndex,
        target: NodeId,
        target_port: InputPortIndex,
    ) -> Self {
        self.edges.insert(
            id,
            Edge {
                id,
                source,
                source_port,
                target,
                target_port,
            },
        );
        self
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::channel::{AnimationChannel, ChannelSource};
    use crate::animation::curve::KeyframeCurve;
    use crate::animation::interpolation::Interpolation;
    use crate::id::{DataTypeId, InputPortIndex, OutputPortIndex};

    fn make_node(id: u64) -> Node {
        Node::new(NodeId::new(id), "test")
            .with_output("out", DataTypeId::FRAME_BUFFER)
            .with_input("in", &[DataTypeId::FRAME_BUFFER])
    }

    // ---- basic operations -------------------------------------------------

    #[test]
    fn empty_graph() {
        let g = Graph::new();
        assert_eq!(g.node_count(), 0);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn add_and_lookup_node() {
        let n = make_node(1);
        let g = Graph::new().add_node(n).unwrap();
        assert_eq!(g.node_count(), 1);
        let node = g.node(NodeId::new(1)).expect("node must exist");
        assert_eq!(node.type_key, "test");
    }

    #[test]
    fn duplicate_with_fresh_ids_remaps_graph_and_parameter_bindings() {
        let source = NodeId::next();
        let target = NodeId::next();
        let edge_id = EdgeId::next();
        let mut curve = KeyframeCurve::with_default(3.0);
        curve.insert(4, 7.0, Interpolation::Bezier);
        let source_node = Node::new(source, "source")
            .with_output("value", DataTypeId::SCALAR)
            .with_position(12.0, 34.0)
            .with_label("Source");
        let mut target_node = Node::new(target, "target")
            .with_input("value", &[DataTypeId::SCALAR])
            .with_param(
                "driven",
                ParameterValue::Channel(AnimationChannel::new(ChannelSource::NodeOutput(
                    source,
                    OutputPortIndex(0),
                ))),
            )
            .with_param(
                "animated",
                ParameterValue::Channel(AnimationChannel::keyframes(curve.clone())),
            );
        target_node.metadata.bypassed = true;
        let graph = Graph::new()
            .add_node(source_node)
            .unwrap()
            .add_node(target_node)
            .unwrap()
            .add_edge(
                edge_id,
                source,
                OutputPortIndex(0),
                target,
                InputPortIndex(0),
            )
            .unwrap();

        let (duplicate, id_map) = graph.duplicate_with_fresh_ids();
        let duplicate_source = id_map[&source];
        let duplicate_target = id_map[&target];
        assert_eq!(duplicate.node_count(), 2);
        assert_eq!(duplicate.edge_count(), 1);
        assert!(!graph.node_ids().any(|id| duplicate.node(id).is_some()));
        let duplicate_edge = duplicate.edges().next().unwrap();
        assert_ne!(duplicate_edge.id, edge_id);
        assert_eq!(duplicate_edge.source, duplicate_source);
        assert_eq!(duplicate_edge.target, duplicate_target);

        let source_copy = duplicate.node(duplicate_source).unwrap();
        assert_eq!(source_copy.metadata.position, (12.0, 34.0));
        assert_eq!(source_copy.metadata.label.as_deref(), Some("Source"));
        assert_eq!(source_copy.outputs, graph.node(source).unwrap().outputs);
        let target_copy = duplicate.node(duplicate_target).unwrap();
        assert!(target_copy.metadata.bypassed);
        assert!(matches!(
            &target_copy.parameters[0].value,
            ParameterValue::Channel(AnimationChannel {
                source: ChannelSource::NodeOutput(node, OutputPortIndex(0))
            }) if *node == duplicate_source
        ));
        assert_eq!(
            target_copy.parameters[1].value,
            ParameterValue::Channel(AnimationChannel::keyframes(curve))
        );
    }

    #[test]
    fn duplicate_with_fresh_ids_recurses_into_subnets() {
        let inner_id = NodeId::next();
        let outer_id = NodeId::next();
        let inner = Graph::new()
            .add_node(Node::new(inner_id, "constant"))
            .unwrap();
        let graph = Graph::new()
            .add_node(Node::new(outer_id, "subnet").with_subnet(inner))
            .unwrap();

        let (duplicate, id_map) = graph.duplicate_with_fresh_ids();
        let outer_copy = duplicate.node(id_map[&outer_id]).unwrap();
        let inner_copy = outer_copy.subnet.as_ref().unwrap();
        assert!(inner_copy.node(id_map[&inner_id]).is_some());
        assert!(inner_copy.node(inner_id).is_none());
    }

    #[test]
    fn remove_node_removes_connected_edges() {
        let g = Graph::new()
            .add_node(make_node(1))
            .unwrap()
            .add_node(make_node(2))
            .unwrap();
        let g = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        assert_eq!(g.edge_count(), 1);
        let g = g.remove_node(NodeId::new(1)).unwrap();
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn remove_nonexistent_node_errors() {
        let g = Graph::new();
        assert!(g.remove_node(NodeId::new(99)).is_err());
    }

    // ---- edge operations --------------------------------------------------

    #[test]
    fn add_edge_simple() {
        let g = Graph::new()
            .add_node(make_node(1))
            .unwrap()
            .add_node(make_node(2))
            .unwrap();
        let g = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        assert_eq!(g.edge_count(), 1);
        assert_eq!(g.inputs_of(NodeId::new(2)), vec![NodeId::new(1)]);
        assert_eq!(g.outputs_of(NodeId::new(1)), vec![NodeId::new(2)]);
    }

    #[test]
    fn add_edge_rejects_cycle() {
        let g = Graph::new()
            .add_node(make_node(1))
            .unwrap()
            .add_node(make_node(2))
            .unwrap();
        let g = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        // 2→1 would form a cycle
        let err = g
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(1),
                InputPortIndex(0),
            )
            .unwrap_err();
        assert!(matches!(err, GraphError::CycleDetected { .. }));
    }

    #[test]
    fn add_edge_rejects_self_loop() {
        let g = Graph::new().add_node(make_node(1)).unwrap();
        let err = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(1),
                InputPortIndex(0),
            )
            .unwrap_err();
        assert!(matches!(err, GraphError::CycleDetected { .. }));
    }

    #[test]
    fn add_edge_rejects_duplicate() {
        let g = Graph::new()
            .add_node(make_node(1))
            .unwrap()
            .add_node(make_node(2))
            .unwrap();
        let g = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        let err = g
            .add_edge(
                EdgeId::new(2),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap_err();
        assert!(matches!(err, GraphError::DuplicateEdge { .. }));
    }

    #[test]
    fn add_edge_rejects_missing_node() {
        let g = Graph::new().add_node(make_node(1)).unwrap();
        let err = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(99),
                InputPortIndex(0),
            )
            .unwrap_err();
        assert!(matches!(err, GraphError::NodeNotFound(_)));
    }

    #[test]
    fn remove_edge() {
        let g = Graph::new()
            .add_node(make_node(1))
            .unwrap()
            .add_node(make_node(2))
            .unwrap();
        let g = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        let g = g.remove_edge(EdgeId::new(1)).unwrap();
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn remove_nonexistent_edge_errors() {
        let g = Graph::new();
        assert!(g.remove_edge(EdgeId::new(99)).is_err());
    }

    // ---- topological sort -------------------------------------------------

    #[test]
    fn topo_sort_empty_graph() {
        let g = Graph::new();
        let order = g.topological_sort().unwrap();
        assert!(order.is_empty());
    }

    #[test]
    fn topo_sort_single_node() {
        let g = Graph::new().add_node(make_node(1)).unwrap();
        let order = g.topological_sort().unwrap();
        assert_eq!(order, vec![NodeId::new(1)]);
    }

    #[test]
    fn topo_sort_linear_chain() {
        // 1 → 2 → 3
        let g = Graph::new()
            .add_node(make_node(1))
            .unwrap()
            .add_node(make_node(2))
            .unwrap()
            .add_node(make_node(3))
            .unwrap();
        let g = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap();

        let order = g.topological_sort().unwrap();
        assert_eq!(order, vec![NodeId::new(1), NodeId::new(2), NodeId::new(3)]);
    }

    #[test]
    fn topo_sort_diamond() {
        // 1 → 2
        // 1 → 3
        // 2 → 4
        // 3 → 4
        let g = Graph::new()
            .add_node(make_node(1))
            .unwrap()
            .add_node(make_node(2))
            .unwrap()
            .add_node(make_node(3))
            .unwrap()
            .add_node(make_node(4))
            .unwrap();
        let g = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(3),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(4),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(4),
                NodeId::new(3),
                OutputPortIndex(0),
                NodeId::new(4),
                InputPortIndex(1),
            )
            .unwrap();

        let order = g.topological_sort().unwrap();
        assert_eq!(order.len(), 4);
        // 1 must come first, 4 must come last.
        assert_eq!(order[0], NodeId::new(1));
        assert_eq!(order[3], NodeId::new(4));
        // 2 and 3 are between.
        assert!(order[1] == NodeId::new(2) || order[1] == NodeId::new(3));
    }

    #[test]
    fn topo_sort_disconnected_components() {
        // Two disconnected chains: 1→2, 3→4
        let g = Graph::new()
            .add_node(make_node(1))
            .unwrap()
            .add_node(make_node(2))
            .unwrap()
            .add_node(make_node(3))
            .unwrap()
            .add_node(make_node(4))
            .unwrap();
        let g = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(3),
                OutputPortIndex(0),
                NodeId::new(4),
                InputPortIndex(0),
            )
            .unwrap();

        let order = g.topological_sort().unwrap();
        assert_eq!(order.len(), 4);
        // 1 before 2, 3 before 4
        let pos = |id: u64| order.iter().position(|n| *n == NodeId::new(id)).unwrap();
        assert!(pos(1) < pos(2));
        assert!(pos(3) < pos(4));
    }

    // ---- structural sharing (im crate) ------------------------------------

    #[test]
    fn graph_clone_shares_structure() {
        let g1 = Graph::new().add_node(make_node(1)).unwrap();
        let g2 = g1.clone().add_node(make_node(2)).unwrap();

        // g1 still has 1 node, g2 has 2.
        assert_eq!(g1.node_count(), 1);
        assert_eq!(g2.node_count(), 2);

        // The Arc<Node> for node 1 is shared (same pointer).
        let n1_from_g1 = g1.node(NodeId::new(1)).unwrap();
        let n1_from_g2 = g2.node(NodeId::new(1)).unwrap();
        assert!(Arc::ptr_eq(n1_from_g1, n1_from_g2));
    }

    // ---- cycle rejection in a longer chain --------------------------------

    #[test]
    fn add_edge_rejects_transitive_cycle() {
        // 1 → 2 → 3, then try 3 → 1
        let g = Graph::new()
            .add_node(make_node(1))
            .unwrap()
            .add_node(make_node(2))
            .unwrap()
            .add_node(make_node(3))
            .unwrap();
        let g = g
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap();

        let err = g
            .add_edge(
                EdgeId::new(3),
                NodeId::new(3),
                OutputPortIndex(0),
                NodeId::new(1),
                InputPortIndex(0),
            )
            .unwrap_err();
        assert!(matches!(err, GraphError::CycleDetected { .. }));
    }

    // ---- node_ids iteration -----------------------------------------------

    #[test]
    fn node_ids_returns_all() {
        let g = Graph::new()
            .add_node(make_node(10))
            .unwrap()
            .add_node(make_node(20))
            .unwrap()
            .add_node(make_node(30))
            .unwrap();
        let mut ids: Vec<_> = g.node_ids().collect();
        ids.sort();
        assert_eq!(ids, vec![NodeId::new(10), NodeId::new(20), NodeId::new(30)]);
    }

    // ---- serde and subnets --------------------------------------------------

    #[test]
    fn graph_with_subnet_roundtrips_through_ron() {
        let inner = Graph::new()
            .add_node(make_node(100))
            .unwrap()
            .add_node(make_node(101))
            .unwrap()
            .add_edge(
                EdgeId::new(1000),
                NodeId::new(100),
                OutputPortIndex(0),
                NodeId::new(101),
                InputPortIndex(0),
            )
            .unwrap();
        let subnet_node = Node::new(NodeId::new(1), "subnet")
            .with_input("in", &[DataTypeId::SCALAR])
            .with_output("out", DataTypeId::SCALAR)
            .with_subnet(inner);
        let g = Graph::new()
            .add_node(subnet_node)
            .unwrap()
            .add_node(make_node(2))
            .unwrap();

        let text = ron::to_string(&g).unwrap();
        let restored: Graph = ron::from_str(&text).unwrap();
        assert_eq!(g, restored);
        let inner = restored.node(NodeId::new(1)).unwrap().subnet.as_deref();
        assert_eq!(inner.map(|g| g.node_count()), Some(2));
    }

    #[test]
    fn non_subnet_nodes_roundtrip_with_empty_subnet_field() {
        let g = Graph::new().add_node(make_node(1)).unwrap();
        let text = ron::to_string(&g).unwrap();
        let restored: Graph = ron::from_str(&text).unwrap();
        assert_eq!(g, restored);
        assert!(restored.node(NodeId::new(1)).unwrap().subnet.is_none());
    }

    #[test]
    fn malformed_subnet_edges_are_rejected_on_deserialize() {
        // Serialize a valid graph, then corrupt an edge target id.
        let g = Graph::new()
            .add_node(make_node(1))
            .unwrap()
            .add_node(make_node(2))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        let text = ron::to_string(&g).unwrap();
        let corrupted = text.replace("target:(2)", "target:(99)");
        assert_ne!(text, corrupted, "corruption must apply");
        assert!(ron::from_str::<Graph>(&corrupted).is_err());
    }

    // ---- parameter ports ----------------------------------------------------

    /// A node with data inputs plus float / color / string parameters.
    fn param_node(id: u64) -> Node {
        use crate::animation::channel::AnimationChannel;
        Node::new(NodeId::new(id), "test")
            .with_input("in_a", &[DataTypeId::FRAME_BUFFER])
            .with_input("in_b", &[DataTypeId::FRAME_BUFFER])
            .with_output("out", DataTypeId::FRAME_BUFFER)
            .with_param("radius", ParameterValue::Float(5.0))
            .with_param(
                "tint",
                ParameterValue::Channel4([
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                ]),
            )
            .with_param("label", ParameterValue::String("x".into()))
    }

    #[test]
    fn expose_param_port_appends_a_typed_port() {
        let g = Graph::new()
            .add_node(param_node(1))
            .unwrap()
            .expose_param_port(NodeId::new(1), "radius")
            .unwrap()
            .expose_param_port(NodeId::new(1), "tint")
            .unwrap();
        let node = g.node(NodeId::new(1)).unwrap();
        assert_eq!(node.inputs.len(), 4, "appended after data ports");
        assert_eq!(node.inputs[2].name, "radius");
        assert!(node.inputs[2].is_param);
        assert_eq!(node.inputs[2].accepted_types, vec![DataTypeId::SCALAR]);
        assert_eq!(node.inputs[3].accepted_types, vec![DataTypeId::COLOR]);
        assert_eq!(
            node.param_port_index("radius"),
            Some(InputPortIndex(2)),
            "helper resolves the port"
        );
    }

    #[test]
    fn expose_param_port_rejects_invalid_requests() {
        let g = Graph::new().add_node(param_node(1)).unwrap();
        // Unknown parameter.
        assert!(matches!(
            g.clone().expose_param_port(NodeId::new(1), "nope"),
            Err(GraphError::ParamNotFound { .. })
        ));
        // String parameters have no wire type.
        assert!(matches!(
            g.clone().expose_param_port(NodeId::new(1), "label"),
            Err(GraphError::UnsupportedParamType { .. })
        ));
        // Double exposure.
        let exposed = g
            .clone()
            .expose_param_port(NodeId::new(1), "radius")
            .unwrap();
        assert!(matches!(
            exposed.expose_param_port(NodeId::new(1), "radius"),
            Err(GraphError::ParamAlreadyExposed { .. })
        ));
        // A name collision with a built-in input port is also rejected.
        let g2 = Graph::new()
            .add_node(
                Node::new(NodeId::new(2), "test")
                    .with_input("radius", &[DataTypeId::SCALAR])
                    .with_param("radius", ParameterValue::Float(1.0)),
            )
            .unwrap();
        assert!(matches!(
            g2.expose_param_port(NodeId::new(2), "radius"),
            Err(GraphError::ParamAlreadyExposed { .. })
        ));
        // Synthetic and network-interface nodes are excluded.
        let mut synthetic = param_node(3);
        synthetic.metadata.synthetic = true;
        let g3 = Graph::new().add_node(synthetic).unwrap();
        assert!(matches!(
            g3.expose_param_port(NodeId::new(3), "radius"),
            Err(GraphError::ParamPortsUnsupported(_))
        ));
        let net_in = Node::new(NodeId::new(4), crate::network::NET_IN_TYPE_KEY)
            .with_param("radius", ParameterValue::Float(1.0));
        let g4 = Graph::new().add_node(net_in).unwrap();
        assert!(matches!(
            g4.expose_param_port(NodeId::new(4), "radius"),
            Err(GraphError::ParamPortsUnsupported(_))
        ));
    }

    #[test]
    fn remove_param_port_drops_edges_and_reindexes() {
        // node 1 (constant-like sources) → node 2 with two exposed params.
        let source = Node::new(NodeId::new(1), "test")
            .with_output("a", DataTypeId::SCALAR)
            .with_output("b", DataTypeId::COLOR);
        let g = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(param_node(2))
            .unwrap()
            .expose_param_port(NodeId::new(2), "radius")
            .unwrap()
            .expose_param_port(NodeId::new(2), "tint")
            .unwrap()
            // data edge into in_b (index 1) stays untouched throughout.
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(1),
            )
            .unwrap()
            // radius port (index 2) and tint port (index 3).
            .add_edge(
                EdgeId::new(2),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(2),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(3),
                NodeId::new(1),
                OutputPortIndex(1),
                NodeId::new(2),
                InputPortIndex(3),
            )
            .unwrap();

        let g = g.remove_param_port(NodeId::new(2), "radius").unwrap();
        let node = g.node(NodeId::new(2)).unwrap();
        assert_eq!(node.inputs.len(), 3);
        assert_eq!(node.param_port_index("tint"), Some(InputPortIndex(2)));
        // The radius edge is gone; the tint edge re-indexed 3 → 2; the data
        // edge is untouched.
        assert!(g.edge(EdgeId::new(2)).is_none(), "radius edge removed");
        assert_eq!(
            g.edge(EdgeId::new(3)).unwrap().target_port,
            InputPortIndex(2),
            "tint edge re-indexed"
        );
        assert_eq!(
            g.edge(EdgeId::new(1)).unwrap().target_port,
            InputPortIndex(1),
            "data edge untouched"
        );

        // Removing a port that is not exposed errors.
        assert!(matches!(
            g.remove_param_port(NodeId::new(2), "radius"),
            Err(GraphError::ParamNotExposed { .. })
        ));
    }

    #[test]
    fn grow_variadic_input_group_appends_an_index_stable_empty_slot() {
        let source = Node::new(NodeId::new(1), "source").with_output("out", DataTypeId::GEOMETRY);
        let mut target =
            Node::new(NodeId::new(2), "variadic").with_input("fixed", &[DataTypeId::GEOMETRY]);
        target.inputs.push(InputPort {
            name: "source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
            is_variadic: true,
        });
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(target)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(1),
            )
            .unwrap()
            .grow_variadic_input_group(NodeId::new(2))
            .unwrap();

        let node = graph.node(NodeId::new(2)).unwrap();
        assert_eq!(node.inputs.len(), 3);
        assert_eq!(node.inputs[2].name, "source_2");
        assert!(node.inputs[2].is_variadic);
        assert_eq!(
            graph.edge(EdgeId::new(1)).unwrap().target_port,
            InputPortIndex(1),
            "append keeps the connected slot index stable"
        );

        let unchanged = graph
            .clone()
            .grow_variadic_input_group(NodeId::new(2))
            .unwrap();
        assert_eq!(unchanged.node(NodeId::new(2)).unwrap().inputs.len(), 3);

        let trailing_retained = graph
            .clone()
            .compact_variadic_input_group(NodeId::new(2), InputPortIndex(2))
            .unwrap();
        assert_eq!(
            trailing_retained.node(NodeId::new(2)).unwrap().inputs.len(),
            3,
            "compaction retains the empty trailing slot"
        );
    }

    #[test]
    fn compact_variadic_input_group_reindexes_edges_and_renumbers_names() {
        let source = Node::new(NodeId::new(1), "source")
            .with_output("a", DataTypeId::GEOMETRY)
            .with_output("b", DataTypeId::GEOMETRY);
        let mut target =
            Node::new(NodeId::new(2), "variadic").with_input("fixed", &[DataTypeId::GEOMETRY]);
        for name in ["source", "source_2", "source_3"] {
            target.inputs.push(InputPort {
                name: name.into(),
                accepted_types: vec![DataTypeId::GEOMETRY],
                is_param: false,
                is_variadic: true,
            });
        }
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(target)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(1),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(1),
                OutputPortIndex(1),
                NodeId::new(2),
                InputPortIndex(2),
            )
            .unwrap()
            .remove_edge(EdgeId::new(1))
            .unwrap()
            .compact_variadic_input_group(NodeId::new(2), InputPortIndex(1))
            .unwrap();

        let node = graph.node(NodeId::new(2)).unwrap();
        assert_eq!(node.inputs.len(), 3);
        assert_eq!(node.inputs[0].name, "fixed");
        assert_eq!(node.inputs[1].name, "source");
        assert_eq!(node.inputs[2].name, "source_2");
        assert!(node.inputs[1..].iter().all(|port| port.is_variadic));
        assert_eq!(
            graph.edge(EdgeId::new(2)).unwrap().target_port,
            InputPortIndex(1),
            "edge into the later group slot shifts down"
        );
        assert!(
            !graph.edges().any(|edge| {
                edge.target == NodeId::new(2) && edge.target_port == InputPortIndex(2)
            }),
            "one disconnected trailing slot remains"
        );
    }

    #[test]
    fn variadic_growth_and_compaction_reindex_following_param_ports() {
        let source = Node::new(NodeId::new(1), "source")
            .with_output("geometry", DataTypeId::GEOMETRY)
            .with_output("scalar", DataTypeId::SCALAR);
        let mut target =
            Node::new(NodeId::new(2), "variadic").with_param("count", ParameterValue::Int(1));
        target.inputs.push(InputPort {
            name: "source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
            is_variadic: true,
        });
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(target)
            .unwrap()
            .expose_param_port(NodeId::new(2), "count")
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(1),
                NodeId::new(2),
                InputPortIndex(1),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap()
            .grow_variadic_input_group(NodeId::new(2))
            .unwrap();

        let node = graph.node(NodeId::new(2)).unwrap();
        assert_eq!(node.inputs.len(), 3);
        assert_eq!(node.inputs[0].name, "source");
        assert_eq!(node.inputs[1].name, "source_2");
        assert_eq!(node.inputs[2].name, "count");
        assert!(node.inputs[0..2].iter().all(|port| port.is_variadic));
        assert!(node.inputs[2].is_param);
        assert_eq!(
            graph.edge(EdgeId::new(1)).unwrap().target_port,
            InputPortIndex(2)
        );

        let compacted = graph
            .remove_edge(EdgeId::new(2))
            .unwrap()
            .compact_variadic_input_group(NodeId::new(2), InputPortIndex(0))
            .unwrap();
        let node = compacted.node(NodeId::new(2)).unwrap();
        assert_eq!(node.inputs.len(), 2);
        assert_eq!(node.inputs[0].name, "source");
        assert!(node.inputs[0].is_variadic);
        assert_eq!(node.inputs[1].name, "count");
        assert!(node.inputs[1].is_param);
        assert_eq!(
            compacted.edge(EdgeId::new(1)).unwrap().target_port,
            InputPortIndex(1)
        );
    }

    #[test]
    fn variadic_load_normalization_repairs_split_and_absent_groups() {
        let base = InputPort {
            name: "source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
            is_variadic: false,
        };
        let source = Node::new(NodeId::new(1), "source")
            .with_output("geometry_a", DataTypeId::GEOMETRY)
            .with_output("geometry_b", DataTypeId::GEOMETRY)
            .with_output("scalar", DataTypeId::SCALAR);
        let mut split = Node::new(NodeId::new(2), "variadic")
            .with_input("fixed", &[DataTypeId::GEOMETRY])
            .with_input("source", &[DataTypeId::GEOMETRY])
            .with_param("count", ParameterValue::Int(1));
        split.inputs.push(InputPort {
            name: "count".into(),
            accepted_types: vec![DataTypeId::SCALAR],
            is_param: true,
            is_variadic: false,
        });
        split.inputs.push(InputPort {
            name: "source_2".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
            is_variadic: true,
        });
        let graph = Graph::new()
            .add_node(source.clone())
            .unwrap()
            .add_node(split)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                source.id,
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(1),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                source.id,
                OutputPortIndex(2),
                NodeId::new(2),
                InputPortIndex(2),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(3),
                source.id,
                OutputPortIndex(1),
                NodeId::new(2),
                InputPortIndex(3),
            )
            .unwrap()
            .normalize_variadic_input_group(NodeId::new(2), 1, &base)
            .unwrap();
        let node = graph.node(NodeId::new(2)).unwrap();
        assert_eq!(
            node.inputs
                .iter()
                .map(|input| input.name.as_str())
                .collect::<Vec<_>>(),
            ["fixed", "source", "source_2", "source_3", "count"]
        );
        assert!(node.inputs[1..4].iter().all(|input| input.is_variadic));
        assert!(node.inputs[4].is_param);
        assert_eq!(
            graph.edge(EdgeId::new(1)).unwrap().target_port,
            InputPortIndex(1)
        );
        assert_eq!(
            graph.edge(EdgeId::new(2)).unwrap().target_port,
            InputPortIndex(4)
        );
        assert_eq!(
            graph.edge(EdgeId::new(3)).unwrap().target_port,
            InputPortIndex(2)
        );

        let mut absent = Node::new(NodeId::new(3), "variadic")
            .with_input("fixed", &[DataTypeId::GEOMETRY])
            .with_param("count", ParameterValue::Int(1));
        absent.inputs.push(InputPort {
            name: "count".into(),
            accepted_types: vec![DataTypeId::SCALAR],
            is_param: true,
            is_variadic: false,
        });
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(absent)
            .unwrap()
            .add_edge(
                EdgeId::new(4),
                NodeId::new(1),
                OutputPortIndex(2),
                NodeId::new(3),
                InputPortIndex(1),
            )
            .unwrap()
            .normalize_variadic_input_group(NodeId::new(3), 1, &base)
            .unwrap();
        let node = graph.node(NodeId::new(3)).unwrap();
        assert_eq!(
            node.inputs
                .iter()
                .map(|input| input.name.as_str())
                .collect::<Vec<_>>(),
            ["fixed", "source", "count"]
        );
        assert!(node.inputs[1].is_variadic);
        assert!(node.inputs[2].is_param);
        assert_eq!(
            graph.edge(EdgeId::new(4)).unwrap().target_port,
            InputPortIndex(2)
        );
    }

    #[test]
    fn exposed_param_port_participates_in_cycle_detection() {
        // a → b via a data edge, then b.out → a's exposed param must be
        // rejected as a cycle.
        let a = Node::new(NodeId::new(1), "test")
            .with_input("in", &[DataTypeId::FRAME_BUFFER])
            .with_output("out", DataTypeId::SCALAR)
            .with_param("radius", ParameterValue::Float(1.0));
        let b = Node::new(NodeId::new(2), "test")
            .with_input("in", &[DataTypeId::SCALAR])
            .with_output("out", DataTypeId::SCALAR);
        let g = Graph::new()
            .add_node(a)
            .unwrap()
            .add_node(b)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap()
            .expose_param_port(NodeId::new(1), "radius")
            .unwrap();
        assert!(matches!(
            g.add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(1),
                InputPortIndex(1),
            ),
            Err(GraphError::CycleDetected { .. })
        ));
    }

    #[test]
    fn replace_node_prunes_orphaned_param_ports() {
        // Exposed radius port with an edge; the replacement node dropped
        // the radius parameter → port and edge are pruned, later edges
        // re-index.
        let source = Node::new(NodeId::new(1), "test")
            .with_output("a", DataTypeId::SCALAR)
            .with_output("b", DataTypeId::COLOR);
        let g = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(param_node(2))
            .unwrap()
            .expose_param_port(NodeId::new(2), "radius")
            .unwrap()
            .expose_param_port(NodeId::new(2), "tint")
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(2),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(1),
                OutputPortIndex(1),
                NodeId::new(2),
                InputPortIndex(3),
            )
            .unwrap();

        let mut replacement = (**g.node(NodeId::new(2)).unwrap()).clone();
        replacement.parameters.retain(|p| p.key != "radius");
        let g = g.replace_node(Arc::new(replacement));

        let node = g.node(NodeId::new(2)).unwrap();
        assert!(node.param_port_index("radius").is_none(), "port pruned");
        assert_eq!(node.param_port_index("tint"), Some(InputPortIndex(2)));
        assert!(g.edge(EdgeId::new(1)).is_none(), "orphaned edge pruned");
        assert_eq!(
            g.edge(EdgeId::new(2)).unwrap().target_port,
            InputPortIndex(2),
            "tint edge re-indexed"
        );
    }

    #[test]
    fn param_port_survives_ron_roundtrip() {
        let g = Graph::new()
            .add_node(param_node(1))
            .unwrap()
            .expose_param_port(NodeId::new(1), "radius")
            .unwrap();
        let text = ron::to_string(&g).unwrap();
        let restored: Graph = ron::from_str(&text).unwrap();
        let node = restored.node(NodeId::new(1)).unwrap();
        assert_eq!(node.param_port_index("radius"), Some(InputPortIndex(2)));
        assert!(node.inputs[2].is_param);
    }

    #[test]
    fn variadic_input_flag_is_serialized_and_defaults_for_legacy_ron() {
        let port = InputPort {
            name: "source".into(),
            accepted_types: vec![DataTypeId::GEOMETRY],
            is_param: false,
            is_variadic: false,
        };
        let current = ron::to_string(&port).unwrap();
        assert!(current.contains("is_variadic:false"));

        let legacy = current.replace(",is_variadic:false", "");
        let restored: InputPort = ron::from_str(&legacy).unwrap();
        assert!(!restored.is_variadic);
    }

    // ---- bypass -------------------------------------------------------------

    #[test]
    fn bypassable_requires_an_input_matching_an_output_type() {
        // 1-in/1-out with matching types.
        assert!(make_node(1).is_bypassable());

        // Pure generator: outputs but no inputs.
        let generator =
            Node::new(NodeId::new(2), "constant").with_output("out", DataTypeId::SCALAR);
        assert!(!generator.is_bypassable());

        // Inputs exist but none accepts the output type.
        let mismatched = Node::new(NodeId::new(3), "test")
            .with_input("in", &[DataTypeId::GEOMETRY])
            .with_output("out", DataTypeId::SCALAR);
        assert!(!mismatched.is_bypassable());

        // No outputs at all.
        let sink = Node::new(NodeId::new(4), "test").with_input("in", &[DataTypeId::SCALAR]);
        assert!(!sink.is_bypassable());
    }

    #[test]
    fn bypassable_requires_every_output_to_match() {
        // Both outputs have a type-matching non-parameter input.
        let both = Node::new(NodeId::new(1), "test")
            .with_input("s", &[DataTypeId::SCALAR])
            .with_input("v", &[DataTypeId::VEC2])
            .with_output("x", DataTypeId::SCALAR)
            .with_output("y", DataTypeId::VEC2);
        assert!(both.is_bypassable());

        // One matching input passes both same-type outputs through.
        let shared = Node::new(NodeId::new(2), "test")
            .with_input("s", &[DataTypeId::SCALAR])
            .with_output("x", DataTypeId::SCALAR)
            .with_output("y", DataTypeId::SCALAR);
        assert!(shared.is_bypassable());

        // Only the scalar output matches: the evaluator falls back to
        // normal processing (the Vec2 output has nothing to pass through),
        // so the node must not look bypassable in the UI.
        let partial = Node::new(NodeId::new(3), "test")
            .with_input("s", &[DataTypeId::SCALAR])
            .with_output("x", DataTypeId::SCALAR)
            .with_output("y", DataTypeId::VEC2);
        assert!(!partial.is_bypassable());
    }

    #[test]
    fn param_ports_do_not_make_a_node_bypassable() {
        // The only type-matching input is a parameter port: parameter drives
        // are stripped before processing and must not count as pass-through
        // sources.
        let node = Node::new(NodeId::new(1), "test")
            .with_output("out", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(1.0));
        let g = Graph::new()
            .add_node(node)
            .unwrap()
            .expose_param_port(NodeId::new(1), "value")
            .unwrap();
        let node = g.node(NodeId::new(1)).unwrap();
        assert!(node.inputs[0].is_param);
        assert_eq!(node.inputs[0].accepted_types, [DataTypeId::SCALAR]);
        assert!(!node.is_bypassable());
    }

    #[test]
    fn bypassed_flag_survives_ron_roundtrip() {
        let mut node = make_node(1);
        node.metadata.bypassed = true;
        let g = Graph::new().add_node(node).unwrap();
        let text = ron::to_string(&g).unwrap();
        let restored: Graph = ron::from_str(&text).unwrap();
        assert_eq!(g, restored);
        assert!(restored.node(NodeId::new(1)).unwrap().metadata.bypassed);
    }

    #[test]
    fn metadata_without_bypassed_field_deserializes_as_false() {
        // Documents saved before the bypassed field existed must load with
        // bypass off (additive serde(default) field).
        let text = ron::to_string(&Graph::new().add_node(make_node(1)).unwrap()).unwrap();
        let stripped = text.replace(",bypassed:false", "");
        assert_ne!(stripped, text, "test precondition: field was present");
        let restored: Graph = ron::from_str(&stripped).unwrap();
        assert!(!restored.node(NodeId::new(1)).unwrap().metadata.bypassed);
    }
}
