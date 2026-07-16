// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! AE-style scrub input: drag a number label left/right to change the value.
//!
//! Key input is handled externally via `handle_key()` — the parent panel
//! forwards `KeyDownEvent`s to the editing scrub input.

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

    editing: bool,
    edit_text: String,
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
            editing: false,
            edit_text: String::new(),
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

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Called by the parent panel to forward key events during editing.
    pub fn handle_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        if !self.editing {
            return false;
        }
        let key = keystroke.key.as_str();
        match key {
            "enter" | "return" | "tab" => {
                self.commit_edit(cx);
                cx.notify();
                true
            }
            "escape" => {
                self.editing = false;
                self.edit_text.clear();
                cx.notify();
                true
            }
            "backspace" | "delete" => {
                self.edit_text.pop();
                cx.notify();
                true
            }
            _ => {
                if let Some(ch) = &keystroke.key_char {
                    let valid = ch
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '.' || c == '-');
                    if valid && !ch.is_empty() {
                        self.edit_text.push_str(ch);
                        cx.notify();
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Cancel editing without committing.
    pub fn cancel_edit(&mut self) {
        self.editing = false;
        self.edit_text.clear();
    }

    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        if let Ok(v) = self.edit_text.parse::<f32>() {
            self.value = self.clamp(v);
            cx.emit(ScrubInputEvent::Change(self.value));
        }
        self.editing = false;
        self.edit_text.clear();
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

        if self.editing {
            let display = self.edit_text.clone();

            div()
                .id("scrub-edit")
                .h(px(18.0))
                .min_w(px(60.0))
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
                        .child(SharedString::from(format!("{display}|"))),
                )
        } else {
            let label = self.format_value();

            div()
                .id("scrub-idle")
                .h(px(18.0))
                .min_w(px(60.0))
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
                        if !this.dragging {
                            return;
                        }
                        let was_scrub = this.drag_moved;
                        this.dragging = false;
                        this.drag_moved = false;

                        if !was_scrub {
                            this.editing = true;
                            this.edit_text = this.format_value();
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
}
