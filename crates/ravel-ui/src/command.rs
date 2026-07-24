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
    // Edit — Copy/Paste/Delete/… are "send to the focused target" commands,
    // not global operations; the focused panel decides what they mean.
    EditUndo,
    EditRedo,
    EditCut,
    EditCopy,
    EditPaste,
    EditDelete,
    EditDuplicate,
    // Keyframe interpolation — handled by the focused Timeline graph.
    KeyframeInterpolationBezier,
    KeyframeInterpolationLinear,
    KeyframeInterpolationStep,
    // View (panel toggles)
    ViewToggleOutliner,
    ViewToggleTimeline,
    ViewToggleNodeGraph,
    ViewToggleViewer,
    ViewToggleDopesheet,
    ViewToggleProperties,
    ViewToggleCurveEditor,
    ViewToggleScopes,
    ViewFit,
    // Playback
    PlaybackToggle,
    PlaybackStop,
    FrameStepForward,
    FrameStepBackward,
    // Layer creation (templates, REQ-LAYER-008)
    LayerAddSolid,
    LayerAddShape,
    LayerAddVideo,
    LayerAddNull,
    // Workspace presets
    WorkspaceEdit,
    WorkspaceNode,
    WorkspaceColor,
    WorkspaceMotion,
    // Tool selection (REQ-UI-011)
    ToolSelect,
    ToolPen,
    ToolRect,
    ToolEllipse,
    ToolHand,
    ToolZoom,
    // Panel window management
    PanelDetach,
    PanelReattach,
    // Help
    HelpAbout,
}

/// The active canvas tool (REQ-UI-011).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ToolKind {
    #[default]
    Select,
    Pen,
    Rect,
    Ellipse,
    Hand,
    Zoom,
}

impl ToolKind {
    pub fn command_id(self) -> CommandId {
        match self {
            Self::Select => CommandId::ToolSelect,
            Self::Pen => CommandId::ToolPen,
            Self::Rect => CommandId::ToolRect,
            Self::Ellipse => CommandId::ToolEllipse,
            Self::Hand => CommandId::ToolHand,
            Self::Zoom => CommandId::ToolZoom,
        }
    }

    pub fn from_command(cmd: CommandId) -> Option<Self> {
        match cmd {
            CommandId::ToolSelect => Some(Self::Select),
            CommandId::ToolPen => Some(Self::Pen),
            CommandId::ToolRect => Some(Self::Rect),
            CommandId::ToolEllipse => Some(Self::Ellipse),
            CommandId::ToolHand => Some(Self::Hand),
            CommandId::ToolZoom => Some(Self::Zoom),
            _ => None,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            Self::Select => "tool.select",
            Self::Pen => "tool.pen",
            Self::Rect => "tool.rect",
            Self::Ellipse => "tool.ellipse",
            Self::Hand => "tool.hand",
            Self::Zoom => "tool.zoom",
        }
    }
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
    (CommandId::EditDelete, "edit.delete"),
    (CommandId::EditDuplicate, "edit.duplicate"),
    (
        CommandId::KeyframeInterpolationBezier,
        "keyframe.interpolation_bezier",
    ),
    (
        CommandId::KeyframeInterpolationLinear,
        "keyframe.interpolation_linear",
    ),
    (
        CommandId::KeyframeInterpolationStep,
        "keyframe.interpolation_step",
    ),
    (CommandId::ViewToggleOutliner, "view.toggle_outliner"),
    (CommandId::ViewToggleTimeline, "view.toggle_timeline"),
    (CommandId::ViewToggleNodeGraph, "view.toggle_node_graph"),
    (CommandId::ViewToggleViewer, "view.toggle_viewer"),
    (CommandId::ViewToggleDopesheet, "view.toggle_dopesheet"),
    (CommandId::ViewToggleProperties, "view.toggle_properties"),
    (CommandId::ViewToggleCurveEditor, "view.toggle_curve_editor"),
    (CommandId::ViewToggleScopes, "view.toggle_scopes"),
    (CommandId::ViewFit, "view.fit"),
    (CommandId::PlaybackToggle, "playback.toggle"),
    (CommandId::PlaybackStop, "playback.stop"),
    (CommandId::FrameStepForward, "playback.step_forward"),
    (CommandId::FrameStepBackward, "playback.step_backward"),
    (CommandId::LayerAddSolid, "layer.add_solid"),
    (CommandId::LayerAddShape, "layer.add_shape"),
    (CommandId::LayerAddVideo, "layer.add_video"),
    (CommandId::LayerAddNull, "layer.add_null"),
    (CommandId::WorkspaceEdit, "workspace.edit"),
    (CommandId::WorkspaceNode, "workspace.node"),
    (CommandId::WorkspaceColor, "workspace.color"),
    (CommandId::WorkspaceMotion, "workspace.motion"),
    (CommandId::ToolSelect, "tool.select"),
    (CommandId::ToolPen, "tool.pen"),
    (CommandId::ToolRect, "tool.rect"),
    (CommandId::ToolEllipse, "tool.ellipse"),
    (CommandId::ToolHand, "tool.hand"),
    (CommandId::ToolZoom, "tool.zoom"),
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
            CommandId::EditDelete => "menu.edit.delete",
            CommandId::EditDuplicate => "menu.edit.duplicate",
            CommandId::KeyframeInterpolationBezier => "timeline.interpolation.bezier",
            CommandId::KeyframeInterpolationLinear => "timeline.interpolation.linear",
            CommandId::KeyframeInterpolationStep => "timeline.interpolation.step",
            CommandId::ViewToggleOutliner => "menu.view.outliner",
            CommandId::ViewToggleTimeline => "menu.view.timeline",
            CommandId::ViewToggleNodeGraph => "menu.view.node_graph",
            CommandId::ViewToggleViewer => "menu.view.viewer",
            CommandId::ViewToggleDopesheet => "menu.view.dopesheet",
            CommandId::ViewToggleProperties => "menu.view.properties",
            CommandId::ViewToggleCurveEditor => "menu.view.curve_editor",
            CommandId::ViewToggleScopes => "menu.view.scopes",
            CommandId::ViewFit => "menu.view.fit",
            CommandId::PlaybackToggle => "menu.playback.toggle",
            CommandId::PlaybackStop => "menu.playback.stop",
            CommandId::FrameStepForward => "menu.playback.step_forward",
            CommandId::FrameStepBackward => "menu.playback.step_backward",
            CommandId::LayerAddSolid => "menu.layer.add_solid",
            CommandId::LayerAddShape => "menu.layer.add_shape",
            CommandId::LayerAddVideo => "menu.layer.add_video",
            CommandId::LayerAddNull => "menu.layer.add_null",
            CommandId::WorkspaceEdit => "menu.workspace.edit",
            CommandId::WorkspaceNode => "menu.workspace.node",
            CommandId::WorkspaceColor => "menu.workspace.color",
            CommandId::WorkspaceMotion => "menu.workspace.motion",
            CommandId::ToolSelect => "menu.tool.select",
            CommandId::ToolPen => "menu.tool.pen",
            CommandId::ToolRect => "menu.tool.rect",
            CommandId::ToolEllipse => "menu.tool.ellipse",
            CommandId::ToolHand => "menu.tool.hand",
            CommandId::ToolZoom => "menu.tool.zoom",
            CommandId::PanelDetach => "menu.panel.detach",
            CommandId::PanelReattach => "menu.panel.reattach",
            CommandId::HelpAbout => "menu.help.about",
        }
    }

    /// Iterates over every known command.
    pub fn all() -> impl Iterator<Item = CommandId> {
        COMMAND_TABLE.iter().map(|(cmd, _)| *cmd)
    }

    /// The layer-template key a `LayerAdd*` command instantiates
    /// (REQ-LAYER-008), `None` for every other command.
    ///
    /// Kept in one place so the host's dispatch and the test tying commands
    /// to `builtin_layer_templates()` share a single mapping.
    pub fn layer_template_key(self) -> Option<&'static str> {
        match self {
            CommandId::LayerAddSolid => Some("solid"),
            CommandId::LayerAddShape => Some("shape"),
            CommandId::LayerAddVideo => Some("video"),
            CommandId::LayerAddNull => Some("null"),
            _ => None,
        }
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

    /// Every builtin layer template is reachable through a creation command,
    /// and every creation command names an existing template — the commands
    /// are generated *from* the template set (REQ-LAYER-008).
    #[test]
    fn layer_commands_cover_builtin_templates() {
        let template_keys: Vec<&str> =
            ravel_core::composition::templates::builtin_layer_templates()
                .iter()
                .map(|t| t.key.as_str())
                .collect();
        let command_keys: Vec<&str> = CommandId::all()
            .filter_map(CommandId::layer_template_key)
            .collect();
        for key in &template_keys {
            assert!(
                command_keys.contains(key),
                "builtin template {key:?} has no LayerAdd command"
            );
        }
        for key in &command_keys {
            assert!(
                template_keys.contains(key),
                "LayerAdd command references unknown template {key:?}"
            );
        }
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
