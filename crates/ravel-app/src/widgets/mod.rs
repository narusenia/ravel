// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel-specific widgets that still live in the application.
//!
//! The parts that no longer borrow from gpui-component live in
//! `ravel-widgets` ([`curve_editor`], [`curve_view`], [`scrub_input`]) and are
//! re-exported here so the panels keep one import path. What stays in this
//! module is what still reads gpui-component's theme: the two
//! `param_*_editor` widgets. Their icons and tooltips are already Ravel's own
//! ([`ravel_widgets::Icon`], [`ravel_widgets::TooltipExt`]).

pub use ravel_widgets::{curve_editor, curve_view, scrub_input};

pub mod param_curve_editor;
pub mod param_ramp_editor;

pub use curve_editor::{
    ControlPoint, CurveDrag, CurveDragAxis, CurveEdit, CurveHit, CurvePoint, CurveSeries,
    CurveSource, CurveTransform, HitPart, SamplePoint, begin_drag, control_points,
    control_points_with_offset, curve_editor_canvas, curve_editor_canvas_with_x_scale,
    dominant_drag_axis, drag_to, drag_to_constrained, drag_to_with_tangent_snap, hit_test,
    hit_test_with_offsets, keyframes_in_rect_with_offsets, sample_curve,
};
pub use param_curve_editor::{
    ParamCurveEditor, ParamCurveEditorState, ParamCurveEvent, curve_thumbnail,
};
pub use param_ramp_editor::{
    ParamRampEditor, ParamRampEditorState, ParamRampEvent, ramp_thumbnail,
};
pub use ravel_widgets::{ScrubEvent, ScrubInput, ScrubInputState};
