// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's own widget layer.
//!
//! [`tokens`] is the single source for the colors, spacing, row heights,
//! typography, motion and radii the UI is built from, and [`theme`] is how a
//! widget reaches the set in force. [`icon`], [`button`] and [`tooltip`] are
//! the three parts Ravel owns outright — the ones the application's texture
//! lives in — and they are built on `gpui-base`'s unstyled primitives, which
//! own focus, keyboard activation and accessibility. [`curve_editor`] and
//! [`curve_view`] are the geometry the curve views share. The remaining
//! borrowed parts move in later units
//! (`docs/implementation/ui-component-layer-plan.md`).
//!
//! `examples/gallery` renders everything this crate exposes without starting
//! `ravel-app` — it is where the widgets are looked at.
//!
//! **This crate must not depend on `gpui-component`.** Ravel's theme schema is
//! authoritative and gpui-component's `ThemeConfig` is derived from it by the
//! application host, so the dependency edge only ever points that way. A
//! borrowed component (an `Input`, the window `Root`) is wired up in
//! `ravel-app`, never here.

pub mod button;
pub mod curve_editor;
pub mod curve_view;
pub mod icon;
pub mod input;
pub mod number_input;
pub mod theme;
pub mod tokens;
pub mod tooltip;

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

pub use button::{Button, ButtonFace, ButtonLayers, ButtonVariant, button_layers};

pub use icon::{Icon, IconPath, UiIcon};

pub use input::{Input, InputFace, InputLayers, frame_is_focused, input_layers};

pub use number_input::NumberInput;

/// The text-editing engine the two input widgets are built on, re-exported.
///
/// These are `gpui-base`'s own types — the same ones gpui-component re-exported
/// rather than defined, which is why moving the widgets onto `gpui-base`
/// changes no behaviour. They are surfaced here so a host does not need
/// `gpui-base` as a direct dependency just to name the state it owns, the way
/// [`TooltipOverlay`] already is.
///
/// [`InputState`] is where everything about the *value* lives — the
/// placeholder, the default, disabled and read-only, the step size and range,
/// the validation pattern. [`crate::Input`] and [`crate::NumberInput`] decide
/// appearance and read that state; they never write to it.
pub use gpui_base::input::{Enter, Escape, InputEvent, InputState, MoveDown, MoveUp, NumberStep};
pub use gpui_base::{Decrement, Increment, StepAction};

pub use tooltip::{
    GRACE_PERIOD, SHOW_DELAY, Tooltip, TooltipExt, install_tooltip_overlay, tooltip_overlay,
};

/// The per-window tooltip overlay a host installs and renders.
///
/// Re-exported so a host does not need `gpui-base` of its own just to name the
/// field it keeps [`install_tooltip_overlay`]'s return value in.
pub use gpui_base::TooltipOverlay;

pub use theme::{ActiveTokens, set_active_tokens};

pub use tokens::{
    Colors, Density, Metrics, Motion, Radii, RavelTheme, Rows, Spacing, ThemeFile, ThemeMode,
    ThemeSpec, Typography, hex_color_string, mix, parse_hex_color,
};
