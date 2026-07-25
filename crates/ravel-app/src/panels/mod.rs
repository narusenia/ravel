// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Panel views for the dock layout.

pub mod node_editor;
mod param_edit;
pub mod timeline;
pub mod viewer;

pub mod properties;

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::dock::{Panel, PanelEvent};
use ravel_core::composition::{Composition, Document};
use ravel_core::id::{CompId, LayerId, NodeId};
use ravel_core::types::FrameBuffer;
use ravel_i18n::t;
use ravel_ui::panel::PanelKind;
use ravel_ui::properties::PropertyValue;
use std::collections::HashSet;
use std::sync::Arc;

/// Global storing the panel that currently contains the focused element.
pub struct FocusedPanelGlobal(pub Option<PanelKind>);

impl Global for FocusedPanelGlobal {}

pub(crate) fn is_panel_focused(kind: PanelKind, cx: &App) -> bool {
    cx.try_global::<FocusedPanelGlobal>().and_then(|g| g.0) == Some(kind)
}

/// Standard dock tab title: panel icon + label, tinted by focus state.
pub(crate) fn tab_title(kind: Option<PanelKind>, label: SharedString, color: Hsla) -> Div {
    let mut row = div()
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .text_color(color);
    if let Some(kind) = kind {
        row = row.child(
            gpui_component::Icon::new(crate::assets::RavelIcon::for_panel(kind))
                .text_color(color)
                .size_3p5(),
        );
    }
    row.child(div().child(label))
}

fn track_panel_focus<T: 'static>(
    kind: PanelKind,
    focus_handle: &FocusHandle,
    window: &mut Window,
    cx: &mut Context<T>,
) -> [Subscription; 2] {
    let focus_in = cx.on_focus_in(focus_handle, window, move |_this, _window, cx| {
        cx.set_global(FocusedPanelGlobal(Some(kind)));
    });
    let focus_out = cx.on_focus_out(focus_handle, window, move |_this, _event, _window, cx| {
        if is_panel_focused(kind, cx) {
            cx.set_global(FocusedPanelGlobal(None));
        }
    });
    [focus_in, focus_out]
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
}

/// Global signal: NodeEditorPanel sets this when selection changes.
#[derive(Clone, Default)]
pub struct SelectedPropertiesTarget(pub PropertiesTarget);

impl Global for SelectedPropertiesTarget {}

/// Durable shared state: the canvas-level node selection. The node editor
/// reads and writes this instead of keeping a panel-local set; future
/// consumers (Viewer tool system, bbox overlay) observe the same global.
#[derive(Clone, Debug, Default)]
pub struct CanvasSelection {
    /// The network owning the selected nodes (`None` when no network is open).
    pub path: Option<ravel_ui::document::NetworkPath>,
    pub nodes: HashSet<NodeId>,
}

impl Global for CanvasSelection {}

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
/// is part of the switch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveComposition(pub Option<CompId>);

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

/// Drop a Properties target left pointing at a layer the selection no longer
/// holds. The target is derived from the selection, so the selection writers
/// own its lifetime; a `Nodes` target belongs to the node editor and is never
/// stolen here.
fn drop_stale_layer_properties_target(cx: &mut App) {
    let selection = layer_selection(cx);
    let stale = matches!(
        cx.try_global::<SelectedPropertiesTarget>().map(|t| &t.0),
        Some(PropertiesTarget::Layer { comp_id, layer_id })
            if selection.comp != Some(*comp_id) || !selection.contains(*layer_id)
    );
    if stale {
        cx.set_global(SelectedPropertiesTarget(PropertiesTarget::Empty));
    }
}

/// Select nothing, keeping the active composition.
pub fn clear_layer_selection(cx: &mut App) {
    set_layer_selection(Vec::new(), cx);
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

/// Global signal: PropertiesPanel sets this when a value is edited.
///
/// `commit == false` is a live edit (e.g. mid-scrub): apply the value but do
/// not record undo. `commit == true` ends the gesture and records one undo
/// snapshot for the whole edit.
#[derive(Clone, Debug)]
pub struct PropertyChanged {
    pub node_ids: Vec<NodeId>,
    pub key: String,
    pub value: PropertyValue,
    pub commit: bool,
}

impl Global for PropertyChanged {}

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

/// Durable registry of the live NodeEditor panel, so the playback controller
/// can post evaluation requests through its `EvalService`.
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

pub struct PlaceholderPanel {
    kind: Option<PanelKind>,
    panel_id: &'static str,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: Option<[Subscription; 2]>,
    #[allow(dead_code)]
    focused_sub: Subscription,
}

impl PlaceholderPanel {
    pub fn new(
        panel_id: &'static str,
        kind: Option<PanelKind>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focused_sub = cx.observe_global::<FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });
        let focus_handle = cx.focus_handle();
        let focus_subscriptions =
            kind.map(|kind| track_panel_focus(kind, &focus_handle, window, cx));
        Self {
            kind,
            panel_id,
            focus_handle,
            focus_subscriptions,
            focused_sub,
        }
    }
}

impl Panel for PlaceholderPanel {
    fn panel_name(&self) -> &'static str {
        self.panel_id
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let display = self
            .kind
            .map(|k| t!(k.label_key()))
            .unwrap_or_else(|| self.panel_id.to_string());
        let focused = self.kind.is_some_and(|k| is_panel_focused(k, cx));
        let color = if focused {
            cx.theme().colors.foreground
        } else {
            cx.theme().colors.muted_foreground
        };
        tab_title(self.kind, SharedString::from(display), color)
    }
}

impl EventEmitter<PanelEvent> for PlaceholderPanel {}

impl Focusable for PlaceholderPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PlaceholderPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let suffix = t!("ui.placeholder_suffix");
        let label = self
            .kind
            .map(|k| format!("{} {suffix}", t!(k.label_key())))
            .unwrap_or_else(|| format!("{} {suffix}", self.panel_id));
        div()
            .id(SharedString::from(
                self.kind.map(|k| k.panel_id()).unwrap_or(self.panel_id),
            ))
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

/// Create a placeholder panel as `Arc<dyn PanelView>`.
pub fn placeholder_panel(
    name: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> Arc<dyn gpui_component::dock::PanelView> {
    let entity = cx.new(|cx| PlaceholderPanel::new(name, None, window, cx));
    Arc::new(entity)
}

/// Returns the translated display name for a [`PanelKind`].
pub fn panel_display_name(kind: PanelKind) -> String {
    t!(kind.label_key())
}

/// Create a panel view for the given [`PanelKind`].
pub fn panel_for_kind(
    kind: PanelKind,
    window: &mut Window,
    cx: &mut App,
) -> Arc<dyn gpui_component::dock::PanelView> {
    match kind {
        PanelKind::Timeline => {
            let entity = cx.new(|cx| timeline::TimelineGpuiPanel::new(window, cx));
            Arc::new(entity)
        }
        PanelKind::NodeGraph => {
            let entity = cx.new(|cx| node_editor::NodeEditorPanel::new(window, cx));
            Arc::new(entity)
        }
        PanelKind::Properties => {
            let entity = cx.new(|cx| properties::PropertiesGpuiPanel::new(window, cx));
            Arc::new(entity)
        }
        PanelKind::Viewer => {
            let entity = cx.new(|cx| viewer::ViewerPanel::new(window, cx));
            Arc::new(entity)
        }
        _ => {
            let panel_id = kind.panel_id();
            let entity = cx.new(|cx| PlaceholderPanel::new(panel_id, Some(kind), window, cx));
            Arc::new(entity)
        }
    }
}
