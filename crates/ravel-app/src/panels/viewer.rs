// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal Viewer panel: displays the FrameBuffer from the current evaluation
//! result. `ProjectState`'s background evaluation publishes the outcome via
//! [`super::ViewerFrame`]; this panel converts a frame into a GPUI
//! [`RenderImage`] once per update and draws it with the `img` element (one
//! textured quad) instead of the previous per-pixel-run `paint_quad` ladder,
//! which degraded to one quad per pixel on gradient/media content. A failed
//! evaluation drops the stale frame and shows a black frame with a small
//! error overlay, so structural edits (e.g. deleting a Geometry node feeding
//! a Rasterize) are immediately visible instead of leaving stale content.

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::dock::{Panel, PanelEvent};
use image::{Frame as ImageFrame, ImageBuffer, Rgba};
use ravel_core::types::FrameBuffer;
use ravel_i18n::t;
use ravel_ui::panel::PanelKind;
use smallvec::SmallVec;
use std::sync::Arc;

use super::{ViewerFrame, is_panel_focused, tab_title, track_panel_focus};

pub struct ViewerPanel {
    /// The current frame converted for GPUI rendering. Rebuilt only when
    /// [`ViewerFrame`] changes, never during `render()`.
    image: Option<Arc<RenderImage>>,
    /// The latest evaluation error, shown over a black frame. When set,
    /// `image` holds the black aspect-fit stand-in (or `None` when the
    /// resolution is unknown); a new result replaces both.
    error: Option<SharedString>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    focused_sub: Subscription,
    #[allow(dead_code)]
    viewer_sub: Subscription,
}

impl ViewerPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let focus_subscriptions = track_panel_focus(PanelKind::Viewer, &focus_handle, window, cx);

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| cx.notify());

        let viewer_sub = cx.observe_global::<ViewerFrame>(|this: &mut Self, cx| {
            let vf = cx.try_global::<ViewerFrame>().cloned().unwrap_or_default();
            let (next, error) = viewer_content(vf);
            this.error = error;
            // `ImageSource::Render` bypasses gpui's image cache, so atlas
            // entries are only freed by an explicit drop_image. Without this
            // every published frame would leak VRAM (one texture per scrub
            // tick). Deferred so `drop_image` sees every window, including
            // one that may be checked out for the current update.
            if let Some(old) = std::mem::replace(&mut this.image, next) {
                cx.defer(move |cx| cx.drop_image(old, None));
            }
            cx.notify();
        });

        // Release the last frame's atlas entry when the panel goes away.
        cx.on_release(|this: &mut Self, cx| {
            if let Some(old) = this.image.take() {
                cx.drop_image(old, None);
            }
        })
        .detach();

        let initial = cx.try_global::<ViewerFrame>().cloned().unwrap_or_default();
        let (image, error) = viewer_content(initial);

        Self {
            image,
            error,
            focus_handle,
            focus_subscriptions,
            focused_sub,
            viewer_sub,
        }
    }
}

/// Split a published [`ViewerFrame`] into the renderable image and the error
/// overlay text. An error with a known resolution gets an opaque black image
/// of that size, so the error state keeps the exact aspect-fit rectangle a
/// real frame would occupy.
fn viewer_content(vf: ViewerFrame) -> (Option<Arc<RenderImage>>, Option<SharedString>) {
    match vf {
        ViewerFrame::Frame(fb) => (frame_buffer_to_render_image(&fb), None),
        ViewerFrame::Blank => (None, None),
        ViewerFrame::Error {
            message,
            resolution,
        } => (
            resolution.and_then(|(w, h)| black_render_image(w, h)),
            Some(message),
        ),
    }
}

/// An opaque black image at the given resolution (the error-state stand-in
/// for the evaluated frame). `None` for degenerate dimensions.
fn black_render_image(width: u32, height: u32) -> Option<Arc<RenderImage>> {
    if width == 0 || height == 0 {
        return None;
    }
    let buffer = ImageBuffer::<Rgba<u8>, _>::from_pixel(width, height, Rgba([0, 0, 0, 255]));
    Some(Arc::new(RenderImage::new(SmallVec::from_elem(
        ImageFrame::new(buffer),
        1,
    ))))
}

impl Panel for ViewerPanel {
    fn panel_name(&self) -> &'static str {
        "viewer"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let display = t!(PanelKind::Viewer.label_key());
        let focused = is_panel_focused(PanelKind::Viewer, cx);
        let color = if focused {
            cx.theme().colors.foreground
        } else {
            cx.theme().colors.muted_foreground
        };
        tab_title(Some(PanelKind::Viewer), SharedString::from(display), color)
    }
}

impl EventEmitter<PanelEvent> for ViewerPanel {}

impl Focusable for ViewerPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ViewerPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border_color = cx.theme().colors.border;
        let bg = cx.theme().colors.background;

        let content: Div = if let Some(message) = &self.error {
            // Evaluation failed: the composition's aspect-fit rectangle
            // drawn black (via the same image path a real frame takes),
            // with a small error overlay instead of the stale image.
            let label = t!("viewer.eval_error");
            let overlay = div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().colors.danger)
                        .child(SharedString::from(format!("{label}: {message}"))),
                );
            let base = match &self.image {
                Some(image) => div().size_full().child(
                    img(image.clone())
                        .object_fit(ObjectFit::ScaleDown)
                        .size_full(),
                ),
                // Resolution unknown: fall back to panel-filling black.
                None => div().size_full().bg(rgb(0x000000)),
            };
            base.relative().child(overlay)
        } else if let Some(image) = &self.image {
            div().size_full().child(
                img(image.clone())
                    // Match the previous paint behavior: aspect-preserving
                    // fit, centered, never upscaled.
                    .object_fit(ObjectFit::ScaleDown)
                    .size_full(),
            )
        } else {
            let msg = t!("viewer.no_output");
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(0x888888))
                .child(SharedString::from(msg))
        };

        div()
            .id("viewer-panel")
            .size_full()
            .bg(bg)
            .border_t_1()
            .border_color(border_color)
            .track_focus(&self.focus_handle)
            .child(content)
    }
}

/// Convert a straight-alpha RGBA f32 [`FrameBuffer`] into the straight-alpha
/// BGRA u8 [`RenderImage`] GPUI's `img` element consumes (the same layout the
/// built-in decoders produce). Returns `None` for degenerate dimensions.
fn frame_buffer_to_render_image(fb: &FrameBuffer) -> Option<Arc<RenderImage>> {
    let span = tracing::debug_span!(
        "frame_to_render_image",
        width = fb.width,
        height = fb.height
    );
    let _guard = span.enter();
    if fb.width == 0 || fb.height == 0 {
        return None;
    }
    let expected = fb.width as usize * fb.height as usize * 4;
    if fb.data.len() != expected {
        return None;
    }

    let mut bytes = Vec::with_capacity(expected);
    for pixel in fb.data.chunks_exact(4) {
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        // BGRA order.
        bytes.push(to_u8(pixel[2]));
        bytes.push(to_u8(pixel[1]));
        bytes.push(to_u8(pixel[0]));
        bytes.push(to_u8(pixel[3]));
    }

    let buffer = ImageBuffer::<Rgba<u8>, _>::from_raw(fb.width, fb.height, bytes)?;
    Some(Arc::new(RenderImage::new(SmallVec::from_elem(
        ImageFrame::new(buffer),
        1,
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back
    // to the built-in one for these plain unit tests.
    use core::prelude::v1::test;

    fn fb(width: u32, height: u32, pixel: [f32; 4]) -> FrameBuffer {
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            data.extend_from_slice(&pixel);
        }
        FrameBuffer {
            width,
            height,
            data: Arc::from(data),
        }
    }

    #[test]
    fn converts_rgba_f32_to_bgra_u8() {
        let frame = fb(2, 2, [1.0, 0.5, 0.0, 1.0]);
        let image = frame_buffer_to_render_image(&frame).unwrap();
        let bytes = image.as_bytes(0).unwrap();
        // BGRA: blue=0, green=128, red=255, alpha=255.
        assert_eq!(&bytes[..4], &[0, 128, 255, 255]);
        assert_eq!(image.size(0).width.0, 2);
        assert_eq!(image.size(0).height.0, 2);
    }

    #[test]
    fn clamps_out_of_range_values() {
        let frame = fb(1, 1, [2.0, -1.0, 0.25, 1.5]);
        let image = frame_buffer_to_render_image(&frame).unwrap();
        let bytes = image.as_bytes(0).unwrap();
        assert_eq!(&bytes[..4], &[64, 0, 255, 255]);
    }

    #[test]
    fn rejects_degenerate_frames() {
        assert!(frame_buffer_to_render_image(&fb(0, 4, [0.0; 4])).is_none());
        let mismatched = FrameBuffer {
            width: 4,
            height: 4,
            data: Arc::from(vec![0.0f32; 8]),
        };
        assert!(frame_buffer_to_render_image(&mismatched).is_none());
    }
}
