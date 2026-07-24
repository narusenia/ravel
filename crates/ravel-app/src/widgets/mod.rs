// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reusable Ravel-specific widgets (gpui-component independent).

pub mod curve_editor;
pub mod scrub_input;

pub use curve_editor::{
    ControlPoint, CurveDrag, CurveDragAxis, CurveEdit, CurveHit, CurvePoint, CurveSeries,
    CurveSource, CurveTransform, HitPart, SamplePoint, begin_drag, control_points,
    control_points_with_offset, curve_editor_canvas, curve_editor_canvas_with_x_scale,
    dominant_drag_axis, drag_to, drag_to_constrained, hit_test, hit_test_with_offsets,
    keyframes_in_rect_with_offsets, sample_curve,
};
pub use scrub_input::{ScrubEvent, ScrubInput, ScrubInputState};
