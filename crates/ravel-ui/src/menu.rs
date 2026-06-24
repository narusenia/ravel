// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Menu bar model.
//!
//! The menu bar is described declaratively as data so the same definition
//! drives the native macOS menu bar and the in-window menu used on
//! Windows/Linux. Each actionable item references a [`CommandId`]; rendering
//! resolves command [`label_key`](CommandId::label_key)s and shortcut hints
//! through the host's `t!` macro and active [`KeyBindings`].
//!
//! [`KeyBindings`]: crate::keybindings::KeyBindings

use crate::command::CommandId;
use crate::panel::{PanelKind, PanelVisibility};
use crate::preset::BuiltinPreset;

/// A single entry in a menu.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuItem {
    /// An actionable command entry.
    Action {
        /// The command invoked when selected.
        command: CommandId,
        /// `Some(checked)` if the entry shows a checkbox (e.g. panel toggles).
        check: Option<bool>,
    },
    /// A horizontal separator.
    Separator,
    /// A nested submenu.
    Submenu(Menu),
}

impl MenuItem {
    /// Builds a plain action item.
    pub fn action(command: CommandId) -> Self {
        MenuItem::Action {
            command,
            check: None,
        }
    }

    /// Builds a checkable action item.
    pub fn check(command: CommandId, checked: bool) -> Self {
        MenuItem::Action {
            command,
            check: Some(checked),
        }
    }
}

/// A named menu (top-level or submenu).
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    /// i18n label key for the menu title.
    pub label_key: &'static str,
    /// Ordered entries.
    pub items: Vec<MenuItem>,
}

impl Menu {
    /// Creates a menu from a title key and entries.
    pub fn new(label_key: &'static str, items: Vec<MenuItem>) -> Self {
        Self { label_key, items }
    }

    /// Collects every command reachable from this menu (recursively).
    pub fn commands(&self) -> Vec<CommandId> {
        let mut out = Vec::new();
        collect_commands(&self.items, &mut out);
        out
    }
}

fn collect_commands(items: &[MenuItem], out: &mut Vec<CommandId>) {
    for item in items {
        match item {
            MenuItem::Action { command, .. } => out.push(*command),
            MenuItem::Submenu(menu) => collect_commands(&menu.items, out),
            MenuItem::Separator => {}
        }
    }
}

/// The application menu bar: an ordered list of top-level menus.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuBar {
    /// Top-level menus, left to right.
    pub menus: Vec<Menu>,
}

impl MenuBar {
    /// Builds the default menu bar.
    ///
    /// View-menu panel toggles reflect `visibility`; the Workspace menu marks
    /// `active_preset` as checked.
    pub fn build(visibility: &PanelVisibility, active_preset: Option<BuiltinPreset>) -> Self {
        let file = Menu::new(
            "menu.file",
            vec![
                MenuItem::action(CommandId::FileNew),
                MenuItem::action(CommandId::FileOpen),
                MenuItem::Separator,
                MenuItem::action(CommandId::FileSave),
                MenuItem::action(CommandId::FileSaveAs),
                MenuItem::Separator,
                MenuItem::action(CommandId::FileQuit),
            ],
        );

        let edit = Menu::new(
            "menu.edit",
            vec![
                MenuItem::action(CommandId::EditUndo),
                MenuItem::action(CommandId::EditRedo),
                MenuItem::Separator,
                MenuItem::action(CommandId::EditCut),
                MenuItem::action(CommandId::EditCopy),
                MenuItem::action(CommandId::EditPaste),
            ],
        );

        let view = Menu::new(
            "menu.view",
            vec![
                MenuItem::check(
                    CommandId::ViewToggleOutliner,
                    visibility.is_visible(PanelKind::Outliner),
                ),
                MenuItem::check(
                    CommandId::ViewToggleTimeline,
                    visibility.is_visible(PanelKind::Timeline),
                ),
                MenuItem::check(
                    CommandId::ViewToggleNodeGraph,
                    visibility.is_visible(PanelKind::NodeGraph),
                ),
                MenuItem::check(
                    CommandId::ViewToggleViewer,
                    visibility.is_visible(PanelKind::Viewer),
                ),
                MenuItem::check(
                    CommandId::ViewToggleDopesheet,
                    visibility.is_visible(PanelKind::Dopesheet),
                ),
                MenuItem::check(
                    CommandId::ViewToggleProperties,
                    visibility.is_visible(PanelKind::Properties),
                ),
                MenuItem::check(
                    CommandId::ViewToggleCurveEditor,
                    visibility.is_visible(PanelKind::CurveEditor),
                ),
                MenuItem::check(
                    CommandId::ViewToggleScopes,
                    visibility.is_visible(PanelKind::Waveform),
                ),
            ],
        );

        let workspace = Menu::new(
            "menu.workspace",
            vec![
                MenuItem::check(
                    CommandId::WorkspaceEdit,
                    active_preset == Some(BuiltinPreset::Edit),
                ),
                MenuItem::check(
                    CommandId::WorkspaceNode,
                    active_preset == Some(BuiltinPreset::Node),
                ),
                MenuItem::check(
                    CommandId::WorkspaceColor,
                    active_preset == Some(BuiltinPreset::Color),
                ),
                MenuItem::check(
                    CommandId::WorkspaceMotion,
                    active_preset == Some(BuiltinPreset::Motion),
                ),
            ],
        );

        let help = Menu::new("menu.help", vec![MenuItem::action(CommandId::HelpAbout)]);

        MenuBar {
            menus: vec![file, edit, view, workspace, help],
        }
    }

    /// Finds a top-level menu by its label key.
    pub fn menu(&self, label_key: &str) -> Option<&Menu> {
        self.menus.iter().find(|m| m.label_key == label_key)
    }

    /// Collects every command reachable from the whole bar.
    pub fn commands(&self) -> Vec<CommandId> {
        self.menus.iter().flat_map(Menu::commands).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bar_has_expected_top_level_menus() {
        let bar = MenuBar::build(&PanelVisibility::new(), Some(BuiltinPreset::Edit));
        let titles: Vec<_> = bar.menus.iter().map(|m| m.label_key).collect();
        assert_eq!(
            titles,
            vec![
                "menu.file",
                "menu.edit",
                "menu.view",
                "menu.workspace",
                "menu.help"
            ]
        );
    }

    #[test]
    fn workspace_menu_checks_active_preset() {
        let bar = MenuBar::build(&PanelVisibility::new(), Some(BuiltinPreset::Color));
        let ws = bar.menu("menu.workspace").unwrap();
        let checked: Vec<_> = ws
            .items
            .iter()
            .filter_map(|i| match i {
                MenuItem::Action {
                    command,
                    check: Some(true),
                } => Some(*command),
                _ => None,
            })
            .collect();
        assert_eq!(checked, vec![CommandId::WorkspaceColor]);
    }

    #[test]
    fn view_menu_reflects_visibility() {
        let vis = PanelVisibility::with_visible([PanelKind::Timeline]);
        let bar = MenuBar::build(&vis, None);
        let view = bar.menu("menu.view").unwrap();
        let timeline_checked = view.items.iter().any(|i| {
            matches!(
                i,
                MenuItem::Action {
                    command: CommandId::ViewToggleTimeline,
                    check: Some(true),
                }
            )
        });
        assert!(timeline_checked);
    }

    #[test]
    fn every_menu_command_is_known() {
        let bar = MenuBar::build(&PanelVisibility::new(), None);
        for cmd in bar.commands() {
            // Round-trips through the command table.
            assert_eq!(cmd.as_str().parse::<CommandId>().unwrap(), cmd);
        }
    }
}
