// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel-specific widgets that still live in the application.
//!
//! The gpui-component-independent geometry moved to `ravel-widgets`
//! ([`curve_editor`], [`curve_view`]) and is re-exported here so the panels and
//! the widgets still in this module keep one import path. What stays is what
//! borrows from gpui-component: `scrub_input` wraps its `Input`, and the two
//! `param_*_editor` widgets read its theme and use its `Icon` / `Tooltip`
//! until `UIX-3` / `UIX-4` replace those.

pub use ravel_widgets::{curve_editor, curve_view};

pub mod param_curve_editor;
pub mod param_ramp_editor;
pub mod scrub_input;

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
pub use scrub_input::{ScrubEvent, ScrubInput, ScrubInputState};
