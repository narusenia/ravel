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
    FileImport,
    /// File ▸ Export…: the render dialog that submits a job to the session's
    /// render queue (`render-export-plan.md`, unit 5).
    FileExport,
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
    /// Edit ▸ Preferences…: the app-wide settings dialog, which writes the
    /// `global` settings layer (REQ-PROJ-004).
    AppPreferences,
    // Keyframe interpolation — handled by the focused Timeline graph.
    KeyframeInterpolationBezier,
    KeyframeInterpolationLinear,
    KeyframeInterpolationStep,
    // Timeline property reveal (After Effects' `U` / `P` / `S` / `R` / `T` /
    // `A` / `L`, plus `UU` and `EE` as `Alt+U` / `Alt+E`). Each criterion has
    // a second command for its `Shift` chord, which adds to the current
    // filter where the unmodified one replaces it — a GPUI action carries no
    // modifiers, so the two meanings are two commands.
    TimelineRevealAnimated,
    TimelineRevealAnimatedAdd,
    TimelineRevealAnchorPoint,
    TimelineRevealAnchorPointAdd,
    TimelineRevealPosition,
    TimelineRevealPositionAdd,
    TimelineRevealScale,
    TimelineRevealScaleAdd,
    TimelineRevealRotation,
    TimelineRevealRotationAdd,
    TimelineRevealOpacity,
    TimelineRevealOpacityAdd,
    TimelineRevealAudioGain,
    TimelineRevealAudioGainAdd,
    TimelineRevealModified,
    TimelineRevealModifiedAdd,
    TimelineRevealExpression,
    TimelineRevealExpressionAdd,
    // Timeline playhead / layer timing (After Effects' `Cmd+Shift+D`, `[`,
    // `]`, `I` and `O`). All five read the Timeline's layer selection and the
    // playhead, so they are Timeline-context bindings rather than global ones.
    /// Cut every selected layer in two at the playhead, as one undo step.
    TimelineSplitLayer,
    /// Move every selected layer so it *starts* at the playhead.
    TimelineAlignLayerStart,
    /// Move every selected layer so it *ends* at the playhead.
    TimelineAlignLayerEnd,
    /// Put the playhead on the earliest selected layer's first frame.
    TimelineGoToLayerIn,
    /// Put the playhead on the last selected layer's end (exclusive) frame.
    TimelineGoToLayerOut,
    // View (panel toggles)
    ViewToggleOutliner,
    ViewToggleTimeline,
    ViewToggleNodeGraph,
    ViewToggleViewer,
    ViewToggleDopesheet,
    ViewToggleProperties,
    ViewToggleCurveEditor,
    ViewToggleScopes,
    ViewToggleMediaBin,
    ViewToggleTextEditor,
    ViewToggleShaderEditor,
    ViewToggleLuaConsole,
    ViewToggleRenderQueue,
    ViewToggleAttributeSpreadsheet,
    /// Show or hide the parameter name/value rows the Node Graph editor
    /// draws inside the node bodies (`parameter-groups-plan.md`, PGRP-5).
    /// Unlike its neighbours this is not a panel toggle — the panel stays,
    /// its nodes get shorter.
    ViewToggleNodeParamValues,
    /// Step the viewer's preview resolution factor to the next one
    /// (`done/viewer-preview-resolution-plan.md`, REQ-UI-004). One cycling command
    /// rather than three "set to X" commands, because the factors are one
    /// ordered axis.
    ViewCyclePreviewResolution,
    /// Show one channel of the composite on its own (`INSP-2`, REQ-UI-004):
    /// the whole composite, R, G, B, or alpha.
    ///
    /// Five "set to X" commands rather than one cycling command, unlike
    /// [`CommandId::ViewCyclePreviewResolution`] next door. The preview
    /// factors are one ordered axis where the next step is always meaningful;
    /// the channels are not an axis, and cycling would make "show me green"
    /// cost between one and five presses depending on where the user already
    /// is. The alpha *matte* view has no command — it is a variant of the
    /// alpha view for judging a matte over black, and the toolbar menu is
    /// where that choice belongs.
    ViewerChannelRgb,
    ViewerChannelRed,
    ViewerChannelGreen,
    ViewerChannelBlue,
    ViewerChannelAlpha,
    ViewFit,
    // Playback
    PlaybackToggle,
    PlaybackStop,
    FrameStepForward,
    FrameStepBackward,
    /// Put the loop range's in point at the playhead.
    PlaybackLoopIn,
    /// Put the loop range's out point at the playhead.
    PlaybackLoopOut,
    /// Play straight through again.
    PlaybackLoopClear,
    // Composition management (REQ-UI-013)
    CompositionNew,
    CompositionSettings,
    CompositionDuplicate,
    CompositionDelete,
    /// Composition ▸ Project Settings…: the settings dialog that writes the
    /// `project` layer (REQ-PROJ-004). It is not composition management — it
    /// only shares the menu with the composition-level settings above.
    ProjectSettings,
    /// Composition ▸ Exposed Parameters: point the Properties panel at the
    /// project's exposed parameter declarations (REQ-PROJ-006). The
    /// declarations are the project's external contract, so they have no
    /// composition, layer or node to be selected through — this command is the
    /// only way to reach them.
    ProjectExposedParameters,
    // Layer creation (templates, REQ-LAYER-008)
    LayerAddSolid,
    LayerAddShape,
    LayerAddVideo,
    LayerAddAudio,
    LayerAddNull,
    // Workspace presets
    WorkspaceEdit,
    WorkspaceNode,
    WorkspaceColor,
    WorkspaceMotion,
    /// Save, apply, or forget a named layout (REQ-UI-005), and toggle whether
    /// saved projects embed the current one.
    WorkspaceManageLayouts,
    // Tool selection (REQ-UI-011)
    ToolSelect,
    ToolPen,
    ToolRect,
    ToolEllipse,
    ToolHand,
    ToolZoom,
    // Node editor — opens the search palette in the focused editor
    NodeSearchPalette,
    /// Move the selected nodes into a new subnet node (REQ-LAYER-003).
    NodeCollapseToSubnet,
    /// Move a selected subnet node's contents back into the open network.
    NodeExtractSubnet,
    /// Re-position the selected nodes into layers (`NGR-2`).
    ///
    /// Fewer than two selected nodes lays out the whole network — a single
    /// node has nothing to be arranged against, so there is no meaningful
    /// one-node layout to lose. That is what makes the shortcut useful right
    /// after a collapse, which leaves the new subnet node selected.
    ///
    /// Never runs by itself: node positions are saved data, so only the user
    /// decides when they move.
    NodeAutoLayout,
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
    (CommandId::FileImport, "file.import"),
    (CommandId::FileExport, "file.export"),
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
    (CommandId::AppPreferences, "app.preferences"),
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
    (
        CommandId::TimelineRevealAnimated,
        "timeline.reveal_animated",
    ),
    (
        CommandId::TimelineRevealAnimatedAdd,
        "timeline.reveal_animated_add",
    ),
    (
        CommandId::TimelineRevealAnchorPoint,
        "timeline.reveal_anchor_point",
    ),
    (
        CommandId::TimelineRevealAnchorPointAdd,
        "timeline.reveal_anchor_point_add",
    ),
    (
        CommandId::TimelineRevealPosition,
        "timeline.reveal_position",
    ),
    (
        CommandId::TimelineRevealPositionAdd,
        "timeline.reveal_position_add",
    ),
    (CommandId::TimelineRevealScale, "timeline.reveal_scale"),
    (
        CommandId::TimelineRevealScaleAdd,
        "timeline.reveal_scale_add",
    ),
    (
        CommandId::TimelineRevealRotation,
        "timeline.reveal_rotation",
    ),
    (
        CommandId::TimelineRevealRotationAdd,
        "timeline.reveal_rotation_add",
    ),
    (CommandId::TimelineRevealOpacity, "timeline.reveal_opacity"),
    (
        CommandId::TimelineRevealOpacityAdd,
        "timeline.reveal_opacity_add",
    ),
    (
        CommandId::TimelineRevealAudioGain,
        "timeline.reveal_audio_gain",
    ),
    (
        CommandId::TimelineRevealAudioGainAdd,
        "timeline.reveal_audio_gain_add",
    ),
    (
        CommandId::TimelineRevealModified,
        "timeline.reveal_modified",
    ),
    (
        CommandId::TimelineRevealModifiedAdd,
        "timeline.reveal_modified_add",
    ),
    (
        CommandId::TimelineRevealExpression,
        "timeline.reveal_expression",
    ),
    (
        CommandId::TimelineRevealExpressionAdd,
        "timeline.reveal_expression_add",
    ),
    (CommandId::TimelineSplitLayer, "timeline.split_layer"),
    (
        CommandId::TimelineAlignLayerStart,
        "timeline.align_layer_start",
    ),
    (CommandId::TimelineAlignLayerEnd, "timeline.align_layer_end"),
    (CommandId::TimelineGoToLayerIn, "timeline.go_to_layer_in"),
    (CommandId::TimelineGoToLayerOut, "timeline.go_to_layer_out"),
    (CommandId::ViewToggleOutliner, "view.toggle_outliner"),
    (CommandId::ViewToggleTimeline, "view.toggle_timeline"),
    (CommandId::ViewToggleNodeGraph, "view.toggle_node_graph"),
    (CommandId::ViewToggleViewer, "view.toggle_viewer"),
    (CommandId::ViewToggleDopesheet, "view.toggle_dopesheet"),
    (CommandId::ViewToggleProperties, "view.toggle_properties"),
    (CommandId::ViewToggleCurveEditor, "view.toggle_curve_editor"),
    (CommandId::ViewToggleScopes, "view.toggle_scopes"),
    (CommandId::ViewToggleMediaBin, "view.toggle_media_bin"),
    (CommandId::ViewToggleTextEditor, "view.toggle_text_editor"),
    (
        CommandId::ViewToggleShaderEditor,
        "view.toggle_shader_editor",
    ),
    (CommandId::ViewToggleLuaConsole, "view.toggle_lua_console"),
    (CommandId::ViewToggleRenderQueue, "view.toggle_render_queue"),
    (
        CommandId::ViewToggleAttributeSpreadsheet,
        "view.toggle_attribute_spreadsheet",
    ),
    (
        CommandId::ViewToggleNodeParamValues,
        "view.toggle_node_param_values",
    ),
    (
        CommandId::ViewCyclePreviewResolution,
        "view.cycle_preview_resolution",
    ),
    (CommandId::ViewerChannelRgb, "viewer.channel_rgb"),
    (CommandId::ViewerChannelRed, "viewer.channel_red"),
    (CommandId::ViewerChannelGreen, "viewer.channel_green"),
    (CommandId::ViewerChannelBlue, "viewer.channel_blue"),
    (CommandId::ViewerChannelAlpha, "viewer.channel_alpha"),
    (CommandId::ViewFit, "view.fit"),
    (CommandId::PlaybackToggle, "playback.toggle"),
    (CommandId::PlaybackStop, "playback.stop"),
    (CommandId::FrameStepForward, "playback.step_forward"),
    (CommandId::FrameStepBackward, "playback.step_backward"),
    (CommandId::PlaybackLoopIn, "playback.loop_in"),
    (CommandId::PlaybackLoopOut, "playback.loop_out"),
    (CommandId::PlaybackLoopClear, "playback.loop_clear"),
    (CommandId::CompositionNew, "composition.new"),
    (CommandId::CompositionSettings, "composition.settings"),
    (CommandId::CompositionDuplicate, "composition.duplicate"),
    (CommandId::CompositionDelete, "composition.delete"),
    (CommandId::ProjectSettings, "project.settings"),
    (
        CommandId::ProjectExposedParameters,
        "project.exposed_parameters",
    ),
    (CommandId::LayerAddSolid, "layer.add_solid"),
    (CommandId::LayerAddShape, "layer.add_shape"),
    (CommandId::LayerAddVideo, "layer.add_video"),
    (CommandId::LayerAddAudio, "layer.add_audio"),
    (CommandId::LayerAddNull, "layer.add_null"),
    (CommandId::WorkspaceEdit, "workspace.edit"),
    (CommandId::WorkspaceNode, "workspace.node"),
    (CommandId::WorkspaceColor, "workspace.color"),
    (CommandId::WorkspaceMotion, "workspace.motion"),
    (
        CommandId::WorkspaceManageLayouts,
        "workspace.manage_layouts",
    ),
    (CommandId::ToolSelect, "tool.select"),
    (CommandId::ToolPen, "tool.pen"),
    (CommandId::ToolRect, "tool.rect"),
    (CommandId::ToolEllipse, "tool.ellipse"),
    (CommandId::ToolHand, "tool.hand"),
    (CommandId::ToolZoom, "tool.zoom"),
    (CommandId::NodeSearchPalette, "node.search_palette"),
    (CommandId::NodeCollapseToSubnet, "node.collapse_to_subnet"),
    (CommandId::NodeExtractSubnet, "node.extract_subnet"),
    (CommandId::NodeAutoLayout, "node.auto_layout"),
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
            CommandId::FileImport => "menu.file.import",
            CommandId::FileExport => "menu.file.export",
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
            CommandId::AppPreferences => "menu.edit.preferences",
            CommandId::KeyframeInterpolationBezier => "timeline.interpolation.bezier",
            CommandId::KeyframeInterpolationLinear => "timeline.interpolation.linear",
            CommandId::KeyframeInterpolationStep => "timeline.interpolation.step",
            CommandId::TimelineRevealAnimated => "timeline.reveal.animated",
            CommandId::TimelineRevealAnimatedAdd => "timeline.reveal.animated_add",
            CommandId::TimelineRevealAnchorPoint => "timeline.reveal.anchor_point",
            CommandId::TimelineRevealAnchorPointAdd => "timeline.reveal.anchor_point_add",
            CommandId::TimelineRevealPosition => "timeline.reveal.position",
            CommandId::TimelineRevealPositionAdd => "timeline.reveal.position_add",
            CommandId::TimelineRevealScale => "timeline.reveal.scale",
            CommandId::TimelineRevealScaleAdd => "timeline.reveal.scale_add",
            CommandId::TimelineRevealRotation => "timeline.reveal.rotation",
            CommandId::TimelineRevealRotationAdd => "timeline.reveal.rotation_add",
            CommandId::TimelineRevealOpacity => "timeline.reveal.opacity",
            CommandId::TimelineRevealOpacityAdd => "timeline.reveal.opacity_add",
            CommandId::TimelineRevealAudioGain => "timeline.reveal.audio_gain",
            CommandId::TimelineRevealAudioGainAdd => "timeline.reveal.audio_gain_add",
            CommandId::TimelineRevealModified => "timeline.reveal.modified",
            CommandId::TimelineRevealModifiedAdd => "timeline.reveal.modified_add",
            CommandId::TimelineRevealExpression => "timeline.reveal.expression",
            CommandId::TimelineRevealExpressionAdd => "timeline.reveal.expression_add",
            CommandId::TimelineSplitLayer => "timeline.layer.split",
            CommandId::TimelineAlignLayerStart => "timeline.layer.align_start",
            CommandId::TimelineAlignLayerEnd => "timeline.layer.align_end",
            CommandId::TimelineGoToLayerIn => "timeline.layer.go_to_in",
            CommandId::TimelineGoToLayerOut => "timeline.layer.go_to_out",
            CommandId::ViewToggleOutliner => "menu.view.outliner",
            CommandId::ViewToggleTimeline => "menu.view.timeline",
            CommandId::ViewToggleNodeGraph => "menu.view.node_graph",
            CommandId::ViewToggleViewer => "menu.view.viewer",
            CommandId::ViewToggleDopesheet => "menu.view.dopesheet",
            CommandId::ViewToggleProperties => "menu.view.properties",
            CommandId::ViewToggleCurveEditor => "menu.view.curve_editor",
            CommandId::ViewToggleScopes => "menu.view.scopes",
            CommandId::ViewToggleMediaBin => "menu.view.media_bin",
            CommandId::ViewToggleTextEditor => "menu.view.text_editor",
            CommandId::ViewToggleShaderEditor => "menu.view.shader_editor",
            CommandId::ViewToggleLuaConsole => "menu.view.lua_console",
            CommandId::ViewToggleRenderQueue => "menu.view.render_queue",
            CommandId::ViewToggleAttributeSpreadsheet => "menu.view.attribute_spreadsheet",
            CommandId::ViewToggleNodeParamValues => "menu.view.node_param_values",
            CommandId::ViewCyclePreviewResolution => "menu.view.cycle_preview_resolution",
            CommandId::ViewerChannelRgb => "menu.view.channel_rgb",
            CommandId::ViewerChannelRed => "menu.view.channel_red",
            CommandId::ViewerChannelGreen => "menu.view.channel_green",
            CommandId::ViewerChannelBlue => "menu.view.channel_blue",
            CommandId::ViewerChannelAlpha => "menu.view.channel_alpha",
            CommandId::ViewFit => "menu.view.fit",
            CommandId::PlaybackToggle => "menu.playback.toggle",
            CommandId::PlaybackStop => "menu.playback.stop",
            CommandId::FrameStepForward => "menu.playback.step_forward",
            CommandId::FrameStepBackward => "menu.playback.step_backward",
            CommandId::PlaybackLoopIn => "menu.playback.loop_in",
            CommandId::PlaybackLoopOut => "menu.playback.loop_out",
            CommandId::PlaybackLoopClear => "menu.playback.loop_clear",
            CommandId::CompositionNew => "menu.composition.new",
            CommandId::CompositionSettings => "menu.composition.settings",
            CommandId::CompositionDuplicate => "menu.composition.duplicate",
            CommandId::CompositionDelete => "menu.composition.delete",
            CommandId::ProjectSettings => "menu.composition.project_settings",
            CommandId::ProjectExposedParameters => "menu.composition.project_exposed_parameters",
            CommandId::LayerAddSolid => "menu.layer.add_solid",
            CommandId::LayerAddShape => "menu.layer.add_shape",
            CommandId::LayerAddVideo => "menu.layer.add_video",
            CommandId::LayerAddAudio => "menu.layer.add_audio",
            CommandId::LayerAddNull => "menu.layer.add_null",
            CommandId::WorkspaceEdit => "menu.workspace.edit",
            CommandId::WorkspaceNode => "menu.workspace.node",
            CommandId::WorkspaceColor => "menu.workspace.color",
            CommandId::WorkspaceMotion => "menu.workspace.motion",
            CommandId::WorkspaceManageLayouts => "menu.workspace.manage_layouts",
            CommandId::ToolSelect => "menu.tool.select",
            CommandId::ToolPen => "menu.tool.pen",
            CommandId::ToolRect => "menu.tool.rect",
            CommandId::ToolEllipse => "menu.tool.ellipse",
            CommandId::ToolHand => "menu.tool.hand",
            CommandId::ToolZoom => "menu.tool.zoom",
            CommandId::NodeSearchPalette => "menu.node.search_palette",
            CommandId::NodeCollapseToSubnet => "menu.node.collapse_to_subnet",
            CommandId::NodeExtractSubnet => "menu.node.extract_subnet",
            CommandId::NodeAutoLayout => "menu.node.auto_layout",
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
            CommandId::LayerAddVideo => Some("media"),
            CommandId::LayerAddAudio => Some("audio"),
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
