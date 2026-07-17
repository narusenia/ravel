// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Panel views for the dock layout.

pub mod node_editor;
pub mod timeline;
pub mod viewer;

pub mod properties;

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::dock::{Panel, PanelEvent};
use ravel_core::graph::Node;
use ravel_core::id::NodeId;
use ravel_core::types::FrameBuffer;
use ravel_i18n::t;
use ravel_ui::panel::PanelKind;
use ravel_ui::properties::PropertyValue;
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

/// What the Properties panel should currently inspect.
#[derive(Clone, Default)]
pub enum PropertiesTarget {
    #[default]
    Empty,
    Nodes {
        ids: Vec<NodeId>,
        nodes: Vec<Arc<Node>>,
    },
    Layer {
        /// Composition owning the layer, for routing edits back into the
        /// document.
        comp_id: ravel_core::id::CompId,
        layer: Box<ravel_core::composition::Layer>,
        frame: u64,
        fps: ravel_core::types::FrameRate,
        resolution: (u32, u32),
    },
}

/// Global signal: NodeEditorPanel sets this when selection changes.
#[derive(Clone, Default)]
pub struct SelectedPropertiesTarget(pub PropertiesTarget);

impl Global for SelectedPropertiesTarget {}

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

/// Global signal: the current FrameBuffer to display in the Viewer panel.
/// Set by the NodeEditor after evaluating the selected output node.
#[derive(Clone, Default)]
pub struct ViewerFrame(pub Option<Arc<FrameBuffer>>);

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
