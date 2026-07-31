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

use gpui::*;
use gpui_component::{ActiveTheme, Icon};
use ravel_core::composition::{Composition, Document};
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_core::types::FrameBuffer;
use ravel_dock::PaneContent;
use ravel_i18n::t;
use ravel_ui::layout::{PanelInstance, PanelInstanceId};
use ravel_ui::panel::PanelKind;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
    /// A successfully evaluated frame. The evaluation buffer may be smaller
    /// than the composition, so drawing geometry must use the separate
    /// composition resolution.
    Frame {
        buffer: Arc<FrameBuffer>,
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
fn build_panel_view(instance: &PanelInstance, window: &mut Window, cx: &mut App) -> AnyView {
    let id = instance.id;
    match instance.kind {
        PanelKind::Outliner => cx
            .new(|cx| outliner::OutlinerGpuiPanel::new(id, window, cx))
            .into(),
        PanelKind::Timeline => cx
            .new(|cx| timeline::TimelineGpuiPanel::new(id, window, cx))
            .into(),
        PanelKind::NodeGraph => cx
            .new(|cx| node_editor::NodeEditorPanel::new(id, window, cx))
            .into(),
        PanelKind::Properties => cx
            .new(|cx| properties::PropertiesGpuiPanel::new(id, window, cx))
            .into(),
        PanelKind::Viewer => cx.new(|cx| viewer::ViewerPanel::new(id, window, cx)).into(),
        PanelKind::MediaBin => cx
            .new(|cx| media_bin::MediaBinGpuiPanel::new(id, window, cx))
            .into(),
        kind => cx
            .new(|cx| PlaceholderPanel::new(id, kind, window, cx))
            .into(),
    }
}

/// One live pane view, with the kind it was built for.
struct CachedPanel {
    kind: PanelKind,
    view: AnyView,
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
        let cached = self
            .views
            .borrow()
            .get(&instance.id)
            .filter(|cached| cached.kind == instance.kind)
            .map(|cached| cached.view.clone());
        if let Some(view) = cached {
            return view;
        }
        let view = build_panel_view(instance, window, cx);
        self.views.borrow_mut().insert(
            instance.id,
            CachedPanel {
                kind: instance.kind,
                view: view.clone(),
            },
        );
        view
    }

    /// Entity id of an instance's view, if one was built (exposed for tests: a
    /// changed id means the pane was rebuilt and lost its view state).
    pub fn view_id(&self, instance: PanelInstanceId) -> Option<EntityId> {
        self.views
            .borrow()
            .get(&instance)
            .map(|cached| cached.view.entity_id())
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
