// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Customizable keybinding system.
//!
//! A keybinding maps a [`KeyChord`] (a key plus its modifier set) to a
//! [`CommandId`]. Bindings are loaded from TOML or JSON definition files (see
//! [`parser`]) so users can fully customize shortcuts and ship NLE presets.
//! Conflict detection prevents a single chord from being bound to two
//! different commands.

pub mod parser;

use crate::command::CommandId;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// Keyboard modifier set for a chord.
///
/// `command` corresponds to the platform "primary" modifier rendered as `Cmd`
/// on macOS and `Ctrl` on Windows/Linux; `control` is the literal Control key.
/// Definition files may use `Cmd`/`Super`/`Meta` for `command` and `Ctrl` for
/// `control`. Keeping them distinct lets a single binding file express
/// platform-correct shortcuts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Platform primary modifier (Cmd on macOS, Ctrl elsewhere).
    pub command: bool,
    /// Literal Control key.
    pub control: bool,
    /// Shift key.
    pub shift: bool,
    /// Alt / Option key.
    pub alt: bool,
}

impl Modifiers {
    /// No modifiers.
    pub const NONE: Modifiers = Modifiers {
        command: false,
        control: false,
        shift: false,
        alt: false,
    };

    /// Returns `true` if no modifier is set.
    pub fn is_empty(self) -> bool {
        self == Modifiers::NONE
    }
}

/// A named (non-character) key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamedKey {
    Space,
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// Function key F1–F24.
    Function(u8),
}

/// The non-modifier portion of a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A printable character key (normalized to lowercase).
    Char(char),
    /// A named key.
    Named(NamedKey),
}

/// A full key chord: a key plus its active modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    /// Active modifiers.
    pub modifiers: Modifiers,
    /// The triggering key.
    pub key: Key,
}

impl KeyChord {
    /// Builds a chord from modifiers and a key.
    pub fn new(modifiers: Modifiers, key: Key) -> Self {
        Self { modifiers, key }
    }
}

/// Error returned when a chord string cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChordParseError {
    /// The chord string was empty.
    #[error("empty key chord")]
    Empty,
    /// A modifier or key token was not recognized.
    #[error("unrecognized key token: {0}")]
    UnknownToken(String),
    /// The chord had modifiers but no terminal key.
    #[error("key chord '{0}' has no key after its modifiers")]
    MissingKey(String),
}

impl FromStr for KeyChord {
    type Err = ChordParseError;

    /// Parses a chord such as `"Ctrl+Shift+Z"` or `"Space"`.
    ///
    /// Tokens are `+`-separated. All leading tokens are treated as modifiers;
    /// the final token is the key.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(ChordParseError::Empty);
        }

        let tokens: Vec<&str> = trimmed.split('+').map(str::trim).collect();
        let (key_token, modifier_tokens) = tokens
            .split_last()
            .expect("split always yields at least one token");

        if key_token.is_empty() {
            return Err(ChordParseError::MissingKey(trimmed.to_owned()));
        }

        let mut modifiers = Modifiers::NONE;
        for token in modifier_tokens {
            match token.to_ascii_lowercase().as_str() {
                "cmd" | "command" | "super" | "meta" | "win" => modifiers.command = true,
                "ctrl" | "control" => modifiers.control = true,
                "shift" => modifiers.shift = true,
                "alt" | "option" | "opt" => modifiers.alt = true,
                "" => return Err(ChordParseError::MissingKey(trimmed.to_owned())),
                _ => return Err(ChordParseError::UnknownToken((*token).to_owned())),
            }
        }

        let key = parse_key(key_token)?;
        Ok(KeyChord::new(modifiers, key))
    }
}

fn parse_key(token: &str) -> Result<Key, ChordParseError> {
    let named = match token.to_ascii_lowercase().as_str() {
        "space" => Some(NamedKey::Space),
        "enter" | "return" => Some(NamedKey::Enter),
        "tab" => Some(NamedKey::Tab),
        "escape" | "esc" => Some(NamedKey::Escape),
        "backspace" => Some(NamedKey::Backspace),
        "delete" | "del" => Some(NamedKey::Delete),
        "left" => Some(NamedKey::Left),
        "right" => Some(NamedKey::Right),
        "up" => Some(NamedKey::Up),
        "down" => Some(NamedKey::Down),
        "home" => Some(NamedKey::Home),
        "end" => Some(NamedKey::End),
        "pageup" | "pgup" => Some(NamedKey::PageUp),
        "pagedown" | "pgdn" => Some(NamedKey::PageDown),
        _ => None,
    };
    if let Some(named) = named {
        return Ok(Key::Named(named));
    }

    // Function keys: F1..F24
    if let Some(rest) = token.strip_prefix(['F', 'f'])
        && let Ok(n) = rest.parse::<u8>()
        && (1..=24).contains(&n)
    {
        return Ok(Key::Named(NamedKey::Function(n)));
    }

    // Single character key.
    let mut chars = token.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(Key::Char(c.to_ascii_lowercase())),
        _ => Err(ChordParseError::UnknownToken(token.to_owned())),
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.modifiers;
        if m.command {
            f.write_str("Cmd+")?;
        }
        if m.control {
            f.write_str("Ctrl+")?;
        }
        if m.alt {
            f.write_str("Alt+")?;
        }
        if m.shift {
            f.write_str("Shift+")?;
        }
        match self.key {
            Key::Char(c) => write!(f, "{}", c.to_ascii_uppercase()),
            Key::Named(named) => write!(f, "{}", named_key_label(named)),
        }
    }
}

fn named_key_label(named: NamedKey) -> String {
    match named {
        NamedKey::Space => "Space".to_owned(),
        NamedKey::Enter => "Enter".to_owned(),
        NamedKey::Tab => "Tab".to_owned(),
        NamedKey::Escape => "Escape".to_owned(),
        NamedKey::Backspace => "Backspace".to_owned(),
        NamedKey::Delete => "Delete".to_owned(),
        NamedKey::Left => "Left".to_owned(),
        NamedKey::Right => "Right".to_owned(),
        NamedKey::Up => "Up".to_owned(),
        NamedKey::Down => "Down".to_owned(),
        NamedKey::Home => "Home".to_owned(),
        NamedKey::End => "End".to_owned(),
        NamedKey::PageUp => "PageUp".to_owned(),
        NamedKey::PageDown => "PageDown".to_owned(),
        NamedKey::Function(n) => format!("F{n}"),
    }
}

/// A conflict between two commands bound to the same chord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyConflict {
    /// The contested chord.
    pub chord: KeyChord,
    /// The command already bound to the chord.
    pub existing: CommandId,
    /// The command that attempted to take the chord.
    pub incoming: CommandId,
}

/// Error returned by [`KeyBindings::bind`] when a chord is already taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("key chord '{}' is already bound to '{}' (cannot bind '{}')", .0.chord, .0.existing, .0.incoming)]
pub struct ConflictError(pub KeyConflict);

/// A set of chord-to-command bindings.
#[derive(Debug, Clone, Default)]
pub struct KeyBindings {
    map: HashMap<KeyChord, CommandId>,
}

impl KeyBindings {
    /// Creates an empty binding set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds `chord` to `command`, returning an error if the chord is already
    /// bound to a different command. Re-binding a chord to the same command is
    /// a no-op success.
    pub fn bind(&mut self, chord: KeyChord, command: CommandId) -> Result<(), ConflictError> {
        if let Some(&existing) = self.map.get(&chord) {
            if existing != command {
                return Err(ConflictError(KeyConflict {
                    chord,
                    existing,
                    incoming: command,
                }));
            }
            return Ok(());
        }
        self.map.insert(chord, command);
        Ok(())
    }

    /// Binds `chord` to `command`, replacing any previous binding.
    pub fn force_bind(&mut self, chord: KeyChord, command: CommandId) {
        self.map.insert(chord, command);
    }

    /// Resolves a chord to its bound command, if any.
    pub fn resolve(&self, chord: &KeyChord) -> Option<CommandId> {
        self.map.get(chord).copied()
    }

    /// Returns the number of bindings.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Returns `true` if there are no bindings.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Iterates over all bindings.
    pub fn iter(&self) -> impl Iterator<Item = (&KeyChord, CommandId)> {
        self.map.iter().map(|(c, cmd)| (c, *cmd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifier_chord() {
        let chord: KeyChord = "Ctrl+Shift+Z".parse().unwrap();
        assert!(chord.modifiers.control);
        assert!(chord.modifiers.shift);
        assert!(!chord.modifiers.command);
        assert_eq!(chord.key, Key::Char('z'));
    }

    #[test]
    fn parses_cmd_aliases_to_command_modifier() {
        for s in ["Cmd+S", "Super+S", "Meta+S", "Win+S"] {
            let chord: KeyChord = s.parse().unwrap();
            assert!(chord.modifiers.command, "{s} should set command");
            assert_eq!(chord.key, Key::Char('s'));
        }
    }

    #[test]
    fn parses_named_and_function_keys() {
        assert_eq!(
            "Space".parse::<KeyChord>().unwrap().key,
            Key::Named(NamedKey::Space)
        );
        assert_eq!(
            "Shift+Delete".parse::<KeyChord>().unwrap().key,
            Key::Named(NamedKey::Delete)
        );
        assert_eq!(
            "F5".parse::<KeyChord>().unwrap().key,
            Key::Named(NamedKey::Function(5))
        );
    }

    #[test]
    fn rejects_empty_and_unknown() {
        assert_eq!("".parse::<KeyChord>().unwrap_err(), ChordParseError::Empty);
        assert!(matches!(
            "Ctrl+Boop".parse::<KeyChord>().unwrap_err(),
            ChordParseError::UnknownToken(_)
        ));
        assert!(matches!(
            "Frobnicate+A".parse::<KeyChord>().unwrap_err(),
            ChordParseError::UnknownToken(_)
        ));
    }

    #[test]
    fn case_insensitive_and_roundtrips_display() {
        let chord: KeyChord = "ctrl+shift+z".parse().unwrap();
        assert_eq!(chord.to_string(), "Ctrl+Shift+Z");
        let reparsed: KeyChord = chord.to_string().parse().unwrap();
        assert_eq!(chord, reparsed);
    }

    #[test]
    fn binding_detects_conflict() {
        let mut kb = KeyBindings::new();
        let chord: KeyChord = "Ctrl+Z".parse().unwrap();
        kb.bind(chord, CommandId::EditUndo).unwrap();
        // Same command -> ok.
        kb.bind(chord, CommandId::EditUndo).unwrap();
        // Different command -> conflict.
        let err = kb.bind(chord, CommandId::EditRedo).unwrap_err();
        assert_eq!(err.0.existing, CommandId::EditUndo);
        assert_eq!(err.0.incoming, CommandId::EditRedo);
    }

    #[test]
    fn resolve_returns_bound_command() {
        let mut kb = KeyBindings::new();
        let chord: KeyChord = "Cmd+S".parse().unwrap();
        kb.bind(chord, CommandId::FileSave).unwrap();
        assert_eq!(kb.resolve(&chord), Some(CommandId::FileSave));
        let other: KeyChord = "Cmd+O".parse().unwrap();
        assert_eq!(kb.resolve(&other), None);
    }

    #[test]
    fn force_bind_overrides() {
        let mut kb = KeyBindings::new();
        let chord: KeyChord = "Ctrl+Z".parse().unwrap();
        kb.bind(chord, CommandId::EditUndo).unwrap();
        kb.force_bind(chord, CommandId::EditRedo);
        assert_eq!(kb.resolve(&chord), Some(CommandId::EditRedo));
        assert_eq!(kb.len(), 1);
    }
}
