// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPUI timeline panel: ruler, track headers, clip rectangles, playhead.

use std::cell::Cell;
use std::rc::Rc;

use gpui::*;
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::{ActiveTheme, ThemeColor};
use ravel_core::timeline::{Clip, ClipId, ClipSource, Track, TrackId, TrackKind};
use ravel_core::types::FrameRate;
use ravel_i18n::t;
use ravel_ui::panels::timeline::TimelinePanel;

const RULER_HEIGHT: f32 = 24.0;
const HEADER_WIDTH: f32 = 150.0;
const CLIP_CORNER_RADIUS: f32 = 4.0;
const CLIP_TEXT_PADDING: f32 = 6.0;
const PLAYHEAD_WIDTH: f32 = 2.0;

pub struct TimelineGpuiPanel {
    state: TimelinePanel,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    focused_sub: Subscription,
}

impl TimelineGpuiPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut state = TimelinePanel::new(FrameRate::new(30, 1));

        let v_tid = TrackId::next();
        let a_tid = TrackId::next();
        let timeline = state
            .timeline()
            .clone()
            .add_track(Track::new(v_tid, "Video 1", TrackKind::Video))
            .unwrap()
            .add_track(Track::new(a_tid, "Audio 1", TrackKind::Audio))
            .unwrap()
            .add_clip(
                v_tid,
                Clip {
                    id: ClipId::next(),
                    name: "Clip A".into(),
                    source: ClipSource::Placeholder("demo.mp4".into()),
                    start_frame: 0,
                    duration_frames: 90,
                    source_in: 0,
                    source_out: 90,
                    color: None,
                },
            )
            .unwrap()
            .add_clip(
                v_tid,
                Clip {
                    id: ClipId::next(),
                    name: "Clip B".into(),
                    source: ClipSource::Placeholder("demo2.mp4".into()),
                    start_frame: 100,
                    duration_frames: 60,
                    source_in: 0,
                    source_out: 60,
                    color: Some([0.2, 0.6, 0.3, 1.0]),
                },
            )
            .unwrap()
            .add_clip(
                a_tid,
                Clip {
                    id: ClipId::next(),
                    name: "Music".into(),
                    source: ClipSource::Placeholder("bgm.wav".into()),
                    start_frame: 10,
                    duration_frames: 150,
                    source_in: 0,
                    source_out: 150,
                    color: None,
                },
            )
            .unwrap();
        state.set_timeline(timeline);

        let focused_sub = cx.observe_global::<super::FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });
        Self {
            state,
            focus_handle: cx.focus_handle(),
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
            move |bounds, state, window, cx| {
                let ppf = state.pixels_per_frame();
                let scroll = state.scroll_offset();
                let fr = state.timeline().frame_rate();
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

    fn build_clip_area(&self, theme_colors: &ThemeColor) -> impl IntoElement {
        let state = self.state.clone();
        let colors = *theme_colors;
        let selected_clip = self.state.selected_clip();

        canvas(
            move |_bounds, _window, _cx| (state, selected_clip),
            move |bounds, (state, selected_clip), window, cx| {
                let ppf = state.pixels_per_frame();
                let scroll = state.scroll_offset();
                let area_width: f32 = bounds.size.width.into();

                window.paint_quad(fill(bounds, colors.background));

                let mut y = bounds.origin.y;
                for track in state.timeline().tracks().iter() {
                    let track_h = px(track.height);

                    let lane_border = Bounds::new(
                        point(bounds.origin.x, y + track_h - px(1.0)),
                        size(bounds.size.width, px(1.0)),
                    );
                    window.paint_quad(fill(lane_border, colors.border));

                    for clip in track.clips.iter() {
                        let clip_x = (clip.start_frame as f64 - scroll) * ppf;
                        let clip_w = clip.duration_frames as f64 * ppf;

                        if clip_x + clip_w < 0.0 || clip_x > area_width as f64 {
                            continue;
                        }

                        let x = bounds.origin.x + px(clip_x.max(0.0) as f32);
                        let visible_w = if clip_x < 0.0 {
                            clip_w + clip_x
                        } else {
                            clip_w
                        };
                        let w = px(visible_w.min(area_width as f64 - clip_x.max(0.0)) as f32);

                        let clip_color = clip
                            .color
                            .map(|c| {
                                Hsla::from(Rgba {
                                    r: c[0],
                                    g: c[1],
                                    b: c[2],
                                    a: c[3],
                                })
                            })
                            .unwrap_or_else(|| Hsla {
                                a: 0.8,
                                ..colors.accent
                            });

                        let clip_bounds =
                            Bounds::new(point(x, y + px(2.0)), size(w, track_h - px(4.0)));
                        let quad =
                            fill(clip_bounds, clip_color).corner_radii(px(CLIP_CORNER_RADIUS));
                        window.paint_quad(quad);

                        if selected_clip == Some((track.id, clip.id)) {
                            let sel_quad =
                                outline(clip_bounds, colors.foreground, BorderStyle::default())
                                    .corner_radii(px(CLIP_CORNER_RADIUS))
                                    .border_widths(px(2.0));
                            window.paint_quad(sel_quad);
                        }

                        if clip_w > 40.0 {
                            let text: SharedString = clip.name.clone().into();
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
                                point(x + px(CLIP_TEXT_PADDING), y + px(track.height / 2.0 - 6.0));
                            shaped
                                .paint(text_origin, track_h, TextAlign::Left, None, window, cx)
                                .ok();
                        }
                    }

                    if track.muted {
                        let mute_bounds = Bounds::new(
                            point(bounds.origin.x, y),
                            size(bounds.size.width, track_h),
                        );
                        window.paint_quad(fill(
                            mute_bounds,
                            Hsla {
                                a: 0.5,
                                ..colors.background
                            },
                        ));
                    }

                    y += track_h;
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
        let clip_area = self.build_clip_area(&theme.colors);

        // Build track headers with context menus for add/remove.
        let selected = self.state.selected_track();
        let entity = cx.entity().downgrade();
        let track_headers = div()
            .id("track-headers")
            .w(px(HEADER_WIDTH))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.colors.border)
            .bg(theme.colors.list)
            .children(self.state.timeline().tracks().iter().map(|track| {
                let is_selected = selected == Some(track.id);
                let bg = if is_selected {
                    theme.colors.list_active
                } else {
                    theme.colors.list
                };
                let kind_label = match track.kind {
                    TrackKind::Video => "V",
                    TrackKind::Audio => "A",
                    TrackKind::Effect => "E",
                };
                let muted_indicator = if track.muted { " [M]" } else { "" };
                let locked_indicator = if track.locked { " [L]" } else { "" };
                let track_id = track.id;
                let entity = entity.clone();

                div()
                    .id(SharedString::from(format!("track-header-{}", track.id)))
                    .h(px(track.height))
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
                            this.state.select_track(Some(track_id));
                            cx.notify();
                        }),
                    )
                    .context_menu(move |menu, _window, _cx| {
                        let entity = entity.clone();
                        menu.item(
                            PopupMenuItem::new(t!("panel.timeline.remove_track")).on_click({
                                let entity = entity.clone();
                                move |_, _window, cx| {
                                    entity
                                        .update(cx, |this, cx| {
                                            if let Ok(tl) =
                                                this.state.timeline().clone().remove_track(track_id)
                                            {
                                                this.state.set_timeline(tl);
                                                cx.notify();
                                            }
                                        })
                                        .ok();
                                }
                            }),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.colors.muted_foreground)
                            .child(SharedString::from(kind_label)),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .text_sm()
                            .text_color(theme.colors.foreground)
                            .child(SharedString::from(format!(
                                "{}{}{}",
                                track.name, muted_indicator, locked_indicator
                            ))),
                    )
            }))
            // Empty area context menu for adding tracks.
            .child({
                let entity = cx.entity().downgrade();
                div()
                    .id("track-header-empty")
                    .flex_grow()
                    .min_h(px(30.0))
                    .context_menu(move |menu, _window, _cx| {
                        let entity_v = entity.clone();
                        let entity_a = entity.clone();
                        menu.item(
                            PopupMenuItem::new(t!("panel.timeline.add_video_track")).on_click(
                                move |_, _window, cx| {
                                    entity_v
                                        .update(cx, |this, cx| {
                                            let tid = TrackId::next();
                                            let count = this
                                                .state
                                                .timeline()
                                                .tracks()
                                                .iter()
                                                .filter(|t| t.kind == TrackKind::Video)
                                                .count();
                                            if let Ok(tl) =
                                                this.state.timeline().clone().add_track(Track::new(
                                                    tid,
                                                    format!("Video {}", count + 1),
                                                    TrackKind::Video,
                                                ))
                                            {
                                                this.state.set_timeline(tl);
                                                cx.notify();
                                            }
                                        })
                                        .ok();
                                },
                            ),
                        )
                        .item(
                            PopupMenuItem::new(t!("panel.timeline.add_audio_track")).on_click(
                                move |_, _window, cx| {
                                    entity_a
                                        .update(cx, |this, cx| {
                                            let tid = TrackId::next();
                                            let count = this
                                                .state
                                                .timeline()
                                                .tracks()
                                                .iter()
                                                .filter(|t| t.kind == TrackKind::Audio)
                                                .count();
                                            if let Ok(tl) =
                                                this.state.timeline().clone().add_track(Track::new(
                                                    tid,
                                                    format!("Audio {}", count + 1),
                                                    TrackKind::Audio,
                                                ))
                                            {
                                                this.state.set_timeline(tl);
                                                cx.notify();
                                            }
                                        })
                                        .ok();
                                },
                            ),
                        )
                    })
            });

        let focus = self.focus_handle.clone();
        div()
            .id("timeline-root")
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.colors.border)
            .track_focus(&self.focus_handle)
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                focus.focus(window, cx);
                cx.set_global(super::FocusedPanelGlobal(Some(
                    ravel_ui::panel::PanelKind::Timeline,
                )));
            })
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
                    .child(track_headers)
                    .child(clip_area),
            )
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
