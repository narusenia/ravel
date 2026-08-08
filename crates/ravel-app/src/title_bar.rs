// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's custom window title bar, shared by every window.
//!
//! [`RavelTitleBar`] is the chrome all windows have in common: a
//! [`gpui_component::TitleBar`] (platform window controls plus the drag
//! region), a label centered on the window, and two slots the window kind
//! fills. The main window puts the application name in the leading slot; a
//! detached window puts its always-on-top pin in the trailing slot. Both go
//! through this one component, so the drag region, the height, and the
//! centering correction have a single definition.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{ActiveTheme, TitleBar, h_flex};
use ravel_i18n::t;
use std::path::Path;

/// Left inset [`TitleBar`] reserves for the platform window controls (the
/// macOS traffic lights sit in it).
///
/// gpui-component keeps its own copy of this private, so the value is mirrored
/// here. Anything centered inside the bar has to reserve the same width on the
/// right, or it renders half an inset right of the window's true center.
#[cfg(target_os = "macos")]
pub const WINDOW_CONTROLS_INSET: Pixels = px(80.);
#[cfg(not(target_os = "macos"))]
pub const WINDOW_CONTROLS_INSET: Pixels = px(12.);

/// Width the platform window controls take on the *right* of the bar.
///
/// macOS draws its traffic lights in the left inset and nothing on the right;
/// the other platforms draw minimize / maximize / close as three square buttons
/// of the bar's own height. The bar's content box already ends where they
/// begin, so a centered element has to reserve the same width on the left to
/// stay on the window's center.
///
/// Derived from the bar's height rather than a literal, and branched with
/// `cfg!` rather than `#[cfg]`, so both arms stay type-checked on every
/// platform.
fn trailing_controls_width() -> Pixels {
    if cfg!(target_os = "macos") {
        px(0.)
    } else {
        gpui_component::TITLE_BAR_HEIGHT * 3.
    }
}

/// Inset the trailing slot keeps from the bar's right end, mirroring the gap
/// the traffic lights leave on the left (`traffic_light_position` = 9px).
const TRAILING_SLOT_INSET: Pixels = px(9.);

/// Ravel's window title bar: shared chrome plus per-window-kind slots.
///
/// Build it with the centered label, then fill the slots the window kind
/// needs. Slot children are appended in call order.
#[derive(IntoElement)]
pub struct RavelTitleBar {
    /// Subdued label centered on the window (project name, window title).
    center: SharedString,
    /// Left-aligned children, after the window controls' inset.
    leading: Vec<AnyElement>,
    /// Right-aligned children, before the platform window controls.
    trailing: Vec<AnyElement>,
}

impl RavelTitleBar {
    /// A bar whose centered label is `center` and whose slots are empty.
    pub fn new(center: impl Into<SharedString>) -> Self {
        Self {
            center: center.into(),
            leading: Vec::new(),
            trailing: Vec::new(),
        }
    }

    /// Appends a child to the leading (left) slot.
    pub fn leading(mut self, child: impl IntoElement) -> Self {
        self.leading.push(child.into_any_element());
        self
    }

    /// Appends a child to the trailing (right) slot.
    pub fn trailing(mut self, child: impl IntoElement) -> Self {
        self.trailing.push(child.into_any_element());
        self
    }
}

impl RenderOnce for RavelTitleBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let has_trailing = !self.trailing.is_empty();
        TitleBar::new().child(
            h_flex()
                .id("title-bar-content")
                .relative()
                .flex_1()
                .h_full()
                .items_center()
                .gap_3()
                // Centered, subdued label. A plain overlay with no listeners:
                // it neither captures clicks nor blocks the platform drag
                // region. The bar's content box starts after the window
                // controls' inset and ends where the platform buttons begin,
                // so the overlay mirrors both to land on the window's true
                // center. This is the only place that correction lives.
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .pl(trailing_controls_width())
                        .pr(WINDOW_CONTROLS_INSET)
                        .flex()
                        .items_center()
                        .justify_center()
                        .overflow_hidden()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().colors.muted_foreground)
                                .child(self.center),
                        ),
                )
                .children(self.leading)
                // Only pushed when something is actually in the trailing slot,
                // so a bar without one keeps its leading children's spacing.
                .when(has_trailing, |this| {
                    this.child(div().flex_1()).child(
                        // The bar's content box ends at the window edge on
                        // macOS (nothing is drawn on the right), so the slot
                        // carries its own inset — matching the traffic lights'
                        // on the other side instead of sitting flush.
                        h_flex()
                            .items_center()
                            .gap_1()
                            .pr(TRAILING_SLOT_INSET)
                            .children(self.trailing),
                    )
                }),
        )
    }
}

/// Display name of the open project: the project file's stem, or the
/// localized "untitled" placeholder before the first save.
pub fn project_display_name(path: Option<&Path>) -> String {
    path.and_then(|p| p.file_stem())
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| t!("app.untitled_project"))
}

/// OS window title for the open project: `<project> — Ravel`.
pub fn window_title(path: Option<&Path>) -> String {
    format!("{} — {}", project_display_name(path), t!("app.title"))
}

/// Renders the main window's title bar: the project name centered, the
/// application name leading, and — off macOS — the application menus beside
/// it.
///
/// Workspace presets are switched through `Cmd+F1`–`F4` and the Workspace
/// menu; the bar deliberately carries no preset buttons.
pub fn render_main_title_bar(project_name: &str, cx: &App) -> impl IntoElement {
    let bar = RavelTitleBar::new(project_name.to_owned()).leading(
        div()
            .text_sm()
            .text_color(cx.theme().colors.foreground)
            .child(t!("app.title")),
    );
    // macOS keeps the menus where they belong, in the OS menu bar that
    // `App::set_menus` fills. No other platform implements that call, so
    // gpui-component's in-window `AppMenuBar` is the only place they can go.
    // This is the single platform branch for the menus, and `cfg!` rather than
    // `#[cfg]` keeps both arms type-checked everywhere.
    match crate::workspace::app_menu_bar(cx) {
        Some(menu_bar) if !cfg!(target_os = "macos") => bar.leading(
            // The bar renders `size_full`, which would claim the whole title
            // bar row; this wrapper keeps it to its content's width.
            h_flex().items_center().child(menu_bar),
        ),
        _ => bar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui::*` re-exports gpui's `test` attribute macro; shadow it
    // back to the built-in one for these plain unit tests.
    use core::prelude::v1::test;

    #[test]
    fn project_display_name_uses_file_stem() {
        let path = Path::new("/tmp/projects/my_film.ravprj");
        assert_eq!(project_display_name(Some(path)), "my_film");
    }

    #[test]
    fn window_title_joins_project_name_and_app_title() {
        let path = Path::new("/x/demo.ravprj");
        assert!(window_title(Some(path)).starts_with("demo — "));
    }
}
