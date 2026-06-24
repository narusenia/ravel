// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use gpui::*;
use ravel_app::workspace::{self, DetachedWindows, MainWindowHandle, RavelWorkspace};
use ravel_ui::shell::AppShell;

fn main() {
    Application::new().run(|cx: &mut App| {
        gpui_component::init(cx);
        workspace::register_action_handlers(cx);

        let shell = AppShell::default();
        cx.set_menus(workspace::build_menus(&shell));
        cx.bind_keys(workspace::build_keybindings(&shell));

        let window = match cx.open_window(
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
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = %e, "failed to open main window");
                cx.quit();
                return;
            }
        };

        // Store globals so App-level action handlers can reach the workspace
        // entity for command dispatch and manage detached panel windows.
        cx.set_global(MainWindowHandle(window));
        cx.set_global(DetachedWindows::default());

        if let Err(e) = window.update(cx, |workspace, window, cx| {
            workspace.rebuild_layout(window, cx);
        }) {
            tracing::error!(error = %e, "failed to setup layout");
            cx.quit();
            return;
        }

        cx.activate(true);
    });
}
