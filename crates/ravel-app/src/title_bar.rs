// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's custom window title bar, shared by every window.
//!
//! [`RavelTitleBar`] is the chrome all windows have in common: a
//! [`gpui_component::TitleBar`] (platform window controls plus the drag
//! region), a label centered on the window, and two slots the window kind
//! fills. The main window puts the application name and the workspace preset
//! switcher in the leading slot; a detached window puts its always-on-top pin
//! in the trailing slot. Both go through this one component, so the drag
//! region, the height, and the centering correction have a single definition.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::Selectable;
use gpui_component::Sizable;
use gpui_component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_component::{ActiveTheme, TitleBar, h_flex};
use ravel_i18n::t;
use ravel_ui::command::CommandId;
use ravel_ui::preset::BuiltinPreset;
use std::path::Path;

use crate::workspace::RavelWorkspace;

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
/// the other platforms draw minimize / maximize / close as three square
/// buttons of the bar's own height. The bar's content box already ends where
/// they begin, so a centered element has to reserve the same width on the left
/// to stay on the window's center.
#[cfg(target_os = "macos")]
const TRAILING_CONTROLS_WIDTH: Pixels = px(0.);
#[cfg(not(target_os = "macos"))]
const TRAILING_CONTROLS_WIDTH: Pixels = px(3. * 34.); // gpui_component::TITLE_BAR_HEIGHT

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
                        .pl(TRAILING_CONTROLS_WIDTH)
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
                .when(has_trailing, |this| this.child(div().flex_1()))
                .children(self.trailing),
        )
    }
}

/// Maps a built-in workspace preset to the command that activates it.
fn preset_command(preset: BuiltinPreset) -> CommandId {
    match preset {
        BuiltinPreset::Edit => CommandId::WorkspaceEdit,
        BuiltinPreset::Node => CommandId::WorkspaceNode,
        BuiltinPreset::Color => CommandId::WorkspaceColor,
        BuiltinPreset::Motion => CommandId::WorkspaceMotion,
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
/// application name and the workspace preset switcher leading.
pub fn render_title_bar(
    workspace: &RavelWorkspace,
    cx: &mut Context<RavelWorkspace>,
) -> impl IntoElement {
    let active = workspace.shell().presets().active_builtin();
    let project_name = project_display_name(workspace.project().read(cx).project_path());

    RavelTitleBar::new(project_name)
        .leading(
            div()
                .text_sm()
                .text_color(cx.theme().colors.foreground)
                .child(t!("app.title")),
        )
        .leading(
            h_flex()
                .id("workspace-switcher")
                .h_full()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().colors.muted_foreground)
                        .child(t!("menu.workspace")),
                )
                .child(
                    ButtonGroup::new("workspace-presets")
                        .compact()
                        .outline()
                        .children(BuiltinPreset::ALL.map(|preset| {
                            let command = preset_command(preset);
                            Button::new(preset.label_key())
                                .small()
                                .ghost()
                                .selected(active == Some(preset))
                                .label(t!(preset.label_key()))
                                .on_click(cx.listener(
                                    move |this: &mut RavelWorkspace, _event, window, cx| {
                                        this.dispatch_command(command, window, cx);
                                    },
                                ))
                        })),
                ),
        )
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
