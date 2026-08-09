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
use ravel_core::exposed::KeyRename;
use ravel_core::graph::{Graph, Node, ParameterValue, PortSide};
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_core::network::PinRename;
use ravel_core::registry::NodeRegistry;
use ravel_core::types::{Color, FrameRate};
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

    /// Where this network sits in the ownership hierarchy, as the core's
    /// two-value answer.
    ///
    /// `ravel-core` decides which custom port types an In node may declare
    /// from this (REQ-LAYER-002/003) but cannot see a `NetworkPath` — that
    /// type lives here, and the core must work without a UI — so every caller
    /// crossing into `ravel_core::network` collapses the path first. Doing it
    /// once, here, keeps `subnets.is_empty()` from being re-decided per panel.
    pub fn context(&self) -> ravel_core::network::NetworkContext {
        if self.subnets.is_empty() {
            ravel_core::network::NetworkContext::LayerRoot
        } else {
            ravel_core::network::NetworkContext::Subnet
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

    /// Restore a gesture's begin snapshot and discard every undo version that
    /// was committed after it. This protects cancellation when another panel
    /// commits the shared live document while the gesture is in progress.
    pub fn restore_snapshot(&mut self, snapshot: Document) -> bool {
        if self.live == snapshot && !self.dirty {
            return false;
        }
        if !self.undo.rollback_to(&snapshot) {
            return false;
        }
        self.live = snapshot;
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

/// The default startup document: one empty root composition at `frame_rate`.
///
/// The rate is a parameter because it is the one field of this document a
/// setting decides: the caller resolves the default frame rate
/// (`ravel_app::app_settings::default_frame_rate`) and this crate stays free of
/// the settings layers. Everything else is
/// [`CompositionSettings::fallback`]'s format, which is what a document with
/// nothing to inherit from is.
pub fn default_document(frame_rate: FrameRate) -> Document {
    Document::default().with_composition(Composition::new(
        CompId::next(),
        "Comp 1",
        CompositionSettings::FALLBACK_RESOLUTION,
        frame_rate,
        CompositionSettings::FALLBACK_DURATION,
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

/// Deep-copy `layer` and insert the duplicate immediately above it in the
/// bottom-to-top composition stack. The duplicate receives fresh layer,
/// node, and edge ids and the conventional `" copy"` name suffix.
pub fn duplicate_layer(doc: &Document, comp: CompId, layer: LayerId) -> Option<Document> {
    let composition = doc.get_composition(comp)?;
    let source_index = composition
        .layers
        .iter()
        .position(|item| item.id == layer)?;
    let source = composition.layers[source_index].clone();
    let mut duplicate = source.duplicate_with_fresh_ids(LayerId::next());
    duplicate.name = format!("{} copy", source.name);
    update_composition(doc, comp, |c| c.insert_layer(source_index + 1, duplicate))
}

/// Remove `layer` (its owned network is dropped with it, REQ-LAYER-009).
pub fn remove_layer(doc: &Document, comp: CompId, layer: LayerId) -> Option<Document> {
    update_composition(doc, comp, |c| c.remove_layer(layer))
}

/// Apply `f` to every named layer of `comp` in one document (REQ-UI-013 bulk
/// editing): the result is a single snapshot, so the whole selection is one
/// undo step. Ids that are not in the composition are skipped; `None` when
/// nothing was edited.
pub fn update_layers(
    doc: &Document,
    comp: CompId,
    layers: &[LayerId],
    mut f: impl FnMut(&mut Layer),
) -> Option<Document> {
    let mut next: Option<Document> = None;
    for layer in layers {
        let base = next.as_ref().unwrap_or(doc);
        if let Some(updated) = update_layer(base, comp, *layer, &mut f) {
            next = Some(updated);
        }
    }
    next
}

/// Remove every named layer of `comp` in one document, skipping locked layers
/// (they are protected from destructive operations, REQ-UI-013). `None` when
/// nothing was removed.
pub fn remove_layers(doc: &Document, comp: CompId, layers: &[LayerId]) -> Option<Document> {
    let mut next: Option<Document> = None;
    for layer in layers {
        let base = next.as_ref().unwrap_or(doc);
        let removable = base
            .get_composition(comp)
            .and_then(|c| c.get_layer(*layer))
            .is_some_and(|l| !l.locked);
        if !removable {
            continue;
        }
        if let Some(updated) = remove_layer(base, comp, *layer) {
            next = Some(updated);
        }
    }
    next
}

/// Duplicate every named layer of `comp` in one document, each copy directly
/// above its source ([`duplicate_layer`]). Returns the new document and the
/// copies in the order the sources were given, so the caller can select them.
/// `None` when nothing was duplicated.
pub fn duplicate_layers(
    doc: &Document,
    comp: CompId,
    layers: &[LayerId],
) -> Option<(Document, Vec<LayerId>)> {
    let mut next: Option<Document> = None;
    let mut copies = Vec::new();
    for layer in layers {
        let base = next.as_ref().unwrap_or(doc);
        let Some(source_index) = base
            .get_composition(comp)
            .and_then(|c| c.layers.iter().position(|item| item.id == *layer))
        else {
            continue;
        };
        let Some(updated) = duplicate_layer(base, comp, *layer) else {
            continue;
        };
        if let Some(copy) = updated
            .get_composition(comp)
            .and_then(|c| c.layers.get(source_index + 1))
            .map(|layer| layer.id)
        {
            copies.push(copy);
        }
        next = Some(updated);
    }
    next.map(|document| (document, copies))
}

/// Cut `layer` in two at composition frame `comp_frame` (After Effects'
/// "Split Layer"), leaving the halves stacked with the later one directly
/// above the earlier one.
///
/// The source keeps its id and becomes the part before the cut
/// (`out_frame` pulled back to the cut); the part after the cut is a
/// [`Layer::duplicate_with_fresh_ids`] copy placed at `comp_frame` with its
/// `in_frame` at the cut. It keeps the source's name — the halves are one
/// layer that was cut, not a copy of it.
///
/// **Nothing inside the layer is rewritten.** A layer maps composition time
/// to local time as `comp - start_frame + in_frame`, and the copy shifts
/// `start_frame` and `in_frame` by the same amount, so the mapping is
/// identical on both sides: keyframes, the owned network, the shell channels
/// and the audio source all stay where they were, and the two halves cover
/// exactly the source's original composition range.
///
/// `None` — no cut — when the layer is missing, locked, or when `comp_frame`
/// is not strictly inside its range (a cut at either edge would produce an
/// empty half).
pub fn split_layer(
    doc: &Document,
    comp: CompId,
    layer: LayerId,
    comp_frame: i64,
) -> Option<Document> {
    let composition = doc.get_composition(comp)?;
    let index = composition.layers.iter().position(|l| l.id == layer)?;
    let source = composition.layers[index].clone();
    if source.locked {
        return None;
    }
    // Deliberately not `Layer::local_frame`: that clamps at zero, which would
    // report a cut before the layer as a cut at its first frame.
    let local = comp_frame - source.start_frame + source.in_frame as i64;
    if local <= source.in_frame as i64 || local >= source.out_frame as i64 {
        return None;
    }
    let local = local as u64;
    let mut tail = source.duplicate_with_fresh_ids(LayerId::next());
    tail.start_frame = comp_frame;
    tail.in_frame = local;
    update_composition(doc, comp, |mut c| {
        let mut head = c.layers[index].clone();
        head.out_frame = local;
        c.layers.set(index, head);
        c.insert_layer(index + 1, tail)
    })
}

/// [`split_layer`] over every named layer, in one document so the whole
/// selection is one undo step. Returns the new document and the ids of the
/// layers created after the cut, in source order. `None` when nothing split.
pub fn split_layers(
    doc: &Document,
    comp: CompId,
    layers: &[LayerId],
    comp_frame: i64,
) -> Option<(Document, Vec<LayerId>)> {
    let mut next: Option<Document> = None;
    let mut tails = Vec::new();
    for layer in layers {
        let base = next.as_ref().unwrap_or(doc);
        // Re-found per iteration: an earlier split inserted a layer and moved
        // every index above it.
        let Some(index) = base
            .get_composition(comp)
            .and_then(|c| c.layers.iter().position(|item| item.id == *layer))
        else {
            continue;
        };
        let Some(updated) = split_layer(base, comp, *layer, comp_frame) else {
            continue;
        };
        if let Some(tail) = updated
            .get_composition(comp)
            .and_then(|c| c.layers.get(index + 1))
            .map(|layer| layer.id)
        {
            tails.push(tail);
        }
        next = Some(updated);
    }
    next.map(|document| (document, tails))
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

/// Override a freshly created shape generator's `center` with `center` so
/// new shapes start at the composition center instead of the registry
/// default `(0, 0)`. Non-shape nodes and nodes without a center param are
/// untouched; existing documents are never rewritten.
pub fn apply_shape_center_default(node: &mut Node, center: (f32, f32)) {
    if !node.type_key.starts_with("shape.") {
        return;
    }
    for param in node.parameters.iter_mut() {
        if param.key == "center" {
            param.value = ParameterValue::vec2(center.0, center.1);
        }
    }
}

/// Apply [`apply_shape_center_default`] to every shape generator in a
/// freshly instantiated network.
fn center_shape_generators(mut network: Graph, resolution: (u32, u32)) -> Graph {
    let center = (resolution.0 as f32 * 0.5, resolution.1 as f32 * 0.5);
    let shapes: Vec<std::sync::Arc<Node>> = network
        .nodes()
        .filter(|node| node.type_key.starts_with("shape."))
        .cloned()
        .collect();
    for node in shapes {
        let mut updated = (*node).clone();
        apply_shape_center_default(&mut updated, center);
        network = network.replace_node(std::sync::Arc::new(updated));
    }
    network
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
    let network = center_shape_generators(template.instantiate(registry)?, composition.resolution);
    let name = unique_layer_name(composition, &template.display_name);
    let id = LayerId::next();
    let mut layer = Layer::new(id, name, network).with_time(0, 0, composition.duration_frames);
    if template.key == "audio" {
        layer.audio = Some(ravel_core::composition::AudioSource::default());
    }
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

/// Placement and binding of one imported media layer
/// (REQ-UI-010, media-import plan unit 3).
pub struct MediaLayerSpec<'a> {
    /// Base of the layer name (uniquified within the composition).
    pub name_base: &'a str,
    /// Asset id the media node is bound to.
    pub asset_id: &'a str,
    /// Composition frame the layer starts at (the playhead on import).
    pub start_frame: i64,
    /// Source-local end frame: the asset's length in composition frames, or
    /// the composition length when the duration is unknown.
    pub out_frame: u64,
    /// Container stream index of the audio stream the shell plays, or `None`
    /// for silent media (audio-plan unit 4). The shell's audio is explicit:
    /// nothing ever scans the network for "a media node with sound".
    pub audio_stream_index: Option<usize>,
}

/// Instantiate `template` (the `media` layer template) into a layer whose
/// media node is bound to `spec.asset_id`, placed at `spec.start_frame` with
/// the source range `0..spec.out_frame` (REQ-UI-010, media-import plan
/// unit 3).
///
/// Unlike [`add_layer_from_template`] — which spans the whole composition
/// and leaves the media node's `asset_id` unset — this places the layer at
/// the playhead with the imported asset's own length and fills the
/// `asset_id` parameter, so the layer evaluates immediately.
///
/// When `spec.audio_stream_index` is set, the shell also gets an
/// [`AudioSource`](ravel_core::composition::AudioSource) for the **same**
/// asset id, which is how a video layer's sound is wired
/// (`docs/implementation/audio-plan.md`, unit 4): the audio is a shell
/// property, so its timing follows the same `start_frame` / `in_frame` /
/// `out_frame` the network is evaluated with.
pub fn add_media_layer(
    doc: &Document,
    comp: CompId,
    template: &LayerTemplate,
    registry: &NodeRegistry,
    spec: MediaLayerSpec<'_>,
) -> Result<Option<(Document, LayerId)>, TemplateError> {
    let Some(composition) = doc.get_composition(comp) else {
        return Ok(None);
    };
    let network = bind_media_asset_id(template.instantiate(registry)?, spec.asset_id);
    let name = unique_layer_name(composition, spec.name_base);
    let id = LayerId::next();
    // A zero-length source range would make the layer invisible; keep at
    // least one frame.
    let mut layer =
        Layer::new(id, name, network).with_time(spec.start_frame, 0, spec.out_frame.max(1));
    layer.audio = spec
        .audio_stream_index
        .map(|stream_index| ravel_core::composition::AudioSource::new(spec.asset_id, stream_index));
    Ok(add_layer(doc, comp, layer).map(|doc| (doc, id)))
}

/// Set the `asset_id` parameter on every media node in a freshly
/// instantiated network (`media`, with `video` accepted as the persisted
/// alias).
fn bind_media_asset_id(mut network: Graph, asset_id: &str) -> Graph {
    let media_nodes: Vec<std::sync::Arc<Node>> = network
        .nodes()
        .filter(|node| matches!(node.type_key.as_str(), "media" | "video"))
        .cloned()
        .collect();
    for node in media_nodes {
        let mut updated = (*node).clone();
        match updated
            .parameters
            .iter_mut()
            .find(|param| param.key == "asset_id")
        {
            Some(param) => param.value = ParameterValue::String(asset_id.to_string()),
            None => updated.parameters.push(ravel_core::graph::Parameter {
                key: "asset_id".to_string(),
                value: ParameterValue::String(asset_id.to_string()),
            }),
        }
        network = network.replace_node(std::sync::Arc::new(updated));
    }
    network
}

// ---------------------------------------------------------------------------
// Composition management (REQ-UI-013)
// ---------------------------------------------------------------------------

/// The editable settings of a composition — everything the New/Settings
/// dialogs and the Properties composition target work with.
///
/// These are plain fields, not `ParameterValue`s: a composition's resolution
/// and frame rate cannot be animated, so no channel or keyframe machinery is
/// involved. [`CompositionSettings::sanitized`] is the single place that keeps
/// a composition constructible (no zero resolution, no zero frame rate).
#[derive(Clone, Debug, PartialEq)]
pub struct CompositionSettings {
    pub name: String,
    pub resolution: (u32, u32),
    pub frame_rate: FrameRate,
    pub duration_frames: u64,
    pub background_color: Color,
}

impl CompositionSettings {
    /// Default settings for a project that has nothing to inherit from.
    pub const FALLBACK_RESOLUTION: (u32, u32) = (1920, 1080);
    pub const FALLBACK_FRAME_RATE: FrameRate = FrameRate::new(30, 1);
    pub const FALLBACK_DURATION: u64 = 300;

    /// Settings for a project with nothing to inherit from.
    pub fn fallback(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            resolution: Self::FALLBACK_RESOLUTION,
            frame_rate: Self::FALLBACK_FRAME_RATE,
            duration_frames: Self::FALLBACK_DURATION,
            background_color: Color::BLACK,
        }
    }

    pub fn from_composition(comp: &Composition) -> Self {
        Self {
            name: comp.name.clone(),
            resolution: comp.resolution,
            frame_rate: comp.frame_rate,
            duration_frames: comp.duration_frames,
            background_color: comp.background_color,
        }
    }

    /// Clamp every field into the range a composition can actually hold: at
    /// least one pixel in each axis, a non-zero frame rate, and at least one
    /// frame of duration (a zero-length composition has no frame to show).
    pub fn sanitized(&self) -> Self {
        Self {
            name: self.name.clone(),
            resolution: (self.resolution.0.max(1), self.resolution.1.max(1)),
            frame_rate: FrameRate::new(self.frame_rate.num.max(1), self.frame_rate.den.max(1)),
            duration_frames: self.duration_frames.max(1),
            background_color: self.background_color,
        }
    }

    /// Build a fresh composition with these settings.
    pub fn into_composition(self, id: CompId) -> Composition {
        let settings = self.sanitized();
        let mut comp = Composition::new(
            id,
            settings.name,
            settings.resolution,
            settings.frame_rate,
            settings.duration_frames,
        );
        comp.background_color = settings.background_color;
        comp
    }

    /// Apply these settings to an existing composition, keeping its layers.
    pub fn apply_to(self, mut comp: Composition) -> Composition {
        let settings = self.sanitized();
        comp.name = settings.name;
        comp.resolution = settings.resolution;
        comp.frame_rate = settings.frame_rate;
        comp.duration_frames = settings.duration_frames;
        comp.background_color = settings.background_color;
        comp
    }
}

/// Compositions in display order — sorted by id, the same order the document
/// serializes them in and the Outliner lists them in.
pub fn compositions_in_order(doc: &Document) -> Vec<&Composition> {
    let mut comps: Vec<&Composition> = doc
        .compositions
        .values()
        .map(|comp| comp.as_ref())
        .collect();
    comps.sort_by_key(|comp| comp.id);
    comps
}

/// The composition that should take over when `comp` goes away: the next one
/// in display order, or the previous one when `comp` is last. `None` when
/// `comp` is the only composition.
pub fn neighbour_composition(doc: &Document, comp: CompId) -> Option<CompId> {
    let ids: Vec<CompId> = compositions_in_order(doc)
        .into_iter()
        .map(|c| c.id)
        .collect();
    let index = ids.iter().position(|id| *id == comp)?;
    ids.get(index + 1)
        .or_else(|| index.checked_sub(1).and_then(|prev| ids.get(prev)))
        .copied()
}

/// A composition name not yet used in `doc`: `base`, else `base 2`, `base 3`…
pub fn unique_composition_name(doc: &Document, base: &str) -> String {
    let taken = |candidate: &str| doc.compositions.values().any(|c| c.name == candidate);
    if !taken(base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base} {n}");
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Default name for a new composition (`Comp 1`, `Comp 2`, …).
pub fn next_composition_name(doc: &Document) -> String {
    let mut n = doc.compositions.len() + 1;
    loop {
        let candidate = format!("Comp {n}");
        if !doc.compositions.values().any(|c| c.name == candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Insert a composition, returning the new document and the composition's id.
///
/// A document with no root composition adopts this one as its root — the model
/// root names what a reopened document starts on, so the first composition of
/// an empty project has to fill it (which composition is *active* stays UI
/// state, see `panels::ActiveComposition`).
pub fn add_composition(doc: &Document, settings: CompositionSettings) -> (Document, CompId) {
    let id = CompId::next();
    let mut next = doc.clone();
    next.compositions
        .insert(id, std::sync::Arc::new(settings.into_composition(id)));
    if next.root_comp.is_none() {
        next.root_comp = Some(id);
    }
    (next, id)
}

/// Deep-copy `comp` under a fresh id: fresh layer ids and fresh ids throughout
/// every layer's network, so the copy shares no identity with the original.
///
/// Parent links are re-pointed at the copies. `duplicate_with_fresh_ids` only
/// remaps what a layer owns, and `parent` names a *sibling*, so carrying it
/// over verbatim would leave every copy pointing into the source composition —
/// a `ValidationError::ParentNotFound` the next time the document is checked.
pub fn duplicate_composition(doc: &Document, comp: CompId) -> Option<(Document, CompId)> {
    let source = doc.get_composition(comp)?;
    let id = CompId::next();
    let mut copy = source.as_ref().clone();
    copy.id = id;
    copy.name = unique_composition_name(doc, &format!("{} copy", source.name));
    let id_map: std::collections::HashMap<LayerId, LayerId> = source
        .layers
        .iter()
        .map(|layer| (layer.id, LayerId::next()))
        .collect();
    copy.layers = source
        .layers
        .iter()
        .map(|layer| {
            let mut duplicate = layer.duplicate_with_fresh_ids(id_map[&layer.id]);
            // A parent outside this composition cannot exist (the model only
            // parents within one stack), so an unmapped id means the source
            // was already inconsistent — drop the link rather than copy the
            // dangling one forward.
            duplicate.parent = duplicate
                .parent
                .and_then(|parent| id_map.get(&parent).copied());
            duplicate
        })
        .collect();
    let mut next = doc.clone();
    next.compositions.insert(id, std::sync::Arc::new(copy));
    Some((next, id))
}

/// Remove a composition. When it was the model root, the root moves to the
/// neighbour in display order (or `None` for the last composition) so no
/// document ever names a composition it does not have.
pub fn remove_composition(doc: &Document, comp: CompId) -> Option<Document> {
    if !doc.compositions.contains_key(&comp) {
        return None;
    }
    let successor = neighbour_composition(doc, comp);
    let mut next = doc.clone();
    next.compositions.remove(&comp);
    if next.root_comp == Some(comp) {
        next.root_comp = successor;
    }
    Some(next)
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
///
/// Every subnet node on that chain has its pins re-derived from the inner
/// graph it now owns ([`ravel_core::network::sync_subnet_pins`]): editing a
/// subnet's inner In / Out **is** editing the enclosing node's interface, and
/// the two have to reach the document together or an outer edge is left
/// pointing at a pin index that no longer means what it did. Doing it here
/// rather than in the caller keeps it inside the caller's single Document
/// commit, so one inner edit is still one undo step, and covers every writer
/// of a nested network at once.
pub fn replace_network(doc: &Document, path: &NetworkPath, network: Graph) -> Option<Document> {
    replace_network_renaming_pin(doc, path, network, None)
}

/// [`replace_network`] told that the edit renamed a custom port of the
/// network's own In / Out node.
///
/// Pin sync matches by name, so without this the enclosing subnet node's pin
/// is removed and re-added: the outer edges, the promoted parameter's value
/// and keyframes, and any `NodeOutput` binding that named the pin all go with
/// it. [`ravel_core::network::rename_subnet_pin`] moves the pin first, and the
/// declarations bound to the promoted parameter follow in the same snapshot —
/// the same obligation [`ravel_core::network::rename_custom_port`] discharges
/// one level down, one level up.
///
/// Only the subnet that **directly owns** the edited graph is affected: an
/// ancestor's pins derive from its own In / Out, which this edit did not
/// touch. A rename with no enclosing subnet (a layer root) changes nothing.
pub fn replace_network_renaming_pin(
    doc: &Document,
    path: &NetworkPath,
    network: Graph,
    pin_rename: Option<&PinRename>,
) -> Option<Document> {
    let layer = doc.get_composition(path.comp)?.get_layer(path.layer)?;
    let rebuilt = rebuild_subnets(&layer.network, &path.subnets, network, pin_rename)?;
    let doc = update_layer(doc, path.comp, path.layer, |l| l.network = rebuilt)?;
    Some(match (pin_rename, path.subnets.last()) {
        (Some(rename), Some(subnet)) if rename.side == PortSide::Input => {
            ravel_core::exposed::apply::follow_key_rename(
                doc,
                &KeyRename::new(*subnet, rename.old_name.clone(), rename.new_name.clone()),
            )
        }
        _ => doc,
    })
}

/// Replace the graph reached through `subnets` inside `graph` with `leaf`,
/// re-wrapping each ancestor subnet node on the way back up and re-deriving
/// its pins from the graph it ends up owning.
fn rebuild_subnets(
    graph: &Graph,
    subnets: &[NodeId],
    leaf: Graph,
    pin_rename: Option<&PinRename>,
) -> Option<Graph> {
    let Some((first, rest)) = subnets.split_first() else {
        return Some(leaf);
    };
    let node = graph.node(*first)?;
    let inner = node.subnet.as_deref()?;
    let new_inner = rebuild_subnets(inner, rest, leaf, pin_rename)?;
    let mut updated = (**node).clone();
    updated.subnet = Some(std::sync::Arc::new(new_inner));
    let rebuilt = graph.clone().replace_node(std::sync::Arc::new(updated));
    // `rest.is_empty()` is the subnet that owns the edited graph — the only
    // one whose pins the rename names.
    let rebuilt = match pin_rename.filter(|_| rest.is_empty()) {
        Some(rename) => ravel_core::network::rename_subnet_pin(rebuilt, *first, rename),
        None => rebuilt,
    };
    // A node that is not a subnet cannot be on this chain (`node.subnet` just
    // answered), so a refusal here would be a bug rather than a state to
    // carry. It is logged rather than dropped, and the edit still reaches the
    // document with the pins it had.
    Some(ravel_core::network::sync_subnet_pins_or_log(
        rebuilt, *first,
    ))
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
        let doc = Document::default().with_composition(comp);
        // Mirror what loading a document does (REQ-LAYER-009): push the id
        // counters past the ids stamped above. Without this, `LayerId::next()`
        // can hand back an id this document already uses — the fixture takes
        // explicit ids while the counter starts at zero — so whether a test
        // that treats a fresh id as "absent" passes depends on how many other
        // tests ran first.
        doc.advance_id_counters();
        (doc, comp_id)
    }

    /// Bulk editing is one snapshot: `DocumentStore::commit` on the result of
    /// one call is one undo step for the whole selection (REQ-UI-013).
    #[test]
    fn update_layers_edits_the_whole_selection_in_one_document() {
        let (doc, comp) = doc_with_layers(3);
        let selection = [LayerId::new(1), LayerId::new(3)];
        let updated = update_layers(&doc, comp, &selection, |layer| layer.muted = true).unwrap();

        let comp_of = |doc: &Document| doc.get_composition(comp).unwrap().clone();
        let muted =
            |doc: &Document, id: u64| comp_of(doc).get_layer(LayerId::new(id)).unwrap().muted;
        assert!(muted(&updated, 1) && muted(&updated, 3));
        assert!(!muted(&updated, 2), "unselected layers are untouched");

        assert!(
            update_layers(&doc, comp, &[LayerId::new(99)], |l| l.muted = true).is_none(),
            "nothing to edit is None, not an empty snapshot"
        );

        let mut store = DocumentStore::new(doc);
        store.commit(updated);
        assert!(store.undo());
        assert!(
            !muted(store.document(), 1) && !muted(store.document(), 3),
            "one undo restores every edited layer"
        );
    }

    /// Locked layers are protected from a bulk delete; the rest still go.
    #[test]
    fn remove_layers_skips_locked_layers() {
        let (doc, comp) = doc_with_layers(3);
        let doc = update_layer(&doc, comp, LayerId::new(2), |l| l.locked = true).unwrap();

        let removed = remove_layers(
            &doc,
            comp,
            &[LayerId::new(1), LayerId::new(2), LayerId::new(3)],
        )
        .unwrap();
        let layers: Vec<LayerId> = removed
            .get_composition(comp)
            .unwrap()
            .layers
            .iter()
            .map(|l| l.id)
            .collect();
        assert_eq!(layers, vec![LayerId::new(2)]);

        assert!(
            remove_layers(&doc, comp, &[LayerId::new(2)]).is_none(),
            "a locked-only selection removes nothing"
        );
    }

    /// Each copy lands directly above its source, and the returned ids are the
    /// copies in the order the sources were given.
    #[test]
    fn duplicate_layers_returns_the_copies_above_their_sources() {
        let (doc, comp) = doc_with_layers(2);
        let (updated, copies) =
            duplicate_layers(&doc, comp, &[LayerId::new(1), LayerId::new(2)]).unwrap();
        assert_eq!(copies.len(), 2);

        let composition = updated.get_composition(comp).unwrap();
        let order: Vec<LayerId> = composition.layers.iter().map(|l| l.id).collect();
        assert_eq!(
            order,
            vec![LayerId::new(1), copies[0], LayerId::new(2), copies[1]]
        );
        assert_eq!(
            composition.get_layer(copies[0]).unwrap().name,
            "Layer 1 copy"
        );
        assert!(duplicate_layers(&doc, comp, &[]).is_none());
    }

    /// The completion criterion for the split: the two halves cover the
    /// source's original composition range exactly — no gap, no overlap — and
    /// both map composition time to the same layer-local time the source did,
    /// so the keyframes on either side stay under the frames they were on.
    #[test]
    fn split_layer_halves_cover_the_original_range() {
        let (doc, comp) = doc_with_layers(1);
        let id = LayerId::new(1);
        let doc = update_layer(&doc, comp, id, |l| {
            l.start_frame = 10;
            l.in_frame = 4;
            l.out_frame = 24;
        })
        .unwrap();
        let source = doc
            .get_composition(comp)
            .unwrap()
            .get_layer(id)
            .unwrap()
            .clone();
        assert_eq!((source.start_frame, source.end_frame()), (10, 30));

        let split = split_layer(&doc, comp, id, 18).unwrap();
        let composition = split.get_composition(comp).unwrap();
        let head = composition.get_layer(id).unwrap();
        let tail_id = composition.layers[1].id;
        let tail = composition.get_layer(tail_id).unwrap();

        assert_eq!((head.start_frame, head.end_frame()), (10, 18));
        assert_eq!((tail.start_frame, tail.end_frame()), (18, 30));
        assert_eq!(head.duration() + tail.duration(), source.duration());
        // Same comp→local mapping on both sides: the cut moved `start_frame`
        // and `in_frame` by the same amount.
        for frame in [10u64, 17, 18, 29] {
            let half = if frame < 18 { head } else { tail };
            assert_eq!(half.local_frame(frame), source.local_frame(frame));
        }
        assert_eq!(
            tail.name, source.name,
            "the halves are one layer, not a copy"
        );
        assert_ne!(tail.id, source.id);
    }

    /// A cut outside the layer, on either edge of it, or on a locked layer
    /// changes nothing — an empty half is not a layer.
    #[test]
    fn split_layer_refuses_cuts_that_produce_an_empty_half() {
        let (doc, comp) = doc_with_layers(1);
        let id = LayerId::new(1);
        let doc = update_layer(&doc, comp, id, |l| {
            l.start_frame = 10;
            l.in_frame = 4;
            l.out_frame = 24;
        })
        .unwrap();
        for frame in [-5, 0, 9, 10, 30, 40] {
            assert!(
                split_layer(&doc, comp, id, frame).is_none(),
                "frame {frame} is not strictly inside the layer"
            );
        }

        let locked = update_layer(&doc, comp, id, |l| l.locked = true).unwrap();
        assert!(split_layer(&locked, comp, id, 18).is_none());
    }

    /// Splitting a selection is one snapshot — one undo step — and each new
    /// half lands directly above its source even though the earlier
    /// insertions moved the indices above them.
    #[test]
    fn split_layers_splits_the_whole_selection_in_one_document() {
        let (doc, comp) = doc_with_layers(2);
        let (split, tails) =
            split_layers(&doc, comp, &[LayerId::new(1), LayerId::new(2)], 100).unwrap();
        assert_eq!(tails.len(), 2);

        let order: Vec<LayerId> = split
            .get_composition(comp)
            .unwrap()
            .layers
            .iter()
            .map(|l| l.id)
            .collect();
        assert_eq!(
            order,
            vec![LayerId::new(1), tails[0], LayerId::new(2), tails[1]]
        );

        let mut store = DocumentStore::new(doc);
        store.commit(split);
        assert!(store.undo());
        assert_eq!(
            store.document().get_composition(comp).unwrap().layers.len(),
            2,
            "one undo puts the whole selection back"
        );

        assert!(
            split_layers(store.document(), comp, &[LayerId::new(1)], 0).is_none(),
            "a selection nothing can be cut in is None, not an empty snapshot"
        );
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
    fn restore_snapshot_removes_a_foreign_commit_that_captured_a_preview() {
        let (doc, comp) = doc_with_layers(1);
        let mut store = DocumentStore::new(doc);
        let snapshot = store.document().clone();

        let preview = update_layer(store.document(), comp, LayerId::new(1), |layer| {
            layer.start_frame = 20;
        })
        .unwrap();
        store.apply(preview);
        let polluted = update_layer(store.document(), comp, LayerId::new(1), |layer| {
            layer.name = "foreign edit".into();
        })
        .unwrap();
        store.commit(polluted);

        assert!(store.restore_snapshot(snapshot.clone()));
        assert_eq!(store.document(), &snapshot);
        assert!(
            !store.can_undo(),
            "the polluted commit was removed from history"
        );
        assert!(
            !store.can_redo(),
            "the polluted commit cannot be resurrected"
        );
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
    fn duplicate_layer_inserts_above_source_with_fresh_global_node_ids() {
        let comp_id = CompId::next();
        let source_id = LayerId::next();
        let top_id = LayerId::next();
        let source_node = NodeId::next();
        let top_node = NodeId::next();
        let source_network = Graph::new()
            .add_node(Node::new(source_node, "constant"))
            .unwrap();
        let top_network = Graph::new()
            .add_node(Node::new(top_node, "constant"))
            .unwrap();
        let comp = Composition::new(comp_id, "Test", (16, 16), FrameRate::new(30, 1), 300)
            .add_layer(Layer::new(source_id, "Source", source_network))
            .add_layer(Layer::new(top_id, "Top", top_network));
        let doc = Document::default().with_composition(comp);

        let duplicate = duplicate_layer(&doc, comp_id, source_id).unwrap();
        let comp = duplicate.get_composition(comp_id).unwrap();
        assert_eq!(comp.layers.len(), 3);
        assert_eq!(comp.layers[0].id, source_id);
        assert_eq!(comp.layers[1].name, "Source copy");
        assert_eq!(comp.layers[2].id, top_id);
        assert_ne!(comp.layers[1].id, source_id);
        assert_ne!(comp.layers[1].network.node_ids().next(), Some(source_node));
        assert_eq!(duplicate.validate(), Ok(()));
    }

    #[test]
    fn duplicate_layer_returns_none_for_missing_targets() {
        let (doc, comp) = doc_with_layers(1);
        assert!(duplicate_layer(&doc, CompId::next(), LayerId::new(1)).is_none());
        assert!(duplicate_layer(&doc, comp, LayerId::next()).is_none());
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
    fn audio_template_creates_a_frameless_layer_with_an_audio_source() {
        let (doc, comp) = doc_with_layers(0);
        let template = ravel_core::composition::templates::builtin_layer_template("audio").unwrap();
        let (doc, id) = add_layer_from_template(&doc, comp, template, &registry())
            .unwrap()
            .unwrap();

        let layer = root_composition(&doc).unwrap().get_layer(id).unwrap();
        assert!(layer.audio.is_some());
        assert!(!layer.has_frame_output());
        assert_eq!(layer.network.node_count(), 2);
    }

    /// A media layer for a clip with sound binds the media node **and** the
    /// shell's audio source to the same asset id, with the container stream
    /// index the import probed (audio-plan unit 4).
    #[test]
    fn media_layer_binds_the_shell_audio_to_the_same_asset() {
        let (doc, comp) = doc_with_layers(0);
        let template = ravel_core::composition::templates::builtin_layer_template("media").unwrap();
        let (doc, id) = add_media_layer(
            &doc,
            comp,
            template,
            &registry(),
            MediaLayerSpec {
                name_base: "clip",
                asset_id: "clip",
                start_frame: 12,
                out_frame: 48,
                audio_stream_index: Some(1),
            },
        )
        .unwrap()
        .unwrap();

        let layer = root_composition(&doc).unwrap().get_layer(id).unwrap();
        assert_eq!(
            (layer.start_frame, layer.in_frame, layer.out_frame),
            (12, 0, 48)
        );
        let audio = layer.audio.as_ref().expect("audio source");
        assert_eq!(audio.asset_id, "clip");
        assert_eq!(audio.stream_index, 1);
        // The media node points at the same asset — one asset, two consumers.
        let media = layer
            .network
            .nodes()
            .find(|node| node.type_key == "media")
            .expect("media node");
        assert!(
            media.parameters.iter().any(|param| param.key == "asset_id"
                && param.value == ParameterValue::String("clip".into()))
        );
    }

    /// Silent media leaves the shell without audio: nothing scans the network
    /// for sound later, so an absent source means a silent layer forever.
    #[test]
    fn media_layer_without_a_stream_index_has_no_audio() {
        let (doc, comp) = doc_with_layers(0);
        let template = ravel_core::composition::templates::builtin_layer_template("media").unwrap();
        let (doc, id) = add_media_layer(
            &doc,
            comp,
            template,
            &registry(),
            MediaLayerSpec {
                name_base: "plate",
                asset_id: "plate",
                start_frame: 0,
                out_frame: 10,
                audio_stream_index: None,
            },
        )
        .unwrap()
        .unwrap();

        assert!(
            root_composition(&doc)
                .unwrap()
                .get_layer(id)
                .unwrap()
                .audio
                .is_none()
        );
    }

    /// The shape template's generator starts at the composition center
    /// (the test comp is 16x16), matching the node-editor insertion rule.
    #[test]
    fn template_shape_generator_defaults_to_the_composition_center() {
        let (doc, comp) = doc_with_layers(0);
        let template = ravel_core::composition::templates::builtin_layer_template("shape").unwrap();
        let reg = registry();

        let (doc, id) = add_layer_from_template(&doc, comp, template, &reg)
            .unwrap()
            .unwrap();

        let layer = root_composition(&doc).unwrap().get_layer(id).unwrap();
        let shape = layer
            .network
            .nodes()
            .find(|n| n.type_key.starts_with("shape."))
            .expect("shape node");
        let param = |key: &str| match shape
            .parameters
            .iter()
            .find(|p| p.key == key)
            .map(|p| &p.value)
        {
            Some(ParameterValue::Float(v)) => *v,
            other => panic!("unexpected {key} parameter: {other:?}"),
        };
        let center = match shape
            .parameters
            .iter()
            .find(|p| p.key == "center")
            .map(|p| &p.value)
        {
            Some(ParameterValue::Channel2(chs)) => chs
                .iter()
                .map(|ch| {
                    ch.evaluate(
                        0.0,
                        &ravel_core::eval::EvalContext::new(0, FrameRate::new(30, 1), (16, 16)),
                    )
                })
                .collect::<Vec<_>>(),
            other => panic!("unexpected center parameter: {other:?}"),
        };
        assert_eq!(center, vec![8.0, 8.0]);
        assert_eq!(param("width"), 100.0, "non-center params keep defaults");
        assert_eq!(doc.validate(), Ok(()));
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

    /// Committing an edit of a subnet's inner network re-derives the
    /// enclosing node's pins in the same document, so an inner port edit and
    /// the outer interface it changes are one undo step (REQ-LAYER-003).
    #[test]
    fn committing_an_inner_network_carries_the_pins_to_the_enclosing_node() {
        let inner = net::new_subnet_inner_graph(NodeId::new(30), NodeId::new(31));
        let mut subnet = Node::new(NodeId::new(10), net::SUBNET_TYPE_KEY);
        let (inputs, outputs) = net::subnet_pins(&inner).unwrap();
        subnet.inputs = inputs;
        subnet.outputs = outputs;
        subnet.subnet = Some(std::sync::Arc::new(inner.clone()));
        let sink = Node::new(NodeId::new(11), net::NET_OUT_TYPE_KEY)
            .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let network = Graph::new()
            .add_node(subnet)
            .unwrap()
            .add_node(sink)
            .unwrap()
            .add_edge(
                EdgeId::next(),
                NodeId::new(10),
                OutputPortIndex(0),
                NodeId::new(11),
                InputPortIndex(0),
            )
            .unwrap();

        let comp_id = CompId::next();
        let comp = Composition::new(comp_id, "Test", (16, 16), FrameRate::new(30, 1), 300)
            .add_layer(Layer::new(LayerId::new(1), "L", network).with_time(0, 0, 300));
        let doc = Document::default().with_composition(comp);
        let path = NetworkPath::layer(comp_id, LayerId::new(1)).entered(NodeId::new(10));

        // Add a custom port to the subnet's inner In, exactly as the node
        // editor's commit path does, and commit the inner network.
        let edited = net::add_custom_port(
            inner,
            NodeId::new(30),
            "amount",
            net::CustomPortType::Float,
            net::NetworkContext::Subnet,
        )
        .unwrap();
        let doc = replace_network(&doc, &path, edited).unwrap();

        let outer = resolve_network(&doc, &path.truncated(0)).unwrap();
        let subnet = outer.node(NodeId::new(10)).unwrap();
        assert_eq!(
            subnet
                .inputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["amount"],
            "the inner declaration reached the enclosing node's pins"
        );
        assert_eq!(subnet.parameters.len(), 1, "promotion parameter followed");
        assert_eq!(
            outer.edges().count(),
            1,
            "the untouched output pin keeps its wiring"
        );
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
        let doc = default_document(FrameRate::new(24, 1));
        let comp = root_composition(&doc).unwrap();
        assert_eq!(comp.layer_count(), 0);
        assert_eq!(comp.resolution, (1920, 1080));
        assert_eq!(
            comp.frame_rate,
            FrameRate::new(24, 1),
            "the root composition starts at the rate the caller resolved"
        );
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

    // ----- subnet pin renames (HIGH-30 regression guards) -------------------

    /// A subnet whose inner In declares one custom `Float` output.
    fn subnet_with_amount(subnet_id: NodeId, in_id: NodeId, out_id: NodeId) -> Node {
        let inner = net::add_custom_port(
            net::new_subnet_inner_graph(in_id, out_id),
            in_id,
            "amount",
            net::CustomPortType::Float,
            net::NetworkContext::Subnet,
        )
        .unwrap();
        let mut subnet = Node::new(subnet_id, net::SUBNET_TYPE_KEY);
        net::adopt_subnet_inner(&mut subnet, inner);
        subnet
    }

    /// A one-layer document holding `network`.
    fn doc_with_network(network: Graph) -> (Document, NetworkPath) {
        let comp_id = CompId::next();
        let layer_id = LayerId::next();
        let comp = Composition::new(comp_id, "Test", (16, 16), FrameRate::new(30, 1), 300)
            .add_layer(Layer::new(layer_id, "L", network).with_time(0, 0, 300));
        (
            Document::default().with_composition(comp),
            NetworkPath::layer(comp_id, layer_id),
        )
    }

    /// Rename the custom port `old` on `node` of the network at `path`, the
    /// way the node editor's commit path does.
    fn rename_port_at(
        doc: &Document,
        path: &NetworkPath,
        node: NodeId,
        old: &str,
        new: &str,
    ) -> Document {
        let inner = resolve_network(doc, path).unwrap().clone();
        let (graph, _key_rename, pin_rename) =
            net::rename_custom_port(inner, node, old, new, net::NetworkContext::Subnet)
                .unwrap()
                .into_parts();
        replace_network_renaming_pin(doc, path, graph, pin_rename.as_ref()).unwrap()
    }

    fn node_output_channel(node: NodeId, port: OutputPortIndex) -> ParameterValue {
        ParameterValue::Channel(ravel_core::animation::channel::AnimationChannel::new(
            ravel_core::animation::channel::ChannelSource::NodeOutput(node, port),
        ))
    }

    fn keyframed_float() -> ParameterValue {
        let mut curve = ravel_core::animation::curve::KeyframeCurve::new();
        curve.insert(
            7,
            42.0,
            ravel_core::animation::interpolation::Interpolation::Linear,
        );
        ParameterValue::Channel(ravel_core::animation::channel::AnimationChannel::keyframes(
            curve,
        ))
    }

    /// Renaming a custom port of a subnet's inner In renames the enclosing
    /// node's **input pin**: the outer edge stays on it, and the promoted
    /// parameter's keyframes move with the key rather than being re-seeded
    /// from the inner In's default.
    #[test]
    fn renaming_an_inner_in_port_keeps_the_outer_input_edge_and_its_keyframes() {
        let (subnet_id, in_id, out_id) = (NodeId::next(), NodeId::next(), NodeId::next());
        let (source_id, sink_id) = (NodeId::next(), NodeId::next());
        let mut subnet = subnet_with_amount(subnet_id, in_id, out_id);
        subnet.parameters = vec![ravel_core::graph::Parameter {
            key: "amount".to_string(),
            value: keyframed_float(),
        }];
        let network = Graph::new()
            .add_node(Node::new(source_id, "constant").with_output("value", DataTypeId::SCALAR))
            .unwrap()
            .add_node(subnet)
            .unwrap()
            .add_node(
                Node::new(sink_id, net::NET_OUT_TYPE_KEY)
                    .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
            )
            .unwrap()
            .add_edge(
                EdgeId::next(),
                source_id,
                OutputPortIndex(0),
                subnet_id,
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::next(),
                subnet_id,
                OutputPortIndex(0),
                sink_id,
                InputPortIndex(0),
            )
            .unwrap();
        let (doc, path) = doc_with_network(network);

        let doc = rename_port_at(&doc, &path.entered(subnet_id), in_id, "amount", "gain");

        let outer = resolve_network(&doc, &path).unwrap();
        let subnet = outer.node(subnet_id).unwrap();
        assert_eq!(
            subnet
                .inputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["gain"],
            "the pin was renamed, not replaced"
        );
        let feed = outer
            .edges()
            .find(|edge| edge.target == subnet_id)
            .expect("the outer input edge survived the rename");
        assert_eq!(feed.source, source_id);
        assert_eq!(feed.target_port, InputPortIndex(0));
        let promoted = subnet
            .parameters
            .iter()
            .find(|p| p.key == "gain")
            .expect("the promotion parameter moved to the new key");
        assert_eq!(
            promoted.value,
            keyframed_float(),
            "with its keyframes, not re-seeded from the inner default"
        );
    }

    /// The output side: renaming a custom input of the subnet's inner Out
    /// renames the enclosing node's **output pin**, so both the outer edge and
    /// the `ChannelSource::NodeOutput` binding that named the slot survive.
    #[test]
    fn renaming_an_inner_out_port_keeps_the_outer_output_edge_and_binding() {
        let (subnet_id, in_id, out_id) = (NodeId::next(), NodeId::next(), NodeId::next());
        let consumer_id = NodeId::next();
        let inner = net::add_custom_port(
            net::new_subnet_inner_graph(in_id, out_id),
            out_id,
            "mask",
            net::CustomPortType::Float,
            net::NetworkContext::Subnet,
        )
        .unwrap();
        let mut subnet = Node::new(subnet_id, net::SUBNET_TYPE_KEY);
        net::adopt_subnet_inner(&mut subnet, inner);
        let mask_slot = OutputPortIndex(
            subnet
                .outputs
                .iter()
                .position(|p| p.name == "mask")
                .expect("the inner Out port became a pin") as u32,
        );
        let consumer = Node::new(consumer_id, "constant")
            .with_input("driver", &[DataTypeId::SCALAR])
            .with_param("driver", node_output_channel(subnet_id, mask_slot));
        let network = Graph::new()
            .add_node(subnet)
            .unwrap()
            .add_node(consumer)
            .unwrap()
            .add_edge(
                EdgeId::next(),
                subnet_id,
                mask_slot,
                consumer_id,
                InputPortIndex(0),
            )
            .unwrap();
        let (doc, path) = doc_with_network(network);

        let doc = rename_port_at(&doc, &path.entered(subnet_id), out_id, "mask", "matte");

        let outer = resolve_network(&doc, &path).unwrap();
        let subnet = outer.node(subnet_id).unwrap();
        assert_eq!(
            subnet.outputs[mask_slot.0 as usize].name, "matte",
            "the pin was renamed in place"
        );
        let edge = outer
            .edges()
            .find(|edge| edge.source == subnet_id)
            .expect("the outer output edge survived the rename");
        assert_eq!(edge.source_port, mask_slot);
        let binding = &outer.node(consumer_id).unwrap().parameters[0].value;
        assert_eq!(
            binding,
            &node_output_channel(subnet_id, mask_slot),
            "the NodeOutput binding did not collapse to a constant"
        );
    }

    /// The same, two levels down: only the subnet that directly owns the
    /// edited network has a pin to rename, and the one above it keeps the
    /// wiring it derives from its own In / Out.
    #[test]
    fn renaming_a_pin_inside_a_nested_subnet_keeps_both_levels_wired() {
        let (outer_id, outer_in, outer_out) = (NodeId::next(), NodeId::next(), NodeId::next());
        let (mid_id, mid_in, mid_out) = (NodeId::next(), NodeId::next(), NodeId::next());
        let source_id = NodeId::next();

        // Inner subnet, placed inside the outer subnet's own network.
        let mut mid = subnet_with_amount(mid_id, mid_in, mid_out);
        mid.parameters = vec![ravel_core::graph::Parameter {
            key: "amount".to_string(),
            value: keyframed_float(),
        }];
        let outer_inner = net::add_custom_port(
            net::new_subnet_inner_graph(outer_in, outer_out),
            outer_in,
            "level",
            net::CustomPortType::Float,
            net::NetworkContext::Subnet,
        )
        .unwrap()
        .add_node(mid)
        .unwrap();
        let level_slot = net::output_port_index(outer_inner.node(outer_in).unwrap(), "level")
            .expect("the custom port was just added");
        let outer_inner = outer_inner
            .add_edge(
                EdgeId::next(),
                outer_in,
                level_slot,
                mid_id,
                InputPortIndex(0),
            )
            .unwrap();
        let mut outer = Node::new(outer_id, net::SUBNET_TYPE_KEY);
        net::adopt_subnet_inner(&mut outer, outer_inner);
        let network = Graph::new()
            .add_node(Node::new(source_id, "constant").with_output("value", DataTypeId::SCALAR))
            .unwrap()
            .add_node(outer)
            .unwrap()
            .add_edge(
                EdgeId::next(),
                source_id,
                OutputPortIndex(0),
                outer_id,
                InputPortIndex(0),
            )
            .unwrap();
        let (doc, path) = doc_with_network(network);

        let deep = path.entered(outer_id).entered(mid_id);
        let doc = rename_port_at(&doc, &deep, mid_in, "amount", "gain");

        // The inner subnet node's pin moved, keeping its feed and keyframes.
        let mid_graph = resolve_network(&doc, &path.entered(outer_id)).unwrap();
        let mid = mid_graph.node(mid_id).unwrap();
        assert_eq!(
            mid.inputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["gain"]
        );
        assert_eq!(
            mid_graph
                .edges()
                .filter(|edge| edge.target == mid_id)
                .count(),
            1,
            "the edge from the enclosing network's In survived"
        );
        assert_eq!(
            mid.parameters
                .iter()
                .find(|p| p.key == "gain")
                .map(|p| &p.value),
            Some(&keyframed_float())
        );

        // The level above is untouched: its own pins come from its own In.
        let outer_graph = resolve_network(&doc, &path).unwrap();
        let outer = outer_graph.node(outer_id).unwrap();
        assert_eq!(
            outer
                .inputs
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            vec!["level"],
            "an ancestor's pins derive from its own In, which nothing renamed"
        );
        assert_eq!(
            outer_graph.edges().count(),
            1,
            "and its outer edge is still attached"
        );
    }

    // ----- composition management (REQ-UI-013) ------------------------------

    fn settings(name: &str) -> CompositionSettings {
        CompositionSettings {
            name: name.to_string(),
            resolution: (1280, 720),
            frame_rate: FrameRate::new(24, 1),
            duration_frames: 120,
            background_color: Color::BLACK,
        }
    }

    #[test]
    fn settings_round_trip_through_a_composition() {
        let source = settings("Shot 1");
        let comp = source.clone().into_composition(CompId::next());
        assert_eq!(CompositionSettings::from_composition(&comp), source);

        // Applying to an existing composition keeps its layers.
        let (doc, comp_id) = doc_with_layers(2);
        let edited =
            update_composition(&doc, comp_id, |comp| settings("Renamed").apply_to(comp)).unwrap();
        let comp = edited.get_composition(comp_id).unwrap();
        assert_eq!(comp.name, "Renamed");
        assert_eq!(comp.resolution, (1280, 720));
        assert_eq!(comp.layer_count(), 2, "settings edits keep the layers");
    }

    #[test]
    fn settings_are_clamped_to_a_constructible_composition() {
        let sanitized = CompositionSettings {
            name: "Zeroes".into(),
            resolution: (0, 0),
            frame_rate: FrameRate::new(0, 1),
            duration_frames: 0,
            background_color: Color::BLACK,
        }
        .sanitized();
        assert_eq!(sanitized.resolution, (1, 1));
        assert_eq!(sanitized.frame_rate, FrameRate::new(1, 1));
        assert_eq!(sanitized.duration_frames, 1);
    }

    #[test]
    fn the_first_composition_of_an_empty_document_becomes_its_root() {
        let doc = Document::default();
        assert_eq!(doc.root_comp, None);

        let (doc, first) = add_composition(&doc, settings("Comp 1"));
        assert_eq!(doc.root_comp, Some(first), "an empty project adopts a root");

        let (doc, second) = add_composition(&doc, settings("Comp 2"));
        assert_eq!(
            doc.root_comp,
            Some(first),
            "a later composition must not steal the root"
        );
        assert_eq!(doc.compositions.len(), 2);
        assert!(doc.get_composition(second).is_some());
    }

    #[test]
    fn new_and_duplicate_names_never_collide() {
        let (doc, _) = add_composition(&Document::default(), settings("Comp 1"));
        assert_eq!(next_composition_name(&doc), "Comp 2");
        assert_eq!(unique_composition_name(&doc, "Comp 1"), "Comp 1 2");
        assert_eq!(unique_composition_name(&doc, "Other"), "Other");

        // `Comp 2` taken out of order still yields a free default name.
        let (doc, _) = add_composition(&doc, settings("Comp 3"));
        assert_eq!(next_composition_name(&doc), "Comp 4");
    }

    #[test]
    fn duplicating_a_composition_copies_it_with_fresh_ids() {
        let (doc, comp_id) = doc_with_layers(2);
        let source_layers: Vec<LayerId> = doc
            .get_composition(comp_id)
            .unwrap()
            .layers
            .iter()
            .map(|l| l.id)
            .collect();

        let (doc, copy_id) = duplicate_composition(&doc, comp_id).unwrap();
        assert_ne!(copy_id, comp_id);
        let copy = doc.get_composition(copy_id).unwrap();
        assert_eq!(copy.id, copy_id);
        assert_eq!(copy.name, "Test copy");
        assert_eq!(copy.layer_count(), 2);
        for layer in copy.layers.iter() {
            assert!(
                !source_layers.contains(&layer.id),
                "a copied layer must not share the original's id"
            );
        }
        assert_eq!(
            doc.get_composition(comp_id).unwrap().layers.len(),
            2,
            "the source composition is untouched"
        );
        assert_eq!(doc.root_comp, Some(comp_id), "a copy is not the root");

        // A second copy gets its own name.
        let (doc, _) = duplicate_composition(&doc, comp_id).unwrap();
        assert!(
            doc.compositions
                .values()
                .any(|comp| comp.name == "Test copy 2")
        );
    }

    /// A copied stack must parent within itself. Carrying `parent` over
    /// verbatim points every copy at the source composition's layers, which
    /// `validate` rejects the next time the document is saved or loaded.
    #[test]
    fn duplicating_a_composition_repoints_parent_links_at_the_copies() {
        let (doc, comp_id) = doc_with_layers(2);
        let mut comp = doc.get_composition(comp_id).unwrap().as_ref().clone();
        let parent = comp.layers[1].id;
        comp.layers[0].parent = Some(parent);
        let mut doc = doc.clone();
        doc.compositions.insert(comp_id, std::sync::Arc::new(comp));

        let (doc, copy_id) = duplicate_composition(&doc, comp_id).unwrap();
        let copy = doc.get_composition(copy_id).unwrap();
        let copied_ids: Vec<LayerId> = copy.layers.iter().map(|l| l.id).collect();
        let copied_parent = copy.layers[0].parent.expect("the copy keeps the link");

        assert!(
            copied_ids.contains(&copied_parent),
            "the copy parents into its own stack, not the source's: \
             parent={copied_parent:?} copies={copied_ids:?}"
        );
        assert_eq!(
            copied_parent, copied_ids[1],
            "the link keeps its position in the stack"
        );
        assert_ne!(copied_parent, parent, "the source id must not survive");
        assert!(
            doc.validate().is_ok(),
            "a duplicated stack must pass validation: {:?}",
            doc.validate()
        );
    }

    #[test]
    fn removing_a_composition_moves_a_dangling_root() {
        let (doc, first) = add_composition(&Document::default(), settings("Comp 1"));
        let (doc, second) = add_composition(&doc, settings("Comp 2"));
        assert_eq!(neighbour_composition(&doc, first), Some(second));
        assert_eq!(
            neighbour_composition(&doc, second),
            Some(first),
            "the last composition falls back to its predecessor"
        );

        // Removing the root moves it to the neighbour.
        let after_root = remove_composition(&doc, first).unwrap();
        assert_eq!(after_root.root_comp, Some(second));
        assert_eq!(after_root.compositions.len(), 1);

        // Removing a non-root leaves the root alone.
        let after_other = remove_composition(&doc, second).unwrap();
        assert_eq!(after_other.root_comp, Some(first));

        // Removing the last composition is a valid, root-less document.
        let empty = remove_composition(&after_root, second).unwrap();
        assert_eq!(empty.root_comp, None);
        assert!(empty.compositions.is_empty());
        assert!(neighbour_composition(&empty, second).is_none());

        assert!(
            remove_composition(&doc, CompId::next()).is_none(),
            "removing an unknown composition is not an edit"
        );
    }

    #[test]
    fn compositions_are_ordered_by_id() {
        let (doc, first) = add_composition(&Document::default(), settings("B"));
        let (doc, second) = add_composition(&doc, settings("A"));
        assert_eq!(
            compositions_in_order(&doc)
                .into_iter()
                .map(|comp| comp.id)
                .collect::<Vec<_>>(),
            [first, second],
            "display order follows ids, not names"
        );
    }
}
