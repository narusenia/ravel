// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! AE-style scrub input: drag a numeric label horizontally to change its
//! value, or click (without dragging) to type an exact value.
//!
//! Scrub sensitivity derives from the UI range (dragging ~200px sweeps the
//! whole span); clamping uses the hard range, so scrubbing can exceed the UI
//! span up to the true limits. Shift = coarse (10×), Cmd/Ctrl = fine (0.1×).
//!
//! Typing goes through `gpui_component::input::Input`, whose `InputState`
//! implements `EntityInputHandler` — the proper text path that works where
//! raw `on_key_down` does not (issue #41). Enter or focus loss commits the
//! typed value (parsed, clamped); unparsable text reverts.
//!
//! Events: [`ScrubEvent::Change`] fires live during the drag (callers apply
//! it without pushing undo); [`ScrubEvent::Commit`] fires once on release or
//! typed-commit (callers record undo there).

use std::ops::RangeInclusive;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme, Sizable as _};

/// Horizontal pixels that sweep the full UI range.
const PIXELS_PER_UI_SPAN: f32 = 200.0;
/// Fallback UI span when a field declares no range.
const DEFAULT_UI_SPAN: f32 = 20.0;

pub enum ScrubEvent {
    /// Live value while dragging. Apply, but do not record undo.
    Change(f32),
    /// Final value on drag end. Record undo here.
    Commit(f32),
}

/// Pure scrub math: value after dragging `dx` pixels from `start_value`.
///
/// `ui_span` controls sensitivity, `hard` clamps the result. `coarse`
/// (Shift) multiplies by 10, `fine` (Cmd/Ctrl) by 0.1.
pub fn scrub_value(
    start_value: f32,
    dx: f32,
    ui_span: f32,
    hard: Option<&RangeInclusive<f32>>,
    coarse: bool,
    fine: bool,
) -> f32 {
    let span = if ui_span > 0.0 {
        ui_span
    } else {
        DEFAULT_UI_SPAN
    };
    let mut per_pixel = span / PIXELS_PER_UI_SPAN;
    if coarse {
        per_pixel *= 10.0;
    }
    if fine {
        per_pixel *= 0.1;
    }
    let value = start_value + dx * per_pixel;
    match hard {
        Some(range) => value.clamp(*range.start(), *range.end()),
        None => value,
    }
}

/// Drag payload identifying the scrub drag by its owning entity.
#[derive(Clone)]
struct DragScrub(EntityId);

impl Render for DragScrub {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

pub struct ScrubInputState {
    value: f32,
    hard_range: Option<RangeInclusive<f32>>,
    ui_span: f32,
    /// Round emitted values to integers (Int parameters).
    integer: bool,
    precision: usize,

    dragging: bool,
    drag_start_x: f32,
    drag_start_value: f32,
    changed_in_drag: bool,

    /// Text editor shown after a click without drag movement.
    editor: Option<Entity<InputState>>,
    editor_sub: Option<Subscription>,
}

impl ScrubInputState {
    pub fn new(value: f32) -> Self {
        Self {
            value,
            hard_range: None,
            ui_span: DEFAULT_UI_SPAN,
            integer: false,
            precision: 2,
            dragging: false,
            drag_start_x: 0.0,
            drag_start_value: 0.0,
            changed_in_drag: false,
            editor: None,
            editor_sub: None,
        }
    }

    pub fn hard_range(mut self, range: Option<RangeInclusive<f32>>) -> Self {
        self.hard_range = range;
        self
    }

    pub fn ui_range(mut self, range: Option<RangeInclusive<f32>>) -> Self {
        if let Some(r) = range {
            self.ui_span = (r.end() - r.start()).abs();
        }
        self
    }

    pub fn integer(mut self, integer: bool) -> Self {
        self.integer = integer;
        if integer {
            self.precision = 0;
        }
        self
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    /// Whether a scrub drag is in progress (external value refreshes must
    /// not fight the gesture).
    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    pub fn set_value(&mut self, value: f32) {
        self.value = self.quantize(self.clamp(value));
    }

    fn clamp(&self, value: f32) -> f32 {
        match &self.hard_range {
            Some(range) => value.clamp(*range.start(), *range.end()),
            None => value,
        }
    }

    fn quantize(&self, value: f32) -> f32 {
        if self.integer { value.round() } else { value }
    }

    fn begin_drag(&mut self, x: f32) {
        self.dragging = true;
        self.drag_start_x = x;
        self.drag_start_value = self.value;
        self.changed_in_drag = false;
    }

    fn drag_to(&mut self, x: f32, modifiers: &Modifiers, cx: &mut Context<Self>) {
        if !self.dragging {
            return;
        }
        let dx = x - self.drag_start_x;
        let raw = scrub_value(
            self.drag_start_value,
            dx,
            self.ui_span,
            self.hard_range.as_ref(),
            modifiers.shift,
            modifiers.platform || modifiers.control,
        );
        let next = self.quantize(raw);
        if (next - self.value).abs() > f32::EPSILON {
            self.value = next;
            self.changed_in_drag = true;
            cx.emit(ScrubEvent::Change(next));
            cx.notify();
        }
    }

    /// Ends the drag phase and reports whether a net change should commit.
    ///
    /// Returns `Some(moved)` when a drag was active: `moved` is true if the
    /// pointer scrubbed at all. A drag that returns to its start value emits
    /// no Commit — the live Change events already restored the start value,
    /// so committing would only record a no-op undo snapshot.
    fn end_drag(&mut self, cx: &mut Context<Self>) -> Option<bool> {
        if !self.dragging {
            return None;
        }
        self.dragging = false;
        let moved = self.changed_in_drag;
        self.changed_in_drag = false;
        if moved && (self.value - self.drag_start_value).abs() > f32::EPSILON {
            cx.emit(ScrubEvent::Commit(self.value));
        }
        cx.notify();
        Some(moved)
    }

    /// Ends a drag. A release without any drag movement counts as a click
    /// and — when `may_edit` (mouse-up inside the widget) — opens the text
    /// editor.
    fn release(&mut self, may_edit: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(moved) = self.end_drag(cx)
            && !moved
            && may_edit
        {
            self.begin_edit(window, cx);
        }
    }

    fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_some() {
            return;
        }
        let text = self.label();
        let editor = cx.new(|cx| InputState::new(window, cx).default_value(text));
        editor.update(cx, |state, cx| state.focus(window, cx));
        // Select the whole value so typing replaces it (AE behavior). The
        // action must dispatch after the Input has rendered into the tree.
        cx.defer_in(window, |_this, window, cx| {
            window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
        });

        let sub = cx.subscribe(
            &editor,
            |this: &mut Self, _state, event: &InputEvent, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_edit(cx),
                _ => {}
            },
        );
        self.editor = Some(editor);
        self.editor_sub = Some(sub);
        cx.notify();
    }

    /// Parses the typed text, clamps it to the hard range, and commits.
    /// Unparsable text reverts to the previous value.
    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        self.editor_sub = None;

        let text = editor.read(cx).value().to_string();
        if let Ok(parsed) = text.trim().parse::<f32>() {
            let next = self.quantize(self.clamp(parsed));
            if (next - self.value).abs() > f32::EPSILON {
                self.value = next;
                cx.emit(ScrubEvent::Commit(next));
            }
        }
        cx.notify();
    }

    fn label(&self) -> String {
        format!("{:.prec$}", self.value, prec = self.precision)
    }
}

impl EventEmitter<ScrubEvent> for ScrubInputState {}

/// The scrub input element. Rebuilt each frame from its state entity.
#[derive(IntoElement)]
pub struct ScrubInput {
    state: Entity<ScrubInputState>,
}

impl ScrubInput {
    pub fn new(state: &Entity<ScrubInputState>) -> Self {
        Self {
            state: state.clone(),
        }
    }
}

impl RenderOnce for ScrubInput {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let entity_id = self.state.entity_id();
        let state = self.state.read(cx);
        let label = state.label();
        let dragging = state.dragging;
        let editor = state.editor.clone();
        let colors = cx.theme().colors;

        // Edit mode: show a focused text input in place of the label.
        if let Some(editor) = editor {
            return div()
                .id(("scrub-input-edit", entity_id))
                .h(px(16.0))
                .min_w(px(48.0))
                .child(Input::new(&editor).small())
                .into_any_element();
        }

        div()
            .id(("scrub-input", entity_id))
            .h(px(16.0))
            .min_w(px(48.0))
            .px_1()
            .flex()
            .items_center()
            .justify_end()
            .rounded(px(2.0))
            .cursor(CursorStyle::ResizeLeftRight)
            .text_xs()
            .text_color(if dragging {
                colors.accent_foreground
            } else {
                colors.foreground
            })
            .when(dragging, |this| this.bg(colors.accent))
            .hover(|this| {
                this.bg(Hsla {
                    a: 0.15,
                    ..colors.accent
                })
            })
            .child(SharedString::from(label))
            .on_mouse_down(
                MouseButton::Left,
                window.listener_for(&self.state, |state, e: &MouseDownEvent, _window, cx| {
                    state.begin_drag(e.position.x.into());
                    cx.notify();
                }),
            )
            .on_drag(DragScrub(entity_id), |drag, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| drag.clone())
            })
            .on_drag_move(window.listener_for(
                &self.state,
                move |state, e: &DragMoveEvent<DragScrub>, _window, cx| {
                    let DragScrub(id) = e.drag(cx);
                    if *id != entity_id {
                        return;
                    }
                    let modifiers = e.event.modifiers;
                    state.drag_to(e.event.position.x.into(), &modifiers, cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                window.listener_for(&self.state, |state, _e: &MouseUpEvent, window, cx| {
                    state.release(true, window, cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                window.listener_for(&self.state, |state, _e: &MouseUpEvent, window, cx| {
                    state.release(false, window, cx);
                }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Selective import: `use super::*` would pull in `gpui::test` and hijack
    // the built-in `#[test]` attribute (recursive expansion).
    use super::{DEFAULT_UI_SPAN, PIXELS_PER_UI_SPAN, ScrubEvent, ScrubInputState, scrub_value};
    use gpui::{AppContext as _, Modifiers, TestAppContext};
    use std::cell::RefCell;
    use std::rc::Rc;

    type EventLog = Rc<RefCell<Vec<f32>>>;

    /// Drives a drag through the state entity and records emitted events.
    fn record_drag(cx: &mut TestAppContext, positions: &[f32]) -> (EventLog, EventLog) {
        let state = cx.new(|_| ScrubInputState::new(5.0).ui_range(Some(0.0..=20.0)));
        let changes: Rc<RefCell<Vec<f32>>> = Rc::default();
        let commits: Rc<RefCell<Vec<f32>>> = Rc::default();

        let (changes_out, commits_out) = (changes.clone(), commits.clone());
        cx.update(|cx| {
            cx.subscribe(&state, move |_state, event: &ScrubEvent, _cx| match event {
                ScrubEvent::Change(v) => changes.borrow_mut().push(*v),
                ScrubEvent::Commit(v) => commits.borrow_mut().push(*v),
            })
            .detach();
        });

        state.update(cx, |state, cx| {
            state.begin_drag(100.0);
            for x in positions {
                state.drag_to(*x, &Modifiers::default(), cx);
            }
            state.end_drag(cx);
        });
        (changes_out, commits_out)
    }

    #[gpui::test]
    fn drag_emits_live_changes_and_one_commit(cx: &mut TestAppContext) {
        let (changes, commits) = record_drag(cx, &[150.0, 200.0]);
        assert_eq!(changes.borrow().len(), 2);
        assert_eq!(commits.borrow().len(), 1, "exactly one commit per gesture");
        assert!((commits.borrow()[0] - 15.0).abs() < 1e-4);
    }

    #[gpui::test]
    fn drag_back_to_start_emits_no_commit(cx: &mut TestAppContext) {
        // Scrub away and return to the starting position: live changes fire
        // but no commit (and therefore no undo snapshot) is recorded.
        let (changes, commits) = record_drag(cx, &[150.0, 100.0]);
        assert_eq!(changes.borrow().len(), 2);
        assert!(
            commits.borrow().is_empty(),
            "no net change → no commit: {:?}",
            commits.borrow()
        );
    }

    #[test]
    fn scrub_full_ui_span_over_reference_distance() {
        // Dragging PIXELS_PER_UI_SPAN pixels sweeps exactly one UI span.
        let v = scrub_value(0.0, PIXELS_PER_UI_SPAN, 50.0, None, false, false);
        assert!((v - 50.0).abs() < 1e-4);
    }

    #[test]
    fn scrub_negative_drag_decreases() {
        let v = scrub_value(10.0, -100.0, 20.0, None, false, false);
        assert!((v - 0.0).abs() < 1e-4);
    }

    #[test]
    fn scrub_clamps_to_hard_range() {
        let hard = 0.0..=10.0;
        let v = scrub_value(5.0, 10_000.0, 20.0, Some(&hard), false, false);
        assert!((v - 10.0).abs() < 1e-6);
        let v = scrub_value(5.0, -10_000.0, 20.0, Some(&hard), false, false);
        assert!(v.abs() < 1e-6);
    }

    #[test]
    fn scrub_can_exceed_ui_span_within_hard_range() {
        // UI span 10 but hard range much wider: a long drag passes the UI span.
        let hard = 0.0..=1000.0;
        let v = scrub_value(0.0, 400.0, 10.0, Some(&hard), false, false);
        assert!((v - 20.0).abs() < 1e-4, "400px at 10/200 per px = 20: {v}");
    }

    #[test]
    fn scrub_modifiers_scale_sensitivity() {
        let base = scrub_value(0.0, 100.0, 20.0, None, false, false);
        let coarse = scrub_value(0.0, 100.0, 20.0, None, true, false);
        let fine = scrub_value(0.0, 100.0, 20.0, None, false, true);
        assert!((coarse - base * 10.0).abs() < 1e-4);
        assert!((fine - base * 0.1).abs() < 1e-4);
    }

    #[test]
    fn scrub_zero_span_falls_back_to_default() {
        let v = scrub_value(0.0, PIXELS_PER_UI_SPAN, 0.0, None, false, false);
        assert!((v - DEFAULT_UI_SPAN).abs() < 1e-4);
    }

    #[test]
    fn state_set_value_clamps_and_quantizes() {
        let mut state = ScrubInputState::new(0.0)
            .hard_range(Some(0.0..=100.0))
            .integer(true);
        state.set_value(150.7);
        assert_eq!(state.value(), 100.0);
        state.set_value(42.4);
        assert_eq!(state.value(), 42.0);
    }
}
