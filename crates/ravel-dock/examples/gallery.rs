// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Standalone validation binary for ravel-dock.
//!
//! Builds the four built-in workspace presets with dummy panes, wires the
//! emitted [`DockEvent`]s back into the model, and toggles the gpui-component
//! theme. Pre-cutover manual verification of the dock happens here: tab drag
//! and drop (split on the edges, merge in the middle), the area menu, and
//! splitter drags all round-trip through the real
//! [`WorkspaceLayout`] operations.

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
use ravel_dock::{
    DockEvent, DockRoot, PaneContent, activate_tab, apply_area_action, apply_tab_drop, set_ratio_at,
};
use ravel_ui::layout::{PanelInstance, PanelInstanceId, WorkspaceLayout};
use ravel_ui::preset::BuiltinPreset;
use ravel_ui::window::WindowId;

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
///
/// The gallery hosts a single window, so it owns a whole [`WorkspaceLayout`]
/// (the layout operations a drop or an area action needs live there) and always
/// renders the main window's tree.
struct GalleryApp {
    layout: WorkspaceLayout,
    window_id: WindowId,
    content: Rc<GalleryContent>,
    dock: Entity<DockRoot>,
    _subscription: Subscription,
}

impl GalleryApp {
    fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let layout = workspace_for(BuiltinPreset::Edit);
        let window_id = layout.main_window().id;
        let root = layout.main_window().root.clone();
        let content = Rc::new(GalleryContent::default());
        let dock = cx.new(|cx| DockRoot::new(root, content.clone(), cx));
        let subscription = cx.subscribe(&dock, |this, _dock, event, cx| {
            this.on_dock_event(event, cx)
        });
        Self {
            layout,
            window_id,
            content,
            dock,
            _subscription: subscription,
        }
    }

    /// Applies a dock event to the model and pushes the updated tree back —
    /// the same round-trip `ravel-app` will perform after cutover.
    fn on_dock_event(&mut self, event: &DockEvent, cx: &mut Context<Self>) {
        let window = self.window_id;
        let applied = match event {
            DockEvent::SplitRatioChanged { path, ratio } => {
                let Some(target) = self.layout.window_mut(window) else {
                    return;
                };
                set_ratio_at(&mut target.root, path, *ratio)
            }
            DockEvent::TabActivated { instance } => {
                let Some(target) = self.layout.window_mut(window) else {
                    return;
                };
                activate_tab(&mut target.root, *instance)
            }
            DockEvent::TabDropped {
                instance,
                anchor,
                zone,
            } => report(apply_tab_drop(
                &mut self.layout,
                window,
                *instance,
                *anchor,
                *zone,
            )),
            DockEvent::AreaActionRequested { instance, action } => report(apply_area_action(
                &mut self.layout,
                window,
                *instance,
                *action,
            )),
            DockEvent::TabDetachRequested {
                instance,
                screen_position,
            } => {
                // Creating windows is the host's job (`ravel-app`), and the
                // gallery only ever has one, so the request is just reported.
                eprintln!(
                    "[gallery] detach requested for {instance:?} at {screen_position:?} \
                     (the gallery does not open windows)"
                );
                false
            }
        };
        if !applied {
            return;
        }
        self.push_layout(cx);
    }

    /// Hands the dock the tree it should render now.
    fn push_layout(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.layout.window(self.window_id) else {
            return;
        };
        let root = target.root.clone();
        self.dock.update(cx, |dock, cx| dock.set_layout(root, cx));
    }

    fn switch_preset(&mut self, preset: BuiltinPreset, cx: &mut Context<Self>) {
        self.layout = workspace_for(preset);
        self.window_id = self.layout.main_window().id;
        self.content.clear();
        self.push_layout(cx);
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

/// A single-window workspace holding `preset`'s tree.
fn workspace_for(preset: BuiltinPreset) -> WorkspaceLayout {
    WorkspaceLayout::new(preset.preset().layout).expect("built-in presets are valid layouts")
}

/// Logs a rejected layout operation and reports whether anything changed.
/// Rejections are expected (closing the last area, splitting a lone tab) and
/// must leave the rendered tree alone.
fn report(result: Result<(), ravel_ui::layout::LayoutError>) -> bool {
    match result {
        Ok(()) => true,
        Err(e) => {
            eprintln!("[gallery] layout operation rejected: {e}");
            false
        }
    }
}

fn main() {
    // The area menu labels come from the locale assets; the gallery is a dev
    // binary so it reads them straight out of the repository.
    let locale_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    if let Err(e) = ravel_i18n::init(&locale_dir, "en") {
        eprintln!("[gallery] locale load failed, menus will show raw keys: {e}");
    }
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
