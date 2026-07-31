// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Standalone validation binary for ravel-dock.
//!
//! Builds the four built-in workspace presets with dummy panes, wires the
//! emitted [`DockEvent`]s back into the model, and toggles the gpui-component
//! theme. Pre-cutover manual verification of the dock happens here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Bounds, Context, Entity, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, WindowBounds,
    WindowOptions, div, px, size,
};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme as _, Root, Theme, ThemeMode, h_flex, v_flex};
use ravel_dock::{DockEvent, DockRoot, PaneContent, activate_tab, set_ratio_at};
use ravel_ui::layout::{LayoutNode, PanelInstance, PanelInstanceId};
use ravel_ui::preset::BuiltinPreset;

/// A colored placeholder pane standing in for a real panel.
struct DummyPane {
    title: SharedString,
    color_ix: usize,
}

impl Render for DummyPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let accents = [
            colors.chart_1,
            colors.chart_2,
            colors.chart_3,
            colors.chart_4,
            colors.chart_5,
        ];
        let accent = accents[self.color_ix % accents.len()];
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .bg(colors.background)
            .child(div().text_lg().text_color(accent).child(self.title.clone()))
            .child(
                div()
                    .text_sm()
                    .text_color(colors.muted_foreground)
                    .child("dummy pane content"),
            )
    }
}

/// [`PaneContent`] over cached dummy panes, one stable view per instance id.
#[derive(Default)]
struct GalleryContent {
    views: RefCell<HashMap<PanelInstanceId, AnyView>>,
}

impl GalleryContent {
    fn label(instance: &PanelInstance) -> SharedString {
        format!("{} #{}", instance.kind.panel_id(), instance.id.0).into()
    }

    /// Drops cached views (on preset switches instance ids are reused for
    /// different panel kinds).
    fn clear(&self) {
        self.views.borrow_mut().clear();
    }
}

impl PaneContent for GalleryContent {
    fn tab_title(&self, instance: &PanelInstance, _window: &Window, _cx: &App) -> SharedString {
        Self::label(instance)
    }

    fn view(&self, instance: &PanelInstance, _window: &mut Window, cx: &mut App) -> AnyView {
        self.views
            .borrow_mut()
            .entry(instance.id)
            .or_insert_with(|| {
                AnyView::from(cx.new(|_cx| DummyPane {
                    title: Self::label(instance),
                    color_ix: instance.id.0 as usize,
                }))
            })
            .clone()
    }

    fn empty_state(&self, _window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        Some(
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("empty area"),
                )
                .into_any_element(),
        )
    }
}

/// The gallery window: preset toolbar above a [`DockRoot`].
struct GalleryApp {
    layout: LayoutNode,
    content: Rc<GalleryContent>,
    dock: Entity<DockRoot>,
    _subscription: Subscription,
}

impl GalleryApp {
    fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let layout = BuiltinPreset::Edit.preset().layout;
        let content = Rc::new(GalleryContent::default());
        let dock = cx.new(|_cx| DockRoot::new(layout.clone(), content.clone()));
        let subscription = cx.subscribe(&dock, |this, _dock, event, cx| {
            this.on_dock_event(event, cx)
        });
        Self {
            layout,
            content,
            dock,
            _subscription: subscription,
        }
    }

    /// Applies a dock event to the model and pushes the updated tree back —
    /// the same round-trip `ravel-app` will perform after cutover.
    fn on_dock_event(&mut self, event: &DockEvent, cx: &mut Context<Self>) {
        let applied = match event {
            DockEvent::SplitRatioChanged { path, ratio } => {
                set_ratio_at(&mut self.layout, path, *ratio)
            }
            DockEvent::TabActivated { instance } => activate_tab(&mut self.layout, *instance),
        };
        if !applied {
            return;
        }
        let layout = self.layout.clone();
        self.dock.update(cx, |dock, cx| dock.set_layout(layout, cx));
    }

    fn switch_preset(&mut self, preset: BuiltinPreset, cx: &mut Context<Self>) {
        self.layout = preset.preset().layout;
        self.content.clear();
        let layout = self.layout.clone();
        self.dock.update(cx, |dock, cx| dock.set_layout(layout, cx));
        cx.notify();
    }

    fn toggle_theme(window: &mut Window, cx: &mut App) {
        let next = if cx.theme().is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(next, Some(window), cx);
    }
}

impl Render for GalleryApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let toolbar = h_flex()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .children(BuiltinPreset::ALL.map(|preset| {
                let weak = weak.clone();
                Button::new(preset.label_key())
                    .label(format!("{preset:?}"))
                    .on_click(move |_, _window, cx| {
                        weak.update(cx, |this, cx| this.switch_preset(preset, cx))
                            .ok();
                    })
            }))
            .child(
                Button::new("toggle-theme")
                    .label("Toggle theme")
                    .on_click(|_, window, cx| Self::toggle_theme(window, cx)),
            );
        v_flex()
            .size_full()
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(self.dock.clone()))
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            gpui_component::Theme::sync_system_appearance(None, cx);
            if let Err(e) = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(1280.0), px(800.0)),
                        cx,
                    ))),
                    titlebar: Some(gpui_component::TitleBar::title_bar_options()),
                    ..Default::default()
                },
                |window, cx| {
                    let app = cx.new(|cx| GalleryApp::new(window, cx));
                    cx.new(|cx| Root::new(app, window, cx))
                },
            ) {
                eprintln!("[gallery] failed to open window: {e}");
                cx.quit();
                return;
            }
            cx.activate(true);
        });
}
