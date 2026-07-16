// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! AE-style Composition/Layer model.
//!
//! Replaces the NLE Track/Clip model with a Composition containing ordered
//! Layers. Each Layer has a source, built-in transform (position/scale/
//! rotation/opacity/anchor_point as [`AnimationChannel`]s), blend mode,
//! and optional effect subgraph.
//!
//! Compositions are stored in the document as
//! `im::HashMap<CompId, Arc<Composition>>` alongside the main `Graph`,
//! enabling structural sharing for undo.

use crate::animation::channel::AnimationChannel;
use crate::graph::Graph;
use crate::id::{CompId, LayerId, NodeId};
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
// LayerSource
// ===========================================================================

/// The content source backing a layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LayerSource {
    Media {
        asset_id: String,
    },
    Solid {
        color: Color,
        width: u32,
        height: u32,
    },
    Shape {
        node_id: NodeId,
    },
    Text {
        node_id: NodeId,
    },
    PreComp {
        comp_id: CompId,
    },
    Generator {
        node_id: NodeId,
    },
    Null,
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

/// A single layer within a [`Composition`].
///
/// Layers are ordered bottom-to-top in the composition's `layers` vector
/// (index 0 = bottommost, rendered first).
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub source: LayerSource,
    /// Position on the composition timeline (can be negative).
    pub start_frame: i64,
    /// Source-local display start frame.
    pub in_frame: u64,
    /// Source-local display end frame (half-open: `[in, out)`).
    pub out_frame: u64,
    pub transform: LayerTransform,
    pub opacity: AnimationChannel,
    pub blend_mode: BlendMode,
    pub solo: bool,
    pub muted: bool,
    pub locked: bool,
    /// Parent layer for transform inheritance (P/R/S only; not opacity/blend).
    pub parent: Option<LayerId>,
    /// Optional effect node subgraph applied between source and transform.
    pub effect_graph: Option<Graph>,
}

impl Layer {
    pub fn new(id: LayerId, name: impl Into<String>, source: LayerSource) -> Self {
        Self {
            id,
            name: name.into(),
            source,
            start_frame: 0,
            in_frame: 0,
            out_frame: 0,
            transform: LayerTransform::default(),
            opacity: AnimationChannel::constant(1.0),
            blend_mode: BlendMode::default(),
            solo: false,
            muted: false,
            locked: false,
            parent: None,
            effect_graph: None,
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

    fn solid_layer(id: u64) -> Layer {
        Layer::new(
            LayerId::new(id),
            format!("Layer {id}"),
            LayerSource::Solid {
                color: Color::WHITE,
                width: 1920,
                height: 1080,
            },
        )
        .with_time(0, 0, 300)
    }

    #[test]
    fn composition_add_remove_layers() {
        let comp = test_comp()
            .add_layer(solid_layer(1))
            .add_layer(solid_layer(2))
            .add_layer(solid_layer(3));
        assert_eq!(comp.layer_count(), 3);

        let comp = comp.remove_layer(LayerId::new(2));
        assert_eq!(comp.layer_count(), 2);
        assert!(comp.get_layer(LayerId::new(2)).is_none());
        assert!(comp.get_layer(LayerId::new(1)).is_some());
        assert!(comp.get_layer(LayerId::new(3)).is_some());
    }

    #[test]
    fn layer_duration_and_end_frame() {
        let layer = solid_layer(1).with_time(10, 5, 100);
        assert_eq!(layer.duration(), 95);
        assert_eq!(layer.end_frame(), 105);
    }

    #[test]
    fn layer_negative_start_frame() {
        let layer = solid_layer(1).with_time(-30, 0, 60);
        assert_eq!(layer.start_frame, -30);
        assert_eq!(layer.end_frame(), 30);
    }

    #[test]
    fn composition_reorder() {
        let comp = test_comp()
            .add_layer(solid_layer(1))
            .add_layer(solid_layer(2))
            .add_layer(solid_layer(3));

        let comp = comp.reorder_layer(0, 2);
        assert_eq!(comp.layers[0].id, LayerId::new(2));
        assert_eq!(comp.layers[1].id, LayerId::new(3));
        assert_eq!(comp.layers[2].id, LayerId::new(1));
    }

    #[test]
    fn composition_insert_layer() {
        let comp = test_comp()
            .add_layer(solid_layer(1))
            .add_layer(solid_layer(3));

        let comp = comp.insert_layer(1, solid_layer(2));
        assert_eq!(comp.layers[0].id, LayerId::new(1));
        assert_eq!(comp.layers[1].id, LayerId::new(2));
        assert_eq!(comp.layers[2].id, LayerId::new(3));
    }

    #[test]
    fn blend_mode_default() {
        assert_eq!(BlendMode::default(), BlendMode::Normal);
    }

    #[test]
    fn layer_source_variants() {
        let _ = LayerSource::Media {
            asset_id: "clip.mov".into(),
        };
        let _ = LayerSource::Solid {
            color: Color::WHITE,
            width: 100,
            height: 100,
        };
        let _ = LayerSource::Shape {
            node_id: NodeId::new(1),
        };
        let _ = LayerSource::Text {
            node_id: NodeId::new(2),
        };
        let _ = LayerSource::PreComp {
            comp_id: CompId::new(1),
        };
        let _ = LayerSource::Generator {
            node_id: NodeId::new(3),
        };
        let _ = LayerSource::Null;
    }

    #[test]
    fn layer_parenting() {
        let parent = solid_layer(1);
        let child = solid_layer(2).with_parent(parent.id);
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
        let comp = test_comp().add_layer(solid_layer(1));
        let comp_clone = comp.clone();
        assert_eq!(comp.layers.len(), comp_clone.layers.len());
    }

    #[test]
    fn layer_default_transform() {
        let layer = solid_layer(1);
        let ctx = crate::eval::EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080));
        assert!((layer.transform.position[0].evaluate(0, &ctx) - 0.0).abs() < f32::EPSILON);
        assert!((layer.transform.scale[0].evaluate(0, &ctx) - 1.0).abs() < f32::EPSILON);
        assert!((layer.transform.rotation.evaluate(0, &ctx) - 0.0).abs() < f32::EPSILON);
        assert!((layer.opacity.evaluate(0, &ctx) - 1.0).abs() < f32::EPSILON);
    }
}
