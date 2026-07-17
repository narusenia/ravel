// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Crash recovery: replay a journal on top of a base graph.

use super::journal::{JournalError, JournalReader};
use crate::graph::{Graph, GraphError};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("journal I/O error")]
    Journal(#[from] JournalError),

    #[error("mutation replay failed at sequence {sequence}")]
    ReplayFailed {
        sequence: u64,
        #[source]
        source: GraphError,
    },
}

/// Result of a recovery attempt.
pub struct RecoveryResult {
    /// The graph after replaying all valid entries.
    pub graph: Graph,
    /// Number of entries successfully replayed.
    pub replayed: usize,
    /// Entries that were skipped due to corruption or replay failure.
    pub skipped: Vec<SkippedEntry>,
}

#[derive(Debug)]
pub struct SkippedEntry {
    pub sequence: u64,
    pub reason: String,
}

/// Replay a journal file on top of `base_graph`.
///
/// Entries whose mutations fail to apply (e.g. removing a node that doesn't
/// exist) are skipped with a warning rather than aborting recovery. After a
/// successful replay the global id counters are advanced past every id in
/// the recovered graph (subnets included), so fresh allocations cannot
/// collide with replayed nodes/edges (REQ-LAYER-009).
pub fn recover(
    base_graph: Graph,
    journal_path: &Path,
    reader: &JournalReader,
) -> Result<RecoveryResult, RecoveryError> {
    let mut skipped = Vec::new();

    let entries = reader.read_all(journal_path, |err| {
        let sequence = match &err {
            JournalError::CorruptEntry { sequence, .. } => *sequence,
            _ => 0,
        };
        skipped.push(SkippedEntry {
            sequence,
            reason: err.to_string(),
        });
    })?;

    let mut graph = base_graph;
    let mut replayed = 0;

    for entry in &entries {
        match entry.mutation.apply(&graph) {
            Ok(new_graph) => {
                graph = new_graph;
                replayed += 1;
            }
            Err(err) => {
                tracing::warn!(
                    sequence = entry.sequence,
                    error = %err,
                    "skipping journal entry during recovery"
                );
                skipped.push(SkippedEntry {
                    sequence: entry.sequence,
                    reason: err.to_string(),
                });
            }
        }
    }

    advance_counters_past(&graph);

    tracing::info!(
        replayed,
        skipped = skipped.len(),
        "journal recovery complete"
    );

    Ok(RecoveryResult {
        graph,
        replayed,
        skipped,
    })
}

/// Advance the global node/edge id counters past every id in `graph`
/// (recursing into subnets), so allocations after a replay never collide
/// with recovered ids.
fn advance_counters_past(graph: &Graph) {
    for node in graph.nodes() {
        crate::id::NodeId::advance_counter_past(node.id.raw());
        if let Some(subnet) = &node.subnet {
            advance_counters_past(subnet);
        }
    }
    for edge in graph.edges() {
        crate::id::EdgeId::advance_counter_past(edge.id.raw());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Node;
    use crate::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use crate::undo::GraphMutation;
    use crate::undo::journal::{BincodeCodec, JournalWriter};

    #[test]
    fn replay_empty_journal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.journal");
        std::fs::write(&path, b"").unwrap();

        let reader = JournalReader::new(Box::new(BincodeCodec));
        let result = recover(Graph::new(), &path, &reader).unwrap();
        assert_eq!(result.replayed, 0);
        assert!(result.skipped.is_empty());
    }

    #[test]
    fn replay_add_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.journal");

        {
            let mut w = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
            w.append(
                GraphMutation::AddNode(
                    Node::new(NodeId::new(1), "a").with_output("out", DataTypeId::SCALAR),
                ),
                1000,
            )
            .unwrap();
            w.append(
                GraphMutation::AddNode(
                    Node::new(NodeId::new(2), "b")
                        .with_input("in", &[DataTypeId::SCALAR])
                        .with_output("out", DataTypeId::SCALAR),
                ),
                1001,
            )
            .unwrap();
            w.append(
                GraphMutation::AddEdge(crate::graph::Edge {
                    id: EdgeId::new(1),
                    source: NodeId::new(1),
                    source_port: OutputPortIndex(0),
                    target: NodeId::new(2),
                    target_port: InputPortIndex(0),
                }),
                1002,
            )
            .unwrap();
        }

        let reader = JournalReader::new(Box::new(BincodeCodec));
        let result = recover(Graph::new(), &path, &reader).unwrap();
        assert_eq!(result.replayed, 3);
        assert_eq!(result.graph.node_count(), 2);
        assert_eq!(result.graph.edge_count(), 1);
    }

    #[test]
    fn replay_skips_failing_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.journal");

        {
            let mut w = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
            // Remove a node that doesn't exist
            w.append(GraphMutation::RemoveNode(NodeId::new(999)), 1000)
                .unwrap();
            // Valid add
            w.append(
                GraphMutation::AddNode(
                    Node::new(NodeId::new(1), "a").with_output("out", DataTypeId::SCALAR),
                ),
                1001,
            )
            .unwrap();
        }

        let reader = JournalReader::new(Box::new(BincodeCodec));
        let result = recover(Graph::new(), &path, &reader).unwrap();
        assert_eq!(result.replayed, 1);
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.graph.node_count(), 1);
    }

    /// Recovery advances the id counters past replayed ids, so fresh
    /// allocations after a crash never collide with recovered nodes
    /// (REQ-LAYER-009).
    #[test]
    fn replay_advances_the_id_counters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("high_ids.journal");

        {
            let mut w = JournalWriter::open(&path, Box::new(BincodeCodec)).unwrap();
            w.append(
                GraphMutation::AddNode(
                    Node::new(NodeId::new(99_999), "a").with_output("out", DataTypeId::SCALAR),
                ),
                1000,
            )
            .unwrap();
            w.append(
                GraphMutation::AddEdge(crate::graph::Edge {
                    id: EdgeId::new(88_888),
                    source: NodeId::new(99_999),
                    source_port: OutputPortIndex(0),
                    target: NodeId::new(99_999),
                    target_port: InputPortIndex(0),
                }),
                1001,
            )
            .unwrap(); // written fine; replay rejects the self-edge
        }

        let reader = JournalReader::new(Box::new(BincodeCodec));
        let result = recover(Graph::new(), &path, &reader).unwrap();
        assert_eq!(result.graph.node_count(), 1);
        assert!(NodeId::next().raw() > 99_999);
        // The edge was never replayed, so its id need not be reserved; the
        // node id must be.
    }
}
