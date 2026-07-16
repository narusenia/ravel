// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! AE-style scrub input: drag a number label left/right to change the value.
//! Cmd/Ctrl + drag = fine (0.1× step).

use gpui::*;
use gpui_component::ActiveTheme;

const DRAG_THRESHOLD: f32 = 3.0;
const PIXELS_PER_STEP: f32 = 4.0;

pub enum ScrubInputEvent {
    Change(f32),
}

pub struct ScrubInput {
    value: f32,
    step: f32,
    min: Option<f32>,
    max: Option<f32>,
    precision: usize,
    suffix: &'static str,

    dragging: bool,
    drag_start_x: f32,
    drag_start_value: f32,
    drag_moved: bool,
}

impl ScrubInput {
    pub fn new(value: f32, _cx: &mut Context<Self>) -> Self {
        Self {
            value,
            step: 1.0,
            min: None,
            max: None,
            precision: 2,
            suffix: "",
            dragging: false,
            drag_start_x: 0.0,
            drag_start_value: 0.0,
            drag_moved: false,
        }
    }

    pub fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    pub fn precision(mut self, p: usize) -> Self {
        self.precision = p;
        self
    }

    pub fn suffix(mut self, s: &'static str) -> Self {
        self.suffix = s;
        self
    }

    pub fn set_value(&mut self, v: f32) {
        self.value = self.clamp(v);
    }

    fn clamp(&self, v: f32) -> f32 {
        let v = self.min.map_or(v, |lo| v.max(lo));
        self.max.map_or(v, |hi| v.min(hi))
    }

    fn format_value(&self) -> String {
        format!(
            "{:.prec$}{}",
            self.value,
            self.suffix,
            prec = self.precision
        )
    }
}

impl EventEmitter<ScrubInputEvent> for ScrubInput {}

impl Render for ScrubInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let label = self.format_value();

        div()
            .id("scrub")
            .h(px(18.0))
            .min_w(px(48.0))
            .px_1()
            .flex()
            .items_center()
            .cursor(CursorStyle::ResizeLeftRight)
            .text_xs()
            .text_color(colors.foreground)
            .rounded(px(2.0))
            .hover(|s| {
                s.bg(Hsla {
                    a: 0.1,
                    ..colors.accent
                })
            })
            .child(SharedString::from(label))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _win, _cx| {
                    let x: f32 = event.position.x.into();
                    this.dragging = true;
                    this.drag_start_x = x;
                    this.drag_start_value = this.value;
                    this.drag_moved = false;
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _win, cx| {
                if !this.dragging {
                    return;
                }
                let x: f32 = event.position.x.into();
                let dx = x - this.drag_start_x;

                if !this.drag_moved && dx.abs() < DRAG_THRESHOLD {
                    return;
                }
                this.drag_moved = true;

                let multiplier = if event.modifiers.platform || event.modifiers.control {
                    0.1
                } else {
                    1.0
                };

                let steps = dx / PIXELS_PER_STEP;
                let new_val = this.drag_start_value + steps * this.step * multiplier;
                this.value = this.clamp(new_val);
                cx.emit(ScrubInputEvent::Change(this.value));
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _win, cx| {
                    if this.dragging {
                        this.dragging = false;
                        this.drag_moved = false;
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _win, cx| {
                    if this.dragging {
                        this.dragging = false;
                        this.drag_moved = false;
                        cx.notify();
                    }
                }),
            )
    }
}
