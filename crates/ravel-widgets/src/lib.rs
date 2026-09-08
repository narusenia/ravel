// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's own widget layer.
//!
//! [`tokens`] is the single source for the colors, spacing, row heights,
//! typography, motion and radii the UI is built from. [`curve_editor`] and
//! [`curve_view`] are the first widgets to live here; the remaining ones move
//! in later units (`docs/implementation/ui-component-layer-plan.md`, `UIX-3`
//! and `UIX-4`).
//!
//! `examples/gallery` renders everything this crate exposes without starting
//! `ravel-app` — it is where the widgets are looked at.
//!
//! **This crate must not depend on `gpui-component`.** Ravel's theme schema is
//! authoritative and gpui-component's `ThemeConfig` is derived from it by the
//! application host, so the dependency edge only ever points that way. A
//! borrowed component (an `Input`, the window `Root`) is wired up in
//! `ravel-app`, never here.

pub mod curve_editor;
pub mod curve_view;
pub mod theme;
pub mod tokens;

pub use curve_editor::{
    ControlPoint, CurveDrag, CurveDragAxis, CurveEdit, CurveHit, CurvePoint, CurveSeries,
    CurveSource, CurveTransform, HitPart, SamplePoint, begin_drag, control_points,
    control_points_with_offset, curve_editor_canvas, curve_editor_canvas_with_x_scale,
    dominant_drag_axis, drag_to, drag_to_constrained, drag_to_with_tangent_snap, hit_test,
    hit_test_with_offsets, keyframes_in_rect_with_offsets, sample_curve,
};
pub use curve_view::{
    CurveValueRange, format_value_label, grid_values, nice_value_step, padded_bounds,
    value_grid_values,
};

pub use theme::{ActiveTokens, set_active_tokens};

pub use tokens::{
    Colors, Density, Metrics, Motion, Radii, RavelTheme, Rows, Spacing, ThemeFile, ThemeMode,
    ThemeSpec, Typography, hex_color_string, mix, parse_hex_color,
};
