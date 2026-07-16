// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! AE-style scrub input: drag a number label left/right to change the value.
//!
//! Three interaction modes:
//! - **Idle**: shows the formatted value with an east-west resize cursor
//! - **Scrubbing**: left-drag horizontally changes the value by `step`
//!   per `drag_pixels` movement. Cmd/Ctrl held = fine (0.1× step)
//! - **Editing**: click without drag → text field for direct numeric entry
//!   (Enter/Tab/blur commits, Escape reverts)

use gpui::*;
use gpui_component::ActiveTheme;

const DRAG_THRESHOLD: f32 = 3.0;
const PIXELS_PER_STEP: f32 = 4.0;

/// Events emitted by [`ScrubInput`].
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

    drag_state: Option<DragState>,
    editing: bool,
    edit_text: String,
    focus_handle: FocusHandle,
}

struct DragState {
    start_x: f32,
    start_value: f32,
    moved: bool,
}

impl ScrubInput {
    pub fn new(value: f32, cx: &mut Context<Self>) -> Self {
        Self {
            value,
            step: 1.0,
            min: None,
            max: None,
            precision: 2,
            suffix: "",
            drag_state: None,
            editing: false,
            edit_text: String::new(),
            focus_handle: cx.focus_handle(),
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

    pub fn value(&self) -> f32 {
        self.value
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

    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        if let Ok(v) = self.edit_text.parse::<f32>() {
            self.value = self.clamp(v);
            cx.emit(ScrubInputEvent::Change(self.value));
        }
        self.editing = false;
        self.edit_text.clear();
    }

    fn cancel_edit(&mut self) {
        self.editing = false;
        self.edit_text.clear();
    }
}

impl EventEmitter<ScrubInputEvent> for ScrubInput {}

impl Focusable for ScrubInput {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ScrubInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;

        if self.editing {
            let text = if self.edit_text.is_empty() {
                self.format_value()
            } else {
                self.edit_text.clone()
            };

            div()
                .id("scrub-edit")
                .track_focus(&self.focus_handle)
                .h(px(18.0))
                .min_w(px(48.0))
                .px_1()
                .flex()
                .items_center()
                .bg(colors.background)
                .border_1()
                .border_color(colors.accent)
                .rounded(px(2.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(colors.foreground)
                        .child(SharedString::from(text)),
                )
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _win, cx| {
                    match &event.keystroke.key {
                        key if key == "enter" || key == "tab" => {
                            this.commit_edit(cx);
                            cx.notify();
                        }
                        key if key == "escape" => {
                            this.cancel_edit();
                            cx.notify();
                        }
                        key if key.len() == 1 => {
                            let ch = key.chars().next().unwrap();
                            if ch.is_ascii_digit() || ch == '.' || ch == '-' {
                                if this.edit_text == this.format_value() {
                                    this.edit_text.clear();
                                }
                                this.edit_text.push(ch);
                                cx.notify();
                            }
                        }
                        key if key == "backspace" => {
                            if this.edit_text == this.format_value() {
                                this.edit_text.clear();
                            } else {
                                this.edit_text.pop();
                            }
                            cx.notify();
                        }
                        _ => {}
                    }
                }))
                .on_mouse_down(MouseButton::Left, |_ev, _win, _cx| {})
        } else {
            let label = self.format_value();

            div()
                .id("scrub-idle")
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
                        this.drag_state = Some(DragState {
                            start_x: x,
                            start_value: this.value,
                            moved: false,
                        });
                    }),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _win, cx| {
                    if event.pressed_button != Some(MouseButton::Left) {
                        return;
                    }
                    let Some(drag) = &mut this.drag_state else {
                        return;
                    };
                    let x: f32 = event.position.x.into();
                    let dx = x - drag.start_x;

                    if !drag.moved && dx.abs() < DRAG_THRESHOLD {
                        return;
                    }
                    drag.moved = true;

                    let multiplier = if event.modifiers.platform || event.modifiers.control {
                        0.1
                    } else {
                        1.0
                    };

                    let steps = dx / PIXELS_PER_STEP;
                    let new_val = drag.start_value + steps * this.step * multiplier;
                    this.value = this.clamp(new_val);
                    cx.emit(ScrubInputEvent::Change(this.value));
                    cx.notify();
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _event: &MouseUpEvent, window, cx| {
                        let was_drag = this.drag_state.as_ref().is_some_and(|d| d.moved);
                        this.drag_state = None;

                        if !was_drag {
                            this.editing = true;
                            this.edit_text = this.format_value();
                            this.focus_handle.focus(window, cx);
                            cx.notify();
                        }
                    }),
                )
        }
    }
}
