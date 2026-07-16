// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPUI timeline panel: ruler, layer bars, playhead.

use std::cell::Cell;
use std::rc::Rc;

use gpui::*;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{ActiveTheme, ThemeColor};
use ravel_core::composition::{Composition, Layer, LayerSource};
use ravel_core::id::{CompId, LayerId};
use ravel_core::types::{Color, FrameRate};
use ravel_i18n::t;
use ravel_ui::panels::timeline::TimelinePanel;

const RULER_HEIGHT: f32 = 24.0;
const HEADER_WIDTH: f32 = 150.0;
const LAYER_HEIGHT: f32 = 28.0;
const LAYER_BAR_CORNER_RADIUS: f32 = 4.0;
const LAYER_TEXT_PADDING: f32 = 6.0;
const PLAYHEAD_WIDTH: f32 = 2.0;

pub struct TimelineGpuiPanel {
    state: TimelinePanel,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focus_subscriptions: [Subscription; 2],
    #[allow(dead_code)]
    focused_sub: Subscription,
}

impl TimelineGpuiPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut state = TimelinePanel::new(FrameRate::new(30, 1));

        let comp = Composition::new(
            CompId::next(),
            "Main Comp",
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        )
        .add_layer(
            Layer::new(
                LayerId::next(),
                "Background",
                LayerSource::Solid {
                    color: Color::new(0.1, 0.1, 0.1, 1.0),
                    width: 1920,
                    height: 1080,
                },
            )
            .with_time(0, 0, 300),
        )
        .add_layer(
            Layer::new(
                LayerId::next(),
                "Footage A",
                LayerSource::Media {
                    asset_id: "demo.mp4".into(),
                },
            )
            .with_time(0, 0, 90),
        )
        .add_layer(
            Layer::new(
                LayerId::next(),
                "Footage B",
                LayerSource::Media {
                    asset_id: "demo2.mp4".into(),
                },
            )
            .with_time(100, 0, 60),
        );
        state.set_composition(comp);

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });
        let focus_handle = cx.focus_handle();
        let focus_subscriptions = super::track_panel_focus(
            ravel_ui::panel::PanelKind::Timeline,
            &focus_handle,
            window,
            cx,
        );
        Self {
            state,
            focus_handle,
            focus_subscriptions,
            focused_sub,
        }
    }

    fn build_ruler(
        &self,
        theme_colors: &ThemeColor,
        ruler_origin_x: Rc<Cell<Pixels>>,
    ) -> impl IntoElement {
        let state = self.state.clone();
        let colors = *theme_colors;

        canvas(
            move |bounds, _window, _cx| {
                ruler_origin_x.set(bounds.origin.x);
                state
            },
            move |bounds, state, window, _cx| {
                let ppf = state.pixels_per_frame();
                let scroll = state.scroll_offset();
                let fr = state.composition().frame_rate;
                let area_width: f32 = bounds.size.width.into();

                window.paint_quad(fill(bounds, colors.tab_bar));

                let border_bounds = Bounds::new(
                    point(
                        bounds.origin.x,
                        bounds.origin.y + bounds.size.height - px(1.0),
                    ),
                    size(bounds.size.width, px(1.0)),
                );
                window.paint_quad(fill(border_bounds, colors.border));

                let (minor_interval, major_interval) = tick_intervals(ppf, fr);
                if minor_interval == 0 || major_interval == 0 {
                    return;
                }

                let first_frame = scroll.floor().max(0.0) as u64;
                let visible_frames = (area_width as f64 / ppf).ceil() as u64 + 1;
                let last_frame = first_frame + visible_frames;
                let start = (first_frame / minor_interval) * minor_interval;

                for frame in (start..=last_frame).step_by(minor_interval as usize) {
                    let x_px = (frame as f64 - scroll) * ppf;
                    if x_px < 0.0 {
                        continue;
                    }
                    let x = bounds.origin.x + px(x_px as f32);
                    let is_major = frame % major_interval == 0;

                    let tick_h = if is_major {
                        bounds.size.height * 0.6
                    } else {
                        bounds.size.height * 0.3
                    };

                    let tick_bounds = Bounds::new(
                        point(x, bounds.origin.y + bounds.size.height - tick_h),
                        size(px(1.0), tick_h),
                    );
                    let tick_color = if is_major {
                        Hsla {
                            a: 0.6,
                            ..colors.foreground
                        }
                    } else {
                        Hsla {
                            a: 0.2,
                            ..colors.foreground
                        }
                    };
                    window.paint_quad(fill(tick_bounds, tick_color));
                }
            },
        )
        .h(px(RULER_HEIGHT))
        .w_full()
    }

    fn build_layer_area(&self, theme_colors: &ThemeColor) -> impl IntoElement {
        let state = self.state.clone();
        let colors = *theme_colors;
        let selected_layer = self.state.selected_layer();

        canvas(
            move |_bounds, _window, _cx| (state, selected_layer),
            move |bounds, (state, selected_layer), window, cx| {
                let ppf = state.pixels_per_frame();
                let scroll = state.scroll_offset();
                let area_width: f32 = bounds.size.width.into();

                window.paint_quad(fill(bounds, colors.background));

                let mut y = bounds.origin.y;
                // Layers are bottom-to-top in data, but visually we render
                // top-to-bottom in the timeline (top layer = last in vector).
                for layer in state.composition().layers.iter().rev() {
                    let lane_border = Bounds::new(
                        point(bounds.origin.x, y + px(LAYER_HEIGHT) - px(1.0)),
                        size(bounds.size.width, px(1.0)),
                    );
                    window.paint_quad(fill(lane_border, colors.border));

                    let bar_x = (layer.start_frame as f64 - scroll) * ppf;
                    let bar_w = layer.duration() as f64 * ppf;

                    if bar_x + bar_w >= 0.0 && bar_x < area_width as f64 {
                        let x = bounds.origin.x + px(bar_x.max(0.0) as f32);
                        let visible_w = if bar_x < 0.0 { bar_w + bar_x } else { bar_w };
                        let w = px(visible_w.min(area_width as f64 - bar_x.max(0.0)) as f32);

                        let bar_color = layer_color(layer, &colors);
                        let bar_bounds =
                            Bounds::new(point(x, y + px(2.0)), size(w, px(LAYER_HEIGHT - 4.0)));
                        window.paint_quad(
                            fill(bar_bounds, bar_color).corner_radii(px(LAYER_BAR_CORNER_RADIUS)),
                        );

                        if selected_layer == Some(layer.id) {
                            window.paint_quad(
                                outline(bar_bounds, colors.foreground, BorderStyle::default())
                                    .corner_radii(px(LAYER_BAR_CORNER_RADIUS))
                                    .border_widths(px(2.0)),
                            );
                        }

                        if bar_w > 40.0 {
                            let text: SharedString = layer.name.clone().into();
                            let text_len = text.len();
                            let font = Font {
                                family: SharedString::from("sans-serif"),
                                ..Default::default()
                            };
                            let shaped = window.text_system().shape_line(
                                text,
                                px(11.0),
                                &[TextRun {
                                    len: text_len,
                                    font,
                                    color: colors.accent_foreground,
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                }],
                                None,
                            );
                            let text_origin =
                                point(x + px(LAYER_TEXT_PADDING), y + px(LAYER_HEIGHT / 2.0 - 6.0));
                            shaped
                                .paint(
                                    text_origin,
                                    px(LAYER_HEIGHT),
                                    TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                )
                                .ok();
                        }
                    }

                    if layer.muted {
                        let mute_bounds = Bounds::new(
                            point(bounds.origin.x, y),
                            size(bounds.size.width, px(LAYER_HEIGHT)),
                        );
                        window.paint_quad(fill(
                            mute_bounds,
                            Hsla {
                                a: 0.5,
                                ..colors.background
                            },
                        ));
                    }

                    y += px(LAYER_HEIGHT);
                }

                // Playhead.
                let playhead_x = (state.playhead() as f64 - scroll) * ppf;
                if playhead_x >= 0.0 && (playhead_x as f32) < area_width {
                    let ph_bounds = Bounds::new(
                        point(
                            bounds.origin.x + px(playhead_x as f32 - PLAYHEAD_WIDTH / 2.0),
                            bounds.origin.y,
                        ),
                        size(px(PLAYHEAD_WIDTH), bounds.size.height),
                    );
                    window.paint_quad(fill(ph_bounds, red()));
                }
            },
        )
        .flex_grow()
        .h_full()
    }
}

impl Panel for TimelineGpuiPanel {
    fn panel_name(&self) -> &'static str {
        "timeline"
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = super::is_panel_focused(ravel_ui::panel::PanelKind::Timeline, cx);
        let color = if focused {
            cx.theme().colors.foreground
        } else {
            cx.theme().colors.muted_foreground
        };
        div()
            .text_xs()
            .text_color(color)
            .child(SharedString::from(t!("panel.timeline")))
    }
}

impl EventEmitter<PanelEvent> for TimelineGpuiPanel {}

impl Focusable for TimelineGpuiPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TimelineGpuiPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let ruler_origin_x = Rc::new(Cell::new(px(0.0)));
        let ruler = self.build_ruler(&theme.colors, ruler_origin_x.clone());
        let layer_area = self.build_layer_area(&theme.colors);

        let selected = self.state.selected_layer();
        let layer_headers = div()
            .id("layer-headers")
            .w(px(HEADER_WIDTH))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.colors.border)
            .bg(theme.colors.list)
            .children(self.state.composition().layers.iter().rev().map(|layer| {
                let is_selected = selected == Some(layer.id);
                let bg = if is_selected {
                    theme.colors.list_active
                } else {
                    theme.colors.list
                };
                let muted_indicator = if layer.muted { " [M]" } else { "" };
                let locked_indicator = if layer.locked { " [L]" } else { "" };
                let layer_id = layer.id;

                div()
                    .id(SharedString::from(format!("layer-header-{}", layer.id)))
                    .h(px(LAYER_HEIGHT))
                    .flex()
                    .items_center()
                    .px_2()
                    .gap_1()
                    .bg(bg)
                    .border_b_1()
                    .border_color(theme.colors.border)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _event, _window, cx| {
                            this.state.select_layer(Some(layer_id));
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .text_sm()
                            .text_color(theme.colors.foreground)
                            .child(SharedString::from(format!(
                                "{}{}{}",
                                layer.name, muted_indicator, locked_indicator
                            ))),
                    )
            }));

        div()
            .id("timeline-root")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.colors.border)
            .track_focus(&self.focus_handle)
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                let delta = event.delta.pixel_delta(px(20.0));
                if event.modifiers.platform || event.modifiers.control {
                    let dy: f32 = delta.y.into();
                    let factor = if dy > 0.0 { 1.2 } else { 1.0 / 1.2 };
                    let cursor_x: f32 = event.position.x.into();
                    this.state
                        .zoom_at(cursor_x as f64 - HEADER_WIDTH as f64, factor);
                } else {
                    let dx: f32 = delta.x.into();
                    let frame_delta = dx as f64 / this.state.pixels_per_frame();
                    let new_offset = this.state.scroll_offset() - frame_delta;
                    this.state.set_scroll_offset(new_offset);
                }
                cx.notify();
            }))
            .child(
                div()
                    .id("ruler-row")
                    .flex()
                    .flex_row()
                    .h(px(RULER_HEIGHT))
                    .child(div().w(px(HEADER_WIDTH)).flex_shrink_0())
                    .child(ruler)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener({
                            let ruler_origin_x = ruler_origin_x.clone();
                            move |this, event: &MouseDownEvent, _window, cx| {
                                let click_x: f32 = event.position.x.into();
                                let origin_x: f32 = ruler_origin_x.get().into();
                                let local_x = (click_x - origin_x).max(0.0) as f64;
                                let frame = this.state.x_to_frame(local_x);
                                this.state.set_playhead(frame);
                                cx.notify();
                            }
                        }),
                    )
                    .on_mouse_move(cx.listener({
                        let ruler_origin_x = ruler_origin_x.clone();
                        move |this, event: &MouseMoveEvent, _window, cx| {
                            if event.pressed_button == Some(MouseButton::Left) {
                                let drag_x: f32 = event.position.x.into();
                                let origin_x: f32 = ruler_origin_x.get().into();
                                let local_x = (drag_x - origin_x).max(0.0) as f64;
                                let frame = this.state.x_to_frame(local_x);
                                this.state.set_playhead(frame);
                                cx.notify();
                            }
                        }
                    })),
            )
            .child(
                div()
                    .flex_grow()
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .child(layer_headers)
                    .child(layer_area),
            )
    }
}

fn layer_color(layer: &Layer, colors: &ThemeColor) -> Hsla {
    match &layer.source {
        LayerSource::Solid { color, .. } => Hsla::from(Rgba {
            r: color.r,
            g: color.g,
            b: color.b,
            a: color.a.min(0.8),
        }),
        LayerSource::Media { .. } => Hsla {
            a: 0.8,
            ..colors.accent
        },
        LayerSource::PreComp { .. } => Hsla {
            h: 0.75,
            s: 0.5,
            l: 0.5,
            a: 0.8,
        },
        LayerSource::Null => Hsla {
            a: 0.3,
            ..colors.muted_foreground
        },
        _ => Hsla {
            a: 0.7,
            ..colors.accent
        },
    }
}

fn tick_intervals(ppf: f64, fr: FrameRate) -> (u64, u64) {
    let fps = fr.as_f64();
    if ppf >= 10.0 {
        (1, 5.max(fps as u64))
    } else if ppf >= 4.0 {
        (5.max(fps as u64 / 6), fps.ceil() as u64)
    } else if ppf >= 1.0 {
        (fps.ceil() as u64, (fps * 10.0).ceil() as u64)
    } else {
        ((fps * 10.0).ceil() as u64, (fps * 60.0).ceil() as u64)
    }
}
