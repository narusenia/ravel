// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Layer-network Composition model (REQ-LAYER-001).
//!
//! Each timeline layer is a **shell** (generic properties: time placement,
//! built-in transform, opacity, blend mode, parenting, adjustment flag) plus
//! **one owned node network** (a [`Graph`]) that generates the layer's
//! appearance — the Houdini-style "one layer = one network" model. The old
//! `LayerSource` structural split is gone: layer "kinds" are merely creation
//! templates that stamp an initial network (REQ-LAYER-008).
//!
//! Compositions are stored in the document as
//! `im::HashMap<CompId, Arc<Composition>>` alongside the main `Graph`,
//! enabling structural sharing for undo.

pub mod compile;
pub mod templates;
pub mod validate;

use crate::animation::channel::AnimationChannel;
use crate::eval::PathSegment;
use crate::graph::Graph;
use crate::id::{CompId, EdgeId, LayerId, NodeId};
use crate::types::{Color, FrameRate};
use serde::{Deserialize, Serialize};

// ===========================================================================
// BlendMode (layer compositing)
// ===========================================================================

/// Compositing blend mode for a layer.
///
/// Distinct from [`crate::animation::blend::BlendMode`] which blends scalar
/// animation channel values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    #[default]
    Normal,
    Add,
    Multiply,
    Screen,
    Overlay,
}

// ===========================================================================
// TrackMatte (reserved, v2)
// ===========================================================================

/// Reserved for the v2 track-matte feature: use another layer's alpha or
/// luminance as this layer's matte. Never evaluated yet; the field exists so
/// the persistence format stays compatible (REQ-LAYER-001 cross-cutting
/// reserved-field policy).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackMatte {
    /// Layer providing the matte.
    pub layer: LayerId,
    /// Matte channel interpretation.
    pub kind: TrackMatteKind,
}

/// How the matte layer's pixels are interpreted (reserved, v2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackMatteKind {
    Alpha,
    Luma,
}

// ===========================================================================
// LayerTransform
// ===========================================================================

/// Built-in transform properties on a layer, each an independently
/// animatable channel.
///
/// Vec2 properties are stored as `[AnimationChannel; 2]` (x, y components)
/// since [`AnimationChannel`] evaluates to `f32`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerTransform {
    pub anchor_point: [AnimationChannel; 2],
    pub position: [AnimationChannel; 2],
    pub scale: [AnimationChannel; 2],
    pub rotation: AnimationChannel,
}

impl Default for LayerTransform {
    fn default() -> Self {
        Self {
            anchor_point: [
                AnimationChannel::constant(0.0),
                AnimationChannel::constant(0.0),
            ],
            position: [
                AnimationChannel::constant(0.0),
                AnimationChannel::constant(0.0),
            ],
            scale: [
                AnimationChannel::constant(1.0),
                AnimationChannel::constant(1.0),
            ],
            rotation: AnimationChannel::constant(0.0),
        }
    }
}

// ===========================================================================
// Layer
// ===========================================================================

/// A single layer within a [`Composition`]: a shell plus one owned network.
///
/// Layers are ordered bottom-to-top in the composition's `layers` vector
/// (index 0 = bottommost, rendered first).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    /// The layer's owned node network (REQ-LAYER-009). Expected to contain
    /// one `net.in` and one `net.out` node (see [`crate::network`]).
    pub network: Graph,
    /// Position on the composition timeline (can be negative).
    pub start_frame: i64,
    /// Source-local display start frame.
    pub in_frame: u64,
    /// Source-local display end frame (half-open: `[in, out)`).
    pub out_frame: u64,
    pub transform: LayerTransform,
    pub opacity: AnimationChannel,
    pub blend_mode: BlendMode,
    /// Adjustment layer: the network receives the composited lower stack on
    /// its `net.in` `source` port and the result replaces the background
    /// (REQ-LAYER-010).
    pub adjustment: bool,
    pub solo: bool,
    pub muted: bool,
    pub locked: bool,
    /// Parent layer for transform inheritance (P/R/S only; not opacity/blend).
    pub parent: Option<LayerId>,
    /// Reserved for v2 time remapping (never evaluated yet).
    pub time_remap: Option<AnimationChannel>,
    /// Reserved for v2 track mattes (never evaluated yet).
    pub track_matte: Option<TrackMatte>,
}

impl Layer {
    pub fn new(id: LayerId, name: impl Into<String>, network: Graph) -> Self {
        Self {
            id,
            name: name.into(),
            network,
            start_frame: 0,
            in_frame: 0,
            out_frame: 0,
            transform: LayerTransform::default(),
            opacity: AnimationChannel::constant(1.0),
            blend_mode: BlendMode::default(),
            adjustment: false,
            solo: false,
            muted: false,
            locked: false,
            parent: None,
            time_remap: None,
            track_matte: None,
        }
    }

    /// Duration of the visible portion in frames.
    pub fn duration(&self) -> u64 {
        self.out_frame.saturating_sub(self.in_frame)
    }

    /// End frame on the composition timeline.
    pub fn end_frame(&self) -> i64 {
        self.start_frame + self.duration() as i64
    }

    /// Whether the network exposes a `frame` output for the shell's
    /// compositing chain. Layers without one are "null" layers: they never
    /// join the merge chain and are consumed only via Layer Ref
    /// (REQ-LAYER-005).
    pub fn has_frame_output(&self) -> bool {
        crate::network::find_out_node(&self.network)
            .and_then(|out| crate::network::frame_port_index(out))
            .is_some()
    }

    pub fn with_time(mut self, start: i64, in_frame: u64, out_frame: u64) -> Self {
        self.start_frame = start;
        self.in_frame = in_frame;
        self.out_frame = out_frame;
        self
    }

    pub fn with_blend_mode(mut self, mode: BlendMode) -> Self {
        self.blend_mode = mode;
        self
    }

    pub fn with_parent(mut self, parent: LayerId) -> Self {
        self.parent = Some(parent);
        self
    }
}

// ===========================================================================
// Composition
// ===========================================================================

/// An AE-style composition: an ordered stack of layers with shared
/// resolution, frame rate, and duration.
///
/// Layers are ordered bottom-to-top: index 0 is composited first (bottom).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Composition {
    pub id: CompId,
    pub name: String,
    pub resolution: (u32, u32),
    pub frame_rate: FrameRate,
    pub duration_frames: u64,
    pub layers: im::Vector<Layer>,
    pub background_color: Color,
}

impl Composition {
    pub fn new(
        id: CompId,
        name: impl Into<String>,
        resolution: (u32, u32),
        frame_rate: FrameRate,
        duration_frames: u64,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            resolution,
            frame_rate,
            duration_frames,
            layers: im::Vector::new(),
            background_color: Color::BLACK,
        }
    }

    pub fn add_layer(mut self, layer: Layer) -> Self {
        self.layers.push_back(layer);
        self
    }

    pub fn insert_layer(mut self, index: usize, layer: Layer) -> Self {
        self.layers.insert(index, layer);
        self
    }

    pub fn remove_layer(mut self, id: LayerId) -> Self {
        self.layers.retain(|l| l.id != id);
        self
    }

    pub fn get_layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// Move a layer from `from_index` to `to_index` in the compositing order.
    pub fn reorder_layer(mut self, from_index: usize, to_index: usize) -> Self {
        if from_index < self.layers.len() && to_index < self.layers.len() {
            let layer = self.layers.remove(from_index);
            self.layers.insert(to_index, layer);
        }
        self
    }
}

// ===========================================================================
// MediaAssetEntry
// ===========================================================================

/// A resolved media asset available to nodes during evaluation (the `video`
/// node's `asset_id` parameter indexes this table). The host application owns
/// asset reference bookkeeping (relative paths, proxies, hashes — see the
/// data-model spec); the document carries only the evaluation-time view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaAssetEntry {
    /// Absolute path of the media file on disk.
    pub path: std::path::PathBuf,
}

// ===========================================================================
// Document
// ===========================================================================

/// Unified document snapshot containing the node graph and all compositions.
///
/// This is the unit of undo: `UndoStack<Document>` captures both the DAG
/// and the composition map in a single structurally-shared snapshot.
///
/// The whole document serializes deterministically (id-sorted maps) so RON
/// persistence stays diff-friendly; `graph` (the legacy flat graph) is
/// serialized as-is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub graph: Graph,
    #[serde(with = "compositions_serde")]
    pub compositions: im::HashMap<CompId, std::sync::Arc<Composition>>,
    pub root_comp: Option<CompId>,
    /// Media assets by id, resolved for evaluation (REQ-LAYER-008).
    #[serde(with = "media_assets_serde")]
    pub media_assets: im::HashMap<String, MediaAssetEntry>,
}

/// Serde adapter for `im::HashMap<CompId, Arc<Composition>>` (same pattern as
/// `graph::subnet_serde`: serde's `Arc` support needs the `rc` feature).
/// Serialized as a `CompId`-sorted `Vec<(CompId, Composition)>` so the output
/// is deterministic and diff-friendly.
mod compositions_serde {
    use super::Composition;
    use crate::id::CompId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;

    pub fn serialize<S: Serializer>(
        value: &im::HashMap<CompId, Arc<Composition>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut entries: Vec<(CompId, &Composition)> = value
            .iter()
            .map(|(id, comp)| (*id, comp.as_ref()))
            .collect();
        entries.sort_by_key(|(id, _)| *id);
        entries.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<im::HashMap<CompId, Arc<Composition>>, D::Error> {
        let entries = Vec::<(CompId, Composition)>::deserialize(deserializer)?;
        Ok(entries
            .into_iter()
            .map(|(id, comp)| (id, Arc::new(comp)))
            .collect())
    }
}

/// Serde adapter for `im::HashMap<String, MediaAssetEntry>`: serialized as a
/// key-sorted `Vec<(String, MediaAssetEntry)>` so the output is deterministic
/// and diff-friendly.
mod media_assets_serde {
    use super::MediaAssetEntry;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(
        value: &im::HashMap<String, MediaAssetEntry>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut entries: Vec<(&str, &MediaAssetEntry)> = value
            .iter()
            .map(|(id, entry)| (id.as_str(), entry))
            .collect();
        entries.sort_by_key(|(id, _)| *id);
        entries.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<im::HashMap<String, MediaAssetEntry>, D::Error> {
        let entries = Vec::<(String, MediaAssetEntry)>::deserialize(deserializer)?;
        Ok(entries.into_iter().collect())
    }
}

/// The largest raw id of each kind used in a [`Document`], as reported by
/// [`Document::id_watermarks`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdWatermarks {
    pub node: u64,
    pub edge: u64,
    pub comp: u64,
    pub layer: u64,
}

/// A structural invariant violation found by [`Document::validate`]
/// (deserialized documents are rejected with this before use).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DocumentValidationError {
    #[error("root composition {0} is missing from the compositions map")]
    MissingRoot(CompId),
    #[error("composition map key {key} does not match its embedded id {embedded}")]
    CompIdMismatch { key: CompId, embedded: CompId },
    #[error("composition {0} has a zero frame-rate component")]
    InvalidFrameRate(CompId),
    #[error("composition {comp} contains duplicate layer id {layer}")]
    DuplicateLayerId { comp: CompId, layer: LayerId },
    #[error("layer {layer} references a missing {kind} layer {target}")]
    DanglingLayerRef {
        comp: CompId,
        layer: LayerId,
        kind: &'static str,
        target: LayerId,
    },
    #[error("node id {0} appears in more than one graph of the document")]
    DuplicateNodeId(NodeId),
    #[error("a persisted {kind} id equals u64::MAX and cannot have a successor")]
    IdExhausted { kind: &'static str },
}

/// Node ids must be document-globally unique (REQ-LAYER-009): processors
/// are registered by bare `NodeId`, so duplicates across graphs would alias
/// one registration.
fn check_unique_node_ids(
    graph: &Graph,
    seen: &mut std::collections::HashSet<NodeId>,
) -> Result<(), DocumentValidationError> {
    for node in graph.nodes() {
        if !seen.insert(node.id) {
            return Err(DocumentValidationError::DuplicateNodeId(node.id));
        }
        if let Some(subnet) = &node.subnet {
            check_unique_node_ids(subnet, seen)?;
        }
    }
    Ok(())
}

impl Document {
    pub fn new(graph: Graph) -> Self {
        Self {
            graph,
            compositions: im::HashMap::new(),
            root_comp: None,
            media_assets: im::HashMap::new(),
        }
    }

    pub fn with_composition(mut self, comp: Composition) -> Self {
        let id = comp.id;
        self.compositions.insert(id, std::sync::Arc::new(comp));
        if self.root_comp.is_none() {
            self.root_comp = Some(id);
        }
        self
    }

    pub fn with_media_asset(
        mut self,
        id: impl Into<String>,
        path: impl Into<std::path::PathBuf>,
    ) -> Self {
        self.media_assets
            .insert(id.into(), MediaAssetEntry { path: path.into() });
        self
    }

    pub fn get_composition(&self, id: CompId) -> Option<&std::sync::Arc<Composition>> {
        self.compositions.get(&id)
    }

    pub fn get_media_asset(&self, id: &str) -> Option<&MediaAssetEntry> {
        self.media_assets.get(id)
    }

    /// Network ownership paths whose contents changed between `old` and
    /// `self` (REQ-LAYER-007/009).
    ///
    /// Used to invalidate scoped evaluation caches after an edit: each
    /// returned prefix is `[PathSegment::Layer(comp, layer)]` of a layer
    /// whose network differs (added layers and layers in added compositions
    /// are included). Comparisons use `Arc` pointer equality first, so
    /// untouched compositions cost nothing.
    pub fn changed_network_paths(&self, old: &Document) -> Vec<Vec<PathSegment>> {
        let mut changed = Vec::new();
        for (comp_id, comp) in &self.compositions {
            match old.compositions.get(comp_id) {
                Some(old_comp) if std::sync::Arc::ptr_eq(comp, old_comp) => {}
                Some(old_comp) => {
                    for layer in &comp.layers {
                        let layer_changed = old_comp
                            .layers
                            .iter()
                            .find(|l| l.id == layer.id)
                            .map(|old_layer| old_layer.network != layer.network)
                            .unwrap_or(true);
                        if layer_changed {
                            changed.push(vec![PathSegment::Layer(*comp_id, layer.id)]);
                        }
                    }
                }
                None => {
                    for layer in &comp.layers {
                        changed.push(vec![PathSegment::Layer(*comp_id, layer.id)]);
                    }
                }
            }
        }
        changed
    }

    /// The largest id of each kind used anywhere in the document
    /// (compositions — map keys and embedded ids alike — layers, every
    /// network recursively including subnets, `layer.ref` parameter
    /// targets, and the legacy flat graph). Reference ids are included so a
    /// fresh allocation can never retarget a persisted reference
    /// (REQ-LAYER-009). No node parameter carries a `CompId` yet (PreComp
    /// is v2), so there is nothing composition-valued to scan.
    pub fn id_watermarks(&self) -> IdWatermarks {
        fn scan_graph(graph: &Graph, watermarks: &mut IdWatermarks) {
            for node in graph.nodes() {
                watermarks.node = watermarks.node.max(node.id.raw());
                if let Some(subnet) = &node.subnet {
                    scan_graph(subnet, watermarks);
                }
            }
            for edge in graph.edges() {
                watermarks.edge = watermarks.edge.max(edge.id.raw());
            }
            // `layer.ref` parameters reference layers by raw id, in any
            // graph (layer networks, subnets, and the legacy flat graph).
            let mut targets = Vec::new();
            validate::layer_ref_targets(graph, &mut targets);
            for target in targets {
                watermarks.layer = watermarks.layer.max(target.raw());
            }
        }

        let mut watermarks = IdWatermarks::default();
        scan_graph(&self.graph, &mut watermarks);
        if let Some(root) = self.root_comp {
            watermarks.comp = watermarks.comp.max(root.raw());
        }
        for (comp_id, comp) in &self.compositions {
            watermarks.comp = watermarks.comp.max(comp_id.raw()).max(comp.id.raw());
            for layer in &comp.layers {
                watermarks.layer = watermarks.layer.max(layer.id.raw());
                if let Some(parent) = layer.parent {
                    watermarks.layer = watermarks.layer.max(parent.raw());
                }
                if let Some(matte) = &layer.track_matte {
                    watermarks.layer = watermarks.layer.max(matte.layer.raw());
                }
                scan_graph(&layer.network, &mut watermarks);
            }
        }
        watermarks
    }

    /// Advance all four global id counters past the document's watermarks
    /// (call after loading a persisted document, REQ-LAYER-009).
    pub fn advance_id_counters(&self) {
        let watermarks = self.id_watermarks();
        NodeId::advance_counter_past(watermarks.node);
        EdgeId::advance_counter_past(watermarks.edge);
        CompId::advance_counter_past(watermarks.comp);
        LayerId::advance_counter_past(watermarks.layer);
    }

    /// Structural validation of a deserialized document: the invariants
    /// serde cannot express (REQ-LAYER-009). Returns the first violation
    /// found; a valid document yields `Ok(())`.
    ///
    /// Checked: the root comp exists, composition map keys match the
    /// embedded ids, frame rates have no zero component (playback divides
    /// by them), layer ids are unique per composition, parent/track-matte
    /// references resolve, and no id equals `u64::MAX` (it could not have a
    /// successor). `layer.ref` network parameters are intentionally NOT
    /// checked — a reference may legitimately dangle after its target is
    /// deleted and errors at evaluation time instead.
    pub fn validate(&self) -> Result<(), DocumentValidationError> {
        if let Some(root) = self.root_comp
            && !self.compositions.contains_key(&root)
        {
            return Err(DocumentValidationError::MissingRoot(root));
        }
        for (comp_id, comp) in &self.compositions {
            if *comp_id != comp.id {
                return Err(DocumentValidationError::CompIdMismatch {
                    key: *comp_id,
                    embedded: comp.id,
                });
            }
            if comp.frame_rate.num == 0 || comp.frame_rate.den == 0 {
                return Err(DocumentValidationError::InvalidFrameRate(*comp_id));
            }
            let mut seen = std::collections::HashSet::new();
            for layer in &comp.layers {
                if !seen.insert(layer.id) {
                    return Err(DocumentValidationError::DuplicateLayerId {
                        comp: *comp_id,
                        layer: layer.id,
                    });
                }
            }
            for layer in &comp.layers {
                if let Some(parent) = layer.parent
                    && !seen.contains(&parent)
                {
                    return Err(DocumentValidationError::DanglingLayerRef {
                        comp: *comp_id,
                        layer: layer.id,
                        kind: "parent",
                        target: parent,
                    });
                }
                if let Some(matte) = &layer.track_matte
                    && !seen.contains(&matte.layer)
                {
                    return Err(DocumentValidationError::DanglingLayerRef {
                        comp: *comp_id,
                        layer: layer.id,
                        kind: "track matte",
                        target: matte.layer,
                    });
                }
            }
        }
        // Node ids are document-globally unique (REQ-LAYER-009), across the
        // flat graph and every layer network (subnets included).
        let mut node_ids = std::collections::HashSet::new();
        check_unique_node_ids(&self.graph, &mut node_ids)?;
        for comp in self.compositions.values() {
            for layer in &comp.layers {
                check_unique_node_ids(&layer.network, &mut node_ids)?;
            }
        }
        let watermarks = self.id_watermarks();
        for (kind, raw) in [
            ("node", watermarks.node),
            ("edge", watermarks.edge),
            ("comp", watermarks.comp),
            ("layer", watermarks.layer),
        ] {
            if raw == u64::MAX {
                return Err(DocumentValidationError::IdExhausted { kind });
            }
        }
        Ok(())
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new(Graph::new())
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{CompId, LayerId};
    use crate::types::FrameRate;

    fn test_comp() -> Composition {
        Composition::new(
            CompId::new(1),
            "Test Comp",
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        )
    }

    fn empty_layer(id: u64) -> Layer {
        Layer::new(LayerId::new(id), format!("Layer {id}"), Graph::new()).with_time(0, 0, 300)
    }

    fn keyframed_channel(keys: &[(u64, f32)]) -> AnimationChannel {
        let mut curve = crate::animation::curve::KeyframeCurve::new();
        for &(frame, value) in keys {
            curve.insert(
                frame,
                value,
                crate::animation::interpolation::Interpolation::Linear,
            );
        }
        AnimationChannel::keyframes(curve)
    }

    #[test]
    fn composition_add_remove_layers() {
        let comp = test_comp()
            .add_layer(empty_layer(1))
            .add_layer(empty_layer(2))
            .add_layer(empty_layer(3));
        assert_eq!(comp.layer_count(), 3);

        let comp = comp.remove_layer(LayerId::new(2));
        assert_eq!(comp.layer_count(), 2);
        assert!(comp.get_layer(LayerId::new(2)).is_none());
        assert!(comp.get_layer(LayerId::new(1)).is_some());
        assert!(comp.get_layer(LayerId::new(3)).is_some());
    }

    #[test]
    fn layer_duration_and_end_frame() {
        let layer = empty_layer(1).with_time(10, 5, 100);
        assert_eq!(layer.duration(), 95);
        assert_eq!(layer.end_frame(), 105);
    }

    #[test]
    fn layer_negative_start_frame() {
        let layer = empty_layer(1).with_time(-30, 0, 60);
        assert_eq!(layer.start_frame, -30);
        assert_eq!(layer.end_frame(), 30);
    }

    #[test]
    fn composition_reorder() {
        let comp = test_comp()
            .add_layer(empty_layer(1))
            .add_layer(empty_layer(2))
            .add_layer(empty_layer(3));

        let comp = comp.reorder_layer(0, 2);
        assert_eq!(comp.layers[0].id, LayerId::new(2));
        assert_eq!(comp.layers[1].id, LayerId::new(3));
        assert_eq!(comp.layers[2].id, LayerId::new(1));
    }

    #[test]
    fn composition_insert_layer() {
        let comp = test_comp()
            .add_layer(empty_layer(1))
            .add_layer(empty_layer(3));

        let comp = comp.insert_layer(1, empty_layer(2));
        assert_eq!(comp.layers[0].id, LayerId::new(1));
        assert_eq!(comp.layers[1].id, LayerId::new(2));
        assert_eq!(comp.layers[2].id, LayerId::new(3));
    }

    #[test]
    fn blend_mode_default() {
        assert_eq!(BlendMode::default(), BlendMode::Normal);
    }

    #[test]
    fn layer_reserved_fields_default_to_none() {
        let layer = empty_layer(1);
        assert!(layer.time_remap.is_none());
        assert!(layer.track_matte.is_none());
        assert!(!layer.adjustment);
    }

    #[test]
    fn layer_has_frame_output_detection() {
        use crate::id::{DataTypeId, NodeId};
        // Empty network: no Out node → no frame output (null layer).
        assert!(!empty_layer(1).has_frame_output());

        // Network with an Out node carrying a `frame` input.
        let out = crate::graph::Node::new(NodeId::new(2), crate::network::NET_OUT_TYPE_KEY)
            .with_input(crate::network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let network = Graph::new().add_node(out).unwrap();
        let layer = Layer::new(LayerId::new(3), "Solid", network);
        assert!(layer.has_frame_output());
    }

    #[test]
    fn layer_parenting() {
        let parent = empty_layer(1);
        let child = empty_layer(2).with_parent(parent.id);
        assert_eq!(child.parent, Some(LayerId::new(1)));
    }

    #[test]
    fn document_composition_management() {
        let comp = test_comp();
        let doc = Document::default().with_composition(comp);
        assert!(doc.get_composition(CompId::new(1)).is_some());
        assert_eq!(doc.root_comp, Some(CompId::new(1)));
    }

    #[test]
    fn composition_structural_sharing() {
        let comp = test_comp().add_layer(empty_layer(1));
        let comp_clone = comp.clone();
        assert_eq!(comp.layers.len(), comp_clone.layers.len());
    }

    #[test]
    fn layer_default_transform() {
        let layer = empty_layer(1);
        let ctx = crate::eval::EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080));
        assert!((layer.transform.position[0].evaluate(0, &ctx) - 0.0).abs() < f32::EPSILON);
        assert!((layer.transform.scale[0].evaluate(0, &ctx) - 1.0).abs() < f32::EPSILON);
        assert!((layer.transform.rotation.evaluate(0, &ctx) - 0.0).abs() < f32::EPSILON);
        assert!((layer.opacity.evaluate(0, &ctx) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn changed_network_paths_detects_edits() {
        use crate::id::{DataTypeId, NodeId};

        let doc1 = Document::default().with_composition(
            test_comp()
                .add_layer(empty_layer(1))
                .add_layer(empty_layer(2)),
        );

        // No change (same Arc) → no paths.
        let doc2 = doc1.clone();
        assert!(doc2.changed_network_paths(&doc1).is_empty());

        // Edit layer 2's network → exactly one path.
        let comp = doc1
            .get_composition(CompId::new(1))
            .unwrap()
            .as_ref()
            .clone();
        let node = crate::graph::Node::new(NodeId::new(10), "constant")
            .with_output("value", DataTypeId::SCALAR);
        let new_layers: im::Vector<Layer> = comp
            .layers
            .iter()
            .map(|l| {
                if l.id == LayerId::new(2) {
                    let mut l = l.clone();
                    l.network = Graph::new().add_node(node.clone()).unwrap();
                    l
                } else {
                    l.clone()
                }
            })
            .collect();
        let comp = Composition {
            layers: new_layers,
            ..comp
        };
        let doc3 = Document::default().with_composition(comp);

        let paths = doc3.changed_network_paths(&doc1);
        assert_eq!(
            paths,
            vec![vec![PathSegment::Layer(CompId::new(1), LayerId::new(2))]]
        );
    }

    #[test]
    fn document_ron_roundtrip_is_deterministic() {
        use crate::graph::Node;
        use crate::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};

        // Layer network containing a subnet node with its own nested graph.
        let inner = Graph::new()
            .add_node(
                Node::new(NodeId::new(101), "constant").with_output("value", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(102), "passthrough")
                    .with_input("in", &[DataTypeId::SCALAR])
                    .with_output("out", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(103),
                NodeId::new(101),
                OutputPortIndex(0),
                NodeId::new(102),
                InputPortIndex(0),
            )
            .unwrap();
        let network = Graph::new()
            .add_node(
                Node::new(NodeId::new(100), crate::network::NET_OUT_TYPE_KEY)
                    .with_input(crate::network::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
            )
            .unwrap()
            .add_node(Node::new(NodeId::new(104), "subnet").with_subnet(inner))
            .unwrap();

        // A fully-dressed layer: keyframed transform/opacity channels,
        // reserved fields set (time_remap, track_matte), adjustment + parent.
        let hero = Layer::new(LayerId::new(11), "Hero", network)
            .with_time(-10, 5, 120)
            .with_blend_mode(BlendMode::Multiply)
            .with_parent(LayerId::new(12));
        let hero = Layer {
            transform: LayerTransform {
                position: [
                    keyframed_channel(&[(0, 0.0), (24, 100.0)]),
                    AnimationChannel::constant(-4.0),
                ],
                scale: [
                    keyframed_channel(&[(0, 1.0), (12, 2.0)]),
                    AnimationChannel::constant(1.0),
                ],
                ..LayerTransform::default()
            },
            opacity: keyframed_channel(&[(0, 0.0), (30, 1.0)]),
            adjustment: true,
            solo: true,
            time_remap: Some(keyframed_channel(&[(0, 0.0), (60, 60.0)])),
            track_matte: Some(TrackMatte {
                layer: LayerId::new(12),
                kind: TrackMatteKind::Luma,
            }),
            ..hero
        };
        let matte_layer = empty_layer(12).with_time(0, 0, 300);

        let comp = test_comp().add_layer(hero).add_layer(matte_layer);

        // Legacy flat graph (still serialized as-is).
        let flat = Graph::new()
            .add_node(
                Node::new(NodeId::new(1), "constant").with_output("value", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(2), "grade")
                    .with_input("in", &[DataTypeId::SCALAR])
                    .with_output("out", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();

        let doc = Document::new(flat)
            .with_composition(comp)
            .with_media_asset("plate", "/tmp/media/plate.mov")
            .with_media_asset("audio", "/tmp/media/mix.wav");

        let text = ron::to_string(&doc).unwrap();
        let restored: Document = ron::from_str(&text).unwrap();
        assert_eq!(doc, restored);

        // Diff-friendly persistence: serializing twice is byte-identical.
        assert_eq!(text, ron::to_string(&doc).unwrap());
    }

    #[test]
    fn id_watermarks_scan_networks_subnets_and_flat_graph() {
        use crate::graph::Node;
        use crate::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};

        // The largest node id lives inside the subnet's inner graph.
        let inner = Graph::new()
            .add_node(
                Node::new(NodeId::new(10_002), "constant").with_output("value", DataTypeId::SCALAR),
            )
            .unwrap();
        let network = Graph::new()
            .add_node(
                Node::new(NodeId::new(10_000), "subnet")
                    .with_subnet(inner)
                    .with_output("out", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(10_001), "sink").with_input("in", &[DataTypeId::SCALAR]),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(20_000),
                NodeId::new(10_000),
                OutputPortIndex(0),
                NodeId::new(10_001),
                InputPortIndex(0),
            )
            .unwrap();

        let layer =
            Layer::new(LayerId::new(40_000), "big", network).with_parent(LayerId::new(30_001));
        let comp = Composition::new(
            CompId::new(30_000),
            "big comp",
            (640, 480),
            FrameRate::new(24, 1),
            100,
        )
        .add_layer(layer);

        let flat = Graph::new()
            .add_node(
                Node::new(NodeId::new(5), "constant").with_output("value", DataTypeId::SCALAR),
            )
            .unwrap();
        let doc = Document::new(flat).with_composition(comp);

        let watermarks = doc.id_watermarks();
        assert_eq!(watermarks.node, 10_002, "subnet contents must be scanned");
        assert_eq!(watermarks.edge, 20_000);
        assert_eq!(watermarks.comp, 30_000);
        assert_eq!(watermarks.layer, 40_000);
    }

    #[test]
    fn advance_id_counters_moves_all_counters_past_watermarks() {
        use crate::graph::Node;
        use crate::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};

        // The largest node id lives inside a subnet (REQ-LAYER-009: loaded
        // ids must never collide with fresh ones).
        let inner = Graph::new()
            .add_node(
                Node::new(NodeId::new(10_000), "constant").with_output("value", DataTypeId::SCALAR),
            )
            .unwrap();
        let network = Graph::new()
            .add_node(
                Node::new(NodeId::new(9_999), "subnet")
                    .with_subnet(inner)
                    .with_output("out", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(Node::new(NodeId::new(9_998), "sink").with_input("in", &[DataTypeId::SCALAR]))
            .unwrap()
            .add_edge(
                EdgeId::new(11_000),
                NodeId::new(9_999),
                OutputPortIndex(0),
                NodeId::new(9_998),
                InputPortIndex(0),
            )
            .unwrap();
        let layer = Layer::new(LayerId::new(12_000), "big", network);
        let comp = Composition::new(
            CompId::new(13_000),
            "big comp",
            (640, 480),
            FrameRate::new(24, 1),
            100,
        )
        .add_layer(layer);
        let doc = Document::default().with_composition(comp);

        doc.advance_id_counters();
        assert!(NodeId::next().raw() > 10_000);
        assert!(EdgeId::next().raw() > 11_000);
        assert!(CompId::next().raw() > 13_000);
        assert!(LayerId::next().raw() > 12_000);
    }

    #[test]
    fn id_watermarks_include_embedded_comp_id_and_layer_ref_targets() {
        use crate::graph::{Node, ParameterValue};
        use crate::id::{CompId, DataTypeId, NodeId};

        // A layer.ref parameter targets LayerId(99_000) by raw id; counters
        // must move past it so a fresh layer never inherits the reference.
        let ref_node = Node::new(NodeId::new(1), "layer.ref")
            .with_param("layer", ParameterValue::Int(99_000))
            .with_output("out", DataTypeId::SCALAR);
        let network = Graph::new().add_node(ref_node).unwrap();
        let comp = Composition::new(CompId::new(7), "c", (16, 16), FrameRate::new(30, 1), 10)
            .add_layer(Layer::new(LayerId::new(2), "L", network));
        let mut doc = Document::default().with_composition(comp);

        let watermarks = doc.id_watermarks();
        assert_eq!(watermarks.layer, 99_000);

        // An embedded composition id larger than its map key counts too.
        let mut comp = Composition::new(
            CompId::new(88_000),
            "d",
            (16, 16),
            FrameRate::new(30, 1),
            10,
        );
        comp.id = CompId::new(88_000);
        doc.compositions
            .insert(CompId::new(3), std::sync::Arc::new(comp));
        assert_eq!(doc.id_watermarks().comp, 88_000);
    }

    #[test]
    fn validate_rejects_structural_violations() {
        use crate::graph::Node;
        use crate::id::{CompId, DataTypeId, NodeId};

        let valid = Document::default().with_composition(test_comp().add_layer(empty_layer(1)));
        assert_eq!(valid.validate(), Ok(()));

        // Root comp missing from the map.
        let mut doc = valid.clone();
        doc.root_comp = Some(CompId::new(999));
        assert_eq!(
            doc.validate(),
            Err(DocumentValidationError::MissingRoot(CompId::new(999)))
        );

        // Map key disagrees with the embedded composition id.
        let mut doc = valid.clone();
        let comp = doc
            .get_composition(CompId::new(1))
            .unwrap()
            .as_ref()
            .clone();
        doc.compositions
            .insert(CompId::new(55), std::sync::Arc::new(comp));
        assert_eq!(
            doc.validate(),
            Err(DocumentValidationError::CompIdMismatch {
                key: CompId::new(55),
                embedded: CompId::new(1),
            })
        );

        // Zero frame-rate component (playback divides by it).
        let mut comp = test_comp();
        comp.frame_rate = FrameRate { num: 30, den: 0 };
        let doc = Document::default().with_composition(comp);
        assert_eq!(
            doc.validate(),
            Err(DocumentValidationError::InvalidFrameRate(CompId::new(1)))
        );

        // Duplicate layer id inside one composition.
        let comp = test_comp()
            .add_layer(empty_layer(1))
            .add_layer(empty_layer(1));
        let doc = Document::default().with_composition(comp);
        assert_eq!(
            doc.validate(),
            Err(DocumentValidationError::DuplicateLayerId {
                comp: CompId::new(1),
                layer: LayerId::new(1),
            })
        );

        // Parent reference into the void.
        let comp = test_comp().add_layer(empty_layer(1).with_parent(LayerId::new(77)));
        let doc = Document::default().with_composition(comp);
        assert_eq!(
            doc.validate(),
            Err(DocumentValidationError::DanglingLayerRef {
                comp: CompId::new(1),
                layer: LayerId::new(1),
                kind: "parent",
                target: LayerId::new(77),
            })
        );

        // An id that cannot have a successor.
        let node =
            Node::new(NodeId::new(u64::MAX), "constant").with_output("value", DataTypeId::SCALAR);
        let network = Graph::new().add_node(node).unwrap();
        let comp = test_comp().add_layer(Layer::new(LayerId::new(1), "L", network));
        let doc = Document::default().with_composition(comp);
        assert_eq!(
            doc.validate(),
            Err(DocumentValidationError::IdExhausted { kind: "node" })
        );
    }

    /// Node ids are document-globally unique (REQ-LAYER-009): the same id
    /// in two different layer networks is rejected even though each network
    /// is internally consistent.
    #[test]
    fn validate_rejects_globally_duplicate_node_ids() {
        use crate::graph::Node;
        use crate::id::{DataTypeId, NodeId};

        let make_network = || {
            Graph::new()
                .add_node(
                    Node::new(NodeId::new(42), "constant").with_output("v", DataTypeId::SCALAR),
                )
                .unwrap()
        };
        let comp = test_comp()
            .add_layer(Layer::new(LayerId::new(1), "A", make_network()))
            .add_layer(Layer::new(LayerId::new(2), "B", make_network()));
        let doc = Document::default().with_composition(comp);
        assert_eq!(
            doc.validate(),
            Err(DocumentValidationError::DuplicateNodeId(NodeId::new(42)))
        );
    }
}
