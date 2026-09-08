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
//! `Icon` / `Button` / `Tooltip` landed as three registrations.
//!
//! Two kinds of widget live here, and they read their colors differently. The
//! curve views take theirs as arguments; `Icon`, `Button` and `Tooltip` read
//! the token set in force through `cx.tokens()`. So the theme toggle does two
//! things — it swaps the palette this file paints from *and* installs it with
//! [`ravel_widgets::set_active_tokens`], because a section that disagreed with
//! its own buttons would be worse than no gallery.

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    WindowBounds, WindowOptions, div, prelude::FluentBuilder as _, px, size,
};
use gpui_base::Button as BaseButton;
use ravel_core::animation::{Interpolation, Keyframe, KeyframeCurve};
use ravel_core::types::Vec2;
use ravel_widgets::curve_editor::{
    CurvePoint, CurveSeries, curve_editor_canvas, curve_editor_canvas_with_x_scale,
};
use ravel_widgets::curve_view::{
    CurveValueRange, format_value_label, nice_value_step, padded_bounds, value_grid_values,
};
use ravel_widgets::tokens::{Density, RavelTheme, ThemeMode, ThemeSpec};
use ravel_widgets::{
    Button, ButtonVariant, Icon, SHOW_DELAY, TooltipExt as _, UiIcon, button_layers,
};

/// One labelled block of the gallery.
type SectionFn = fn(&RavelTheme) -> AnyElement;

/// Every section the gallery shows, in order.
///
/// **This is the registration point.** A widget that moves into this crate
/// gets one entry here; the window, the theme toggle, and the layout need no
/// change.
const SECTIONS: &[(&str, SectionFn)] = &[
    ("Tokens", tokens_section),
    ("icon · UiIcon at both densities", icon_section),
    ("button · every variant against every state", button_section),
    ("tooltip · the popup and its delay", tooltip_section),
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

/// `Icon`: every shape-named glyph, at both density steps.
///
/// The colour is not set on any of them — an icon takes the ambient text
/// colour, which is what keeps an icon inside a button the same colour as the
/// label beside it. The last row proves an explicit colour still wins.
fn icon_section(theme: &RavelTheme) -> AnyElement {
    let row = |density: Density, label: &str| {
        div()
            .flex()
            .flex_col()
            .gap(theme.spacing.xs)
            .child(caption(
                theme,
                format!("{label} — {}px", f32::from(density.icon_size())),
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(theme.spacing.sm)
                    .children(UiIcon::ALL.map(|glyph| Icon::new(glyph).density(density))),
            )
    };

    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.md)
        .child(row(Density::Compact, "Density::Compact"))
        .child(row(Density::Default, "Density::Default"))
        .child(
            div()
                .flex()
                .items_center()
                .gap(theme.spacing.sm)
                .child(Icon::new(UiIcon::TriangleAlert).text_color(theme.colors.danger))
                .child(Icon::new(UiIcon::Network).text_color(theme.colors.info))
                .child(caption(
                    theme,
                    "an explicit text_color wins over the ambient one",
                )),
        )
        .into_any_element()
}

/// `Button`: every variant against every state.
///
/// Three of the four states have to be *performed* rather than shown — hover
/// with the pointer, hold to press, Tab to focus — so the first block is live
/// buttons and the second is the same surfaces painted flat, side by side, for
/// reading them apart without a mouse.
///
/// **Tab through this section.** The ring appears only under keyboard
/// navigation (`focus_visible`), so clicking a button focuses it without
/// painting a ring; Tab paints one. The third button in each row declares
/// `tab_stop(false)` and Tab skips it.
fn button_section(theme: &RavelTheme) -> AnyElement {
    let variants = [
        (ButtonVariant::Ghost, "ghost"),
        (ButtonVariant::Solid, "solid"),
        (ButtonVariant::Primary, "primary"),
    ];

    // Live buttons: one row per variant, both density steps, and the states a
    // caller sets rather than the pointer.
    let live = div()
        .flex()
        .flex_col()
        .gap(theme.spacing.sm)
        .children(variants.map(|(variant, name)| {
            let button = move |id: &str, density: Density| {
                // The id has to carry the variant: three rows of the same
                // buttons would otherwise share one focus state each.
                Button::new(SharedString::from(format!("gallery-{name}-{id}")))
                    .density(density)
                    .map(|button| match variant {
                        ButtonVariant::Ghost => button.ghost(),
                        ButtonVariant::Solid => button.solid(),
                        ButtonVariant::Primary => button.primary(),
                    })
            };
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(theme.spacing.sm)
                .child(div().w(px(64.0)).child(caption(theme, name.to_string())))
                .child(
                    button("compact", Density::Compact)
                        .label("compact")
                        .icon(UiIcon::Plus)
                        .tooltip("compact · 20px"),
                )
                .child(
                    button("default", Density::Default)
                        .label("default")
                        .icon(UiIcon::Plus)
                        .tooltip("default · 24px"),
                )
                .child(
                    button("no-tab-stop", Density::Default)
                        .label("no tab stop")
                        .tab_stop(false)
                        .tooltip("Tab skips this one"),
                )
                .child(
                    button("selected", Density::Default)
                        .label("selected")
                        .selected(true)
                        .tab_index(2),
                )
                .child(
                    button("disabled", Density::Default)
                        .label("disabled")
                        .icon(UiIcon::Plus)
                        .disabled(true),
                )
                .child(
                    button("icon-only", Density::Compact)
                        .icon(UiIcon::Ellipsis)
                        .tooltip("icon only · square"),
                )
        }));

    // The same surfaces, flat, so the four states can be compared without
    // performing them. This is the check the completion criterion means by
    // "the four states can be read apart".
    let swatches = div()
        .flex()
        .flex_col()
        .gap(theme.spacing.xs)
        .children(variants.map(|(variant, name)| {
            let enabled = button_layers(variant, false, false, &theme.colors);
            let disabled = button_layers(variant, false, true, &theme.colors);
            let faces = [
                ("rest", Some(enabled.rest)),
                ("hover", enabled.hover),
                ("press", enabled.pressed),
                ("disabled", disabled.disabled),
            ];
            div()
                .flex()
                .items_center()
                .gap(theme.spacing.sm)
                .child(div().w(px(64.0)).child(caption(theme, name.to_string())))
                .children(faces.map(|(state, face)| {
                    let face = face.unwrap_or(enabled.rest);
                    div()
                        .h(theme.rows.default)
                        .w(px(80.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius.radius)
                        .bg(face.surface)
                        .text_size(theme.text.mono_font_size)
                        .text_color(face.foreground)
                        .child(state)
                }))
                .child(
                    // The ring, on its own, so it can be told apart from the
                    // hover surface beside it.
                    div()
                        .h(theme.rows.default)
                        .w(px(80.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius.radius)
                        .border_1()
                        .border_color(enabled.focus_ring.unwrap_or(theme.colors.border))
                        .text_size(theme.text.mono_font_size)
                        // The page's own foreground, not the variant's: a
                        // primary button's label colour is the one that reads
                        // on *primary*, and on the page it is invisible.
                        .text_color(theme.colors.foreground)
                        .child("ring"),
                )
        }));

    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.md)
        .child(live)
        .child(swatches)
        .child(caption(
            theme,
            "hover for the hover surface · hold for the press surface · Tab for \
             the 1px ring (a click focuses without one) · Tab walks these left \
             to right and skips the third button in each row, which declares \
             tab_stop(false)",
        ))
        .into_any_element()
}

/// `Tooltip`: the popup, on a button and on a bare element.
///
/// Both go through the same path — `TooltipExt::ravel_tooltip`, which is what
/// `Button::tooltip` calls — so there is one delay and one appearance. Hover
/// and wait 500ms; press Escape while one is up and it goes away.
fn tooltip_section(theme: &RavelTheme) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.sm)
        .child(
            div()
                .flex()
                .items_center()
                .gap(theme.spacing.sm)
                .child(
                    Button::new("gallery-tooltip-button")
                        .solid()
                        .label("on a button")
                        .tooltip("Button::tooltip"),
                )
                .child(
                    div()
                        .id("gallery-tooltip-plain")
                        .h(theme.rows.default)
                        .px(theme.spacing.sm)
                        .flex()
                        .items_center()
                        .rounded(theme.radius.radius)
                        .bg(theme.colors.accent)
                        .text_size(theme.text.font_size)
                        .text_color(theme.colors.foreground)
                        .child("on a bare element")
                        .ravel_tooltip("TooltipExt::ravel_tooltip"),
                )
                .child(
                    Button::new("gallery-tooltip-disabled")
                        .solid()
                        .label("disabled, still explains itself")
                        .disabled(true)
                        .tooltip("a control that cannot act still says why"),
                ),
        )
        .child(caption(
            theme,
            format!(
                "hover and wait {}ms · Escape dismisses the showing · the surface is \
                 background mixed {}% toward black, with a 1px border and no shadow",
                SHOW_DELAY.as_millis(),
                (ravel_widgets::tokens::RAISED_MIX * 100.0).round(),
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

/// Ravel is a dark-first tool, so the gallery opens where the application
/// does. One click shows the other palette.
const STARTING_MODE: ThemeMode = ThemeMode::Dark;

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
                BaseButton::new("toggle-theme")
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
                        // `Icon`, `Button` and `Tooltip` read the installed
                        // token set rather than taking colors as arguments, so
                        // the toggle has to move both or the section and its
                        // own buttons disagree.
                        ravel_widgets::set_active_tokens(
                            ThemeSpec {
                                name: "Ravel".to_string(),
                                mode: next,
                                ..ThemeSpec::default()
                            }
                            .resolve(),
                            cx,
                        );
                    }),
            );

        let body = div()
            .id("gallery-body")
            // Without a tab group there is nothing for Tab to traverse, and
            // the focus ring — the one state that cannot be shown any other
            // way — would never appear in the gallery.
            .tab_group()
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
            // Tab traversal, which the gallery has to wire itself: GPUI does
            // not bind Tab to focus movement — `Window::focus_next` is a
            // method, not a keystroke — and in the application that binding
            // lives in `gpui_component::Root`. Without this the focus ring
            // would be the one state nobody could see here. It is a raw key
            // handler rather than an Action because an example binary has no
            // `CommandId` table to add one to, and declaring app-level
            // actions from a library's example is worse than reading one
            // keystroke.
            .on_key_down(|event, window, cx| {
                if event.keystroke.key != "tab" {
                    return;
                }
                if event.keystroke.modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
            })
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
            ravel_widgets::set_active_tokens(
                ThemeSpec {
                    name: "Ravel".to_string(),
                    mode: STARTING_MODE,
                    ..ThemeSpec::default()
                }
                .resolve(),
                cx,
            );
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
                        mode: STARTING_MODE,
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
