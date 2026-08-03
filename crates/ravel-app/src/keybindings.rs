// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Startup resolution of the keybindings in force (`SET-5`, closes
//! `LOW-APP-15`).
//!
//! The parser has always been able to read a user-authored definition file
//! ([`ravel_ui::keybindings::parser`]); what was missing was the launch step
//! that looks for one. This module is that step, and it is the only place that
//! reads `<config_base>/ravel/keybindings.toml`:
//!
//! ```text
//! assets/keybindings/default.toml (embedded) ─┐
//!                                             ├→ LoadedKeybindings ─→ AppShell
//! <config>/ravel/keybindings.toml ────────────┘   (Global)              │
//!                                                     │                 ▼
//!                                          Preferences ▸ Keybindings   cx.bind_keys(
//!                                          (the read-only list)          build_keybindings)
//! ```
//!
//! Two rules shape the code here:
//!
//! - **A bad keybinding file must never cost a launch, or the bindings it got
//!   right.** A missing file is the ordinary first launch. A file that is not
//!   TOML at all is a warning and the defaults. Anything a *single* entry can
//!   get wrong — an unknown command, an unparseable chord — costs that entry
//!   only ([`ravel_ui::keybindings::parser::overlay_user_toml`]).
//! - **There is one route from a chord to a GPUI binding.** The merged set goes
//!   into [`AppShell`], and
//!   [`build_keybindings`](crate::workspace::build_keybindings) turns it into
//!   `KeyBinding`s that all carry the `!Input` context, so a user's `Right` is
//!   as harmless to a focused text field as the default one is (`MED-APP-16`).
//!   Nothing here constructs a `KeyBinding`.
//!
//! Editing bindings from the UI is `SET-12`; this unit only reads them, so the
//! global is written once at startup and never mutated.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use gpui::{App, Global};
use ravel_ui::command::CommandId;
use ravel_ui::keybindings::parser;
use ravel_ui::keybindings::{KeyBindings, KeyChord};

use crate::project::paths;
use crate::workspace;

/// The bindings in force, plus the provenance a [`KeyBindings`] cannot express.
///
/// Durable shared application state (`.agents/rules/gpui.md`): resolved once at
/// startup, read by whoever renders the keybinding list next. It is not an event
/// channel — nothing is parked here for another entity to consume and clear.
///
/// The same set is handed to [`AppShell`](ravel_ui::shell::AppShell), which is
/// what `cx.bind_keys` is derived from. This global is the *provenance* record
/// beside it; `SET-12` (editing) has to keep the two in step, or collapse them.
pub struct LoadedKeybindings {
    bindings: KeyBindings,
    from_user: HashSet<CommandId>,
    path: Option<PathBuf>,
}

impl Global for LoadedKeybindings {}

impl LoadedKeybindings {
    /// The effective bindings, to be installed on the shell.
    pub fn bindings(&self) -> &KeyBindings {
        &self.bindings
    }

    /// Where a user file would be read from, when the platform has a config
    /// directory at all. The file itself need not exist.
    pub fn user_file(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Only the defaults are in force.
    fn defaults(bindings: KeyBindings, path: Option<PathBuf>) -> Self {
        Self {
            bindings,
            from_user: HashSet::new(),
            path,
        }
    }
}

/// Where a command's current chord came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindingOrigin {
    /// From the bundled `assets/keybindings/default.toml`.
    Default,
    /// From the user's `<config_base>/ravel/keybindings.toml`.
    User,
    /// The command has no chord. Reachable from a menu or a panel only.
    Unassigned,
}

impl KeybindingOrigin {
    /// Every origin, for the locale-coverage test.
    pub const ALL: [Self; 3] = [Self::Default, Self::User, Self::Unassigned];

    /// i18n key for the origin column. UI text is never hardcoded; the host
    /// resolves this through `t!` at render time.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Default => "settings.keybindings.origin.default",
            Self::User => "settings.keybindings.origin.user",
            Self::Unassigned => "settings.keybindings.origin.unassigned",
        }
    }
}

/// One row of the read-only keybinding list: a command, its chord, its origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingRow {
    /// The command the row describes. Its label comes from
    /// [`CommandId::label_key`].
    pub command: CommandId,
    /// The chord bound to it, `None` when nothing is.
    pub chord: Option<KeyChord>,
    /// Where [`Self::chord`] came from. [`KeybindingOrigin::Unassigned`] iff
    /// `chord` is `None`.
    pub origin: KeybindingOrigin,
}

/// Builds the list rows in `CommandId` declaration order.
///
/// Every command gets a row, bound or not: the page answers "what is this
/// command's shortcut" as much as "what does this chord do", and a command
/// silently missing from the list would read as one that does not exist.
///
/// A command bound to more than one chord cannot arise today — a definition
/// file names each command at most once, and
/// [`overlay_user_toml`](parser::overlay_user_toml) drops the default chord of
/// a command the user rebound — but the row picks the lexicographically first
/// chord rather than an arbitrary one so the list can never depend on hash
/// order. `the_defaults_bind_each_command_at_most_once` pins the invariant.
pub fn rows(loaded: &LoadedKeybindings) -> Vec<KeybindingRow> {
    CommandId::all()
        .map(|command| row(command, loaded))
        .collect()
}

/// One command's row, built from the same rules as [`rows`].
fn row(command: CommandId, loaded: &LoadedKeybindings) -> KeybindingRow {
    let chord = loaded
        .bindings
        .iter()
        .filter(|(_, bound)| *bound == command)
        .map(|(chord, _)| *chord)
        .min_by_key(|chord| chord.to_string());
    let origin = match (chord, loaded.from_user.contains(&command)) {
        (None, _) => KeybindingOrigin::Unassigned,
        (Some(_), true) => KeybindingOrigin::User,
        (Some(_), false) => KeybindingOrigin::Default,
    };
    KeybindingRow {
        command,
        chord,
        origin,
    }
}

/// What a context that never called [`install`] is actually running on.
///
/// Parsed once: the keybinding list asks per command and per render, so a
/// per-call parse of the embedded asset would be paid sixty times a frame.
fn without_a_user_file() -> &'static LoadedKeybindings {
    static DEFAULTS: OnceLock<LoadedKeybindings> = OnceLock::new();
    DEFAULTS.get_or_init(|| LoadedKeybindings::defaults(parser::default_bindings(), None))
}

/// The current row for one command, for a list rendering it.
///
/// A context without the global — a test window, a harness that only needs the
/// dialog to render — gets the truth for that context rather than a blank row:
/// the bundled defaults it is in fact running on.
pub fn current_row(command: CommandId, cx: &App) -> KeybindingRow {
    match cx.try_global::<LoadedKeybindings>() {
        Some(loaded) => row(command, loaded),
        None => row(command, without_a_user_file()),
    }
}

/// Resolves the bindings for this launch from the platform config directory.
pub fn read_keybindings() -> LoadedKeybindings {
    read_keybindings_at(paths::global_keybindings_path())
}

/// [`read_keybindings`] against an explicit path (tests, and any future
/// `--keybindings` override).
pub fn read_keybindings_at(path: Option<PathBuf>) -> LoadedKeybindings {
    let defaults = parser::default_bindings();
    let Some(text) = path.as_deref().and_then(read_document) else {
        return LoadedKeybindings::defaults(defaults, path);
    };

    // The commands the file must not reassign, derived from the one code-side
    // table (`workspace::PANEL_BINDINGS`) rather than listed again here.
    match parser::overlay_user_toml(&defaults, &text, &workspace::panel_bound_commands()) {
        Ok(overlay) => {
            // Each skipped entry is named on its own line: "the file had three
            // mistakes" is not actionable, "file.frobnicate is not a command"
            // is. The launch continues either way.
            for skipped in &overlay.skipped {
                tracing::warn!(
                    error = %skipped,
                    path = path.as_deref().unwrap_or(Path::new("")).display().to_string(),
                    "ignoring a keybinding entry"
                );
            }
            LoadedKeybindings {
                bindings: overlay.bindings,
                from_user: overlay.from_user,
                path,
            }
        }
        Err(error) => {
            tracing::warn!(
                %error,
                path = path.as_deref().unwrap_or(Path::new("")).display().to_string(),
                "could not read the keybinding file; starting on the defaults"
            );
            LoadedKeybindings::defaults(defaults, path)
        }
    }
}

/// Reads the user document, degrading to "no document".
///
/// A file that is simply not there is the ordinary first launch and is not
/// logged; anything else is a warning, because a file the user wrote and Ravel
/// cannot open must not silently look empty forever.
fn read_document(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(
                %error,
                path = %path.display(),
                "could not open the keybinding file; starting on the defaults"
            );
            None
        }
    }
}

/// Publishes the resolved bindings as the durable global the list reads.
pub fn install(loaded: LoadedKeybindings, cx: &mut App) {
    cx.set_global(loaded);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &tempfile::TempDir, text: &str) -> PathBuf {
        let path = dir.path().join(paths::GLOBAL_KEYBINDINGS_FILE);
        std::fs::write(&path, text).expect("the temp dir is writable");
        path
    }

    fn row_of(rows: &[KeybindingRow], command: CommandId) -> &KeybindingRow {
        rows.iter()
            .find(|row| row.command == command)
            .expect("every command has a row")
    }

    /// The invariant the row model relies on: no command carries two chords, so
    /// "the command's chord" is well defined.
    #[test]
    fn the_defaults_bind_each_command_at_most_once() {
        let loaded = read_keybindings_at(None);
        for command in CommandId::all() {
            let count = loaded
                .bindings
                .iter()
                .filter(|(_, bound)| *bound == command)
                .count();
            assert!(count <= 1, "{command} is bound to {count} chords");
        }
    }

    /// A user file moves a command's chord, and the list says the new chord came
    /// from the user while its neighbours still read as defaults.
    #[test]
    fn a_user_file_overrides_the_default_chord_and_the_list_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            r#"
            [file]
            save = "Cmd+Alt+S"
        "#,
        );
        let loaded = read_keybindings_at(Some(path));

        assert_eq!(
            loaded.bindings().resolve(&"Cmd+Alt+S".parse().unwrap()),
            Some(CommandId::FileSave)
        );
        assert_eq!(loaded.bindings().resolve(&"Cmd+S".parse().unwrap()), None);

        let rows = rows(&loaded);
        let save = row_of(&rows, CommandId::FileSave);
        assert_eq!(save.origin, KeybindingOrigin::User);
        assert_eq!(
            save.chord.map(|c| c.to_string()).as_deref(),
            Some("Cmd+Alt+S")
        );

        let open = row_of(&rows, CommandId::FileOpen);
        assert_eq!(open.origin, KeybindingOrigin::Default);
        assert_eq!(open.chord.map(|c| c.to_string()).as_deref(), Some("Cmd+O"));
    }

    /// A command nothing binds is listed as unassigned rather than omitted, and
    /// a command whose chord the user took away goes back to unassigned.
    #[test]
    fn an_unbound_command_is_listed_as_unassigned() {
        let plain = rows(&read_keybindings_at(None));
        // `edit.delete` is a panel-context binding registered in code, so the
        // asset leaves it without a chord of its own.
        assert_eq!(
            row_of(&plain, CommandId::EditDelete).origin,
            KeybindingOrigin::Unassigned
        );
        assert_eq!(row_of(&plain, CommandId::EditDelete).chord, None);

        let dir = tempfile::tempdir().unwrap();
        let path = write(
            &dir,
            r#"
            [edit]
            undo = "Cmd+S"
        "#,
        );
        let stolen = rows(&read_keybindings_at(Some(path)));
        assert_eq!(
            row_of(&stolen, CommandId::EditUndo).origin,
            KeybindingOrigin::User
        );
        assert_eq!(
            row_of(&stolen, CommandId::FileSave).origin,
            KeybindingOrigin::Unassigned,
            "a command whose chord the user reassigned has none left"
        );
    }

    /// Every row's origin agrees with its chord, in both directions.
    #[test]
    fn every_row_states_an_origin_consistent_with_its_chord() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, "[view]\nfit = \"Cmd+Alt+F\"\n");
        for loaded in [read_keybindings_at(None), read_keybindings_at(Some(path))] {
            for row in rows(&loaded) {
                assert_eq!(
                    row.chord.is_none(),
                    row.origin == KeybindingOrigin::Unassigned,
                    "{:?} disagrees with its chord",
                    row
                );
            }
        }
    }

    /// The list covers the command table exactly, in declaration order.
    #[test]
    fn the_list_covers_every_command_once_in_declaration_order() {
        let listed: Vec<CommandId> = rows(&read_keybindings_at(None))
            .into_iter()
            .map(|row| row.command)
            .collect();
        assert_eq!(listed, CommandId::all().collect::<Vec<_>>());
    }

    /// No file at all is the ordinary first launch: the defaults, silently.
    #[test]
    fn an_absent_file_launches_on_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = read_keybindings_at(Some(dir.path().join("absent.toml")));
        assert_eq!(
            loaded.bindings().resolve(&"Cmd+Z".parse().unwrap()),
            Some(CommandId::EditUndo)
        );
        assert!(
            rows(&loaded)
                .iter()
                .all(|row| row.origin != KeybindingOrigin::User)
        );
    }

    /// Every way the file can be broken still launches, and a file that is
    /// broken only in places keeps the parts that work.
    #[test]
    fn a_broken_file_launches_on_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let defaults = read_keybindings_at(None);

        // Unreadable as a whole: nothing of it applies.
        for text in ["[file\n", "= = =", "[file]\nsave", "[file]\n[file]\n"] {
            let loaded = read_keybindings_at(Some(write(&dir, text)));
            assert_eq!(
                loaded.bindings().len(),
                defaults.bindings().len(),
                "{text:?} must fall back to the defaults"
            );
            assert!(
                rows(&loaded)
                    .iter()
                    .all(|row| row.origin != KeybindingOrigin::User),
                "{text:?} must contribute nothing"
            );
        }

        // Broken entry by entry: the good entries still apply.
        for text in [
            "[file]\nfrobnicate = \"Cmd+J\"\nimport = \"Cmd+Alt+I\"\n",
            "[file]\nsave = \"Cmd+Boop\"\nimport = \"Cmd+Alt+I\"\n",
            "[file]\nsave = 42\nimport = \"Cmd+Alt+I\"\n",
            "quit = \"Cmd+Q\"\n[file]\nimport = \"Cmd+Alt+I\"\n",
        ] {
            let loaded = read_keybindings_at(Some(write(&dir, text)));
            assert_eq!(
                loaded.bindings().resolve(&"Cmd+Alt+I".parse().unwrap()),
                Some(CommandId::FileImport),
                "{text:?} must keep the entry that is fine"
            );
            let rows = rows(&loaded);
            assert_eq!(
                row_of(&rows, CommandId::FileImport).origin,
                KeybindingOrigin::User,
                "{text:?}"
            );
            assert_eq!(
                row_of(&rows, CommandId::FileSave)
                    .chord
                    .map(|c| c.to_string()),
                Some("Cmd+S".to_owned()),
                "{text:?} must leave the rejected command on its default"
            );
        }
    }

    /// A platform with no config directory resolves to the defaults rather than
    /// to no bindings at all.
    #[test]
    fn no_config_directory_still_yields_the_defaults() {
        let loaded = read_keybindings_at(None);
        assert!(!loaded.bindings().is_empty());
        assert!(loaded.user_file().is_none());
    }
}
