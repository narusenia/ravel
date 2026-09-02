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
//! See `docs/specifications/ui-spec.md`.

pub mod command;
pub mod document;
pub mod export;
pub mod keybindings;
pub mod keyframes;
pub mod layout;
pub mod layout_doc;
pub mod menu;
pub mod node_editor;
pub mod node_locale;
pub mod node_search;
pub mod panel;
pub mod panels;
pub mod preset;
pub mod properties;
pub mod shell;
pub mod view_state;
pub mod window;

pub use command::{CommandId, ToolKind};
pub use export::{ExportError, ExportRequest, ExportSettings};
pub use keybindings::{KeyBindings, KeyChord};
pub use layout::{
    LayoutError, LayoutNode, LayoutValidationError, Orientation, PanelInstance, PanelInstanceId,
    WindowLayout, WorkspaceLayout,
};
pub use layout_doc::{LAYOUT_VERSION, LayoutDocError, LayoutDocument, LayoutStore};
pub use menu::{Menu, MenuBar, MenuItem};
pub use panel::{DockSlot, PanelKind, PanelVisibility};
pub use preset::{BuiltinPreset, PresetLibrary, WorkspacePreset};
pub use shell::{AppShell, CommandOutcome};
pub use view_state::ViewStates;
pub use window::{WindowId, WindowPlacement};

#[cfg(test)]
mod i18n_coverage {
    use super::*;

    /// The directory the shipped catalogs live in.
    fn locale_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales")
    }

    /// Every shipped catalog, by locale, discovered from the directory.
    ///
    /// Not a hardcoded `["en", "ja"]`: a catalog added to `assets/locales`
    /// has to be held to the same coverage as the two that are there now, and
    /// a list here would let it ship half-translated.
    fn catalogs() -> Vec<(String, toml::Table)> {
        let mut found: Vec<(String, toml::Table)> = std::fs::read_dir(locale_dir())
            .expect("the locale directory is shipped")
            .map(|entry| entry.expect("readable locale directory").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .map(|path| {
                let locale = path
                    .file_stem()
                    .expect("a .toml file has a stem")
                    .to_string_lossy()
                    .into_owned();
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|_| panic!("{} not readable", path.display()));
                let table = text
                    .parse::<toml::Table>()
                    .unwrap_or_else(|_| panic!("{} is invalid TOML", path.display()));
                (locale, table)
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        assert!(
            found.len() >= 2,
            "the shipped catalogs went missing from {}",
            locale_dir().display()
        );
        found
    }

    /// Whether `dotted_key` resolves to a **string** in `table`.
    ///
    /// The terminal type matters: a dotted key that lands on a table means the
    /// section exists and the string inside it does not, which is exactly the
    /// shape a half-finished translation has. A section that also carries a
    /// label of its own spells it `_self`, the same convention
    /// `ravel_i18n::flatten_toml` reads — so a key naming such a section is
    /// satisfied by that entry.
    fn has_string(table: &toml::Table, dotted_key: &str) -> bool {
        let mut current = toml::Value::Table(table.clone());
        for segment in dotted_key.split('.') {
            match current.as_table().and_then(|t| t.get(segment)) {
                Some(v) => current = v.clone(),
                None => return false,
            }
        }
        match &current {
            toml::Value::String(_) => true,
            toml::Value::Table(section) => section.get("_self").is_some_and(toml::Value::is_str),
            _ => false,
        }
    }

    /// Assert that every key in `keys` is a string in **every** shipped
    /// catalog.
    ///
    /// `t!` falls back to English for a missing key, so a `translate(key) !=
    /// key` assertion cannot see a translation that was never written — only
    /// the catalog files can, which is why the coverage is a file check and
    /// why it runs per locale rather than on `en` alone.
    fn assert_translated_everywhere(what: &str, keys: impl IntoIterator<Item = String>) {
        let keys: Vec<String> = keys.into_iter().collect();
        assert!(!keys.is_empty(), "no {what} keys to check");
        for (locale, catalog) in catalogs() {
            for key in &keys {
                assert!(
                    has_string(&catalog, key),
                    "{locale}.toml has no string for the {what} key \"{key}\""
                );
            }
        }
    }

    #[test]
    fn all_command_label_keys_in_catalog() {
        assert_translated_everywhere(
            "command label",
            CommandId::all().map(|cmd| cmd.label_key().to_owned()),
        );
    }

    /// Every canvas tool, under both keys it is shown by: the toolbar tooltip
    /// and the command label the keybinding list shows for its chord.
    ///
    /// The tools come from the command table rather than a second list, so a
    /// tool added without locale strings fails here.
    #[test]
    fn all_tool_label_keys_in_catalog() {
        assert_translated_everywhere(
            "tool label",
            CommandId::all()
                .filter_map(|cmd| ToolKind::from_command(cmd).map(|tool| (tool, cmd)))
                .flat_map(|(tool, cmd)| [tool.label_key().to_owned(), cmd.label_key().to_owned()]),
        );
    }

    #[test]
    fn all_panel_label_keys_in_catalog() {
        assert_translated_everywhere(
            "panel label",
            PanelKind::ALL
                .iter()
                .map(|kind| kind.label_key().to_owned()),
        );
    }

    #[test]
    fn all_viewer_resolution_label_keys_in_catalog() {
        assert_translated_everywhere(
            "preview resolution label",
            panels::viewer::ViewerResolution::ALL
                .iter()
                .map(|factor| factor.label_key().to_owned()),
        );
    }

    #[test]
    fn all_preset_label_keys_in_catalog() {
        assert_translated_everywhere(
            "workspace preset label",
            BuiltinPreset::ALL
                .iter()
                .map(|preset| preset.label_key().to_owned()),
        );
    }
}
