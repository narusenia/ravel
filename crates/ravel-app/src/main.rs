// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::path::PathBuf;

use gpui::*;
use ravel_app::workspace;
use ravel_ui::shell::AppShell;

fn locale_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_dir = exe.parent().unwrap_or(exe.as_path());
    let candidates = [
        // macOS .app bundle: Contents/MacOS/../Resources/locales
        exe_dir.join("../Resources/locales"),
        // Next to binary
        exe_dir.join("assets/locales"),
        // Workspace root (cargo run)
        PathBuf::from("assets/locales"),
    ];
    candidates
        .into_iter()
        .find(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from("assets/locales"))
}

fn themes_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_dir = exe.parent().unwrap_or(exe.as_path());
    let candidates = [
        // macOS .app bundle: Contents/MacOS/../Resources/themes
        exe_dir.join("../Resources/themes"),
        // Next to binary
        exe_dir.join("assets/themes"),
        // Workspace root (cargo run)
        PathBuf::from("assets/themes"),
    ];
    candidates
        .into_iter()
        .find(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from("assets/themes"))
}

fn main() {
    let _ = ravel_core::logging::init_logging("RAVEL_LOG", None);

    // The global settings layer decides the language, so it is read before the
    // first translated string exists. The same values are published as the
    // settings global below, so the file is read once per launch.
    let global_settings = ravel_app::app_settings::read_global_settings();
    if let ravel_app::app_settings::LocaleOutcome::Unavailable { error } =
        ravel_app::app_settings::apply_startup_locale(
            &locale_dir(),
            &global_settings.resolved().locale,
        )
    {
        eprintln!("[ravel] failed to initialize i18n: {error}");
    }

    gpui_platform::application()
        .with_assets(ravel_app::assets::RavelAssets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            // Before the theme: it names families that only resolve once the
            // bundled faces are registered.
            ravel_app::fonts::init(cx);
            load_ravel_themes(cx);
            workspace::register_action_handlers(cx);
            ravel_app::trace::init(cx);
            // The resolved settings, from the layer that was read above. The
            // project layer joins them when a project is opened
            // (`app_settings::set_project_layer`). This also puts the appearance
            // in force, which is why the themes are in the registry first: the
            // theme the settings name has to be there to be chosen.
            ravel_app::app_settings::install(global_settings, cx);
            cx.set_global(ravel_app::panels::FocusedPanelGlobal(None));
            cx.set_global(ravel_app::panels::SelectedPropertiesTarget::default());
            cx.set_global(ravel_app::panels::CanvasSelection::default());
            cx.set_global(ravel_app::panels::ToolState::default());
            cx.set_global(ravel_app::panels::PlaybackPosition::default());
            cx.set_global(ravel_app::panels::ViewerFrame::default());

            // The workspace arrangement of the previous session, if one was
            // recorded and is readable; anything else leaves the built-in
            // default in place (`layout_persist::read_document`).
            let saved_layout = ravel_app::layout_persist::install(cx);
            let mut shell = AppShell::default();
            // The user's keybinding overrides, if any, laid over the bundled
            // defaults. They are installed on the shell rather than bound
            // directly, so `build_keybindings` gives them the same `!Input`
            // context every asset binding gets (`MED-APP-16`), and published as
            // a global so Preferences can say which chord came from where.
            let keybindings = ravel_app::keybindings::read_keybindings();
            shell.set_keybindings(keybindings.bindings().clone());
            ravel_app::keybindings::install(keybindings, cx);
            let restored_windows =
                ravel_app::layout_persist::restore_into(&mut shell, saved_layout.as_ref());
            workspace::install_menus(&shell, cx);
            cx.bind_keys(workspace::build_keybindings(&shell));

            if let Err(e) = ravel_app::window_host::open_main(shell, cx) {
                tracing::error!(error = %e, "failed to open main window");
                cx.quit();
                return;
            }
            // Detached windows follow the main one: they resolve the session
            // through its global, which only exists once the main window's root
            // has been built.
            ravel_app::window_host::open_restored(&restored_windows, cx);

            cx.activate(true);
        });
}

/// Fills the theme registry from the themes directory, and watches it for
/// hot-reloading during development.
///
/// **Which theme is worn is not decided here** — that is the resolved
/// appearance (`app_settings::apply_resolved_appearance`), which runs once the
/// settings are installed. This function only makes the themes available to
/// choose from, so no theme name is hardcoded on this path.
///
/// The directory is read **synchronously** even though `watch_dir` reloads it
/// again a moment later, for two reasons: the first frame must already wear the
/// user's theme rather than flash a default one, and the theme the settings name
/// has to exist by the time the appearance is applied. The asynchronous reload
/// is not a substitute — it lands after the window is up. Re-applying the
/// appearance after a reload is not wired here either: `watch_dir`'s `on_load`
/// runs once, at setup, and says nothing about the reloads that follow every
/// later file change. The appearance follows the registry itself
/// (`app_settings::install` observes it), which covers both.
fn load_ravel_themes(cx: &mut App) {
    let themes_dir = themes_dir();
    if !themes_dir.exists() {
        // Not fatal: the registry keeps gpui-component's built-in themes, and
        // the appearance settings fall back to them by name.
        tracing::warn!("themes directory not found: {}", themes_dir.display());
        return;
    }

    for path in theme_files(&themes_dir) {
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                if let Err(e) =
                    gpui_component::ThemeRegistry::global_mut(cx).load_themes_from_str(&content)
                {
                    // One malformed theme file must not cost the others, which
                    // is also how the registry's own reload treats them.
                    tracing::error!("ignored invalid theme file {}: {e}", path.display());
                }
            }
            Err(e) => tracing::warn!("failed to read theme file {}: {e}", path.display()),
        }
    }

    // Watch the themes directory for hot-reloading during development. Every
    // reload replaces the registry's entries, and re-applying the appearance is
    // the job of the observer in `app_settings` — this callback fires only for
    // the first reload, so using it here would leave every later edit unapplied.
    if let Err(e) = gpui_component::ThemeRegistry::watch_dir(themes_dir, cx, |_cx| {}) {
        tracing::error!("failed to watch themes directory: {e}");
    }
}

/// The `*.json` files in `dir`, in a stable order.
///
/// Sorted because the registry keeps the *first* theme it sees under a given
/// name: which file wins a name collision must not depend on directory order.
/// Only this synchronous pass is ordered — the registry's own asynchronous
/// reload reads the directory itself and takes no order from here, so two files
/// claiming one theme name are the user's to sort out, not something the app
/// promises to resolve the same way twice.
fn theme_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("failed to read themes directory {}: {e}", dir.display());
            return Vec::new();
        }
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json")
        })
        .collect();
    files.sort();
    files
}
