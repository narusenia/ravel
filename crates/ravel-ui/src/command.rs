// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Command identifiers shared by the menu bar, keybinding system, and the
//! (future) GPUI action dispatch layer.
//!
//! Every user-triggerable operation in the shell is named by a stable
//! [`CommandId`]. Menus reference commands, keybindings resolve key chords to
//! commands, and the command registry (host side) maps a command to an action.
//! Keeping the identifier set in one place lets the keybinding parser and the
//! menu builder share a single source of truth.

use std::fmt;
use std::str::FromStr;

/// A stable identifier for a shell command.
///
/// The canonical string form is a dotted `section.action` name (for example
/// `global.undo`). The string form is what appears in keybinding definition
/// files, so it is part of the public configuration contract and must remain
/// stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandId {
    // File
    FileNew,
    FileOpen,
    FileSave,
    FileSaveAs,
    FileQuit,
    // Edit
    EditUndo,
    EditRedo,
    EditCut,
    EditCopy,
    EditPaste,
    // View (panel toggles)
    ViewToggleTimeline,
    ViewToggleNodeGraph,
    ViewToggleViewer,
    ViewToggleProperties,
    ViewToggleCurveEditor,
    ViewToggleScopes,
    // Workspace presets
    WorkspaceEdit,
    WorkspaceNode,
    WorkspaceColor,
    WorkspaceMotion,
    // Panel window management
    PanelDetach,
    PanelReattach,
    // Help
    HelpAbout,
}

/// All commands in declaration order, paired with their canonical string id.
///
/// This is the single table consulted by [`CommandId::as_str`] and
/// [`CommandId::from_str`]; adding a command here wires it into both directions
/// and into [`CommandId::all`].
const COMMAND_TABLE: &[(CommandId, &str)] = &[
    (CommandId::FileNew, "file.new"),
    (CommandId::FileOpen, "file.open"),
    (CommandId::FileSave, "file.save"),
    (CommandId::FileSaveAs, "file.save_as"),
    (CommandId::FileQuit, "file.quit"),
    (CommandId::EditUndo, "edit.undo"),
    (CommandId::EditRedo, "edit.redo"),
    (CommandId::EditCut, "edit.cut"),
    (CommandId::EditCopy, "edit.copy"),
    (CommandId::EditPaste, "edit.paste"),
    (CommandId::ViewToggleTimeline, "view.toggle_timeline"),
    (CommandId::ViewToggleNodeGraph, "view.toggle_node_graph"),
    (CommandId::ViewToggleViewer, "view.toggle_viewer"),
    (CommandId::ViewToggleProperties, "view.toggle_properties"),
    (CommandId::ViewToggleCurveEditor, "view.toggle_curve_editor"),
    (CommandId::ViewToggleScopes, "view.toggle_scopes"),
    (CommandId::WorkspaceEdit, "workspace.edit"),
    (CommandId::WorkspaceNode, "workspace.node"),
    (CommandId::WorkspaceColor, "workspace.color"),
    (CommandId::WorkspaceMotion, "workspace.motion"),
    (CommandId::PanelDetach, "panel.detach"),
    (CommandId::PanelReattach, "panel.reattach"),
    (CommandId::HelpAbout, "help.about"),
];

impl CommandId {
    /// Returns the canonical dotted string identifier.
    pub fn as_str(self) -> &'static str {
        COMMAND_TABLE
            .iter()
            .find_map(|(cmd, name)| (*cmd == self).then_some(*name))
            .expect("every CommandId variant is present in COMMAND_TABLE")
    }

    /// Returns the i18n label key used to render this command in menus.
    ///
    /// UI text is never hardcoded; the host resolves this key through the
    /// `t!` macro at render time.
    pub fn label_key(self) -> &'static str {
        match self {
            CommandId::FileNew => "menu.file.new",
            CommandId::FileOpen => "menu.file.open",
            CommandId::FileSave => "menu.file.save",
            CommandId::FileSaveAs => "menu.file.save_as",
            CommandId::FileQuit => "menu.file.quit",
            CommandId::EditUndo => "menu.edit.undo",
            CommandId::EditRedo => "menu.edit.redo",
            CommandId::EditCut => "menu.edit.cut",
            CommandId::EditCopy => "menu.edit.copy",
            CommandId::EditPaste => "menu.edit.paste",
            CommandId::ViewToggleTimeline => "menu.view.timeline",
            CommandId::ViewToggleNodeGraph => "menu.view.node_graph",
            CommandId::ViewToggleViewer => "menu.view.viewer",
            CommandId::ViewToggleProperties => "menu.view.properties",
            CommandId::ViewToggleCurveEditor => "menu.view.curve_editor",
            CommandId::ViewToggleScopes => "menu.view.scopes",
            CommandId::WorkspaceEdit => "menu.workspace.edit",
            CommandId::WorkspaceNode => "menu.workspace.node",
            CommandId::WorkspaceColor => "menu.workspace.color",
            CommandId::WorkspaceMotion => "menu.workspace.motion",
            CommandId::PanelDetach => "menu.panel.detach",
            CommandId::PanelReattach => "menu.panel.reattach",
            CommandId::HelpAbout => "menu.help.about",
        }
    }

    /// Iterates over every known command.
    pub fn all() -> impl Iterator<Item = CommandId> {
        COMMAND_TABLE.iter().map(|(cmd, _)| *cmd)
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when a string does not name a known command.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown command id: {0}")]
pub struct UnknownCommand(pub String);

impl FromStr for CommandId {
    type Err = UnknownCommand;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        COMMAND_TABLE
            .iter()
            .find_map(|(cmd, name)| (*name == s).then_some(*cmd))
            .ok_or_else(|| UnknownCommand(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_string_form() {
        for cmd in CommandId::all() {
            let parsed = CommandId::from_str(cmd.as_str()).unwrap();
            assert_eq!(cmd, parsed);
        }
    }

    #[test]
    fn table_has_no_duplicate_ids() {
        let mut seen = std::collections::HashSet::new();
        for (_, name) in COMMAND_TABLE {
            assert!(seen.insert(*name), "duplicate command id: {name}");
        }
    }

    #[test]
    fn unknown_command_is_rejected() {
        let err = CommandId::from_str("does.not.exist").unwrap_err();
        assert_eq!(err, UnknownCommand("does.not.exist".to_owned()));
    }

    #[test]
    fn every_command_has_distinct_label_key() {
        let mut seen = std::collections::HashSet::new();
        for cmd in CommandId::all() {
            assert!(
                seen.insert(cmd.label_key()),
                "duplicate label key for {cmd}"
            );
        }
    }
}
