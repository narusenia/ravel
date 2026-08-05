// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Parsers for keybinding definition files (TOML and JSON).
//!
//! # Format
//!
//! A definition file is a set of *sections*, each a table whose keys are
//! command actions and whose values are chord strings. The command id is the
//! dotted concatenation `"<section>.<action>"`, matching
//! [`crate::command::CommandId`]'s canonical string form. A `[meta]` section
//! (file name, author, …) is ignored by the parser.
//!
//! # Two readers, deliberately
//!
//! [`parse_toml`] / [`parse_json`] are **strict**: one bad entry fails the whole
//! document. That is the right rule for the bundled asset, whose mistakes are
//! ours and must fail the test suite rather than degrade silently
//! ([`default_bindings`]).
//!
//! [`overlay_user_toml`] is **tolerant**: it lays a user-authored document over
//! a base set entry by entry, skipping the entries it cannot use and reporting
//! them. That is the right rule for a file the user edited by hand, where one
//! typo must not cost the other bindings — or the launch.
//!
//! ```toml
//! [meta]
//! name = "Ravel Default"
//!
//! [file]
//! save = "Cmd+S"
//!
//! [edit]
//! undo = "Cmd+Z"
//! redo = "Cmd+Shift+Z"
//! ```

use super::{ChordParseError, ConflictError, KeyBindings, KeyChord};
use crate::command::CommandId;
use std::collections::HashSet;
use std::str::FromStr;

/// The section name reserved for file metadata; ignored when parsing bindings.
const META_SECTION: &str = "meta";

/// Errors produced while parsing a keybinding definition file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeybindError {
    /// The document was not valid TOML or JSON.
    #[error("failed to parse keybinding document: {0}")]
    Document(String),
    /// The document root was not a table/object of sections.
    #[error("keybinding document root must be a table of sections")]
    NotSectioned,
    /// A section value was not a table/object.
    #[error("keybinding section '{0}' must be a table")]
    BadSection(String),
    /// A binding value was not a string chord.
    #[error("keybinding '{section}.{action}' must be a string chord")]
    BadValue {
        /// Owning section name.
        section: String,
        /// Action key within the section.
        action: String,
    },
    /// A `<section>.<action>` pair did not name a known command.
    #[error("keybinding '{0}' does not name a known command")]
    UnknownCommand(String),
    /// The command's binding is registered in code against a key context, which
    /// a definition file has no way to express, so the file must not reassign
    /// it. Only [`overlay_user_toml`] produces this.
    #[error("keybinding '{0}' is scoped to a panel in code and cannot be reassigned from a file")]
    PanelScoped(String),
    /// A chord string failed to parse.
    #[error("keybinding '{id}': {source}")]
    Chord {
        /// The `<section>.<action>` id whose chord failed.
        id: String,
        /// The underlying chord parse error.
        #[source]
        source: ChordParseError,
    },
    /// Two commands were bound to the same chord.
    #[error(transparent)]
    Conflict(#[from] ConflictError),
}

/// Parses keybindings from a TOML document.
pub fn parse_toml(input: &str) -> Result<KeyBindings, KeybindError> {
    let value: toml::Value =
        toml::from_str(input).map_err(|e| KeybindError::Document(e.to_string()))?;
    let table = value.as_table().ok_or(KeybindError::NotSectioned)?;

    let mut bindings = KeyBindings::new();
    for (section, section_value) in table {
        if section == META_SECTION {
            continue;
        }
        let section_table = section_value
            .as_table()
            .ok_or_else(|| KeybindError::BadSection(section.clone()))?;
        for (action, chord_value) in section_table {
            let chord_str = chord_value.as_str().ok_or_else(|| KeybindError::BadValue {
                section: section.clone(),
                action: action.clone(),
            })?;
            insert_binding(&mut bindings, section, action, chord_str)?;
        }
    }
    Ok(bindings)
}

/// Parses keybindings from a JSON document.
pub fn parse_json(input: &str) -> Result<KeyBindings, KeybindError> {
    let value: serde_json::Value =
        serde_json::from_str(input).map_err(|e| KeybindError::Document(e.to_string()))?;
    let object = value.as_object().ok_or(KeybindError::NotSectioned)?;

    let mut bindings = KeyBindings::new();
    for (section, section_value) in object {
        if section == META_SECTION {
            continue;
        }
        let section_object = section_value
            .as_object()
            .ok_or_else(|| KeybindError::BadSection(section.clone()))?;
        for (action, chord_value) in section_object {
            let chord_str = chord_value.as_str().ok_or_else(|| KeybindError::BadValue {
                section: section.clone(),
                action: action.clone(),
            })?;
            insert_binding(&mut bindings, section, action, chord_str)?;
        }
    }
    Ok(bindings)
}

fn insert_binding(
    bindings: &mut KeyBindings,
    section: &str,
    action: &str,
    chord_str: &str,
) -> Result<(), KeybindError> {
    let id = format!("{section}.{action}");
    let command = CommandId::from_str(&id).map_err(|_| KeybindError::UnknownCommand(id.clone()))?;
    let chord = KeyChord::from_str(chord_str).map_err(|source| KeybindError::Chord {
        id: id.clone(),
        source,
    })?;
    bindings.bind(chord, command)?;
    Ok(())
}

/// The default keybinding definition shipped with Ravel (`assets/keybindings/default.toml`).
pub const DEFAULT_KEYBINDINGS_TOML: &str =
    include_str!("../../../../assets/keybindings/default.toml");

/// Parses the embedded default keybindings.
///
/// Panics only if the bundled asset is malformed, which is caught by the test
/// suite.
pub fn default_bindings() -> KeyBindings {
    parse_toml(DEFAULT_KEYBINDINGS_TOML).expect("bundled default keybindings must be valid")
}

/// A user-authored document laid over a base binding set.
///
/// Produced by [`overlay_user_toml`]. `from_user` is what makes a list able to
/// say *where* a chord came from, which no [`KeyBindings`] can express on its
/// own: it holds chords, not provenance.
#[derive(Debug, Clone)]
pub struct UserOverlay {
    /// The effective bindings: the base set with the document applied.
    pub bindings: KeyBindings,
    /// Commands whose chord the document supplied. Every other bound command
    /// still carries the base set's chord.
    pub from_user: HashSet<CommandId>,
    /// Entries the document could not contribute, each with its reason. They
    /// are skipped, never fatal — the caller logs them.
    pub skipped: Vec<KeybindError>,
}

/// Lays a user keybinding document over `base`, entry by entry.
///
/// `Err` is reserved for a document that cannot be read *at all* (invalid TOML);
/// the caller's answer to that is to keep `base`. Everything a single entry can
/// get wrong — an unknown command id, an unparseable chord, a non-string value,
/// a section that is not a table — skips that entry, records it in
/// [`UserOverlay::skipped`], and leaves the rest of the document in force.
///
/// Two resolution rules, both chosen so that the user's file is the one that
/// decides:
///
/// - **A command the document names takes only the document's chord.** Its
///   chord in `base` is dropped rather than kept alongside, so rebinding
///   `file.save` *moves* the shortcut instead of adding a second one.
/// - **A chord the document claims is taken from `base`.** The base command
///   that held it keeps whatever other chord the document gave it, and is
///   otherwise left unbound. A user file that could not reuse a chord already
///   spent by a default would be unable to express most rebindings.
///
/// Within the document itself the strict rule still holds — a chord names one
/// command — but it cannot fail a launch either. **Entries are applied in
/// ascending `<section>.<action>` order, and the first one to claim a chord
/// keeps it**; the later entry is skipped with a [`KeybindError::Conflict`]
/// naming both, so the log says which one lost. The order is imposed here rather
/// than inherited from the TOML crate's map iteration, so which entry wins is a
/// stated rule instead of an implementation detail that could change under us.
///
/// `reserved` names the commands the file must not touch at all: their bindings
/// are registered in code against a panel key context, which the definition
/// format cannot express, so accepting an entry for one of them would silently
/// widen a deliberately panel-scoped shortcut into a global one. Those entries
/// are skipped with [`KeybindError::PanelScoped`]. A command that is simply
/// bound nowhere is *not* reserved — giving it a chord is what this file is for.
pub fn overlay_user_toml(
    base: &KeyBindings,
    input: &str,
    reserved: &HashSet<CommandId>,
) -> Result<UserOverlay, KeybindError> {
    let value: toml::Value =
        toml::from_str(input).map_err(|e| KeybindError::Document(e.to_string()))?;
    let table = value.as_table().ok_or(KeybindError::NotSectioned)?;

    let mut skipped = Vec::new();
    let mut entries: Vec<(String, &str)> = Vec::new();
    for (section, section_value) in table {
        if section == META_SECTION {
            continue;
        }
        let Some(section_table) = section_value.as_table() else {
            skipped.push(KeybindError::BadSection(section.clone()));
            continue;
        };
        for (action, chord_value) in section_table {
            let Some(chord_str) = chord_value.as_str() else {
                skipped.push(KeybindError::BadValue {
                    section: section.clone(),
                    action: action.clone(),
                });
                continue;
            };
            entries.push((format!("{section}.{action}"), chord_str));
        }
    }
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut user = KeyBindings::new();
    for (id, chord_str) in &entries {
        if let Err(error) = insert_user_binding(&mut user, id, chord_str, reserved) {
            skipped.push(error);
        }
    }

    Ok(UserOverlay {
        bindings: merge_over_base(base, user.clone()),
        from_user: user.iter().map(|(_, cmd)| cmd).collect(),
        skipped,
    })
}

/// Inserts one entry of a user document, refusing the reserved commands.
///
/// Separate from [`insert_binding`] so the strict asset path keeps exactly the
/// rules it had: the asset may bind anything, including the panel-scoped
/// commands, because a reserved command is only reserved against *files the
/// user edits*.
fn insert_user_binding(
    bindings: &mut KeyBindings,
    id: &str,
    chord_str: &str,
    reserved: &HashSet<CommandId>,
) -> Result<(), KeybindError> {
    let command =
        CommandId::from_str(id).map_err(|_| KeybindError::UnknownCommand(id.to_owned()))?;
    if reserved.contains(&command) {
        return Err(KeybindError::PanelScoped(id.to_owned()));
    }
    let chord = KeyChord::from_str(chord_str).map_err(|source| KeybindError::Chord {
        id: id.to_owned(),
        source,
    })?;
    bindings.bind(chord, command)?;
    Ok(())
}

/// Fills `user` out with the entries of `base` that it neither replaced nor
/// displaced.
///
/// Both skips implement the rules documented on [`overlay_user_toml`]: a base
/// entry for a command the user rebound would be a duplicate shortcut, and a
/// base entry on a chord the user claimed would be a conflict the user already
/// resolved in their favour.
fn merge_over_base(base: &KeyBindings, user: KeyBindings) -> KeyBindings {
    let claimed: HashSet<CommandId> = user.iter().map(|(_, cmd)| cmd).collect();
    let mut merged = user;
    for (chord, cmd) in base.iter() {
        if claimed.contains(&cmd) || merged.resolve(chord).is_some() {
            continue;
        }
        // Cannot conflict: the chord is unbound in `merged` as just checked.
        merged.force_bind(*chord, cmd);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::{Key, Modifiers};

    #[test]
    fn parses_sectioned_toml() {
        let doc = r#"
            [meta]
            name = "Test"

            [file]
            save = "Cmd+S"
            open = "Cmd+O"

            [edit]
            undo = "Cmd+Z"
        "#;
        let kb = parse_toml(doc).unwrap();
        assert_eq!(kb.len(), 3);
        let save: KeyChord = "Cmd+S".parse().unwrap();
        assert_eq!(kb.resolve(&save), Some(CommandId::FileSave));
    }

    #[test]
    fn parses_equivalent_json() {
        let doc = r#"{
            "meta": { "name": "Test" },
            "edit": { "undo": "Cmd+Z", "redo": "Cmd+Shift+Z" }
        }"#;
        let kb = parse_json(doc).unwrap();
        assert_eq!(kb.len(), 2);
        let redo = KeyChord::new(
            Modifiers {
                command: true,
                shift: true,
                ..Modifiers::NONE
            },
            Key::Char('z'),
        );
        assert_eq!(kb.resolve(&redo), Some(CommandId::EditRedo));
    }

    #[test]
    fn unknown_command_is_rejected() {
        let doc = r#"
            [file]
            frobnicate = "Cmd+J"
        "#;
        let err = parse_toml(doc).unwrap_err();
        assert!(matches!(err, KeybindError::UnknownCommand(id) if id == "file.frobnicate"));
    }

    #[test]
    fn default_bindings_cover_playback_transport() {
        let kb = default_bindings();
        let cases = [
            ("Space", CommandId::PlaybackToggle),
            ("K", CommandId::PlaybackStop),
            ("Right", CommandId::FrameStepForward),
            ("Left", CommandId::FrameStepBackward),
        ];
        for (chord, command) in cases {
            let chord: KeyChord = chord.parse().unwrap();
            assert_eq!(kb.resolve(&chord), Some(command));
        }
    }

    /// The `Alt+N` row is muscle memory, so a renumbering has to be a
    /// deliberate edit here rather than a side effect of touching the asset.
    /// Outliner and Media Bin joined the row after `MED-APP-23`; the four
    /// placeholder panels are intentionally absent (see
    /// `issues/closed/medium-app-shell.md`).
    #[test]
    fn default_bindings_cover_the_view_toggle_row() {
        let kb = default_bindings();
        let cases = [
            ("Alt+1", CommandId::ViewToggleTimeline),
            ("Alt+2", CommandId::ViewToggleNodeGraph),
            ("Alt+3", CommandId::ViewToggleViewer),
            ("Alt+4", CommandId::ViewToggleProperties),
            ("Alt+5", CommandId::ViewToggleCurveEditor),
            ("Alt+6", CommandId::ViewToggleScopes),
            ("Alt+7", CommandId::ViewToggleOutliner),
            ("Alt+8", CommandId::ViewToggleMediaBin),
        ];
        for (chord, command) in cases {
            let chord: KeyChord = chord.parse().unwrap();
            assert_eq!(kb.resolve(&chord), Some(command), "{chord:?}");
        }
    }

    #[test]
    fn bad_chord_is_rejected() {
        let doc = r#"
            [file]
            save = "Cmd+Boop"
        "#;
        let err = parse_toml(doc).unwrap_err();
        assert!(matches!(err, KeybindError::Chord { .. }));
    }

    #[test]
    fn conflicting_bindings_are_rejected() {
        let doc = r#"
            [file]
            save = "Cmd+S"
            open = "Cmd+S"
        "#;
        let err = parse_toml(doc).unwrap_err();
        assert!(matches!(err, KeybindError::Conflict(_)));
    }

    #[test]
    fn non_string_value_is_rejected() {
        let doc = r#"
            [file]
            save = 42
        "#;
        let err = parse_toml(doc).unwrap_err();
        assert!(matches!(err, KeybindError::BadValue { .. }));
    }

    fn chord(s: &str) -> KeyChord {
        s.parse().expect("test chord parses")
    }

    /// [`overlay_user_toml`] with nothing reserved, which is what most of these
    /// cases are about. The reserved path has its own test.
    fn overlay(base: &KeyBindings, input: &str) -> Result<UserOverlay, KeybindError> {
        overlay_user_toml(base, input, &HashSet::new())
    }

    /// Rebinding a command *moves* its shortcut: the default chord stops
    /// resolving, so the command has exactly one chord and the origin of that
    /// chord is the user.
    #[test]
    fn a_user_chord_replaces_the_default_one_for_that_command() {
        let base = default_bindings();
        let overlay = overlay(
            &base,
            r#"
            [file]
            save = "Cmd+Alt+S"
        "#,
        )
        .unwrap();

        assert_eq!(
            overlay.bindings.resolve(&chord("Cmd+Alt+S")),
            Some(CommandId::FileSave)
        );
        assert_eq!(
            overlay.bindings.resolve(&chord("Cmd+S")),
            None,
            "the default chord must not survive as a second shortcut"
        );
        assert!(overlay.from_user.contains(&CommandId::FileSave));
        assert!(
            !overlay.from_user.contains(&CommandId::FileOpen),
            "an untouched command keeps its default origin"
        );
        assert!(overlay.skipped.is_empty());
        // Every other default is still there.
        assert_eq!(
            overlay.bindings.resolve(&chord("Cmd+O")),
            Some(CommandId::FileOpen)
        );
    }

    /// The user wins a chord already spent by a default. The displaced command
    /// is simply left unbound rather than keeping a chord the user reassigned.
    #[test]
    fn a_user_chord_is_taken_from_the_default_command_that_held_it() {
        let base = default_bindings();
        let overlay = overlay(
            &base,
            r#"
            [edit]
            undo = "Cmd+S"
        "#,
        )
        .unwrap();

        assert_eq!(
            overlay.bindings.resolve(&chord("Cmd+S")),
            Some(CommandId::EditUndo)
        );
        assert!(
            !overlay
                .bindings
                .iter()
                .any(|(_, cmd)| cmd == CommandId::FileSave),
            "the displaced command must not keep a second chord"
        );
        assert_eq!(overlay.bindings.resolve(&chord("Cmd+Z")), None);
    }

    /// Swapping two commands' chords resolves as written, in either key order.
    #[test]
    fn a_swap_between_two_commands_resolves_both_ways() {
        let base = default_bindings();
        let overlay = overlay(
            &base,
            r#"
            [edit]
            undo = "Cmd+Shift+Z"
            redo = "Cmd+Z"
        "#,
        )
        .unwrap();

        assert!(overlay.skipped.is_empty());
        assert_eq!(
            overlay.bindings.resolve(&chord("Cmd+Shift+Z")),
            Some(CommandId::EditUndo)
        );
        assert_eq!(
            overlay.bindings.resolve(&chord("Cmd+Z")),
            Some(CommandId::EditRedo)
        );
    }

    /// Every way a single entry can be wrong costs that entry only.
    #[test]
    fn an_unusable_entry_is_skipped_and_the_rest_of_the_document_applies() {
        let base = default_bindings();
        let overlay = overlay(
            &base,
            r#"
            [meta]
            name = "Mine"

            [file]
            frobnicate = "Cmd+J"
            save = "Cmd+Boop"
            open = 42
            import = "Cmd+Alt+I"
        "#,
        )
        .unwrap();

        assert_eq!(overlay.skipped.len(), 3, "{:?}", overlay.skipped);
        assert!(
            overlay
                .skipped
                .iter()
                .any(|e| matches!(e, KeybindError::UnknownCommand(id) if id == "file.frobnicate"))
        );
        assert!(
            overlay
                .skipped
                .iter()
                .any(|e| matches!(e, KeybindError::Chord { id, .. } if id == "file.save"))
        );
        assert!(
            overlay
                .skipped
                .iter()
                .any(|e| matches!(e, KeybindError::BadValue { action, .. } if action == "open"))
        );

        // The one good entry took effect; the rejected ones kept their defaults.
        assert_eq!(
            overlay.bindings.resolve(&chord("Cmd+Alt+I")),
            Some(CommandId::FileImport)
        );
        assert_eq!(overlay.from_user, HashSet::from([CommandId::FileImport]));
        assert_eq!(
            overlay.bindings.resolve(&chord("Cmd+S")),
            Some(CommandId::FileSave)
        );
        assert_eq!(
            overlay.bindings.resolve(&chord("Cmd+O")),
            Some(CommandId::FileOpen)
        );
    }

    /// A top-level value where a section belongs costs that key, not the file.
    #[test]
    fn a_section_that_is_not_a_table_is_skipped() {
        let base = default_bindings();
        let overlay = overlay(
            &base,
            r#"
            save = "Cmd+S"

            [edit]
            undo = "Cmd+Alt+Z"
        "#,
        )
        .unwrap();

        assert!(
            overlay
                .skipped
                .iter()
                .any(|e| matches!(e, KeybindError::BadSection(s) if s == "save"))
        );
        assert_eq!(
            overlay.bindings.resolve(&chord("Cmd+Alt+Z")),
            Some(CommandId::EditUndo)
        );
    }

    /// A chord bound twice inside the user's own file keeps **the entry with the
    /// lower id** and reports the other, instead of failing the document.
    ///
    /// The winner is asserted by name, not as "one of the two": the rule is
    /// ascending `<section>.<action>` order, imposed by an explicit sort. If the
    /// sort were dropped the result would fall back to the TOML crate's map
    /// iteration order — which happens to agree today, and would stop agreeing
    /// the moment that map stops being ordered. This test is what notices.
    #[test]
    fn the_lower_id_wins_a_conflict_inside_the_user_document() {
        let base = default_bindings();
        let overlay = overlay(
            &base,
            r#"
            [file]
            save = "Cmd+Alt+J"
            open = "Cmd+Alt+J"
        "#,
        )
        .unwrap();

        assert_eq!(
            overlay.bindings.resolve(&chord("Cmd+Alt+J")),
            Some(CommandId::FileOpen),
            "file.open sorts before file.save, so it keeps the chord"
        );
        assert_eq!(overlay.from_user, HashSet::from([CommandId::FileOpen]));
        // The loser is reported, naming both sides so a log says who lost.
        assert_eq!(overlay.skipped.len(), 1, "{:?}", overlay.skipped);
        let KeybindError::Conflict(conflict) = &overlay.skipped[0] else {
            panic!("expected a conflict, got {:?}", overlay.skipped[0]);
        };
        assert_eq!(conflict.0.existing, CommandId::FileOpen);
        assert_eq!(conflict.0.incoming, CommandId::FileSave);
        // The command that lost keeps its default rather than nothing.
        assert_eq!(
            overlay.bindings.resolve(&chord("Cmd+S")),
            Some(CommandId::FileSave)
        );
    }

    /// The winner does not depend on the order the sections appear in the file:
    /// the same two entries, written the other way round, resolve the same.
    #[test]
    fn the_conflict_winner_does_not_depend_on_the_written_order() {
        let base = default_bindings();
        let first = overlay(
            &base,
            "[file]\nopen = \"Cmd+Alt+J\"\nsave = \"Cmd+Alt+J\"\n",
        )
        .unwrap();
        let second = overlay(
            &base,
            "[file]\nsave = \"Cmd+Alt+J\"\nopen = \"Cmd+Alt+J\"\n",
        )
        .unwrap();

        for overlay in [first, second] {
            assert_eq!(
                overlay.bindings.resolve(&chord("Cmd+Alt+J")),
                Some(CommandId::FileOpen)
            );
        }
    }

    /// A command bound in code against a panel key context cannot be reassigned
    /// from a file: the format has no way to carry the context, so accepting the
    /// entry would turn a Viewer-only shortcut into a global one.
    #[test]
    fn a_reserved_command_is_refused_and_costs_only_its_own_entry() {
        let base = default_bindings();
        let reserved = HashSet::from([CommandId::ToolPen, CommandId::EditDelete]);
        let result = overlay_user_toml(
            &base,
            r#"
            [tool]
            pen = "D"

            [edit]
            delete = "Cmd+Alt+Backspace"

            [file]
            import = "Cmd+Alt+I"
        "#,
            &reserved,
        )
        .unwrap();

        assert_eq!(result.skipped.len(), 2, "{:?}", result.skipped);
        for id in ["tool.pen", "edit.delete"] {
            assert!(
                result
                    .skipped
                    .iter()
                    .any(|e| matches!(e, KeybindError::PanelScoped(got) if got == id)),
                "{id} must be refused, got {:?}",
                result.skipped
            );
        }
        // Neither reserved command gained a chord.
        assert_eq!(result.bindings.resolve(&chord("D")), None);
        assert_eq!(result.bindings.resolve(&chord("Cmd+Alt+Backspace")), None);
        assert!(!result.from_user.contains(&CommandId::ToolPen));
        assert!(!result.from_user.contains(&CommandId::EditDelete));
        // The entry that was allowed still applies.
        assert_eq!(
            result.bindings.resolve(&chord("Cmd+Alt+I")),
            Some(CommandId::FileImport)
        );
        assert_eq!(result.from_user, HashSet::from([CommandId::FileImport]));
    }

    /// Reserving is only against user files. The strict asset path is unchanged
    /// and may bind anything, including a command a panel also binds.
    #[test]
    fn reserving_does_not_touch_the_strict_asset_path() {
        let kb = parse_toml("[tool]\npen = \"D\"\n").unwrap();
        assert_eq!(kb.resolve(&chord("D")), Some(CommandId::ToolPen));
    }

    /// Only a document that is not TOML at all is fatal, and even then the
    /// caller keeps the base set.
    #[test]
    fn unreadable_toml_is_the_only_fatal_outcome() {
        let base = default_bindings();
        for text in ["[file\n", "= = =", "[file]\nsave"] {
            assert!(
                matches!(overlay(&base, text), Err(KeybindError::Document(_))),
                "{text:?} must be reported as an unreadable document"
            );
        }
    }

    /// An empty or metadata-only document is a no-op, not an empty binding set.
    #[test]
    fn an_empty_document_leaves_the_base_in_force() {
        let base = default_bindings();
        for text in ["", "[meta]\nname = \"Mine\"\n"] {
            let overlay = overlay(&base, text).unwrap();
            assert_eq!(overlay.bindings.len(), base.len(), "{text:?}");
            assert!(overlay.from_user.is_empty(), "{text:?}");
            assert!(overlay.skipped.is_empty(), "{text:?}");
            assert_eq!(
                overlay.bindings.resolve(&chord("Cmd+Z")),
                Some(CommandId::EditUndo),
                "{text:?}"
            );
        }
    }

    #[test]
    fn embedded_default_is_valid_and_nonempty() {
        let kb = default_bindings();
        assert!(!kb.is_empty());
        // Sanity: undo/redo/save are present.
        let undo: KeyChord = "Cmd+Z".parse().unwrap();
        assert_eq!(kb.resolve(&undo), Some(CommandId::EditUndo));
    }
}
