// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! RON serialization of the node graph (`graph/main.ron`).
//!
//! The in-memory [`Graph`] uses `im` persistent maps whose iteration order is
//! unspecified. To keep project files **diff-friendly** (a stable text form
//! where unrelated edits don't reshuffle the file), the graph is projected onto
//! a [`GraphDoc`] with nodes and edges stored in id-sorted [`Vec`]s before
//! serialization. Loading reverses the projection through
//! [`Graph::from_parts`], which re-validates acyclicity, so a corrupt edge set
//! is rejected rather than producing an invalid graph.

use ravel_core::graph::{Edge, Graph, GraphError, Node};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors raised while (de)serializing a graph document.
#[derive(Debug, Error)]
pub enum GraphDocError {
    #[error("failed to serialize graph to RON: {0}")]
    Serialize(#[from] ron::Error),

    #[error("failed to parse graph RON: {0}")]
    Parse(#[from] ron::de::SpannedError),

    #[error("graph document is structurally invalid: {0}")]
    Invalid(#[from] GraphError),
}

/// Diff-friendly, serializable projection of a [`Graph`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphDoc {
    /// Nodes, sorted by [`NodeId`] for deterministic output.
    pub nodes: Vec<Node>,
    /// Edges, sorted by [`EdgeId`] for deterministic output.
    pub edges: Vec<Edge>,
}

impl GraphDoc {
    /// Project a live [`Graph`] into a deterministic document.
    pub fn from_graph(graph: &Graph) -> Self {
        let mut nodes: Vec<Node> = graph.nodes().map(|n| (**n).clone()).collect();
        nodes.sort_by_key(|n| n.id);

        let mut edges: Vec<Edge> = graph.edges().cloned().collect();
        edges.sort_by_key(|e| e.id);

        Self { nodes, edges }
    }

    /// Rebuild a validated [`Graph`] from this document.
    pub fn into_graph(self) -> Result<Graph, GraphError> {
        Graph::from_parts(self.nodes, self.edges)
    }

    /// Serialize to pretty-printed RON text.
    pub fn to_ron(&self) -> Result<String, GraphDocError> {
        let config = ron::ser::PrettyConfig::new()
            .struct_names(true)
            .indentor("  ".to_string());
        Ok(ron::ser::to_string_pretty(self, config)?)
    }

    /// Parse a graph document from RON text.
    pub fn from_ron(text: &str) -> Result<Self, GraphDocError> {
        Ok(ron::from_str(text)?)
    }

    /// Convenience: serialize a live graph straight to RON.
    pub fn graph_to_ron(graph: &Graph) -> Result<String, GraphDocError> {
        Self::from_graph(graph).to_ron()
    }

    /// Convenience: parse RON straight into a validated graph.
    pub fn graph_from_ron(text: &str) -> Result<Graph, GraphDocError> {
        Ok(Self::from_ron(text)?.into_graph()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};

    fn sample_node(id: u64) -> Node {
        Node::new(NodeId::new(id), "color_correct")
            .with_input("in", &[DataTypeId::FRAME_BUFFER])
            .with_output("out", DataTypeId::FRAME_BUFFER)
            .with_label(format!("node-{id}"))
            .with_position(id as f32 * 100.0, 200.0)
    }

    fn sample_graph() -> Graph {
        let g = Graph::new()
            .add_node(sample_node(1))
            .add_node(sample_node(2))
            .add_node(sample_node(3));
        g.add_edge(
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
        .unwrap()
    }

    #[test]
    fn roundtrip_preserves_graph() {
        let graph = sample_graph();
        let ron = GraphDoc::graph_to_ron(&graph).unwrap();
        let back = GraphDoc::graph_from_ron(&ron).unwrap();

        assert_eq!(back.node_count(), graph.node_count());
        assert_eq!(back.edge_count(), graph.edge_count());

        let original = GraphDoc::from_graph(&graph);
        let restored = GraphDoc::from_graph(&back);
        assert_eq!(original, restored);
    }

    #[test]
    fn document_is_sorted_deterministically() {
        // Build the same graph with nodes inserted in different orders.
        let g1 = Graph::new()
            .add_node(sample_node(3))
            .add_node(sample_node(1))
            .add_node(sample_node(2));
        let g2 = Graph::new()
            .add_node(sample_node(1))
            .add_node(sample_node(2))
            .add_node(sample_node(3));

        let d1 = GraphDoc::from_graph(&g1);
        let d2 = GraphDoc::from_graph(&g2);
        assert_eq!(d1, d2);
        assert_eq!(d1.nodes[0].id, NodeId::new(1));
        assert_eq!(d1.nodes[2].id, NodeId::new(3));
    }

    #[test]
    fn empty_graph_roundtrip() {
        let ron = GraphDoc::graph_to_ron(&Graph::new()).unwrap();
        let back = GraphDoc::graph_from_ron(&ron).unwrap();
        assert_eq!(back.node_count(), 0);
        assert_eq!(back.edge_count(), 0);
    }

    #[test]
    fn malformed_ron_is_error_not_panic() {
        assert!(GraphDoc::from_ron("this is not ron").is_err());
        assert!(GraphDoc::from_ron("GraphDoc(nodes: [], edges: ").is_err());
        assert!(GraphDoc::from_ron("").is_err());
    }

    #[test]
    fn cyclic_edge_set_is_rejected() {
        // Manually craft a document whose edges form a cycle 1->2->1.
        let doc = GraphDoc {
            nodes: vec![sample_node(1), sample_node(2)],
            edges: vec![
                Edge {
                    id: EdgeId::new(1),
                    source: NodeId::new(1),
                    source_port: OutputPortIndex(0),
                    target: NodeId::new(2),
                    target_port: InputPortIndex(0),
                },
                Edge {
                    id: EdgeId::new(2),
                    source: NodeId::new(2),
                    source_port: OutputPortIndex(0),
                    target: NodeId::new(1),
                    target_port: InputPortIndex(0),
                },
            ],
        };
        assert!(doc.into_graph().is_err());
    }
}
