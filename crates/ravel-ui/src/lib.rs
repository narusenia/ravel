// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPUI-based UI shell for Ravel.
//!
//! This crate implements the application shell: the workspace panel taxonomy,
//! workspace presets (Edit / Node / Color / Motion), the menu bar model, a
//! fully customizable keybinding system, multi-window / panel-detach
//! bookkeeping, and the Properties inspector shell. These pieces are kept
//! framework-agnostic and headless so they are unit-testable without a live
//! window.
//!
//! The live GPUI integration — `gpui::App` bootstrap, window creation, native
//! menu bar wiring, and per-panel views built on `gpui_component`'s dock/sheet
//! — is layered on top of this state in the application host (`ravel-app`).
//! [`AppShell`] is the headless state object that host drives: it owns the
//! workspace, keybindings, detached windows, and inspector, and exposes command
//! dispatch (`handle_command` / `handle_chord`) plus a live menu-bar builder.
//!
//! See `docs/specifications/ui-spec.md` and `docs/implementation/tasks/TASK-006.md`.

pub mod command;
pub mod document;
pub mod keybindings;
pub mod keyframes;
pub mod menu;
pub mod panel;
pub mod panels;
pub mod preset;
pub mod properties;
pub mod shell;
pub mod window;

pub use command::CommandId;
pub use keybindings::{KeyBindings, KeyChord};
pub use menu::{Menu, MenuBar, MenuItem};
pub use panel::{PanelKind, PanelVisibility};
pub use preset::{BuiltinPreset, LayoutNode, Orientation, PresetLibrary, WorkspacePreset};
pub use shell::{AppShell, CommandOutcome};
pub use window::{WindowError, WindowId, WindowManager, WindowPlacement};

#[cfg(test)]
mod i18n_coverage {
    use super::*;

    fn load_catalog() -> toml::Table {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/locales/en.toml");
        let text = std::fs::read_to_string(path).expect("en.toml not found");
        text.parse::<toml::Table>()
            .expect("en.toml is invalid TOML")
    }

    fn has_key(table: &toml::Table, dotted_key: &str) -> bool {
        let mut current = toml::Value::Table(table.clone());
        for segment in dotted_key.split('.') {
            match current.as_table().and_then(|t| t.get(segment)) {
                Some(v) => current = v.clone(),
                None => return false,
            }
        }
        true
    }

    #[test]
    fn all_command_label_keys_in_catalog() {
        let catalog = load_catalog();
        for cmd in CommandId::all() {
            let key = cmd.label_key();
            assert!(
                has_key(&catalog, key),
                "missing locale key for CommandId::{cmd:?}: \"{key}\""
            );
        }
    }

    #[test]
    fn all_panel_label_keys_in_catalog() {
        let catalog = load_catalog();
        for kind in PanelKind::ALL {
            let key = kind.label_key();
            assert!(
                has_key(&catalog, key),
                "missing locale key for PanelKind::{kind:?}: \"{key}\""
            );
        }
    }

    #[test]
    fn all_preset_label_keys_in_catalog() {
        let catalog = load_catalog();
        for preset in BuiltinPreset::ALL {
            let key = preset.label_key();
            assert!(
                has_key(&catalog, key),
                "missing locale key for BuiltinPreset::{preset:?}: \"{key}\""
            );
        }
    }
}
