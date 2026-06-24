// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Placeholder panel views for the dock layout.

use gpui::*;
use gpui_component::dock::{Panel, PanelEvent};
use ravel_ui::panel::PanelKind;
use std::sync::Arc;

pub struct PlaceholderPanel {
    name: &'static str,
    label: SharedString,
    focus_handle: FocusHandle,
}

impl PlaceholderPanel {
    pub fn new(name: &'static str, cx: &mut Context<Self>) -> Self {
        let label = SharedString::from(format!("{name} (placeholder)"));
        Self {
            name,
            label,
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Panel for PlaceholderPanel {
    fn panel_name(&self) -> &'static str {
        self.name
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(self.name)
    }
}

impl EventEmitter<PanelEvent> for PlaceholderPanel {}

impl Focusable for PlaceholderPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PlaceholderPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(0x888888))
            .child(self.label.clone())
    }
}

/// Create a placeholder panel as `Arc<dyn PanelView>`.
pub fn placeholder_panel(
    name: &'static str,
    cx: &mut App,
) -> Arc<dyn gpui_component::dock::PanelView> {
    let entity = cx.new(|cx| PlaceholderPanel::new(name, cx));
    Arc::new(entity)
}

/// Returns the human-readable display name for a [`PanelKind`].
///
/// This name is used both as the DockArea panel identifier and as the
/// placeholder title shown in the UI.
pub fn panel_display_name(kind: PanelKind) -> &'static str {
    match kind {
        PanelKind::Outliner => "Outliner",
        PanelKind::NodeGraph => "Node Graph",
        PanelKind::Timeline => "Timeline",
        PanelKind::Viewer => "Viewer",
        PanelKind::Dopesheet => "Dopesheet",
        PanelKind::Properties => "Properties",
        PanelKind::MediaBin => "Media Bin",
        PanelKind::CurveEditor => "Curve Editor",
        PanelKind::Waveform => "Waveform",
        PanelKind::Vectorscope => "Vectorscope",
        PanelKind::Histogram => "Histogram",
        PanelKind::Parade => "Parade",
        PanelKind::TextEditor => "Text Editor",
        PanelKind::ShaderEditor => "Shader Editor",
        PanelKind::LuaConsole => "Lua Console",
        PanelKind::RenderQueue => "Render Queue",
    }
}

/// Create a placeholder panel for the given [`PanelKind`].
pub fn panel_for_kind(kind: PanelKind, cx: &mut App) -> Arc<dyn gpui_component::dock::PanelView> {
    placeholder_panel(panel_display_name(kind), cx)
}
