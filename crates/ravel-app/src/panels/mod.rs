// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Panel views for the dock layout.

pub mod media_bin;
pub mod node_editor;
pub mod outliner;
mod param_edit;
pub mod timeline;
pub mod viewer;

pub mod properties;
pub mod render_queue;
pub mod sync_probe;

use gpui::*;
use gpui_component::{ActiveTheme, Icon};
use image::{Frame as ImageFrame, ImageBuffer, Rgba};
use ravel_core::composition::{Composition, Document};
use ravel_core::graph::GraphError;
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_core::network::NetworkError;
use ravel_core::runtime::playback::LoopRange;
use ravel_dock::PaneContent;
use ravel_gpu::GpuFrameBuffer;
use ravel_i18n::t;
use ravel_nodes::DisplayFrame;
use ravel_ui::layout::{PanelInstance, PanelInstanceId};
use ravel_ui::panel::PanelKind;
use ravel_ui::panels::timeline::BpmGrid;
use smallvec::SmallVec;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

/// The reason a refused custom-port edit gives the user.
///
/// [`NetworkError`]'s own text names node ids and enum variants — it is for a
/// log, not a panel — so each variant maps to the sentence that says what to
/// do instead. Nothing is swallowed: an edit that cannot be described falls
/// back to the generic line and is logged.
///
/// Both entry points to the same core API share it: the Properties Ports
/// section prints it under the list, the node editor's port context menu in
/// its notice strip. One refusal must not read as two different problems
/// depending on which panel the user reached for.
pub(crate) fn port_error_message(err: &NetworkError) -> SharedString {
    let message = match err {
        NetworkError::PortTypeNotAllowed { .. } => t!("properties.ports.error.type_not_allowed"),
        NetworkError::ReservedPortName { .. } => t!("properties.ports.error.reserved"),
        NetworkError::FixedPort { .. } => t!("properties.ports.error.builtin"),
        NetworkError::Graph(
            GraphError::DuplicatePortName { .. } | GraphError::DuplicateParamKey { .. },
        ) => t!("properties.ports.error.duplicate"),
        other => {
            tracing::warn!(error = %other, "port edit refused");
            t!("properties.ports.error.failed")
        }
    };
    SharedString::from(message)
}

/// Durable shared state: the panel *instance* that currently contains the
/// focused element, or `None` when the focus is outside every panel.
///
/// The same panel kind can be open several times, and a command acts on the
/// one the user is working in, so the focus is tracked per
/// [`PanelInstanceId`] — [`ravel_ui::shell::AppShell`] resolves it back to a
/// window and a kind through the layout. Written only from real GPUI focus
/// events (see [`track_panel_focus`]), never from click handlers.
pub struct FocusedPanelGlobal(pub Option<PanelInstanceId>);

impl Global for FocusedPanelGlobal {}

/// Panel instances whose tab is currently at the front of an area in any
/// open window.
///
/// This is durable shared state, not a one-shot notification. The window host
/// maintains it when it applies a layout tree; panels will start observing it
/// in a later visibility-gating unit.
#[derive(Default)]
pub struct VisiblePanels(pub HashSet<PanelInstanceId>);

impl Global for VisiblePanels {}

/// Whether `instance` is the panel instance holding the focus.
pub(crate) fn is_instance_focused(instance: PanelInstanceId, cx: &App) -> bool {
    cx.try_global::<FocusedPanelGlobal>().and_then(|g| g.0) == Some(instance)
}

/// Keeps [`FocusedPanelGlobal`] pointing at `instance` while this panel holds
/// the focus.
fn track_panel_focus<T: 'static>(
    instance: PanelInstanceId,
    focus_handle: &FocusHandle,
    window: &mut Window,
    cx: &mut Context<T>,
) -> [Subscription; 2] {
    let focus_in = cx.on_focus_in(focus_handle, window, move |_this, _window, cx| {
        cx.set_global(FocusedPanelGlobal(Some(instance)));
    });
    let focus_out = cx.on_focus_out(focus_handle, window, move |_this, _event, _window, cx| {
        if is_instance_focused(instance, cx) {
            cx.set_global(FocusedPanelGlobal(None));
        }
    });
    [focus_in, focus_out]
}

/// Repoints a "focused instance" global at this panel whenever it takes focus.
///
/// Globals like [`TimelinePanelHandle`] are single handles into a world that
/// can hold several instances of the same panel; they name the instance the
/// user is working in. Construction installs the newest instance (so a
/// freshly opened session has one before anything is focused) and this moves
/// the handle with the focus.
fn track_focused_handle<T: 'static, G: Global>(
    focus_handle: &FocusHandle,
    window: &mut Window,
    cx: &mut Context<T>,
    make: impl Fn(WeakEntity<T>) -> G + 'static,
) -> Subscription {
    cx.on_focus_in(focus_handle, window, move |_this, _window, cx| {
        let handle = cx.entity().downgrade();
        cx.set_global(make(handle));
    })
}

// ---------------------------------------------------------------------------
// Properties panel globals
// ---------------------------------------------------------------------------

/// What the Properties panel should currently inspect. The target only
/// IDENTIFIES the subject — the panel resolves live values from the
/// `ProjectState` document on every build/refresh (and observes the
/// document plus the shared `PlaybackPosition`), so edits, undo/redo, and
/// playhead moves never leave stale snapshots behind.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum PropertiesTarget {
    #[default]
    Empty,
    Nodes {
        /// Network owning the selected nodes (the node editor's context).
        network: ravel_ui::document::NetworkPath,
        ids: Vec<NodeId>,
    },
    Layer {
        /// Composition owning the layer, for resolving and routing edits
        /// back into the document.
        comp_id: ravel_core::id::CompId,
        layer_id: ravel_core::id::LayerId,
    },
    /// Several layers of one composition (REQ-UI-013 multi-selection). Kept
    /// distinct from `Layer` because a single layer is what an editable field
    /// routes to: this target is read-only in v1 (count plus the fields the
    /// layers agree on), so a bulk edit cannot leak out of a widget that has
    /// no layer to apply to. `layer_ids` is in selection order.
    Layers {
        comp_id: ravel_core::id::CompId,
        layer_ids: Vec<ravel_core::id::LayerId>,
    },
    /// A composition's own settings (name, resolution, frame rate, duration,
    /// background). Written by the Outliner's composition rows and by the
    /// composition commands (REQ-UI-013); like every other target it only
    /// identifies the subject.
    Composition { comp_id: ravel_core::id::CompId },
    /// A media asset of the project (REQ-UI-008, media-import plan unit 4).
    /// Written by the MediaBin selection; `id` is the `media_assets` key and
    /// only identifies the subject — the full inspector (metadata, path
    /// editing, relink) is unit 6.
    MediaAsset { id: String },
    /// The project's exposed parameter declarations (REQ-PROJ-006), reached
    /// through `CommandId::ProjectExposedParameters`.
    ///
    /// The other targets name something the user selected; this one names the
    /// project itself, because the declarations are the project's external
    /// contract and belong to no composition, layer or node. It carries no
    /// identifier for the same reason — a document has exactly one set of
    /// declarations.
    Project,
}

/// Durable shared state identifying what the Properties panel should resolve
/// from the live document. NodeEditorPanel updates it when selection changes.
#[derive(Clone, Default)]
pub struct SelectedPropertiesTarget(pub PropertiesTarget);

impl Global for SelectedPropertiesTarget {}

/// Durable shared state: the canvas-level node selection. The node editor
/// reads and writes this instead of keeping a panel-local set; future
/// consumers (Viewer tool system, bbox overlay) observe the same global.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CanvasSelection {
    /// The network owning the selected nodes (`None` when no network is open).
    pub path: Option<ravel_ui::document::NetworkPath>,
    pub nodes: HashSet<NodeId>,
}

impl Global for CanvasSelection {}

// ---------------------------------------------------------------------------
// Media asset selection (REQ-UI-008, media-import plan unit 4)
// ---------------------------------------------------------------------------

/// Durable shared state: the selected media assets of the MediaBin. The
/// MediaBin panel reads and writes this instead of keeping a panel-local set
/// — the same split as [`LayerSelection`] (the #151 / REQ-UI-013 decision).
/// Asset ids are the keys of [`Document::media_assets`], kept in click order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaSelection {
    assets: Vec<String>,
}

impl Global for MediaSelection {}

impl MediaSelection {
    /// The selected asset ids, in click order.
    pub fn assets(&self) -> &[String] {
        &self.assets
    }

    /// The single asset that one-asset views follow (Properties): the first
    /// of the selection.
    pub fn primary(&self) -> Option<&str> {
        self.assets.first().map(String::as_str)
    }

    pub fn contains(&self, asset_id: &str) -> bool {
        self.assets.iter().any(|id| id == asset_id)
    }

    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }
}

/// The current media asset selection.
pub fn media_selection(cx: &App) -> MediaSelection {
    cx.try_global::<MediaSelection>()
        .cloned()
        .unwrap_or_default()
}

/// Replace the media asset selection and publish it as the Properties
/// subject: one asset is an identifiable [`PropertiesTarget::MediaAsset`],
/// none or several leave the panel empty (a multi-asset inspector is unit 6
/// territory). This is the selection's only writer, so the two globals can
/// never disagree.
pub fn set_media_selection(assets: Vec<String>, cx: &mut App) {
    let target = match assets.as_slice() {
        [id] => PropertiesTarget::MediaAsset { id: id.clone() },
        _ => PropertiesTarget::Empty,
    };
    cx.set_global(MediaSelection { assets });
    cx.set_global(SelectedPropertiesTarget(target));
}

/// Drop selected assets `document` no longer holds (a delete, an undo, a
/// redo), mirroring [`prune_layer_selection`]; a `MediaAsset` Properties
/// target pointing at a gone asset is dropped with it.
///
/// [`crate::project_state::ProjectState`] calls this after every document
/// change, so the selection cannot name an asset that is gone regardless of
/// whether a MediaBin panel exists. Returns whether anything changed.
pub(crate) fn prune_media_selection(document: &Document, cx: &mut App) -> bool {
    let mut changed = false;
    let selection = media_selection(cx);
    let surviving: Vec<String> = selection
        .assets()
        .iter()
        .filter(|id| document.media_assets.contains_key(*id))
        .cloned()
        .collect();
    if surviving.len() != selection.assets().len() {
        set_media_selection(surviving, cx);
        changed = true;
    }
    // The Properties target can name a gone asset without the selection
    // covering it (a single click published both, but nothing else writes a
    // MediaAsset target) — drop it independently of the selection.
    let target_stale = matches!(
        cx.try_global::<SelectedPropertiesTarget>().map(|t| &t.0),
        Some(PropertiesTarget::MediaAsset { id }) if !document.media_assets.contains_key(id)
    );
    if target_stale {
        cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Empty));
        changed = true;
    }
    changed
}

// ---------------------------------------------------------------------------
// Active composition and layer selection (REQ-UI-013)
// ---------------------------------------------------------------------------

/// Durable shared state: the composition the UI is currently editing.
///
/// This — not `Document::root_comp` — is what Timeline, Viewer evaluation,
/// the playback clock, Properties, and the Outliner follow. `root_comp`
/// stays the model-level root (the composition that becomes active when a
/// document is opened) and is never rewritten by a UI switch, so switching
/// compositions lands in neither the undo history nor the saved document.
///
/// `None` is a legitimate state (a document with no composition): every
/// consumer draws its empty state instead of assuming a composition exists.
///
/// [`crate::project_state::ProjectState`] is the only writer — it owns the
/// document the id has to resolve in, and dropping its compiled root cache
/// is part of the switch. The field is private so no caller can install an
/// active composition without [`set_active_composition`] resetting
/// [`LayerSelection`] with it; read it through [`active_composition`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveComposition(Option<CompId>);

impl Global for ActiveComposition {}

/// Durable shared state: the selected layers of the active composition
/// (REQ-UI-013). Timeline and Outliner both read and write this; the node
/// editor, Properties, and the Viewer bbox follow the selection.
///
/// `layers` keeps click order — range-selection anchors and display order
/// depend on it.
///
/// **Invariant**: `comp == ActiveComposition`. It is upheld by construction:
/// the only writers are the functions below, which always stamp the active
/// composition (or switch it first, for a cross-composition selection).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayerSelection {
    comp: Option<CompId>,
    layers: Vec<LayerId>,
}

impl Global for LayerSelection {}

impl LayerSelection {
    /// Build a selection from ids that are already known, without touching
    /// the global. Application code goes through [`set_layer_selection`].
    #[cfg(test)]
    pub(crate) fn of(comp: CompId, layers: Vec<LayerId>) -> Self {
        Self {
            comp: Some(comp),
            layers,
        }
    }

    /// The composition the selection belongs to (always the active one).
    pub fn comp(&self) -> Option<CompId> {
        self.comp
    }

    /// The selected layers, in click order.
    pub fn layers(&self) -> &[LayerId] {
        &self.layers
    }

    /// The single layer that panels with a one-layer view follow (node
    /// editor network, Properties target): the first of the selection.
    pub fn primary(&self) -> Option<LayerId> {
        self.layers.first().copied()
    }

    pub fn contains(&self, layer: LayerId) -> bool {
        self.layers.contains(&layer)
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

/// Durable shared state: the Timeline's musical beat grid (unit 8 of
/// `docs/implementation/refactor-plan-0808.md`).
///
/// A Global rather than panel-local state because the value outlives any one
/// Timeline view: the project save path writes it to `ui_state.json` and a
/// load installs it, and a second Timeline instance must show the same grid.
/// It is UI state, so it stays out of the `Document` and out of undo.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BpmGridState(BpmGrid);

impl Global for BpmGridState {}

/// The Timeline's beat grid, defaulted before any project state published one.
pub fn bpm_grid(cx: &App) -> BpmGrid {
    cx.try_global::<BpmGridState>()
        .map_or_else(BpmGrid::default, |state| state.0)
}

/// Install a beat grid: the Timeline toolbar on a user edit, and
/// [`crate::project_state::ProjectState`] on a project load or File ▸ New.
/// Sanitized here so no caller can install a degenerate tempo.
pub(crate) fn set_bpm_grid(grid: BpmGrid, cx: &mut App) {
    cx.set_global(BpmGridState(grid.sanitized()));
}

/// Durable shared state: the loop range of every composition that has one
/// (unit 9 of `docs/implementation/refactor-plan-0808.md`).
///
/// Keyed by composition because that is the granularity of the feature — a
/// loop range belongs to the composition you set it in, the way After
/// Effects' work area does — and a Global for the same reasons as
/// [`BpmGridState`]: the project save path writes it to `ui_state.json`, a
/// load installs it, and the transport reads it without going through a
/// Timeline view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoopRangeState(BTreeMap<CompId, LoopRange>);

impl Global for LoopRangeState {}

/// Every stored loop range, for the project save path.
pub fn loop_ranges(cx: &App) -> BTreeMap<CompId, LoopRange> {
    cx.try_global::<LoopRangeState>()
        .map_or_else(BTreeMap::new, |state| state.0.clone())
}

/// The active composition's loop range, `None` when it has none.
pub fn loop_range(cx: &App) -> Option<LoopRange> {
    let comp = active_composition(cx)?;
    cx.try_global::<LoopRangeState>()?.0.get(&comp).copied()
}

/// Install the active composition's loop range (`None` clears it). A no-op
/// without an active composition: there is nothing for the range to belong
/// to. Returns whether anything changed, so callers can skip the notify.
pub(crate) fn set_loop_range(range: Option<LoopRange>, cx: &mut App) -> bool {
    let Some(comp) = active_composition(cx) else {
        return false;
    };
    let mut ranges = loop_ranges(cx);
    let changed = match range {
        Some(range) => ranges.insert(comp, range) != Some(range),
        None => ranges.remove(&comp).is_some(),
    };
    if changed {
        cx.set_global(LoopRangeState(ranges));
    }
    changed
}

/// Install every loop range at once: [`crate::project_state::ProjectState`]
/// on a project load or File ▸ New.
pub(crate) fn set_loop_ranges(ranges: BTreeMap<CompId, LoopRange>, cx: &mut App) {
    cx.set_global(LoopRangeState(ranges));
}

/// Durable shared state: which frames of each composition the output-stage
/// frame cache is currently holding — the Timeline's cache band (`CACHE-6`).
///
/// Written by [`crate::project_state::ProjectState`] when an evaluation
/// completes, and **only when the ranges actually changed** (see
/// [`set_cache_band`]).
///
/// # Why nothing observes this global
///
/// The Timeline reads it during `render` instead of subscribing. A
/// subscription would notify the panel on every evaluation, which during
/// playback is every frame — a second repaint on top of the one the playhead
/// already causes, which is exactly the shape of `HIGH-21`. The band grows
/// with playback anyway because the playhead repaint re-reads this global,
/// and an edit repaints the Timeline through the document it mirrors. The
/// cost of the band is therefore one map lookup per repaint that was going to
/// happen regardless.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheBandState(BTreeMap<CompId, Vec<Range<u64>>>);

impl Global for CacheBandState {}

/// The active composition's cached frame ranges; empty when nothing is
/// cached, or before any evaluation completed.
pub fn cache_band(cx: &App) -> Vec<Range<u64>> {
    let Some(comp) = active_composition(cx) else {
        return Vec::new();
    };
    cx.try_global::<CacheBandState>()
        .and_then(|state| state.0.get(&comp).cloned())
        .unwrap_or_default()
}

/// Publish `ranges` as `comp`'s cached frames. Returns whether anything
/// changed.
///
/// The comparison is the point: an evaluation that adds nothing to the cache
/// (a hit, a failed target) must not write the global, so no present or
/// future observer of it wakes for a band that looks the same.
pub(crate) fn set_cache_band(comp: CompId, ranges: Vec<Range<u64>>, cx: &mut App) -> bool {
    // Compared *before* the map is cloned: the common case during playback is
    // "same band as last time", and paying a clone of every composition's
    // ranges to discover that is UI-thread work for nothing.
    let stored = cx
        .try_global::<CacheBandState>()
        .and_then(|state| state.0.get(&comp));
    let changed = match stored {
        Some(current) => *current != ranges,
        None => !ranges.is_empty(),
    };
    if !changed {
        return false;
    }
    let mut bands = cx
        .try_global::<CacheBandState>()
        .map_or_else(BTreeMap::new, |state| state.0.clone());
    if ranges.is_empty() {
        bands.remove(&comp);
    } else {
        bands.insert(comp, ranges);
    }
    cx.set_global(CacheBandState(bands));
    true
}

/// Drop every composition's band.
///
/// Called the moment the document changes — before the evaluation that would
/// republish it — and on the paths where no evaluation follows at all (an
/// emptied composition, one that stopped compiling). A band that outlives the
/// frames it describes is worse than no band: it says a scrub will be free
/// when it will not.
pub(crate) fn clear_cache_band(cx: &mut App) {
    if cx
        .try_global::<CacheBandState>()
        .is_none_or(|state| state.0.is_empty())
    {
        return;
    }
    cx.set_global(CacheBandState::default());
}

/// The active composition id, `None` when the document has no composition
/// (or before any project state published one).
pub fn active_composition(cx: &App) -> Option<CompId> {
    cx.try_global::<ActiveComposition>()
        .and_then(|active| active.0)
}

/// The active composition resolved inside `doc`. `None` when nothing is
/// active or the active id no longer exists in this document.
pub fn active_composition_in<'a>(doc: &'a Document, cx: &App) -> Option<&'a Composition> {
    let id = active_composition(cx)?;
    doc.get_composition(id).map(|arc| arc.as_ref())
}

/// Switch the active composition and reset the layer selection (a selection
/// never survives a composition switch — it belongs to the composition it
/// was made in).
///
/// Call [`crate::project_state::ProjectState::set_active_composition`]
/// instead of this from application code; it also drops the compiled root
/// and re-requests the viewer evaluation.
pub(crate) fn set_active_composition(comp: Option<CompId>, cx: &mut App) {
    cx.set_global(ActiveComposition(comp));
    cx.set_global(LayerSelection {
        comp,
        layers: Vec::new(),
    });
    drop_stale_layer_properties_target(cx);
}

/// [`set_active_composition`] reached from an integration test, so a test can
/// drive the one path that writes the global *without* a `ProjectState` notify
/// behind it. Application code has no reason to call it — use
/// [`crate::project_state::ProjectState::set_active_composition`], which is what
/// keeps the compiled root and the viewer in step.
#[cfg(debug_assertions)]
pub fn set_active_composition_for_tests(comp: Option<CompId>, cx: &mut App) {
    set_active_composition(comp, cx);
}

/// The current layer selection.
pub fn layer_selection(cx: &App) -> LayerSelection {
    cx.try_global::<LayerSelection>()
        .cloned()
        .unwrap_or_default()
}

/// The layer a single-layer view follows (see [`LayerSelection::primary`]).
pub fn selected_layer(cx: &App) -> Option<LayerId> {
    cx.try_global::<LayerSelection>()
        .and_then(LayerSelection::primary)
}

/// Replace the layer selection inside the active composition.
pub fn set_layer_selection(layers: Vec<LayerId>, cx: &mut App) {
    let comp = active_composition(cx);
    cx.set_global(LayerSelection { comp, layers });
    drop_stale_layer_properties_target(cx);
}

/// Drop a Properties target left pointing at layers the selection no longer
/// holds. The target is derived from the selection, so the selection writers
/// own its lifetime; a `Nodes` target belongs to the node editor and is never
/// stolen here.
fn drop_stale_layer_properties_target(cx: &mut App) {
    let selection = layer_selection(cx);
    let stale = match cx.try_global::<SelectedPropertiesTarget>().map(|t| &t.0) {
        Some(PropertiesTarget::Layer { comp_id, layer_id }) => {
            selection.comp != Some(*comp_id) || !selection.contains(*layer_id)
        }
        // A multi-layer target mirrors the whole selection, so any change to it
        // makes the target stale; the writer republishes the new one.
        Some(PropertiesTarget::Layers { comp_id, layer_ids }) => {
            selection.comp != Some(*comp_id) || selection.layers != *layer_ids
        }
        _ => false,
    };
    if stale {
        cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Empty));
    }
}

/// Whether the Properties panel is currently showing the layer selection
/// (one layer or several). Callers that republish a *changed* selection use this
/// to leave a `Nodes` or `Composition` subject alone.
pub(crate) fn properties_shows_layer_selection(cx: &App) -> bool {
    matches!(
        cx.try_global::<SelectedPropertiesTarget>().map(|t| &t.0),
        Some(PropertiesTarget::Layer { .. } | PropertiesTarget::Layers { .. })
    )
}

/// Publish the current layer selection as the Properties subject: one layer is
/// an editable [`PropertiesTarget::Layer`], several are a read-only
/// [`PropertiesTarget::Layers`], and an empty selection leaves the panel empty.
///
/// Both selection writers (Timeline, Outliner) publish through here so the
/// Properties panel shows the same subject whichever panel the click landed in.
/// A node row publishes its own `Nodes` target *after* selecting the layer, so
/// this never overrides it.
pub(crate) fn publish_layer_properties_target(cx: &mut App) {
    let selection = layer_selection(cx);
    let Some(comp_id) = selection.comp() else {
        return;
    };
    let target = match selection.layers() {
        [] => return,
        [layer_id] => PropertiesTarget::Layer {
            comp_id,
            layer_id: *layer_id,
        },
        layer_ids => PropertiesTarget::Layers {
            comp_id,
            layer_ids: layer_ids.to_vec(),
        },
    };
    cx.set_global(SelectedPropertiesTarget(target));
}

/// Select nothing, keeping the active composition.
pub fn clear_layer_selection(cx: &mut App) {
    set_layer_selection(Vec::new(), cx);
}

/// Drop selected layers `document` no longer holds (a delete, an undo, a redo),
/// keeping the rest of the selection.
///
/// [`crate::project_state::ProjectState`] calls this after every document
/// change, so the selection cannot name a layer that is gone regardless of which
/// panels exist — the check used to live in the Timeline panel, which the
/// `motion` and `node` workspaces do not contain. Returns whether the selection
/// changed.
pub(crate) fn prune_layer_selection(document: &Document, cx: &mut App) -> bool {
    let selection = layer_selection(cx);
    let Some(comp_id) = selection.comp() else {
        return false;
    };
    if selection.is_empty() {
        return false;
    }
    // The active composition itself can vanish (an undo past its creation).
    // Which composition is active stays out of the undo history by design
    // (unit 1), so the id is left alone — but nothing can be selected inside a
    // composition the document does not have.
    let Some(comp) = document.get_composition(comp_id) else {
        let showing_layers = properties_shows_layer_selection(cx);
        clear_layer_selection(cx);
        if showing_layers {
            cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Empty));
        }
        return true;
    };
    let surviving: Vec<LayerId> = selection
        .layers()
        .iter()
        .copied()
        .filter(|layer| comp.get_layer(*layer).is_some())
        .collect();
    if surviving.len() == selection.layers().len() {
        return false;
    }
    // Republish only what was already showing the selection, so a `Nodes` or
    // `Composition` subject is never stolen.
    let showing_layers = properties_shows_layer_selection(cx);
    set_layer_selection(surviving, cx);
    if showing_layers {
        publish_layer_properties_target(cx);
    }
    true
}

/// Drop a Properties target naming a composition that no longer exists.
///
/// Called by the deleter (`ProjectState::delete_composition`), mirroring how
/// the layer-selection writers drop a stale `Layer` target: without it
/// [`command_target_composition`] would keep pointing the composition commands
/// at a composition the document has lost, so Settings and Duplicate would
/// quietly do nothing instead of acting on the active composition.
pub(crate) fn drop_composition_properties_target(comp: CompId, cx: &mut App) {
    let stale = matches!(
        cx.try_global::<SelectedPropertiesTarget>().map(|t| &t.0),
        Some(PropertiesTarget::Composition { comp_id }) if *comp_id == comp
    );
    if stale {
        cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Empty));
    }
}

/// The composition the composition commands (Settings / Duplicate / Delete)
/// act on: the one the Properties panel is inspecting when the user picked a
/// composition row in the Outliner, otherwise the active composition.
///
/// This is what makes the menu and the Outliner's own buttons and context menu
/// dispatch *one* command and still act on what the user pointed at
/// (REQ-UI-013): the row click publishes the target, the command reads it.
pub fn command_target_composition(cx: &App) -> Option<CompId> {
    match cx.try_global::<SelectedPropertiesTarget>().map(|t| &t.0) {
        Some(PropertiesTarget::Composition { comp_id }) => Some(*comp_id),
        _ => active_composition(cx),
    }
}

/// Durable shared state: the active canvas tool and temporary hand-hold
/// state. Written by tool-switch commands; observed by the Viewer toolbar
/// and tool-specific input handlers.
#[derive(Clone, Debug, Default)]
pub struct ToolState {
    pub active: ravel_ui::ToolKind,
    pub hand_hold: bool,
    pub previous: ravel_ui::ToolKind,
}

impl Global for ToolState {}

/// Where a [`ViewerImage`] conversion ran.
///
/// Test-only: the completion criterion for moving the conversion off the UI
/// thread (issue HIGH-08) is a thread-identity assertion, and the only place
/// that identity exists is inside the conversion itself. Carried by the value
/// rather than parked in a static so parallel tests cannot read each other's
/// record.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConversionThread {
    pub id: std::thread::ThreadId,
    pub name: Option<String>,
}

#[cfg(test)]
impl ConversionThread {
    fn current() -> Self {
        let current = std::thread::current();
        Self {
            id: current.id(),
            name: current.name().map(str::to_owned),
        }
    }
}

/// A display-ready frame: the straight-alpha BGRA u8 [`RenderImage`] GPUI's
/// `img` element consumes, plus the dimensions of the evaluation buffer it
/// came from (which may be smaller than the composition).
///
/// Produced on the evaluation worker thread — `ProjectState` wires
/// [`Self::from_display_frame`] into `EvalService`'s result callback, which
/// runs there — so publishing a frame costs the UI thread an `Arc` move.
#[derive(Clone)]
pub struct ViewerImage {
    image: Arc<RenderImage>,
    width: u32,
    height: u32,
    #[cfg(test)]
    converted_on: ConversionThread,
}

impl ViewerImage {
    /// Wrap the display bytes the evaluation worker produced into the image
    /// GPUI's `img` element consumes. Returns `None` for degenerate
    /// dimensions or a byte count that disagrees with them.
    ///
    /// # The display transform happened before this
    ///
    /// The evaluation buffer holds working-space (linear) light and a screen
    /// wants display-encoded bytes. `CM-3` put that conversion here, on the
    /// evaluation worker; `CM-7` moved it onto the GPU, into the dispatch that
    /// runs before the frame is read back (`ravel_nodes::DisplayTransform`).
    /// What arrives is already the finished BGRA image — the same bytes the
    /// render exits reach by their own encode-then-quantise road, to within
    /// the tolerance `docs/specifications/color-management.md` records — so
    /// this function allocates and wraps and performs no per-pixel colour
    /// arithmetic at all.
    ///
    /// It is also orthogonal to `quality` and [`ViewerResolution`]: those
    /// decide *which pixels* are evaluated, the transform decides what a pixel
    /// value means, and a value means the same thing at every resolution.
    ///
    /// The bytes are copied once, out of the shared readback buffer into the
    /// `Vec` [`RenderImage`] requires. The allocation cannot be reused across
    /// frames: it is moved into the image, which GPUI holds (and the panel
    /// keeps alive) until the explicit `drop_image`.
    pub fn from_display_frame(frame: &DisplayFrame) -> Option<Self> {
        let (width, height) = (frame.width(), frame.height());
        let span = tracing::debug_span!("frame_to_render_image", width, height);
        let _guard = span.enter();
        if width == 0 || height == 0 {
            return None;
        }
        // `None` here means the frame is GPU-resident, which the caller has
        // already routed to the surface path; reaching this with one is a bug
        // rather than a degenerate frame, but blanking is still the safe read.
        let bgra = frame.bgra()?;
        if bgra.len() != width as usize * height as usize * 4 {
            return None;
        }

        let buffer = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, bgra.to_vec())?;
        Some(Self {
            image: Arc::new(RenderImage::new(SmallVec::from_elem(
                ImageFrame::new(buffer),
                1,
            ))),
            width,
            height,
            #[cfg(test)]
            converted_on: ConversionThread::current(),
        })
    }

    /// Width of the evaluation buffer this image was converted from.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height of the evaluation buffer this image was converted from.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The image the `img` element draws. Cloning shares the atlas entry, so
    /// the panel's `drop_image` still owns its lifetime.
    pub fn image(&self) -> &Arc<RenderImage> {
        &self.image
    }

    /// Take the image out, for a panel that stores it for the frame's
    /// lifetime.
    pub fn into_image(self) -> Arc<RenderImage> {
        self.image
    }

    #[cfg(test)]
    pub(crate) fn converted_on(&self) -> &ConversionThread {
        &self.converted_on
    }
}

/// `RenderImage` has no `Debug`, and the enum below is matched with `{other:?}`
/// in tests; print what identifies the frame instead of its bytes.
impl std::fmt::Debug for ViewerImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewerImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// Durable shared state: what the Viewer panel should currently display.
/// Published by `ProjectState` from the background evaluation of the root
/// composition output. Results newer than the currently displayed generation
/// are published monotonically, so slow evaluation can still advance while an
/// older result can never overwrite a newer frame. A failed evaluation
/// replaces the previous frame instead of leaving a stale image behind.
#[derive(Clone, Debug)]
pub enum ViewerFrame {
    /// Nothing evaluable. A composition with no active layers still carries
    /// its resolution so the panel can draw an interactive black frame;
    /// `None` means that the project has no composition.
    Blank {
        composition_resolution: Option<(u32, u32)>,
    },
    /// A successfully evaluated frame, already converted for display on the
    /// evaluation worker. The evaluation buffer may be smaller than the
    /// composition, so drawing geometry must use the separate composition
    /// resolution.
    Frame {
        image: ViewerImage,
        composition_resolution: (u32, u32),
    },
    /// A display-encoded GPU texture ready for the GPUI surface path. The
    /// handle stays alive in this durable global until the next frame replaces
    /// it.
    GpuFrame {
        frame: GpuFrameBuffer,
        composition_resolution: (u32, u32),
    },
    /// The latest evaluation failed; the panel drops the stale frame and
    /// shows a black frame with a small error overlay. The composition
    /// resolution keeps that frame on the same viewport transform as normal
    /// and blank output.
    Error {
        message: SharedString,
        composition_resolution: Option<(u32, u32)>,
    },
}

impl Default for ViewerFrame {
    fn default() -> Self {
        Self::Blank {
            composition_resolution: None,
        }
    }
}

impl Global for ViewerFrame {}

/// Durable registry of the live Timeline panel, so the playback controller
/// can drive its playhead. Panel (re)construction overwrites the handle; a
/// stale weak entity simply fails to upgrade.
pub struct TimelinePanelHandle(pub WeakEntity<timeline::TimelineGpuiPanel>);

impl Global for TimelinePanelHandle {}

/// Durable registry of the live NodeEditor panel. The playback controller uses
/// it to post evaluation requests, and Properties uses it for deferred node
/// parameter edits owned by the editor's current network.
pub struct NodeEditorHandle(pub WeakEntity<node_editor::NodeEditorPanel>);

impl Global for NodeEditorHandle {}

/// Durable shared state: the current playback position. Written by the
/// playback controller on every position change; read wherever an
/// `EvalContext` needs the frame under the playhead (e.g. the NodeEditor's
/// selection-driven evaluation), so a parameter edit while paused re-renders
/// the paused frame instead of frame 0.
#[derive(Clone, Copy, Debug)]
pub struct PlaybackPosition {
    pub frame: u64,
    pub fps: ravel_core::types::FrameRate,
}

impl Default for PlaybackPosition {
    fn default() -> Self {
        Self {
            frame: 0,
            fps: ravel_core::types::FrameRate::new(30, 1),
        }
    }
}

impl Global for PlaybackPosition {}

// ---------------------------------------------------------------------------
// Panel rebuild gate
// ---------------------------------------------------------------------------

/// The last [`ProjectState::mirror_epoch`] a panel rebuilt from.
///
/// Every panel that mirrors the document observes `ProjectState`, and its
/// callback is the expensive one (a `Composition` or `Graph` deep compare, a
/// full row walk, a section rebuild). `ProjectState` also notifies for things
/// no panel mirrors — a completed save moves only the window title — and a
/// mid-gesture drag funnels one notify per mouse move. Holding the epoch the
/// panel last synced turns "nothing I show has changed" into an early return.
///
/// Only the `ProjectState` observer is gated. Global-driven paths (a
/// composition switch, a selection change) call the same sync functions
/// directly and must not be filtered by an unchanged document epoch.
///
/// [`ProjectState::mirror_epoch`]: crate::project_state::ProjectState::mirror_epoch
#[derive(Default)]
pub struct MirrorEpoch(Option<u64>);

impl MirrorEpoch {
    /// Whether `epoch` differs from the last one recorded, recording it when it
    /// does. `None` (never synced) always counts as advanced, so a panel built
    /// before its first notify cannot start out gated shut — the panel
    /// constructors do not all sync, so starting the gate closed would leave
    /// one of them showing nothing until the next real change. The cost is one
    /// rebuild per panel on the first notify after startup.
    pub fn advanced(&mut self, epoch: u64) -> bool {
        if self.0 == Some(epoch) {
            return false;
        }
        self.0 = Some(epoch);
        true
    }
}

/// Stand-in for a [`PanelKind`] whose real panel does not exist yet.
///
/// It is focusable and tab-titled like every other pane, so an unimplemented
/// panel still docks, splits, and detaches; only its content is a label.
pub struct PlaceholderPanel {
    kind: PanelKind,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
}

impl PlaceholderPanel {
    pub fn new(
        instance: PanelInstanceId,
        kind: PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let focus_subscriptions = track_panel_focus(instance, &focus_handle, window, cx);
        Self {
            kind,
            focus_handle,
            focus_subscriptions,
        }
    }
}

impl Focusable for PlaceholderPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PlaceholderPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let label = format!(
            "{} {}",
            t!(self.kind.label_key()),
            t!("ui.placeholder_suffix")
        );
        div()
            .id(SharedString::from(self.kind.panel_id()))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .border_t_1()
            .border_color(cx.theme().colors.border)
            .text_color(rgb(0x888888))
            .track_focus(&self.focus_handle)
            .child(SharedString::from(label))
    }
}

/// Returns the translated display name for a [`PanelKind`].
pub fn panel_display_name(kind: PanelKind) -> String {
    t!(kind.label_key())
}

// ---------------------------------------------------------------------------
// The panel factory and the per-window instance registry
// ---------------------------------------------------------------------------

/// Creates the view of one panel instance. **The panel factory** — the single
/// place a panel view comes into existence.
///
/// Reached only through [`PanelViews::view`], which caches what it returns, so
/// there is no second construction path that could drift out of sync with this
/// `match` (adding a panel means one entry here).
fn build_panel_view(
    instance: &PanelInstance,
    window: &mut Window,
    cx: &mut App,
) -> (AnyView, FocusHandle) {
    let id = instance.id;
    // Each arm builds a different concrete panel, and only the concrete type
    // exposes the focus handle `track_panel_focus` watches — `AnyView` has
    // erased it by the time the arm returns. Taking the handle before the
    // conversion is the whole reason for the macro.
    macro_rules! panel {
        ($build:expr) => {{
            let entity = cx.new($build);
            let focus = entity.read(cx).focus_handle(cx);
            (AnyView::from(entity), focus)
        }};
    }
    match instance.kind {
        PanelKind::Outliner => panel!(|cx| outliner::OutlinerGpuiPanel::new(id, window, cx)),
        PanelKind::Timeline => panel!(|cx| timeline::TimelineGpuiPanel::new(id, window, cx)),
        PanelKind::NodeGraph => panel!(|cx| node_editor::NodeEditorPanel::new(id, window, cx)),
        PanelKind::Properties => panel!(|cx| properties::PropertiesGpuiPanel::new(id, window, cx)),
        PanelKind::Viewer => panel!(|cx| viewer::ViewerPanel::new(id, window, cx)),
        PanelKind::MediaBin => panel!(|cx| media_bin::MediaBinGpuiPanel::new(id, window, cx)),
        PanelKind::RenderQueue => {
            panel!(|cx| render_queue::RenderQueueGpuiPanel::new(id, window, cx))
        }
        kind => panel!(|cx| PlaceholderPanel::new(id, kind, window, cx)),
    }
}

/// One live pane view, with the kind it was built for.
struct CachedPanel {
    kind: PanelKind,
    view: AnyView,
    /// The pane's own focus handle, so a host can focus the pane itself rather
    /// than the window frame around it.
    focus: FocusHandle,
}

/// A window's panel views, keyed by [`PanelInstanceId`].
///
/// The same [`PanelKind`] may appear any number of times in a layout, so the
/// instance — not the kind — is what identifies a view. The cache is also what
/// keeps a pane's view state (scroll, zoom, selection) alive across tab
/// switches, splitter drags, tree changes, and a detach round trip: a
/// re-rendered instance gets the view it already had.
#[derive(Default)]
pub struct PanelViews {
    views: RefCell<HashMap<PanelInstanceId, CachedPanel>>,
}

impl PanelViews {
    /// The view of `instance`, building and registering it on first use.
    pub fn view_for(&self, instance: &PanelInstance, window: &mut Window, cx: &mut App) -> AnyView {
        self.entry_for(instance, window, cx).0
    }

    /// Gives `instance`'s pane the keyboard focus, building it if this window
    /// has not rendered it yet.
    ///
    /// A window that opens around a pane has to focus the pane, not its own
    /// frame: `FocusedPanelGlobal` follows real focus events, so a frame that
    /// keeps the focus leaves the workspace with no focused instance and the
    /// commands that act on one (reattach, and every panel-scoped action) with
    /// nothing to work on.
    pub fn focus_pane(&self, instance: &PanelInstance, window: &mut Window, cx: &mut App) {
        let (_, focus) = self.entry_for(instance, window, cx);
        window.focus(&focus, cx);
    }

    /// The cached view and focus handle of `instance`, building them on first
    /// use.
    fn entry_for(
        &self,
        instance: &PanelInstance,
        window: &mut Window,
        cx: &mut App,
    ) -> (AnyView, FocusHandle) {
        let cached = self
            .views
            .borrow()
            .get(&instance.id)
            .filter(|cached| cached.kind == instance.kind)
            .map(|cached| (cached.view.clone(), cached.focus.clone()));
        if let Some(entry) = cached {
            return entry;
        }
        let (view, focus) = build_panel_view(instance, window, cx);
        self.views.borrow_mut().insert(
            instance.id,
            CachedPanel {
                kind: instance.kind,
                view: view.clone(),
                focus: focus.clone(),
            },
        );
        (view, focus)
    }

    /// Entity id of an instance's view, if one was built (exposed for tests: a
    /// changed id means the pane was rebuilt and lost its view state).
    pub fn view_id(&self, instance: PanelInstanceId) -> Option<EntityId> {
        self.views
            .borrow()
            .get(&instance)
            .map(|cached| cached.view.entity_id())
    }

    /// Whether `instance`'s pane currently holds the keyboard focus (exposed
    /// for tests: a window that opens around a pane must focus the pane, not
    /// its own frame).
    pub fn pane_is_focused(&self, instance: PanelInstanceId, window: &Window) -> bool {
        self.views
            .borrow()
            .get(&instance)
            .is_some_and(|cached| cached.focus.is_focused(window))
    }

    /// Drops the views of instances that no longer exist anywhere in the
    /// workspace. An instance that only left this window (a detach) keeps its
    /// view, so reattaching returns the pane the user was working in.
    pub fn retain(&self, live: &[PanelInstance]) {
        self.views
            .borrow_mut()
            .retain(|id, _| live.iter().any(|instance| instance.id == *id));
    }
}

impl PaneContent for PanelViews {
    fn tab_title(&self, instance: &PanelInstance, _window: &Window, _cx: &App) -> SharedString {
        panel_display_name(instance.kind).into()
    }

    /// The panel's icon, lit up while that panel holds the focus — the tab bar
    /// is where the user reads which pane a command will act on.
    fn tab_icon(&self, instance: &PanelInstance, _window: &Window, cx: &App) -> Option<Icon> {
        let icon = Icon::new(crate::assets::RavelIcon::for_panel(instance.kind));
        Some(if is_instance_focused(instance.id, cx) {
            icon.text_color(cx.theme().colors.foreground)
        } else {
            icon
        })
    }

    fn view(&self, instance: &PanelInstance, window: &mut Window, cx: &mut App) -> AnyView {
        self.view_for(instance, window, cx)
    }
}

// A `use super::*;` glob in a test module here crashes rustc 1.95 (SIGBUS
// inside the gpui proc macros); name what the tests need instead.
#[cfg(test)]
mod mirror_epoch_tests {
    use super::MirrorEpoch;

    #[test]
    fn first_sync_and_every_change_pass_the_gate() {
        let mut gate = MirrorEpoch::default();
        // A panel built before its first notify must not start out gated shut.
        assert!(gate.advanced(7));
        assert!(!gate.advanced(7));
        assert!(!gate.advanced(7));
        assert!(gate.advanced(8));
        assert!(!gate.advanced(8));
    }

    #[test]
    fn epoch_zero_is_a_real_epoch_not_unset() {
        let mut gate = MirrorEpoch::default();
        assert!(gate.advanced(0));
        assert!(!gate.advanced(0));
    }
}

// Same rustc 1.95 constraint as above: no `use super::*;` glob here.
#[cfg(test)]
mod viewer_image_tests {
    use super::ViewerImage;
    use ravel_nodes::DisplayFrame;
    use std::sync::Arc;

    /// A display frame of `width` x `height` filled with one BGRA pixel.
    ///
    /// The bytes are already display-encoded: `CM-7` performs that transform
    /// on the GPU before the readback, and all this panel does with the result
    /// is wrap it. The transform itself — and its agreement with
    /// `to_display_rgba8` — is pinned by
    /// `ravel-nodes/tests/display_transform.rs`, which needs an adapter.
    fn display_frame(width: u32, height: u32, bgra: [u8; 4]) -> DisplayFrame {
        let bytes: Vec<u8> = bgra
            .iter()
            .copied()
            .cycle()
            .take((width as usize) * (height as usize) * 4)
            .collect();
        DisplayFrame::new(width, height, Arc::from(bytes))
    }

    #[test]
    fn wraps_the_display_bytes_without_touching_them() {
        // BGRA of the working-space pixel (1.0, 0.5, 0.0, 1.0): blue 0,
        // green 188, red 255. 0.5 linear is sRGB 188, not the 128 a
        // display-referred pipeline produced before `CM-3`.
        let frame = display_frame(2, 2, [0, 188, 255, 255]);
        let converted = ViewerImage::from_display_frame(&frame).unwrap();
        let bytes = converted.image().as_bytes(0).unwrap();
        assert_eq!(&bytes[..4], &[0, 188, 255, 255]);
        assert_eq!(bytes.len(), 2 * 2 * 4);
        assert_eq!(converted.image().size(0).width.0, 2);
        assert_eq!(converted.image().size(0).height.0, 2);
        assert_eq!((converted.width(), converted.height()), (2, 2));
    }

    #[test]
    fn rejects_degenerate_frames() {
        assert!(ViewerImage::from_display_frame(&display_frame(0, 4, [0; 4])).is_none());
        // A byte count that disagrees with the dimensions is a broken
        // readback, not an image: 8 bytes for a 4x4 frame.
        let mismatched = DisplayFrame::new(4, 4, Arc::from(vec![0u8; 8]));
        assert!(ViewerImage::from_display_frame(&mismatched).is_none());
    }
}
