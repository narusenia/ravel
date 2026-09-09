// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where theme files live, how they are read, and what happens when one
//! changes.
//!
//! Two directories, read in order: the **bundled** one that ships with the
//! installation, and the **user's** one under the global config directory
//! (`<config>/ravel/themes`). The user's is read last, so a theme that claims a
//! name the bundled set already uses replaces it rather than being ignored —
//! putting a file called `ravel.json` in your own directory is how you edit the
//! shipped theme without writing inside the application bundle.
//!
//! A load **replaces** the theme set rather than adding to it, so re-running it
//! is all a reload is: same directories, same order, same derivation.

use std::path::{Path, PathBuf};

use gpui::App;
use gpui_component::ThemeRegistry;

use crate::theme_tokens::RavelThemes;

/// The user's themes directory, under the global config directory.
pub const USER_THEMES_DIR: &str = "themes";

/// The themes directory that ships with the installation.
///
/// `None` when none of the three layouts is present, which is a broken
/// installation rather than an error worth refusing to launch over: the
/// registry keeps gpui-component's built-in themes and the appearance settings
/// fall back to them by name.
pub fn bundled_theme_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_dir = exe.parent().unwrap_or(exe.as_path()).to_path_buf();
    [
        // macOS .app bundle: Contents/MacOS/../Resources/themes
        exe_dir.join("../Resources/themes"),
        // Next to binary
        exe_dir.join("assets/themes"),
        // Workspace root (cargo run)
        PathBuf::from("assets/themes"),
    ]
    .into_iter()
    .find(|path| path.is_dir())
}

/// The directories to read, in load order: bundled first, the user's last.
///
/// Both are optional and neither is created here. A user who has never written
/// a theme has no `themes` directory, which is the normal case and not
/// something to report; a platform with no config base at all
/// ([`ravel_project::paths::global_config_dir`] returning `None`) leaves the
/// bundled directory on its own.
pub fn theme_dirs() -> Vec<PathBuf> {
    theme_dirs_from(
        bundled_theme_dir().as_deref(),
        ravel_project::paths::global_config_dir().as_deref(),
    )
}

/// [`theme_dirs`] against explicit locations, which is what the tests drive.
pub fn theme_dirs_from(bundled: Option<&Path>, config_dir: Option<&Path>) -> Vec<PathBuf> {
    [
        bundled.map(Path::to_path_buf),
        config_dir.map(|dir| dir.join(USER_THEMES_DIR)),
    ]
    .into_iter()
    .flatten()
    .filter(|dir| dir.is_dir())
    .collect()
}

/// The `*.json` files in `dirs`, in load order.
///
/// Sorted within each directory, because which of two files in one directory
/// claims a name must not depend on the order the filesystem hands them back.
/// Across directories the order is `dirs`' own, and the last file to claim a
/// name keeps it (`RavelThemes::insert_file`).
pub fn theme_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("failed to read themes directory {}: {e}", dir.display());
                continue;
            }
        };
        let mut in_dir: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json")
            })
            .collect();
        in_dir.sort();
        files.append(&mut in_dir);
    }
    files
}

/// Read `dirs` and install what they hold as the themes in force.
///
/// The startup path and the reload path, which is the point: a file change
/// redoes this and nothing else, so a reloaded theme cannot differ from the one
/// a launch would have read. One malformed file costs its own themes and not
/// the others, the way it always has.
///
/// **Which theme is worn is not decided here** — that is the resolved
/// appearance, re-applied at the end because the set it resolves against has
/// just been replaced. No theme name is hardcoded on this path.
pub fn load(dirs: &[PathBuf], cx: &mut App) {
    let mut themes = RavelThemes::default();
    for path in theme_files(dirs) {
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                tracing::warn!("failed to read theme file {}: {e}", path.display());
                continue;
            }
        };
        // A theme file is written in Ravel's schema, and both forms of every
        // theme come out of that one parse (`theme_tokens::RavelThemes`).
        if let Err(e) = themes.insert_file(&content) {
            tracing::error!("ignored invalid theme file {}: {e}", path.display());
        }
    }

    // The borrowed components resolve their theme through the registry, and so
    // does the appearance dropdown, so it gets a copy of the derived set. It
    // can only be added to — `load_themes_from_str` keeps the first theme it
    // sees under a name and nothing removes one — which is why the set above
    // is what an edited or deleted file is resolved against.
    if cx.has_global::<ThemeRegistry>() {
        match themes.registry_json() {
            Ok(json) => {
                if let Err(e) = ThemeRegistry::global_mut(cx).load_themes_from_str(&json) {
                    tracing::error!("failed to hand the themes to the registry: {e}");
                }
            }
            Err(e) => tracing::error!("failed to encode the themes for the registry: {e}"),
        }
    }

    themes.install(cx);
    crate::app_settings::apply_resolved_appearance(cx);
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use ravel_widgets::tokens::{self, Colors, hex_color_string};

    use super::*;

    /// A theme file naming one theme and setting one colour.
    fn theme_file(name: &str, primary: &str) -> String {
        format!(
            r##"{{"name": "Set", "themes": [
                 {{"name": "{name}", "mode": "dark", "colors": {{"primary.background": "{primary}"}}}}
               ]}}"##
        )
    }

    /// The two directories a launch reads, both populated.
    fn dirs(bundled: &Path, config: &Path) -> Vec<PathBuf> {
        std::fs::create_dir_all(config.join(USER_THEMES_DIR)).unwrap();
        theme_dirs_from(Some(bundled), Some(config))
    }

    fn user_dir(config: &Path) -> PathBuf {
        config.join(USER_THEMES_DIR)
    }

    /// The Ravel form of a loaded theme's `primary`.
    fn primary(name: &str, cx: &mut TestAppContext) -> Option<gpui::Hsla> {
        cx.update(|cx| {
            crate::theme_tokens::apply_ravel_theme(name, tokens::ThemeMode::Dark, cx);
            Some(ravel_widgets::ActiveTokens::tokens(&*cx).colors.primary)
        })
    }

    #[test]
    fn a_platform_without_a_config_directory_reads_the_bundled_themes_only() {
        let bundled = tempfile::tempdir().unwrap();
        // `dirs::config_dir()` is documented to return `None` — a headless
        // environment with no `HOME` is the case Ravel still has to launch in.
        assert_eq!(
            theme_dirs_from(Some(bundled.path()), None),
            vec![bundled.path().to_path_buf()],
        );
        // And a config directory that exists but holds no `themes` yet is the
        // normal case, not an error: nothing is created for it.
        let config = tempfile::tempdir().unwrap();
        assert_eq!(
            theme_dirs_from(Some(bundled.path()), Some(config.path())),
            vec![bundled.path().to_path_buf()],
        );
        assert!(!user_dir(config.path()).exists(), "nothing was created");
    }

    #[gpui::test]
    fn a_user_theme_replaces_the_bundled_one_of_the_same_name(cx: &mut TestAppContext) {
        let bundled = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        std::fs::write(
            bundled.path().join("ravel.json"),
            theme_file("Ravel Dark", "#010203"),
        )
        .unwrap();
        let dirs = dirs(bundled.path(), config.path());
        std::fs::write(
            user_dir(config.path()).join("mine.json"),
            theme_file("Ravel Dark", "#040506"),
        )
        .unwrap();

        cx.update(|cx| {
            gpui_component::init(cx);
            load(&dirs, cx);
        });

        assert_eq!(
            primary("Ravel Dark", cx),
            Some(tokens::parse_hex_color("#040506").unwrap()),
            "the user's directory is read last and keeps the name",
        );
    }

    #[gpui::test]
    fn a_reload_derives_the_edited_file_through_ravels_schema(cx: &mut TestAppContext) {
        let bundled = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let dirs = dirs(bundled.path(), config.path());
        let path = user_dir(config.path()).join("mine.json");
        std::fs::write(&path, theme_file("Mine", "#010203")).unwrap();

        cx.update(|cx| {
            gpui_component::init(cx);
            load(&dirs, cx);
        });
        assert_eq!(
            primary("Mine", cx),
            Some(tokens::parse_hex_color("#010203").unwrap()),
        );

        // The edit a user makes with the settings dialog open.
        std::fs::write(&path, theme_file("Mine", "#0A0B0C")).unwrap();
        cx.update(|cx| load(&dirs, cx));

        assert_eq!(
            primary("Mine", cx),
            Some(tokens::parse_hex_color("#0A0B0C").unwrap()),
            "a reload updates the Ravel form, not only the registry's",
        );
        // And the derived form is still derived: the file sets `primary` and
        // nothing else, so every other colour has to be Ravel's built-in rather
        // than gpui-component's stock palette. Borrowing the registry's own
        // reload is exactly what would break this.
        cx.update(|cx| {
            let themes = cx.global::<RavelThemes>();
            let config = themes
                .config("Mine", tokens::ThemeMode::Dark)
                .expect("the theme is loaded");
            assert_eq!(
                config.colors.background.as_deref(),
                Some(hex_color_string(Colors::dark().background).as_str()),
                "an omitted colour is Ravel's built-in after a reload",
            );
        });
    }

    #[gpui::test]
    fn a_deleted_theme_file_takes_its_themes_with_it(cx: &mut TestAppContext) {
        let bundled = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let dirs = dirs(bundled.path(), config.path());
        let kept = user_dir(config.path()).join("kept.json");
        let gone = user_dir(config.path()).join("gone.json");
        std::fs::write(&kept, theme_file("Kept", "#010203")).unwrap();
        std::fs::write(&gone, theme_file("Gone", "#040506")).unwrap();

        cx.update(|cx| {
            gpui_component::init(cx);
            load(&dirs, cx);
        });
        cx.update(|cx| {
            assert!(
                cx.global::<RavelThemes>()
                    .config("Gone", tokens::ThemeMode::Dark)
                    .is_some()
            )
        });

        std::fs::remove_file(&gone).unwrap();
        cx.update(|cx| load(&dirs, cx));

        cx.update(|cx| {
            let themes = cx.global::<RavelThemes>();
            assert!(
                themes.config("Gone", tokens::ThemeMode::Dark).is_none(),
                "a reload rebuilds the set rather than adding to it",
            );
            assert!(
                themes.config("Kept", tokens::ThemeMode::Dark).is_some(),
                "and keeps what is still on disk",
            );
        });
    }
}
