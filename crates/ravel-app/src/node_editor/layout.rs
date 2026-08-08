// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Auto-layout of node positions (`node-graph-readability-plan.md`, `NGR-1`).
//!
//! A pure function: it takes the graph, the nodes to move, their drawn sizes
//! and the axis, and returns the new position of every node it moved. It never
//! touches the [`Graph`] — the caller splices the positions into the document
//! so the move is one undo step like any other edit
//! (`NodeMetadata::position` is saved data, not a view property).
//!
//! Layering is the longest path over the subgraph *induced by the target set*,
//! computed here rather than borrowed from the evaluator: an alignment runs
//! once per keystroke while the evaluator's traversal is a hot path with
//! different obligations.
//!
//! The sizes come from the caller because the drawn height of a node is a
//! painting concern (`painting::compute_node_size`); this module therefore
//! needs to know nothing about GPUI or the zoom.

use std::collections::{HashMap, HashSet};

use ravel_core::graph::Graph;
use ravel_core::id::NodeId;

/// The direction layers advance in.
///
/// `NGR-5` (top-down flow mode) passes [`LayoutAxis::Vertical`]; nothing else
/// about the layout changes with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayoutAxis {
    /// Layers advance to the right, nodes within a layer stack downwards.
    #[default]
    Horizontal,
    /// Layers advance downwards, nodes within a layer stack to the right.
    Vertical,
}

/// Gap between two layers, along the axis.
const LAYER_GAP: f32 = 80.0;
/// Gap between two nodes inside one layer, across the axis.
const NODE_GAP: f32 = 24.0;
/// Size used for a node the caller gave no measurement for. The same fallback
/// the panel uses when it draws before its size cache is filled.
const FALLBACK_SIZE: (f32, f32) = (160.0, 60.0);

/// Lay `targets` out in layers and return their new positions.
///
/// Fewer than two usable `targets` means the whole network — a single node
/// has nothing to be aligned against, so there is no one-node alignment to
/// lose (see [`layout_members`]). Ids that name no node in `graph` are
/// ignored before that count is taken: node ids are unique across networks,
/// but a selection published for another network must not decide what moves
/// here.
///
/// Synthetic nodes (the compositing chain the Composition compiler generates)
/// are never moved: they are not drawn, so they have no position a user can
/// see.
///
/// `sizes` are widths and heights in **network coordinates**, the same space
/// `NodeMetadata::position` lives in — a caller holding zoomed sizes divides
/// them first.
///
/// The result is anchored at the top-left of the bounding box the moved nodes
/// already occupied, so aligning part of a network leaves it where the user
/// put it. Nodes outside `targets` never appear in the result.
pub fn auto_layout(
    graph: &Graph,
    targets: &HashSet<NodeId>,
    sizes: &HashMap<NodeId, (f32, f32)>,
    axis: LayoutAxis,
) -> HashMap<NodeId, (f32, f32)> {
    let members = layout_members(graph, targets);
    if members.is_empty() {
        return HashMap::new();
    }

    let depths = longest_path_depths(graph, &members);
    let anchor = bounding_box_origin(graph, &members);

    // Layer index → the nodes in it, in a deterministic order: by their
    // current cross-axis coordinate so the user's arrangement survives the
    // alignment, with the id as the tie-break so two nodes at the same
    // coordinate cannot swap between runs.
    let mut layers: Vec<Vec<NodeId>> = Vec::new();
    for (&id, &depth) in &depths {
        if layers.len() <= depth {
            layers.resize(depth + 1, Vec::new());
        }
        layers[depth].push(id);
    }
    for layer in &mut layers {
        layer.sort_by(|a, b| {
            let key = |id: &NodeId| match axis {
                LayoutAxis::Horizontal => position(graph, *id).1,
                LayoutAxis::Vertical => position(graph, *id).0,
            };
            key(a).total_cmp(&key(b)).then_with(|| a.cmp(b))
        });
    }

    let size_of = |id: NodeId| sizes.get(&id).copied().unwrap_or(FALLBACK_SIZE);
    let mut out = HashMap::new();
    let mut along = 0.0f32;
    for layer in &layers {
        // The layer occupies one band whose thickness is its widest member, so
        // the next layer clears every node in this one.
        let mut band = 0.0f32;
        let mut across = 0.0f32;
        for &id in layer {
            let (w, h) = size_of(id);
            // `thickness` is what the node consumes along the axis and
            // `extent` what it consumes across it; the two swap with the axis.
            let (thickness, extent) = match axis {
                LayoutAxis::Horizontal => (w, h),
                LayoutAxis::Vertical => (h, w),
            };
            let point = match axis {
                LayoutAxis::Horizontal => (anchor.0 + along, anchor.1 + across),
                LayoutAxis::Vertical => (anchor.0 + across, anchor.1 + along),
            };
            out.insert(id, point);
            band = band.max(thickness);
            across += extent + NODE_GAP;
        }
        along += band + LAYER_GAP;
    }
    out
}

/// The nodes an alignment moves: the target set restricted to nodes this graph
/// actually has and a user can see, or all of them when fewer than two remain.
///
/// **Fewer than two is the whole network, not a one-node alignment.** Laying a
/// single node out is undefined by construction — it has nothing to be lined
/// up against, and the result is anchored at its own bounding box, so it would
/// simply not move. There is no meaning to lose by widening it, and there is
/// one to gain: a collapse leaves its new subnet node selected, so the very
/// next alignment is a one-node selection that would otherwise do nothing.
fn layout_members(graph: &Graph, targets: &HashSet<NodeId>) -> HashSet<NodeId> {
    let movable = |id: NodeId| graph.node(id).is_some_and(|node| !node.metadata.synthetic);
    let selected: HashSet<NodeId> = targets.iter().copied().filter(|id| movable(*id)).collect();
    if selected.len() >= 2 {
        return selected;
    }
    graph
        .nodes()
        .filter(|node| !node.metadata.synthetic)
        .map(|node| node.id)
        .collect()
}

/// Longest-path depth of every member over the subgraph they induce.
///
/// The graph is a DAG by construction ([`Graph`] refuses a cycle), so the
/// relaxation below terminates; the bound is defensive only, and a graph that
/// somehow reached one still returns a usable layering instead of hanging.
fn longest_path_depths(graph: &Graph, members: &HashSet<NodeId>) -> HashMap<NodeId, usize> {
    let mut depths: HashMap<NodeId, usize> = members.iter().map(|&id| (id, 0)).collect();
    let edges: Vec<(NodeId, NodeId)> = graph
        .edges()
        .filter(|edge| members.contains(&edge.source) && members.contains(&edge.target))
        .map(|edge| (edge.source, edge.target))
        .collect();
    for _ in 0..members.len() {
        let mut moved = false;
        for &(source, target) in &edges {
            let candidate = depths[&source] + 1;
            if candidate > depths[&target] {
                depths.insert(target, candidate);
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    depths
}

/// Top-left corner of the box the members currently occupy.
fn bounding_box_origin(graph: &Graph, members: &HashSet<NodeId>) -> (f32, f32) {
    members
        .iter()
        .map(|&id| position(graph, id))
        .fold((f32::MAX, f32::MAX), |acc, p| {
            (acc.0.min(p.0), acc.1.min(p.1))
        })
}

fn position(graph: &Graph, id: NodeId) -> (f32, f32) {
    graph
        .node(id)
        .map(|node| node.metadata.position)
        .unwrap_or((0.0, 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::graph::Node;
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, OutputPortIndex};
    use std::collections::BTreeMap;

    fn id(raw: u64) -> NodeId {
        NodeId::new(raw)
    }

    fn node(raw: u64, position: (f32, f32)) -> Node {
        let mut node = Node::new(id(raw), "test.node")
            .with_input("in", &[DataTypeId::SCALAR])
            .with_output("out", DataTypeId::SCALAR);
        node.metadata.position = position;
        node
    }

    fn add(graph: Graph, node: Node) -> Graph {
        graph.add_node(node).expect("the test node is valid")
    }

    fn connect(graph: Graph, source: u64, target: u64) -> Graph {
        graph
            .add_edge(
                EdgeId::new(source * 100 + target),
                id(source),
                OutputPortIndex(0),
                id(target),
                InputPortIndex(0),
            )
            .expect("the test wiring is valid")
    }

    /// A → B → C plus an unconnected D, all overlapping at the origin.
    fn chain_graph() -> Graph {
        let mut graph = Graph::new();
        for raw in 1..=4 {
            graph = add(graph, node(raw, (0.0, 0.0)));
        }
        connect(connect(graph, 1, 2), 2, 3)
    }

    fn sizes(ids: &[u64]) -> HashMap<NodeId, (f32, f32)> {
        ids.iter().map(|&raw| (id(raw), (160.0, 60.0))).collect()
    }

    fn rects(
        layout: &HashMap<NodeId, (f32, f32)>,
        sizes: &HashMap<NodeId, (f32, f32)>,
    ) -> Vec<(f32, f32, f32, f32)> {
        layout
            .iter()
            .map(|(id, &(x, y))| {
                let (w, h) = sizes.get(id).copied().unwrap_or(FALLBACK_SIZE);
                (x, y, w, h)
            })
            .collect()
    }

    fn any_overlap(rects: &[(f32, f32, f32, f32)]) -> bool {
        rects.iter().enumerate().any(|(i, a)| {
            rects[i + 1..]
                .iter()
                .any(|b| a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3)
        })
    }

    #[test]
    fn the_same_graph_lays_out_the_same_way_twice() {
        let graph = chain_graph();
        let sizes = sizes(&[1, 2, 3, 4]);
        let first = auto_layout(&graph, &HashSet::new(), &sizes, LayoutAxis::Horizontal);
        let second = auto_layout(&graph, &HashSet::new(), &sizes, LayoutAxis::Horizontal);
        assert_eq!(first, second);
    }

    #[test]
    fn overlapping_nodes_come_apart() {
        let graph = chain_graph();
        let sizes = sizes(&[1, 2, 3, 4]);
        let before: Vec<_> = graph
            .nodes()
            .map(|n| (n.metadata.position.0, n.metadata.position.1, 160.0, 60.0))
            .collect();
        assert!(any_overlap(&before), "the fixture starts overlapping");

        let layout = auto_layout(&graph, &HashSet::new(), &sizes, LayoutAxis::Horizontal);
        assert_eq!(layout.len(), 4);
        assert!(!any_overlap(&rects(&layout, &sizes)));
    }

    /// The point of the whole unit: this computes positions, it does not
    /// apply them. The caller splices the result into the document so the move
    /// is one undo step — a function that also updated the graph would give
    /// the edit a second, untracked path in.
    #[test]
    fn the_graph_is_left_exactly_as_it_was() {
        let graph = chain_graph();
        let snapshot = |graph: &Graph| -> BTreeMap<NodeId, (f32, f32)> {
            graph
                .nodes()
                .map(|node| (node.id, node.metadata.position))
                .collect()
        };
        let before = snapshot(&graph);
        let layout = auto_layout(
            &graph,
            &HashSet::new(),
            &sizes(&[1, 2, 3, 4]),
            LayoutAxis::Horizontal,
        );
        // The fixture stacks every node on the origin, so a layout that
        // returns anything but the origin really did compute new positions —
        // without which "the graph did not change" would be vacuous.
        assert!(layout.values().any(|&point| point != (0.0, 0.0)));
        assert_eq!(snapshot(&graph), before);
    }

    /// Layers advance by the *widest* member, not by whatever node happened to
    /// come first. Equal-sized nodes cannot tell the two apart, so the sizes
    /// here differ by a factor of five and the layer-0 pair is deliberately
    /// ordered narrow-then-wide.
    #[test]
    fn nodes_of_different_sizes_still_do_not_overlap() {
        let graph = chain_graph();
        // Layer 0 is [1, 4] (equal positions, so id order), layer 1 is [2],
        // layer 2 is [3]. Node 2 is tall enough to reach node 4's band, so a
        // layer advanced by node 1's width alone lands on top of node 4.
        let sizes: HashMap<NodeId, (f32, f32)> = [
            (id(1), (100.0, 400.0)),
            (id(4), (500.0, 60.0)),
            (id(2), (150.0, 600.0)),
            (id(3), (80.0, 200.0)),
        ]
        .into_iter()
        .collect();

        let layout = auto_layout(&graph, &HashSet::new(), &sizes, LayoutAxis::Horizontal);
        assert_eq!(layout.len(), 4);
        assert!(!any_overlap(&rects(&layout, &sizes)));
        assert_eq!(
            layout,
            auto_layout(&graph, &HashSet::new(), &sizes, LayoutAxis::Horizontal),
            "uneven sizes do not make the result depend on iteration order"
        );
    }

    #[test]
    fn the_vertical_axis_stacks_layers_downwards() {
        let graph = chain_graph();
        let sizes = sizes(&[1, 2, 3, 4]);
        let layout = auto_layout(&graph, &HashSet::new(), &sizes, LayoutAxis::Vertical);
        assert!(!any_overlap(&rects(&layout, &sizes)));
        // A → B → C run down the y axis, not across the x axis.
        assert!(layout[&id(1)].1 < layout[&id(2)].1);
        assert!(layout[&id(2)].1 < layout[&id(3)].1);
        assert_eq!(layout[&id(1)].0, layout[&id(2)].0);
    }

    #[test]
    fn a_chain_is_laid_out_one_node_per_layer() {
        let graph = chain_graph();
        let sizes = sizes(&[1, 2, 3, 4]);
        let layout = auto_layout(&graph, &HashSet::new(), &sizes, LayoutAxis::Horizontal);
        assert!(layout[&id(1)].0 < layout[&id(2)].0);
        assert!(layout[&id(2)].0 < layout[&id(3)].0);
        // The unconnected node shares the source layer with A.
        assert_eq!(layout[&id(4)].0, layout[&id(1)].0);
        assert_ne!(layout[&id(4)].1, layout[&id(1)].1);
    }

    #[test]
    fn an_empty_selection_moves_every_node() {
        let graph = chain_graph();
        let layout = auto_layout(
            &graph,
            &HashSet::new(),
            &sizes(&[1, 2, 3, 4]),
            LayoutAxis::Horizontal,
        );
        let moved: HashSet<NodeId> = layout.keys().copied().collect();
        assert_eq!(moved, graph.nodes().map(|n| n.id).collect::<HashSet<_>>());
    }

    #[test]
    fn a_partial_selection_moves_only_its_own_nodes() {
        let graph = chain_graph();
        let targets: HashSet<NodeId> = [id(1), id(2)].into_iter().collect();
        let layout = auto_layout(&graph, &targets, &sizes(&[1, 2]), LayoutAxis::Horizontal);
        assert_eq!(layout.keys().copied().collect::<HashSet<_>>(), targets);
    }

    /// One node cannot be aligned against anything, so a one-node selection
    /// is the whole network — which is what makes the alignment right after a
    /// collapse (whose new subnet node is the selection) do something.
    #[test]
    fn a_single_node_selection_lays_out_the_whole_network() {
        let graph = chain_graph();
        let sizes = sizes(&[1, 2, 3, 4]);
        let targets: HashSet<NodeId> = [id(2)].into_iter().collect();
        let layout = auto_layout(&graph, &targets, &sizes, LayoutAxis::Horizontal);
        assert_eq!(layout.len(), 4);
        assert!(!any_overlap(&rects(&layout, &sizes)));
    }

    /// Ids that name nothing here cannot decide what moves: a selection
    /// published for another network resolves to "align everything".
    #[test]
    fn a_selection_of_foreign_ids_is_treated_as_no_selection() {
        let graph = chain_graph();
        let targets: HashSet<NodeId> = [id(99)].into_iter().collect();
        let layout = auto_layout(
            &graph,
            &targets,
            &sizes(&[1, 2, 3, 4]),
            LayoutAxis::Horizontal,
        );
        assert_eq!(layout.len(), 4);
        assert!(!layout.contains_key(&id(99)));
    }

    /// Synthetic nodes are not drawn, so an alignment has no position of
    /// theirs to fix.
    #[test]
    fn synthetic_nodes_are_never_moved() {
        let mut synthetic = Node::new(id(5), "test.synthetic");
        synthetic.metadata.synthetic = true;
        let graph = add(chain_graph(), synthetic);
        let layout = auto_layout(
            &graph,
            &HashSet::new(),
            &sizes(&[1, 2, 3, 4, 5]),
            LayoutAxis::Horizontal,
        );
        assert!(!layout.contains_key(&id(5)));
        let targets: HashSet<NodeId> = [id(5)].into_iter().collect();
        assert!(
            !auto_layout(&graph, &targets, &sizes(&[5]), LayoutAxis::Horizontal)
                .contains_key(&id(5))
        );
    }

    /// A diamond puts both middle nodes in one layer and the sink after them,
    /// which is what the longest path (not the shortest) gives.
    #[test]
    fn a_diamond_places_the_sink_after_both_branches() {
        let mut graph = Graph::new();
        for raw in 1..=4 {
            graph = add(graph, node(raw, (0.0, 10.0 * (raw - 1) as f32)));
        }
        let graph = connect(connect(connect(graph, 1, 2), 1, 3), 2, 4);
        let graph = connect(graph, 3, 4);
        let sizes = sizes(&[1, 2, 3, 4]);
        let layout = auto_layout(&graph, &HashSet::new(), &sizes, LayoutAxis::Horizontal);
        assert_eq!(layout[&id(2)].0, layout[&id(3)].0);
        assert!(layout[&id(3)].0 < layout[&id(4)].0);
        assert!(!any_overlap(&rects(&layout, &sizes)));
    }

    /// The moved set keeps the corner it already occupied, so aligning part of
    /// a network does not fling it across the canvas.
    #[test]
    fn the_layout_is_anchored_at_the_previous_bounding_box() {
        let graph = add(
            add(Graph::new(), node(1, (300.0, 200.0))),
            node(2, (500.0, 260.0)),
        );
        let graph = connect(graph, 1, 2);
        let layout = auto_layout(
            &graph,
            &HashSet::new(),
            &sizes(&[1, 2]),
            LayoutAxis::Horizontal,
        );
        assert_eq!(layout[&id(1)], (300.0, 200.0));
        assert_eq!(layout[&id(2)].1, 200.0);
    }

    /// A node the caller measured nothing for still gets a slot no other node
    /// overlaps.
    #[test]
    fn a_node_without_a_measurement_uses_the_fallback_size() {
        let graph = chain_graph();
        let sizes = sizes(&[1, 2, 3]);
        let layout = auto_layout(&graph, &HashSet::new(), &sizes, LayoutAxis::Horizontal);
        assert!(layout.contains_key(&id(4)));
        assert!(!any_overlap(&rects(&layout, &sizes)));
    }
}
