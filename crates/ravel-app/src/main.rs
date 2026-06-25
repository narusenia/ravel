// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use gpui::*;
use gpui_component::Root;
use ravel_app::workspace::{self, RavelWorkspace};
use ravel_ui::shell::AppShell;

fn main() {
    let _ = ravel_core::logging::init_logging("RAVEL_LOG", None);

    gpui_platform::application().run(|cx: &mut App| {
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
                    title: Some("Ravel".into()),
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
