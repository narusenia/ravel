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
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            // Before the theme: it names families that only resolve once the
            // bundled faces are registered. Before the splash too — the splash
            // draws text.
            ravel_app::fonts::init(cx);

            // Everything the splash needs is now in place: the locale gives it
            // labels, `fonts::init` gives it a face, and `gpui_component::init`
            // gives `Root` a theme to read. Everything that comes *after* is a
            // startup stage the splash reports on.
            let splash = ravel_app::splash::open(cx);
            cx.spawn(async move |cx| bootstrap(splash, global_settings, cx).await)
                .detach();
        });
}

/// Runs the disk-bound half of startup with the splash up, then hands the
/// session over to the main window.
///
/// Asynchronous for one reason: the platform has to get the main thread back
/// between the stages or none of the progress labels is ever painted. The work
/// itself is the same sequence the synchronous bootstrap ran, in the same
/// order, and each step still runs inside a single `cx.update` on the main
/// thread — nothing here made the startup concurrent.
async fn bootstrap(
    splash: Option<ravel_app::splash::Splash>,
    global_settings: ravel_app::app_settings::GlobalSettingsFile,
    cx: &mut AsyncApp,
) {
    use ravel_app::splash::StartupStage;

    // Moved into `app_settings::install` by the `Settings` stage. Wrapped
    // because the loop body cannot consume a captured value it might visit
    // again — the stage list says it visits each exactly once.
    let mut global_settings = Some(global_settings);
    let mut shell = AppShell::default();
    let mut restored_windows = Vec::new();

    for stage in StartupStage::ALL {
        if let Some(splash) = &splash {
            cx.update(|cx| splash.show_stage(stage, cx));
        }
        // The frame carrying this label, before the work that would otherwise
        // hold the main thread through it.
        ravel_app::splash::stage_break(cx).await;

        cx.update(|cx| match stage {
            StartupStage::Themes => load_ravel_themes(cx),
            StartupStage::Settings => {
                workspace::register_action_handlers(cx);
                ravel_app::trace::init(cx);
                // The resolved settings, from the layer read before the
                // application existed. The project layer joins them when a
                // project is opened (`app_settings::set_project_layer`). This
                // also puts the appearance in force, which is why the themes
                // are in the registry first: the theme the settings name has
                // to be there to be chosen.
                if let Some(file) = global_settings.take() {
                    ravel_app::app_settings::install(file, cx);
                }
                cx.set_global(ravel_app::panels::FocusedPanelGlobal(None));
                cx.set_global(ravel_app::panels::SelectedPropertiesTarget::default());
                cx.set_global(ravel_app::panels::CanvasSelection::default());
                cx.set_global(ravel_app::panels::ToolState::default());
                cx.set_global(ravel_app::panels::PlaybackPosition::default());
                cx.set_global(ravel_app::panels::ViewerFrame::default());
            }
            StartupStage::Keybindings => {
                // The user's keybinding overrides, if any, laid over the
                // bundled defaults. They are installed on the shell rather
                // than bound directly, so `build_keybindings` gives them the
                // same context every asset binding gets (`MED-APP-16` /
                // `MED-APP-31`), and published as a global so Preferences can
                // say which chord came from where.
                let keybindings = ravel_app::keybindings::read_keybindings();
                shell.set_keybindings(keybindings.bindings().clone());
                ravel_app::keybindings::install(keybindings, cx);
                cx.bind_keys(workspace::build_keybindings(&shell));
            }
            StartupStage::Layout => {
                // The workspace arrangement of the previous session, if one
                // was recorded and is readable; anything else leaves the
                // built-in default in place
                // (`layout_persist::read_document`).
                let saved_layout = ravel_app::layout_persist::install(cx);
                restored_windows =
                    ravel_app::layout_persist::restore_into(&mut shell, saved_layout.as_ref());
                // After the restore: the menus describe the arrangement the
                // shell now holds.
                workspace::install_menus(&shell, cx);
            }
        });
    }

    let opened = cx.update(|cx| {
        ravel_app::splash::hand_off_to_main(
            cx,
            |cx| match ravel_app::window_host::open_main(shell, cx) {
                Ok(_handle) => true,
                Err(e) => {
                    tracing::error!(error = %e, "failed to open main window");
                    false
                }
            },
            |cx| {
                if let Some(splash) = splash {
                    splash.dismiss(cx);
                }
            },
            |cx| cx.quit(),
        )
    });
    if !opened {
        // The platform refused the main window. `hand_off_to_main` has already
        // dismissed the splash and asked to quit; there is no session for the
        // restored windows to attach to.
        return;
    }

    cx.update(|cx| {
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
