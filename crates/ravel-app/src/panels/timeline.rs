// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! AE-style GPUI timeline panel: ruler, layer bars, solo/mute/lock,
//! property expansion rows, keyframe diamonds, playhead.

use std::cell::Cell;
use std::rc::Rc;

use gpui::*;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{ActiveTheme, ThemeColor};
use ravel_core::animation::channel::ChannelSource;
use ravel_core::composition::{Layer, LayerSource};
use ravel_core::types::FrameRate;
use ravel_i18n::t;
use ravel_ui::panels::timeline::{PropertyGroup, TimelinePanel};

const RULER_HEIGHT: f32 = 24.0;
const HEADER_WIDTH: f32 = 200.0;
const LAYER_ROW_HEIGHT: f32 = 28.0;
const PROPERTY_ROW_HEIGHT: f32 = 20.0;
const LAYER_BAR_CORNER_RADIUS: f32 = 4.0;
const LAYER_TEXT_PADDING: f32 = 6.0;
const PLAYHEAD_WIDTH: f32 = 2.0;
const TOGGLE_BUTTON_SIZE: f32 = 16.0;
const DIAMOND_SIZE: f32 = 8.0;

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
        use ravel_core::composition::{Composition, LayerSource};
        use ravel_core::id::{CompId, LayerId};
        use ravel_core::types::Color;

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
    ) -> impl IntoElement + use<> {
        let state = self.state.clone();
        let colors = *theme_colors;

        canvas(
            move |bounds, _window, _cx| {
                ruler_origin_x.set(bounds.origin.x);
                state
            },
            move |bounds, state, window, cx| {
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

                    if is_major && ppf > 0.5 {
                        let label = format_frame_label(frame, fr);
                        let text: SharedString = label.into();
                        let text_len = text.len();
                        let font = Font {
                            family: SharedString::from("sans-serif"),
                            ..Default::default()
                        };
                        let shaped = window.text_system().shape_line(
                            text,
                            px(10.0),
                            &[TextRun {
                                len: text_len,
                                font,
                                color: colors.muted_foreground,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            }],
                            None,
                        );
                        let text_origin = point(x + px(3.0), bounds.origin.y + px(2.0));
                        shaped
                            .paint(
                                text_origin,
                                bounds.size.height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            )
                            .ok();
                    }
                }
            },
        )
        .h(px(RULER_HEIGHT))
        .w_full()
    }

    fn layer_at_content_y(&self, content_y: f32) -> Option<ravel_core::id::LayerId> {
        let mut y = 0.0f32;
        for layer in self.state.composition().layers.iter().rev() {
            let next_y = y + LAYER_ROW_HEIGHT;
            if content_y >= y && content_y < next_y {
                return Some(layer.id);
            }
            y = next_y;
            if self.state.is_layer_expanded(layer.id) {
                let groups = [
                    PropertyGroup::Position,
                    PropertyGroup::Scale,
                    PropertyGroup::Rotation,
                    PropertyGroup::Opacity,
                ];
                for group in &groups {
                    y += PROPERTY_ROW_HEIGHT;
                    if self.state.is_property_expanded(layer.id, *group) {
                        y += property_channel_names(*group).len() as f32 * PROPERTY_ROW_HEIGHT;
                    }
                }
            }
        }
        None
    }

    fn total_layer_height(&self) -> f32 {
        let mut h = 0.0f32;
        for layer in self.state.composition().layers.iter() {
            h += LAYER_ROW_HEIGHT;
            if self.state.is_layer_expanded(layer.id) {
                let groups = [
                    PropertyGroup::Position,
                    PropertyGroup::Scale,
                    PropertyGroup::Rotation,
                    PropertyGroup::Opacity,
                ];
                for group in &groups {
                    h += PROPERTY_ROW_HEIGHT;
                    if self.state.is_property_expanded(layer.id, *group) {
                        h += property_channel_names(*group).len() as f32 * PROPERTY_ROW_HEIGHT;
                    }
                }
            }
        }
        h
    }

    fn build_layer_area(
        &self,
        theme_colors: &ThemeColor,
        area_origin_y: Rc<Cell<Pixels>>,
    ) -> impl IntoElement + use<> {
        let state = self.state.clone();
        let colors = *theme_colors;
        let selected_layer = self.state.selected_layer();
        let content_height = self.total_layer_height();

        canvas(
            move |bounds, _window, _cx| {
                area_origin_y.set(bounds.origin.y);
                (state, selected_layer)
            },
            move |bounds, (state, selected_layer), window, cx| {
                let ppf = state.pixels_per_frame();
                let scroll = state.scroll_offset();
                let area_width: f32 = bounds.size.width.into();

                window.paint_quad(fill(bounds, colors.background));

                let mut y = bounds.origin.y;
                for layer in state.composition().layers.iter().rev() {
                    // Layer bar row
                    let lane_border = Bounds::new(
                        point(bounds.origin.x, y + px(LAYER_ROW_HEIGHT) - px(1.0)),
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
                            Bounds::new(point(x, y + px(2.0)), size(w, px(LAYER_ROW_HEIGHT - 4.0)));
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
                            let bar_top = y + px(2.0);
                            let bar_h = LAYER_ROW_HEIGHT - 4.0;
                            paint_bar_label(
                                &layer.name,
                                x + px(LAYER_TEXT_PADDING),
                                bar_top + px(bar_h / 2.0 - 5.5),
                                px(bar_h),
                                &colors,
                                window,
                                cx,
                            );
                        }
                    }

                    if layer.muted {
                        let mute_bounds = Bounds::new(
                            point(bounds.origin.x, y),
                            size(bounds.size.width, px(LAYER_ROW_HEIGHT)),
                        );
                        window.paint_quad(fill(
                            mute_bounds,
                            Hsla {
                                a: 0.5,
                                ..colors.background
                            },
                        ));
                    }

                    y += px(LAYER_ROW_HEIGHT);

                    // Property rows (always present when layer is expanded)
                    if state.is_layer_expanded(layer.id) {
                        let props = [
                            PropertyGroup::Position,
                            PropertyGroup::Scale,
                            PropertyGroup::Rotation,
                            PropertyGroup::Opacity,
                        ];

                        for group in &props {
                            let prop_border = Bounds::new(
                                point(bounds.origin.x, y + px(PROPERTY_ROW_HEIGHT) - px(1.0)),
                                size(bounds.size.width, px(1.0)),
                            );
                            window.paint_quad(fill(
                                prop_border,
                                Hsla {
                                    a: 0.3,
                                    ..colors.border
                                },
                            ));

                            y += px(PROPERTY_ROW_HEIGHT);

                            // Channel sub-rows with keyframe diamonds
                            if state.is_property_expanded(layer.id, *group) {
                                let channels = property_channels(layer, *group);
                                for channel in channels {
                                    // Channel row border
                                    let ch_border = Bounds::new(
                                        point(
                                            bounds.origin.x,
                                            y + px(PROPERTY_ROW_HEIGHT) - px(1.0),
                                        ),
                                        size(bounds.size.width, px(1.0)),
                                    );
                                    window.paint_quad(fill(
                                        ch_border,
                                        Hsla {
                                            a: 0.15,
                                            ..colors.border
                                        },
                                    ));

                                    if let ChannelSource::Keyframes(curve) = &channel.source {
                                        for kf in curve.keyframes() {
                                            let kf_x = (kf.frame as f64 + layer.start_frame as f64
                                                - scroll)
                                                * ppf;
                                            if kf_x >= 0.0 && kf_x < area_width as f64 {
                                                paint_diamond(
                                                    bounds.origin.x + px(kf_x as f32),
                                                    y + px(PROPERTY_ROW_HEIGHT / 2.0),
                                                    &colors,
                                                    window,
                                                );
                                            }
                                        }
                                    }

                                    y += px(PROPERTY_ROW_HEIGHT);
                                }
                            }
                        }
                    }
                }

                // Playhead
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
        .h(px(content_height))
    }

    fn build_layer_headers(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme().clone();
        let selected = self.state.selected_layer();

        let mut headers = div()
            .id("layer-headers")
            .w(px(HEADER_WIDTH))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.colors.border)
            .bg(theme.colors.list);

        // Collect layer data to avoid borrow issues
        let layers: Vec<_> = self
            .state
            .composition()
            .layers
            .iter()
            .rev()
            .map(|l| (l.id, l.name.clone(), l.solo, l.muted, l.locked))
            .collect();
        let expanded_layers: Vec<_> = layers
            .iter()
            .map(|(id, ..)| self.state.is_layer_expanded(*id))
            .collect();
        let expanded_props: Vec<Vec<bool>> = layers
            .iter()
            .map(|(id, ..)| {
                [
                    PropertyGroup::Position,
                    PropertyGroup::Scale,
                    PropertyGroup::Rotation,
                    PropertyGroup::Opacity,
                ]
                .iter()
                .map(|g| self.state.is_property_expanded(*id, *g))
                .collect()
            })
            .collect();

        for (i, (layer_id, name, solo, muted, locked)) in layers.iter().enumerate() {
            let is_selected = selected == Some(*layer_id);
            let bg = if is_selected {
                theme.colors.list_active
            } else {
                theme.colors.list
            };
            let lid = *layer_id;
            let is_expanded = expanded_layers[i];

            let expand_arrow = if is_expanded { "▼" } else { "▶" };

            headers = headers.child(
                div()
                    .id(SharedString::from(format!("lh-{}", lid)))
                    .h(px(LAYER_ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .px_1()
                    .gap_1()
                    .bg(bg)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, _win, cx| {
                            this.state.select_layer(Some(lid));
                            if let Some(layer) = this.state.composition().get_layer(lid).cloned() {
                                let comp = this.state.composition();
                                cx.set_global(super::SelectedPropertiesTarget(
                                    super::PropertiesTarget::Layer {
                                        layer: Box::new(layer),
                                        frame: this.state.playhead(),
                                        fps: comp.frame_rate,
                                        resolution: comp.resolution,
                                    },
                                ));
                            }
                            cx.notify();
                        }),
                    )
                    // Expand arrow
                    .child(
                        div()
                            .id(SharedString::from(format!("exp-{}", lid)))
                            .text_xs()
                            .text_color(theme.colors.muted_foreground)
                            .cursor_pointer()
                            .child(SharedString::from(expand_arrow))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _win, cx| {
                                    this.state.toggle_layer_expanded(lid);
                                    cx.notify();
                                }),
                            ),
                    )
                    // Layer name
                    .child(
                        div()
                            .flex_grow()
                            .text_sm()
                            .text_color(theme.colors.foreground)
                            .overflow_x_hidden()
                            .child(SharedString::from(name.clone())),
                    )
                    // S/M/L toggle buttons
                    .child(
                        make_toggle(format!("s-{lid}"), "S", *solo, &theme.colors).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _win, cx| {
                                this.state.toggle_solo(lid);
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        make_toggle(format!("m-{lid}"), "M", *muted, &theme.colors).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _win, cx| {
                                this.state.toggle_mute(lid);
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        make_toggle(format!("l-{lid}"), "L", *locked, &theme.colors).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _ev, _win, cx| {
                                this.state.toggle_lock(lid);
                                cx.notify();
                            }),
                        ),
                    ),
            );

            // Property expansion sub-rows
            if is_expanded {
                let prop_labels = ["Position", "Scale", "Rotation", "Opacity"];
                let prop_groups = [
                    PropertyGroup::Position,
                    PropertyGroup::Scale,
                    PropertyGroup::Rotation,
                    PropertyGroup::Opacity,
                ];

                for (j, (label, group)) in prop_labels.iter().zip(prop_groups.iter()).enumerate() {
                    let is_prop_expanded = expanded_props[i][j];
                    let arrow = if is_prop_expanded { "▼" } else { "▶" };
                    let group = *group;

                    headers = headers.child(
                        div()
                            .id(SharedString::from(format!("prop-{lid}-{j}")))
                            .h(px(PROPERTY_ROW_HEIGHT))
                            .flex()
                            .items_center()
                            .pl(px(20.0))
                            .bg(theme.colors.list)
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _ev, _win, cx| {
                                    this.state.toggle_property_expanded(lid, group);
                                    cx.notify();
                                }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.colors.muted_foreground)
                                    .mr_1()
                                    .child(SharedString::from(arrow)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.colors.muted_foreground)
                                    .child(SharedString::from(*label)),
                            ),
                    );

                    if is_prop_expanded {
                        let channel_names = property_channel_names(group);
                        for (ci, ch_name) in channel_names.iter().enumerate() {
                            headers = headers.child(
                                div()
                                    .id(SharedString::from(format!("ch-{lid}-{j}-{ci}")))
                                    .h(px(PROPERTY_ROW_HEIGHT))
                                    .flex()
                                    .items_center()
                                    .pl(px(36.0))
                                    .bg(theme.colors.list)
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(Hsla {
                                                a: 0.6,
                                                ..theme.colors.muted_foreground
                                            })
                                            .child(SharedString::from(*ch_name)),
                                    ),
                            );
                        }
                    }
                }
            }
        }

        headers
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
        let layer_area_origin = Rc::new(Cell::new(px(0.0)));
        let layer_area = self.build_layer_area(&theme.colors, layer_area_origin.clone());
        let layer_headers = self.build_layer_headers(cx);

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
                    .id("layer-scroll-area")
                    .flex_grow()
                    .flex()
                    .flex_row()
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .child(layer_headers)
                    .child(
                        div()
                            .id("layer-area-click")
                            .flex_grow()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener({
                                    let layer_area_origin = layer_area_origin.clone();
                                    move |this, event: &MouseDownEvent, _win, cx| {
                                        let click_y: f32 = event.position.y.into();
                                        let origin_y: f32 = layer_area_origin.get().into();
                                        let content_y = click_y - origin_y;
                                        if let Some(lid) = this.layer_at_content_y(content_y) {
                                            this.state.select_layer(Some(lid));
                                            let layer =
                                                this.state.composition().get_layer(lid).cloned();
                                            if let Some(layer) = layer {
                                                let comp = this.state.composition();
                                                cx.set_global(super::SelectedPropertiesTarget(
                                                    super::PropertiesTarget::Layer {
                                                        layer: Box::new(layer),
                                                        frame: this.state.playhead(),
                                                        fps: comp.frame_rate,
                                                        resolution: comp.resolution,
                                                    },
                                                ));
                                            }
                                            cx.notify();
                                        }
                                    }
                                }),
                            )
                            .child(layer_area),
                    ),
            )
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

fn make_toggle(id: String, label: &str, active: bool, colors: &ThemeColor) -> Stateful<Div> {
    let text_color = if active {
        colors.accent
    } else {
        Hsla {
            a: 0.4,
            ..colors.muted_foreground
        }
    };
    div()
        .id(SharedString::from(id))
        .w(px(TOGGLE_BUTTON_SIZE))
        .h(px(TOGGLE_BUTTON_SIZE))
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(text_color)
        .cursor_pointer()
        .child(SharedString::from(label))
}

fn paint_bar_label(
    text: &str,
    x: Pixels,
    y: Pixels,
    max_h: Pixels,
    colors: &ThemeColor,
    window: &mut Window,
    cx: &mut App,
) {
    let text: SharedString = text.into();
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
    shaped
        .paint(point(x, y), max_h, TextAlign::Left, None, window, cx)
        .ok();
}

fn paint_diamond(cx_pos: Pixels, cy: Pixels, colors: &ThemeColor, window: &mut Window) {
    let half = DIAMOND_SIZE / 2.0;
    let diamond = Bounds::new(
        point(cx_pos - px(half), cy - px(half)),
        size(px(DIAMOND_SIZE), px(DIAMOND_SIZE)),
    );
    window.paint_quad(
        fill(diamond, colors.accent)
            .corner_radii(px(1.0))
            .border_widths(px(0.0)),
    );
}

fn property_channels(
    layer: &Layer,
    group: PropertyGroup,
) -> Vec<&ravel_core::animation::channel::AnimationChannel> {
    match group {
        PropertyGroup::Position => vec![&layer.transform.position[0], &layer.transform.position[1]],
        PropertyGroup::Scale => vec![&layer.transform.scale[0], &layer.transform.scale[1]],
        PropertyGroup::Rotation => vec![&layer.transform.rotation],
        PropertyGroup::Opacity => vec![&layer.opacity],
        PropertyGroup::AnchorPoint => {
            vec![
                &layer.transform.anchor_point[0],
                &layer.transform.anchor_point[1],
            ]
        }
    }
}

fn property_channel_names(group: PropertyGroup) -> &'static [&'static str] {
    match group {
        PropertyGroup::Position => &["X", "Y"],
        PropertyGroup::Scale => &["X", "Y"],
        PropertyGroup::Rotation => &["Rotation"],
        PropertyGroup::Opacity => &["Opacity"],
        PropertyGroup::AnchorPoint => &["X", "Y"],
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

fn format_frame_label(frame: u64, fr: FrameRate) -> String {
    let fps = fr.as_f64();
    let total_seconds = frame as f64 / fps;
    let minutes = (total_seconds / 60.0).floor() as u64;
    let seconds = (total_seconds % 60.0).floor() as u64;
    let remaining_frames = frame % fps.ceil() as u64;
    if minutes > 0 {
        format!("{minutes}:{seconds:02}:{remaining_frames:02}")
    } else {
        format!("{seconds}:{remaining_frames:02}")
    }
}
