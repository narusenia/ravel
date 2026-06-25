// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Graph mutation types for undo journaling.

use crate::graph::{Edge, Graph, GraphError, Node, NodeMetadata};
use crate::id::{EdgeId, NodeId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A discrete, replayable change to the graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GraphMutation {
    AddNode(Node),
    RemoveNode(NodeId),
    UpdateNodeMetadata { id: NodeId, metadata: NodeMetadata },
    AddEdge(Edge),
    RemoveEdge(EdgeId),
}

impl GraphMutation {
    /// Apply this mutation to `graph`, returning the new graph.
    pub fn apply(&self, graph: &Graph) -> Result<Graph, GraphError> {
        match self {
            Self::AddNode(node) => Ok(graph.clone().add_node(node.clone())?),
            Self::RemoveNode(id) => graph.clone().remove_node(*id),
            Self::UpdateNodeMetadata { id, metadata } => {
                let node = graph.node(*id).ok_or(GraphError::NodeNotFound(*id))?;
                let mut updated = node.as_ref().clone();
                updated.metadata = metadata.clone();
                Ok(graph.clone().replace_node(Arc::new(updated)))
            }
            Self::AddEdge(edge) => graph.clone().add_edge(
                edge.id,
                edge.source,
                edge.source_port,
                edge.target,
                edge.target_port,
            ),
            Self::RemoveEdge(id) => graph.clone().remove_edge(*id),
        }
    }
}
