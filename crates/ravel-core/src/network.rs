// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Network interface conventions (REQ-LAYER-002).
//!
//! Every layer network (and, by the same mechanism, every subnet) contains
//! exactly one **In** node and one **Out** node, identified by type key:
//!
//! * `net.in` — the shell→network injection point. Fixed outputs
//!   `base_geometry` (GEOMETRY) and `t` (SCALAR, layer-local seconds), plus
//!   user-defined custom parameter ports, plus `source` (FRAME_BUFFER) on
//!   adjustment layers. A multi-output node: its value is a
//!   [`crate::types::PortRecord`] in output-port order.
//! * `net.out` — the network→shell result. Input port `frame`
//!   (FRAME_BUFFER) is the only port the shell consumes; additional custom
//!   input ports are exposed to Layer Ref (REQ-LAYER-005). Its value is a
//!   `PortRecord` in input-port order.

use crate::graph::{Graph, Node};
use crate::id::OutputPortIndex;
use std::sync::Arc;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{DataTypeId, NodeId};

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
}
