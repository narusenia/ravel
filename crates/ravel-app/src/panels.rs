// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Placeholder panel views for the dock layout.

use gpui::*;
use gpui_component::dock::{Panel, PanelEvent};
use ravel_ui::panel::PanelKind;
use std::sync::Arc;

/// Global storing the most recently focused panel kind.
pub struct FocusedPanelGlobal(pub Option<PanelKind>);

impl Global for FocusedPanelGlobal {}

pub struct PlaceholderPanel {
    kind: Option<PanelKind>,
    name: &'static str,
    label: SharedString,
    focus_handle: FocusHandle,
}

impl PlaceholderPanel {
    pub fn new(name: &'static str, kind: Option<PanelKind>, cx: &mut Context<Self>) -> Self {
        let label = SharedString::from(format!("{name} (placeholder)"));
        Self {
            kind,
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
        let kind = self.kind;
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(0x888888))
            .track_focus(&self.focus_handle)
            .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
                if let Some(k) = kind {
                    cx.set_global(FocusedPanelGlobal(Some(k)));
                }
            })
            .child(self.label.clone())
    }
}

/// Create a placeholder panel as `Arc<dyn PanelView>`.
pub fn placeholder_panel(
    name: &'static str,
    cx: &mut App,
) -> Arc<dyn gpui_component::dock::PanelView> {
    let entity = cx.new(|cx| PlaceholderPanel::new(name, None, cx));
    Arc::new(entity)
}

/// Returns the human-readable display name for a [`PanelKind`].
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
pub fn panel_for_kind(
    kind: PanelKind,
    _window: &mut Window,
    cx: &mut App,
) -> Arc<dyn gpui_component::dock::PanelView> {
    let name = panel_display_name(kind);
    let entity = cx.new(|cx| PlaceholderPanel::new(name, Some(kind), cx));
    Arc::new(entity)
}
