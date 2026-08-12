// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless state and tree building for the Outliner panel (REQ-UI-013).
//!
//! The Outliner is the project-structure view: **Composition → Layer → Node**,
//! next to the Timeline's time view of the active composition. This module
//! flattens that structure into [`OutlinerRow`]s — one row per visible line,
//! carrying its indent depth, its label, and what it points at — so the GPUI
//! host paints rows top to bottom and never walks a graph inside `render()`.
//!
//! Node rows come from an upstream depth-first walk of the layer network,
//! rooted at `net.out` (the boundary node itself is not a row). A node already
//! emitted in the same network becomes a reference leaf instead of expanding
//! again: a DAG where two chains rejoin would otherwise grow exponentially.
//! Nodes the walk never reaches — the ones wired to nothing that feeds the
//! output — collect in a trailing Unused group so they stay discoverable.

use ravel_core::composition::{Composition, Document, Layer};
use ravel_core::graph::Graph;
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_core::network;
use std::collections::{HashMap, HashSet};

use crate::panel::PanelKind;

/// What an [`OutlinerRow`] points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutlinerRowKind {
    /// A composition of the document.
    Comp { comp: CompId },
    /// A layer of the composition above it.
    Layer { comp: CompId, layer: LayerId },
    /// A node of the layer network above it.
    Node {
        comp: CompId,
        layer: LayerId,
        node: NodeId,
        /// The node owns a nested network (REQ-LAYER-003). The row is a leaf
        /// with a badge — the inner network is entered in the node editor, so
        /// the Outliner keeps its Composition → Layer → Node levels. The
        /// node's *own* upstream inputs still expand below it: they live in
        /// this network, and hiding them would file them under Unused.
        subnet: bool,
        /// This node was already shown elsewhere in the same network, so the
        /// row is a leaf with a reference mark instead of a second copy of
        /// its whole upstream chain.
        reference: bool,
    },
    /// Bucket for the nodes of the layer above that `net.out` cannot reach.
    UnusedGroup {
        comp: CompId,
        layer: LayerId,
        /// How many nodes the group holds (the host labels the row with it).
        count: usize,
    },
}

/// One visible line of the Outliner tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlinerRow {
    /// Indent level: compositions are 0, their layers 1, layer nodes 2 and
    /// deeper (one level per upstream hop).
    pub depth: usize,
    pub kind: OutlinerRowKind,
    /// Display text. Empty for [`OutlinerRowKind::UnusedGroup`], whose label
    /// is localized by the host.
    pub label: String,
    /// Whether the row has children to show (draws a disclosure triangle).
    pub expandable: bool,
    /// Whether those children are currently shown.
    pub expanded: bool,
}

impl OutlinerRow {
    /// The expansion key of this row, or `None` for a leaf.
    pub fn key(&self) -> Option<OutlinerKey> {
        match self.kind {
            OutlinerRowKind::Comp { comp } => Some(OutlinerKey::Comp(comp)),
            OutlinerRowKind::Layer { comp, layer } => Some(OutlinerKey::Layer(comp, layer)),
            OutlinerRowKind::Node {
                comp,
                layer,
                node,
                reference,
                ..
            } => (!reference).then_some(OutlinerKey::Node(comp, layer, node)),
            OutlinerRowKind::UnusedGroup { comp, layer, .. } => {
                Some(OutlinerKey::Unused(comp, layer))
            }
        }
    }

    /// The composition this row belongs to.
    pub fn comp(&self) -> CompId {
        match self.kind {
            OutlinerRowKind::Comp { comp }
            | OutlinerRowKind::Layer { comp, .. }
            | OutlinerRowKind::Node { comp, .. }
            | OutlinerRowKind::UnusedGroup { comp, .. } => comp,
        }
    }

    /// The layer this row belongs to, or `None` for a composition row.
    pub fn layer(&self) -> Option<LayerId> {
        match self.kind {
            OutlinerRowKind::Comp { .. } => None,
            OutlinerRowKind::Layer { layer, .. }
            | OutlinerRowKind::Node { layer, .. }
            | OutlinerRowKind::UnusedGroup { layer, .. } => Some(layer),
        }
    }
}

/// Identity of an expandable tree row, stable across rebuilds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OutlinerKey {
    Comp(CompId),
    Layer(CompId, LayerId),
    Node(CompId, LayerId, NodeId),
    Unused(CompId, LayerId),
}

impl OutlinerKey {
    /// Whether this kind of row starts out expanded.
    ///
    /// Compositions open so their layers are visible without a click; layers
    /// stay closed so a project full of layers does not open as hundreds of
    /// node rows; an opened layer then shows its whole upstream chain at once,
    /// which is the structure the panel exists to show.
    fn default_expanded(self) -> bool {
        match self {
            OutlinerKey::Comp(_) | OutlinerKey::Node(..) => true,
            OutlinerKey::Layer(..) | OutlinerKey::Unused(..) => false,
        }
    }
}

/// Headless Outliner state: what is expanded, and the flattening of a
/// document into rows.
///
/// The panel holds no selection and no active composition — those are the
/// host's `LayerSelection` / `CanvasSelection` / `ActiveComposition` globals,
/// shared with the Timeline and the node editor (REQ-UI-013).
#[derive(Clone, Debug, Default)]
pub struct OutlinerPanel {
    /// Rows whose expansion differs from [`OutlinerKey::default_expanded`].
    /// Storing the *difference* keeps per-kind defaults working for rows the
    /// user has never touched, including ones that appear later.
    toggled: HashSet<OutlinerKey>,
}

impl OutlinerPanel {
    pub const KIND: PanelKind = PanelKind::Outliner;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_expanded(&self, key: OutlinerKey) -> bool {
        key.default_expanded() != self.toggled.contains(&key)
    }

    pub fn toggle_expanded(&mut self, key: OutlinerKey) {
        if !self.toggled.remove(&key) {
            self.toggled.insert(key);
        }
    }

    pub fn set_expanded(&mut self, key: OutlinerKey, expanded: bool) {
        if expanded == key.default_expanded() {
            self.toggled.remove(&key);
        } else {
            self.toggled.insert(key);
        }
    }

    /// Flatten `document` into the visible rows, compositions ordered by id
    /// (the document's own ordering) and layers top-most first (the Timeline's
    /// stacking order).
    pub fn rows(&self, document: &Document) -> Vec<OutlinerRow> {
        let mut comps: Vec<&Composition> = document
            .compositions
            .values()
            .map(|comp| comp.as_ref())
            .collect();
        comps.sort_by_key(|comp| comp.id);

        let mut rows = Vec::new();
        for comp in comps {
            let comp_key = OutlinerKey::Comp(comp.id);
            let expanded = self.is_expanded(comp_key);
            rows.push(OutlinerRow {
                depth: 0,
                kind: OutlinerRowKind::Comp { comp: comp.id },
                label: comp.name.clone(),
                expandable: !comp.layers.is_empty(),
                expanded,
            });
            if !expanded {
                continue;
            }
            for layer in comp.layers.iter().rev() {
                self.push_layer_rows(comp.id, layer, &mut rows);
            }
        }
        rows
    }

    fn push_layer_rows(&self, comp: CompId, layer: &Layer, rows: &mut Vec<OutlinerRow>) {
        let key = OutlinerKey::Layer(comp, layer.id);
        let expanded = self.is_expanded(key);
        rows.push(OutlinerRow {
            depth: 1,
            kind: OutlinerRowKind::Layer {
                comp,
                layer: layer.id,
            },
            label: layer.name.clone(),
            // Whether the row has an arrow, not what is behind it. This used to
            // call `network_rows`, which builds the edge map, walks the graph
            // depth-first and allocates a label `String` per node — for every
            // layer of every composition, collapsed ones included, on every
            // rebuild (`MED-UI-05`). A collapsed layer contributes no node row,
            // so all that work only ever decided this bool.
            expandable: network_has_rows(&layer.network),
            expanded,
        });
        if !expanded {
            return;
        }
        let node_rows = network_rows(&layer.network);

        // The walk order is fixed by `network_rows`; expansion only decides
        // whether a node's children survive. Skipping a collapsed node's
        // subtree needs the depth of the first row that is no longer inside
        // it, so this filter runs over the flat list rather than recursing.
        let mut collapsed_at: Option<usize> = None;
        for entry in &node_rows.reachable {
            match collapsed_at {
                Some(depth) if entry.depth > depth => continue,
                _ => collapsed_at = None,
            }
            let key = OutlinerKey::Node(comp, layer.id, entry.node);
            let expanded = !entry.reference && self.is_expanded(key);
            if entry.has_inputs && !expanded {
                collapsed_at = Some(entry.depth);
            }
            rows.push(OutlinerRow {
                depth: 2 + entry.depth,
                kind: OutlinerRowKind::Node {
                    comp,
                    layer: layer.id,
                    node: entry.node,
                    subnet: entry.subnet,
                    reference: entry.reference,
                },
                label: entry.label.clone(),
                expandable: entry.has_inputs && !entry.reference,
                expanded,
            });
        }

        if node_rows.unused.is_empty() {
            return;
        }
        let key = OutlinerKey::Unused(comp, layer.id);
        let expanded = self.is_expanded(key);
        rows.push(OutlinerRow {
            depth: 2,
            kind: OutlinerRowKind::UnusedGroup {
                comp,
                layer: layer.id,
                count: node_rows.unused.len(),
            },
            label: String::new(),
            expandable: true,
            expanded,
        });
        if !expanded {
            return;
        }
        for entry in &node_rows.unused {
            rows.push(OutlinerRow {
                depth: 3,
                kind: OutlinerRowKind::Node {
                    comp,
                    layer: layer.id,
                    node: entry.node,
                    subnet: entry.subnet,
                    reference: false,
                },
                label: entry.label.clone(),
                expandable: false,
                expanded: false,
            });
        }
    }
}

/// A node row produced by the network walk, before expansion is applied.
struct NodeEntry {
    node: NodeId,
    label: String,
    /// Hops from the output boundary (0 for `net.out`'s own inputs).
    depth: usize,
    subnet: bool,
    reference: bool,
    has_inputs: bool,
}

/// The walk result for one layer network.
struct NetworkRows {
    /// Upstream rows in depth-first order, `net.out`'s inputs first.
    reachable: Vec<NodeEntry>,
    /// Nodes no upstream walk reached, ordered by id.
    unused: Vec<NodeEntry>,
}

/// Whether `graph` would produce any node row, without producing them.
///
/// [`network_rows`] splits a network into `reachable` (everything the upstream
/// walk from `net.out` reaches) and `unused` (everything it does not). `net.out`
/// itself is the walk's root and never a row, and every other node lands in
/// exactly one of the two lists — so "some row exists" is "some node other than
/// `net.out` exists", which needs no walk, no edge map and no label.
fn network_has_rows(graph: &Graph) -> bool {
    let out = network::find_out_node(graph).map(|node| node.id);
    graph.nodes().any(|node| Some(node.id) != out)
}

/// Walk `graph` upstream from `net.out` and collect its rows.
fn network_rows(graph: &Graph) -> NetworkRows {
    let inputs = input_map(graph);
    let mut reachable = Vec::new();
    let mut visited = HashSet::new();

    if let Some(out) = network::find_out_node(graph) {
        visited.insert(out.id);
        // `net.out` is the walk's root, not a row: the layer row already
        // stands for the network's result.
        for source in inputs.get(&out.id).into_iter().flatten() {
            push_upstream(graph, &inputs, *source, 0, &mut visited, &mut reachable);
        }
    }

    let mut unused: Vec<NodeEntry> = graph
        .nodes()
        .filter(|node| !visited.contains(&node.id))
        .map(|node| NodeEntry {
            node: node.id,
            label: node_label(node),
            depth: 0,
            subnet: node.subnet.is_some(),
            reference: false,
            has_inputs: false,
        })
        .collect();
    unused.sort_by_key(|entry| entry.node);

    NetworkRows { reachable, unused }
}

fn push_upstream(
    graph: &Graph,
    inputs: &HashMap<NodeId, Vec<NodeId>>,
    node_id: NodeId,
    depth: usize,
    visited: &mut HashSet<NodeId>,
    rows: &mut Vec<NodeEntry>,
) {
    let Some(node) = graph.node(node_id) else {
        return;
    };
    let sources = inputs.get(&node_id).map(Vec::as_slice).unwrap_or_default();
    let reference = !visited.insert(node_id);
    rows.push(NodeEntry {
        node: node_id,
        label: node_label(node),
        depth,
        subnet: node.subnet.is_some(),
        reference,
        has_inputs: !sources.is_empty(),
    });
    if reference {
        return;
    }
    for source in sources {
        push_upstream(graph, inputs, *source, depth + 1, visited, rows);
    }
}

/// Upstream neighbours per node, ordered by the input port they feed.
///
/// [`Graph::inputs_of`] answers the same question but iterates an unordered
/// edge map, so its result cannot drive a stable row order; the port index is
/// also the order the node editor draws its inputs in.
fn input_map(graph: &Graph) -> HashMap<NodeId, Vec<NodeId>> {
    let mut edges: Vec<_> = graph.edges().collect();
    edges.sort_by_key(|edge| (edge.target, edge.target_port, edge.source));
    let mut map: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for edge in edges {
        map.entry(edge.target).or_default().push(edge.source);
    }
    map
}

fn node_label(node: &ravel_core::graph::Node) -> String {
    node.metadata
        .label
        .clone()
        .unwrap_or_else(|| node.type_key.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::graph::Node;
    use ravel_core::id::DataTypeId;
    use ravel_core::types::FrameRate;
    use std::sync::Arc;

    fn out_node(id: NodeId) -> Node {
        Node::new(id, network::NET_OUT_TYPE_KEY)
            .with_input(network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER])
    }

    fn image_node(id: NodeId, key: &str) -> Node {
        Node::new(id, key)
            .with_input("a", &[DataTypeId::FRAME_BUFFER])
            .with_input("b", &[DataTypeId::FRAME_BUFFER])
            .with_output("out", DataTypeId::FRAME_BUFFER)
    }

    fn connect(graph: Graph, source: NodeId, target: NodeId, port: u32) -> Graph {
        graph
            .add_edge(
                ravel_core::id::EdgeId::next(),
                source,
                ravel_core::id::OutputPortIndex(0),
                target,
                ravel_core::id::InputPortIndex(port),
            )
            .expect("edge")
    }

    /// `out ← merge ← (a, b)`: a branch, with `a` on the first input port.
    fn branching_network() -> (Graph, [NodeId; 4]) {
        let out = NodeId::next();
        let merge = NodeId::next();
        let a = NodeId::next();
        let b = NodeId::next();
        let graph = Graph::new()
            .add_node(out_node(out))
            .unwrap()
            .add_node(image_node(merge, "merge"))
            .unwrap()
            .add_node(image_node(a, "shape.rect"))
            .unwrap()
            .add_node(image_node(b, "shape.ellipse"))
            .unwrap();
        let graph = connect(graph, merge, out, 0);
        let graph = connect(graph, a, merge, 0);
        let graph = connect(graph, b, merge, 1);
        (graph, [out, merge, a, b])
    }

    fn document(comps: Vec<Composition>) -> Document {
        let mut doc = Document {
            root_comp: comps.first().map(|comp| comp.id),
            ..Document::default()
        };
        for comp in comps {
            doc.compositions.insert(comp.id, Arc::new(comp));
        }
        doc
    }

    fn comp(name: &str, layers: Vec<Layer>) -> Composition {
        let mut comp = Composition::new(
            CompId::next(),
            name,
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        );
        for layer in layers {
            comp = comp.add_layer(layer);
        }
        comp
    }

    /// `MED-UI-05`: the layer row's arrow no longer costs the network walk that
    /// used to produce it. The cheap answer must agree with the walk it replaced
    /// on every shape of network, or a layer would lose (or gain) its arrow.
    #[test]
    fn the_cheap_expandable_check_agrees_with_the_network_walk() {
        let empty = Graph::new();
        let out_only = Graph::new().add_node(out_node(NodeId::next())).unwrap();
        let orphan_only = Graph::new()
            .add_node(image_node(NodeId::next(), "shape.rect"))
            .unwrap();
        let (branching, _) = branching_network();
        let out = NodeId::next();
        let unused = NodeId::next();
        let with_unused = Graph::new()
            .add_node(out_node(out))
            .unwrap()
            .add_node(image_node(unused, "shape.rect"))
            .unwrap();

        for graph in [empty, out_only, orphan_only, branching, with_unused] {
            let walked = network_rows(&graph);
            let expected = !walked.reachable.is_empty() || !walked.unused.is_empty();
            assert_eq!(
                network_has_rows(&graph),
                expected,
                "{} nodes: reachable {}, unused {}",
                graph.nodes().count(),
                walked.reachable.len(),
                walked.unused.len()
            );
        }
    }

    /// The saving itself: a collapsed layer contributes exactly one row and no
    /// node label, however deep its network is. A rebuild that walked the
    /// network would allocate one `String` per node here.
    #[test]
    fn a_collapsed_layer_contributes_one_row_whatever_its_network() {
        let (branching, _) = branching_network();
        let layer_id = LayerId::next();
        let comp = comp("Comp 1", vec![Layer::new(layer_id, "Layer", branching)]);
        let comp_id = comp.id;
        let doc = document(vec![comp]);
        let mut panel = OutlinerPanel::new();
        panel.set_expanded(OutlinerKey::Comp(comp_id), true);
        panel.set_expanded(OutlinerKey::Layer(comp_id, layer_id), false);

        let rows = panel.rows(&doc);
        assert_eq!(rows.len(), 2, "one composition row and one layer row");
        let layer_row = &rows[1];
        assert_eq!(layer_row.label, "Layer");
        assert!(
            layer_row.expandable,
            "the arrow still says the network has nodes"
        );
        assert!(
            !layer_row.expanded,
            "and none of them became a row while collapsed"
        );
    }

    /// Rows of the single layer of a single-composition document, with the
    /// layer expanded.
    fn layer_rows(network: Graph) -> (Vec<OutlinerRow>, CompId, LayerId) {
        let layer_id = LayerId::next();
        let comp = comp("Comp 1", vec![Layer::new(layer_id, "Layer", network)]);
        let comp_id = comp.id;
        let doc = document(vec![comp]);
        let mut panel = OutlinerPanel::new();
        panel.set_expanded(OutlinerKey::Layer(comp_id, layer_id), true);
        (panel.rows(&doc), comp_id, layer_id)
    }

    fn node_labels(rows: &[OutlinerRow]) -> Vec<(usize, &str)> {
        rows.iter()
            .filter(|row| matches!(row.kind, OutlinerRowKind::Node { .. }))
            .map(|row| (row.depth, row.label.as_str()))
            .collect()
    }

    #[test]
    fn compositions_are_listed_by_id_with_layers_top_most_first() {
        let bottom = LayerId::next();
        let top = LayerId::next();
        let first = comp(
            "First",
            vec![
                Layer::new(bottom, "Bottom", Graph::new()),
                Layer::new(top, "Top", Graph::new()),
            ],
        );
        let second = comp("Second", vec![]);
        let (first_id, second_id) = (first.id, second.id);
        // Inserted in reverse so the row order cannot come from the map.
        let doc = document(vec![second, first]);

        let rows = OutlinerPanel::new().rows(&doc);
        assert_eq!(
            rows.iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["First", "Top", "Bottom", "Second"],
            "compositions sort by id; layers list top-most first"
        );
        assert_eq!(
            rows[0].kind,
            OutlinerRowKind::Comp { comp: first_id },
            "first composition row"
        );
        assert_eq!(
            rows[1].kind,
            OutlinerRowKind::Layer {
                comp: first_id,
                layer: top
            }
        );
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[3].kind, OutlinerRowKind::Comp { comp: second_id });
        assert!(!rows[3].expandable, "a composition with no layer is a leaf");
    }

    #[test]
    fn layers_stay_collapsed_until_expanded_and_compositions_start_open() {
        let layer_id = LayerId::next();
        let (network, [_out, _merge, ..]) = branching_network();
        let comp = comp("Comp 1", vec![Layer::new(layer_id, "Layer", network)]);
        let comp_id = comp.id;
        let doc = document(vec![comp]);

        let mut panel = OutlinerPanel::new();
        let rows = panel.rows(&doc);
        assert_eq!(rows.len(), 2, "composition open, layer closed");
        assert!(rows[0].expanded);
        assert!(rows[1].expandable && !rows[1].expanded);

        panel.toggle_expanded(OutlinerKey::Layer(comp_id, layer_id));
        assert!(panel.rows(&doc).len() > 2, "nodes appear");

        panel.toggle_expanded(OutlinerKey::Comp(comp_id));
        assert_eq!(
            panel.rows(&doc).len(),
            1,
            "a collapsed composition hides its layers and their nodes"
        );
    }

    #[test]
    fn node_rows_walk_upstream_from_the_out_node_in_port_order() {
        let (network, [_out, _merge, _a, _b]) = branching_network();
        let (rows, ..) = layer_rows(network);

        assert_eq!(
            node_labels(&rows),
            [(2, "merge"), (3, "shape.rect"), (3, "shape.ellipse"),],
            "net.out is not a row; branches follow input port order"
        );
        let merge = &rows[2];
        assert!(merge.expandable && merge.expanded);
        let leaf = &rows[3];
        assert!(!leaf.expandable, "a node with no input is a leaf");
    }

    #[test]
    fn a_collapsed_node_hides_its_whole_upstream_subtree() {
        let (network, [_out, merge, ..]) = branching_network();
        let layer_id = LayerId::next();
        let comp = comp("Comp 1", vec![Layer::new(layer_id, "Layer", network)]);
        let comp_id = comp.id;
        let doc = document(vec![comp]);
        let mut panel = OutlinerPanel::new();
        panel.set_expanded(OutlinerKey::Layer(comp_id, layer_id), true);
        panel.toggle_expanded(OutlinerKey::Node(comp_id, layer_id, merge));

        let rows = panel.rows(&doc);
        assert_eq!(
            node_labels(&rows),
            [(2, "merge")],
            "collapsing the branch point drops both of its inputs"
        );
        assert!(rows[2].expandable && !rows[2].expanded);
    }

    #[test]
    fn a_shared_node_is_a_reference_leaf_on_its_second_appearance() {
        // out ← merge ← (left, right), both fed by the same `source` node.
        let out = NodeId::next();
        let merge = NodeId::next();
        let left = NodeId::next();
        let right = NodeId::next();
        let source = NodeId::next();
        let graph = Graph::new()
            .add_node(out_node(out))
            .unwrap()
            .add_node(image_node(merge, "merge"))
            .unwrap()
            .add_node(image_node(left, "blur"))
            .unwrap()
            .add_node(image_node(right, "glow"))
            .unwrap()
            .add_node(image_node(source, "shape.rect"))
            .unwrap();
        let graph = connect(graph, merge, out, 0);
        let graph = connect(graph, left, merge, 0);
        let graph = connect(graph, right, merge, 1);
        let graph = connect(graph, source, left, 0);
        let graph = connect(graph, source, right, 0);

        let (rows, ..) = layer_rows(graph);
        assert_eq!(
            node_labels(&rows),
            [
                (2, "merge"),
                (3, "blur"),
                (4, "shape.rect"),
                (3, "glow"),
                (4, "shape.rect"),
            ],
            "the shared node appears under both branches"
        );

        let shared: Vec<&OutlinerRow> = rows
            .iter()
            .filter(|row| matches!(row.kind, OutlinerRowKind::Node { node, .. } if node == source))
            .collect();
        assert_eq!(shared.len(), 2);
        assert!(
            matches!(
                shared[0].kind,
                OutlinerRowKind::Node {
                    reference: false,
                    ..
                }
            ),
            "the first appearance expands"
        );
        assert!(
            matches!(
                shared[1].kind,
                OutlinerRowKind::Node {
                    reference: true,
                    ..
                }
            ),
            "the second is a reference leaf"
        );
        assert!(!shared[1].expandable);
        assert!(
            shared[1].key().is_none(),
            "a reference leaf has no expansion state of its own"
        );
    }

    #[test]
    fn a_chain_of_diamonds_cannot_expand_exponentially() {
        // Each level rejoins, so a walk without the visited set would double
        // its work per level (2^LEVELS paths through the chain).
        const LEVELS: usize = 8;
        let out = NodeId::next();
        let mut graph = Graph::new().add_node(out_node(out)).unwrap();
        let mut target = out;
        for _ in 0..LEVELS {
            let merge = NodeId::next();
            let left = NodeId::next();
            let right = NodeId::next();
            let joined = NodeId::next();
            graph = graph
                .add_node(image_node(merge, "merge"))
                .unwrap()
                .add_node(image_node(left, "blur"))
                .unwrap()
                .add_node(image_node(right, "glow"))
                .unwrap()
                .add_node(image_node(joined, "shape.rect"))
                .unwrap();
            graph = connect(graph, merge, target, 0);
            graph = connect(graph, left, merge, 0);
            graph = connect(graph, right, merge, 1);
            graph = connect(graph, joined, left, 0);
            graph = connect(graph, joined, right, 0);
            target = joined;
        }

        let (rows, ..) = layer_rows(graph);
        let nodes = node_labels(&rows).len();
        // Per level: merge, left, right, and the rejoin node twice — once
        // expanded, once as a reference leaf. Linear in the graph, not in the
        // number of paths through it.
        assert_eq!(nodes, LEVELS * 5);
        assert!(nodes < 2usize.pow(LEVELS as u32));
    }

    #[test]
    fn nodes_the_output_cannot_reach_collect_in_the_unused_group() {
        let (network, [_out, _merge, _a, _b]) = branching_network();
        let orphan_a = NodeId::next();
        let orphan_b = NodeId::next();
        // Wired to each other, but not to anything the output pulls from.
        let network = network
            .add_node(image_node(orphan_a, "orphan.a"))
            .unwrap()
            .add_node(image_node(orphan_b, "orphan.b"))
            .unwrap();
        let network = connect(network, orphan_a, orphan_b, 0);

        let layer_id = LayerId::next();
        let comp = comp("Comp 1", vec![Layer::new(layer_id, "Layer", network)]);
        let comp_id = comp.id;
        let doc = document(vec![comp]);
        let mut panel = OutlinerPanel::new();
        panel.set_expanded(OutlinerKey::Layer(comp_id, layer_id), true);

        let rows = panel.rows(&doc);
        let group = rows.last().expect("rows");
        assert_eq!(
            group.kind,
            OutlinerRowKind::UnusedGroup {
                comp: comp_id,
                layer: layer_id,
                count: 2
            },
            "the group closes the layer and counts its nodes"
        );
        assert!(group.expandable && !group.expanded, "collapsed by default");
        assert!(
            group.label.is_empty(),
            "the group label is localized by the host"
        );

        panel.toggle_expanded(OutlinerKey::Unused(comp_id, layer_id));
        let rows = panel.rows(&doc);
        assert_eq!(
            node_labels(&rows),
            [
                (2, "merge"),
                (3, "shape.rect"),
                (3, "shape.ellipse"),
                (3, "orphan.a"),
                (3, "orphan.b"),
            ],
            "unused nodes are flat leaves under the group"
        );
        assert!(rows.last().is_some_and(|row| !row.expandable));
    }

    #[test]
    fn a_network_without_an_out_node_lists_everything_as_unused() {
        let node = NodeId::next();
        let network = Graph::new().add_node(image_node(node, "blur")).unwrap();
        let (rows, ..) = layer_rows(network);
        assert!(matches!(
            rows[2].kind,
            OutlinerRowKind::UnusedGroup { count: 1, .. }
        ));
    }

    #[test]
    fn a_subnet_node_is_badged_and_keeps_its_own_upstream_inputs() {
        let out = NodeId::next();
        let subnet_id = NodeId::next();
        let feeder = NodeId::next();
        let inner = Graph::new()
            .add_node(image_node(NodeId::next(), "inner.blur"))
            .unwrap();
        let subnet = image_node(subnet_id, "subnet").with_subnet(inner);
        let graph = Graph::new()
            .add_node(out_node(out))
            .unwrap()
            .add_node(subnet)
            .unwrap()
            .add_node(image_node(feeder, "shape.rect"))
            .unwrap();
        let graph = connect(graph, subnet_id, out, 0);
        let graph = connect(graph, feeder, subnet_id, 0);

        let (rows, ..) = layer_rows(graph);
        assert!(
            matches!(
                rows[2].kind,
                OutlinerRowKind::Node { node, subnet: true, .. } if node == subnet_id
            ),
            "the subnet node carries a badge"
        );
        assert_eq!(
            node_labels(&rows),
            [(2, "subnet"), (3, "shape.rect")],
            "the inner network is not flattened into the tree, but the \
             subnet's own upstream input is not hidden either"
        );
    }

    #[test]
    fn a_labelled_node_shows_its_label_instead_of_its_type() {
        let out = NodeId::next();
        let node = NodeId::next();
        let graph = Graph::new()
            .add_node(out_node(out))
            .unwrap()
            .add_node(image_node(node, "blur").with_label("Soft edge"))
            .unwrap();
        let graph = connect(graph, node, out, 0);

        let (rows, ..) = layer_rows(graph);
        assert_eq!(node_labels(&rows), [(2, "Soft edge")]);
    }

    #[test]
    fn expansion_is_stored_as_a_difference_from_the_per_kind_default() {
        let comp_id = CompId::next();
        let layer_id = LayerId::next();
        let mut panel = OutlinerPanel::new();
        assert!(panel.is_expanded(OutlinerKey::Comp(comp_id)));
        assert!(!panel.is_expanded(OutlinerKey::Layer(comp_id, layer_id)));

        panel.set_expanded(OutlinerKey::Comp(comp_id), true);
        assert!(
            panel.toggled.is_empty(),
            "setting the default must not record a difference"
        );

        panel.set_expanded(OutlinerKey::Layer(comp_id, layer_id), true);
        assert!(panel.is_expanded(OutlinerKey::Layer(comp_id, layer_id)));
        panel.set_expanded(OutlinerKey::Layer(comp_id, layer_id), false);
        assert!(
            panel.toggled.is_empty(),
            "returning to the default must not leak an entry"
        );
    }

    #[test]
    fn an_empty_document_has_no_rows() {
        assert!(OutlinerPanel::new().rows(&Document::default()).is_empty());
    }
}
