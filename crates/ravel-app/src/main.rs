// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use gpui::*;
use ravel_app::workspace::{self, RavelWorkspace};

fn main() {
    Application::new().run(|cx: &mut App| {
        gpui_component::init(cx);

        cx.on_action(|_: &workspace::Quit, cx: &mut App| cx.quit());

        cx.set_menus(workspace::build_menus());
        cx.bind_keys(workspace::default_keybindings());

        let window = cx
            .open_window(
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
                |window, cx| cx.new(|cx| RavelWorkspace::new(window, cx)),
            )
            .expect("failed to open main window");

        window
            .update(cx, |workspace, window, cx| {
                workspace.setup_layout(window, cx);
            })
            .expect("failed to setup layout");

        cx.activate(true);
    });
}
