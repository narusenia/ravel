// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel custom window title bar.
//!
//! Replaces the default OS title bar with a [`gpui_component::TitleBar`].
//! The bar shows the application name and a segmented workspace preset switcher;
//! platform window controls are rendered automatically.

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

/// Renders Ravel's custom window title bar for the given workspace.
pub fn render_title_bar(
    workspace: &RavelWorkspace,
    cx: &mut Context<RavelWorkspace>,
) -> impl IntoElement {
    let active = workspace.shell().presets().active_builtin();
    let project_name = project_display_name(workspace.project().read(cx).project_path());

    TitleBar::new().child(
        h_flex()
            .id("title-bar-content")
            .relative()
            .flex_1()
            .h_full()
            .items_center()
            .gap_3()
            // Centered, subdued project name. A plain overlay with no
            // listeners: it neither captures clicks nor blocks the
            // platform drag region.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().colors.muted_foreground)
                            .child(project_name),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().colors.foreground)
                    .child(t!("app.title")),
            )
            .child(
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
