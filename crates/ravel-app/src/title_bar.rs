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
use gpui_component::{ActiveTheme, StyledExt as _, TitleBar, h_flex};
use ravel_i18n::t;
use ravel_ui::command::CommandId;
use ravel_ui::preset::BuiltinPreset;

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

/// Renders Ravel's custom window title bar for the given workspace.
pub fn render_title_bar(
    workspace: &RavelWorkspace,
    cx: &mut Context<RavelWorkspace>,
) -> impl IntoElement {
    let active = workspace.shell().presets().active_builtin();

    TitleBar::new().child(
        h_flex()
            .id("title-bar-left")
            .h_full()
            .items_center()
            .gap_3()
            .child(
                div()
                    .font_semibold()
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
