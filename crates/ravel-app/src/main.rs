// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::path::PathBuf;

use gpui::*;
use gpui_component::Root;
use ravel_app::workspace::{self, RavelWorkspace};
use ravel_i18n::t;
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

    if let Err(e) = ravel_i18n::init(&locale_dir(), "en") {
        eprintln!("[ravel] failed to initialize i18n: {e}");
    }

    gpui_platform::application()
        .with_assets(ravel_app::assets::RavelAssets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            load_ravel_theme(cx);
            workspace::register_panels(cx);
            workspace::register_action_handlers(cx);
            ravel_app::trace::init(cx);
            cx.set_global(ravel_app::panels::FocusedPanelGlobal(None));
            cx.set_global(ravel_app::panels::SelectedPropertiesTarget::default());
            cx.set_global(ravel_app::panels::ViewerFrame::default());
            cx.set_global(workspace::DetachedWindowHandles(Default::default()));

            let shell = AppShell::default();
            cx.set_menus(workspace::build_menus(&shell));
            cx.bind_keys(workspace::build_keybindings(&shell));

            let mut main_workspace = None;
            match cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(1280.0), px(800.0)),
                        cx,
                    ))),
                    titlebar: Some(TitlebarOptions {
                        title: Some(t!("app.title").into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let workspace = cx.new(|cx| RavelWorkspace::new(shell, window, cx));
                    main_workspace = Some(workspace.downgrade());
                    cx.new(|cx| Root::new(workspace, window, cx))
                },
            ) {
                Ok(window) => {
                    cx.set_global(workspace::MainWorkspace::new(
                        window.into(),
                        main_workspace.expect("main workspace was created"),
                    ));
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to open main window");
                    cx.quit();
                    return;
                }
            };

            cx.activate(true);
        });
}

/// Loads the bundled Ravel theme and optionally watches the themes directory for
/// hot-reloading during development.
fn load_ravel_theme(cx: &mut App) {
    let themes_dir = themes_dir();
    if !themes_dir.exists() {
        tracing::warn!("themes directory not found: {}", themes_dir.display());
        // Fall back to the default gpui-component system appearance theme.
        gpui_component::Theme::sync_system_appearance(None, cx);
        return;
    }

    // Load the theme synchronously so it is applied immediately on startup,
    // avoiding a flash of the default theme before the async watcher fires.
    let theme_path = themes_dir.join("ravel.json");
    if let Ok(content) = std::fs::read_to_string(&theme_path) {
        if let Err(e) = gpui_component::ThemeRegistry::global_mut(cx).load_themes_from_str(&content)
        {
            tracing::error!(
                "failed to load Ravel theme from {}: {e}",
                theme_path.display()
            );
        } else {
            let light = gpui_component::ThemeRegistry::global(cx)
                .themes()
                .get("Ravel Light")
                .cloned();
            let dark = gpui_component::ThemeRegistry::global(cx)
                .themes()
                .get("Ravel Dark")
                .cloned();
            if let Some(light) = light {
                gpui_component::Theme::global_mut(cx).light_theme = light;
            }
            if let Some(dark) = dark {
                gpui_component::Theme::global_mut(cx).dark_theme = dark;
            }
        }
    } else {
        tracing::warn!("failed to read Ravel theme from {}", theme_path.display());
    }

    // Watch the themes directory for hot-reloading during development.
    if let Err(e) = gpui_component::ThemeRegistry::watch_dir(themes_dir, cx, |cx| {
        gpui_component::Theme::sync_system_appearance(None, cx);
    }) {
        tracing::error!("failed to watch themes directory: {e}");
    }

    gpui_component::Theme::sync_system_appearance(None, cx);
}
