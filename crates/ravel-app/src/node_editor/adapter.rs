// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bidirectional conversion between ravel-core Graph and gpui-flow FlowState.

use gpui::SharedString;
#[cfg(test)]
use gpui_flow::HandleType;
use gpui_flow::{FlowEdge, FlowNode, HandleDef, HandlePosition};
use ravel_core::graph::{Edge, Graph, Node};
use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};

pub fn node_to_flow_node(node: &Node) -> FlowNode {
    let id: SharedString = format!("n{}", node.id.raw()).into();
    let (x, y) = node.metadata.position;

    let mut handles = Vec::new();

    for (i, _input) in node.inputs.iter().enumerate() {
        handles.push(HandleDef::target(HandlePosition::Left).id(format!("in:{i}")));
    }
    for (i, _output) in node.outputs.iter().enumerate() {
        handles.push(HandleDef::source(HandlePosition::Right).id(format!("out:{i}")));
    }

    let label = node
        .metadata
        .label
        .clone()
        .unwrap_or_else(|| node.type_key.clone());

    FlowNode::new(id, x, y)
        .label(label)
        .node_type(SharedString::from(node.type_key.clone()))
        .handles(handles)
}

pub fn edge_to_flow_edge(edge: &Edge) -> FlowEdge {
    let id: SharedString = format!("e{}", edge.id.raw()).into();
    let source: SharedString = format!("n{}", edge.source.raw()).into();
    let target: SharedString = format!("n{}", edge.target.raw()).into();

    FlowEdge::new(id, source, target)
        .source_handle(format!("out:{}", edge.source_port.0))
        .target_handle(format!("in:{}", edge.target_port.0))
}

pub fn graph_to_flow(graph: &Graph) -> (Vec<FlowNode>, Vec<FlowEdge>) {
    let nodes = graph.nodes().map(|n| node_to_flow_node(n)).collect();
    let edges = graph.edges().map(edge_to_flow_edge).collect();
    (nodes, edges)
}

pub fn parse_flow_node_id(flow_id: &str) -> Option<NodeId> {
    flow_id
        .strip_prefix('n')
        .and_then(|s| s.parse::<u64>().ok())
        .map(NodeId::new)
}

pub fn parse_flow_edge_id(flow_id: &str) -> Option<EdgeId> {
    flow_id
        .strip_prefix('e')
        .and_then(|s| s.parse::<u64>().ok())
        .map(EdgeId::new)
}

pub fn parse_handle_id(handle_id: &str) -> Option<(bool, u32)> {
    if let Some(idx) = handle_id.strip_prefix("in:") {
        idx.parse::<u32>().ok().map(|i| (true, i))
    } else if let Some(idx) = handle_id.strip_prefix("out:") {
        idx.parse::<u32>().ok().map(|i| (false, i))
    } else {
        None
    }
}

pub fn connection_to_edge_params(
    source_node: &str,
    target_node: &str,
    source_handle: Option<&str>,
    target_handle: Option<&str>,
) -> Option<(NodeId, OutputPortIndex, NodeId, InputPortIndex)> {
    let src_id = parse_flow_node_id(source_node)?;
    let tgt_id = parse_flow_node_id(target_node)?;

    let (_, out_idx) = source_handle.and_then(parse_handle_id)?;
    let (_, in_idx) = target_handle.and_then(parse_handle_id)?;

    Some((
        src_id,
        OutputPortIndex(out_idx),
        tgt_id,
        InputPortIndex(in_idx),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::id::DataTypeId;

    #[test]
    fn roundtrip_node_id() {
        let id = NodeId::new(42);
        let flow_id = format!("n{}", id.raw());
        assert_eq!(parse_flow_node_id(&flow_id), Some(id));
    }

    #[test]
    fn roundtrip_handle_id() {
        assert_eq!(parse_handle_id("in:2"), Some((true, 2)));
        assert_eq!(parse_handle_id("out:0"), Some((false, 0)));
        assert_eq!(parse_handle_id("bad"), None);
    }

    #[test]
    fn node_to_flow_preserves_ports() {
        let node = Node::new(NodeId::new(1), "blur")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_input("radius", &[DataTypeId::SCALAR])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_position(100.0, 200.0);

        let flow = node_to_flow_node(&node);
        assert_eq!(flow.handles.len(), 3);
        assert_eq!(flow.handles[0].handle_type, HandleType::Target);
        assert_eq!(flow.handles[2].handle_type, HandleType::Source);
        assert_eq!(flow.position.x, 100.0);
        assert_eq!(flow.position.y, 200.0);
    }

    #[test]
    fn connection_params_parsing() {
        let result = connection_to_edge_params("n1", "n2", Some("out:0"), Some("in:1"));
        let (src, out, tgt, inp) = result.unwrap();
        assert_eq!(src, NodeId::new(1));
        assert_eq!(out, OutputPortIndex(0));
        assert_eq!(tgt, NodeId::new(2));
        assert_eq!(inp, InputPortIndex(1));
    }
}
