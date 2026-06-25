// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Undo/redo stack backed by structurally-shared `Graph` snapshots.

use crate::graph::Graph;
use std::collections::VecDeque;

/// Linear undo/redo stack.
///
/// Each push stores an `Arc`-shared `Graph` snapshot. Because `Graph` uses
/// `im::HashMap` internally, snapshots that share structure only pay for the
/// changed entries.
pub struct UndoStack {
    versions: VecDeque<Graph>,
    current: usize,
    max_history: Option<usize>,
}

impl UndoStack {
    /// Create a new stack with the initial graph as version 0.
    pub fn new(initial: Graph) -> Self {
        let mut versions = VecDeque::new();
        versions.push_back(initial);
        Self {
            versions,
            current: 0,
            max_history: None,
        }
    }

    /// Set the maximum number of retained versions (excluding the current one).
    /// When exceeded, the oldest versions are dropped (O(1) via VecDeque).
    pub fn with_max_history(mut self, max: usize) -> Self {
        self.max_history = Some(max);
        self
    }

    /// The current graph.
    pub fn current(&self) -> &Graph {
        &self.versions[self.current]
    }

    /// Push a new version, discarding any redo history.
    pub fn push(&mut self, graph: Graph) {
        self.versions.truncate(self.current + 1);
        self.versions.push_back(graph);
        self.current += 1;

        if let Some(max) = self.max_history {
            while self.versions.len() > max + 1 {
                self.versions.pop_front();
                self.current -= 1;
            }
        }
    }

    /// Move back one version. Returns the graph, or `None` if at the oldest.
    pub fn undo(&mut self) -> Option<&Graph> {
        if self.current == 0 {
            return None;
        }
        self.current -= 1;
        Some(&self.versions[self.current])
    }

    /// Move forward one version. Returns the graph, or `None` if at the newest.
    pub fn redo(&mut self) -> Option<&Graph> {
        if self.current + 1 >= self.versions.len() {
            return None;
        }
        self.current += 1;
        Some(&self.versions[self.current])
    }

    pub fn can_undo(&self) -> bool {
        self.current > 0
    }

    pub fn can_redo(&self) -> bool {
        self.current + 1 < self.versions.len()
    }

    /// Number of stored versions (including current).
    pub fn len(&self) -> usize {
        self.versions.len()
    }

    /// Whether the stack contains only the initial version.
    pub fn is_empty(&self) -> bool {
        self.versions.len() <= 1
    }

    /// Current version index (0-based).
    pub fn current_index(&self) -> usize {
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Node;
    use crate::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};

    fn node(id: u64) -> Node {
        Node::new(NodeId::new(id), "test").with_output("out", DataTypeId::SCALAR)
    }

    #[test]
    fn initial_state() {
        let g = Graph::new().add_node(node(1)).unwrap();
        let stack = UndoStack::new(g);
        assert_eq!(stack.current().node_count(), 1);
        assert!(!stack.can_undo());
        assert!(!stack.can_redo());
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn push_and_undo() {
        let g0 = Graph::new().add_node(node(1)).unwrap();
        let mut stack = UndoStack::new(g0);
        let g1 = stack.current().clone().add_node(node(2)).unwrap();
        stack.push(g1);
        assert_eq!(stack.current().node_count(), 2);
        assert!(stack.can_undo());

        let prev = stack.undo().unwrap();
        assert_eq!(prev.node_count(), 1);
        assert!(!stack.can_undo());
        assert!(stack.can_redo());
    }

    #[test]
    fn redo_after_undo() {
        let g0 = Graph::new().add_node(node(1)).unwrap();
        let mut stack = UndoStack::new(g0);
        let g1 = stack.current().clone().add_node(node(2)).unwrap();
        stack.push(g1);

        stack.undo();
        let next = stack.redo().unwrap();
        assert_eq!(next.node_count(), 2);
        assert!(!stack.can_redo());
    }

    #[test]
    fn push_after_undo_discards_redo_history() {
        let g0 = Graph::new().add_node(node(1)).unwrap();
        let mut stack = UndoStack::new(g0);

        let g1 = stack.current().clone().add_node(node(2)).unwrap();
        stack.push(g1);
        let g2 = stack.current().clone().add_node(node(3)).unwrap();
        stack.push(g2);

        stack.undo(); // back to g1
        let g_alt = stack.current().clone().add_node(node(4)).unwrap();
        stack.push(g_alt);

        assert!(!stack.can_redo());
        assert_eq!(stack.len(), 3); // g0, g1, g_alt
        assert_eq!(stack.current().node_count(), 3); // 1, 2, 4
    }

    #[test]
    fn max_history_trims_oldest() {
        let g0 = Graph::new().add_node(node(1)).unwrap();
        let mut stack = UndoStack::new(g0).with_max_history(2);

        for i in 2..=5u64 {
            let g = stack.current().clone().add_node(node(i)).unwrap();
            stack.push(g);
        }

        // max_history=2 → keep 3 versions (current + 2 undo steps)
        assert_eq!(stack.len(), 3);
        assert_eq!(stack.current().node_count(), 5);
    }

    #[test]
    fn undo_at_oldest_returns_none() {
        let g0 = Graph::new();
        let mut stack = UndoStack::new(g0);
        assert!(stack.undo().is_none());
    }

    #[test]
    fn redo_at_newest_returns_none() {
        let g0 = Graph::new();
        let mut stack = UndoStack::new(g0);
        assert!(stack.redo().is_none());
    }

    #[test]
    fn structural_sharing_across_versions() {
        let g0 = Graph::new().add_node(node(1)).unwrap();
        let mut stack = UndoStack::new(g0);

        let g1 = stack.current().clone().add_node(node(2)).unwrap();
        stack.push(g1);

        // Both versions share node 1's Arc
        stack.undo();
        let n1_v0 = std::sync::Arc::as_ptr(stack.current().node(NodeId::new(1)).unwrap());
        stack.redo();
        let n1_v1 = std::sync::Arc::as_ptr(stack.current().node(NodeId::new(1)).unwrap());
        assert_eq!(n1_v0, n1_v1);
    }

    #[test]
    fn undo_redo_with_edges() {
        let g0 = Graph::new()
            .add_node(node(1))
            .unwrap()
            .add_node(node(2).with_input("in", &[DataTypeId::SCALAR]))
            .unwrap();
        let mut stack = UndoStack::new(g0);

        let g1 = stack
            .current()
            .clone()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        stack.push(g1);
        assert_eq!(stack.current().edge_count(), 1);

        stack.undo();
        assert_eq!(stack.current().edge_count(), 0);

        stack.redo();
        assert_eq!(stack.current().edge_count(), 1);
    }
}
