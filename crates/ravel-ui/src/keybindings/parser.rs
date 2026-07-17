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

    #[test]
    fn embedded_default_is_valid_and_nonempty() {
        let kb = default_bindings();
        assert!(!kb.is_empty());
        // Sanity: undo/redo/save are present.
        let undo: KeyChord = "Cmd+Z".parse().unwrap();
        assert_eq!(kb.resolve(&undo), Some(CommandId::EditUndo));
    }
}
