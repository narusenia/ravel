// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reusable Ravel-specific widgets (gpui-component independent).

pub mod curve_editor;
pub mod scrub_input;

pub use curve_editor::{
    ControlPoint, CurveDrag, CurveEdit, CurveHit, CurvePoint, CurveSeries, CurveSource,
    CurveTransform, HitPart, SamplePoint, begin_drag, control_points, control_points_with_offset,
    curve_editor_canvas, curve_editor_canvas_with_x_scale, drag_to, hit_test,
    hit_test_with_offsets, sample_curve,
};
pub use scrub_input::{ScrubEvent, ScrubInput, ScrubInputState};
