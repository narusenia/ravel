// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Application shell state and command dispatch.
//!
//! [`AppShell`] is the headless heart of the GPUI application shell: it owns the
//! workspace preset library, panel visibility, keybindings, detached-window
//! bookkeeping, and the Properties inspector shell. The GPUI host wraps this
//! state, renders the menu bar and panels from it, and feeds it user input
//! (resolved key chords, menu selections). Keeping the logic here makes the
//! shell fully testable without a live window.

use crate::command::CommandId;
use crate::keybindings::{KeyBindings, KeyChord};
use crate::menu::MenuBar;
use crate::panel::{PanelKind, PanelVisibility};
use crate::panels::properties::PropertiesPanel;
use crate::preset::{BuiltinPreset, PresetLibrary};
use crate::window::{WindowId, WindowManager};

/// The scope panels toggled together by [`CommandId::ViewToggleScopes`].
const SCOPE_PANELS: [PanelKind; 4] = [
    PanelKind::Waveform,
    PanelKind::Vectorscope,
    PanelKind::Histogram,
    PanelKind::Parade,
];

/// Result of dispatching a command to the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// The shell handled the command and mutated its own state.
    Handled,
    /// The command must be handled by the host (file I/O, clipboard, dialogs).
    Delegate(CommandId),
    /// A panel was detached from the main window; the host should open a new
    /// OS window for it.
    DetachPanel {
        /// The panel that was detached.
        panel: PanelKind,
        /// The window id assigned by the [`WindowManager`].
        window_id: WindowId,
    },
    /// A panel was reattached to the main window; the host should close the
    /// detached OS window.
    ReattachPanel {
        /// The panel returning to the main window.
        panel: PanelKind,
        /// The window id that was released.
        window_id: WindowId,
    },
}

/// Aggregate, headless state for the application shell.
#[derive(Debug, Clone)]
pub struct AppShell {
    presets: PresetLibrary,
    keybindings: KeyBindings,
    windows: WindowManager,
    properties: PropertiesPanel,
    focused_panel: Option<PanelKind>,
}

impl AppShell {
    /// Builds a shell with the given initial preset and keybindings.
    pub fn new(initial: BuiltinPreset, keybindings: KeyBindings) -> Self {
        Self {
            presets: PresetLibrary::new(initial),
            keybindings,
            windows: WindowManager::new(),
            properties: PropertiesPanel::new(),
            focused_panel: None,
        }
    }

    /// The preset library (active layout, custom presets, visibility).
    pub fn presets(&self) -> &PresetLibrary {
        &self.presets
    }

    /// Mutable access to the preset library.
    pub fn presets_mut(&mut self) -> &mut PresetLibrary {
        &mut self.presets
    }

    /// Current panel visibility.
    pub fn visibility(&self) -> &PanelVisibility {
        self.presets.visibility()
    }

    /// The active keybindings.
    pub fn keybindings(&self) -> &KeyBindings {
        &self.keybindings
    }

    /// Replaces the active keybindings (e.g. after reloading the config file).
    pub fn set_keybindings(&mut self, keybindings: KeyBindings) {
        self.keybindings = keybindings;
    }

    /// Detached-window bookkeeping.
    pub fn windows(&self) -> &WindowManager {
        &self.windows
    }

    /// Mutable access to detached-window bookkeeping.
    pub fn windows_mut(&mut self) -> &mut WindowManager {
        &mut self.windows
    }

    /// The Properties inspector shell.
    pub fn properties(&self) -> &PropertiesPanel {
        &self.properties
    }

    /// Mutable access to the Properties inspector shell.
    pub fn properties_mut(&mut self) -> &mut PropertiesPanel {
        &mut self.properties
    }

    /// The currently focused panel in the main window, if any.
    pub fn focused_panel(&self) -> Option<PanelKind> {
        self.focused_panel
    }

    /// Updates which panel currently has focus (called by the host when panel
    /// focus changes in the DockArea or a detached window).
    pub fn set_focused_panel(&mut self, panel: Option<PanelKind>) {
        self.focused_panel = panel;
    }

    /// Directly reattaches a detached window, restoring panel visibility.
    ///
    /// This bypasses command dispatch and is meant for the host to call when a
    /// detached OS window is closed by the user (window close button or Cmd+W).
    /// Returns the reattached panel, or `None` if the window was already gone.
    pub fn reattach_window(&mut self, id: WindowId) -> Option<PanelKind> {
        match self.windows.reattach(id) {
            Ok(panel) => {
                self.presets.visibility_mut().set(panel, true);
                Some(panel)
            }
            Err(_) => None,
        }
    }

    /// Builds the current menu bar (checkboxes reflect live state).
    pub fn menu_bar(&self) -> MenuBar {
        MenuBar::build(self.presets.visibility(), self.presets.active_builtin())
    }

    /// Resolves a key chord to its command, then dispatches it.
    ///
    /// Returns `None` if the chord is unbound.
    pub fn handle_chord(&mut self, chord: &KeyChord) -> Option<CommandOutcome> {
        let command = self.keybindings.resolve(chord)?;
        Some(self.handle_command(command))
    }

    /// Dispatches a command, mutating shell state for commands the shell owns
    /// (panel toggles, preset switches, detach/reattach) and delegating the
    /// rest to the host.
    pub fn handle_command(&mut self, command: CommandId) -> CommandOutcome {
        match command {
            CommandId::ViewToggleOutliner => self.toggle(PanelKind::Outliner),
            CommandId::ViewToggleTimeline => self.toggle(PanelKind::Timeline),
            CommandId::ViewToggleNodeGraph => self.toggle(PanelKind::NodeGraph),
            CommandId::ViewToggleViewer => self.toggle(PanelKind::Viewer),
            CommandId::ViewToggleDopesheet => self.toggle(PanelKind::Dopesheet),
            CommandId::ViewToggleProperties => self.toggle(PanelKind::Properties),
            CommandId::ViewToggleCurveEditor => self.toggle(PanelKind::CurveEditor),
            CommandId::ViewToggleScopes => self.toggle_scopes(),
            CommandId::WorkspaceEdit => self.switch_preset(BuiltinPreset::Edit),
            CommandId::WorkspaceNode => self.switch_preset(BuiltinPreset::Node),
            CommandId::WorkspaceColor => self.switch_preset(BuiltinPreset::Color),
            CommandId::WorkspaceMotion => self.switch_preset(BuiltinPreset::Motion),
            CommandId::PanelDetach => self.handle_detach(),
            CommandId::PanelReattach => self.handle_reattach(),
            other => CommandOutcome::Delegate(other),
        }
    }

    fn toggle(&mut self, panel: PanelKind) -> CommandOutcome {
        self.presets.visibility_mut().toggle(panel);
        CommandOutcome::Handled
    }

    fn toggle_scopes(&mut self) -> CommandOutcome {
        // Drive all scopes from the Waveform state so they move together.
        let next = !self.presets.visibility().is_visible(PanelKind::Waveform);
        for panel in SCOPE_PANELS {
            self.presets.visibility_mut().set(panel, next);
        }
        CommandOutcome::Handled
    }

    fn switch_preset(&mut self, preset: BuiltinPreset) -> CommandOutcome {
        self.presets.switch_builtin(preset);
        CommandOutcome::Handled
    }

    /// Detaches the currently focused panel into a new window.
    ///
    /// Returns [`CommandOutcome::DetachPanel`] on success so the host can open
    /// the actual OS window, or [`CommandOutcome::Handled`] when there is no
    /// focused panel or the panel is already detached.
    fn handle_detach(&mut self) -> CommandOutcome {
        let Some(panel) = self.focused_panel else {
            return CommandOutcome::Handled;
        };
        if self.windows.is_detached(panel) {
            return CommandOutcome::Handled;
        }
        match self.windows.detach(panel) {
            Ok(window_id) => {
                self.presets.visibility_mut().set(panel, false);
                CommandOutcome::DetachPanel { panel, window_id }
            }
            Err(_) => CommandOutcome::Handled,
        }
    }

    /// Reattaches a detached panel back to the main window.
    ///
    /// Prefers the focused panel if it is detached; otherwise falls back to
    /// the most recently detached panel.
    fn handle_reattach(&mut self) -> CommandOutcome {
        let target_id = self
            .focused_panel
            .and_then(|p| self.windows.window_of(p))
            .or_else(|| self.windows.detached().last().map(|w| w.id));

        let Some(id) = target_id else {
            return CommandOutcome::Handled;
        };

        match self.windows.reattach(id) {
            Ok(panel) => {
                self.presets.visibility_mut().set(panel, true);
                CommandOutcome::ReattachPanel {
                    panel,
                    window_id: id,
                }
            }
            Err(_) => CommandOutcome::Handled,
        }
    }
}

impl Default for AppShell {
    fn default() -> Self {
        Self::new(
            BuiltinPreset::Edit,
            crate::keybindings::parser::default_bindings(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::parser::default_bindings;

    fn shell() -> AppShell {
        AppShell::new(BuiltinPreset::Edit, default_bindings())
    }

    #[test]
    fn view_toggle_command_flips_panel() {
        let mut s = shell();
        assert!(s.visibility().is_visible(PanelKind::Timeline));
        assert_eq!(
            s.handle_command(CommandId::ViewToggleTimeline),
            CommandOutcome::Handled
        );
        assert!(!s.visibility().is_visible(PanelKind::Timeline));
    }

    #[test]
    fn workspace_command_switches_preset() {
        let mut s = shell();
        assert_eq!(
            s.handle_command(CommandId::WorkspaceColor),
            CommandOutcome::Handled
        );
        assert_eq!(s.presets().active_builtin(), Some(BuiltinPreset::Color));
        assert!(s.visibility().is_visible(PanelKind::Waveform));
    }

    #[test]
    fn toggle_scopes_moves_all_scope_panels_together() {
        let mut s = shell();
        s.handle_command(CommandId::WorkspaceColor); // scopes on
        for p in SCOPE_PANELS {
            assert!(s.visibility().is_visible(p));
        }
        s.handle_command(CommandId::ViewToggleScopes); // scopes off
        for p in SCOPE_PANELS {
            assert!(!s.visibility().is_visible(p));
        }
        s.handle_command(CommandId::ViewToggleScopes); // scopes on
        for p in SCOPE_PANELS {
            assert!(s.visibility().is_visible(p));
        }
    }

    #[test]
    fn unowned_command_is_delegated() {
        let mut s = shell();
        assert_eq!(
            s.handle_command(CommandId::FileSave),
            CommandOutcome::Delegate(CommandId::FileSave)
        );
    }

    #[test]
    fn playback_transport_commands_are_delegated_to_the_host() {
        let mut s = shell();
        for cmd in [
            CommandId::PlaybackToggle,
            CommandId::PlaybackStop,
            CommandId::FrameStepForward,
            CommandId::FrameStepBackward,
        ] {
            assert_eq!(s.handle_command(cmd), CommandOutcome::Delegate(cmd));
        }
    }

    #[test]
    fn chord_dispatch_resolves_and_handles() {
        let mut s = shell();
        let chord: KeyChord = "Cmd+F3".parse().unwrap();
        assert_eq!(s.handle_chord(&chord), Some(CommandOutcome::Handled));
        assert_eq!(s.presets().active_builtin(), Some(BuiltinPreset::Color));
    }

    #[test]
    fn unbound_chord_returns_none() {
        let mut s = shell();
        let chord: KeyChord = "Cmd+Alt+Shift+J".parse().unwrap();
        assert_eq!(s.handle_chord(&chord), None);
    }

    #[test]
    fn detach_via_window_manager() {
        let mut s = shell();
        let id = s.windows_mut().detach(PanelKind::Viewer).unwrap();
        assert!(s.windows().is_detached(PanelKind::Viewer));
        let panel = s.windows_mut().reattach(id).unwrap();
        assert_eq!(panel, PanelKind::Viewer);
    }

    #[test]
    fn default_shell_uses_bundled_bindings() {
        let s = AppShell::default();
        assert!(!s.keybindings().is_empty());
        assert_eq!(s.presets().active_builtin(), Some(BuiltinPreset::Edit));
    }

    #[test]
    fn menu_bar_reflects_live_state() {
        let mut s = shell();
        s.handle_command(CommandId::WorkspaceNode);
        let bar = s.menu_bar();
        let ws = bar.menu("menu.workspace").unwrap();
        let node_checked = ws.items.iter().any(|i| {
            matches!(
                i,
                crate::menu::MenuItem::Action {
                    command: CommandId::WorkspaceNode,
                    check: Some(true),
                }
            )
        });
        assert!(node_checked);
    }

    // -- Panel detach / reattach via command dispatch --

    #[test]
    fn detach_command_with_focused_panel() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        assert!(s.visibility().is_visible(PanelKind::Viewer));

        let outcome = s.handle_command(CommandId::PanelDetach);
        match outcome {
            CommandOutcome::DetachPanel { panel, window_id } => {
                assert_eq!(panel, PanelKind::Viewer);
                assert!(s.windows().is_detached(PanelKind::Viewer));
                assert_eq!(s.windows().window_of(PanelKind::Viewer), Some(window_id));
                // Panel hidden from main window visibility
                assert!(!s.visibility().is_visible(PanelKind::Viewer));
            }
            other => panic!("expected DetachPanel, got {other:?}"),
        }
    }

    #[test]
    fn detach_command_without_focus_is_noop() {
        let mut s = shell();
        assert_eq!(s.focused_panel(), None);
        assert_eq!(
            s.handle_command(CommandId::PanelDetach),
            CommandOutcome::Handled
        );
        assert!(s.windows().is_empty());
    }

    #[test]
    fn detach_command_already_detached_is_noop() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        s.handle_command(CommandId::PanelDetach);
        // Try again — same panel still focused
        assert_eq!(
            s.handle_command(CommandId::PanelDetach),
            CommandOutcome::Handled
        );
        assert_eq!(s.windows().len(), 1);
    }

    #[test]
    fn reattach_command_returns_panel_to_main_window() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        s.handle_command(CommandId::PanelDetach);
        assert!(!s.visibility().is_visible(PanelKind::Viewer));

        let outcome = s.handle_command(CommandId::PanelReattach);
        match outcome {
            CommandOutcome::ReattachPanel { panel, .. } => {
                assert_eq!(panel, PanelKind::Viewer);
                assert!(!s.windows().is_detached(PanelKind::Viewer));
                // Panel visible again in main window
                assert!(s.visibility().is_visible(PanelKind::Viewer));
            }
            other => panic!("expected ReattachPanel, got {other:?}"),
        }
        assert!(s.windows().is_empty());
    }

    #[test]
    fn reattach_command_with_nothing_detached_is_noop() {
        let mut s = shell();
        assert_eq!(
            s.handle_command(CommandId::PanelReattach),
            CommandOutcome::Handled
        );
    }

    #[test]
    fn reattach_prefers_focused_detached_panel() {
        let mut s = shell();
        // Detach two panels
        s.set_focused_panel(Some(PanelKind::Viewer));
        s.handle_command(CommandId::PanelDetach);
        s.set_focused_panel(Some(PanelKind::NodeGraph));
        s.handle_command(CommandId::PanelDetach);
        assert_eq!(s.windows().len(), 2);

        // Focus back to Viewer (which is detached)
        s.set_focused_panel(Some(PanelKind::Viewer));
        let outcome = s.handle_command(CommandId::PanelReattach);
        match outcome {
            CommandOutcome::ReattachPanel { panel, .. } => {
                assert_eq!(panel, PanelKind::Viewer);
            }
            other => panic!("expected ReattachPanel, got {other:?}"),
        }
        assert!(!s.windows().is_detached(PanelKind::Viewer));
        assert!(s.windows().is_detached(PanelKind::NodeGraph));
    }

    #[test]
    fn reattach_falls_back_to_last_detached() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        s.handle_command(CommandId::PanelDetach);
        s.set_focused_panel(Some(PanelKind::NodeGraph));
        s.handle_command(CommandId::PanelDetach);

        // Focus on a non-detached panel (or clear focus)
        s.set_focused_panel(Some(PanelKind::Timeline));
        let outcome = s.handle_command(CommandId::PanelReattach);
        match outcome {
            CommandOutcome::ReattachPanel { panel, .. } => {
                // Last detached was NodeGraph
                assert_eq!(panel, PanelKind::NodeGraph);
            }
            other => panic!("expected ReattachPanel, got {other:?}"),
        }
    }

    #[test]
    fn detach_reattach_cycle_is_consistent() {
        let mut s = shell();
        let panel = PanelKind::Timeline;

        for _ in 0..3 {
            s.set_focused_panel(Some(panel));
            let det = s.handle_command(CommandId::PanelDetach);
            assert!(matches!(det, CommandOutcome::DetachPanel { .. }));
            assert!(s.windows().is_detached(panel));
            assert!(!s.visibility().is_visible(panel));

            let re = s.handle_command(CommandId::PanelReattach);
            assert!(matches!(re, CommandOutcome::ReattachPanel { .. }));
            assert!(!s.windows().is_detached(panel));
            assert!(s.visibility().is_visible(panel));
        }
        assert!(s.windows().is_empty());
    }

    #[test]
    fn detach_chord_dispatches_through_keybindings() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        let chord: KeyChord = "Cmd+Shift+D".parse().unwrap();
        let outcome = s.handle_chord(&chord);
        assert!(matches!(outcome, Some(CommandOutcome::DetachPanel { .. })));
    }

    #[test]
    fn reattach_chord_dispatches_through_keybindings() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        s.handle_command(CommandId::PanelDetach);

        let chord: KeyChord = "Cmd+Shift+R".parse().unwrap();
        let outcome = s.handle_chord(&chord);
        assert!(matches!(
            outcome,
            Some(CommandOutcome::ReattachPanel { .. })
        ));
    }
}
