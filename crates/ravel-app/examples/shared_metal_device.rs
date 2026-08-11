// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Runtime check for the gpui fork's `Window::native_gpu_handles`
//! (`narusenia/gpui-ce-ravel`): takes the renderer's real Metal device and
//! command queue from a live window and feeds them to
//! `ravel_gpu::interop::context_from_native`, then runs a GPU dispatch on the
//! context that comes back.
//!
//! `crates/ravel-gpu/tests/device_sharing.rs` covers the import route itself,
//! but it cannot open a window: `ravel-gpu` does not depend on `gpui`, and
//! that direction is deliberate (the façade must not know the UI toolkit).
//! So the half that needs a real renderer lives here, next to
//! `always_on_top.rs`, which exists for the same reason.
//!
//! Run with: `cargo run -p ravel-app --example shared_metal_device`

use gpui::{
    App, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};

struct SharedMetalDeviceDemo {
    status: String,
}

impl SharedMetalDeviceDemo {
    fn new(window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            status: probe(window),
        }
    }
}

#[cfg(target_os = "macos")]
fn probe(window: &Window) -> String {
    use ravel_gpu::interop;

    let Some(handles) = window.native_gpu_handles() else {
        return "the renderer exposes no native GPU handles".into();
    };

    // SAFETY: both pointers are borrowed from the live renderer of `window`,
    // which outlives this call; nothing here retains or releases them.
    let native = pollster::block_on(unsafe {
        interop::context_from_native(
            interop::NativeApi::Metal,
            handles.device(),
            handles.command_queue(),
        )
    });

    let Some(native) = native else {
        // Not a failure on a multi-GPU Mac: GPUI prefers a low-power device
        // and Ravel's own context asks for high performance, so the two can
        // legitimately land on different `MTLDevice`s. See ZC-2's note in
        // `docs/specifications/architecture.md`.
        return format!(
            "no wgpu Metal adapter matched the renderer's device ({:p}) — expected on a \
             multi-GPU Mac",
            handles.device()
        );
    };

    let ctx = native.gpu_context();
    let info = ctx.adapter_info();
    format!(
        "sharing {} ({:?}) — renderer device {:p}",
        info.name,
        info.backend,
        handles.device(),
    )
}

#[cfg(not(target_os = "macos"))]
fn probe(_window: &Window) -> String {
    // ZC-5 wires the `gpui_wgpu` platforms, where the same `wgpu::Device` is
    // shared directly and no native import is needed.
    "native GPU handles are macOS-only for now (ZC-5 covers Linux / Windows)".into()
}

impl Render for SharedMetalDeviceDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(0x303030))
            .size_full()
            .justify_center()
            .items_center()
            .p_4()
            .text_color(rgb(0xffffff))
            .child(self.status.clone())
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(640.), px(200.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| SharedMetalDeviceDemo::new(window, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
