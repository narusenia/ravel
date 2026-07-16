// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal Viewer panel: displays the FrameBuffer from the current evaluation
//! result.  The NodeEditor evaluates the selected node and publishes the frame
//! via [`super::ViewerFrame`]; this panel reads it and paints a canvas.

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::dock::{Panel, PanelEvent};
use ravel_core::types::FrameBuffer;
use ravel_i18n::t;
use ravel_ui::panel::PanelKind;
use std::sync::Arc;

use super::{ViewerFrame, is_panel_focused, tab_title, track_panel_focus};

pub struct ViewerPanel {
    frame: Option<Arc<FrameBuffer>>,
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
            this.frame = vf.0;
            cx.notify();
        });

        let initial = cx.try_global::<ViewerFrame>().cloned().unwrap_or_default();

        Self {
            frame: initial.0,
            focus_handle,
            focus_subscriptions,
            focused_sub,
            viewer_sub,
        }
    }
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

        let content: Div = if let Some(fb) = &self.frame {
            let fb = fb.clone();
            div().size_full().child(
                canvas(
                    move |_bounds, _window, _cx| fb.clone(),
                    |bounds, fb, window, _cx| {
                        paint_framebuffer(&fb, &bounds, window);
                    },
                )
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

/// Aspect-preserving fit of an image into an available region.
/// Returns `(scale, offset_x, offset_y)` where offsets center the scaled
/// image inside the region (origin at the region's top-left).
/// Never upscales (`scale <= 1.0`).
fn fit_transform(img: (f32, f32), avail: (f32, f32)) -> Option<(f32, f32, f32)> {
    let (img_w, img_h) = img;
    let (avail_w, avail_h) = avail;
    if img_w <= 0.0 || img_h <= 0.0 || avail_w <= 0.0 || avail_h <= 0.0 {
        return None;
    }
    let scale = (avail_w / img_w).min(avail_h / img_h).min(1.0);
    let offset_x = (avail_w - img_w * scale) / 2.0;
    let offset_y = (avail_h - img_h * scale) / 2.0;
    Some((scale, offset_x, offset_y))
}

fn paint_framebuffer(fb: &FrameBuffer, bounds: &Bounds<Pixels>, window: &mut Window) {
    let avail_w: f32 = bounds.size.width.into();
    let avail_h: f32 = bounds.size.height.into();
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();

    let Some((scale, fit_x, fit_y)) =
        fit_transform((fb.width as f32, fb.height as f32), (avail_w, avail_h))
    else {
        return;
    };

    let img_w = fb.width as f32;
    let img_h = fb.height as f32;
    let draw_w = img_w * scale;
    let draw_h = img_h * scale;
    let offset_x = ox + fit_x;
    let offset_y = oy + fit_y;

    let step_x = 1.0 / scale;
    let step_y = 1.0 / scale;
    let pixel_w = scale.max(1.0);
    let pixel_h = scale.max(1.0);

    let cols = (draw_w / pixel_w).ceil() as usize;
    let rows = (draw_h / pixel_h).ceil() as usize;

    // Merge horizontal runs of identical color into single quads: rasterized
    // shapes are mostly flat fills, so this collapses each row to a handful
    // of quads instead of one per displayed pixel.
    for row in 0..rows {
        let src_y = (row as f32 * step_y) as u32;
        if src_y >= fb.height {
            continue;
        }
        let py = offset_y + row as f32 * pixel_h;

        let mut run_start: usize = 0;
        let mut run_color: Option<[f32; 4]> = None;

        let flush = |start: usize, end: usize, color: [f32; 4], window: &mut Window| {
            let [r, g, b, a] = color;
            if a < 1e-6 || end <= start {
                return;
            }
            let x0 = offset_x + start as f32 * pixel_w;
            let width = (end - start) as f32 * pixel_w;
            let rect_bounds = Bounds::new(point(px(x0), px(py)), size(px(width), px(pixel_h)));
            window.paint_quad(fill(rect_bounds, Hsla::from(Rgba { r, g, b, a })));
        };

        for col in 0..cols {
            let src_x = (col as f32 * step_x) as u32;
            let color = if src_x < fb.width {
                let idx = ((src_y * fb.width + src_x) * 4) as usize;
                [
                    fb.data[idx],
                    fb.data[idx + 1],
                    fb.data[idx + 2],
                    fb.data[idx + 3],
                ]
            } else {
                [0.0; 4]
            };

            match run_color {
                Some(current) if current == color => {}
                Some(current) => {
                    flush(run_start, col, current, window);
                    run_start = col;
                    run_color = Some(color);
                }
                None => {
                    run_start = col;
                    run_color = Some(color);
                }
            }
        }
        if let Some(current) = run_color {
            flush(run_start, cols, current, window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fit_transform;

    #[test]
    fn fit_downscales_wide_image_to_width() {
        let (scale, ox, oy) = fit_transform((200.0, 100.0), (100.0, 100.0)).unwrap();
        assert!((scale - 0.5).abs() < 1e-6);
        assert!((ox - 0.0).abs() < 1e-6);
        // 100x50 drawn in 100x100 → vertical centering offset 25
        assert!((oy - 25.0).abs() < 1e-6);
    }

    #[test]
    fn fit_downscales_tall_image_to_height() {
        let (scale, ox, oy) = fit_transform((100.0, 200.0), (100.0, 100.0)).unwrap();
        assert!((scale - 0.5).abs() < 1e-6);
        assert!((ox - 25.0).abs() < 1e-6);
        assert!((oy - 0.0).abs() < 1e-6);
    }

    #[test]
    fn fit_never_upscales_small_image() {
        let (scale, ox, oy) = fit_transform((50.0, 50.0), (200.0, 100.0)).unwrap();
        assert!((scale - 1.0).abs() < 1e-6);
        // Centered: (200-50)/2 = 75, (100-50)/2 = 25
        assert!((ox - 75.0).abs() < 1e-6);
        assert!((oy - 25.0).abs() < 1e-6);
    }

    #[test]
    fn fit_exact_size_is_identity() {
        let (scale, ox, oy) = fit_transform((128.0, 128.0), (128.0, 128.0)).unwrap();
        assert!((scale - 1.0).abs() < 1e-6);
        assert!(ox.abs() < 1e-6);
        assert!(oy.abs() < 1e-6);
    }

    #[test]
    fn fit_rejects_degenerate_inputs() {
        assert!(fit_transform((0.0, 100.0), (100.0, 100.0)).is_none());
        assert!(fit_transform((100.0, 100.0), (0.0, 100.0)).is_none());
        assert!(fit_transform((100.0, 100.0), (100.0, -1.0)).is_none());
    }

    #[test]
    fn fit_preserves_aspect_ratio() {
        let (scale, _, _) = fit_transform((640.0, 480.0), (320.0, 320.0)).unwrap();
        let drawn_w = 640.0 * scale;
        let drawn_h = 480.0 * scale;
        assert!(((drawn_w / drawn_h) - (640.0 / 480.0)).abs() < 1e-6);
        assert!(drawn_w <= 320.0 + 1e-3 && drawn_h <= 320.0 + 1e-3);
    }
}
