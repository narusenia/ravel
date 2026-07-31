// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal runtime check for the gpui fork's window APIs
//! (`narusenia/gpui-ce-ravel`): toggles `Window::set_always_on_top` every
//! three seconds and logs window minimize/restore notifications.
//!
//! Run with: `cargo run -p ravel-app --example always_on_top`

use std::time::Duration;

use gpui::{
    App, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};

struct AlwaysOnTopDemo;

impl AlwaysOnTopDemo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe_window_minimized(window, |minimized, _this, _window, _cx| {
            eprintln!("[always_on_top] window minimized: {minimized}");
        })
        .detach();

        window
            .spawn(cx, async move |cx| {
                let mut on_top = false;
                loop {
                    cx.background_executor().timer(Duration::from_secs(3)).await;
                    on_top = !on_top;
                    if cx
                        .update(|window, _| {
                            window.set_always_on_top(on_top);
                            eprintln!("[always_on_top] set_always_on_top({on_top})");
                        })
                        .is_err()
                    {
                        // The window was closed; stop toggling.
                        break;
                    }
                }
            })
            .detach();

        Self
    }
}

impl Render for AlwaysOnTopDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(0x303030))
            .size(px(400.0))
            .justify_center()
            .items_center()
            .text_color(rgb(0xffffff))
            .child("Always-on-top toggles every 3s (see stderr)")
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(400.), px(400.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| AlwaysOnTopDemo::new(window, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
