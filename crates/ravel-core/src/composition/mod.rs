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

pub mod asset;
mod color_upgrade;
pub mod compile;
mod curve_upgrade;
pub(crate) mod graph_walk;
mod param_fold;
pub mod templates;
pub mod transform;
pub mod validate;

pub use color_upgrade::{ColorMigrationNote, ColorMigrationReport, is_color_param};

pub use asset::{
    AssetKind, AssetMetadata, AssetPath, AudioStreamMetadata, ColorSpaceSource, MediaAssetEntry,
    expand_variables,
};

use crate::animation::channel::{AnimationChannel, ChannelSource};
use crate::eval::PathSegment;
use crate::exposed::ExposedParameters;
use crate::graph::{Graph, InputPort, Parameter, PortSide};
use crate::id::{CompId, DataTypeId, EdgeId, LayerId, NodeId};
use crate::network;
use crate::registry::NodeRegistry;
use crate::types::{Color, FrameRate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
// AudioSource
// ===========================================================================

/// Audio source owned by a layer shell.
///
/// The same shell can describe an audio-only layer (a network without a
/// `frame` output) or the explicit audio stream paired with a video layer.
/// Timing comes exclusively from the owning [`Layer`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioSource {
    /// Key into [`Document::media_assets`].
    #[serde(default)]
    pub asset_id: String,
    /// Audio stream number inside the media container.
    #[serde(default)]
    pub stream_index: usize,
    /// Linear gain (0.0 and above), evaluated in layer-local frames.
    #[serde(default = "default_audio_gain")]
    pub gain: AnimationChannel,
    #[serde(default)]
    pub fade_in_frames: u64,
    #[serde(default)]
    pub fade_out_frames: u64,
    /// Mute only this audio source, independently of [`Layer::muted`].
    #[serde(default)]
    pub audio_muted: bool,
}

fn default_audio_gain() -> AnimationChannel {
    AnimationChannel::constant(1.0)
}

impl Default for AudioSource {
    fn default() -> Self {
        Self {
            asset_id: String::new(),
            stream_index: 0,
            gain: default_audio_gain(),
            fade_in_frames: 0,
            fade_out_frames: 0,
            audio_muted: false,
        }
    }
}

impl AudioSource {
    pub fn new(asset_id: impl Into<String>, stream_index: usize) -> Self {
        Self {
            asset_id: asset_id.into(),
            stream_index,
            ..Self::default()
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
    /// Explicit audio carried by the shell. Missing in older format-v4 files.
    #[serde(default)]
    pub audio: Option<AudioSource>,
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
            audio: None,
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

    /// Deep-copy this layer with a fresh layer id and fresh ids throughout
    /// its owned graph hierarchy. Shell node-output bindings are remapped to
    /// the duplicated network; every other shell field is preserved.
    pub fn duplicate_with_fresh_ids(&self, id: LayerId) -> Self {
        let (network, id_map) = self.network.duplicate_with_fresh_ids();
        let mut duplicate = self.clone();
        duplicate.id = id;
        duplicate.network = network;
        for channel in &mut duplicate.transform.anchor_point {
            remap_layer_channel_node_outputs(channel, &id_map);
        }
        for channel in &mut duplicate.transform.position {
            remap_layer_channel_node_outputs(channel, &id_map);
        }
        for channel in &mut duplicate.transform.scale {
            remap_layer_channel_node_outputs(channel, &id_map);
        }
        remap_layer_channel_node_outputs(&mut duplicate.transform.rotation, &id_map);
        remap_layer_channel_node_outputs(&mut duplicate.opacity, &id_map);
        if let Some(audio) = &mut duplicate.audio {
            remap_layer_channel_node_outputs(&mut audio.gain, &id_map);
        }
        if let Some(time_remap) = &mut duplicate.time_remap {
            remap_layer_channel_node_outputs(time_remap, &id_map);
        }
        duplicate
    }

    /// The layer-local frame a composition frame maps to:
    /// `comp_frame - start_frame + in_frame`, clamped at zero
    /// (REQ-LAYER-006). Channel evaluation, keyframe display and keyframe
    /// writes all go through this one formula.
    pub fn local_frame(&self, comp_frame: u64) -> u64 {
        (comp_frame as i64 - self.start_frame + self.in_frame as i64).max(0) as u64
    }

    /// [`local_frame`](Self::local_frame) for a continuous composition frame.
    ///
    /// Channel evaluation samples between integer frames (motion blur, time
    /// remapping), so the shell's compositing chain maps a fractional
    /// composition frame onto a fractional layer-local one. Keyframe display
    /// and keyframe writes keep using the integer form: they address
    /// keyframes, which only ever sit on the frame grid.
    pub fn local_frame_continuous(&self, comp_frame: f64) -> f64 {
        (comp_frame - self.start_frame as f64 + self.in_frame as f64).max(0.0)
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

fn remap_layer_channel_node_outputs(
    channel: &mut AnimationChannel,
    id_map: &HashMap<NodeId, NodeId>,
) {
    fn remap(source: &mut ChannelSource, id_map: &HashMap<NodeId, NodeId>) {
        match source {
            ChannelSource::NodeOutput(node, _) => {
                if let Some(duplicate) = id_map.get(node) {
                    *node = *duplicate;
                }
            }
            ChannelSource::Blend(a, b, _, _) => {
                remap(a, id_map);
                remap(b, id_map);
            }
            ChannelSource::Constant(_)
            | ChannelSource::Keyframes(_)
            | ChannelSource::Expression(_)
            | ChannelSource::AudioReactive(_) => {}
        }
    }
    remap(&mut channel.source, id_map);
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

    /// Remove the layer and unparent everything that pointed at it.
    ///
    /// A child left holding the removed id would name a layer the composition
    /// no longer has — exactly the dangling reference [`Document::validate`]
    /// reports, and a chain [`Self::ancestors`] would silently cut short.
    /// Deleting a parent therefore promotes its children to the composition
    /// root, which is the same state the Parent picker's "no parent" option
    /// produces. Grandchildren keep their own parent: only the link to the
    /// removed layer is gone.
    pub fn remove_layer(mut self, id: LayerId) -> Self {
        self.layers.retain(|l| l.id != id);
        for index in 0..self.layers.len() {
            if self.layers[index].parent != Some(id) {
                continue;
            }
            let mut orphan = self.layers[index].clone();
            orphan.parent = None;
            self.layers.set(index, orphan);
        }
        self
    }

    pub fn get_layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    /// The layer's parent chain, nearest ancestor first.
    ///
    /// Solo / mute state is irrelevant here: parenting is a transform
    /// relationship, not a visibility one (REQ-LAYER-001). Parent cycles are
    /// rejected by validation; a visited guard terminates the walk anyway so
    /// unvalidated documents cannot hang a caller.
    pub fn ancestors(&self, layer: &Layer) -> Vec<&Layer> {
        let mut chain = Vec::new();
        let mut seen = vec![layer.id];
        let mut current = layer.parent;
        while let Some(parent_id) = current {
            if seen.contains(&parent_id) {
                break;
            }
            let Some(parent) = self.get_layer(parent_id) else {
                break;
            };
            seen.push(parent_id);
            chain.push(parent);
            current = parent.parent;
        }
        chain
    }

    /// Whether `layer` inherits a transform from `ancestor` (any depth).
    pub fn descends_from(&self, layer: &Layer, ancestor: LayerId) -> bool {
        self.ancestors(layer).iter().any(|l| l.id == ancestor)
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
    /// The project's external parameter contract (REQ-PROJ-006): the values a
    /// CLI render or a template instantiation may set, by name, without
    /// knowing the network behind them.
    ///
    /// Added in `.ravprj` v7. `default` — not `skip_serializing_if` — so a v6
    /// document, which has no such field, reads as zero declarations and
    /// round-trips; the version bump exists so an older build refuses the file
    /// instead of silently rewriting the contract away
    /// (`docs/dev/persistence.md`).
    #[serde(default)]
    pub exposed_parameters: ExposedParameters,
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
    #[error("node {node} has a parameter port {key:?} without a matching parameter")]
    ParamPortWithoutParameter { node: NodeId, key: String },
    #[error("subnet nesting exceeds the supported depth of {limit}")]
    SubnetDepthExceeded { limit: usize },
}

/// Maximum number of nested subnet ownership boundaries in a document.
pub const MAX_SUBNET_DEPTH: usize = 64;

fn check_subnet_depth(graph: &Graph) -> Result<(), DocumentValidationError> {
    if subnet_depth_exceeds(graph, MAX_SUBNET_DEPTH) {
        return Err(DocumentValidationError::SubnetDepthExceeded {
            limit: MAX_SUBNET_DEPTH,
        });
    }
    Ok(())
}

/// Whether `graph` nests subnet ownership boundaries more than `limit` deep.
///
/// Public because the document is not the only thing that has to hold this
/// line: a `.ravtpl` carries a graph that will *become* part of a document
/// ([`crate::subgraph_template`]), and checking it there with a second walk
/// would be a second place for the two to disagree.
///
/// The walk keeps its own stack rather than recursing, so measuring a graph
/// too deep to recurse over cannot itself overflow.
pub fn subnet_depth_exceeds(graph: &Graph, limit: usize) -> bool {
    let mut pending = vec![(graph, 0usize)];
    while let Some((graph, depth)) = pending.pop() {
        for node in graph.nodes() {
            if let Some(subnet) = node.subnet.as_deref() {
                let nested_depth = depth + 1;
                if nested_depth > limit {
                    return true;
                }
                pending.push((subnet, nested_depth));
            }
        }
    }
    false
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

/// The wire types a parameter port named `key` should accept, given the
/// node's `parameters`. `None` when no such parameter exists or it cannot be
/// exposed at all (an empty accepted list would read as "accepts anything").
fn accepted_types_for_param(parameters: &[Parameter], key: &str) -> Option<Vec<DataTypeId>> {
    let accepted = parameters
        .iter()
        .find(|p| p.key == key)?
        .value
        .port_accepted_types();
    (!accepted.is_empty()).then_some(accepted)
}

/// Whether `port` is a legacy pre-exposed pin: not yet flagged `is_param`,
/// but declared with exactly the principal wire type of a same-named
/// parameter — how such a pin was written before parameter ports existed.
fn is_legacy_param_pin(parameters: &[Parameter], port: &InputPort) -> bool {
    !port.is_param
        && parameters
            .iter()
            .find(|p| p.key == port.name)
            .and_then(|p| p.value.port_data_type())
            .is_some_and(|t| port.accepted_types == vec![t])
}

/// Upgrade legacy pre-exposed parameter pins to `is_param` ports, and bring
/// every parameter port's accepted wire types up to what its parameter takes
/// today.
///
/// Documents persisted before parameter ports existed (`.ravprj` v3 with
/// `InputPort.is_param` defaulting to false) carry input ports that shadow
/// a same-named parameter — the rasterize `color` pin pattern. The
/// evaluator only overlays `is_param` ports, so without this upgrade a
/// connected legacy pin would be silently ignored.
///
/// The accepted set is re-derived rather than trusted because it widens over
/// time: a 4-component parameter port was stored with `[COLOR]` and now takes
/// `[COLOR, VEC4]`. Leaving the stored list alone would make an old project
/// refuse a connection an identical new one accepts. Nodes that cannot carry
/// parameter ports (synthetic, `net.in`/`net.out`, subnets — whose same-named
/// pin/parameter pairs are the promotion mechanism with the *opposite*
/// fallback direction) are left untouched; subnet inner graphs are normalized
/// recursively.
fn normalize_param_ports(graph: &Graph) -> Graph {
    let mut normalized = graph.clone();
    let ids: Vec<crate::id::NodeId> = normalized.node_ids().collect();
    for id in ids {
        let Some(node) = normalized.node(id) else {
            continue;
        };
        let subnet_normalized = node
            .subnet
            .as_ref()
            .map(|inner| normalize_param_ports(inner));
        let needs_port_upgrade = node.supports_param_ports()
            && node.inputs.iter().any(|port| {
                is_legacy_param_pin(&node.parameters, port)
                    || (port.is_param
                        && accepted_types_for_param(&node.parameters, &port.name)
                            .is_some_and(|accepted| accepted != port.accepted_types))
            });
        if !needs_port_upgrade && subnet_normalized.is_none() {
            continue;
        }
        let mut updated = (**node).clone();
        if needs_port_upgrade {
            let parameters = updated.parameters.clone();
            for port in &mut updated.inputs {
                if is_legacy_param_pin(&parameters, port) {
                    port.is_param = true;
                }
                if port.is_param
                    && let Some(accepted) = accepted_types_for_param(&parameters, &port.name)
                {
                    port.accepted_types = accepted;
                }
            }
        }
        if let Some(inner) = subnet_normalized {
            updated.subnet = Some(std::sync::Arc::new(inner));
        }
        normalized = normalized.replace_node(std::sync::Arc::new(updated));
    }
    normalized
}

/// Append builtin In-node output ports introduced after a network was
/// persisted. Currently: the layer-local frame index `f`
/// (REQ-LAYER-002). Appending at the end keeps existing index-addressed
/// edges valid. Subnet inner In nodes are left untouched — their ports
/// define the enclosing subnet node's pin interface, which must not
/// change shape on load. A user-defined custom port that already claims
/// the builtin name is kept as-is.
fn append_missing_in_ports(graph: &Graph) -> Graph {
    let Some(in_node) = crate::network::find_in_node(graph) else {
        return graph.clone();
    };
    if in_node
        .outputs
        .iter()
        .any(|p| p.name == crate::network::PORT_FRAME_INDEX)
    {
        return graph.clone();
    }
    let mut updated = (**in_node).clone();
    updated.outputs.push(crate::graph::OutputPort {
        name: crate::network::PORT_FRAME_INDEX.to_string(),
        data_type: crate::id::DataTypeId::SCALAR,
    });
    graph.clone().replace_node(std::sync::Arc::new(updated))
}

/// Upgrade nodes whose templates declare a variadic input group. Existing
/// non-parameter ports after the fixed template inputs are flagged and renamed
/// in place. Exposed parameter ports remain outside the group; when a connected
/// group needs an empty slot, graph growth inserts it before those parameter
/// ports and reindexes their edges. Nested subnets are normalized recursively.
fn normalize_variadic_input_ports(graph: &Graph, registry: &NodeRegistry) -> Graph {
    let mut normalized = graph.clone();
    let ids: Vec<NodeId> = normalized.node_ids().collect();
    for id in ids {
        let Some(node) = normalized.node(id) else {
            continue;
        };
        let subnet_normalized = node
            .subnet
            .as_ref()
            .map(|inner| normalize_variadic_input_ports(inner, registry));
        let group = registry.get(&node.type_key).and_then(|template| {
            template
                .variadic_input_group
                .as_ref()
                .map(|base| (template.inputs.len(), base))
        });
        if group.is_none() && subnet_normalized.is_none() {
            continue;
        }

        let mut updated = (**node).clone();
        if let Some(inner) = subnet_normalized {
            updated.subnet = Some(std::sync::Arc::new(inner));
        }
        normalized = normalized.replace_node(std::sync::Arc::new(updated));
        if let Some((fixed_input_count, base)) = group {
            normalized = normalized
                .clone()
                .normalize_variadic_input_group(id, fixed_input_count, base)
                .unwrap_or(normalized);
        }
    }
    normalized
}

/// Rewrite renamed node type keys to their canonical form in one graph.
///
/// Currently the only rename is `video` → `media` (the unified media node,
/// `docs/implementation/media-import-plan.md` decision 2). Aliases are
/// resolved on load rather than registered twice so every later lookup —
/// registry templates, processor dispatch, param ranges — sees only the
/// canonical key. Subnet inner graphs are normalized recursively.
fn normalize_node_type_aliases(graph: &Graph) -> Graph {
    let mut normalized = graph.clone();
    let ids: Vec<NodeId> = normalized.node_ids().collect();
    for id in ids {
        let Some(node) = normalized.node(id) else {
            continue;
        };
        let subnet_normalized = node
            .subnet
            .as_ref()
            .map(|inner| normalize_node_type_aliases(inner));
        let canonical = match node.type_key.as_str() {
            "video" => Some("media"),
            _ => None,
        };
        if canonical.is_none() && subnet_normalized.is_none() {
            continue;
        }
        let mut updated = (**node).clone();
        if let Some(key) = canonical {
            updated.type_key = key.to_string();
        }
        if let Some(inner) = subnet_normalized {
            updated.subnet = Some(std::sync::Arc::new(inner));
        }
        normalized = normalized.replace_node(std::sync::Arc::new(updated));
    }
    normalized
}

/// Every `is_param` input port must be backed by a same-named parameter on
/// its node (the evaluator resolves the port value into that parameter).
/// Warn about every subnet pin a load-time repair deleted, and the outer edges
/// that went with it.
///
/// `before` and `after` are one graph across one call of
/// [`crate::network::sync_subnet_pins_in`], so a pin present in the first and
/// missing from the second was dropped by the repair — with no user action to
/// attribute it to, which is the whole reason this is worth a warning. Edges
/// are counted in `before`, where the pin still has its slot.
fn warn_pins_lost_on_load(before: &Graph, after: &Graph) {
    for node in before.nodes().filter(|node| network::is_subnet_node(node)) {
        let Some(repaired) = after.node(node.id) else {
            continue;
        };
        let lost = |side: PortSide, name: &str, index: usize| {
            let dropped = before
                .edges()
                .filter(|edge| match side {
                    PortSide::Input => {
                        edge.target == node.id && edge.target_port.0 as usize == index
                    }
                    PortSide::Output => {
                        edge.source == node.id && edge.source_port.0 as usize == index
                    }
                })
                .count();
            tracing::warn!(
                node = ?node.id,
                ?side,
                pin = %name,
                dropped_edges = dropped,
                "subnet pin removed on load; the stored pins disagreed with the inner network"
            );
        };
        for (index, port) in node.inputs.iter().enumerate() {
            if !repaired.inputs.iter().any(|p| p.name == port.name) {
                lost(PortSide::Input, &port.name, index);
            }
        }
        for (index, port) in node.outputs.iter().enumerate() {
            if !repaired.outputs.iter().any(|p| p.name == port.name) {
                lost(PortSide::Output, &port.name, index);
            }
        }
    }
}

fn check_param_ports(graph: &Graph) -> Result<(), DocumentValidationError> {
    for node in graph.nodes() {
        for port in &node.inputs {
            if port.is_param && !node.parameters.iter().any(|p| p.key == port.name) {
                return Err(DocumentValidationError::ParamPortWithoutParameter {
                    node: node.id,
                    key: port.name.clone(),
                });
            }
        }
        if let Some(subnet) = &node.subnet {
            check_param_ports(subnet)?;
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
            exposed_parameters: ExposedParameters::new(),
        }
    }

    /// Validate only the subnet nesting budget. Persistence calls this before
    /// recursive compatibility normalization; full [`Self::validate`] calls
    /// it again as its first invariant.
    pub fn validate_subnet_depth(&self) -> Result<(), DocumentValidationError> {
        check_subnet_depth(&self.graph)?;
        for composition in self.compositions.values() {
            for layer in &composition.layers {
                check_subnet_depth(&layer.network)?;
            }
        }
        Ok(())
    }

    /// Upgrade legacy pre-exposed parameter pins (an input port shadowing a
    /// same-named, type-matching parameter) to `is_param` ports in every
    /// graph of the document — the flat graph, each layer network, and
    /// nested subnets. Run on load: documents persisted before parameter
    /// ports existed deserialize with `is_param: false`, and the evaluator
    /// only overlays `is_param` ports.
    pub fn normalize_param_ports(mut self) -> Self {
        self.graph = normalize_param_ports(&self.graph);
        let comp_ids: Vec<CompId> = self.compositions.keys().copied().collect();
        for id in comp_ids {
            let Some(comp) = self.compositions.get(&id) else {
                continue;
            };
            let mut updated = (**comp).clone();
            for layer in updated.layers.iter_mut() {
                layer.network = normalize_param_ports(&layer.network);
            }
            self.compositions.insert(id, std::sync::Arc::new(updated));
        }
        self
    }

    /// Append builtin In-node output ports (currently the frame index `f`)
    /// to every layer network that predates them. Run on load; idempotent.
    pub fn normalize_net_in_ports(mut self) -> Self {
        let comp_ids: Vec<CompId> = self.compositions.keys().copied().collect();
        for id in comp_ids {
            let Some(comp) = self.compositions.get(&id) else {
                continue;
            };
            let mut updated = (**comp).clone();
            for layer in updated.layers.iter_mut() {
                layer.network = append_missing_in_ports(&layer.network);
            }
            self.compositions.insert(id, std::sync::Arc::new(updated));
        }
        self
    }

    /// Upgrade template-declared variadic input groups in every graph of the
    /// document. Existing slots are flagged in place and an empty trailing
    /// slot is appended when needed; nested subnets are included. Run on load.
    pub fn normalize_variadic_input_ports(mut self, registry: &NodeRegistry) -> Self {
        self.graph = normalize_variadic_input_ports(&self.graph, registry);
        let comp_ids: Vec<CompId> = self.compositions.keys().copied().collect();
        for id in comp_ids {
            let Some(comp) = self.compositions.get(&id) else {
                continue;
            };
            let mut updated = (**comp).clone();
            for layer in updated.layers.iter_mut() {
                layer.network = normalize_variadic_input_ports(&layer.network, registry);
            }
            self.compositions.insert(id, std::sync::Arc::new(updated));
        }
        self
    }

    /// Re-derive every subnet node's pins from the inner graph it owns
    /// ([`crate::network::sync_subnet_pins`]) in every graph of the document —
    /// the flat graph, each layer network, and nested subnets, inner-most
    /// first.
    ///
    /// This is drift repair, not a format upgrade. Pins and inner declaration
    /// are kept in step by the editing path, but an archive written before
    /// that path existed — or by a version whose derivation differed — can
    /// hold a subnet whose pins say something its inner In / Out does not, and
    /// a pin index that no longer means what an edge thinks it means fails
    /// silently at evaluation. A document already in step is returned
    /// unchanged, so this is idempotent and costs one traversal.
    ///
    /// **Mints no ids**, which is why a subnet with no inner graph at all is
    /// left broken here rather than repaired: it runs before
    /// [`Self::advance_id_counters`], where a fresh id can still collide with
    /// a stored one.
    ///
    /// A repair that **loses a pin** is warned about
    /// ([`warn_pins_lost_on_load`]). The same removal after a deliberate
    /// `remove_custom_port` is what the user asked for and stays quiet, so the
    /// warning lives here — the one caller that knows the removal is nobody's
    /// doing — rather than in the shared branch inside
    /// [`crate::network::sync_subnet_pins`].
    pub fn sync_subnet_pins(self) -> Self {
        self.map_graphs(|graph| {
            graph_walk::map_subnets(graph, &|graph: &Graph| {
                let synced = crate::network::sync_subnet_pins_in(graph);
                warn_pins_lost_on_load(graph, &synced);
                synced
            })
        })
    }

    /// Fold `.ravprj` v4 component parameters (`center_x` / `center_y`, the
    /// scalar `geometry.transform` `rotation`, …) into the `Channel2` /
    /// `Channel3` vector parameters the templates now declare, in every graph
    /// of the document — the flat graph, each layer network, and nested
    /// subnets. Two separately driven component ports are preserved by an
    /// inserted `vector.construct` node, so this **mints node and edge ids**
    /// and must run after `advance_id_counters`. Idempotent.
    pub fn fold_component_params(self) -> Self {
        // Every inserted `vector.construct` must get an id no graph in the
        // document uses, including the ones folded later in this pass.
        self.advance_id_counters();
        self.map_graphs(param_fold::fold_graph)
    }

    /// Convert `.ravprj` v5 curve parameters stored as `"in:out,…"` strings
    /// into [`ParameterValue::Curve`](crate::graph::ParameterValue::Curve), in
    /// every graph of the document — the flat graph, each layer network, and
    /// nested subnets. A string that cannot be read becomes the identity curve
    /// and is logged. Mints no ids. Idempotent.
    pub fn upgrade_curve_params(self) -> Self {
        self.map_graphs(curve_upgrade::upgrade_graph)
    }

    /// Reinterpret every authored colour for the linear working space
    /// (`.ravprj` v7 → v8), in every graph of the document — the flat graph,
    /// each layer network, and nested subnets.
    ///
    /// **Not idempotent, and cannot be**: `srgb → linear` applied twice is a
    /// different colour, and no inspection of a stored number can tell how
    /// many times it has been applied. Idempotence is the *format version's*
    /// job — [`ProjectFile::from_archive`](../../ravel_project/struct.ProjectFile.html)
    /// runs this only for an archive written before v8, and a v8 archive is
    /// never converted again.
    ///
    /// The returned [`ColorMigrationReport`] lists what could not be
    /// converted; the caller is expected to surface it.
    pub fn linearize_colors(
        self,
        registry: &NodeRegistry,
    ) -> (Self, color_upgrade::ColorMigrationReport) {
        let report = std::cell::RefCell::new(color_upgrade::ColorMigrationReport::default());
        let mut document =
            self.map_graphs(|graph| color_upgrade::upgrade_graph(graph, registry, &report));

        // Two authored colours live outside every graph, so the node walk
        // above cannot see them.
        let comp_ids: Vec<CompId> = document.compositions.keys().copied().collect();
        for id in comp_ids {
            let Some(comp) = document.compositions.get(&id) else {
                continue;
            };
            let mut updated = (**comp).clone();
            updated.background_color = color_upgrade::linearize_color(updated.background_color);
            report.borrow_mut().converted += 3;
            document
                .compositions
                .insert(id, std::sync::Arc::new(updated));
        }
        document.exposed_parameters =
            document
                .exposed_parameters
                .map_defaults(|value| match value {
                    crate::exposed::ExposedValue::Color(color) => {
                        report.borrow_mut().converted += 3;
                        crate::exposed::ExposedValue::Color(color_upgrade::linearize_color(color))
                    }
                    other => other,
                });

        (document, report.into_inner())
    }

    /// Apply a graph rewrite to every graph the document owns: the flat
    /// graph and each layer network of each composition. Rewrites that must
    /// also reach nested subnets compose this with
    /// [`graph_walk::map_subnets`].
    pub(crate) fn map_graphs(mut self, upgrade: impl Fn(&Graph) -> Graph) -> Self {
        self.graph = upgrade(&self.graph);
        let comp_ids: Vec<CompId> = self.compositions.keys().copied().collect();
        for id in comp_ids {
            let Some(comp) = self.compositions.get(&id) else {
                continue;
            };
            let mut updated = (**comp).clone();
            for layer in updated.layers.iter_mut() {
                layer.network = upgrade(&layer.network);
            }
            self.compositions.insert(id, std::sync::Arc::new(updated));
        }
        self
    }

    /// Rewrite renamed node type keys (`video` → `media`) in every graph of
    /// the document — the flat graph, each layer network, and nested
    /// subnets. Run on load, before the registry-dependent normalizations,
    /// so persisted documents that predate the rename behave exactly like
    /// freshly written ones. Idempotent.
    pub fn normalize_node_type_aliases(mut self) -> Self {
        self.graph = normalize_node_type_aliases(&self.graph);
        let comp_ids: Vec<CompId> = self.compositions.keys().copied().collect();
        for id in comp_ids {
            let Some(comp) = self.compositions.get(&id) else {
                continue;
            };
            let mut updated = (**comp).clone();
            for layer in updated.layers.iter_mut() {
                layer.network = normalize_node_type_aliases(&layer.network);
            }
            self.compositions.insert(id, std::sync::Arc::new(updated));
        }
        self
    }

    pub fn with_composition(mut self, comp: Composition) -> Self {
        let id = comp.id;
        self.compositions.insert(id, std::sync::Arc::new(comp));
        if self.root_comp.is_none() {
            self.root_comp = Some(id);
        }
        self
    }

    /// Register a media asset that already has a known absolute location
    /// (import, `Relink`, and every test fixture). The persisted form starts
    /// out absolute and narrows to project-relative at save time.
    pub fn with_media_asset(
        self,
        id: impl Into<String>,
        path: impl Into<std::path::PathBuf>,
    ) -> Self {
        self.with_media_asset_entry(id, MediaAssetEntry::from_absolute(path))
    }

    /// Register a fully-described media asset.
    pub fn with_media_asset_entry(mut self, id: impl Into<String>, entry: MediaAssetEntry) -> Self {
        self.media_assets.insert(id.into(), entry);
        self
    }

    /// Replace the document's exposed parameter declarations (REQ-PROJ-006).
    ///
    /// Takes a whole [`ExposedParameters`] rather than one declaration because
    /// that type owns the uniqueness invariant: a builder that could fail
    /// half-way would leave the caller holding a document with an ambiguous
    /// contract.
    pub fn with_exposed_parameters(mut self, exposed_parameters: ExposedParameters) -> Self {
        self.exposed_parameters = exposed_parameters;
        self
    }

    /// A copy whose assets have `resolved` recomputed from their persisted
    /// paths against `project_root` (`None` for a project that has never
    /// been saved). Called after a load, after `Save As`, and after an
    /// import — never during evaluation.
    ///
    /// The result is a plain `Document` rather than a mutation so it stays
    /// one undo-visible snapshot; callers install it wholesale.
    pub fn with_resolved_assets(
        mut self,
        project_root: Option<&std::path::Path>,
        vars: &HashMap<String, String>,
    ) -> Self {
        self.media_assets = self
            .media_assets
            .iter()
            .map(|(id, entry)| (id.clone(), entry.resolved_against(project_root, vars)))
            .collect();
        self
    }

    /// A copy whose assets' persisted paths describe a project stored at
    /// `project_root`. Applied to the snapshot being written, so saving does
    /// not itself dirty the in-memory document.
    pub fn with_relativized_assets(mut self, project_root: Option<&std::path::Path>) -> Self {
        self.media_assets = self
            .media_assets
            .iter()
            .map(|(id, entry)| (id.clone(), entry.relativized(project_root)))
            .collect();
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
                            // `ptr_eq` first: an edit to one layer leaves
                            // every other layer's network sharing the same
                            // map root, and proving that is O(1) where the
                            // deep compare that follows is proportional to
                            // the whole network's nodes and keyframes.
                            .map(|old_layer| {
                                !old_layer.network.ptr_eq(&layer.network)
                                    && old_layer.network != layer.network
                            })
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
        self.validate_subnet_depth()?;
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
        check_param_ports(&self.graph)?;
        for comp in self.compositions.values() {
            for layer in &comp.layers {
                check_unique_node_ids(&layer.network, &mut node_ids)?;
                check_param_ports(&layer.network)?;
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
    use crate::graph::Node;
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

    /// Removing a parent leaves no layer pointing at an id the composition no
    /// longer has: its children fall back to the composition root, while a
    /// grandchild keeps the parent it still has.
    #[test]
    fn removing_a_parent_unparents_its_children() {
        let comp = test_comp()
            .add_layer(empty_layer(1))
            .add_layer(empty_layer(2).with_parent(LayerId::new(1)))
            .add_layer(empty_layer(3).with_parent(LayerId::new(2)));

        let comp = comp.remove_layer(LayerId::new(1));
        assert_eq!(comp.get_layer(LayerId::new(2)).unwrap().parent, None);
        assert_eq!(
            comp.get_layer(LayerId::new(3)).unwrap().parent,
            Some(LayerId::new(2)),
            "a grandchild keeps the parent that is still there"
        );
        assert!(
            comp.layers
                .iter()
                .all(|l| l.parent != Some(LayerId::new(1)))
        );
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
    fn layer_duplication_remaps_shell_bindings_and_preserves_keyframes() {
        use crate::animation::channel::ChannelSource;
        use crate::graph::Node;
        use crate::id::{DataTypeId, NodeId, OutputPortIndex};

        let node = NodeId::next();
        let network = Graph::new()
            .add_node(Node::new(node, "constant").with_output("v", DataTypeId::SCALAR))
            .unwrap();
        let mut layer = Layer::new(LayerId::next(), "Source", network).with_time(12, 3, 90);
        layer.transform.position[0] =
            AnimationChannel::new(ChannelSource::NodeOutput(node, OutputPortIndex(0)));
        layer.opacity = keyframed_channel(&[(0, 0.25), (10, 0.75)]);
        layer.audio = Some(AudioSource {
            gain: AnimationChannel::new(ChannelSource::NodeOutput(node, OutputPortIndex(0))),
            ..AudioSource::new("audio", 1)
        });
        layer.locked = true;
        let duplicate_id = LayerId::next();

        let duplicate = layer.duplicate_with_fresh_ids(duplicate_id);
        let duplicate_node = duplicate.network.node_ids().next().unwrap();
        assert_eq!(duplicate.id, duplicate_id);
        assert_ne!(duplicate_node, node);
        assert!(matches!(
            duplicate.transform.position[0].source,
            ChannelSource::NodeOutput(bound, OutputPortIndex(0)) if bound == duplicate_node
        ));
        assert_eq!(duplicate.opacity, layer.opacity);
        assert!(matches!(
            duplicate.audio.as_ref().unwrap().gain.source,
            ChannelSource::NodeOutput(bound, OutputPortIndex(0)) if bound == duplicate_node
        ));
        assert_eq!(duplicate.audio.as_ref().unwrap().asset_id, "audio");
        assert_eq!(duplicate.audio.as_ref().unwrap().stream_index, 1);
        assert_eq!(duplicate.start_frame, 12);
        assert_eq!((duplicate.in_frame, duplicate.out_frame), (3, 90));
        assert!(duplicate.locked);
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

    /// `fold_component_params` reaches every graph of the document: the flat
    /// graph, each layer network, and a subnet inside a layer network.
    #[test]
    fn fold_component_params_reaches_every_graph_of_the_document() {
        use crate::graph::{Node, ParameterValue};
        use crate::id::{DataTypeId, NodeId};

        let v4_rect = |id: u64, cx: f32, cy: f32| {
            Node::new(NodeId::new(id), "shape.rect")
                .with_output("output", DataTypeId::GEOMETRY)
                .with_param("center_x", ParameterValue::Float(cx))
                .with_param("center_y", ParameterValue::Float(cy))
        };
        let center = |graph: &Graph, id: u64| {
            let value = &graph
                .node(NodeId::new(id))
                .unwrap_or_else(|| panic!("node {id}"))
                .parameters
                .iter()
                .find(|p| p.key == "center")
                .unwrap_or_else(|| panic!("node {id} center"))
                .value;
            match value {
                ParameterValue::Channel2(chs) => chs
                    .iter()
                    .map(|ch| match ch.source {
                        ChannelSource::Constant(v) => v,
                        ref other => panic!("{other:?}"),
                    })
                    .collect::<Vec<_>>(),
                other => panic!("{other:?}"),
            }
        };

        let inner = Graph::new().add_node(v4_rect(30, 5.0, 6.0)).unwrap();
        let network = Graph::new()
            .add_node(v4_rect(20, 3.0, 4.0))
            .unwrap()
            .add_node(
                Node::new(NodeId::new(21), "subnet")
                    .with_subnet(inner)
                    .with_output("out", DataTypeId::GEOMETRY),
            )
            .unwrap();
        let comp = Composition::new(
            CompId::new(100),
            "Comp",
            (64, 64),
            FrameRate::new(30, 1),
            100,
        )
        .add_layer(Layer::new(LayerId::new(200), "L", network));
        let document = Document::new(Graph::new().add_node(v4_rect(10, 1.0, 2.0)).unwrap())
            .with_composition(comp);

        let folded = document.fold_component_params();
        assert_eq!(center(&folded.graph, 10), vec![1.0, 2.0]);
        let network = &folded.get_composition(CompId::new(100)).unwrap().layers[0].network;
        assert_eq!(center(network, 20), vec![3.0, 4.0]);
        let subnet = network
            .node(NodeId::new(21))
            .unwrap()
            .subnet
            .clone()
            .expect("subnet preserved");
        assert_eq!(center(&subnet, 30), vec![5.0, 6.0]);
        assert_eq!(folded.validate(), Ok(()));
    }

    /// `upgrade_curve_params` reaches every graph of the document: the flat
    /// graph, each layer network, and a subnet inside a layer network.
    #[test]
    fn upgrade_curve_params_reaches_every_graph_of_the_document() {
        use crate::graph::{Node, ParameterValue};
        use crate::id::{DataTypeId, NodeId};

        let v5_curve = |id: u64, points: &str| {
            Node::new(NodeId::new(id), "field.curve_remap")
                .with_input("field", &[DataTypeId::FIELD])
                .with_output("field", DataTypeId::FIELD)
                .with_param("points", ParameterValue::String(points.into()))
        };
        let remapped = |graph: &Graph, id: u64, input: f32| {
            graph
                .node(NodeId::new(id))
                .unwrap_or_else(|| panic!("node {id}"))
                .parameters
                .iter()
                .find(|p| p.key == "points")
                .and_then(|p| p.value.as_curve())
                .unwrap_or_else(|| panic!("node {id} has no curve"))
                .evaluate(input)
        };

        let inner = Graph::new().add_node(v5_curve(30, "0:0,1:3")).unwrap();
        let network = Graph::new()
            .add_node(v5_curve(20, "0:0,1:2"))
            .unwrap()
            .add_node(
                Node::new(NodeId::new(21), "subnet")
                    .with_subnet(inner)
                    .with_output("out", DataTypeId::FIELD),
            )
            .unwrap();
        let comp = Composition::new(
            CompId::new(100),
            "Comp",
            (64, 64),
            FrameRate::new(30, 1),
            100,
        )
        .add_layer(Layer::new(LayerId::new(200), "L", network));
        let document = Document::new(Graph::new().add_node(v5_curve(10, "0:0,1:1")).unwrap())
            .with_composition(comp);

        let upgraded = document.upgrade_curve_params();
        assert_eq!(remapped(&upgraded.graph, 10, 1.0), 1.0);
        let network = &upgraded.get_composition(CompId::new(100)).unwrap().layers[0].network;
        assert_eq!(remapped(network, 20, 1.0), 2.0);
        let subnet = network
            .node(NodeId::new(21))
            .unwrap()
            .subnet
            .clone()
            .expect("subnet preserved");
        assert_eq!(remapped(&subnet, 30, 1.0), 3.0);
        assert_eq!(upgraded.validate(), Ok(()));
    }

    /// Loading upgrades parameter ports in two ways: a legacy pin that
    /// predates `is_param` is flagged, and a port whose stored accepted set
    /// is narrower than what its parameter takes today is widened. Without
    /// the second, an old project would refuse a `VEC4` connection into a
    /// 4-component parameter that an identical new project accepts.
    #[test]
    fn normalize_param_ports_flags_legacy_pins_and_widens_accepted_types() {
        use crate::animation::channel::AnimationChannel;
        use crate::graph::{InputPort, Node, ParameterValue};
        use crate::id::{DataTypeId, NodeId};

        let colour = || {
            ParameterValue::Channel4([
                AnimationChannel::constant(1.0),
                AnimationChannel::constant(1.0),
                AnimationChannel::constant(1.0),
                AnimationChannel::constant(1.0),
            ])
        };
        let narrow_port = |name: &str, is_param: bool| InputPort {
            name: name.into(),
            accepted_types: vec![DataTypeId::COLOR],
            is_param,
            is_variadic: false,
        };
        let mut node = Node::new(NodeId::new(1), "rasterize")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("color", colour())
            .with_param("tint", colour());
        // A v3 pin (never flagged) and a port that was exposed before
        // 4-component parameters accepted VEC4.
        node.inputs.push(narrow_port("color", false));
        node.inputs.push(narrow_port("tint", true));

        let normalized = normalize_param_ports(&Graph::new().add_node(node).unwrap());
        let node = normalized.node(NodeId::new(1)).unwrap();
        for name in ["color", "tint"] {
            let port = node.inputs.iter().find(|p| p.name == name).unwrap();
            assert!(port.is_param, "{name} is a parameter port");
            assert_eq!(
                port.accepted_types,
                vec![DataTypeId::COLOR, DataTypeId::VEC4],
                "{name} takes either reading of its four floats"
            );
        }
        // The data input is untouched, and a second pass changes nothing.
        assert_eq!(
            node.inputs[0].accepted_types,
            vec![DataTypeId::GEOMETRY],
            "an ordinary input is left alone"
        );
        assert!(!node.inputs[0].is_param);
        assert_eq!(normalize_param_ports(&normalized), normalized);
    }

    #[test]
    fn normalize_net_in_ports_appends_the_frame_index_port() {
        use crate::graph::Node;
        use crate::id::{DataTypeId, NodeId};
        use crate::network as net;

        // Pre-`f` layer In node, plus a subnet whose inner In node defines
        // the subnet's pin interface and must keep its shape.
        let inner = Graph::new()
            .add_node(
                Node::new(NodeId::new(20), net::NET_IN_TYPE_KEY)
                    .with_output("gain", DataTypeId::SCALAR),
            )
            .unwrap();
        let network = Graph::new()
            .add_node(
                Node::new(NodeId::new(10), net::NET_IN_TYPE_KEY)
                    .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
                    .with_output(net::PORT_TIME, DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(Node::new(NodeId::new(11), "subnet").with_subnet(inner))
            .unwrap();
        let doc = Document::default().with_composition(test_comp().add_layer(Layer::new(
            LayerId::new(1),
            "L1",
            network,
        )));

        let doc = doc.normalize_net_in_ports();
        let comp = doc.get_composition(CompId::new(1)).unwrap();
        let in_node = net::find_in_node(&comp.layers[0].network).unwrap();
        // Appended at the end so index-addressed edges stay valid.
        assert_eq!(in_node.outputs.len(), 3);
        let appended = in_node.outputs.last().unwrap();
        assert_eq!(appended.name, net::PORT_FRAME_INDEX);
        assert_eq!(appended.data_type, DataTypeId::SCALAR);
        // The subnet's inner In node keeps its pin interface.
        let subnet = comp.layers[0].network.node(NodeId::new(11)).unwrap();
        let inner_in = net::find_in_node(subnet.subnet.as_deref().unwrap()).unwrap();
        assert_eq!(inner_in.outputs.len(), 1);

        // Idempotent.
        let again = doc.clone().normalize_net_in_ports();
        let comp = again.get_composition(CompId::new(1)).unwrap();
        let in_again = net::find_in_node(&comp.layers[0].network).unwrap();
        assert_eq!(in_again.outputs.len(), 3);
    }

    #[test]
    fn excessive_subnet_nesting_is_rejected_iteratively() {
        let mut graph = Graph::new();
        for raw in 1..=(MAX_SUBNET_DEPTH as u64 + 1) {
            graph = Graph::new()
                .add_node(Node::new(NodeId::new(raw), "subnet").with_subnet(graph))
                .unwrap();
        }
        let document = Document::new(graph);
        assert_eq!(
            document.validate(),
            Err(DocumentValidationError::SubnetDepthExceeded {
                limit: MAX_SUBNET_DEPTH
            })
        );
    }

    #[test]
    fn normalize_net_in_ports_keeps_an_existing_f_port() {
        use crate::graph::Node;
        use crate::id::{DataTypeId, NodeId};
        use crate::network as net;

        // A user-defined custom port already claiming the builtin name.
        let network = Graph::new()
            .add_node(
                Node::new(NodeId::new(10), net::NET_IN_TYPE_KEY)
                    .with_output(net::PORT_FRAME_INDEX, DataTypeId::SCALAR),
            )
            .unwrap();
        let doc = Document::default().with_composition(test_comp().add_layer(Layer::new(
            LayerId::new(1),
            "L1",
            network,
        )));
        let doc = doc.normalize_net_in_ports();
        let comp = doc.get_composition(CompId::new(1)).unwrap();
        let in_node = net::find_in_node(&comp.layers[0].network).unwrap();
        assert_eq!(in_node.outputs.len(), 1);
    }

    /// A document whose innermost subnet declares a pin (`stale`, wired from a
    /// constant) that its inner In does not: the drift load-time repair has to
    /// resolve, at the cost of that one edge.
    fn document_with_drifted_subnet_pins() -> Document {
        use crate::graph::Node;
        use crate::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
        use crate::network as net;

        // Innermost subnet: its inner In declares `amount`, its pins say
        // `stale` — the shape a project written before pin derivation holds.
        let innermost = net::new_subnet_inner_graph(NodeId::new(30), NodeId::new(31));
        let innermost = net::add_custom_port(
            innermost,
            NodeId::new(30),
            "amount",
            net::CustomPortType::Float,
            net::NetworkContext::Subnet,
        )
        .unwrap();
        let mut inner_subnet = Node::new(NodeId::new(20), "subnet");
        inner_subnet.inputs = vec![crate::graph::InputPort {
            name: "stale".into(),
            accepted_types: vec![DataTypeId::SCALAR],
            is_param: false,
            is_variadic: false,
        }];
        inner_subnet.subnet = Some(std::sync::Arc::new(innermost));

        // The enclosing subnet holds it beside a constant wired to the stale
        // pin; the edge must survive as the pin it was drawn to disappears.
        let middle = net::new_subnet_inner_graph(NodeId::new(21), NodeId::new(22))
            .add_node(inner_subnet)
            .unwrap()
            .add_node(
                Node::new(NodeId::new(23), "constant").with_output("value", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(200),
                NodeId::new(23),
                OutputPortIndex(0),
                NodeId::new(20),
                InputPortIndex(0),
            )
            .unwrap();
        let mut outer_subnet = Node::new(NodeId::new(10), "subnet");
        outer_subnet.subnet = Some(std::sync::Arc::new(middle));
        let network = Graph::new().add_node(outer_subnet).unwrap();

        Document::default().with_composition(test_comp().add_layer(Layer::new(
            LayerId::new(1),
            "L1",
            network,
        )))
    }

    /// Load-time drift repair reaches every subnet the document owns, at any
    /// depth: pins that disagree with the inner In / Out are rebuilt from it,
    /// the outer edges follow by name, and a second pass changes nothing.
    #[test]
    fn sync_subnet_pins_repairs_nested_drift_on_load() {
        use crate::id::NodeId;
        use crate::network as net;

        let doc = document_with_drifted_subnet_pins().sync_subnet_pins();

        let comp = doc.get_composition(CompId::new(1)).unwrap();
        let outer = comp.layers[0].network.node(NodeId::new(10)).unwrap();
        assert_eq!(
            outer
                .outputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec![net::PORT_FRAME],
            "the enclosing subnet's own pins are derived too"
        );
        let middle = outer.subnet.as_deref().unwrap();
        let inner = middle.node(NodeId::new(20)).unwrap();
        assert_eq!(
            inner
                .inputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["amount"],
            "the nested subnet is repaired from its own inner In"
        );
        assert_eq!(
            inner.parameters.len(),
            1,
            "the promotion parameter comes with the pin"
        );
        assert_eq!(
            middle.edges().count(),
            0,
            "the edge into the vanished pin is gone rather than left on a stale index"
        );

        let again = doc.clone().sync_subnet_pins();
        assert_eq!(again, doc, "the repair is idempotent");
    }

    /// The `WARN`-level output of `f`, as text.
    fn warnings_from(f: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
            type Writer = Self;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let sink = Sink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        String::from_utf8(sink.0.lock().unwrap().clone()).unwrap()
    }

    /// A repair that deletes a pin deletes the outer edges drawn to it, and it
    /// runs on load — so a stored document whose pins drifted loses wiring with
    /// no user action to attribute it to. That has to leave a trace.
    #[test]
    fn a_load_time_pin_removal_is_logged() {
        let logged = warnings_from(|| {
            document_with_drifted_subnet_pins().sync_subnet_pins();
        });
        assert!(
            logged.contains("subnet pin removed on load"),
            "the removal was silent: {logged:?}"
        );
        assert!(
            logged.contains("dropped_edges=1"),
            "and it did not say what the removal cost: {logged:?}"
        );
    }

    /// The same removal reached through an ordinary edit is what the user
    /// asked for, so it stays out of the warning stream: the shared branch in
    /// `network::sync_subnet_pins` logs at debug and only the load-time caller
    /// escalates.
    #[test]
    fn an_edit_time_pin_removal_is_not_warned_about() {
        use crate::id::NodeId;

        let doc = document_with_drifted_subnet_pins();
        let middle = doc.get_composition(CompId::new(1)).unwrap().layers[0]
            .network
            .node(NodeId::new(10))
            .unwrap()
            .subnet
            .as_deref()
            .unwrap()
            .clone();

        let logged = warnings_from(|| {
            let repaired = crate::network::sync_subnet_pins(middle, NodeId::new(20)).unwrap();
            assert!(
                repaired
                    .node(NodeId::new(20))
                    .unwrap()
                    .inputs
                    .iter()
                    .all(|p| p.name != "stale"),
                "the pin really was removed, so the silence is about the log"
            );
        });
        assert_eq!(logged, "", "an edit-path removal warned: {logged:?}");
    }

    #[test]
    fn normalize_node_type_aliases_rewrites_video_to_media() {
        use crate::graph::Node;
        use crate::id::NodeId;

        // `video` nodes in the flat graph, a layer network, and a nested
        // subnet — every place a persisted document can carry one.
        let inner = Graph::new()
            .add_node(Node::new(NodeId::new(20), "video"))
            .unwrap();
        let network = Graph::new()
            .add_node(Node::new(NodeId::new(10), "video"))
            .unwrap()
            .add_node(Node::new(NodeId::new(11), "subnet").with_subnet(inner))
            .unwrap();
        let flat = Graph::new()
            .add_node(Node::new(NodeId::new(30), "video"))
            .unwrap();
        let doc = Document::new(flat).with_composition(test_comp().add_layer(Layer::new(
            LayerId::new(1),
            "L1",
            network,
        )));

        // A persisted document arrives as RON text; parse it directly and
        // normalize, exactly as the archive loader does.
        let text = ron::to_string(&doc).unwrap();
        let parsed: Document = ron::from_str(&text).unwrap();
        let doc = parsed.normalize_node_type_aliases();

        assert_eq!(doc.graph.node(NodeId::new(30)).unwrap().type_key, "media");
        let comp = doc.get_composition(CompId::new(1)).unwrap();
        let network = &comp.layers[0].network;
        assert_eq!(network.node(NodeId::new(10)).unwrap().type_key, "media");
        let subnet = network.node(NodeId::new(11)).unwrap();
        assert_eq!(
            subnet
                .subnet
                .as_deref()
                .unwrap()
                .node(NodeId::new(20))
                .unwrap()
                .type_key,
            "media"
        );

        // Idempotent, and already-canonical keys are untouched.
        let again = doc.clone().normalize_node_type_aliases();
        assert_eq!(again, doc);
    }

    #[test]
    fn normalize_variadic_input_ports_flags_legacy_slots_and_appends_when_connected() {
        use crate::graph::{InputPort, Node};
        use crate::id::{DataTypeId, InputPortIndex, NodeId, OutputPortIndex};
        use crate::registry::{NodeCategory, NodeRegistry, NodeTemplate};

        let mut registry = NodeRegistry::new();
        registry.register(
            NodeTemplate::new("path_array", "Path Array", NodeCategory::Geometry)
                .with_input(InputPort {
                    name: "path".into(),
                    accepted_types: vec![DataTypeId::GEOMETRY],
                    is_param: false,
                    is_variadic: false,
                })
                .with_variadic_input_group(InputPort {
                    name: "instance_source".into(),
                    accepted_types: vec![DataTypeId::GEOMETRY],
                    is_param: false,
                    is_variadic: false,
                }),
        );

        let source = Node::new(NodeId::new(1), "source")
            .with_output("geometry", DataTypeId::GEOMETRY)
            .with_output("scalar", DataTypeId::SCALAR);
        let legacy = Node::new(NodeId::new(2), "path_array")
            .with_input("path", &[DataTypeId::GEOMETRY])
            .with_input("instance_source", &[DataTypeId::GEOMETRY])
            .with_param("count", crate::graph::ParameterValue::Int(10));
        let inner_legacy = Node::new(NodeId::new(4), "path_array")
            .with_input("path", &[DataTypeId::GEOMETRY])
            .with_input("instance_source", &[DataTypeId::GEOMETRY]);
        let subnet = Node::new(NodeId::new(3), "subnet")
            .with_subnet(Graph::new().add_node(inner_legacy).unwrap());
        let network = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(legacy)
            .unwrap()
            .expose_param_port(NodeId::new(2), "count")
            .unwrap()
            .add_node(subnet)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(1),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(1),
                OutputPortIndex(1),
                NodeId::new(2),
                InputPortIndex(2),
            )
            .unwrap();
        let doc = Document::default()
            .with_composition(test_comp().add_layer(Layer::new(LayerId::new(1), "Legacy", network)))
            .normalize_variadic_input_ports(&registry);

        let comp = doc.get_composition(CompId::new(1)).unwrap();
        let network = &comp.layers[0].network;
        let migrated = network.node(NodeId::new(2)).unwrap();
        assert_eq!(migrated.inputs.len(), 4);
        assert_eq!(migrated.inputs[0].name, "path");
        assert!(!migrated.inputs[0].is_variadic);
        assert_eq!(migrated.inputs[1].name, "instance_source");
        assert!(migrated.inputs[1].is_variadic);
        assert_eq!(migrated.inputs[2].name, "instance_source_2");
        assert!(migrated.inputs[2].is_variadic);
        assert_eq!(migrated.inputs[3].name, "count");
        assert!(migrated.inputs[3].is_param);
        assert_eq!(
            network.edge(EdgeId::new(1)).unwrap().target_port,
            InputPortIndex(1),
            "append-only migration preserves the legacy edge index"
        );
        assert_eq!(
            network.edge(EdgeId::new(2)).unwrap().target_port,
            InputPortIndex(3),
            "inserting the empty source slot reindexes the parameter edge"
        );

        let nested = network
            .node(NodeId::new(3))
            .unwrap()
            .subnet
            .as_ref()
            .unwrap()
            .node(NodeId::new(4))
            .unwrap()
            .clone();
        assert_eq!(nested.inputs.len(), 2, "empty legacy slot is reused");
        assert!(nested.inputs[1].is_variadic, "nested subnet is migrated");
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
        assert!((layer.transform.position[0].evaluate(0.0, &ctx) - 0.0).abs() < f32::EPSILON);
        assert!((layer.transform.scale[0].evaluate(0.0, &ctx) - 1.0).abs() < f32::EPSILON);
        assert!((layer.transform.rotation.evaluate(0.0, &ctx) - 0.0).abs() < f32::EPSILON);
        assert!((layer.opacity.evaluate(0.0, &ctx) - 1.0).abs() < f32::EPSILON);
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

    /// A structural-sharing edit must report the edited layer and nothing
    /// else — and "nothing else" has to survive both ways of getting it
    /// wrong: reporting untouched layers whose networks are pointer-shared,
    /// and reporting a layer whose network was rebuilt into fresh
    /// allocations but holds identical content.
    #[test]
    fn changed_network_paths_reports_only_the_edited_layer() {
        use crate::id::{DataTypeId, NodeId};

        let populated = |seed: u64| {
            Graph::new()
                .add_node(
                    crate::graph::Node::new(NodeId::new(seed), "constant")
                        .with_output("value", DataTypeId::SCALAR),
                )
                .unwrap()
        };

        // Eight layers, each with its own non-empty network.
        let base_layers: Vec<Layer> = (1..=8)
            .map(|i| {
                let mut layer = empty_layer(i);
                layer.network = populated(100 + i);
                layer
            })
            .collect();
        let comp = base_layers
            .iter()
            .cloned()
            .fold(test_comp(), |c, l| c.add_layer(l));
        let doc1 = Document::default().with_composition(comp.clone());

        // Edit layer 5 only, cloning the rest — every other layer keeps the
        // very same `Graph` allocation.
        let edited: im::Vector<Layer> = comp
            .layers
            .iter()
            .map(|l| {
                if l.id == LayerId::new(5) {
                    let mut l = l.clone();
                    l.network = populated(999);
                    l
                } else {
                    l.clone()
                }
            })
            .collect();
        let doc2 = Document::default().with_composition(Composition {
            layers: edited,
            ..comp.clone()
        });
        assert_eq!(
            doc2.changed_network_paths(&doc1),
            vec![vec![PathSegment::Layer(CompId::new(1), LayerId::new(5))]],
            "only the edited layer's network changed"
        );

        // Rebuild layer 3's network from scratch with identical content: a
        // different allocation, so `ptr_eq` says nothing, and the deep
        // compare behind it has to say "equal".
        let rebuilt: im::Vector<Layer> = comp
            .layers
            .iter()
            .map(|l| {
                if l.id == LayerId::new(3) {
                    let mut l = l.clone();
                    l.network = populated(103);
                    l
                } else {
                    l.clone()
                }
            })
            .collect();
        let doc3 = Document::default().with_composition(Composition {
            layers: rebuilt,
            ..comp
        });
        assert!(
            doc3.changed_network_paths(&doc1).is_empty(),
            "an equal network rebuilt into a fresh allocation is not a change"
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

        // A fully-dressed layer: keyframed transform/opacity/audio channels,
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
            audio: Some(AudioSource {
                asset_id: "audio".into(),
                stream_index: 2,
                gain: keyframed_channel(&[(0, 1.0), (30, 0.5)]),
                fade_in_frames: 3,
                fade_out_frames: 7,
                audio_muted: true,
            }),
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
            .with_media_asset_entry(
                "plate",
                MediaAssetEntry {
                    resolved: None,
                    ..MediaAssetEntry::from_absolute("/tmp/media/plate.mov")
                },
            )
            .with_media_asset_entry(
                "audio",
                MediaAssetEntry {
                    resolved: None,
                    ..MediaAssetEntry::from_absolute("/tmp/media/mix.wav")
                },
            )
            // The external contract persists with the document it describes
            // (REQ-PROJ-006, `.ravprj` v7).
            .with_exposed_parameters(sample_declarations());

        let text = ron::to_string(&doc).unwrap();
        let restored: Document = ron::from_str(&text).unwrap();
        // `MediaAssetEntry::resolved` is runtime-only, so the restored
        // document is offline until the host re-resolves it. Everything
        // else must match exactly.
        assert!(
            restored
                .media_assets
                .values()
                .all(|entry| entry.resolved.is_none())
        );
        assert_eq!(
            doc.clone().with_resolved_assets(None, &HashMap::new()),
            restored.clone().with_resolved_assets(None, &HashMap::new()),
        );

        assert_eq!(restored.exposed_parameters, sample_declarations());

        // Diff-friendly persistence: serializing twice is byte-identical.
        assert_eq!(text, ron::to_string(&doc).unwrap());
    }

    /// Three declarations covering a scalar, a colour and a media reference —
    /// the three shapes an [`ExposedValue`](crate::exposed::ExposedValue) takes
    /// (a plain constant, a component value, an asset path).
    fn sample_declarations() -> ExposedParameters {
        use crate::exposed::{ExposedBinding, ExposedParameter, ExposedType, ExposedValue};

        ExposedParameters::from_declarations([
            ExposedParameter::new(
                "headline",
                ExposedType::String,
                ExposedValue::String("Ravel".into()),
                ExposedBinding::new(NodeId::new(2), "text"),
            )
            .unwrap()
            .with_description("The title card's text"),
            ExposedParameter::inferred(
                "tint",
                ExposedValue::Color(Color::new(1.0, 0.5, 0.25, 1.0)),
                ExposedBinding::new(NodeId::new(2), "color"),
            )
            .unwrap(),
            ExposedParameter::inferred(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/plate.mov".into())),
                ExposedBinding::new(NodeId::new(1), "asset_id"),
            )
            .unwrap(),
        ])
        .expect("the names differ")
    }

    /// A `.ravprj` v6 document has no `exposed_parameters` field at all. It
    /// must read as a project with no external contract, not fail the load.
    #[test]
    fn a_document_without_declarations_reads_as_zero_declarations() {
        let doc = Document::new(Graph::new()).with_composition(test_comp());
        let text = ron::to_string(&doc).unwrap();

        // The v6 shape is this document minus the field v7 added. The field is
        // written last, so cutting from its name to the closing paren leaves a
        // v6 document rather than a truncated one.
        let (head, _) = text
            .rsplit_once("exposed_parameters:")
            .expect("the field is written");
        let v6_text = format!("{})", head.trim_end().trim_end_matches(','));
        assert!(
            !v6_text.contains("exposed_parameters"),
            "the v6 shape has no such field: {v6_text}"
        );

        let restored: Document = ron::from_str(&v6_text)
            .unwrap_or_else(|err| panic!("a v6 document still parses: {err} in {v6_text}"));
        assert!(restored.exposed_parameters.is_empty());
        assert_eq!(restored, doc, "everything else reads unchanged");
    }

    #[test]
    fn audio_source_missing_fields_use_forward_compatible_defaults() {
        let source: AudioSource = ron::from_str(r#"AudioSource(asset_id: "clip")"#).unwrap();
        assert_eq!(source.asset_id, "clip");
        assert_eq!(source.stream_index, 0);
        assert_eq!(source.fade_in_frames, 0);
        assert_eq!(source.fade_out_frames, 0);
        assert!(!source.audio_muted);
        let ctx = crate::eval::EvalContext::new(0, FrameRate::new(30, 1), (16, 16));
        assert!((source.gain.evaluate(0.0, &ctx) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn layer_audio_ron_uses_the_struct_named_option_shape() {
        let mut layer = empty_layer(1);
        layer.audio = Some(AudioSource::new("dialogue", 3));
        let text =
            ron::ser::to_string_pretty(&layer, ron::ser::PrettyConfig::new().struct_names(true))
                .unwrap();
        assert!(text.contains("audio: Some(AudioSource("), "{text}");
        let restored: Layer = ron::from_str(&text).unwrap();
        assert_eq!(restored, layer);
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

    #[test]
    fn normalize_upgrades_legacy_param_pins() {
        use crate::animation::channel::AnimationChannel;
        use crate::graph::{Node, ParameterValue};
        use crate::id::{DataTypeId, NodeId};

        // Legacy rasterize shape: a non-param `color` COLOR pin shadowing
        // the `color` Channel4 parameter (as old .ravprj files carry it).
        let legacy = Node::new(NodeId::new(1), "rasterize")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_input("color", &[DataTypeId::COLOR])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param(
                "color",
                ParameterValue::Channel4([
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                    AnimationChannel::constant(1.0),
                ]),
            );
        // A subnet whose pin/parameter pair is the promotion mechanism —
        // must NOT be upgraded — wrapping an inner legacy node that must.
        let mut inner_legacy = legacy.clone();
        inner_legacy.id = NodeId::new(3);
        let inner = Graph::new().add_node(inner_legacy).unwrap();
        let subnet_node = Node::new(NodeId::new(2), "subnet")
            .with_input("amount", &[DataTypeId::SCALAR])
            .with_param("amount", ParameterValue::Float(1.0))
            .with_subnet(inner);
        let network = Graph::new()
            .add_node(legacy)
            .unwrap()
            .add_node(subnet_node)
            .unwrap();
        let comp = test_comp().add_layer(Layer::new(LayerId::new(1), "A", network));
        let doc = Document::default()
            .with_composition(comp)
            .normalize_param_ports();

        let comp = doc.compositions.values().next().unwrap();
        let layer = comp.layers.front().unwrap();
        let node = layer.network.node(NodeId::new(1)).unwrap();
        assert!(!node.inputs[0].is_param, "data port untouched");
        assert!(node.inputs[1].is_param, "legacy color pin upgraded");
        let subnet = layer.network.node(NodeId::new(2)).unwrap();
        assert!(
            !subnet.inputs[0].is_param,
            "subnet promotion pins stay plain"
        );
        let nested = subnet
            .subnet
            .as_ref()
            .unwrap()
            .node(NodeId::new(3))
            .unwrap();
        assert!(nested.inputs[1].is_param, "nested legacy pin upgraded");
        assert!(doc.validate().is_ok());
    }

    #[test]
    fn validate_rejects_param_ports_without_parameters() {
        use crate::graph::{InputPort, Node};
        use crate::id::{DataTypeId, NodeId};

        let mut node = Node::new(NodeId::new(7), "blur").with_output("out", DataTypeId::SCALAR);
        node.inputs.push(InputPort {
            name: "radius".into(),
            accepted_types: vec![DataTypeId::SCALAR],
            is_param: true,
            is_variadic: false,
        });
        let network = Graph::new().add_node(node).unwrap();
        let comp = test_comp().add_layer(Layer::new(LayerId::new(1), "A", network));
        let doc = Document::default().with_composition(comp);
        assert_eq!(
            doc.validate(),
            Err(DocumentValidationError::ParamPortWithoutParameter {
                node: NodeId::new(7),
                key: "radius".into(),
            })
        );
    }
}
