// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared traversal for load-time graph upgrades.
//!
//! Every `.ravprj` upgrade that rewrites nodes has to reach the same set of
//! graphs: the document's flat graph, each `Layer::network`, and the inner
//! graph of every subnet at any depth. The document half of that walk is
//! [`Document::map_graphs`](super::Document::map_graphs); the nested half is
//! [`map_subnets`] here. An upgrade supplies only the one-graph rewrite and
//! inherits the reach.

use crate::graph::Graph;
use std::sync::Arc;

/// Apply `upgrade` to `graph` and to every graph nested inside it,
/// inner-most first.
///
/// Subnets are rewritten before their owning node so that replacing the outer
/// node afterwards cannot discard the inner rewrite.
pub(super) fn map_subnets(graph: &Graph, upgrade: &dyn Fn(&Graph) -> Graph) -> Graph {
    let mut mapped = graph.clone();
    for id in mapped.node_ids().collect::<Vec<_>>() {
        let Some(node) = mapped.node(id) else {
            continue;
        };
        let Some(inner) = node
            .subnet
            .as_ref()
            .map(|inner| map_subnets(inner, upgrade))
        else {
            continue;
        };
        let mut updated = (**node).clone();
        updated.subnet = Some(Arc::new(inner));
        mapped = mapped.replace_node(Arc::new(updated));
    }
    upgrade(&mapped)
}
