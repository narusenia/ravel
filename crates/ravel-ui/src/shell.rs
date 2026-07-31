// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Application shell state and command dispatch.
//!
//! [`AppShell`] is the headless heart of the GPUI application shell: it owns the
//! workspace preset library, the effective [`WorkspaceLayout`] (the live
//! split/area trees across all windows), keybindings, and the Properties
//! inspector shell. The GPUI host wraps this state, renders the menu bar and
//! panels from it, and feeds it user input (resolved key chords, menu
//! selections). Keeping the logic here makes the shell fully testable without
//! a live window.
//!
//! The effective layout is the source of truth for panel placement. A preset
//! supplies the initial main-window tree; from then on the layout evolves
//! through [`WorkspaceLayout`] operations — View menu toggles insert missing
//! panels at their [`PanelKind::default_slot`] and remove present ones from
//! their area, so toggling works regardless of what the preset lays out.

use crate::command::CommandId;
use crate::keybindings::{KeyBindings, KeyChord};
use crate::layout::{PanelInstance, PanelInstanceId, WorkspaceLayout};
use crate::menu::MenuBar;
use crate::panel::{PanelKind, PanelVisibility};
use crate::panels::properties::PropertiesPanel;
use crate::preset::{BuiltinPreset, PresetLibrary};
use crate::window::WindowId;

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
    /// A panel instance was detached into a new window; the host should open
    /// the corresponding OS window.
    DetachPanel {
        /// The instance that was moved into the new window.
        instance: PanelInstanceId,
        /// The window id assigned by the [`WorkspaceLayout`].
        window_id: WindowId,
    },
    /// Every panel of a detached window returned to the main window; the host
    /// should close the detached OS window.
    ReattachPanel {
        /// The window id that was released.
        window_id: WindowId,
        /// The instances that moved back into the main window (ids preserved).
        instances: Vec<PanelInstance>,
    },
}

/// Aggregate, headless state for the application shell.
#[derive(Debug, Clone)]
pub struct AppShell {
    presets: PresetLibrary,
    keybindings: KeyBindings,
    layout: WorkspaceLayout,
    properties: PropertiesPanel,
    focused: Option<PanelInstanceId>,
}

impl AppShell {
    /// Builds a shell with the given initial preset and keybindings.
    pub fn new(initial: BuiltinPreset, keybindings: KeyBindings) -> Self {
        let layout = WorkspaceLayout::new(initial.preset().layout)
            .expect("built-in preset layouts are valid");
        Self {
            presets: PresetLibrary::new(initial),
            keybindings,
            layout,
            properties: PropertiesPanel::new(),
            focused: None,
        }
    }

    /// The preset library (active preset, custom presets).
    pub fn presets(&self) -> &PresetLibrary {
        &self.presets
    }

    /// Mutable access to the preset library.
    pub fn presets_mut(&mut self) -> &mut PresetLibrary {
        &mut self.presets
    }

    /// The effective workspace layout: every window and its live layout tree.
    pub fn layout(&self) -> &WorkspaceLayout {
        &self.layout
    }

    /// Mutable access to the effective layout (e.g. the host driving
    /// drag-and-drop rearrangement).
    ///
    /// Mutations that can destroy instances (window close, tab removal)
    /// bypass the shell's focus bookkeeping — prefer the shell-owned
    /// operations (e.g. [`AppShell::close_window`]) for those.
    pub fn layout_mut(&mut self) -> &mut WorkspaceLayout {
        &mut self.layout
    }

    /// Installs a layout that came from outside the session — the one restored
    /// from the application's `layout.toml` at launch, or the one a just-opened
    /// project embedded.
    ///
    /// Returns the windows besides the main one, which the host has to open;
    /// see [`WorkspaceLayout::adopt`] for why the ids are reassigned. Focus
    /// that pointed at an instance the new layout does not have is dropped, so
    /// the shell cannot keep addressing a pane nothing renders.
    pub fn restore_layout(&mut self, layout: &WorkspaceLayout) -> Vec<crate::layout::WindowLayout> {
        let opened = self.layout.adopt(layout);
        self.clear_stale_focus();
        opened
    }

    /// Saves the main window's current tree as a named layout (REQ-UI-005).
    ///
    /// Only the main window's tree is saved, for the same reason preset
    /// switching only replaces it: detached windows are panes the user
    /// deliberately cut out of the session, not part of the arrangement a
    /// named layout describes.
    pub fn save_layout_as(&mut self, name: impl Into<String>) {
        let preset = crate::preset::WorkspacePreset {
            name: name.into(),
            layout: self.layout.main_window().root.clone(),
        };
        self.presets.save_custom(preset);
    }

    /// Applies a previously saved named layout to the main window.
    pub fn apply_custom_layout(&mut self, name: &str) -> Result<(), crate::preset::PresetError> {
        self.presets.switch_custom(name)?;
        let tree = self.presets.active().layout.clone();
        // A stored preset is validated on parse and on save, so the
        // replacement only fails for a layout that was never valid; ignoring
        // it leaves the current arrangement in place.
        let _ = self.layout.replace_main_tree(tree);
        self.clear_stale_focus();
        Ok(())
    }

    /// Forgets a saved named layout. Returns whether one was removed.
    pub fn remove_custom_layout(&mut self, name: &str) -> bool {
        self.presets.remove_custom(name)
    }

    /// Closes a detached window, discarding its instances, and drops the
    /// focus if it pointed into that window. This is the path the host must
    /// take when a detached OS window is closed by the user.
    pub fn close_window(&mut self, id: WindowId) -> Result<(), crate::layout::LayoutError> {
        let result = self.layout.close_window(id);
        if result.is_ok() {
            self.clear_stale_focus();
        }
        result
    }

    /// Current panel visibility, derived from the main window's tree: a panel
    /// is visible iff at least one instance of it is docked in the main
    /// window. This is what the View menu checkboxes reflect.
    pub fn visibility(&self) -> PanelVisibility {
        PanelVisibility::with_visible(self.layout.main_window().root.panels())
    }

    /// The active keybindings.
    pub fn keybindings(&self) -> &KeyBindings {
        &self.keybindings
    }

    /// Replaces the active keybindings (e.g. after reloading the config file).
    pub fn set_keybindings(&mut self, keybindings: KeyBindings) {
        self.keybindings = keybindings;
    }

    /// The Properties inspector shell.
    pub fn properties(&self) -> &PropertiesPanel {
        &self.properties
    }

    /// Mutable access to the Properties inspector shell.
    pub fn properties_mut(&mut self) -> &mut PropertiesPanel {
        &mut self.properties
    }

    /// The currently focused panel instance, if any.
    pub fn focused_instance(&self) -> Option<PanelInstanceId> {
        self.focused
    }

    /// Updates which panel instance currently has focus (called by the host
    /// when panel focus changes in the dock or a detached window).
    pub fn set_focused_instance(&mut self, instance: Option<PanelInstanceId>) {
        self.focused = instance;
    }

    /// The kind of the currently focused panel instance, if any.
    pub fn focused_panel(&self) -> Option<PanelKind> {
        self.focused
            .and_then(|id| self.layout.find_instance(id))
            .map(|(_, instance)| instance.kind)
    }

    /// Focuses the first instance of `panel`. Callers that know the instance
    /// (the GPUI host, whose focus events carry it) use
    /// [`AppShell::set_focused_instance`]; this is the by-kind convenience the
    /// headless tests and menu-level callers use.
    pub fn set_focused_panel(&mut self, panel: Option<PanelKind>) {
        self.focused = panel.and_then(|kind| self.first_instance_of(kind).map(|t| t.id));
    }

    /// The first instance of `kind`: the main window's first, then detached
    /// windows in order.
    pub fn first_instance_of(&self, kind: PanelKind) -> Option<PanelInstance> {
        self.layout
            .windows()
            .iter()
            .find_map(|w| w.root.instances().into_iter().find(|t| t.kind == kind))
    }

    /// The first instance of `kind` docked in the main window, if any.
    fn first_main_instance_of(&self, kind: PanelKind) -> Option<PanelInstance> {
        self.layout
            .main_window()
            .root
            .instances()
            .into_iter()
            .find(|t| t.kind == kind)
    }

    /// Drops the focus if it points at an instance that no longer exists.
    fn clear_stale_focus(&mut self) {
        if self
            .focused
            .is_some_and(|id| self.layout.find_instance(id).is_none())
        {
            self.focused = None;
        }
    }

    /// Builds the current menu bar (checkboxes reflect live state).
    pub fn menu_bar(&self) -> MenuBar {
        MenuBar::build(&self.visibility(), self.presets.active_builtin())
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
            CommandId::ViewToggleOutliner => self.toggle_panel(PanelKind::Outliner),
            CommandId::ViewToggleTimeline => self.toggle_panel(PanelKind::Timeline),
            CommandId::ViewToggleNodeGraph => self.toggle_panel(PanelKind::NodeGraph),
            CommandId::ViewToggleViewer => self.toggle_panel(PanelKind::Viewer),
            CommandId::ViewToggleDopesheet => self.toggle_panel(PanelKind::Dopesheet),
            CommandId::ViewToggleProperties => self.toggle_panel(PanelKind::Properties),
            CommandId::ViewToggleCurveEditor => self.toggle_panel(PanelKind::CurveEditor),
            CommandId::ViewToggleScopes => self.toggle_scopes(),
            CommandId::ViewToggleMediaBin => self.toggle_panel(PanelKind::MediaBin),
            CommandId::WorkspaceEdit => self.switch_preset(BuiltinPreset::Edit),
            CommandId::WorkspaceNode => self.switch_preset(BuiltinPreset::Node),
            CommandId::WorkspaceColor => self.switch_preset(BuiltinPreset::Color),
            CommandId::WorkspaceMotion => self.switch_preset(BuiltinPreset::Motion),
            CommandId::PanelDetach => self.handle_detach(),
            CommandId::PanelReattach => self.handle_reattach(),
            other => CommandOutcome::Delegate(other),
        }
    }

    /// Toggles `panel` in the main window.
    ///
    /// If the main window's tree hosts the panel, its first instance is
    /// removed from its area (the area and its splits fold away when they
    /// empty; the main window's last tab refuses to move). If the panel is
    /// absent, a new instance is inserted at its
    /// [`PanelKind::default_slot`] and focused. Panel placement therefore
    /// never depends on what the active preset lays out.
    pub fn toggle_panel(&mut self, panel: PanelKind) -> CommandOutcome {
        if let Some(existing) = self.first_main_instance_of(panel) {
            // Removing the main window's very last tab is rejected by the
            // layout; the panel simply stays visible.
            let _ = self.layout.remove_instance(existing.id);
        } else {
            let main = self.layout.main_window().id;
            if let Ok(id) = self.layout.insert_instance(main, panel) {
                self.focused = Some(id);
            }
        }
        self.clear_stale_focus();
        CommandOutcome::Handled
    }

    fn toggle_scopes(&mut self) -> CommandOutcome {
        // Drive all scopes from their presence in the main window so they
        // move together.
        let show = !SCOPE_PANELS
            .iter()
            .any(|kind| self.first_main_instance_of(*kind).is_some());
        for kind in SCOPE_PANELS {
            let existing = self.first_main_instance_of(kind);
            match (show, existing) {
                (true, None) => {
                    let main = self.layout.main_window().id;
                    let _ = self.layout.insert_instance(main, kind);
                }
                (false, Some(instance)) => {
                    let _ = self.layout.remove_instance(instance.id);
                }
                _ => {}
            }
        }
        self.clear_stale_focus();
        CommandOutcome::Handled
    }

    fn switch_preset(&mut self, preset: BuiltinPreset) -> CommandOutcome {
        self.presets.switch_builtin(preset);
        let tree = self.presets.active().layout.clone();
        // Built-in preset trees are valid; replacement renumbers instance ids
        // around any detached windows, which stay untouched.
        let _ = self.layout.replace_main_tree(tree);
        self.clear_stale_focus();
        CommandOutcome::Handled
    }

    /// Detaches the focused panel instance into a new window.
    ///
    /// Returns [`CommandOutcome::DetachPanel`] on success so the host can open
    /// the actual OS window, or [`CommandOutcome::Handled`] when there is no
    /// focused instance, the instance cannot leave the main window (its last
    /// tab), or it already sits alone in its own detached window.
    fn handle_detach(&mut self) -> CommandOutcome {
        let Some(instance) = self.focused else {
            return CommandOutcome::Handled;
        };
        let Some((window_id, _)) = self.layout.find_instance(instance) else {
            return CommandOutcome::Handled;
        };
        let already_alone = window_id != self.layout.main_window().id
            && self
                .layout
                .window(window_id)
                .is_some_and(|w| w.root.instances().len() == 1);
        if already_alone {
            return CommandOutcome::Handled;
        }
        match self.layout.detach_to_window(instance) {
            Ok(window_id) => CommandOutcome::DetachPanel {
                instance,
                window_id,
            },
            Err(_) => CommandOutcome::Handled,
        }
    }

    /// Returns every panel of the focused instance's window to the main
    /// window, closing that window.
    ///
    /// Returns [`CommandOutcome::ReattachPanel`] on success so the host can
    /// close the actual OS window, or [`CommandOutcome::Handled`] when the
    /// focus is unset or already in the main window.
    fn handle_reattach(&mut self) -> CommandOutcome {
        let Some(instance) = self.focused else {
            return CommandOutcome::Handled;
        };
        let Some((window_id, _)) = self.layout.find_instance(instance) else {
            return CommandOutcome::Handled;
        };
        if window_id == self.layout.main_window().id {
            return CommandOutcome::Handled;
        }
        match self.layout.absorb_window(window_id) {
            Ok(instances) => {
                self.clear_stale_focus();
                CommandOutcome::ReattachPanel {
                    window_id,
                    instances,
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

    fn main_contains(shell: &AppShell, kind: PanelKind) -> bool {
        shell.layout().main_window().root.panels().contains(&kind)
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
        assert!(!main_contains(&s, PanelKind::Timeline));
    }

    #[test]
    fn toggling_unplaced_panel_inserts_it_at_its_default_slot() {
        let mut s = shell();
        // Dopesheet is not part of the Edit preset (issue #181).
        assert!(!main_contains(&s, PanelKind::Dopesheet));
        assert_eq!(
            s.handle_command(CommandId::ViewToggleDopesheet),
            CommandOutcome::Handled
        );
        assert!(main_contains(&s, PanelKind::Dopesheet));
        assert!(s.visibility().is_visible(PanelKind::Dopesheet));
        assert!(s.layout().is_valid());
        // The new instance gains focus.
        assert_eq!(s.focused_panel(), Some(PanelKind::Dopesheet));
        // Toggling again removes it from its area.
        s.handle_command(CommandId::ViewToggleDopesheet);
        assert!(!main_contains(&s, PanelKind::Dopesheet));
        assert!(s.layout().is_valid());
    }

    /// Issue #181 regression: every panel must be toggleable into the tree in
    /// every preset, regardless of what the preset lays out.
    #[test]
    fn every_panel_toggles_into_every_preset() {
        for preset in BuiltinPreset::ALL {
            let mut s = AppShell::new(preset, default_bindings());
            for kind in PanelKind::ALL {
                if main_contains(&s, kind) {
                    continue;
                }
                assert_eq!(
                    s.toggle_panel(kind),
                    CommandOutcome::Handled,
                    "{preset:?}/{kind:?}"
                );
                assert!(
                    main_contains(&s, kind),
                    "{preset:?}: {kind:?} must appear in the tree"
                );
                assert!(s.layout().is_valid(), "{preset:?}/{kind:?}");
            }
            // All 16 panels are now docked in the main window.
            assert_eq!(
                s.visibility().visible_panels().count(),
                PanelKind::ALL.len()
            );
        }
    }

    #[test]
    fn preset_switch_replaces_main_tree_and_keeps_detached_windows() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        let detach = s.handle_command(CommandId::PanelDetach);
        let CommandOutcome::DetachPanel { window_id, .. } = detach else {
            panic!("expected DetachPanel, got {detach:?}");
        };

        s.handle_command(CommandId::WorkspaceColor);
        assert_eq!(s.presets().active_builtin(), Some(BuiltinPreset::Color));
        // The main tree matches the Color preset; the detached Viewer window
        // is untouched and still valid alongside the renumbered main tree.
        assert!(s.layout().window(window_id).is_some());
        assert!(main_contains(&s, PanelKind::Waveform));
        assert!(s.layout().is_valid());
        // The detached Viewer instance survived the switch.
        assert_eq!(s.focused_panel(), Some(PanelKind::Viewer));
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
            assert!(!main_contains(&s, p));
        }
        s.handle_command(CommandId::ViewToggleScopes); // scopes back on
        for p in SCOPE_PANELS {
            assert!(s.visibility().is_visible(p));
            assert!(main_contains(&s, p));
        }
        assert!(s.layout().is_valid());
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
    fn detach_command_moves_focused_instance_to_new_window() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        assert!(main_contains(&s, PanelKind::Viewer));

        let outcome = s.handle_command(CommandId::PanelDetach);
        let CommandOutcome::DetachPanel {
            instance,
            window_id,
        } = outcome
        else {
            panic!("expected DetachPanel, got {outcome:?}");
        };
        // The instance moved out of the main window into its own window,
        // keeping its id and kind.
        assert!(!main_contains(&s, PanelKind::Viewer));
        let (host, instance_ref) = s.layout().find_instance(instance).unwrap();
        assert_eq!(host, window_id);
        assert_eq!(instance_ref.kind, PanelKind::Viewer);
        assert!(s.layout().is_valid());
    }

    #[test]
    fn detach_command_without_focus_is_noop() {
        let mut s = shell();
        assert_eq!(s.focused_instance(), None);
        assert_eq!(
            s.handle_command(CommandId::PanelDetach),
            CommandOutcome::Handled
        );
        assert_eq!(s.layout().windows().len(), 1);
    }

    #[test]
    fn detach_command_already_alone_in_detached_window_is_noop() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        s.handle_command(CommandId::PanelDetach);
        // The instance is still focused, now alone in its own window.
        assert_eq!(
            s.handle_command(CommandId::PanelDetach),
            CommandOutcome::Handled
        );
        assert_eq!(s.layout().windows().len(), 2);
    }

    #[test]
    fn reattach_returns_focused_windows_panels_to_main() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        s.handle_command(CommandId::PanelDetach);
        assert!(!main_contains(&s, PanelKind::Viewer));

        let outcome = s.handle_command(CommandId::PanelReattach);
        let CommandOutcome::ReattachPanel {
            window_id,
            instances,
        } = outcome
        else {
            panic!("expected ReattachPanel, got {outcome:?}");
        };
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].kind, PanelKind::Viewer);
        assert!(s.layout().window(window_id).is_none());
        assert_eq!(s.layout().windows().len(), 1);
        // The Viewer is docked in the main window again (its default slot),
        // with its instance id preserved.
        assert!(main_contains(&s, PanelKind::Viewer));
        assert_eq!(
            s.layout().find_instance(instances[0].id).unwrap().0,
            s.layout().main_window().id
        );
        assert!(s.layout().is_valid());
    }

    #[test]
    fn reattach_with_focus_in_main_window_is_noop() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        s.handle_command(CommandId::PanelDetach);
        s.set_focused_panel(Some(PanelKind::Timeline));
        assert_eq!(
            s.handle_command(CommandId::PanelReattach),
            CommandOutcome::Handled
        );
        assert_eq!(s.layout().windows().len(), 2);
    }

    #[test]
    fn reattach_with_nothing_detached_is_noop() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        assert_eq!(
            s.handle_command(CommandId::PanelReattach),
            CommandOutcome::Handled
        );
    }

    #[test]
    fn detach_close_toggle_roundtrip_restores_panel_at_default_slot() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        let outcome = s.handle_command(CommandId::PanelDetach);
        let CommandOutcome::DetachPanel { window_id, .. } = outcome else {
            panic!("expected DetachPanel, got {outcome:?}");
        };

        // The user closes the detached OS window: the host drops the window
        // and its instances from the layout.
        s.layout_mut().close_window(window_id).unwrap();
        assert!(
            !s.layout()
                .windows()
                .iter()
                .any(|w| w.root.panels().contains(&PanelKind::Viewer))
        );

        // Toggling the panel back on inserts a fresh instance at its default
        // slot in the main window.
        s.handle_command(CommandId::ViewToggleViewer);
        assert!(main_contains(&s, PanelKind::Viewer));
        assert!(s.visibility().is_visible(PanelKind::Viewer));
        assert!(s.layout().is_valid());
    }

    #[test]
    fn detach_reattach_cycle_is_consistent() {
        let mut s = shell();
        let panel = PanelKind::Timeline;

        for _ in 0..3 {
            s.set_focused_panel(Some(panel));
            let det = s.handle_command(CommandId::PanelDetach);
            assert!(matches!(det, CommandOutcome::DetachPanel { .. }));
            assert!(!main_contains(&s, panel));
            assert_eq!(s.layout().windows().len(), 2);

            // Focus follows the detached instance; reattach absorbs its
            // window back into the main window.
            let re = s.handle_command(CommandId::PanelReattach);
            assert!(matches!(re, CommandOutcome::ReattachPanel { .. }));
            assert!(main_contains(&s, panel));
            assert_eq!(s.layout().windows().len(), 1);
            assert!(s.layout().is_valid());
        }
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

    // -- named layouts (REQ-UI-005) ------------------------------------------

    #[test]
    fn a_saved_layout_can_be_applied_again_after_a_preset_switch() {
        let mut s = shell();
        s.handle_command(CommandId::ViewToggleDopesheet);
        let saved = s.layout().main_window().root.panels();
        s.save_layout_as("Mine");

        s.handle_command(CommandId::WorkspaceColor);
        assert_ne!(s.layout().main_window().root.panels(), saved);

        s.apply_custom_layout("Mine").unwrap();
        assert_eq!(s.layout().main_window().root.panels(), saved);
        assert_eq!(s.presets().active_builtin(), None);
        assert!(s.layout().is_valid());
    }

    #[test]
    fn applying_an_unknown_layout_leaves_the_session_alone() {
        let mut s = shell();
        let before = s.layout().clone();
        assert!(s.apply_custom_layout("nope").is_err());
        assert_eq!(s.layout(), &before);
    }

    #[test]
    fn a_saved_layout_can_be_forgotten() {
        let mut s = shell();
        s.save_layout_as("Mine");
        assert!(s.remove_custom_layout("Mine"));
        assert!(!s.remove_custom_layout("Mine"));
        assert!(s.apply_custom_layout("Mine").is_err());
    }

    /// A named layout describes the main window; detached windows are panes the
    /// user cut out on purpose and are not part of it.
    #[test]
    fn a_saved_layout_covers_only_the_main_window() {
        let mut s = shell();
        s.set_focused_panel(Some(PanelKind::Viewer));
        s.handle_command(CommandId::PanelDetach);
        let main_panels = s.layout().main_window().root.panels();

        s.save_layout_as("Mine");
        let saved = s
            .presets()
            .custom_presets()
            .find(|preset| preset.name == "Mine")
            .expect("saved layout");
        assert_eq!(saved.panels(), main_panels);
    }

    // -- restore -------------------------------------------------------------

    /// A restored layout replaces the session's arrangement and reports the
    /// windows the host still has to open.
    #[test]
    fn restore_layout_installs_the_layout_and_reports_its_windows() {
        let mut source = AppShell::new(BuiltinPreset::Color, default_bindings());
        source.set_focused_panel(Some(PanelKind::Viewer));
        source.handle_command(CommandId::PanelDetach);
        let restored = source.layout().clone();

        let mut s = shell();
        let opened = s.restore_layout(&restored);
        assert_eq!(opened.len(), 1);
        assert_eq!(
            s.layout().main_window().root.panels(),
            restored.main_window().root.panels()
        );
        assert!(s.layout().is_valid());
        // Focus from the previous arrangement cannot survive into a layout that
        // does not hold that instance.
        assert!(
            s.focused_instance()
                .is_none_or(|id| s.layout().find_instance(id).is_some())
        );
    }
}
