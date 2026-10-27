// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Graph-level wiring: the one implementation of connecting, disconnecting and
//! removing nodes, including the variadic-group upkeep that goes with it.
//!
//! The functions take and return a [`Graph`], so a caller holding a network it
//! has not committed yet (the Viewer's drawing tools add a node and wire it in
//! one step) shares them with [`super::apply`], which runs the same code on the
//! network a [`NetworkPath`] addresses.
//!
//! [`Graph::add_edge`] checks existence, duplicates and cycles only, so port
//! numbers and types are validated here.

use super::EditError;
use super::ops::PortRef;
use crate::graph::{Graph, GraphError, Node};
use crate::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use std::collections::HashSet;

/// Restore the variadic-group invariant after one or more edges have been
/// removed from `node_id`: connected slots stay ordered and exactly one empty
/// slot remains at the end.
pub fn compact_empty_variadic_inputs(mut graph: Graph, node_id: NodeId) -> Graph {
    loop {
        let Some(node) = graph.node(node_id) else {
            return graph;
        };
        let Some((index, _)) = node.inputs.iter().enumerate().find(|(index, input)| {
            input.is_variadic
                && node.inputs[*index + 1..]
                    .first()
                    .is_some_and(|next| next.is_variadic)
                && !graph.edges().any(|edge| {
                    edge.target == node_id && edge.target_port == InputPortIndex(*index as u32)
                })
        }) else {
            return graph;
        };
        let Ok(compacted) = graph
            .clone()
            .compact_variadic_input_group(node_id, InputPortIndex(index as u32))
        else {
            return graph;
        };
        graph = compacted;
    }
}

/// Remove one edge and compact its target's variadic group in the same graph
/// snapshot.
pub fn remove_edge_and_compact(graph: Graph, edge_id: EdgeId) -> Result<Graph, EditError> {
    let target = graph
        .edge(edge_id)
        .ok_or(EditError::EdgeNotFound(edge_id))?
        .target;
    let removed = graph.remove_edge(edge_id)?;
    Ok(compact_empty_variadic_inputs(removed, target))
}

/// Remove one node and compact every surviving variadic target reached by its
/// outgoing edges.
pub fn remove_node_and_compact(graph: Graph, node_id: NodeId) -> Result<Graph, EditError> {
    if graph.node(node_id).is_none() {
        return Err(EditError::NodeNotFound(node_id));
    }
    let targets: HashSet<_> = graph
        .edges()
        .filter(|edge| edge.source == node_id)
        .map(|edge| edge.target)
        .collect();
    let mut graph = graph.remove_node(node_id)?;
    for target in targets {
        graph = compact_empty_variadic_inputs(graph, target);
    }
    Ok(graph)
}

/// Index of the port `port` names on `node`'s outputs (`input == false`) or
/// inputs, refusing a name or number the node does not have.
pub fn resolve_port(node: &Node, port: &PortRef, input: bool) -> Result<u32, EditError> {
    let names: Vec<&str> = if input {
        node.inputs.iter().map(|p| p.name.as_str()).collect()
    } else {
        node.outputs.iter().map(|p| p.name.as_str()).collect()
    };
    let found = match port {
        PortRef::Index(index) => (*index as usize) < names.len(),
        PortRef::Name(name) => names.contains(&name.as_str()),
    };
    match (found, port) {
        (true, PortRef::Index(index)) => Ok(*index),
        (true, PortRef::Name(name)) => Ok(names.iter().position(|n| n == name).unwrap_or(0) as u32),
        (false, _) => Err(EditError::PortNotFound {
            node: node.id,
            port: port.clone(),
            input,
        }),
    }
}

/// Replace any edge occupying `target_port`, connect the new edge, and grow a
/// trailing variadic slot when the connection fills the group. Replacement of
/// a variadic slot compacts first, so the replacement is appended after the
/// surviving connected sources.
///
/// Refuses a missing node, a port number the node does not have, a source type
/// the target port does not accept (an empty accepted list takes anything), a
/// duplicate edge and a cycle.
pub fn connect_edge_and_update_variadic_inputs(
    mut graph: Graph,
    edge_id: EdgeId,
    source: NodeId,
    source_port: OutputPortIndex,
    target: NodeId,
    mut target_port: InputPortIndex,
) -> Result<Graph, EditError> {
    let source_node = graph.node(source).ok_or(EditError::NodeNotFound(source))?;
    let target_node = graph.node(target).ok_or(EditError::NodeNotFound(target))?;
    let output =
        source_node
            .outputs
            .get(source_port.0 as usize)
            .ok_or(EditError::PortNotFound {
                node: source,
                port: PortRef::Index(source_port.0),
                input: false,
            })?;
    let input = target_node
        .inputs
        .get(target_port.0 as usize)
        .ok_or(EditError::PortNotFound {
            node: target,
            port: PortRef::Index(target_port.0),
            input: true,
        })?;
    if !(input.accepted_types.is_empty() || input.accepted_types.contains(&output.data_type)) {
        return Err(EditError::PortTypeMismatch {
            output_node: source,
            output_port: source_port.0,
            input_node: target,
            input_port: target_port.0,
        });
    }
    let target_is_variadic = input.is_variadic;
    let existing: Vec<_> = graph
        .edges()
        .filter(|edge| edge.target == target && edge.target_port == target_port)
        .map(|edge| edge.id)
        .collect();
    for existing_edge in existing {
        graph = remove_edge_and_compact(graph, existing_edge)?;
    }

    if target_is_variadic {
        target_port = graph
            .node(target)
            .ok_or(EditError::NodeNotFound(target))?
            .inputs
            .iter()
            .enumerate()
            .find(|(index, input)| {
                input.is_variadic
                    && !graph.edges().any(|edge| {
                        edge.target == target && edge.target_port == InputPortIndex(*index as u32)
                    })
            })
            .map(|(index, _)| InputPortIndex(index as u32))
            .ok_or(GraphError::VariadicInputGroupNotFound(target))?;
    }

    let graph = graph.add_edge(edge_id, source, source_port, target, target_port)?;
    Ok(graph
        .clone()
        .grow_variadic_input_group(target)
        .unwrap_or(graph))
}
