// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Standalone viewer for `ravel-widgets`.
//!
//! This is the only way to look at Ravel's own widgets without starting
//! `ravel-app`: it opens one window listing every section in [`SECTIONS`],
//! painted entirely from [`RavelTheme`] tokens, and swaps the whole palette
//! between light and dark from the toolbar.
//!
//! **Adding a widget adds one row to [`SECTIONS`] and one `fn` beside the
//! others.** Nothing else in this file knows how many sections there are, so
//! `UIX-4`'s `Icon` / `Button` / `Tooltip` land as three registrations.
//!
//! Note what this file does *not* do: the widgets it renders take their colors
//! as arguments and read no theme of their own. The gallery hands them tokens
//! so the two palettes can be compared, but wiring the tokens into the widgets
//! is `UIX-3`.

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    WindowBounds, WindowOptions, div, px, size,
};
use gpui_base::Button;
use ravel_core::animation::{Interpolation, Keyframe, KeyframeCurve};
use ravel_core::types::Vec2;
use ravel_widgets::curve_editor::{
    CurvePoint, CurveSeries, curve_editor_canvas, curve_editor_canvas_with_x_scale,
};
use ravel_widgets::curve_view::{
    CurveValueRange, format_value_label, nice_value_step, padded_bounds, value_grid_values,
};
use ravel_widgets::tokens::{RavelTheme, ThemeMode, ThemeSpec};

/// One labelled block of the gallery.
type SectionFn = fn(&RavelTheme) -> AnyElement;

/// Every section the gallery shows, in order.
///
/// **This is the registration point.** A widget that moves into this crate
/// gets one entry here; the window, the theme toggle, and the layout need no
/// change.
const SECTIONS: &[(&str, SectionFn)] = &[
    ("Tokens", tokens_section),
    ("curve_editor · curve_editor_canvas", curve_canvas_section),
    (
        "curve_editor · curve_editor_canvas_with_x_scale",
        curve_scaled_section,
    ),
    ("curve_view · range and grid", curve_view_section),
];

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

/// The tokens themselves: ten colors, four spacing steps, three row heights,
/// two feedback durations, two radii. The visual baseline `UIX-3` wires the
/// widgets up against.
fn tokens_section(theme: &RavelTheme) -> AnyElement {
    let colors = [
        ("background", theme.colors.background),
        ("foreground", theme.colors.foreground),
        ("border", theme.colors.border),
        ("muted_foreground", theme.colors.muted_foreground),
        ("accent", theme.colors.accent),
        ("primary", theme.colors.primary),
        ("secondary", theme.colors.secondary),
        ("danger", theme.colors.danger),
        ("info", theme.colors.info),
        ("drop_target", theme.colors.drop_target),
    ];
    let swatches = div()
        .flex()
        .flex_wrap()
        .gap(theme.spacing.sm)
        .children(colors.map(|(name, color)| {
            div()
                .flex()
                .flex_col()
                .gap(theme.spacing.xs)
                .child(
                    div()
                        .w(px(96.0))
                        .h(px(40.0))
                        .rounded(theme.radius.radius)
                        .border_1()
                        .border_color(theme.colors.border)
                        .bg(color),
                )
                .child(caption(theme, name))
        }));

    // Spacing and row heights are read as widths and heights rather than
    // printed: a 4px step is only legible next to the next one.
    let spacing = labelled_bars(
        theme,
        "spacing",
        &[
            ("xs", theme.spacing.xs),
            ("sm", theme.spacing.sm),
            ("md", theme.spacing.md),
            ("lg", theme.spacing.lg),
        ],
    );
    let rows = div().flex().flex_col().gap(theme.spacing.xs).children(
        [
            ("row.compact", theme.rows.compact),
            ("row.default", theme.rows.default),
            ("header", theme.rows.header),
        ]
        .map(|(name, height)| {
            div()
                .h(height)
                .px(theme.spacing.sm)
                .flex()
                .items_center()
                .bg(theme.colors.accent)
                .border_1()
                .border_color(theme.colors.border)
                .text_size(theme.text.font_size)
                .text_color(theme.colors.foreground)
                .child(format!("{name} — {}px", f32::from(height)))
        }),
    );

    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.md)
        .child(swatches)
        .child(spacing)
        .child(rows)
        .child(caption(
            theme,
            format!(
                "motion feedback.in {}ms / feedback.out {}ms · radius {}px / {}px · \
                 text {} {}px, mono {} {}px",
                theme.motion.feedback_in.as_millis(),
                theme.motion.feedback_out.as_millis(),
                f32::from(theme.radius.radius),
                f32::from(theme.radius.radius_lg),
                theme.text.font_family,
                f32::from(theme.text.font_size),
                theme.text.mono_font_family,
                f32::from(theme.text.mono_font_size),
            ),
        ))
        .into_any_element()
}

/// Three curves over an explicit data range — one per interpolation mode, plus
/// the integral staircase.
fn curve_canvas_section(theme: &RavelTheme) -> AnyElement {
    let series = vec![
        series(bezier_curve(), theme.colors.primary, false, &[0, 24]),
        series(linear_curve(), theme.colors.info, false, &[]),
        series(step_curve(), theme.colors.danger, true, &[12]),
    ];
    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.sm)
        .child(canvas_frame(theme).child(curve_editor_canvas(
            CurvePoint::new(0.0, -1.2),
            CurvePoint::new(24.0, 1.2),
            series,
            theme.colors.background,
            theme.colors.foreground,
        )))
        .child(caption(
            theme,
            "primary: Bézier with two selected keys · info: linear · \
             danger: integral Step (staircase, control points on the float grid)",
        ))
        .into_any_element()
}

/// The same curves on the Timeline's axis: x is pixels per frame, so the
/// visible frame range follows the canvas width instead of being given.
fn curve_scaled_section(theme: &RavelTheme) -> AnyElement {
    let series = vec![
        series(bezier_curve(), theme.colors.primary, false, &[]),
        series(linear_curve(), theme.colors.info, false, &[]),
    ];
    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.sm)
        .child(canvas_frame(theme).child(curve_editor_canvas_with_x_scale(
            0.0,
            12.0,
            -1.2,
            1.2,
            series,
            theme.colors.background,
            theme.colors.foreground,
        )))
        .child(caption(
            theme,
            "12 px per frame from frame 0 — resize the window to see the visible \
             frame range follow the width",
        ))
        .into_any_element()
}

/// `curve_view` is pure arithmetic (no painting of its own), so its outputs are
/// shown as the numbers a caller would draw: the fitted range, the tick step,
/// and the labelled grid values, for an auto range and a pinned one.
fn curve_view_section(theme: &RavelTheme) -> AnyElement {
    let data = (-0.85_f64, 0.85_f64);
    let auto = padded_bounds(data.0, data.1);
    let ranges = [
        ("auto (fitted to the data)", CurveValueRange::auto()),
        ("pinned -2..2", CurveValueRange::pinned(-2.0, 2.0)),
    ];
    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.sm)
        .children(ranges.map(|(label, range)| {
            let (min, max) = range.resolved(auto);
            let ticks = value_grid_values(min, max, 240.0)
                .into_iter()
                .map(format_value_label)
                .collect::<Vec<_>>()
                .join("  ");
            div()
                .flex()
                .flex_col()
                .gap(theme.spacing.xs)
                .child(
                    div()
                        .h(theme.rows.compact)
                        .flex()
                        .items_center()
                        .text_size(theme.text.font_size)
                        .text_color(theme.colors.foreground)
                        .child(format!(
                            "{label}: {} .. {} (step {})",
                            format_value_label(min),
                            format_value_label(max),
                            format_value_label(nice_value_step((max - min) / 5.0)),
                        )),
                )
                .child(
                    div()
                        .font_family(theme.text.mono_font_family.clone())
                        .text_size(theme.text.mono_font_size)
                        .text_color(theme.colors.muted_foreground)
                        .child(ticks),
                )
        }))
        .child(caption(
            theme,
            format!(
                "data {} .. {} · padded_bounds widens it to {} .. {}",
                format_value_label(data.0),
                format_value_label(data.1),
                format_value_label(auto.0),
                format_value_label(auto.1),
            ),
        ))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Section helpers
// ---------------------------------------------------------------------------

fn caption(theme: &RavelTheme, text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_size(theme.text.mono_font_size)
        .text_color(theme.colors.muted_foreground)
        .child(text.into())
}

/// A fixed-height, bordered box for a widget that fills its parent.
fn canvas_frame(theme: &RavelTheme) -> gpui::Div {
    div()
        .h(px(200.0))
        .rounded(theme.radius.radius)
        .border_1()
        .border_color(theme.colors.border)
        .overflow_hidden()
}

fn labelled_bars(theme: &RavelTheme, title: &str, steps: &[(&str, gpui::Pixels)]) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.xs)
        .child(caption(theme, title.to_string()))
        .children(steps.iter().map(|(name, step)| {
            div()
                .flex()
                .items_center()
                .gap(theme.spacing.sm)
                .child(
                    div()
                        .w(*step)
                        .h(px(12.0))
                        .bg(theme.colors.primary)
                        .rounded(theme.radius.radius),
                )
                .child(caption(theme, format!("{name} — {}px", f32::from(*step))))
        }))
        .into_any_element()
}

fn series(
    curve: KeyframeCurve,
    color: gpui::Hsla,
    integral: bool,
    selected: &[u64],
) -> CurveSeries {
    CurveSeries {
        curve: Arc::new(curve),
        color,
        frame_offset: 0,
        selected_frames: Arc::new(selected.iter().copied().collect::<HashSet<u64>>()),
        integral,
    }
}

fn bezier_curve() -> KeyframeCurve {
    let mut curve = KeyframeCurve::new();
    curve.insert_keyframe(
        Keyframe::new(0, -1.0, Interpolation::Bezier).with_tangents(Vec2(0.0, 0.0), Vec2(6.0, 0.9)),
    );
    curve.insert_keyframe(
        Keyframe::new(12, 0.6, Interpolation::Bezier)
            .with_tangents(Vec2(-4.0, -0.4), Vec2(4.0, 0.4)),
    );
    curve.insert_keyframe(
        Keyframe::new(24, -0.2, Interpolation::Bezier)
            .with_tangents(Vec2(-6.0, 0.6), Vec2(0.0, 0.0)),
    );
    curve
}

fn linear_curve() -> KeyframeCurve {
    let mut curve = KeyframeCurve::new();
    curve.insert_keyframe(Keyframe::new(0, 0.9, Interpolation::Linear));
    curve.insert_keyframe(Keyframe::new(16, -0.7, Interpolation::Linear));
    curve.insert_keyframe(Keyframe::new(24, 0.3, Interpolation::Linear));
    curve
}

fn step_curve() -> KeyframeCurve {
    let mut curve = KeyframeCurve::new();
    curve.insert_keyframe(Keyframe::new(0, -0.6, Interpolation::Linear));
    curve.insert_keyframe(Keyframe::new(12, 0.4, Interpolation::Linear));
    curve.insert_keyframe(Keyframe::new(24, 1.0, Interpolation::Step));
    curve
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

struct Gallery {
    mode: ThemeMode,
}

impl Gallery {
    /// The built-in palette for `mode`, with every token at its default —
    /// no theme file is read, so the gallery shows the shipped baseline.
    fn theme(&self) -> RavelTheme {
        ThemeSpec {
            name: "Ravel".to_string(),
            mode: self.mode,
            ..ThemeSpec::default()
        }
        .resolve()
    }
}

impl Render for Gallery {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme();
        let weak = cx.entity().downgrade();
        let next = if self.mode.is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };

        let toolbar = div()
            .flex()
            .items_center()
            .gap(theme.spacing.sm)
            .h(theme.rows.header)
            .px(theme.spacing.sm)
            .border_b_1()
            .border_color(theme.colors.border)
            .child(
                div()
                    .flex_1()
                    .text_size(theme.text.font_size)
                    .text_color(theme.colors.foreground)
                    .child(format!("ravel-widgets · {:?}", self.mode)),
            )
            // gpui-base's Button already routes pointer, Enter and Space
            // through this one handler and declares its own tab stop, which is
            // what UX invariant 10 asks for.
            .child(
                Button::new("toggle-theme")
                    .accessibility_label("Toggle theme")
                    .px(theme.spacing.sm)
                    .h(theme.rows.compact)
                    .rounded(theme.radius.radius)
                    .border_1()
                    .border_color(theme.colors.border)
                    .bg(theme.colors.secondary)
                    .text_size(theme.text.font_size)
                    .text_color(theme.colors.foreground)
                    .child(format!("Switch to {next:?}"))
                    .on_click(move |_, _window, cx| {
                        weak.update(cx, |this, cx| {
                            this.mode = next;
                            cx.notify();
                        })
                        .ok();
                    }),
            );

        let body = div()
            .id("gallery-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(theme.spacing.lg)
            .p(theme.spacing.lg)
            .children(SECTIONS.iter().map(|(title, section)| {
                div()
                    .flex()
                    .flex_col()
                    .gap(theme.spacing.sm)
                    .child(
                        div()
                            .h(theme.rows.header)
                            .flex()
                            .items_center()
                            .border_b_1()
                            .border_color(theme.colors.border)
                            .text_size(theme.text.font_size)
                            .text_color(theme.colors.foreground)
                            .child(*title),
                    )
                    .child(section(&theme))
            }));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.colors.background)
            .text_color(theme.colors.foreground)
            .child(toolbar)
            .child(body)
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(gpui_kit_assets::Assets)
        .run(|cx: &mut App| {
            gpui_base::init(cx);
            if let Err(e) = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(900.0), px(900.0)),
                        cx,
                    ))),
                    ..Default::default()
                },
                |_window, cx| {
                    cx.new(|_cx| Gallery {
                        mode: ThemeMode::Dark,
                    })
                },
            ) {
                eprintln!("[gallery] failed to open window: {e}");
                cx.quit();
                return;
            }
            cx.activate(true);
        });
}
