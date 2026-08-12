// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Inline editor for [`RampParam`] colour ramps.
//!
//! The Properties panel folds a ramp parameter into one row with a gradient
//! band and expands this editor underneath it, exactly as it does for a curve
//! ([`super::param_curve_editor`]). Stops are added, moved, recoloured and
//! removed here; the host owns the document write and the undo granularity
//! (live [`ParamRampEvent::Change`] during a drag, one
//! [`ParamRampEvent::Commit`] per gesture — the same contract a scrub and the
//! curve editor follow).
//!
//! # Why this is not the curve editor
//!
//! A ramp has no output axis: its stops are keyed by position and carry a
//! colour, not a number, so there is no view box to fit, no vertical zoom, and
//! no [`CurveValueRange`](super::curve_view::CurveValueRange) to share. The
//! position axis is the fixed `0..=1` the band is drawn over. What *is* shared
//! is the rule that makes both types safe to drag —
//! [`clamp_between`](super::param_curve_editor::clamp_between), which keeps a
//! dragged element strictly between its neighbours so two never collapse onto
//! one key — and the gesture and expansion contracts, which the panel owns.
//!
//! # Colour
//!
//! Stops hold working-space linear light like every other colour in the core
//! (`CM-2`); the band, the markers and the swatch are displays, so they are
//! encoded for display on the way out ([`display_hsla`]). The picker that
//! edits the selected stop lives in the panel beside this element, for the
//! same reason every other `ColorPicker` does: it needs a `Window` to be
//! created and refreshed.

use std::cell::Cell;
use std::rc::Rc;

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::tooltip::Tooltip;
use ravel_core::color::ColorSpace;
use ravel_core::param_ramp::{RampInterpolation, RampParam, RampStop};
use ravel_core::types::Color;
use ravel_i18n::t;

use super::param_curve_editor::clamp_between;
use super::scrub_input::{ScrubEvent, ScrubInput, ScrubInputState};
use crate::assets::RavelIcon;

/// Pointer distance (widget pixels, horizontal) that still counts as grabbing
/// a stop.
pub const HIT_RADIUS: f32 = 7.0;
/// Height of the marker strip along the bottom of the band.
const MARKER_STRIP: f32 = 12.0;
/// Half-width of a stop marker.
const MARKER_HALF: f32 = 4.0;
/// Upper bound on the number of quads the band is painted with.
const MAX_BAND_SAMPLES: usize = 512;
/// Width of the toolbar's position field.
const FIELD_WIDTH: f32 = 52.0;

/// Live value while a stop is being dragged. Apply it, but do not record undo.
///
/// [`ParamRampEvent::Commit`] ends the gesture and is where the host records
/// exactly one undo step.
pub enum ParamRampEvent {
    Change(RampParam),
    Commit(RampParam),
}

/// Widget bounds shared between paint and the mouse handlers, on the same
/// terms as the curve editor's: paint is the only place the element's bounds
/// are known, and a `Cell` keeps painting free of entity updates.
type SharedBounds = Rc<Cell<(f32, f32, f32, f32)>>;

/// Immutable state captured when a stop drag starts.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RampDrag {
    /// Position the dragged stop currently sits at (it moves as the drag
    /// applies, so this tracks the live identity).
    current: f32,
    origin: RampStop,
    pointer_start: f32,
    /// Widget width when the gesture started, so the drag keeps its scale
    /// even if the row is resized mid-gesture.
    width: f32,
    /// Exclusive bounds from the neighbours at drag start. Stops are
    /// identified by their position, so a drag must never make two share one.
    lower: Option<f32>,
    upper: Option<f32>,
}

/// A working-space colour as the display-referred `Hsla` a widget paints with.
///
/// Ramp stops are linear light (`CM-2`); painting them raw would show every
/// gradient far darker than it renders.
pub fn display_hsla(color: Color) -> Hsla {
    let display = ColorSpace::DISPLAY.from_linear([color.r, color.g, color.b]);
    Hsla::from(Rgba {
        r: display[0],
        g: display[1],
        b: display[2],
        a: color.a,
    })
}

pub struct ParamRampEditorState {
    ramp: RampParam,
    /// Position of the selected stop, if any. Selection is view state: it
    /// drives the position field, the colour picker beside the editor and the
    /// highlighted marker. **Never in the Document, so outside undo.**
    selected: Option<f32>,
    drag: Option<RampDrag>,
    /// Whether the live drag has moved the stop at all (a drag that never
    /// moved must not record an undo step).
    moved_in_drag: bool,
    bounds: SharedBounds,
    position: Entity<ScrubInputState>,
    /// Kept for the lifetime of the state, which owns the input above.
    #[allow(dead_code)]
    input_subs: Vec<Subscription>,
}

impl ParamRampEditorState {
    pub fn new(ramp: RampParam, cx: &mut Context<Self>) -> Self {
        let position = cx.new(|_| ScrubInputState::new(0.0).hard_range(Some(0.0..=1.0)));
        let input_subs =
            vec![
                cx.subscribe(&position, move |this, _state, event: &ScrubEvent, cx| {
                    let (value, commit) = match event {
                        ScrubEvent::Change(value) => (*value, false),
                        ScrubEvent::Commit(value) => (*value, true),
                    };
                    this.set_selected_position(value, commit, cx);
                }),
            ];
        let mut state = Self {
            ramp,
            selected: None,
            drag: None,
            moved_in_drag: false,
            bounds: Rc::new(Cell::new((0.0, 0.0, 0.0, 0.0))),
            position,
            input_subs,
        };
        state.sync_inputs(cx);
        state
    }

    pub fn ramp(&self) -> &RampParam {
        &self.ramp
    }

    /// The selected stop, if it is still in the ramp.
    pub fn selected_stop(&self) -> Option<RampStop> {
        let position = self.selected?;
        self.ramp
            .stops()
            .iter()
            .find(|stop| stop.position.total_cmp(&position).is_eq())
            .copied()
    }

    /// Whether a drag is in progress (external refreshes must not fight the
    /// gesture).
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Replace the displayed ramp from the document. Ignored mid-gesture: the
    /// drag is the source of truth until it ends.
    pub fn set_ramp(&mut self, ramp: RampParam) {
        if self.is_dragging() {
            return;
        }
        self.ramp = ramp;
        // A selection the new ramp no longer contains is dropped, so the
        // picker never edits a stop that is gone.
        if self.selected_stop().is_none() {
            self.selected = None;
        }
    }

    /// [`set_ramp`](Self::set_ramp) plus a refresh of the position field.
    pub fn set_ramp_synced(&mut self, ramp: RampParam, cx: &mut Context<Self>) {
        if self.is_dragging() {
            return;
        }
        self.set_ramp(ramp);
        self.sync_inputs(cx);
    }

    /// Push the live position into the idle field. A field being scrubbed owns
    /// its value until the gesture ends, exactly as the Properties panel
    /// treats its own scrub inputs.
    fn sync_inputs(&mut self, cx: &mut Context<Self>) {
        self.write_inputs(false, cx);
    }

    /// Put the last accepted value back after an edit was refused, overriding
    /// even a field mid-gesture — that field is the one holding the rejected
    /// value.
    fn restore_inputs(&mut self, cx: &mut Context<Self>) {
        self.write_inputs(true, cx);
    }

    fn write_inputs(&mut self, force: bool, cx: &mut Context<Self>) {
        let Some(stop) = self.selected_stop() else {
            return;
        };
        self.position.update(cx, |input, cx| {
            if (force || !input.is_dragging()) && input.value() != stop.position {
                input.set_value(stop.position);
                cx.notify();
            }
        });
    }

    /// Test hook: paint is what normally records the element's bounds, so a
    /// headless test has to supply them before driving the pointer.
    #[cfg(test)]
    pub(crate) fn set_bounds_for_tests(&self, origin: (f32, f32), size: (f32, f32)) {
        self.bounds.set((origin.0, origin.1, size.0, size.1));
    }

    fn width(&self) -> f32 {
        self.bounds.get().2
    }

    /// Widget-space x from a window-space pointer position.
    fn local_x(&self, position: Point<Pixels>) -> f32 {
        f32::from(position.x) - self.bounds.get().0
    }

    /// The ramp position under widget x, clamped to the band.
    fn position_at(&self, x: f32) -> f32 {
        let width = self.width();
        if width <= 0.0 {
            return 0.0;
        }
        (x / width).clamp(0.0, 1.0)
    }

    /// Widget x of a ramp position. Positions outside `0..=1` — which a
    /// hand-edited file or another consumer may hold — are pinned to the band
    /// ends rather than painted off the element.
    fn x_of(&self, position: f32) -> f32 {
        position.clamp(0.0, 1.0) * self.width()
    }

    /// Position of the stop nearest `x` within [`HIT_RADIUS`], closest first.
    fn hit_stop(&self, x: f32) -> Option<f32> {
        let mut best: Option<(f32, f32)> = None;
        for stop in self.ramp.stops() {
            let distance = (self.x_of(stop.position) - x).abs();
            if distance <= HIT_RADIUS && best.is_none_or(|(current, _)| distance < current) {
                best = Some((distance, stop.position));
            }
        }
        best.map(|(_, position)| position)
    }

    /// The neighbours of the stop at `position`, as exclusive drag bounds.
    fn neighbours(&self, position: f32) -> (Option<f32>, Option<f32>) {
        let stops = self.ramp.stops();
        let Some(index) = stops
            .iter()
            .position(|stop| stop.position.total_cmp(&position).is_eq())
        else {
            return (None, None);
        };
        (
            index.checked_sub(1).map(|i| stops[i].position),
            stops.get(index + 1).map(|stop| stop.position),
        )
    }

    /// Left-button press: a second click adds a stop (or removes the one under
    /// the pointer), a first click selects the stop it grabbed and starts
    /// dragging it — the same gesture vocabulary the curve editor uses.
    pub(crate) fn pointer_down(&mut self, x: f32, click_count: usize, cx: &mut Context<Self>) {
        if self.width() <= 0.0 {
            return;
        }
        let hit = self.hit_stop(x);
        if click_count >= 2 {
            match hit {
                Some(position) => self.remove_stop(position, cx),
                None => self.insert_stop(self.position_at(x), cx),
            }
            return;
        }
        let Some(position) = hit else {
            // A press on empty space clears the selection, so the picker
            // beside the editor stops describing a stop nobody is editing.
            self.selected = None;
            cx.notify();
            return;
        };
        self.selected = Some(position);
        self.sync_inputs(cx);
        let (lower, upper) = self.neighbours(position);
        self.drag = self
            .ramp
            .stops()
            .iter()
            .find(|stop| stop.position.total_cmp(&position).is_eq())
            .map(|stop| RampDrag {
                current: position,
                origin: *stop,
                pointer_start: x,
                width: self.width(),
                lower,
                upper,
            });
        self.moved_in_drag = false;
        cx.notify();
    }

    /// Move the dragged stop to widget x.
    ///
    /// The move keeps the grab offset (the stop follows the pointer's delta,
    /// not its absolute position), stays inside the band, and stays strictly
    /// between the neighbours it started between. When the neighbours leave no
    /// room at all the stop keeps the position it has: refusing the move is
    /// the one outcome that cannot merge two stops.
    pub(crate) fn drag_to(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else {
            return;
        };
        if drag.width <= 0.0 {
            return;
        }
        let delta = (x - drag.pointer_start) / drag.width;
        let target = (drag.origin.position + delta).clamp(0.0, 1.0);
        let target = clamp_between(target, drag.lower, drag.upper).unwrap_or(drag.current);
        if !self.ramp.move_stop(drag.current, target) {
            return;
        }
        if self
            .selected
            .is_some_and(|selected| selected.total_cmp(&drag.current).is_eq())
        {
            self.selected = Some(target);
        }
        if let Some(drag) = self.drag.as_mut() {
            drag.current = target;
        }
        self.moved_in_drag = true;
        self.sync_inputs(cx);
        cx.emit(ParamRampEvent::Change(self.ramp.clone()));
        cx.notify();
    }

    /// Ends the drag phase and reports whether it emitted a `Commit`, so a
    /// caller that has to take that undo step itself knows there is one.
    pub(crate) fn end_drag(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(drag) = self.drag.take() else {
            return false;
        };
        let moved = self.moved_in_drag;
        self.moved_in_drag = false;
        // A gesture that returned to where it started emitted live Changes
        // that already restored the original ramp; committing would only
        // record a no-op undo step.
        let settled = drag.current.total_cmp(&drag.origin.position).is_eq();
        let committed = moved && !settled;
        if committed {
            cx.emit(ParamRampEvent::Commit(self.ramp.clone()));
        } else if moved {
            cx.emit(ParamRampEvent::Change(self.ramp.clone()));
        }
        cx.notify();
        committed
    }

    /// Abandon the drag without committing, for the case where the pointer
    /// arrives with the button already released (a button lost outside the
    /// window). The live value stays where the last `Change` put it, which is
    /// the same place `end_drag` would leave it.
    pub(crate) fn cancel_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.is_none() {
            return;
        }
        self.end_drag(cx);
    }

    /// Move the selected stop from the toolbar's position field.
    ///
    /// A non-finite value is refused outright and the field rolled back:
    /// `RampParam` orders its stops by position and cannot order a `NaN`.
    pub(crate) fn set_selected_position(
        &mut self,
        value: f32,
        commit: bool,
        cx: &mut Context<Self>,
    ) {
        if !value.is_finite() {
            self.restore_inputs(cx);
            return;
        }
        let Some(stop) = self.selected_stop() else {
            return;
        };
        let (lower, upper) = self.neighbours(stop.position);
        let Some(position) = clamp_between(value.clamp(0.0, 1.0), lower, upper) else {
            return;
        };
        if !self.ramp.move_stop(stop.position, position) {
            return;
        }
        self.selected = Some(position);
        if commit {
            cx.emit(ParamRampEvent::Commit(self.ramp.clone()));
        } else {
            cx.emit(ParamRampEvent::Change(self.ramp.clone()));
        }
        cx.notify();
    }

    /// Recolour the selected stop, returning the edited ramp.
    ///
    /// Nothing is emitted: the colour picker lives in the panel (it needs a
    /// `Window`), and the panel routes the edit through the same debounced
    /// commit every other picker uses. Returning the ramp is what it routes.
    pub fn set_selected_color(
        &mut self,
        color: Color,
        cx: &mut Context<Self>,
    ) -> Option<RampParam> {
        let stop = self.selected_stop()?;
        if !self.ramp.set_stop_color(stop.position, color) {
            return None;
        }
        cx.notify();
        Some(self.ramp.clone())
    }

    /// Switch how the spans between stops are filled. One click, one undo step.
    pub(crate) fn set_interpolation(
        &mut self,
        interpolation: RampInterpolation,
        cx: &mut Context<Self>,
    ) {
        if self.ramp.interpolation() == interpolation {
            return;
        }
        self.ramp.set_interpolation(interpolation);
        cx.emit(ParamRampEvent::Commit(self.ramp.clone()));
        cx.notify();
    }

    /// Add a stop at `position`, coloured with what the ramp already gives
    /// there, so a double-click adds a stop without changing the gradient.
    fn insert_stop(&mut self, position: f32, cx: &mut Context<Self>) {
        let color = self.ramp.evaluate(position);
        if !self.ramp.insert_stop(RampStop::new(position, color)) {
            return;
        }
        self.selected = Some(position);
        self.sync_inputs(cx);
        cx.emit(ParamRampEvent::Commit(self.ramp.clone()));
        cx.notify();
    }

    /// Remove the stop at `position`.
    ///
    /// The floor is [`RampParam::remove_stop`]'s: the last stop stays, because
    /// a ramp with no stops has no colour to evaluate. A one-stop ramp is a
    /// legitimate state (one flat colour), which is why the floor is one and
    /// not the curve editor's two.
    fn remove_stop(&mut self, position: f32, cx: &mut Context<Self>) {
        if self.ramp.remove_stop(position).is_none() {
            return;
        }
        if self
            .selected
            .is_some_and(|selected| selected.total_cmp(&position).is_eq())
        {
            self.selected = None;
        }
        cx.emit(ParamRampEvent::Commit(self.ramp.clone()));
        cx.notify();
    }
}

impl EventEmitter<ParamRampEvent> for ParamRampEditorState {}

/// Drag payload identifying a stop drag by its owning entity.
#[derive(Clone)]
struct DragRampStop(EntityId);

impl Render for DragRampStop {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Paints the gradient itself across `bounds`.
fn paint_band(bounds: Bounds<Pixels>, ramp: &RampParam, window: &mut Window) {
    let width: f32 = bounds.size.width.into();
    if width <= 0.0 || f32::from(bounds.size.height) <= 0.0 {
        return;
    }
    let steps = (width.ceil() as usize).clamp(1, MAX_BAND_SAMPLES);
    let step = width / steps as f32;
    for index in 0..steps {
        let start = index as f32 * step;
        // Sampled at the middle of the quad so the two ends of the band show
        // the end colours rather than half a quad of the next one.
        let color = ramp.evaluate((start + step / 2.0) / width);
        window.paint_quad(fill(
            Bounds::new(
                point(bounds.origin.x + px(start), bounds.origin.y),
                // One extra pixel of overlap: the quads are laid out in
                // fractional pixels and a rounded-down width leaves seams.
                size(px(step + 1.0), bounds.size.height),
            ),
            display_hsla(color),
        ));
    }
}

/// Paints one stop marker: the stop's own colour inside an outline, so a stop
/// whose colour matches the band behind it is still visible.
fn paint_marker(
    center_x: Pixels,
    bottom: Pixels,
    color: Hsla,
    outline: Hsla,
    selected: bool,
    window: &mut Window,
) {
    let half = if selected {
        MARKER_HALF + 1.5
    } else {
        MARKER_HALF
    };
    let top = bottom - px(MARKER_STRIP);
    window.paint_quad(fill(
        Bounds::new(
            point(center_x - px(half), top),
            size(px(half * 2.0), px(MARKER_STRIP)),
        ),
        outline,
    ));
    window.paint_quad(fill(
        Bounds::new(
            point(center_x - px(half - 1.5), top + px(1.5)),
            size(px((half - 1.5) * 2.0), px(MARKER_STRIP - 3.0)),
        ),
        color,
    ));
}

/// A small non-interactive preview of `ramp`, for the collapsed row.
pub fn ramp_thumbnail(ramp: RampParam) -> impl IntoElement {
    canvas(
        |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx| {
            paint_band(bounds, &ramp, window);
        },
    )
    .size_full()
}

/// The inline ramp editor element. Rebuilt each frame from its state entity.
#[derive(IntoElement)]
pub struct ParamRampEditor {
    state: Entity<ParamRampEditorState>,
}

impl ParamRampEditor {
    pub fn new(state: &Entity<ParamRampEditorState>) -> Self {
        Self {
            state: state.clone(),
        }
    }
}

/// One interpolation button of the toolbar. Active when the ramp already uses
/// that mode.
fn interpolation_button(
    state: &Entity<ParamRampEditorState>,
    interpolation: RampInterpolation,
    current: RampInterpolation,
    active: Hsla,
    muted: Hsla,
    window: &mut Window,
) -> Stateful<Div> {
    // The three ramp modes are the three curve modes without tangents, so they
    // reuse the interpolation icons the Timeline and the curve editor already
    // ship rather than adding a near-identical set.
    let (icon, tooltip) = match interpolation {
        RampInterpolation::Linear => (
            RavelIcon::InterpolationLinear,
            "properties.ramp.interpolation.linear",
        ),
        RampInterpolation::Smooth => (
            RavelIcon::InterpolationBezier,
            "properties.ramp.interpolation.smooth",
        ),
        RampInterpolation::Constant => (
            RavelIcon::InterpolationStep,
            "properties.ramp.interpolation.constant",
        ),
    };
    let color = if current == interpolation {
        active
    } else {
        muted
    };
    div()
        .id(SharedString::from(format!("ramp-interpolation-{icon:?}")))
        .flex_shrink_0()
        .cursor_pointer()
        .child(Icon::new(icon).size_3().text_color(color))
        .tooltip(move |window, cx| Tooltip::new(ravel_i18n::translate(tooltip)).build(window, cx))
        .on_mouse_down(
            MouseButton::Left,
            window.listener_for(state, move |state, _e: &MouseDownEvent, _window, cx| {
                state.set_interpolation(interpolation, cx);
            }),
        )
}

impl RenderOnce for ParamRampEditor {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let entity_id = self.state.entity_id();
        let state = self.state.read(cx);
        let ramp = state.ramp.clone();
        let selected = state.selected;
        let interpolation = ramp.interpolation();
        let bounds = state.bounds.clone();
        let position_input = state.position.clone();
        let selected_stop = state.selected_stop();
        let colors = cx.theme().colors;
        let outline = colors.foreground;
        let accent = colors.accent_foreground;

        let band = div()
            .id(("param-ramp-band", entity_id))
            .flex_1()
            .overflow_hidden()
            .cursor(CursorStyle::Crosshair)
            .child(
                canvas(
                    move |canvas_bounds, _window, _cx| {
                        bounds.set((
                            canvas_bounds.origin.x.into(),
                            canvas_bounds.origin.y.into(),
                            canvas_bounds.size.width.into(),
                            canvas_bounds.size.height.into(),
                        ));
                    },
                    move |canvas_bounds, (), window, _cx| {
                        paint_band(canvas_bounds, &ramp, window);
                        let width: f32 = canvas_bounds.size.width.into();
                        let bottom = canvas_bounds.origin.y + canvas_bounds.size.height;
                        for stop in ramp.stops() {
                            let x =
                                canvas_bounds.origin.x + px(stop.position.clamp(0.0, 1.0) * width);
                            let is_selected = selected
                                .is_some_and(|position| position.total_cmp(&stop.position).is_eq());
                            paint_marker(
                                x,
                                bottom,
                                display_hsla(stop.color),
                                if is_selected { accent } else { outline },
                                is_selected,
                                window,
                            );
                        }
                    },
                )
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                window.listener_for(&self.state, |state, e: &MouseDownEvent, _window, cx| {
                    let x = state.local_x(e.position);
                    state.pointer_down(x, e.click_count, cx);
                }),
            )
            .on_drag(DragRampStop(entity_id), |drag, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| drag.clone())
            })
            .on_drag_move(window.listener_for(
                &self.state,
                move |state, e: &DragMoveEvent<DragRampStop>, _window, cx| {
                    let DragRampStop(id) = e.drag(cx);
                    if *id != entity_id {
                        return;
                    }
                    // The button can be lost outside the window, in which case
                    // the drag state has to go rather than follow a pointer
                    // that is no longer pressing anything.
                    if e.event.pressed_button != Some(MouseButton::Left) {
                        state.cancel_drag(cx);
                        return;
                    }
                    let x = state.local_x(e.event.position);
                    state.drag_to(x, cx);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                window.listener_for(&self.state, |state, _e: &MouseUpEvent, _window, cx| {
                    state.end_drag(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                window.listener_for(&self.state, |state, _e: &MouseUpEvent, _window, cx| {
                    state.end_drag(cx);
                }),
            );

        let mut toolbar = div()
            .flex()
            .items_center()
            .gap_2()
            .px_1()
            .text_xs()
            .text_color(colors.muted_foreground);
        toolbar = match selected_stop {
            Some(_) => toolbar
                .child(
                    div()
                        .flex_shrink_0()
                        .child(SharedString::from(t!("properties.ramp.position"))),
                )
                .child(
                    div()
                        .w(px(FIELD_WIDTH))
                        .child(ScrubInput::new(&position_input)),
                ),
            None => toolbar.child(
                div()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(t!("properties.ramp.no_selection"))),
            ),
        };
        let mut modes = div().flex().items_center().gap_1();
        for mode in [
            RampInterpolation::Linear,
            RampInterpolation::Smooth,
            RampInterpolation::Constant,
        ] {
            modes = modes.child(interpolation_button(
                &self.state,
                mode,
                interpolation,
                colors.primary,
                colors.muted_foreground,
                window,
            ));
        }

        div()
            .id(("param-ramp-editor", entity_id))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(colors.background)
            .border_1()
            .border_color(colors.border)
            .rounded(px(2.0))
            .child(band)
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .py(px(2.0))
                    .child(toolbar)
                    .child(div().flex_grow())
                    .child(modes),
            )
    }
}

#[cfg(test)]
mod tests {
    // Selective import: `use super::*` would pull in `gpui::test` and hijack
    // the built-in `#[test]` attribute (recursive expansion).
    use super::{ParamRampEditorState, ParamRampEvent, display_hsla};
    use gpui::{AppContext as _, TestAppContext};
    use ravel_core::param_ramp::{RampInterpolation, RampParam};
    use ravel_core::types::Color;
    use std::cell::RefCell;
    use std::rc::Rc;

    const RED: Color = Color::new(1.0, 0.0, 0.0, 1.0);
    const BLUE: Color = Color::new(0.0, 0.0, 1.0, 1.0);

    /// Widget size the headless gestures below are expressed in: 200 px wide,
    /// so one ramp position unit is 200 px.
    const SIZE: (f32, f32) = (200.0, 60.0);

    /// `(committed, ramp)` for every event the state emits.
    type EventLog = Rc<RefCell<Vec<(bool, RampParam)>>>;

    fn state(cx: &mut TestAppContext, ramp: RampParam) -> gpui::Entity<ParamRampEditorState> {
        cx.new(|cx| {
            let state = ParamRampEditorState::new(ramp, cx);
            state.set_bounds_for_tests((0.0, 0.0), SIZE);
            state
        })
    }

    fn state_with_log(
        cx: &mut TestAppContext,
        ramp: RampParam,
    ) -> (gpui::Entity<ParamRampEditorState>, EventLog) {
        let state = state(cx, ramp);
        let log: EventLog = Rc::default();
        let sink = log.clone();
        cx.update(|cx| {
            cx.subscribe(
                &state,
                move |_state, event: &ParamRampEvent, _cx| match event {
                    ParamRampEvent::Change(ramp) => sink.borrow_mut().push((false, ramp.clone())),
                    ParamRampEvent::Commit(ramp) => sink.borrow_mut().push((true, ramp.clone())),
                },
            )
            .detach();
        });
        (state, log)
    }

    #[gpui::test]
    fn a_press_selects_the_nearest_stop_and_empty_space_clears_it(cx: &mut TestAppContext) {
        let state = state(cx, RampParam::linear([(0.0, RED), (1.0, BLUE)]));
        state.update(cx, |state, cx| {
            state.pointer_down(0.0, 1, cx);
            assert_eq!(state.selected_stop().map(|stop| stop.position), Some(0.0));
            state.end_drag(cx);
            // 100 px is position 0.5, far from both stops.
            state.pointer_down(100.0, 1, cx);
            assert!(state.selected_stop().is_none());
        });
    }

    /// Dragging keeps the stop inside the band: the position axis is `0..=1`
    /// and a pointer past either end cannot push a stop out of it.
    #[gpui::test]
    fn a_dragged_stop_stays_inside_the_band(cx: &mut TestAppContext) {
        let state = state(cx, RampParam::linear([(0.0, RED), (0.5, RED), (1.0, BLUE)]));
        state.update(cx, |state, cx| {
            state.pointer_down(100.0, 1, cx);
            state.drag_to(10_000.0, cx);
            let position = state.selected_stop().expect("selected").position;
            assert!(
                position < 1.0,
                "the right neighbour holds it back: {position}"
            );
            state.drag_to(-10_000.0, cx);
            let position = state.selected_stop().expect("selected").position;
            assert!(
                position > 0.0,
                "the left neighbour holds it back: {position}"
            );
            state.end_drag(cx);
            assert_eq!(state.ramp().len(), 3, "no stop was merged away");
        });
    }

    /// The band ends are reachable when nothing is in the way, and never
    /// exceeded.
    #[gpui::test]
    fn the_end_stops_clamp_to_the_band_edges(cx: &mut TestAppContext) {
        let state = state(cx, RampParam::linear([(0.25, RED), (0.75, BLUE)]));
        state.update(cx, |state, cx| {
            state.pointer_down(50.0, 1, cx);
            state.drag_to(-500.0, cx);
            assert_eq!(state.selected_stop().expect("selected").position, 0.0);
            state.end_drag(cx);
            state.pointer_down(150.0, 1, cx);
            state.drag_to(500.0, cx);
            assert_eq!(state.selected_stop().expect("selected").position, 1.0);
        });
    }

    #[gpui::test]
    fn a_double_click_adds_a_stop_without_changing_the_gradient(cx: &mut TestAppContext) {
        let state = state(cx, RampParam::linear([(0.0, RED), (1.0, BLUE)]));
        let before = state.read_with(cx, |state, _| state.ramp().evaluate(0.25));
        state.update(cx, |state, cx| {
            state.pointer_down(50.0, 2, cx);
            assert_eq!(state.ramp().len(), 3);
            assert_eq!(
                state.selected_stop().map(|stop| stop.position),
                Some(0.25),
                "the new stop is the selected one"
            );
            let after = state.ramp().evaluate(0.25);
            assert!((after.r - before.r).abs() < 1e-6 && (after.b - before.b).abs() < 1e-6);
        });
    }

    #[gpui::test]
    fn a_double_click_on_a_stop_removes_it_but_never_the_last(cx: &mut TestAppContext) {
        let state = state(cx, RampParam::linear([(0.0, RED), (1.0, BLUE)]));
        state.update(cx, |state, cx| {
            state.pointer_down(0.0, 2, cx);
            assert_eq!(state.ramp().len(), 1);
            state.pointer_down(200.0, 2, cx);
            assert_eq!(
                state.ramp().len(),
                1,
                "the last stop stays: a ramp with no stops has no colour"
            );
        });
    }

    #[gpui::test]
    fn the_position_field_moves_the_selected_stop_within_the_band(cx: &mut TestAppContext) {
        let state = state(cx, RampParam::linear([(0.0, RED), (0.5, RED), (1.0, BLUE)]));
        state.update(cx, |state, cx| {
            state.pointer_down(100.0, 1, cx);
            state.end_drag(cx);
            state.set_selected_position(0.9, true, cx);
            let position = state.selected_stop().expect("selected").position;
            assert!(position > 0.5 && position < 1.0, "{position}");
            state.set_selected_position(50.0, true, cx);
            assert!(state.selected_stop().expect("selected").position <= 1.0);
            state.set_selected_position(f32::NAN, true, cx);
            assert!(
                state
                    .selected_stop()
                    .expect("selected")
                    .position
                    .is_finite()
            );
        });
    }

    #[gpui::test]
    fn recolouring_the_selected_stop_returns_the_edited_ramp(cx: &mut TestAppContext) {
        let state = state(cx, RampParam::linear([(0.0, RED), (1.0, BLUE)]));
        state.update(cx, |state, cx| {
            assert!(
                state.set_selected_color(Color::WHITE, cx).is_none(),
                "nothing is selected yet"
            );
            state.pointer_down(0.0, 1, cx);
            state.end_drag(cx);
            let edited = state.set_selected_color(Color::WHITE, cx).expect("edited");
            assert_eq!(edited.evaluate(0.0), Color::WHITE);
            assert_eq!(edited.evaluate(1.0), BLUE);
        });
    }

    #[gpui::test]
    fn switching_the_interpolation_keeps_the_stops(cx: &mut TestAppContext) {
        let state = state(cx, RampParam::linear([(0.0, RED), (1.0, BLUE)]));
        state.update(cx, |state, cx| {
            state.set_interpolation(RampInterpolation::Constant, cx);
            assert_eq!(state.ramp().interpolation(), RampInterpolation::Constant);
            assert_eq!(state.ramp().len(), 2);
            assert_eq!(state.ramp().evaluate(0.9), RED);
        });
    }

    /// An external refresh (undo, another panel) must not fight an in-flight
    /// gesture: the drag owns the ramp until it ends.
    #[gpui::test]
    fn an_external_refresh_never_interrupts_a_drag(cx: &mut TestAppContext) {
        let state = state(cx, RampParam::linear([(0.0, RED), (0.5, RED), (1.0, BLUE)]));
        state.update(cx, |state, cx| {
            state.pointer_down(100.0, 1, cx);
            state.drag_to(120.0, cx);
            let during = state.ramp().clone();
            state.set_ramp_synced(RampParam::linear([(0.0, BLUE), (1.0, RED)]), cx);
            assert_eq!(state.ramp(), &during, "the drag is the source of truth");
            state.end_drag(cx);
            state.set_ramp_synced(RampParam::linear([(0.0, BLUE), (1.0, RED)]), cx);
            assert_eq!(state.ramp().len(), 2, "an idle editor follows the document");
        });
    }

    /// The gesture contract: live changes while dragging, exactly one commit
    /// at the end — the host records one undo step per gesture.
    #[gpui::test]
    fn a_stop_drag_emits_live_changes_and_one_commit(cx: &mut TestAppContext) {
        let (state, log) =
            state_with_log(cx, RampParam::linear([(0.0, RED), (0.5, RED), (1.0, BLUE)]));
        state.update(cx, |state, cx| {
            state.pointer_down(100.0, 1, cx);
            assert!(state.is_dragging());
            state.drag_to(110.0, cx);
            state.drag_to(120.0, cx);
            assert!(state.end_drag(cx), "the gesture committed");
            assert!(!state.is_dragging());
        });
        let events = log.borrow();
        assert_eq!(
            events.iter().filter(|(committed, _)| *committed).count(),
            1,
            "one commit for the whole gesture: {events:?}"
        );
        assert!(
            events.len() > 1,
            "the moves applied live before the commit: {events:?}"
        );
    }

    /// A gesture that never moved the stop records nothing: the live changes
    /// already put the ramp back where it started.
    #[gpui::test]
    fn a_drag_that_never_moved_commits_nothing(cx: &mut TestAppContext) {
        let (state, log) = state_with_log(cx, RampParam::linear([(0.0, RED), (1.0, BLUE)]));
        state.update(cx, |state, cx| {
            state.pointer_down(0.0, 1, cx);
            assert!(!state.end_drag(cx));
        });
        assert!(
            log.borrow().iter().all(|(committed, _)| !committed),
            "a press that moved nothing is not an edit"
        );
    }

    /// Working-space linear light is encoded for display before it is painted,
    /// like every other colour a widget shows (`CM-2`).
    #[test]
    fn stops_are_painted_in_the_display_encoding() {
        let mid = display_hsla(Color::new(0.5, 0.5, 0.5, 1.0));
        let rgba = gpui::Rgba::from(mid);
        assert!(
            rgba.r > 0.6,
            "linear 0.5 is well above 0.5 once encoded: {}",
            rgba.r
        );
    }
}
