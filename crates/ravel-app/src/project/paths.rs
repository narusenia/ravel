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
}
