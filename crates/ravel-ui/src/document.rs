// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless document editing state (REQ-LAYER-009).
//!
//! [`DocumentStore`] owns the live [`Document`] and its undo stack: the
//! document snapshot is the unit of undo for every graph and composition
//! edit, so layer edits (timeline), network edits (node editor), and shell
//! property edits (properties panel) all roll back through one history.
//! Live gesture updates ([`DocumentStore::apply`], e.g. a mid-scrub value)
//! replace the current document without recording history; the
//! gesture-ending [`DocumentStore::commit`] records one undo step.
//!
//! The free functions are pure `Document → Document` transforms shared by
//! the GPUI panels: they never mutate in place (`im` structural sharing
//! keeps them cheap).

use ravel_core::composition::templates::{LayerTemplate, TemplateError};
use ravel_core::composition::{Composition, Document, Layer};
use ravel_core::graph::Graph;
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_core::registry::NodeRegistry;
use ravel_core::types::FrameRate;
use ravel_core::undo::UndoStack;

/// Ownership path of the network a node editor is looking at:
/// `CompId / LayerId / [SubnetNodeId ...]` (REQ-LAYER-011).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkPath {
    pub comp: CompId,
    pub layer: LayerId,
    /// Subnet nodes entered from the layer network, outermost first.
    pub subnets: Vec<NodeId>,
}

impl NetworkPath {
    pub fn layer(comp: CompId, layer: LayerId) -> Self {
        Self {
            comp,
            layer,
            subnets: Vec::new(),
        }
    }

    /// The path one subnet deeper.
    pub fn entered(&self, subnet: NodeId) -> Self {
        let mut subnets = self.subnets.clone();
        subnets.push(subnet);
        Self {
            comp: self.comp,
            layer: self.layer,
            subnets,
        }
    }

    /// The path truncated to `depth` subnet segments (0 = the layer network).
    pub fn truncated(&self, depth: usize) -> Self {
        Self {
            comp: self.comp,
            layer: self.layer,
            subnets: self.subnets[..depth.min(self.subnets.len())].to_vec(),
        }
    }

    /// The evaluator ownership path of this network's scope.
    pub fn segments(&self) -> Vec<ravel_core::eval::PathSegment> {
        let mut segments = vec![ravel_core::eval::PathSegment::Layer(self.comp, self.layer)];
        segments.extend(
            self.subnets
                .iter()
                .map(|id| ravel_core::eval::PathSegment::Subnet(*id)),
        );
        segments
    }
}

/// The live document plus its undo history.
pub struct DocumentStore {
    live: Document,
    undo: UndoStack<Document>,
    /// Whether `live` holds uncommitted gesture updates (`apply` since the
    /// last `commit`/`undo`/`redo`).
    dirty: bool,
}

impl DocumentStore {
    pub fn new(document: Document) -> Self {
        Self {
            live: document.clone(),
            undo: UndoStack::new(document).with_max_history(200),
            dirty: false,
        }
    }

    pub fn document(&self) -> &Document {
        &self.live
    }

    /// Replace the live document without recording history (mid-gesture
    /// updates: parameter scrubs, drag previews).
    pub fn apply(&mut self, document: Document) {
        self.live = document;
        self.dirty = true;
    }

    /// Replace the live document and record one undo step.
    pub fn commit(&mut self, document: Document) {
        self.live = document.clone();
        self.undo.push(document);
        self.dirty = false;
    }

    /// Discard uncommitted [`apply`](Self::apply) updates, restoring the
    /// last committed snapshot (cancelled gestures). Returns whether
    /// anything changed.
    pub fn revert(&mut self) -> bool {
        if !self.dirty {
            return false;
        }
        self.live = self.undo.current().clone();
        self.dirty = false;
        true
    }

    /// Roll back one step. Returns whether anything changed. A pending
    /// uncommitted [`apply`](Self::apply) is discarded first — the first
    /// undo cancels the live preview instead of skipping past the current
    /// committed snapshot.
    pub fn undo(&mut self) -> bool {
        if self.revert() {
            return true;
        }
        match self.undo.undo() {
            Some(doc) => {
                self.live = doc.clone();
                true
            }
            None => false,
        }
    }

    /// Roll forward one step. Returns whether anything changed. A pending
    /// uncommitted [`apply`](Self::apply) is discarded.
    pub fn redo(&mut self) -> bool {
        let reverted = self.revert();
        match self.undo.redo() {
            Some(doc) => {
                self.live = doc.clone();
                true
            }
            None => reverted,
        }
    }

    pub fn can_undo(&self) -> bool {
        self.undo.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.undo.can_redo()
    }
}

/// The default startup document: one empty root composition.
pub fn default_document() -> Document {
    Document::default().with_composition(Composition::new(
        CompId::next(),
        "Comp 1",
        (1920, 1080),
        FrameRate::new(30, 1),
        300,
    ))
}

/// The root composition of `doc`, if any.
pub fn root_composition(doc: &Document) -> Option<&Composition> {
    doc.root_comp
        .and_then(|id| doc.get_composition(id))
        .map(|arc| arc.as_ref())
}

/// Rebuild `doc` with composition `comp` replaced by `f(comp)`.
pub fn update_composition(
    doc: &Document,
    comp: CompId,
    f: impl FnOnce(Composition) -> Composition,
) -> Option<Document> {
    let current = doc.get_composition(comp)?.as_ref().clone();
    let mut next = doc.clone();
    next.compositions
        .insert(comp, std::sync::Arc::new(f(current)));
    Some(next)
}

/// Rebuild `doc` with layer `layer` in `comp` replaced by `f(layer)`.
pub fn update_layer(
    doc: &Document,
    comp: CompId,
    layer: LayerId,
    f: impl FnOnce(&mut Layer),
) -> Option<Document> {
    let composition = doc.get_composition(comp)?;
    let index = composition.layers.iter().position(|l| l.id == layer)?;
    update_composition(doc, comp, |mut c| {
        let mut edited = c.layers[index].clone();
        f(&mut edited);
        c.layers.set(index, edited);
        c
    })
}

/// Append a layer on top of `comp`'s stack.
pub fn add_layer(doc: &Document, comp: CompId, layer: Layer) -> Option<Document> {
    update_composition(doc, comp, |c| c.add_layer(layer))
}

/// Remove `layer` (its owned network is dropped with it, REQ-LAYER-009).
pub fn remove_layer(doc: &Document, comp: CompId, layer: LayerId) -> Option<Document> {
    update_composition(doc, comp, |c| c.remove_layer(layer))
}

/// Move `layer` to stack index `to_index` (0 = bottom).
pub fn reorder_layer(
    doc: &Document,
    comp: CompId,
    layer: LayerId,
    to_index: usize,
) -> Option<Document> {
    let composition = doc.get_composition(comp)?;
    let from = composition.layers.iter().position(|l| l.id == layer)?;
    let to = to_index.min(composition.layers.len().saturating_sub(1));
    update_composition(doc, comp, |c| c.reorder_layer(from, to))
}

/// Instantiate `template` into a fresh layer spanning the whole composition
/// and stack it on top (REQ-LAYER-008). The layer is named
/// `"{display_name} {n}"` with `n` unique within the composition.
pub fn add_layer_from_template(
    doc: &Document,
    comp: CompId,
    template: &LayerTemplate,
    registry: &NodeRegistry,
) -> Result<Option<(Document, LayerId)>, TemplateError> {
    let Some(composition) = doc.get_composition(comp) else {
        return Ok(None);
    };
    let network = template.instantiate(registry)?;
    let name = unique_layer_name(composition, &template.display_name);
    let id = LayerId::next();
    let layer = Layer::new(id, name, network).with_time(0, 0, composition.duration_frames);
    Ok(add_layer(doc, comp, layer).map(|doc| (doc, id)))
}

fn unique_layer_name(comp: &Composition, base: &str) -> String {
    let mut n = comp.layer_count() + 1;
    loop {
        let candidate = format!("{base} {n}");
        if comp.layers.iter().all(|l| l.name != candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Resolve the graph `path` points at: the layer's network, descended
/// through each subnet node's inner graph.
pub fn resolve_network<'a>(doc: &'a Document, path: &NetworkPath) -> Option<&'a Graph> {
    let layer = doc.get_composition(path.comp)?.get_layer(path.layer)?;
    let mut graph = &layer.network;
    for subnet in &path.subnets {
        graph = graph.node(*subnet)?.subnet.as_deref()?;
    }
    Some(graph)
}

/// Rebuild `doc` with the graph at `path` replaced by `network`, rebuilding
/// the nested subnet chain up to the owning layer.
pub fn replace_network(doc: &Document, path: &NetworkPath, network: Graph) -> Option<Document> {
    let layer = doc.get_composition(path.comp)?.get_layer(path.layer)?;
    let rebuilt = rebuild_subnets(&layer.network, &path.subnets, network)?;
    update_layer(doc, path.comp, path.layer, |l| l.network = rebuilt)
}

/// Replace the graph reached through `subnets` inside `graph` with `leaf`,
/// re-wrapping each ancestor subnet node on the way back up.
fn rebuild_subnets(graph: &Graph, subnets: &[NodeId], leaf: Graph) -> Option<Graph> {
    let Some((first, rest)) = subnets.split_first() else {
        return Some(leaf);
    };
    let node = graph.node(*first)?;
    let inner = node.subnet.as_deref()?;
    let new_inner = rebuild_subnets(inner, rest, leaf)?;
    let mut updated = (**node).clone();
    updated.subnet = Some(std::sync::Arc::new(new_inner));
    Some(graph.clone().replace_node(std::sync::Arc::new(updated)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::graph::Node;
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, OutputPortIndex};
    use ravel_core::network as net;
    use ravel_core::registry::builtin::register_builtins;

    fn registry() -> NodeRegistry {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        reg
    }

    fn doc_with_layers(n: u64) -> (Document, CompId) {
        let comp_id = CompId::next();
        let mut comp = Composition::new(comp_id, "Test", (16, 16), FrameRate::new(30, 1), 300);
        for i in 1..=n {
            comp = comp.add_layer(
                Layer::new(LayerId::new(i), format!("Layer {i}"), Graph::new())
                    .with_time(0, 0, 300),
            );
        }
        (Document::default().with_composition(comp), comp_id)
    }

    #[test]
    fn store_apply_does_not_record_history() {
        let (doc, comp) = doc_with_layers(1);
        let mut store = DocumentStore::new(doc);

        let live = update_layer(store.document(), comp, LayerId::new(1), |l| {
            l.start_frame = 5;
        })
        .unwrap();
        store.apply(live);
        assert!(!store.can_undo());

        let committed = update_layer(store.document(), comp, LayerId::new(1), |l| {
            l.start_frame = 10;
        })
        .unwrap();
        store.commit(committed);
        assert!(store.can_undo());

        // One undo returns to the pre-gesture state, not the live value.
        assert!(store.undo());
        let layer = root_composition(store.document())
            .unwrap()
            .get_layer(LayerId::new(1))
            .unwrap()
            .clone();
        assert_eq!(layer.start_frame, 0);
        assert!(store.redo());
    }

    /// A cancelled gesture (apply without commit) is discarded by revert /
    /// the first undo, restoring the committed snapshot instead of stepping
    /// past it.
    #[test]
    fn revert_and_undo_discard_uncommitted_live_edits() {
        let (doc, comp) = doc_with_layers(1);
        let mut store = DocumentStore::new(doc);

        let committed = update_layer(store.document(), comp, LayerId::new(1), |l| {
            l.start_frame = 10;
        })
        .unwrap();
        store.commit(committed);

        // Live preview past the committed state, then cancel.
        let live = update_layer(store.document(), comp, LayerId::new(1), |l| {
            l.start_frame = 99;
        })
        .unwrap();
        store.apply(live);
        assert!(store.revert());
        let start = |store: &DocumentStore| {
            root_composition(store.document())
                .unwrap()
                .get_layer(LayerId::new(1))
                .unwrap()
                .start_frame
        };
        assert_eq!(start(&store), 10, "revert restores the committed snapshot");
        assert!(!store.revert(), "clean store has nothing to revert");

        // Undo with a pending preview: first undo only cancels the preview.
        let live = update_layer(store.document(), comp, LayerId::new(1), |l| {
            l.start_frame = 99;
        })
        .unwrap();
        store.apply(live);
        assert!(store.undo());
        assert_eq!(start(&store), 10);
        assert!(store.undo());
        assert_eq!(start(&store), 0, "second undo steps through history");
    }

    #[test]
    fn layer_add_remove_reorder_roundtrip_through_undo() {
        let (doc, comp) = doc_with_layers(2);
        let mut store = DocumentStore::new(doc);

        let added = add_layer(
            store.document(),
            comp,
            Layer::new(LayerId::new(3), "Layer 3", Graph::new()).with_time(0, 0, 300),
        )
        .unwrap();
        store.commit(added);

        let reordered = reorder_layer(store.document(), comp, LayerId::new(3), 0).unwrap();
        store.commit(reordered);
        let ids: Vec<u64> = root_composition(store.document())
            .unwrap()
            .layers
            .iter()
            .map(|l| l.id.raw())
            .collect();
        assert_eq!(ids, [3, 1, 2]);

        let removed = remove_layer(store.document(), comp, LayerId::new(1)).unwrap();
        store.commit(removed);
        assert_eq!(root_composition(store.document()).unwrap().layer_count(), 2);

        // Roll everything back.
        assert!(store.undo());
        assert!(store.undo());
        assert!(store.undo());
        let ids: Vec<u64> = root_composition(store.document())
            .unwrap()
            .layers
            .iter()
            .map(|l| l.id.raw())
            .collect();
        assert_eq!(ids, [1, 2]);
    }

    #[test]
    fn template_layer_spans_the_composition_and_gets_a_unique_name() {
        let (doc, comp) = doc_with_layers(0);
        let template = ravel_core::composition::templates::builtin_layer_template("solid").unwrap();
        let reg = registry();

        let (doc, id) = add_layer_from_template(&doc, comp, template, &reg)
            .unwrap()
            .unwrap();
        let (doc, id2) = add_layer_from_template(&doc, comp, template, &reg)
            .unwrap()
            .unwrap();

        let comp = root_composition(&doc).unwrap();
        let layer = comp.get_layer(id).unwrap();
        assert_eq!((layer.in_frame, layer.out_frame), (0, 300));
        assert!(layer.has_frame_output());
        assert_ne!(
            comp.get_layer(id).unwrap().name,
            comp.get_layer(id2).unwrap().name
        );
    }

    #[test]
    fn network_resolution_descends_and_replaces_through_subnets() {
        // layer network: [subnet A [subnet B [constant]]]
        let constant =
            Node::new(NodeId::new(100), "constant").with_output("value", DataTypeId::SCALAR);
        let inner_b = Graph::new().add_node(constant).unwrap();
        let subnet_b = Node::new(NodeId::new(20), "subnet").with_subnet(inner_b);
        let inner_a = Graph::new().add_node(subnet_b).unwrap();
        let subnet_a = Node::new(NodeId::new(10), "subnet").with_subnet(inner_a);
        let network = Graph::new().add_node(subnet_a).unwrap();

        let comp_id = CompId::next();
        let comp = Composition::new(comp_id, "Test", (16, 16), FrameRate::new(30, 1), 300)
            .add_layer(Layer::new(LayerId::new(1), "L", network).with_time(0, 0, 300));
        let doc = Document::default().with_composition(comp);

        let path = NetworkPath::layer(comp_id, LayerId::new(1))
            .entered(NodeId::new(10))
            .entered(NodeId::new(20));
        let resolved = resolve_network(&doc, &path).unwrap();
        assert!(resolved.node(NodeId::new(100)).is_some());

        // Replace the innermost graph; ancestors are re-wrapped.
        let replacement = Graph::new()
            .add_node(Node::new(NodeId::new(101), "constant").with_output("v", DataTypeId::SCALAR))
            .unwrap();
        let doc = replace_network(&doc, &path, replacement).unwrap();
        let resolved = resolve_network(&doc, &path).unwrap();
        assert!(resolved.node(NodeId::new(100)).is_none());
        assert!(resolved.node(NodeId::new(101)).is_some());

        // Truncation walks back up the breadcrumb.
        assert_eq!(path.truncated(1).subnets, vec![NodeId::new(10)]);
        assert_eq!(path.truncated(0).subnets, Vec::<NodeId>::new());
    }

    #[test]
    fn network_path_segments_match_evaluator_scopes() {
        use ravel_core::eval::PathSegment;
        let path = NetworkPath::layer(CompId::new(1), LayerId::new(2)).entered(NodeId::new(3));
        assert_eq!(
            path.segments(),
            vec![
                PathSegment::Layer(CompId::new(1), LayerId::new(2)),
                PathSegment::Subnet(NodeId::new(3)),
            ]
        );
    }

    #[test]
    fn default_document_has_a_root_comp() {
        let doc = default_document();
        let comp = root_composition(&doc).unwrap();
        assert_eq!(comp.layer_count(), 0);
        assert_eq!(comp.resolution, (1920, 1080));
    }

    // Edge wiring survives replace_network (regression guard for the
    // rebuild path dropping edges).
    #[test]
    fn replace_network_keeps_layer_edges_intact() {
        let (doc, comp) = doc_with_layers(1);
        let a = Node::new(NodeId::new(1000), "constant").with_output("v", DataTypeId::SCALAR);
        let b = Node::new(NodeId::new(1001), net::NET_OUT_TYPE_KEY)
            .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let network = Graph::new()
            .add_node(a)
            .unwrap()
            .add_node(b)
            .unwrap()
            .add_edge(
                EdgeId::next(),
                NodeId::new(1000),
                OutputPortIndex(0),
                NodeId::new(1001),
                InputPortIndex(0),
            )
            .unwrap();

        let path = NetworkPath::layer(comp, LayerId::new(1));
        let doc = replace_network(&doc, &path, network).unwrap();
        let resolved = resolve_network(&doc, &path).unwrap();
        assert_eq!(resolved.edges().count(), 1);
    }
}
