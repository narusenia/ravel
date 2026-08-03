// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! OS-conformant resolution of global (per-user) configuration paths.
//!
//! Ravel keeps a single global settings layer outside any project so that
//! user-wide preferences survive across projects. The concrete directory is
//! resolved by the [`dirs`] crate, which follows each platform's convention:
//!
//! | Platform | Base directory                                   |
//! |----------|--------------------------------------------------|
//! | macOS    | `~/Library/Application Support`                  |
//! | Windows  | `%APPDATA%` (`C:\Users\<user>\AppData\Roaming`)  |
//! | Linux    | `$XDG_CONFIG_HOME` or `~/.config`                |

use std::path::PathBuf;

/// Application directory name appended to the platform config base.
pub const APP_DIR: &str = "ravel";

/// File name of the global settings layer.
pub const GLOBAL_SETTINGS_FILE: &str = "settings.toml";

/// File name of the persisted workspace layout.
///
/// Deliberately its own file rather than a section of [`GLOBAL_SETTINGS_FILE`]:
/// settings are a four-layer merge (default → global → project → user) and the
/// layout is not layered at all — it is one arrangement, replaced wholesale.
/// Keeping them apart also means a corrupt layout can be discarded without
/// touching the user's preferences.
pub const GLOBAL_LAYOUT_FILE: &str = "layout.toml";

/// File name of the user's keybinding overrides.
///
/// Its own file, and in the keybinding assets' own format rather than a section
/// of [`GLOBAL_SETTINGS_FILE`], for two reasons. The shipped defaults already
/// live in that format (`assets/keybindings/default.toml`), so a user file is a
/// copy of something they can read, and a preset someone else authored is a
/// file they can drop in. And the settings layers merge per field, while
/// bindings merge per *command* and can move a chord off another command
/// entirely — a rule the settings resolver does not have and should not grow.
pub const GLOBAL_KEYBINDINGS_FILE: &str = "keybindings.toml";

/// Resolve the global Ravel configuration directory (`<config_base>/ravel`).
///
/// Returns `None` only when the platform config base cannot be determined
/// (e.g. a headless environment with no `HOME`).
pub fn global_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|base| base.join(APP_DIR))
}

/// Resolve the path to the global settings file
/// (`<config_base>/ravel/settings.toml`).
pub fn global_settings_path() -> Option<PathBuf> {
    global_config_dir().map(|dir| dir.join(GLOBAL_SETTINGS_FILE))
}

/// Resolve the path to the persisted workspace layout
/// (`<config_base>/ravel/layout.toml`).
pub fn global_layout_path() -> Option<PathBuf> {
    global_config_dir().map(|dir| dir.join(GLOBAL_LAYOUT_FILE))
}

/// Resolve the path to the user's keybinding overrides
/// (`<config_base>/ravel/keybindings.toml`).
pub fn global_keybindings_path() -> Option<PathBuf> {
    global_config_dir().map(|dir| dir.join(GLOBAL_KEYBINDINGS_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_config_dir_is_under_app_dir() {
        // The function may return None in a sandbox without HOME, which is a
        // valid outcome; only assert structure when a path is produced.
        if let Some(dir) = global_config_dir() {
            assert!(dir.ends_with(APP_DIR));
        }
    }

    #[test]
    fn global_settings_path_ends_with_file() {
        if let Some(path) = global_settings_path() {
            assert!(path.ends_with(GLOBAL_SETTINGS_FILE));
            assert!(path.parent().unwrap().ends_with(APP_DIR));
        }
    }

    #[test]
    fn global_layout_path_is_a_sibling_of_the_settings_file() {
        let (Some(layout), Some(settings)) = (global_layout_path(), global_settings_path()) else {
            return;
        };
        assert!(layout.ends_with(GLOBAL_LAYOUT_FILE));
        assert_eq!(layout.parent(), settings.parent());
        assert_ne!(layout, settings, "the layout is its own file");
    }

    #[test]
    fn global_keybindings_path_is_a_sibling_of_the_settings_file() {
        let (Some(keybindings), Some(settings)) =
            (global_keybindings_path(), global_settings_path())
        else {
            return;
        };
        assert!(keybindings.ends_with(GLOBAL_KEYBINDINGS_FILE));
        assert_eq!(keybindings.parent(), settings.parent());
        assert_ne!(keybindings, settings, "the keybindings are their own file");
    }
}
