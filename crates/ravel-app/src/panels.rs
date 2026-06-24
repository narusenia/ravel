// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Placeholder panel views for the dock layout.

use gpui::*;
use gpui_component::dock::{Panel, PanelEvent};
use std::sync::Arc;

pub struct PlaceholderPanel {
    name: &'static str,
    focus_handle: FocusHandle,
}

impl PlaceholderPanel {
    pub fn new(name: &'static str, cx: &mut Context<Self>) -> Self {
        Self {
            name,
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
            .child(format!("{} (placeholder)", self.name))
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
