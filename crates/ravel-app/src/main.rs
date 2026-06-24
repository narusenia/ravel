// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use gpui::*;
use ravel_app::workspace::{self, RavelWorkspace};
use ravel_ui::shell::AppShell;

fn main() {
    let _ = ravel_core::logging::init_logging("RAVEL_LOG", None);

    Application::new().run(|cx: &mut App| {
        gpui_component::init(cx);
        workspace::register_quit_handler(cx);

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
                    title: Some("Ravel".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| RavelWorkspace::new(shell, window, cx)),
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
