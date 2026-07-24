// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Axis-agnostic curve-editor geometry.
//!
//! The normalized/widget transforms, hit testing, and drag transitions in this
//! module are GPUI-independent. [`sample_curve`] is the small adapter from the
//! reusable geometry to Ravel's animation model; it deliberately delegates to
//! [`KeyframeCurve::sample`], the authoritative curve evaluation path.

use std::collections::{BTreeSet, HashSet};
use std::ops::ControlFlow;
use std::sync::Arc;

use gpui::{
    Bounds, Hsla, IntoElement, ParentElement as _, PathBuilder, Pixels, Styled as _, Window,
    canvas, div, fill, point, px, size,
};
use ravel_core::animation::{Interpolation, Keyframe, KeyframeCurve};
use ravel_core::types::Vec2;

const MIN_SPAN: f64 = 1.0e-12;
const MAX_PAINT_SAMPLES: usize = 4_096;
const MAX_PAINT_CONTROL_POINTS: usize = 4_096;

/// A point in either data, normalized, or widget coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CurvePoint {
    pub x: f64,
    pub y: f64,
}

impl CurvePoint {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Data-to-widget mapping. Widget y increases downwards; data y increases up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveTransform {
    pub data_min: CurvePoint,
    pub data_max: CurvePoint,
    pub widget_size: CurvePoint,
}

impl CurveTransform {
    pub fn new(data_min: CurvePoint, data_max: CurvePoint, widget_size: CurvePoint) -> Self {
        let (min_x, max_x) = normalized_axis_bounds(data_min.x, data_max.x);
        let (min_y, max_y) = normalized_axis_bounds(data_min.y, data_max.y);
        Self {
            data_min: CurvePoint::new(min_x, min_y),
            data_max: CurvePoint::new(max_x, max_y),
            widget_size: CurvePoint::new(finite_span(widget_size.x), finite_span(widget_size.y)),
        }
    }

    pub fn data_to_normalized(self, point: CurvePoint) -> CurvePoint {
        let span = self.data_span();
        CurvePoint::new(
            (point.x - self.data_min.x) / span.x,
            1.0 - (point.y - self.data_min.y) / span.y,
        )
    }

    pub fn normalized_to_data(self, point: CurvePoint) -> CurvePoint {
        let span = self.data_span();
        CurvePoint::new(
            self.data_min.x + point.x * span.x,
            self.data_min.y + (1.0 - point.y) * span.y,
        )
    }

    pub fn data_to_widget(self, point: CurvePoint) -> CurvePoint {
        let normalized = self.data_to_normalized(point);
        CurvePoint::new(
            normalized.x * self.widget_size.x,
            normalized.y * self.widget_size.y,
        )
    }

    pub fn widget_to_data(self, point: CurvePoint) -> CurvePoint {
        let normalized = CurvePoint::new(
            point.x / self.widget_span().x,
            point.y / self.widget_span().y,
        );
        self.normalized_to_data(normalized)
    }

    fn data_span(self) -> CurvePoint {
        CurvePoint::new(
            self.data_max.x - self.data_min.x,
            self.data_max.y - self.data_min.y,
        )
    }

    fn widget_span(self) -> CurvePoint {
        CurvePoint::new(
            self.widget_size.x.abs().max(MIN_SPAN),
            self.widget_size.y.abs().max(MIN_SPAN),
        )
    }
}

fn normalized_axis_bounds(a: f64, b: f64) -> (f64, f64) {
    if !a.is_finite() || !b.is_finite() {
        return (0.0, 1.0);
    }
    let (min, max) = if a <= b { (a, b) } else { (b, a) };
    if max - min < MIN_SPAN {
        (min - 0.5, max + 0.5)
    } else {
        (min, max)
    }
}

fn finite_span(value: f64) -> f64 {
    if value.is_finite() {
        value.abs().max(MIN_SPAN)
    } else {
        1.0
    }
}

/// One integer-axis sample produced by the core animation evaluator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplePoint {
    pub frame: u64,
    pub value: f32,
}

/// Samples every integer frame in the inclusive range.
pub fn sample_curve(
    curve: &KeyframeCurve,
    frames: std::ops::RangeInclusive<u64>,
) -> Vec<SamplePoint> {
    frames
        .map(|frame| SamplePoint {
            frame,
            value: curve.sample(frame),
        })
        .collect()
}

fn visit_curve_samples_for_view(
    curve: &KeyframeCurve,
    frame_offset: i64,
    min_x: f64,
    max_x: f64,
    max_samples: usize,
    mut visit: impl FnMut(i64, SamplePoint),
) {
    if max_samples == 0 {
        return;
    }
    let (min_x, max_x) = normalized_axis_bounds(min_x, max_x);
    let first = min_x.floor().max(i64::MIN as f64) as i64;
    let last = max_x.ceil().min(i64::MAX as f64) as i64;

    // Segment boundaries are mandatory. The frame immediately before each
    // key preserves Step's vertical transition when regular samples are
    // decimated. Mandatory samples win if they alone exceed `max_samples`;
    // the cap limits only smooth-span samples, never semantic boundaries.
    let mut frames = BTreeSet::new();
    frames.insert(first);
    frames.insert(last);
    for key in visible_keys(curve, frame_offset, min_x, max_x) {
        let frame = offset_frame(key.frame, frame_offset);
        if frame >= first && frame <= last {
            frames.insert(frame);
        }
        if frame > first && frame - 1 <= last {
            frames.insert(frame - 1);
        }
    }

    let regular_budget = max_samples.saturating_sub(frames.len());
    if first < last && regular_budget > 0 {
        let intervals = (last as i128 - first as i128) as u128;
        let step = intervals
            .div_ceil((regular_budget + 1) as u128)
            .max(1)
            .min(i64::MAX as u128) as i64;
        let mut frame = first.saturating_add(step);
        while frame < last {
            frames.insert(frame);
            frame = frame.saturating_add(step);
        }
    }

    for composition_frame in frames {
        let local_frame = local_frame(composition_frame, frame_offset);
        visit(
            composition_frame,
            SamplePoint {
                frame: local_frame,
                value: curve.sample(local_frame),
            },
        );
    }
}

fn offset_frame(local_frame: u64, frame_offset: i64) -> i64 {
    (local_frame as i128 + frame_offset as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn local_frame(composition_frame: i64, frame_offset: i64) -> u64 {
    (composition_frame as i128 - frame_offset as i128).clamp(0, u64::MAX as i128) as u64
}

/// The editable part represented by a hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitPart {
    Keyframe,
    TangentIn,
    TangentOut,
}

/// Stable identity of an editable curve point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveHit {
    pub curve: usize,
    pub frame: u64,
    pub part: HitPart,
}

/// A point and its widget-space position, used for painting and hit testing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlPoint {
    pub hit: CurveHit,
    pub position: CurvePoint,
    pub anchor: CurvePoint,
}

/// Returns keyframe anchors and applicable Bézier handles in widget space.
pub fn control_points(
    curve_index: usize,
    curve: &KeyframeCurve,
    transform: CurveTransform,
) -> Vec<ControlPoint> {
    control_points_with_offset(curve_index, curve, 0, transform)
}

/// Like [`control_points`], with a composition-frame offset applied to x.
pub fn control_points_with_offset(
    curve_index: usize,
    curve: &KeyframeCurve,
    frame_offset: i64,
    transform: CurveTransform,
) -> Vec<ControlPoint> {
    let mut points = Vec::new();
    let _ = visit_control_points(curve_index, curve, frame_offset, transform, |point| {
        points.push(point);
        ControlFlow::Continue(())
    });
    points
}

fn visit_control_points(
    curve_index: usize,
    curve: &KeyframeCurve,
    frame_offset: i64,
    transform: CurveTransform,
    mut visit: impl FnMut(ControlPoint) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let keys = curve.keyframes();
    let range = visible_key_range(
        curve,
        frame_offset,
        transform.data_min.x,
        transform.data_max.x,
    );
    for index in range {
        let key = &keys[index];
        let anchor_data = CurvePoint::new(
            offset_frame(key.frame, frame_offset) as f64,
            key.value as f64,
        );
        let anchor = transform.data_to_widget(anchor_data);
        let point = ControlPoint {
            hit: CurveHit {
                curve: curve_index,
                frame: key.frame,
                part: HitPart::Keyframe,
            },
            position: anchor,
            anchor,
        };
        if control_intersects_widget(point, transform.widget_size) && visit(point).is_break() {
            return ControlFlow::Break(());
        }

        if index > 0 && keys[index - 1].interpolation == Interpolation::Bezier {
            let point = handle_point(
                curve_index,
                key,
                HitPart::TangentIn,
                key.tangent_in,
                frame_offset,
                anchor,
                transform,
            );
            if control_intersects_widget(point, transform.widget_size) && visit(point).is_break() {
                return ControlFlow::Break(());
            }
        }
        if index + 1 < keys.len() && key.interpolation == Interpolation::Bezier {
            let point = handle_point(
                curve_index,
                key,
                HitPart::TangentOut,
                key.tangent_out,
                frame_offset,
                anchor,
                transform,
            );
            if control_intersects_widget(point, transform.widget_size) && visit(point).is_break() {
                return ControlFlow::Break(());
            }
        }
    }
    ControlFlow::Continue(())
}

fn visible_key_range(
    curve: &KeyframeCurve,
    frame_offset: i64,
    min_x: f64,
    max_x: f64,
) -> std::ops::Range<usize> {
    let keys = curve.keyframes();
    let local_min = min_x - frame_offset as f64;
    let local_max = max_x - frame_offset as f64;
    let start = keys
        .partition_point(|key| (key.frame as f64) < local_min)
        .saturating_sub(1);
    let end = (keys.partition_point(|key| (key.frame as f64) <= local_max) + 1).min(keys.len());
    start..end
}

fn visible_keys(curve: &KeyframeCurve, frame_offset: i64, min_x: f64, max_x: f64) -> &[Keyframe] {
    let range = visible_key_range(curve, frame_offset, min_x, max_x);
    &curve.keyframes()[range]
}

fn handle_point(
    curve: usize,
    key: &Keyframe,
    part: HitPart,
    tangent: Vec2,
    frame_offset: i64,
    anchor: CurvePoint,
    transform: CurveTransform,
) -> ControlPoint {
    ControlPoint {
        hit: CurveHit {
            curve,
            frame: key.frame,
            part,
        },
        position: transform.data_to_widget(CurvePoint::new(
            offset_frame(key.frame, frame_offset) as f64 + tangent.0 as f64,
            key.value as f64 + tangent.1 as f64,
        )),
        anchor,
    }
}

/// Finds the closest anchor/handle within `radius` widget pixels.
/// Keyframes win ties so zero-length handles remain selectable as anchors.
pub fn hit_test(
    curves: &[&KeyframeCurve],
    transform: CurveTransform,
    pointer: CurvePoint,
    radius: f64,
) -> Option<CurveHit> {
    hit_test_sources(
        curves.iter().map(|curve| (*curve, 0)),
        transform,
        pointer,
        radius,
    )
}

/// A borrowed curve with its composition-frame x offset.
#[derive(Clone, Copy)]
pub struct CurveSource<'a> {
    pub curve: &'a KeyframeCurve,
    pub frame_offset: i64,
}

/// Offset-aware variant of [`hit_test`] for Timeline integration.
pub fn hit_test_with_offsets(
    curves: &[CurveSource<'_>],
    transform: CurveTransform,
    pointer: CurvePoint,
    radius: f64,
) -> Option<CurveHit> {
    hit_test_sources(
        curves
            .iter()
            .map(|source| (source.curve, source.frame_offset)),
        transform,
        pointer,
        radius,
    )
}

fn hit_test_sources<'a>(
    curves: impl IntoIterator<Item = (&'a KeyframeCurve, i64)>,
    transform: CurveTransform,
    pointer: CurvePoint,
    radius: f64,
) -> Option<CurveHit> {
    let radius_sq = radius.max(0.0).powi(2);
    let mut best: Option<(f64, u8, CurveHit)> = None;
    for (curve_index, (curve, frame_offset)) in curves.into_iter().enumerate() {
        let _ = visit_control_points(curve_index, curve, frame_offset, transform, |point| {
            let dx = point.position.x - pointer.x;
            let dy = point.position.y - pointer.y;
            let distance_sq = dx * dx + dy * dy;
            if distance_sq > radius_sq {
                return ControlFlow::Continue(());
            }
            let priority = u8::from(point.hit.part != HitPart::Keyframe);
            let candidate = (distance_sq, priority, point.hit);
            if best.is_none_or(|current| (distance_sq, priority) < (current.0, current.1)) {
                best = Some(candidate);
            }
            ControlFlow::Continue(())
        });
    }
    best.map(|(_, _, hit)| hit)
}

/// Immutable state captured at the start of a drag gesture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurveDrag {
    pub hit: CurveHit,
    pointer_start: CurvePoint,
    keyframe: Keyframe,
    previous_frame: Option<u64>,
    next_frame: Option<u64>,
}

/// Model-level edit emitted during a drag. The host owns mutation and undo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurveEdit {
    MoveKeyframe {
        curve: usize,
        from_frame: u64,
        to_frame: u64,
        value: f32,
    },
    SetTangent {
        curve: usize,
        frame: u64,
        part: HitPart,
        tangent: Vec2,
    },
}

/// Begins a drag if `hit` still resolves against the supplied curve.
pub fn begin_drag(curve: &KeyframeCurve, hit: CurveHit, pointer: CurvePoint) -> Option<CurveDrag> {
    let index = curve
        .keyframes()
        .iter()
        .position(|key| key.frame == hit.frame)?;
    let applicable = match hit.part {
        HitPart::Keyframe => true,
        HitPart::TangentIn => {
            index > 0 && curve.keyframes()[index - 1].interpolation == Interpolation::Bezier
        }
        HitPart::TangentOut => {
            index + 1 < curve.len()
                && curve.keyframes()[index].interpolation == Interpolation::Bezier
        }
    };
    if !applicable {
        return None;
    }
    Some(CurveDrag {
        hit,
        pointer_start: pointer,
        keyframe: curve.keyframes()[index],
        previous_frame: index.checked_sub(1).map(|i| curve.keyframes()[i].frame),
        next_frame: curve.keyframes().get(index + 1).map(|key| key.frame),
    })
}

/// Computes the live model edit for a pointer position without mutating a curve.
pub fn drag_to(drag: CurveDrag, pointer: CurvePoint, transform: CurveTransform) -> CurveEdit {
    let start = transform.widget_to_data(drag.pointer_start);
    let current = transform.widget_to_data(pointer);
    let delta = CurvePoint::new(current.x - start.x, current.y - start.y);
    match drag.hit.part {
        HitPart::Keyframe => CurveEdit::MoveKeyframe {
            curve: drag.hit.curve,
            from_frame: drag.keyframe.frame,
            to_frame: ((drag.keyframe.frame as f64 + delta.x).round().max(0.0)) as u64,
            value: (drag.keyframe.value as f64 + delta.y) as f32,
        },
        part @ (HitPart::TangentIn | HitPart::TangentOut) => {
            let original = if part == HitPart::TangentIn {
                drag.keyframe.tangent_in
            } else {
                drag.keyframe.tangent_out
            };
            let mut x = original.0 as f64 + delta.x;
            x = match part {
                HitPart::TangentIn => x.clamp(
                    drag.previous_frame
                        .map_or(0.0, |frame| -((drag.keyframe.frame - frame) as f64)),
                    0.0,
                ),
                HitPart::TangentOut => x.clamp(
                    0.0,
                    drag.next_frame
                        .map_or(0.0, |frame| (frame - drag.keyframe.frame) as f64),
                ),
                HitPart::Keyframe => unreachable!(),
            };
            CurveEdit::SetTangent {
                curve: drag.hit.curve,
                frame: drag.keyframe.frame,
                part,
                tangent: Vec2(x as f32, (original.1 as f64 + delta.y) as f32),
            }
        }
    }
}

/// An owned curve and paint color suitable for capture by a GPUI canvas.
#[derive(Clone)]
pub struct CurveSeries {
    pub curve: Arc<KeyframeCurve>,
    pub color: Hsla,
    /// Composition-frame x = layer-local keyframe + this offset.
    pub frame_offset: i64,
    /// Layer-local keyframes selected by the host.
    pub selected_frames: Arc<HashSet<u64>>,
}

/// Builds a minimal, reusable GPUI canvas for one or more curves.
///
/// The axes remain data-agnostic: callers decide what x/y mean by supplying
/// `data_min` and `data_max`. Curves are rendered as integer-frame polylines
/// evaluated by [`sample_curve`], never by a second Bézier implementation.
pub fn curve_editor_canvas(
    data_min: CurvePoint,
    data_max: CurvePoint,
    series: Vec<CurveSeries>,
    background: Hsla,
    control_color: Hsla,
) -> impl IntoElement {
    div().size_full().overflow_hidden().child(
        canvas(
            |_bounds, _window, _cx| (),
            move |bounds, (), window, _cx| {
                paint_curve_editor(
                    bounds,
                    data_min,
                    data_max,
                    &series,
                    background,
                    control_color,
                    window,
                );
            },
        )
        .size_full(),
    )
}

/// Builds a curve canvas whose horizontal axis uses a fixed pixel scale.
///
/// This is the Timeline integration entry point: `data_min_x` is the shared
/// scroll offset and `pixels_per_x` is the shared pixels-per-frame value. The
/// right edge is derived from the actual canvas width during paint, so the
/// graph cannot drift from an adjacent ruler after resize.
pub fn curve_editor_canvas_with_x_scale(
    data_min_x: f64,
    pixels_per_x: f64,
    data_min_y: f64,
    data_max_y: f64,
    series: Vec<CurveSeries>,
    background: Hsla,
    control_color: Hsla,
) -> impl IntoElement {
    let pixels_per_x = finite_span(pixels_per_x);
    div().size_full().overflow_hidden().child(
        canvas(
            |_bounds, _window, _cx| (),
            move |bounds, (), window, _cx| {
                let width: f32 = bounds.size.width.into();
                let width = width as f64;
                let data_max_x = scaled_view_max_x(data_min_x, pixels_per_x, width);
                paint_curve_editor(
                    bounds,
                    CurvePoint::new(data_min_x, data_min_y),
                    CurvePoint::new(data_max_x, data_max_y),
                    &series,
                    background,
                    control_color,
                    window,
                );
            },
        )
        .size_full(),
    )
}

fn scaled_view_max_x(data_min_x: f64, pixels_per_x: f64, widget_width: f64) -> f64 {
    data_min_x + finite_span(widget_width) / finite_span(pixels_per_x)
}

fn paint_curve_editor(
    bounds: Bounds<Pixels>,
    data_min: CurvePoint,
    data_max: CurvePoint,
    series: &[CurveSeries],
    background: Hsla,
    control_color: Hsla,
    window: &mut Window,
) {
    window.paint_quad(fill(bounds, background));
    let transform = CurveTransform::new(
        data_min,
        data_max,
        CurvePoint::new(bounds.size.width.into(), bounds.size.height.into()),
    );
    let sample_budget =
        ((transform.widget_size.x.ceil() as usize).saturating_mul(2)).clamp(2, MAX_PAINT_SAMPLES);
    for (curve_index, item) in series.iter().enumerate() {
        let mut path = PathBuilder::stroke(px(1.5));
        let mut sample_count = 0;
        visit_curve_samples_for_view(
            &item.curve,
            item.frame_offset,
            transform.data_min.x,
            transform.data_max.x,
            sample_budget,
            |composition_frame, sample| {
                let local = transform.data_to_widget(CurvePoint::new(
                    composition_frame as f64,
                    sample.value as f64,
                ));
                let position = point(
                    bounds.origin.x + px(local.x as f32),
                    bounds.origin.y + px(local.y as f32),
                );
                if sample_count == 0 {
                    path.move_to(position);
                } else {
                    path.line_to(position);
                }
                sample_count += 1;
            },
        );
        if sample_count > 0
            && let Ok(path) = path.build()
        {
            window.paint_path(path, item.color);
        }

        let mut painted_controls = 0;
        let _ = visit_control_points(
            curve_index,
            &item.curve,
            item.frame_offset,
            transform,
            |control| {
                if painted_controls >= MAX_PAINT_CONTROL_POINTS {
                    return ControlFlow::Break(());
                }
                painted_controls += 1;
                let position = offset_point(bounds, control.position);
                let selected = item.selected_frames.contains(&control.hit.frame);
                if control.hit.part != HitPart::Keyframe {
                    let anchor = offset_point(bounds, control.anchor);
                    let mut line = PathBuilder::stroke(px(1.0));
                    line.move_to(anchor);
                    line.line_to(position);
                    if let Ok(line) = line.build() {
                        window.paint_path(line, control_color);
                    }
                }
                let radius = if selected {
                    4.5
                } else if control.hit.part == HitPart::Keyframe {
                    3.5
                } else {
                    2.5
                };
                window.paint_quad(
                    fill(
                        Bounds::new(
                            point(position.x - px(radius), position.y - px(radius)),
                            size(px(radius * 2.0), px(radius * 2.0)),
                        ),
                        if selected {
                            control_color
                        } else if control.hit.part == HitPart::Keyframe {
                            item.color
                        } else {
                            control_color
                        },
                    )
                    .corner_radii(px(radius)),
                );
                ControlFlow::Continue(())
            },
        );
    }
}

fn control_intersects_widget(control: ControlPoint, widget_size: CurvePoint) -> bool {
    let min_x = control.position.x.min(control.anchor.x);
    let max_x = control.position.x.max(control.anchor.x);
    let min_y = control.position.y.min(control.anchor.y);
    let max_y = control.position.y.max(control.anchor.y);
    max_x >= 0.0 && min_x <= widget_size.x && max_y >= 0.0 && min_y <= widget_size.y
}

fn offset_point(bounds: Bounds<Pixels>, point_: CurvePoint) -> gpui::Point<Pixels> {
    point(
        bounds.origin.x + px(point_.x as f32),
        bounds.origin.y + px(point_.y as f32),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transform() -> CurveTransform {
        CurveTransform::new(
            CurvePoint::new(0.0, -1.0),
            CurvePoint::new(10.0, 1.0),
            CurvePoint::new(200.0, 100.0),
        )
    }

    fn curve() -> KeyframeCurve {
        let mut curve = KeyframeCurve::new();
        curve.insert_keyframe(
            Keyframe::new(0, -1.0, Interpolation::Bezier)
                .with_tangents(Vec2(0.0, 0.0), Vec2(4.0, 0.5)),
        );
        curve.insert_keyframe(
            Keyframe::new(10, 1.0, Interpolation::Linear)
                .with_tangents(Vec2(-4.0, -0.5), Vec2(0.0, 0.0)),
        );
        curve
    }

    #[test]
    fn data_widget_round_trip_inverts_y() {
        let transform = transform();
        let data = CurvePoint::new(2.5, 0.5);
        assert_eq!(transform.data_to_widget(data), CurvePoint::new(50.0, 25.0));
        let restored = transform.widget_to_data(transform.data_to_widget(data));
        assert!((restored.x - data.x).abs() < 1.0e-9);
        assert!((restored.y - data.y).abs() < 1.0e-9);
    }

    #[test]
    fn transform_orders_reversed_bounds_and_expands_zero_spans() {
        let reversed = CurveTransform::new(
            CurvePoint::new(10.0, 2.0),
            CurvePoint::new(0.0, -2.0),
            CurvePoint::new(200.0, 100.0),
        );
        assert_eq!(reversed.data_min, CurvePoint::new(0.0, -2.0));
        assert_eq!(reversed.data_max, CurvePoint::new(10.0, 2.0));

        let zero = CurveTransform::new(
            CurvePoint::new(5.0, 3.0),
            CurvePoint::new(5.0, 3.0),
            CurvePoint::new(200.0, 100.0),
        );
        assert_eq!(
            zero.data_to_widget(CurvePoint::new(5.0, 3.0)),
            CurvePoint::new(100.0, 50.0)
        );
    }

    #[test]
    fn sampling_delegates_to_keyframe_curve_evaluation() {
        let curve = curve();
        let samples = sample_curve(&curve, 0..=10);
        assert_eq!(samples.len(), 11);
        for sample in samples {
            assert_eq!(sample.value, curve.sample(sample.frame));
        }
    }

    #[test]
    fn paint_sampling_includes_fractional_edges_and_is_bounded() {
        let curve = curve();
        let mut fractional = Vec::new();
        visit_curve_samples_for_view(&curve, 0, 0.25, 0.75, 16, |frame, _sample| {
            fractional.push(frame);
        });
        assert_eq!(fractional, vec![0, 1]);

        let mut wide = Vec::new();
        visit_curve_samples_for_view(&curve, 0, 0.0, 100_000.0, 64, |frame, _sample| {
            wide.push(frame);
        });
        assert!(wide.len() <= 64);
        assert_eq!(wide.first(), Some(&0));
        assert_eq!(wide.last(), Some(&100_000));
    }

    #[test]
    fn decimated_sampling_preserves_step_and_acute_key_boundaries() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(50_000, 10.0, Interpolation::Step);
        curve.insert(50_001, -10.0, Interpolation::Linear);
        curve.insert(100_000, 0.0, Interpolation::Linear);

        let mut samples = Vec::new();
        visit_curve_samples_for_view(&curve, 0, 0.0, 100_000.0, 8, |composition_frame, sample| {
            samples.push((composition_frame, sample.value))
        });

        for frame in [49_999, 50_000, 50_001, 100_000] {
            let (_, value) = samples
                .iter()
                .find(|(sample_frame, _)| *sample_frame == frame)
                .unwrap_or_else(|| panic!("missing semantic boundary {frame}"));
            assert_eq!(*value, curve.sample(frame as u64));
        }
    }

    #[test]
    fn control_iteration_is_visible_range_limited_and_breakable() {
        let mut curve = KeyframeCurve::new();
        for frame in 0..=10_000 {
            curve.insert(frame, frame as f32, Interpolation::Linear);
        }
        let transform = CurveTransform::new(
            CurvePoint::new(5_000.0, 0.0),
            CurvePoint::new(5_010.0, 10_000.0),
            CurvePoint::new(100.0, 100.0),
        );
        let points = control_points(0, &curve, transform);
        assert!(points.len() <= 11, "visited {} controls", points.len());
        assert_eq!(points.first().map(|point| point.hit.frame), Some(5_000));
        assert_eq!(points.last().map(|point| point.hit.frame), Some(5_010));

        let mut visits = 0;
        let result = visit_control_points(0, &curve, 0, transform, |_point| {
            visits += 1;
            ControlFlow::Break(())
        });
        assert!(result.is_break());
        assert_eq!(visits, 1);
    }

    #[test]
    fn frame_offset_applies_to_samples_controls_and_hits() {
        let curve = curve();
        let offset = -5;
        let transform = CurveTransform::new(
            CurvePoint::new(-5.0, -1.0),
            CurvePoint::new(5.0, 1.0),
            CurvePoint::new(200.0, 100.0),
        );
        let points = control_points_with_offset(0, &curve, offset, transform);
        assert_eq!(points[0].position, CurvePoint::new(0.0, 100.0));

        assert_eq!(
            hit_test_with_offsets(
                &[CurveSource {
                    curve: &curve,
                    frame_offset: offset,
                }],
                transform,
                CurvePoint::new(0.0, 100.0),
                5.0,
            ),
            Some(CurveHit {
                curve: 0,
                frame: 0,
                part: HitPart::Keyframe,
            })
        );

        let mut samples = Vec::new();
        visit_curve_samples_for_view(&curve, offset, -5.0, 5.0, 32, |frame, sample| {
            samples.push((frame, sample.frame, sample.value));
        });
        assert!(samples.contains(&(-5, 0, curve.sample(0))));
        assert!(samples.contains(&(5, 10, curve.sample(10))));
    }

    #[test]
    fn fixed_x_scale_derives_visible_range_from_widget_width() {
        assert_eq!(scaled_view_max_x(25.0, 4.0, 400.0), 125.0);
        assert!(scaled_view_max_x(0.0, 0.0, 400.0).is_finite());
    }

    #[test]
    fn hit_test_finds_keyframe_and_handle() {
        let curve = curve();
        assert_eq!(
            hit_test(&[&curve], transform(), CurvePoint::new(0.0, 100.0), 5.0),
            Some(CurveHit {
                curve: 0,
                frame: 0,
                part: HitPart::Keyframe,
            })
        );
        let handle = transform().data_to_widget(CurvePoint::new(4.0, -0.5));
        assert_eq!(
            hit_test(&[&curve], transform(), handle, 5.0),
            Some(CurveHit {
                curve: 0,
                frame: 0,
                part: HitPart::TangentOut,
            })
        );
    }

    #[test]
    fn keyframe_drag_uses_data_delta_and_quantizes_x() {
        let curve = curve();
        let hit = CurveHit {
            curve: 0,
            frame: 0,
            part: HitPart::Keyframe,
        };
        let drag = begin_drag(&curve, hit, CurvePoint::new(0.0, 100.0)).unwrap();
        assert_eq!(
            drag_to(drag, CurvePoint::new(50.0, 75.0), transform()),
            CurveEdit::MoveKeyframe {
                curve: 0,
                from_frame: 0,
                to_frame: 3,
                value: -0.5,
            }
        );
    }

    #[test]
    fn handle_drag_clamps_time_to_adjacent_segment() {
        let curve = curve();
        let hit = CurveHit {
            curve: 0,
            frame: 10,
            part: HitPart::TangentIn,
        };
        let drag = begin_drag(&curve, hit, CurvePoint::new(120.0, 25.0)).unwrap();
        assert_eq!(
            drag_to(drag, CurvePoint::new(-200.0, 25.0), transform()),
            CurveEdit::SetTangent {
                curve: 0,
                frame: 10,
                part: HitPart::TangentIn,
                tangent: Vec2(-10.0, -0.5),
            }
        );
    }

    #[test]
    fn drag_rejects_handles_that_are_not_active_for_a_bezier_segment() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 1.0, Interpolation::Linear);

        assert!(
            begin_drag(
                &curve,
                CurveHit {
                    curve: 0,
                    frame: 0,
                    part: HitPart::TangentOut,
                },
                CurvePoint::default(),
            )
            .is_none()
        );
        assert!(
            begin_drag(
                &curve,
                CurveHit {
                    curve: 0,
                    frame: 10,
                    part: HitPart::TangentIn,
                },
                CurvePoint::default(),
            )
            .is_none()
        );
    }
}
