// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The pure `Document -> Document` transforms the edit operations are built
//! from: composition and layer management, and the [`NetworkPath`] that
//! addresses a network.
//!
//! These lived in `ravel-ui::document`, which re-exports them, so the UI
//! panels and the headless callers of [`super::apply`] run the same code.

use crate::composition::templates::{LayerTemplate, TemplateError};
use crate::composition::{Composition, Document, Layer};
use crate::graph::{Graph, Node, ParameterValue};
use crate::id::{CompId, LayerId, NodeId};
use crate::registry::NodeRegistry;
use crate::types::{Color, FrameRate};
use serde::{Deserialize, Serialize};

/// Ownership path of the network a node editor is looking at:
/// `CompId / LayerId / [SubnetNodeId ...]` (REQ-LAYER-011).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// crossing into `crate::network` collapses the path first. Doing it
    /// once, here, keeps `subnets.is_empty()` from being re-decided per panel.
    pub fn context(&self) -> crate::network::NetworkContext {
        if self.subnets.is_empty() {
            crate::network::NetworkContext::LayerRoot
        } else {
            crate::network::NetworkContext::Subnet
        }
    }

    /// The evaluator ownership path of this network's scope.
    pub fn segments(&self) -> Vec<crate::eval::PathSegment> {
        let mut segments = vec![crate::eval::PathSegment::Layer(self.comp, self.layer)];
        segments.extend(
            self.subnets
                .iter()
                .map(|id| crate::eval::PathSegment::Subnet(*id)),
        );
        segments
    }
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
        layer.audio = Some(crate::composition::AudioSource::default());
    }
    Ok(add_layer(doc, comp, layer).map(|doc| (doc, id)))
}

pub fn unique_layer_name(comp: &Composition, base: &str) -> String {
    let mut n = comp.layer_count() + 1;
    loop {
        let candidate = format!("{base} {n}");
        if comp.layers.iter().all(|l| l.name != candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// The editable settings of a composition — everything the New/Settings
/// dialogs and the Properties composition target work with.
///
/// These are plain fields, not `ParameterValue`s: a composition's resolution
/// and frame rate cannot be animated, so no channel or keyframe machinery is
/// involved. [`CompositionSettings::sanitized`] is the single place that keeps
/// a composition constructible (no zero resolution, no zero frame rate).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
