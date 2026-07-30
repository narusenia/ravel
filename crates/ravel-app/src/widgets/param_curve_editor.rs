// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Inline editor for [`CurveParam`] scalar transfer curves.
//!
//! The Properties panel folds a curve parameter into one row with a
//! thumbnail and expands this editor underneath it. Control points are added,
//! moved, and removed here; the host owns the document write and the undo
//! granularity (live [`ParamCurveEvent::Change`] during a drag, one
//! [`ParamCurveEvent::Commit`] per gesture — the same contract as
//! [`ScrubEvent`](super::ScrubEvent)).
//!
//! # Why this is not `widgets/curve_editor.rs`
//!
//! [`super::curve_editor`] is the Timeline's graph editor and is built around
//! `KeyframeCurve`: a hit is identified by an **integer frame**
//! (`CurveHit::frame: u64`), a drag captures a `Keyframe`, and paint sampling
//! decimates over integer frames while pinning key boundaries. A `CurveParam`
//! is indexed by an arbitrary scalar, so nothing built on that identity
//! carries over — generalizing it would change the Timeline's hit identity,
//! its drag quantization (`to_frame` rounds to whole frames), and its sample
//! decimation, all of which its tests pin.
//!
//! What *is* axis-agnostic is shared rather than reimplemented:
//!
//! * [`CurveTransform`] — the data ↔ widget mapping, used verbatim.
//! * [`CurveParam::evaluate`] — the curve maths, which itself delegates to
//!   `animation::interpolation::{linear_at, bezier_at}`, the same functions
//!   `KeyframeCurve::sample` uses. There is no second evaluator here.
//!
//! # View state
//!
//! The vertical range is the *caller's* state, exactly as the Timeline holds
//! `curve_value_range` for the graph editor: [`ParamCurveEditorState`] takes
//! an optional value range and otherwise fits the curve. Vertical zoom is a
//! later unit and belongs to whoever owns that range, not to this widget.

use std::cell::Cell;
use std::rc::Rc;

use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::Icon;
use gpui_component::tooltip::Tooltip;
use ravel_core::animation::Interpolation;
use ravel_core::param_curve::{CurveParam, CurvePoint};
use ravel_core::types::Vec2;

use super::curve_editor::{
    CurvePoint as ViewPoint, CurveTransform, HitPart, handle_anchor, snap_to_diagonals,
};
use super::curve_view;
use crate::assets::RavelIcon;

/// Pointer distance (widget pixels) that still counts as grabbing a point.
pub const HIT_RADIUS: f64 = 7.0;
/// Fraction of the fitted span kept as empty margin, so end points sit
/// inside the editor instead of on its border.
const FIT_MARGIN: f32 = 0.08;
/// Half-height of the degenerate range a flat curve is drawn in.
const FLAT_SPAN: f32 = 0.5;
/// Control points a curve keeps: a remap that collapses to a constant (or to
/// the implicit identity) cannot be told apart from an empty editor, so
/// removal stops here. Points are always addable again.
const MIN_POINTS: usize = 2;
/// Gap kept between a dragged point and its neighbour. Points are identified
/// by their input value, so two points must never share one. This is an upper
/// bound: [`gap_between`] shrinks it when the neighbours sit closer together
/// than twice this.
const POINT_GAP: f32 = 1.0e-4;
/// Share of the space between two neighbours that the gap may take. A quarter
/// leaves the dragged point at least half the span to move in.
const GAP_SHARE: f32 = 0.25;
/// Painted radius of a control point, and of a Bézier handle.
const POINT_RADIUS: f32 = 3.0;
const HANDLE_RADIUS: f32 = 2.5;
/// Upper bound on painted polyline samples, mirroring the Timeline editor's
/// paint budget.
const MAX_SAMPLES: usize = 2_048;
/// Target spacing of the input-axis grid. Wider than the output axis because
/// its labels run along the axis and would collide sooner.
const INPUT_GRID_TARGET_PX: f64 = 72.0;
/// Opacity of an ordinary grid line, and of the line at zero.
const GRID_ALPHA: f32 = 0.10;
const GRID_ZERO_ALPHA: f32 = 0.28;
/// Axis label plate size and text size.
const LABEL_WIDTH: f32 = 40.0;
const LABEL_HEIGHT: f32 = 12.0;
const LABEL_FONT_SIZE: f32 = 9.0;
/// Below this the axis is too short to carry readable labels; the grid lines
/// stay, the numbers are dropped.
const LABEL_MIN_EXTENT_PX: f32 = 64.0;

/// Data-space view box of a curve editor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveView {
    pub x: (f32, f32),
    pub y: (f32, f32),
}

/// The view box that shows the whole curve with a margin.
///
/// An empty curve is the identity mapping, so it is shown over the unit
/// square rather than as an empty plot.
pub fn fit_view(curve: &CurveParam) -> CurveView {
    let points = curve.points();
    let Some(first) = points.first() else {
        return CurveView {
            x: (0.0, 1.0),
            y: (0.0, 1.0),
        };
    };
    let mut min = (first.x, first.y);
    let mut max = (first.x, first.y);
    for point in points {
        min = (min.0.min(point.x), min.1.min(point.y));
        max = (max.0.max(point.x), max.1.max(point.y));
    }
    CurveView {
        x: padded(min.0, max.0),
        y: padded(min.1, max.1),
    }
}

fn padded(min: f32, max: f32) -> (f32, f32) {
    if !min.is_finite() || !max.is_finite() {
        return (0.0, 1.0);
    }
    let span = max - min;
    if span <= f32::EPSILON {
        return (min - FLAT_SPAN, max + FLAT_SPAN);
    }
    let margin = span * FIT_MARGIN;
    (min - margin, max + margin)
}

/// Data ↔ widget mapping for `view` inside a widget of `size` pixels.
pub fn transform_for(view: CurveView, size: (f32, f32)) -> CurveTransform {
    CurveTransform::new(
        ViewPoint::new(view.x.0 as f64, view.y.0 as f64),
        ViewPoint::new(view.x.1 as f64, view.y.1 as f64),
        ViewPoint::new(size.0 as f64, size.1 as f64),
    )
}

/// The input value of the control point within `radius` widget pixels of
/// `pointer`, closest first.
pub fn hit_point(
    curve: &CurveParam,
    transform: CurveTransform,
    pointer: ViewPoint,
    radius: f64,
) -> Option<f32> {
    let radius_sq = radius.max(0.0).powi(2);
    let mut best: Option<(f64, f32)> = None;
    for point in curve.points() {
        let widget = transform.data_to_widget(ViewPoint::new(point.x as f64, point.y as f64));
        let distance_sq = (widget.x - pointer.x).powi(2) + (widget.y - pointer.y).powi(2);
        if distance_sq <= radius_sq && best.is_none_or(|(current, _)| distance_sq < current) {
            best = Some((distance_sq, point.x));
        }
    }
    best.map(|(_, x)| x)
}

/// Whether the control point at `x` may have its input value changed.
///
/// **The two outer points are pinned to their inputs** and only their outputs
/// are editable. They are the curve's domain, and outside it a `CurveParam`
/// clamps: dragging an end point inwards silently shortens the domain, and
/// dragging it outwards pushes it off the visible range. The domain is
/// changed by editing the curve's values, not by sliding its ends around.
pub fn x_is_editable(curve: &CurveParam, x: f32) -> bool {
    let points = curve.points();
    match points
        .iter()
        .position(|point| point.x.total_cmp(&x).is_eq())
    {
        Some(index) => index > 0 && index + 1 < points.len(),
        None => false,
    }
}

/// Immutable state captured when a control-point drag starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveParamDrag {
    /// Input value the dragged point currently sits at (it moves as the
    /// drag applies, so this tracks the live identity).
    current_x: f32,
    origin: CurvePoint,
    pointer_start: ViewPoint,
    transform: CurveTransform,
    /// Exclusive bounds the point may move between, from its neighbours at
    /// drag start. Points are identified by their input value, so a drag
    /// must never make two points share one (which would silently drop one).
    lower: Option<f32>,
    upper: Option<f32>,
    /// Outer points keep their input value (see [`x_is_editable`]).
    x_locked: bool,
}

impl CurveParamDrag {
    /// The input value the dragged point currently occupies.
    pub fn current_x(self) -> f32 {
        self.current_x
    }
}

/// Starts a drag on the control point at `x`, if the curve still has one.
pub fn begin_point_drag(
    curve: &CurveParam,
    x: f32,
    pointer: ViewPoint,
    transform: CurveTransform,
) -> Option<CurveParamDrag> {
    let points = curve.points();
    let index = points
        .iter()
        .position(|point| point.x.total_cmp(&x).is_eq())?;
    Some(CurveParamDrag {
        current_x: x,
        origin: points[index],
        pointer_start: pointer,
        transform,
        lower: index.checked_sub(1).map(|i| points[i].x),
        upper: points.get(index + 1).map(|point| point.x),
        x_locked: index == 0 || index + 1 == points.len(),
    })
}

/// The gap to keep from each neighbour when a point is dragged between
/// `lower` and `upper`.
///
/// A fixed gap is wrong when the neighbours are closer together than twice
/// it: clamping up from `lower` and then down from `upper` inverts, and the
/// result can land *on* a neighbour, where `insert_point` overwrites it and
/// the point silently disappears. The gap therefore never takes more than
/// [`GAP_SHARE`] of the space actually available.
fn gap_between(lower: Option<f32>, upper: Option<f32>) -> f32 {
    match (lower, upper) {
        (Some(lower), Some(upper)) => POINT_GAP.min((upper - lower) * GAP_SHARE),
        _ => POINT_GAP,
    }
}

/// `x` clamped strictly inside `(lower, upper)`, or `None` when the two are
/// so close that no `f32` between them survives the rounding.
fn clamp_between(x: f32, lower: Option<f32>, upper: Option<f32>) -> Option<f32> {
    let gap = gap_between(lower, upper);
    if !gap.is_finite() || gap <= 0.0 {
        return None;
    }
    let mut x = x;
    if let Some(lower) = lower {
        x = x.max(lower + gap);
    }
    if let Some(upper) = upper {
        x = x.min(upper - gap);
    }
    // `lower + gap` can round back onto `lower` once the neighbours are within
    // an ulp or two of each other, so the bound is re-checked rather than
    // trusted.
    let inside = lower.is_none_or(|lower| x > lower) && upper.is_none_or(|upper| x < upper);
    inside.then_some(x)
}

/// The `(input, output)` the dragged point moves to for `pointer`, clamped
/// so it stays strictly between its neighbours.
///
/// An outer point keeps its input value entirely ([`x_is_editable`]). When the
/// neighbours leave no room at all the point likewise keeps the input value it
/// has and only its output follows the pointer: refusing the horizontal move
/// is the one outcome that cannot merge two points.
pub fn drag_point_to(drag: CurveParamDrag, pointer: ViewPoint) -> (f32, f32) {
    let start = drag.transform.widget_to_data(drag.pointer_start);
    let current = drag.transform.widget_to_data(pointer);
    let x = drag.origin.x as f64 + (current.x - start.x);
    let y = drag.origin.y as f64 + (current.y - start.y);
    let x = if drag.x_locked {
        drag.origin.x
    } else {
        clamp_between(x as f32, drag.lower, drag.upper).unwrap_or(drag.current_x)
    };
    (x, y as f32)
}

/// Which Bézier handles the point at `index` shows: the incoming one exists
/// when the previous point's segment is Bézier, the outgoing one when this
/// point's is. Same rule as the Timeline graph editor's
/// [`control_points`](super::curve_editor::control_points).
fn handle_visibility(points: &[CurvePoint], index: usize) -> (bool, bool) {
    let incoming = index > 0 && points[index - 1].interpolation == Interpolation::Bezier;
    let outgoing = index + 1 < points.len() && points[index].interpolation == Interpolation::Bezier;
    (incoming, outgoing)
}

/// Widget position of one of the point's Bézier handles.
fn handle_position(point: &CurvePoint, part: HitPart, transform: CurveTransform) -> ViewPoint {
    let tangent = match part {
        HitPart::TangentIn => point.tangent_in,
        _ => point.tangent_out,
    };
    transform.data_to_widget(ViewPoint::new(
        point.x as f64 + tangent.0 as f64,
        point.y as f64 + tangent.1 as f64,
    ))
}

/// An editable part of the curve: the control point's input value plus which
/// of its handles was grabbed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamCurveHit {
    pub x: f32,
    pub part: HitPart,
}

/// The closest control point or Bézier handle within `radius` widget pixels.
///
/// Anchors win ties so a zero-length handle never makes its point
/// ungrabbable — the same rule the Timeline graph editor uses.
pub fn hit_test(
    curve: &CurveParam,
    transform: CurveTransform,
    pointer: ViewPoint,
    radius: f64,
) -> Option<ParamCurveHit> {
    let radius_sq = radius.max(0.0).powi(2);
    let points = curve.points();
    let mut best: Option<(f64, u8, ParamCurveHit)> = None;
    let mut consider = |position: ViewPoint, hit: ParamCurveHit| {
        let distance_sq = (position.x - pointer.x).powi(2) + (position.y - pointer.y).powi(2);
        if distance_sq > radius_sq {
            return;
        }
        let priority = u8::from(hit.part != HitPart::Keyframe);
        if best.is_none_or(|(current, current_priority, _)| {
            (distance_sq, priority) < (current, current_priority)
        }) {
            best = Some((distance_sq, priority, hit));
        }
    };
    for (index, point) in points.iter().enumerate() {
        consider(
            transform.data_to_widget(ViewPoint::new(point.x as f64, point.y as f64)),
            ParamCurveHit {
                x: point.x,
                part: HitPart::Keyframe,
            },
        );
        let (incoming, outgoing) = handle_visibility(points, index);
        if incoming {
            consider(
                handle_position(point, HitPart::TangentIn, transform),
                ParamCurveHit {
                    x: point.x,
                    part: HitPart::TangentIn,
                },
            );
        }
        if outgoing {
            consider(
                handle_position(point, HitPart::TangentOut, transform),
                ParamCurveHit {
                    x: point.x,
                    part: HitPart::TangentOut,
                },
            );
        }
    }
    best.map(|(_, _, hit)| hit)
}

/// Immutable state captured when a Bézier handle drag starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TangentDrag {
    hit: ParamCurveHit,
    origin: CurvePoint,
    pointer_start: ViewPoint,
    transform: CurveTransform,
    previous_x: Option<f32>,
    next_x: Option<f32>,
}

/// Starts a handle drag, if that handle is actually shown for `hit`.
pub fn begin_tangent_drag(
    curve: &CurveParam,
    hit: ParamCurveHit,
    pointer: ViewPoint,
    transform: CurveTransform,
) -> Option<TangentDrag> {
    let points = curve.points();
    let index = points
        .iter()
        .position(|point| point.x.total_cmp(&hit.x).is_eq())?;
    let (incoming, outgoing) = handle_visibility(points, index);
    let applicable = match hit.part {
        HitPart::TangentIn => incoming,
        HitPart::TangentOut => outgoing,
        HitPart::Keyframe => false,
    };
    if !applicable {
        return None;
    }
    Some(TangentDrag {
        hit,
        origin: points[index],
        pointer_start: pointer,
        transform,
        previous_x: index.checked_sub(1).map(|i| points[i].x),
        next_x: points.get(index + 1).map(|point| point.x),
    })
}

/// The tangent the dragged handle moves to, clamped so it cannot reach past
/// the adjacent control point.
///
/// `snap` (Shift) rotates the handle onto the nearest screen-space diagonal
/// through [`snap_to_diagonals`], the same helper the Timeline graph editor
/// uses, so the modifier behaves identically in both.
pub fn drag_tangent_to(drag: TangentDrag, pointer: ViewPoint, snap: bool) -> Vec2 {
    let original = match drag.hit.part {
        HitPart::TangentIn => drag.origin.tangent_in,
        _ => drag.origin.tangent_out,
    };
    let pointer = if snap {
        let anchor = handle_anchor(drag.transform, drag.pointer_start, original);
        snap_to_diagonals(anchor, pointer).unwrap_or(pointer)
    } else {
        pointer
    };
    let start = drag.transform.widget_to_data(drag.pointer_start);
    let current = drag.transform.widget_to_data(pointer);
    let x = original.0 as f64 + (current.x - start.x);
    let y = original.1 as f64 + (current.y - start.y);
    let x = match drag.hit.part {
        HitPart::TangentIn => x.clamp(
            drag.previous_x
                .map_or(0.0, |previous| -((drag.origin.x - previous) as f64)),
            0.0,
        ),
        _ => x.clamp(
            0.0,
            drag.next_x
                .map_or(0.0, |next| (next - drag.origin.x) as f64),
        ),
    };
    Vec2(x as f32, y as f32)
}

/// Set the interpolation of the segment leaving the point at `x`.
///
/// Switching a straight segment to Bézier seeds one-third handles along the
/// same line: the curve looks unchanged but both controls become grabbable
/// immediately. Handles the user already shaped survive the switch. This
/// mirrors `keyframes::set_curve_interpolation` so the two editors convert
/// identically.
pub fn set_curve_interpolation(
    curve: &mut CurveParam,
    x: f32,
    interpolation: Interpolation,
) -> bool {
    let points = curve.points();
    let Some(index) = points
        .iter()
        .position(|point| point.x.total_cmp(&x).is_eq())
    else {
        return false;
    };
    let mut point = points[index];
    let mut next = points.get(index + 1).copied();

    if interpolation == Interpolation::Bezier
        && point.interpolation != Interpolation::Bezier
        && let Some(next_point) = &mut next
    {
        let third = 1.0 / 3.0;
        let input_delta = (next_point.x - point.x) * third;
        let output_delta = (next_point.y - point.y) * third;
        if point.tangent_out == Vec2(0.0, 0.0) {
            point.tangent_out = Vec2(input_delta, output_delta);
        }
        if next_point.tangent_in == Vec2(0.0, 0.0) {
            next_point.tangent_in = Vec2(-input_delta, -output_delta);
        }
    }
    point.interpolation = interpolation;
    curve.insert_point(point);
    if let Some(next) = next {
        curve.insert_point(next);
    }
    true
}

/// Set one handle of the control point at `x`.
pub fn set_curve_tangent(curve: &mut CurveParam, x: f32, part: HitPart, tangent: Vec2) -> bool {
    let Some(mut point) = curve
        .points()
        .iter()
        .find(|point| point.x.total_cmp(&x).is_eq())
        .copied()
    else {
        return false;
    };
    match part {
        HitPart::TangentIn => point.tangent_in = tangent,
        HitPart::TangentOut => point.tangent_out = tangent,
        HitPart::Keyframe => return false,
    }
    // The input value is unchanged, so this replaces the point in place.
    curve.insert_point(point);
    true
}

/// Live value while a control point is being dragged. Apply it, but do not
/// record undo.
///
/// [`ParamCurveEvent::Commit`] ends the gesture and is where the host records
/// exactly one undo step.
pub enum ParamCurveEvent {
    Change(CurveParam),
    Commit(CurveParam),
}

/// Widget bounds shared between paint and the mouse handlers.
///
/// Paint is the only place the element's bounds are known, and writing them
/// into a `Cell` (rather than into the entity) keeps painting free of entity
/// updates and re-render loops — the same arrangement the Timeline uses for
/// its graph area.
type SharedBounds = Rc<Cell<(f32, f32, f32, f32)>>;

/// The gesture in progress: a control point being moved, or one of its
/// Bézier handles being shaped.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ActiveDrag {
    Point(CurveParamDrag),
    Tangent(TangentDrag),
}

impl ActiveDrag {
    fn transform(self) -> CurveTransform {
        match self {
            Self::Point(drag) => drag.transform,
            Self::Tangent(drag) => drag.transform,
        }
    }
}

pub struct ParamCurveEditorState {
    curve: CurveParam,
    /// Caller-supplied vertical range; `None` fits the curve.
    value_range: Option<(f32, f32)>,
    /// Input value of the selected control point, if any. Selection is view
    /// state: it drives the value readout and the interpolation buttons and
    /// never reaches the Document.
    selected: Option<f32>,
    drag: Option<ActiveDrag>,
    /// Whether the live drag has moved the point at all (a drag that never
    /// moved must not record an undo step).
    moved_in_drag: bool,
    bounds: SharedBounds,
}

impl ParamCurveEditorState {
    pub fn new(curve: CurveParam) -> Self {
        Self {
            curve,
            value_range: None,
            selected: None,
            drag: None,
            moved_in_drag: false,
            bounds: Rc::new(Cell::new((0.0, 0.0, 0.0, 0.0))),
        }
    }

    /// Builder: pin the vertical range instead of fitting the curve.
    pub fn value_range(mut self, range: Option<(f32, f32)>) -> Self {
        self.value_range = range;
        self
    }

    pub fn curve(&self) -> &CurveParam {
        &self.curve
    }

    /// The selected control point, if it is still in the curve.
    pub fn selected_point(&self) -> Option<CurvePoint> {
        let x = self.selected?;
        self.curve
            .points()
            .iter()
            .find(|point| point.x.total_cmp(&x).is_eq())
            .copied()
    }

    /// Whether a drag is in progress (external refreshes must not fight the
    /// gesture).
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Replace the displayed curve from the document. Ignored mid-gesture:
    /// the drag is the source of truth until it ends.
    pub fn set_curve(&mut self, curve: CurveParam) {
        if self.is_dragging() {
            return;
        }
        self.curve = curve;
        // A selection that the new curve no longer contains is dropped, so
        // the readout never describes a point that is gone.
        if self.selected_point().is_none() {
            self.selected = None;
        }
    }

    /// The view box in data space: the caller's vertical range over the
    /// curve's own horizontal extent, or a full fit when none was supplied.
    /// While dragging, the view stays as it was when the gesture started, so
    /// a point being dragged outward does not rescale the axes under the
    /// pointer.
    pub fn view(&self) -> CurveView {
        if let Some(drag) = self.drag {
            let transform = drag.transform();
            return CurveView {
                x: (transform.data_min.x as f32, transform.data_max.x as f32),
                y: (transform.data_min.y as f32, transform.data_max.y as f32),
            };
        }
        let mut view = fit_view(&self.curve);
        if let Some(range) = self.value_range {
            view.y = range;
        }
        view
    }

    /// Test hook: paint is what normally records the element's bounds, so
    /// a headless test has to supply them before driving the pointer.
    #[cfg(test)]
    pub(crate) fn set_bounds_for_tests(&self, origin: (f32, f32), size: (f32, f32)) {
        self.bounds.set((origin.0, origin.1, size.0, size.1));
    }

    fn size(&self) -> (f32, f32) {
        let (_, _, width, height) = self.bounds.get();
        (width, height)
    }

    fn transform(&self) -> Option<CurveTransform> {
        let size = self.size();
        if size.0 <= 0.0 || size.1 <= 0.0 {
            return None;
        }
        Some(transform_for(self.view(), size))
    }

    /// Widget-space pointer position from a window-space one.
    fn local(&self, position: Point<Pixels>) -> ViewPoint {
        let (origin_x, origin_y, _, _) = self.bounds.get();
        ViewPoint::new(
            f64::from(position.x) - origin_x as f64,
            f64::from(position.y) - origin_y as f64,
        )
    }

    /// Left-button press: a second click adds a point (or removes the one
    /// under the pointer), a first click selects and starts dragging whatever
    /// it grabbed — the anchor or one of its Bézier handles.
    pub(crate) fn pointer_down(
        &mut self,
        pointer: ViewPoint,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(transform) = self.transform() else {
            return;
        };
        let hit = hit_test(&self.curve, transform, pointer, HIT_RADIUS);
        if click_count >= 2 {
            match hit {
                Some(hit) => self.remove_point(hit.x, cx),
                None => self.insert_point(pointer, transform, cx),
            }
            return;
        }
        let Some(hit) = hit else {
            // A press on empty space clears the selection, so the readout
            // stops describing a point the user is no longer working on.
            self.selected = None;
            cx.notify();
            return;
        };
        self.selected = Some(hit.x);
        self.drag = match hit.part {
            HitPart::Keyframe => {
                begin_point_drag(&self.curve, hit.x, pointer, transform).map(ActiveDrag::Point)
            }
            _ => begin_tangent_drag(&self.curve, hit, pointer, transform).map(ActiveDrag::Tangent),
        };
        self.moved_in_drag = false;
        cx.notify();
    }

    /// Unmodified drag, used by the headless gesture tests.
    #[cfg(test)]
    pub(crate) fn drag_to(&mut self, pointer: ViewPoint, cx: &mut Context<Self>) {
        self.drag_to_with_modifiers(pointer, false, cx);
    }

    /// `snap` (Shift) constrains a handle drag to screen-space diagonals, the
    /// same modifier the Timeline graph editor uses.
    pub(crate) fn drag_to_with_modifiers(
        &mut self,
        pointer: ViewPoint,
        snap: bool,
        cx: &mut Context<Self>,
    ) {
        match self.drag {
            Some(ActiveDrag::Point(drag)) => {
                let (x, y) = drag_point_to(drag, pointer);
                if !self.curve.move_point(drag.current_x, x, y) {
                    return;
                }
                if let Some(ActiveDrag::Point(drag)) = self.drag.as_mut() {
                    drag.current_x = x;
                }
                if self.selected == Some(drag.current_x) {
                    self.selected = Some(x);
                }
            }
            Some(ActiveDrag::Tangent(drag)) => {
                let tangent = drag_tangent_to(drag, pointer, snap);
                if !set_curve_tangent(&mut self.curve, drag.hit.x, drag.hit.part, tangent) {
                    return;
                }
            }
            None => return,
        }
        self.moved_in_drag = true;
        cx.emit(ParamCurveEvent::Change(self.curve.clone()));
        cx.notify();
    }

    pub(crate) fn end_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let moved = self.moved_in_drag;
        self.moved_in_drag = false;
        // A gesture that returned to where it started emitted live Changes
        // that already restored the original curve; committing would only
        // record a no-op undo step.
        let settled = match drag {
            ActiveDrag::Point(drag) => {
                drag.current_x.total_cmp(&drag.origin.x).is_eq()
                    && self
                        .curve
                        .points()
                        .iter()
                        .any(|point| point == &drag.origin)
            }
            ActiveDrag::Tangent(drag) => self
                .curve
                .points()
                .iter()
                .any(|point| point == &drag.origin),
        };
        if moved && !settled {
            cx.emit(ParamCurveEvent::Commit(self.curve.clone()));
        } else if moved {
            cx.emit(ParamCurveEvent::Change(self.curve.clone()));
        }
        cx.notify();
    }

    /// Switch the interpolation of the segment leaving the selected point.
    /// One click, one undo step.
    pub(crate) fn set_selected_interpolation(
        &mut self,
        interpolation: Interpolation,
        cx: &mut Context<Self>,
    ) {
        let Some(point) = self.selected_point() else {
            return;
        };
        if point.interpolation == interpolation {
            return;
        }
        if !set_curve_interpolation(&mut self.curve, point.x, interpolation) {
            return;
        }
        cx.emit(ParamCurveEvent::Commit(self.curve.clone()));
        cx.notify();
    }

    /// Add a point on the curve at the pointer's input value. Its output is
    /// the pointer's, so a double-click both adds and places the point.
    fn insert_point(
        &mut self,
        pointer: ViewPoint,
        transform: CurveTransform,
        cx: &mut Context<Self>,
    ) {
        let data = transform.widget_to_data(pointer);
        let (x, y) = (data.x as f32, data.y as f32);
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        // The new point inherits the interpolation of the segment it lands
        // in, so adding a point never changes the curve's character.
        let interpolation = self
            .curve
            .points()
            .iter()
            .rev()
            .find(|point| point.x < x)
            .or_else(|| self.curve.points().first())
            .map(|point| point.interpolation)
            .unwrap_or_default();
        self.curve
            .insert_point(CurvePoint::new(x, y, interpolation));
        self.selected = Some(x);
        cx.emit(ParamCurveEvent::Commit(self.curve.clone()));
        cx.notify();
    }

    /// Remove the point at input value `x`, keeping [`MIN_POINTS`].
    ///
    /// The outer points are not removable: dropping one moves the curve's
    /// domain edge onto its neighbour, which is the same change pinning their
    /// inputs rules out ([`x_is_editable`]).
    fn remove_point(&mut self, x: f32, cx: &mut Context<Self>) {
        if self.curve.len() <= MIN_POINTS || !x_is_editable(&self.curve, x) {
            return;
        }
        if self.curve.remove_point(x).is_none() {
            return;
        }
        if self
            .selected
            .is_some_and(|selected| selected.total_cmp(&x).is_eq())
        {
            self.selected = None;
        }
        cx.emit(ParamCurveEvent::Commit(self.curve.clone()));
        cx.notify();
    }
}

impl EventEmitter<ParamCurveEvent> for ParamCurveEditorState {}

/// Drag payload identifying a control-point drag by its owning entity.
#[derive(Clone)]
struct DragCurvePoint(EntityId);

impl Render for DragCurvePoint {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Draws one axis label with a translucent plate behind it, so it stays
/// readable where it crosses the curve.
#[allow(clippy::too_many_arguments)]
fn paint_label(
    text: String,
    origin: Point<Pixels>,
    color: Hsla,
    background: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    let label = SharedString::from(text);
    let len = label.len();
    let width = px(LABEL_WIDTH);
    let height = px(LABEL_HEIGHT);
    window.paint_quad(fill(
        Bounds::new(origin, size(width, height)),
        Hsla {
            a: 0.82,
            ..background
        },
    ));
    let shaped = window.text_system().shape_line(
        label,
        px(LABEL_FONT_SIZE),
        &[TextRun {
            len,
            font: Font {
                family: SharedString::from("sans-serif"),
                ..Default::default()
            },
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        Some(width),
    );
    let _ = shaped.paint(
        point(origin.x + px(3.0), origin.y),
        height,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// The `(input, output)` tick values drawn for `view` at `size` pixels.
///
/// Both axes take their values from the shared [`curve_view`] module, so the
/// inline editor and the Timeline graph put lines in the same places for the
/// same range. The input axis asks for wider spacing because its labels run
/// along the axis and would collide sooner.
pub fn grid_ticks(view: CurveView, size: (f32, f32)) -> (Vec<f64>, Vec<f64>) {
    let transform = transform_for(view, size);
    (
        curve_view::grid_values(
            transform.data_min.x,
            transform.data_max.x,
            size.0 as f64,
            INPUT_GRID_TARGET_PX,
        ),
        curve_view::value_grid_values(transform.data_min.y, transform.data_max.y, size.1 as f64),
    )
}

/// Whether an axis of `size` pixels is long enough to carry tick labels.
/// Below it the grid lines stay and the numbers are dropped, so a short row
/// does not fill with unreadable text.
fn labels_fit(size: (f32, f32)) -> bool {
    size.0 >= LABEL_MIN_EXTENT_PX && size.1 >= LABEL_MIN_EXTENT_PX
}

/// Paints the grid and the axis tick labels of `view`.
///
/// Tick values come from the shared [`curve_view`] module, so the inline
/// editor and the Timeline graph put lines in the same places for the same
/// range. Labels are dropped on an axis too short to carry them, and the
/// tick spacing itself thins out with the widget size.
fn paint_grid(
    bounds: Bounds<Pixels>,
    view: CurveView,
    line: Hsla,
    label: Hsla,
    background: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let transform = transform_for(view, (width, height));
    let (min_x, min_y) = (transform.data_min.x, transform.data_min.y);
    let (inputs, outputs) = grid_ticks(view, (width, height));
    let labels_fit = labels_fit((width, height));

    // Input axis: vertical lines, labels along the bottom edge.
    for value in inputs {
        let widget = transform.data_to_widget(ViewPoint::new(value, min_y));
        let x = bounds.origin.x + px(widget.x as f32);
        let zero = value.abs() < f64::EPSILON;
        window.paint_quad(fill(
            Bounds::new(point(x, bounds.origin.y), size(px(1.0), bounds.size.height)),
            Hsla {
                a: if zero { GRID_ZERO_ALPHA } else { GRID_ALPHA },
                ..line
            },
        ));
        if labels_fit {
            let origin = point(
                x.min(bounds.origin.x + px(width - LABEL_WIDTH))
                    .max(bounds.origin.x),
                bounds.origin.y + px(height - LABEL_HEIGHT),
            );
            paint_label(
                curve_view::format_value_label(value),
                origin,
                label,
                background,
                window,
                cx,
            );
        }
    }

    // Output axis: horizontal lines, labels along the left edge.
    for value in outputs {
        let widget = transform.data_to_widget(ViewPoint::new(min_x, value));
        let y = bounds.origin.y + px(widget.y as f32);
        let zero = value.abs() < f64::EPSILON;
        window.paint_quad(fill(
            Bounds::new(point(bounds.origin.x, y), size(bounds.size.width, px(1.0))),
            Hsla {
                a: if zero { GRID_ZERO_ALPHA } else { GRID_ALPHA },
                ..line
            },
        ));
        if labels_fit {
            let top = (y - px(LABEL_HEIGHT / 2.0))
                .max(bounds.origin.y)
                .min(bounds.origin.y + px(height - LABEL_HEIGHT));
            paint_label(
                curve_view::format_value_label(value),
                point(bounds.origin.x + px(2.0), top),
                label,
                background,
                window,
                cx,
            );
        }
    }
}

/// How the control points of a curve are drawn: their colour, the colour of
/// the selected one and of the Bézier handles, and which point is selected.
#[derive(Clone, Copy)]
struct PointPaint {
    color: Hsla,
    accent: Hsla,
    selected: Option<f32>,
}

/// Paints the curve polyline, and optionally its control points, into
/// `bounds`.
fn paint_curve(
    bounds: Bounds<Pixels>,
    curve: &CurveParam,
    view: CurveView,
    stroke: Hsla,
    point_color: Option<PointPaint>,
    window: &mut Window,
) {
    let width: f32 = bounds.size.width.into();
    let height: f32 = bounds.size.height.into();
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let transform = transform_for(view, (width, height));
    let samples = ((width.ceil() as usize).saturating_mul(2)).clamp(2, MAX_SAMPLES);

    let mut path = PathBuilder::stroke(px(1.5));
    for step in 0..samples {
        let t = step as f64 / (samples - 1) as f64;
        let x = transform.data_min.x + (transform.data_max.x - transform.data_min.x) * t;
        let widget = transform.data_to_widget(ViewPoint::new(x, curve.evaluate(x as f32) as f64));
        let position = point(
            bounds.origin.x + px(widget.x as f32),
            bounds.origin.y + px(widget.y as f32),
        );
        if step == 0 {
            path.move_to(position);
        } else {
            path.line_to(position);
        }
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, stroke);
    }

    let Some(paint) = point_color else {
        return;
    };
    let dot = |center: Point<Pixels>, radius: f32, color: Hsla, window: &mut Window| {
        window.paint_quad(
            fill(
                Bounds::new(
                    point(center.x - px(radius), center.y - px(radius)),
                    size(px(radius * 2.0), px(radius * 2.0)),
                ),
                color,
            )
            .corner_radii(px(radius)),
        );
    };
    let points = curve.points();
    for (index, control) in points.iter().enumerate() {
        let widget = transform.data_to_widget(ViewPoint::new(control.x as f64, control.y as f64));
        let center = point(
            bounds.origin.x + px(widget.x as f32),
            bounds.origin.y + px(widget.y as f32),
        );

        // Bézier handles first, so the anchor sits on top of its own lines.
        let (incoming, outgoing) = handle_visibility(points, index);
        for part in [HitPart::TangentIn, HitPart::TangentOut] {
            let shown = match part {
                HitPart::TangentIn => incoming,
                _ => outgoing,
            };
            if !shown {
                continue;
            }
            let handle = handle_position(control, part, transform);
            let end = point(
                bounds.origin.x + px(handle.x as f32),
                bounds.origin.y + px(handle.y as f32),
            );
            let mut line = PathBuilder::stroke(px(1.0));
            line.move_to(center);
            line.line_to(end);
            if let Ok(line) = line.build() {
                window.paint_path(line, paint.accent);
            }
            dot(end, HANDLE_RADIUS, paint.accent, window);
        }

        let selected = paint
            .selected
            .is_some_and(|x| x.total_cmp(&control.x).is_eq());
        let (radius, color) = if selected {
            (POINT_RADIUS + 1.5, paint.accent)
        } else {
            (POINT_RADIUS, paint.color)
        };
        dot(center, radius, color, window);
    }
}

/// A small non-interactive preview of `curve`, for the collapsed row.
pub fn curve_thumbnail(curve: CurveParam, stroke: Hsla) -> impl IntoElement {
    let view = fit_view(&curve);
    canvas(
        |_bounds, _window, _cx| (),
        move |bounds, (), window, _cx| {
            paint_curve(bounds, &curve, view, stroke, None, window);
        },
    )
    .size_full()
}

/// The inline curve editor element. Rebuilt each frame from its state entity.
#[derive(IntoElement)]
pub struct ParamCurveEditor {
    state: Entity<ParamCurveEditorState>,
}

impl ParamCurveEditor {
    pub fn new(state: &Entity<ParamCurveEditorState>) -> Self {
        Self {
            state: state.clone(),
        }
    }
}

/// One interpolation button of the toolbar. Active when the selected point
/// already uses that mode; disabled-looking when nothing is selected.
fn interpolation_button(
    state: &Entity<ParamCurveEditorState>,
    interpolation: Interpolation,
    current: Option<Interpolation>,
    active: Hsla,
    muted: Hsla,
    window: &mut Window,
) -> Stateful<Div> {
    let (icon, tooltip) = match interpolation {
        Interpolation::Bezier => (
            RavelIcon::InterpolationBezier,
            "timeline.interpolation.bezier",
        ),
        Interpolation::Linear => (
            RavelIcon::InterpolationLinear,
            "timeline.interpolation.linear",
        ),
        Interpolation::Step => (RavelIcon::InterpolationStep, "timeline.interpolation.step"),
    };
    let color = if current == Some(interpolation) {
        active
    } else {
        muted
    };
    div()
        .id(SharedString::from(format!("curve-interpolation-{icon:?}")))
        .flex_shrink_0()
        .cursor_pointer()
        .child(Icon::new(icon).size_3().text_color(color))
        .tooltip(move |window, cx| Tooltip::new(ravel_i18n::translate(tooltip)).build(window, cx))
        .on_mouse_down(
            MouseButton::Left,
            window.listener_for(state, move |state, _e: &MouseDownEvent, _window, cx| {
                state.set_selected_interpolation(interpolation, cx);
            }),
        )
}

impl RenderOnce for ParamCurveEditor {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let entity_id = self.state.entity_id();
        let state = self.state.read(cx);
        let curve = state.curve.clone();
        let view = state.view();
        let selected = state.selected;
        let selected_point = state.selected_point();
        let bounds = state.bounds.clone();
        let colors = cx.theme().colors;

        let graph = div()
            .id(("param-curve-graph", entity_id))
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
                    move |canvas_bounds, (), window, cx| {
                        paint_grid(
                            canvas_bounds,
                            view,
                            colors.foreground,
                            colors.muted_foreground,
                            colors.background,
                            window,
                            cx,
                        );
                        paint_curve(
                            canvas_bounds,
                            &curve,
                            view,
                            colors.primary,
                            Some(PointPaint {
                                color: colors.foreground,
                                accent: colors.accent_foreground,
                                selected,
                            }),
                            window,
                        );
                    },
                )
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                window.listener_for(&self.state, |state, e: &MouseDownEvent, _window, cx| {
                    let pointer = state.local(e.position);
                    state.pointer_down(pointer, e.click_count, cx);
                }),
            )
            .on_drag(DragCurvePoint(entity_id), |drag, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| drag.clone())
            })
            .on_drag_move(window.listener_for(
                &self.state,
                move |state, e: &DragMoveEvent<DragCurvePoint>, _window, cx| {
                    let DragCurvePoint(id) = e.drag(cx);
                    if *id != entity_id {
                        return;
                    }
                    let pointer = state.local(e.event.position);
                    let snap = e.event.modifiers.shift;
                    state.drag_to_with_modifiers(pointer, snap, cx);
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
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_1()
            .py(px(2.0))
            .text_xs()
            .text_color(colors.muted_foreground);
        let mut modes = div().flex().items_center().gap_1();
        for interpolation in [
            Interpolation::Linear,
            Interpolation::Bezier,
            Interpolation::Step,
        ] {
            modes = modes.child(interpolation_button(
                &self.state,
                interpolation,
                selected_point.map(|point| point.interpolation),
                colors.primary,
                colors.muted_foreground,
                window,
            ));
        }
        toolbar = toolbar.child(modes);

        div()
            .id(("param-curve-editor", entity_id))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(colors.background)
            .border_1()
            .border_color(colors.border)
            .rounded(px(2.0))
            .child(graph)
            .child(toolbar)
    }
}

#[cfg(test)]
mod tests {
    // Selective import: `use super::*` would pull in `gpui::test` and hijack
    // the built-in `#[test]` attribute (recursive expansion).
    use super::{
        CurveView, HIT_RADIUS, HitPart, MIN_POINTS, ParamCurveEditorState, ParamCurveEvent,
        ParamCurveHit, ViewPoint, begin_point_drag, begin_tangent_drag, drag_point_to,
        drag_tangent_to, fit_view, grid_ticks, hit_point, hit_test, labels_fit,
        set_curve_interpolation, transform_for, x_is_editable,
    };
    use gpui::{AppContext as _, TestAppContext};
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_core::param_curve::{CurveParam, CurvePoint};
    use ravel_core::types::Vec2;
    use std::cell::RefCell;
    use std::rc::Rc;

    const SIZE: (f32, f32) = (200.0, 100.0);

    fn curve() -> CurveParam {
        CurveParam::linear([(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)])
    }

    fn unit_view() -> CurveView {
        CurveView {
            x: (0.0, 1.0),
            y: (0.0, 1.0),
        }
    }

    #[test]
    fn an_empty_curve_is_shown_over_the_unit_square() {
        assert_eq!(fit_view(&CurveParam::from_points([])), unit_view());
    }

    #[test]
    fn the_fitted_view_keeps_the_end_points_off_the_border() {
        let view = fit_view(&curve());
        assert!(view.x.0 < 0.0 && view.x.1 > 1.0);
        assert!(view.y.0 < 0.0 && view.y.1 > 1.0);
    }

    /// A flat curve has no vertical extent to fit; it still needs a range to
    /// be drawn in.
    #[test]
    fn a_flat_curve_gets_a_finite_vertical_range() {
        let view = fit_view(&CurveParam::linear([(0.0, 0.5), (1.0, 0.5)]));
        assert!(view.y.1 - view.y.0 > 0.0);
    }

    /// The grid is derived from the visible range, so changing the range
    /// moves the ticks with it — the labels always say what is on screen.
    #[test]
    fn grid_ticks_follow_the_visible_range() {
        let (inputs, outputs) = grid_ticks(unit_view(), SIZE);
        assert!(inputs.iter().all(|v| (0.0..=1.0).contains(v)), "{inputs:?}");
        assert!(
            outputs.iter().all(|v| (0.0..=1.0).contains(v)),
            "{outputs:?}"
        );
        assert!(
            outputs.contains(&0.0),
            "the zero line is drawn: {outputs:?}"
        );

        let zoomed = CurveView {
            x: (10.0, 12.0),
            y: (-5.0, -3.0),
        };
        let (inputs, outputs) = grid_ticks(zoomed, SIZE);
        assert!(
            inputs.iter().all(|v| (10.0..=12.0).contains(v)) && !inputs.is_empty(),
            "{inputs:?}"
        );
        assert!(
            outputs.iter().all(|v| (-5.0..=-3.0).contains(v)) && !outputs.is_empty(),
            "{outputs:?}"
        );
    }

    /// A row dragged down to a sliver keeps its grid lines but drops the
    /// numbers, which would only overlap each other.
    #[test]
    fn a_short_axis_drops_its_labels() {
        assert!(labels_fit(SIZE));
        assert!(!labels_fit((SIZE.0, 20.0)));
        assert!(!labels_fit((20.0, SIZE.1)));
    }

    #[test]
    fn hit_testing_finds_the_nearest_control_point() {
        let transform = transform_for(unit_view(), SIZE);
        // (0.5, 0.5) maps to the widget centre; widget y grows downwards.
        assert_eq!(
            hit_point(&curve(), transform, ViewPoint::new(100.0, 50.0), HIT_RADIUS),
            Some(0.5)
        );
        assert_eq!(
            hit_point(&curve(), transform, ViewPoint::new(140.0, 50.0), HIT_RADIUS),
            None
        );
    }

    /// Points are identified by their input value, so a drag may not push one
    /// point onto (or past) its neighbour — that would silently merge them.
    #[test]
    fn a_dragged_point_stays_between_its_neighbours() {
        let curve = curve();
        let transform = transform_for(unit_view(), SIZE);
        let drag =
            begin_point_drag(&curve, 0.5, ViewPoint::new(100.0, 50.0), transform).expect("drag");
        let (x, _) = drag_point_to(drag, ViewPoint::new(400.0, 50.0));
        assert!(x < 1.0, "clamped below the next point: {x}");
        let (x, _) = drag_point_to(drag, ViewPoint::new(-400.0, 50.0));
        assert!(x > 0.0, "clamped above the previous point: {x}");
    }

    /// Neighbours can sit closer together than the nominal gap — points are
    /// added by double-click, so nothing stops two from landing a fraction
    /// apart. Clamping with a fixed gap then inverts and lands the dragged
    /// point on a neighbour, where `insert_point` overwrites it and the point
    /// silently disappears.
    #[test]
    fn a_drag_between_close_neighbours_never_overwrites_one() {
        let transform = transform_for(unit_view(), SIZE);
        for spacing in [1.0e-3f32, 1.0e-4, 5.0e-5, 2.5e-5, 1.0e-6, 1.0e-7] {
            let mid = 0.5f32;
            let (lower, upper) = (mid - spacing, mid + spacing);
            let curve = CurveParam::linear([(lower, 0.0), (mid, 0.5), (upper, 1.0)]);
            assert_eq!(curve.len(), 3, "spacing {spacing} vanished in f32");
            let drag = begin_point_drag(&curve, mid, ViewPoint::new(100.0, 50.0), transform)
                .expect("drag");
            for pointer_x in [-400.0, -1.0, 99.0, 101.0, 201.0, 400.0] {
                let (x, y) = drag_point_to(drag, ViewPoint::new(pointer_x, 50.0));
                assert!(
                    x > lower && x < upper,
                    "spacing {spacing}, pointer {pointer_x}: {x} left ({lower}, {upper})"
                );
                let mut moved = curve.clone();
                assert!(moved.move_point(mid, x, y));
                assert_eq!(
                    moved.len(),
                    3,
                    "spacing {spacing}, pointer {pointer_x}: a neighbour was overwritten"
                );
            }
        }
    }

    /// Even pinched horizontally, the vertical half of the drag still applies
    /// — refusing the whole gesture would make such a point uneditable.
    #[test]
    fn a_pinched_drag_still_moves_the_point_vertically() {
        let mid = 0.5f32;
        let curve = CurveParam::linear([(mid - 1.0e-7, 0.0), (mid, 0.5), (mid + 1.0e-7, 1.0)]);
        let transform = transform_for(unit_view(), SIZE);
        let drag =
            begin_point_drag(&curve, mid, ViewPoint::new(100.0, 50.0), transform).expect("drag");
        // 10px up = +0.1 in y.
        let (_, y) = drag_point_to(drag, ViewPoint::new(400.0, 40.0));
        assert!((y - 0.6).abs() < 1e-5, "{y}");
    }

    /// The two outer points are the curve's domain: their inputs are pinned,
    /// so no drag can shorten the domain or push an end off the view.
    #[test]
    fn the_outer_points_keep_their_input_value() {
        let curve = curve();
        assert!(!x_is_editable(&curve, 0.0), "first point");
        assert!(x_is_editable(&curve, 0.5), "middle point");
        assert!(!x_is_editable(&curve, 1.0), "last point");

        let transform = transform_for(unit_view(), SIZE);
        for (x, pointer) in [
            (0.0f32, ViewPoint::new(0.0, 100.0)),
            (1.0, ViewPoint::new(200.0, 0.0)),
        ] {
            let drag = begin_point_drag(&curve, x, pointer, transform).expect("drag");
            for dx in [-500.0, -20.0, 20.0, 500.0] {
                let (moved_x, moved_y) =
                    drag_point_to(drag, ViewPoint::new(pointer.x + dx, pointer.y - 10.0));
                assert_eq!(moved_x, x, "the end point moved horizontally");
                assert!(
                    (moved_y - (curve.evaluate(x) + 0.1)).abs() < 1e-5,
                    "but still follows the pointer vertically: {moved_y}"
                );
            }
        }
    }

    #[test]
    fn dragging_moves_the_point_by_the_pointer_delta() {
        let curve = curve();
        let transform = transform_for(unit_view(), SIZE);
        let drag =
            begin_point_drag(&curve, 0.5, ViewPoint::new(100.0, 50.0), transform).expect("drag");
        // 20px right = +0.1 in x, 10px up = +0.1 in y.
        let (x, y) = drag_point_to(drag, ViewPoint::new(120.0, 40.0));
        assert!((x - 0.6).abs() < 1e-5, "{x}");
        assert!((y - 0.6).abs() < 1e-5, "{y}");
    }

    #[test]
    fn a_drag_cannot_start_on_a_point_that_is_gone() {
        assert!(
            begin_point_drag(
                &curve(),
                0.25,
                ViewPoint::new(0.0, 0.0),
                transform_for(unit_view(), SIZE)
            )
            .is_none()
        );
    }

    /// A Bézier segment shows the handle that leaves its left point and the
    /// one that arrives at its right point — the same rule the Timeline
    /// graph editor applies to keyframes.
    #[test]
    fn only_bezier_segments_expose_their_handles() {
        let transform = transform_for(unit_view(), SIZE);
        let linear = curve();
        assert!(
            begin_tangent_drag(
                &linear,
                ParamCurveHit {
                    x: 0.0,
                    part: HitPart::TangentOut
                },
                ViewPoint::default(),
                transform,
            )
            .is_none(),
            "a linear segment has no handles"
        );

        let mut bezier = linear.clone();
        assert!(set_curve_interpolation(
            &mut bezier,
            0.0,
            Interpolation::Bezier
        ));
        assert!(
            begin_tangent_drag(
                &bezier,
                ParamCurveHit {
                    x: 0.0,
                    part: HitPart::TangentOut
                },
                ViewPoint::default(),
                transform,
            )
            .is_some(),
            "the outgoing handle of the Bezier point"
        );
        assert!(
            begin_tangent_drag(
                &bezier,
                ParamCurveHit {
                    x: 0.5,
                    part: HitPart::TangentIn
                },
                ViewPoint::default(),
                transform,
            )
            .is_some(),
            "and the incoming handle of the next point"
        );
        assert!(
            begin_tangent_drag(
                &bezier,
                ParamCurveHit {
                    x: 0.0,
                    part: HitPart::TangentIn
                },
                ViewPoint::default(),
                transform,
            )
            .is_none(),
            "but not the incoming handle of the first point"
        );
    }

    /// Switching to Bezier seeds one-third handles along the existing straight
    /// line: the shape is unchanged but both controls become grabbable. This
    /// is what `keyframes::set_curve_interpolation` does for keyframes.
    #[test]
    fn switching_to_bezier_seeds_grabbable_handles_without_moving_the_curve() {
        let mut curve = CurveParam::linear([(0.0, 0.0), (3.0, 3.0)]);
        let before: Vec<f32> = (0..=6).map(|i| curve.evaluate(i as f32 * 0.5)).collect();
        assert!(set_curve_interpolation(
            &mut curve,
            0.0,
            Interpolation::Bezier
        ));

        let points = curve.points();
        assert_eq!(points[0].tangent_out, Vec2(1.0, 1.0));
        assert_eq!(points[1].tangent_in, Vec2(-1.0, -1.0));
        for (i, expected) in before.into_iter().enumerate() {
            let sampled = curve.evaluate(i as f32 * 0.5);
            assert!((sampled - expected).abs() < 1e-4, "{sampled} vs {expected}");
        }

        // A handle the user already shaped survives a mode round trip.
        assert!(set_curve_interpolation(
            &mut curve,
            0.0,
            Interpolation::Linear
        ));
        assert!(set_curve_interpolation(
            &mut curve,
            0.0,
            Interpolation::Bezier
        ));
        assert_eq!(curve.points()[0].tangent_out, Vec2(1.0, 1.0));
    }

    /// A handle may not reach past the adjacent control point, matching the
    /// Timeline graph editor's clamp.
    #[test]
    fn a_handle_cannot_reach_past_the_adjacent_point() {
        let mut curve = CurveParam::linear([(0.0, 0.0), (1.0, 1.0)]);
        assert!(set_curve_interpolation(
            &mut curve,
            0.0,
            Interpolation::Bezier
        ));
        let transform = transform_for(unit_view(), SIZE);
        let hit = ParamCurveHit {
            x: 0.0,
            part: HitPart::TangentOut,
        };
        let start = transform.data_to_widget(ViewPoint::new(1.0 / 3.0, 1.0 / 3.0));
        let drag = begin_tangent_drag(&curve, hit, start, transform).expect("drag");

        let far = drag_tangent_to(drag, ViewPoint::new(2_000.0, 0.0), false);
        assert!(far.0 <= 1.0, "clamped at the next point: {far:?}");
        let back = drag_tangent_to(drag, ViewPoint::new(-2_000.0, 0.0), false);
        assert!(
            back.0 >= 0.0,
            "an outgoing handle never points backwards: {back:?}"
        );
    }

    /// Shift snaps the handle onto a screen-space diagonal, through the same
    /// helper the Timeline graph editor uses.
    #[test]
    fn shift_snaps_a_handle_to_the_screen_diagonals() {
        let mut curve = CurveParam::linear([(0.0, 0.0), (1.0, 1.0)]);
        assert!(set_curve_interpolation(
            &mut curve,
            0.0,
            Interpolation::Bezier
        ));
        let transform = transform_for(unit_view(), SIZE);
        let hit = ParamCurveHit {
            x: 0.0,
            part: HitPart::TangentOut,
        };
        let start = transform.data_to_widget(ViewPoint::new(1.0 / 3.0, 1.0 / 3.0));
        let drag = begin_tangent_drag(&curve, hit, start, transform).expect("drag");

        let snapped = drag_tangent_to(drag, ViewPoint::new(start.x, start.y - 30.0), true);
        // The widget is 200x100 over a unit square, so one data unit is 200px
        // horizontally and 100px vertically; a 45-degree screen direction is
        // an equal pixel delta.
        let screen_dx = snapped.0 as f64 * 200.0;
        let screen_dy = snapped.1 as f64 * 100.0;
        assert!(
            (screen_dx.abs() - screen_dy.abs()).abs() < 1.0,
            "{screen_dx} vs {screen_dy}"
        );
    }

    #[test]
    fn hit_testing_prefers_an_anchor_over_a_handle_on_top_of_it() {
        let mut curve = CurveParam::linear([(0.0, 0.0), (1.0, 1.0)]);
        assert!(set_curve_interpolation(
            &mut curve,
            0.0,
            Interpolation::Bezier
        ));
        let transform = transform_for(unit_view(), SIZE);
        let anchor = transform.data_to_widget(ViewPoint::new(0.0, 0.0));
        assert_eq!(
            hit_test(&curve, transform, anchor, HIT_RADIUS),
            Some(ParamCurveHit {
                x: 0.0,
                part: HitPart::Keyframe
            })
        );
        let handle = transform.data_to_widget(ViewPoint::new(1.0 / 3.0, 1.0 / 3.0));
        assert_eq!(
            hit_test(&curve, transform, handle, HIT_RADIUS),
            Some(ParamCurveHit {
                x: 0.0,
                part: HitPart::TangentOut
            })
        );
    }

    type EventLog = Rc<RefCell<Vec<(bool, CurveParam)>>>;

    /// Widget position of a data point under the state's current view.
    fn widget_pos(state: &ParamCurveEditorState, x: f32, y: f32) -> ViewPoint {
        transform_for(state.view(), SIZE).data_to_widget(ViewPoint::new(x as f64, y as f64))
    }

    /// Records `(committed, curve)` for every event the state emits.
    fn state_with_log(
        cx: &mut TestAppContext,
        curve: CurveParam,
    ) -> (gpui::Entity<ParamCurveEditorState>, EventLog) {
        let state = cx.new(|_| {
            let mut state = ParamCurveEditorState::new(curve);
            state.set_bounds_for_tests((0.0, 0.0), SIZE);
            state.value_range = Some((0.0, 1.0));
            state
        });
        let log: EventLog = Rc::default();
        let sink = log.clone();
        cx.update(|cx| {
            cx.subscribe(
                &state,
                move |_state, event: &ParamCurveEvent, _cx| match event {
                    ParamCurveEvent::Change(curve) => {
                        sink.borrow_mut().push((false, curve.clone()))
                    }
                    ParamCurveEvent::Commit(curve) => sink.borrow_mut().push((true, curve.clone())),
                },
            )
            .detach();
        });
        (state, log)
    }

    /// The gesture contract: live changes while dragging, exactly one commit
    /// at the end — the host records one undo step per gesture.
    #[gpui::test]
    fn a_point_drag_emits_live_changes_and_one_commit(cx: &mut TestAppContext) {
        let (state, log) = state_with_log(cx, curve());
        state.update(cx, |state, cx| {
            state.pointer_down(ViewPoint::new(100.0, 50.0), 1, cx);
            assert!(state.is_dragging());
            state.drag_to(ViewPoint::new(110.0, 40.0), cx);
            state.drag_to(ViewPoint::new(120.0, 30.0), cx);
            state.end_drag(cx);
            assert!(!state.is_dragging());
        });
        let log = log.borrow();
        assert_eq!(log.iter().filter(|(commit, _)| *commit).count(), 1);
        assert!(log.iter().filter(|(commit, _)| !*commit).count() >= 2);
        // The committed curve carries the moved point.
        let (_, committed) = log.last().expect("committed");
        assert_eq!(committed.len(), 3);
        assert!(committed.points().iter().any(|p| (p.y - 0.7).abs() < 1e-4));
    }

    /// Switching a point to Bezier and dragging its handle changes the curve
    /// shape, with one undo step per gesture.
    #[gpui::test]
    fn a_handle_drag_reshapes_the_curve_in_one_gesture(cx: &mut TestAppContext) {
        let (state, log) = state_with_log(cx, CurveParam::linear([(0.0, 0.0), (1.0, 1.0)]));
        state.update(cx, |state, cx| {
            // Select the first point, then make its segment Bezier.
            let anchor = widget_pos(state, 0.0, 0.0);
            state.pointer_down(anchor, 1, cx);
            state.end_drag(cx);
            state.set_selected_interpolation(Interpolation::Bezier, cx);
            assert_eq!(
                state.curve().points()[0].interpolation,
                Interpolation::Bezier
            );
        });
        assert_eq!(log.borrow().len(), 1, "the mode switch is one commit");
        assert!(log.borrow()[0].0);

        let midpoint_before = state.read_with(cx, |state, _| state.curve().evaluate(0.5));
        state.update(cx, |state, cx| {
            let handle = widget_pos(state, 1.0 / 3.0, 1.0 / 3.0);
            state.pointer_down(handle, 1, cx);
            assert!(state.is_dragging());
            state.drag_to(ViewPoint::new(handle.x + 20.0, handle.y), cx);
            state.drag_to(ViewPoint::new(handle.x + 40.0, handle.y), cx);
            state.end_drag(cx);
        });

        let log = log.borrow();
        let commits = log.iter().filter(|(commit, _)| *commit).count();
        assert_eq!(commits, 2, "the mode switch and the handle drag: {commits}");
        let (_, committed) = log.last().expect("committed");
        assert!(
            (committed.evaluate(0.5) - midpoint_before).abs() > 1e-3,
            "the handle drag changed the shape"
        );
        assert_eq!(committed.len(), 2, "and added no point");
    }

    #[gpui::test]
    fn a_press_that_misses_every_point_starts_no_drag(cx: &mut TestAppContext) {
        let (state, log) = state_with_log(cx, curve());
        state.update(cx, |state, cx| {
            state.pointer_down(ViewPoint::new(10.0, 90.0), 1, cx);
            assert!(!state.is_dragging());
            state.drag_to(ViewPoint::new(40.0, 60.0), cx);
            state.end_drag(cx);
        });
        assert!(log.borrow().is_empty());
    }

    #[gpui::test]
    fn a_double_click_on_empty_space_adds_a_point(cx: &mut TestAppContext) {
        let (state, log) = state_with_log(cx, CurveParam::linear([(0.0, 0.0), (1.0, 1.0)]));
        state.update(cx, |state, cx| {
            // Aim at a spot the identity curve does not pass through, so the
            // added point is placed where the pointer was, not on the curve.
            let pointer = widget_pos(state, 0.25, 0.75);
            state.pointer_down(pointer, 2, cx);
        });
        let log = log.borrow();
        assert_eq!(log.len(), 1, "one commit, no live change");
        let (commit, curve) = &log[0];
        assert!(commit);
        assert_eq!(curve.len(), 3);
        let added = curve.points()[1];
        assert!((added.x - 0.25).abs() < 1e-4, "{added:?}");
        assert!((added.y - 0.75).abs() < 1e-4, "{added:?}");
    }

    /// A point added inside a Step segment stays a Step point, so adding a
    /// point never changes the curve's character.
    #[gpui::test]
    fn an_added_point_inherits_the_segment_interpolation(cx: &mut TestAppContext) {
        let stepped = CurveParam::from_points([
            CurvePoint::new(0.0, 0.0, Interpolation::Step),
            CurvePoint::new(1.0, 1.0, Interpolation::Step),
        ]);
        let (state, log) = state_with_log(cx, stepped);
        state.update(cx, |state, cx| {
            state.pointer_down(ViewPoint::new(50.0, 25.0), 2, cx);
        });
        let (_, curve) = log.borrow()[0].clone();
        assert_eq!(curve.points()[1].interpolation, Interpolation::Step);
    }

    #[gpui::test]
    fn a_double_click_on_a_point_removes_it(cx: &mut TestAppContext) {
        let (state, log) = state_with_log(cx, curve());
        state.update(cx, |state, cx| {
            state.pointer_down(ViewPoint::new(100.0, 50.0), 2, cx);
        });
        let log = log.borrow();
        assert_eq!(log.len(), 1);
        let (commit, curve) = &log[0];
        assert!(commit);
        assert_eq!(curve.len(), 2);
        assert!(!curve.points().iter().any(|p| p.x == 0.5));
    }

    /// Removing an outer point would move the domain edge onto its
    /// neighbour — the same change pinned inputs rule out.
    #[gpui::test]
    fn an_outer_point_cannot_be_removed(cx: &mut TestAppContext) {
        let (state, log) = state_with_log(cx, curve());
        state.update(cx, |state, cx| {
            for x in [0.0f32, 1.0] {
                let pointer = widget_pos(state, x, state.curve().evaluate(x));
                state.pointer_down(pointer, 2, cx);
            }
            assert_eq!(state.curve().len(), 3, "both ends survived");
        });
        assert!(log.borrow().is_empty(), "no edit, no undo step");
    }

    /// Removal stops at the minimum: a curve that collapsed to a constant (or
    /// to the implicit identity) would look like an empty editor.
    #[gpui::test]
    fn removal_keeps_the_minimum_number_of_points(cx: &mut TestAppContext) {
        let (state, log) = state_with_log(cx, CurveParam::linear([(0.0, 0.0), (1.0, 1.0)]));
        state.update(cx, |state, cx| {
            let pointer = widget_pos(state, 0.0, 0.0);
            state.pointer_down(pointer, 2, cx);
            assert_eq!(state.curve().len(), MIN_POINTS);
        });
        assert!(log.borrow().is_empty(), "no edit, no undo step");
    }

    /// The document keeps refreshing the panel during a gesture; the drag
    /// must stay the source of truth until it ends.
    #[gpui::test]
    fn an_external_refresh_never_interrupts_a_drag(cx: &mut TestAppContext) {
        let (state, _log) = state_with_log(cx, curve());
        state.update(cx, |state, cx| {
            state.pointer_down(ViewPoint::new(100.0, 50.0), 1, cx);
            state.drag_to(ViewPoint::new(120.0, 30.0), cx);
            state.set_curve(CurveParam::identity());
            assert_eq!(state.curve().len(), 3, "the drag owns the curve");
            state.end_drag(cx);
            state.set_curve(CurveParam::identity());
            assert_eq!(state.curve().len(), 2, "and releases it on release");
        });
    }

    /// The axes must not rescale under the pointer mid-drag, so the view is
    /// frozen for the gesture.
    #[gpui::test]
    fn the_view_is_frozen_while_dragging(cx: &mut TestAppContext) {
        let (state, _log) = state_with_log(cx, curve());
        state.update(cx, |state, cx| {
            state.value_range = None;
            let before = state.view();
            state.pointer_down(ViewPoint::new(100.0, 50.0), 1, cx);
            state.drag_to(ViewPoint::new(120.0, 0.0), cx);
            assert_eq!(state.view(), before);
        });
    }
}
