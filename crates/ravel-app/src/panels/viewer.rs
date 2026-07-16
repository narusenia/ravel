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

fn paint_framebuffer(fb: &FrameBuffer, bounds: &Bounds<Pixels>, window: &mut Window) {
    if fb.width == 0 || fb.height == 0 {
        return;
    }

    let avail_w: f32 = bounds.size.width.into();
    let avail_h: f32 = bounds.size.height.into();
    let ox: f32 = bounds.origin.x.into();
    let oy: f32 = bounds.origin.y.into();

    if avail_w <= 0.0 || avail_h <= 0.0 {
        return;
    }

    let img_w = fb.width as f32;
    let img_h = fb.height as f32;
    let scale = (avail_w / img_w).min(avail_h / img_h).min(1.0);
    let draw_w = img_w * scale;
    let draw_h = img_h * scale;
    let offset_x = ox + (avail_w - draw_w) / 2.0;
    let offset_y = oy + (avail_h - draw_h) / 2.0;

    let step_x = 1.0 / scale;
    let step_y = 1.0 / scale;
    let pixel_w = scale.max(1.0);
    let pixel_h = scale.max(1.0);

    let cols = (draw_w / pixel_w).ceil() as usize;
    let rows = (draw_h / pixel_h).ceil() as usize;

    for row in 0..rows {
        for col in 0..cols {
            let src_x = (col as f32 * step_x) as u32;
            let src_y = (row as f32 * step_y) as u32;
            if src_x >= fb.width || src_y >= fb.height {
                continue;
            }

            let idx = ((src_y * fb.width + src_x) * 4) as usize;
            let r = fb.data[idx];
            let g = fb.data[idx + 1];
            let b = fb.data[idx + 2];
            let a = fb.data[idx + 3];

            if a < 1e-6 {
                continue;
            }

            let rect_bounds = Bounds::new(
                point(
                    px(offset_x + col as f32 * pixel_w),
                    px(offset_y + row as f32 * pixel_h),
                ),
                size(px(pixel_w), px(pixel_h)),
            );
            let color = Hsla::from(Rgba { r, g, b, a });
            window.paint_quad(fill(rect_bounds, color));
        }
    }
}
