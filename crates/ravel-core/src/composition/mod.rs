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
pub mod validate;

use crate::animation::channel::AnimationChannel;
use crate::eval::PathSegment;
use crate::graph::Graph;
use crate::id::{CompId, LayerId};
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
#[derive(Clone, Debug, PartialEq)]
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
#[derive(Clone, Debug, PartialEq)]
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
#[derive(Clone, Debug, PartialEq)]
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
// Document
// ===========================================================================

/// Unified document snapshot containing the node graph and all compositions.
///
/// This is the unit of undo: `UndoStack<Document>` captures both the DAG
/// and the composition map in a single structurally-shared snapshot.
#[derive(Clone, Debug)]
pub struct Document {
    pub graph: Graph,
    pub compositions: im::HashMap<CompId, std::sync::Arc<Composition>>,
    pub root_comp: Option<CompId>,
}

impl Document {
    pub fn new(graph: Graph) -> Self {
        Self {
            graph,
            compositions: im::HashMap::new(),
            root_comp: None,
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

    pub fn get_composition(&self, id: CompId) -> Option<&std::sync::Arc<Composition>> {
        self.compositions.get(&id)
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
}
