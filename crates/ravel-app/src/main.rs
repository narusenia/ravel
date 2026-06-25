// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::path::PathBuf;

use gpui::*;
use gpui_component::Root;
use ravel_app::workspace::{self, RavelWorkspace};
use ravel_i18n::t;
use ravel_ui::shell::AppShell;

fn locale_dir() -> PathBuf {
    // In development: assets/locales relative to the executable's ancestor.
    // In production: bundled alongside the binary.
    let exe = std::env::current_exe().unwrap_or_default();
    let candidates = [
        exe.parent().unwrap_or(exe.as_path()).join("assets/locales"),
        PathBuf::from("assets/locales"),
    ];
    candidates
        .into_iter()
        .find(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from("assets/locales"))
}

fn main() {
    let _ = ravel_core::logging::init_logging("RAVEL_LOG", None);

    if let Err(e) = ravel_i18n::init(&locale_dir(), "en") {
        eprintln!("[ravel] failed to initialize i18n: {e}");
    }

    gpui_platform::application()
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            gpui_component::Theme::sync_system_appearance(None, cx);
            workspace::register_action_handlers(cx);
            cx.set_global(ravel_app::panels::FocusedPanelGlobal(None));
            cx.set_global(workspace::PendingCommand(None));
            cx.set_global(workspace::DetachedWindowHandles(Default::default()));

            let shell = AppShell::default();
            cx.set_menus(workspace::build_menus(&shell));
            cx.bind_keys(workspace::build_keybindings(&shell));

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
                    cx.new(|cx| Root::new(workspace, window, cx))
                },
            ) {
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(error = %e, "failed to open main window");
                    cx.quit();
                    return;
                }
            };

            cx.activate(true);
        });
}
