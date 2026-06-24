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
use crate::window::WindowManager;

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
}

/// Aggregate, headless state for the application shell.
#[derive(Debug, Clone)]
pub struct AppShell {
    presets: PresetLibrary,
    keybindings: KeyBindings,
    windows: WindowManager,
    properties: PropertiesPanel,
}

impl AppShell {
    /// Builds a shell with the given initial preset and keybindings.
    pub fn new(initial: BuiltinPreset, keybindings: KeyBindings) -> Self {
        Self {
            presets: PresetLibrary::new(initial),
            keybindings,
            windows: WindowManager::new(),
            properties: PropertiesPanel::new(),
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
    /// (panel toggles, preset switches) and delegating the rest to the host.
    pub fn handle_command(&mut self, command: CommandId) -> CommandOutcome {
        match command {
            CommandId::ViewToggleTimeline => self.toggle(PanelKind::Timeline),
            CommandId::ViewToggleNodeGraph => self.toggle(PanelKind::NodeGraph),
            CommandId::ViewToggleViewer => self.toggle(PanelKind::Viewer),
            CommandId::ViewToggleProperties => self.toggle(PanelKind::Properties),
            CommandId::ViewToggleCurveEditor => self.toggle(PanelKind::CurveEditor),
            CommandId::ViewToggleScopes => self.toggle_scopes(),
            CommandId::WorkspaceEdit => self.switch_preset(BuiltinPreset::Edit),
            CommandId::WorkspaceNode => self.switch_preset(BuiltinPreset::Node),
            CommandId::WorkspaceColor => self.switch_preset(BuiltinPreset::Color),
            CommandId::WorkspaceMotion => self.switch_preset(BuiltinPreset::Motion),
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
}
