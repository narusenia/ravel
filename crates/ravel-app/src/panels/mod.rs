// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Panel views for the dock layout.

pub mod timeline;

use gpui::*;
use gpui_component::dock::{Panel, PanelEvent};
use ravel_i18n::t;
use ravel_ui::panel::PanelKind;
use std::sync::Arc;

/// Global storing the most recently focused panel kind.
pub struct FocusedPanelGlobal(pub Option<PanelKind>);

impl Global for FocusedPanelGlobal {}

pub struct PlaceholderPanel {
    kind: Option<PanelKind>,
    panel_id: &'static str,
    focus_handle: FocusHandle,
}

impl PlaceholderPanel {
    pub fn new(panel_id: &'static str, kind: Option<PanelKind>, cx: &mut Context<Self>) -> Self {
        Self {
            kind,
            panel_id,
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Panel for PlaceholderPanel {
    fn panel_name(&self) -> &'static str {
        self.panel_id
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let display = self
            .kind
            .map(|k| t!(k.label_key()))
            .unwrap_or_else(|| self.panel_id.to_string());
        SharedString::from(display)
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
        let suffix = t!("ui.placeholder_suffix");
        let label = self
            .kind
            .map(|k| format!("{} {suffix}", t!(k.label_key())))
            .unwrap_or_else(|| format!("{} {suffix}", self.panel_id));
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
            .child(SharedString::from(label))
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

/// Returns the translated display name for a [`PanelKind`].
pub fn panel_display_name(kind: PanelKind) -> String {
    t!(kind.label_key())
}

/// Create a panel view for the given [`PanelKind`].
///
/// Returns a concrete implementation for panels that have one, or a
/// placeholder for panels not yet implemented.
pub fn panel_for_kind(
    kind: PanelKind,
    _window: &mut Window,
    cx: &mut App,
) -> Arc<dyn gpui_component::dock::PanelView> {
    match kind {
        PanelKind::Timeline => {
            let entity = cx.new(timeline::TimelineGpuiPanel::new);
            Arc::new(entity)
        }
        _ => {
            let panel_id = kind.panel_id();
            let entity = cx.new(|cx| PlaceholderPanel::new(panel_id, Some(kind), cx));
            Arc::new(entity)
        }
    }
}
