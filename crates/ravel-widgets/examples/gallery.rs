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
//! each widget landed as one registration.
//!
//! ```text
//! cargo run -p ravel-widgets --example gallery
//! ```
//!
//! GPUI lays the whole element tree out on every frame, and this file is one
//! deep flex tree with `flex_wrap` rows of icons and buttons — taffy's most
//! expensive path, and the single hottest function in a `sample` of scrolling
//! it in either profile. That used to make the debug build scroll badly enough
//! to read as a bug in the widgets. **It no longer does**: the workspace
//! `Cargo.toml` raises `gpui-ce` and `taffy` for `dev` (measured there: 54.0ms
//! per frame with both at the default, 4.3ms with both raised), so `cargo run`
//! is usable and `--release` is merely faster.
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
    AnyElement, App, AppContext as _, Bounds, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, WindowBounds, WindowOptions, div, prelude::FluentBuilder as _, px, size,
};
use gpui_base::Button as BaseButton;
use ravel_core::animation::{Interpolation, Keyframe, KeyframeCurve};
use ravel_core::param_curve::CurveParam;
use ravel_core::param_ramp::RampParam;
use ravel_core::types::Vec2;
use ravel_widgets::curve_editor::{
    CurvePoint, CurveSeries, curve_editor_canvas, curve_editor_canvas_with_x_scale,
};
use ravel_widgets::curve_view::{
    CurveValueRange, format_value_label, nice_value_step, padded_bounds, value_grid_values,
};
use ravel_widgets::tokens::{Density, RavelTheme, ThemeMode, ThemeSpec};
use ravel_widgets::{
    Button, ButtonVariant, CHECKBOX_MARK_SIZE, CHECKBOX_SIZE, Checkbox, CheckboxState, ColorPicker,
    ColorPickerState, Icon, Input, InputState, NumberInput, ParamCurveEditor,
    ParamCurveEditorState, ParamRampEditor, ParamRampEditorState, SHOW_DELAY, TooltipExt as _,
    TooltipOverlay, UiIcon, button_layers, checkbox_layers, hex_color_string, hsla_from_hsv,
    input_layers, swatch, with_alpha,
};

/// One labelled block of the gallery.
///
/// The second argument exists for the two input sections: an `Input` paints an
/// `Entity<InputState>` that the caller owns, and an entity can only be built
/// with a window, so the states are made once when the gallery opens rather
/// than in the section function. The sections that do not paint text ignore it.
type SectionFn = fn(&RavelTheme, &GalleryInputs) -> AnyElement;

/// One widget's three statically-showable states, at one density step.
///
/// The fourth state — focused — is not in here because it cannot be staged:
/// it is what the *reader* produces by clicking or tabbing into a field, which
/// is exactly the thing the input sections exist to demonstrate.
struct StateRow {
    rest: Entity<InputState>,
    invalid: Entity<InputState>,
    disabled: Entity<InputState>,
}

impl StateRow {
    /// Three fresh states. One entity cannot serve two rows: each `Input`
    /// derives its element id from the state's entity id, so the same state
    /// painted twice in one frame would paint two elements with one id.
    fn new(value: &str, window: &mut Window, cx: &mut App) -> Self {
        fn make(
            value: &str,
            disabled: bool,
            window: &mut Window,
            cx: &mut App,
        ) -> Entity<InputState> {
            let value = value.to_string();
            cx.new(|cx: &mut Context<InputState>| {
                let mut state = InputState::new(window, cx).default_value(value);
                if disabled {
                    state.set_disabled(true, cx);
                }
                state
            })
        }
        Self {
            rest: make(value, false, window, cx),
            invalid: make(value, false, window, cx),
            disabled: make(value, true, window, cx),
        }
    }
}

/// Every `InputState` the gallery paints.
struct GalleryInputs {
    text: StateRow,
    text_compact: StateRow,
    number: StateRow,
    number_compact: StateRow,
    /// The two parameter editors own scrub fields of their own, so they are
    /// entities for the same reason the rows above are.
    param_curve: Entity<ParamCurveEditorState>,
    param_ramp: Entity<ParamRampEditorState>,
    /// The colour picker, opened on load: its face, hue strip, alpha rail and
    /// hex field only exist inside the popup, so a shut picker would show the
    /// gallery nothing but a 16px square.
    color: Entity<ColorPickerState>,
    /// A second picker at 40% alpha, left shut, so the swatch's slash underlay
    /// can be compared against the opaque one beside it.
    color_translucent: Entity<ColorPickerState>,
}

impl GalleryInputs {
    fn new(window: &mut Window, cx: &mut App) -> Self {
        Self {
            text: StateRow::new("Ravel", window, cx),
            text_compact: StateRow::new("Ravel", window, cx),
            number: StateRow::new("24", window, cx),
            number_compact: StateRow::new("24", window, cx),
            param_curve: cx.new(|cx| ParamCurveEditorState::new(CurveParam::default(), cx)),
            param_ramp: cx.new(|cx| ParamRampEditorState::new(RampParam::default(), cx)),
            color: cx.new(|cx| {
                let mut state = ColorPickerState::new(window, cx).default_value(gallery_color(1.0));
                state.set_open(true, cx);
                state
            }),
            color_translucent: cx
                .new(|cx| ColorPickerState::new(window, cx).default_value(gallery_color(0.4))),
        }
    }
}

/// Every section the gallery shows, in order.
///
/// **This is the registration point.** A widget that moves into this crate
/// gets one entry here; the window, the theme toggle, and the layout need no
/// change.
const SECTIONS: &[(&str, SectionFn)] = &[
    ("Tokens", tokens_section),
    ("icon · UiIcon at both densities", icon_section),
    ("button · every variant against every state", button_section),
    ("checkbox · three states and four looks", checkbox_section),
    (
        "input · the frame, its states, and a ring you can click",
        input_section,
    ),
    (
        "number_input · the same frame with a step on each side",
        number_input_section,
    ),
    ("tooltip · the popup and its delay", tooltip_section),
    (
        "color_picker · the face, the hue strip, alpha and hex",
        color_picker_section,
    ),
    ("curve_editor · curve_editor_canvas", curve_canvas_section),
    (
        "curve_editor · curve_editor_canvas_with_x_scale",
        curve_scaled_section,
    ),
    ("curve_view · range and grid", curve_view_section),
    (
        "param_curve_editor · the keyframe curve and its toolbar",
        param_curve_section,
    ),
    (
        "param_ramp_editor · the gradient ramp and its stops",
        param_ramp_section,
    ),
];

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

/// The parameter curve editor, at the size the Properties panel gives it.
///
/// Both this and [`param_ramp_section`] are the *whole* widget rather than the
/// canvas underneath: the toolbar, the scrub fields and the value readout are
/// where the tokens actually show, and they are what a palette change has to
/// be checked against.
fn param_curve_section(_theme: &RavelTheme, inputs: &GalleryInputs) -> AnyElement {
    // Both editors take their canvas height from the parent, exactly as the
    // Properties panel's resizable row does. A section that forgot to give
    // them one would show the toolbar and no picture.
    div()
        .w(px(360.0))
        .h(px(220.0))
        .child(ParamCurveEditor::new(&inputs.param_curve))
        .into_any_element()
}

/// The parameter ramp editor: the gradient, its stops, and the mode buttons.
fn param_ramp_section(_theme: &RavelTheme, inputs: &GalleryInputs) -> AnyElement {
    div()
        .w(px(360.0))
        .h(px(140.0))
        .child(ParamRampEditor::new(&inputs.param_ramp))
        .into_any_element()
}

/// The tokens themselves: ten colors, four spacing steps, three row heights,
/// two feedback durations, two radii. The visual baseline `UIX-3` wires the
/// widgets up against.
fn tokens_section(theme: &RavelTheme, _inputs: &GalleryInputs) -> AnyElement {
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
fn curve_canvas_section(theme: &RavelTheme, _inputs: &GalleryInputs) -> AnyElement {
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
fn curve_scaled_section(theme: &RavelTheme, _inputs: &GalleryInputs) -> AnyElement {
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
fn curve_view_section(theme: &RavelTheme, _inputs: &GalleryInputs) -> AnyElement {
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
fn icon_section(theme: &RavelTheme, _inputs: &GalleryInputs) -> AnyElement {
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
fn button_section(theme: &RavelTheme, _inputs: &GalleryInputs) -> AnyElement {
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

/// `Checkbox`: the three values it can hold, and the four looks each one has.
///
/// **The row to read is `disabled`.** A disabled *checked* box keeps its fill
/// at 38% instead of losing it, so it still reads as checked — the one place
/// this widget's disabled rule deliberately differs from the button's. A
/// mechanical "remove the surface" would draw it exactly like the disabled
/// unchecked box beside it.
fn checkbox_section(theme: &RavelTheme, _inputs: &GalleryInputs) -> AnyElement {
    let states = [
        (CheckboxState::Unchecked, "unchecked"),
        (CheckboxState::Checked, "checked"),
        (CheckboxState::Indeterminate, "indeterminate"),
    ];

    // Live checkboxes: both density steps and the states a caller sets. They
    // are controlled, so clicking one paints no new value — the section shows
    // the looks; the unit tests hold the transitions.
    let live = div()
        .flex()
        .flex_col()
        .gap(theme.spacing.sm)
        .children(states.map(|(state, name)| {
            let checkbox = move |id: &str, density: Density| {
                Checkbox::new(SharedString::from(format!("gallery-{name}-{id}")))
                    .density(density)
                    .state(state)
            };
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(theme.spacing.md)
                .child(div().w(px(96.0)).child(caption(theme, name.to_string())))
                .child(checkbox("compact", Density::Compact).label("compact"))
                .child(checkbox("default", Density::Default).label("default"))
                .child(checkbox("bare", Density::Default))
                .child(
                    checkbox("no-tab-stop", Density::Default)
                        .label("no tab stop")
                        .tab_stop(false),
                )
                .child(
                    checkbox("disabled", Density::Default)
                        .label("disabled")
                        .disabled(true),
                )
        }));

    // The same boxes, flat, from the pure function: the four looks side by
    // side without having to perform hover and press.
    let swatches = div()
        .flex()
        .flex_col()
        .gap(theme.spacing.xs)
        .children(states.map(|(state, name)| {
            let enabled = checkbox_layers(state, false, &theme.colors);
            let disabled = checkbox_layers(state, true, &theme.colors);
            let looks = [
                ("rest", enabled, None),
                ("hover", enabled, enabled.hover),
                ("press", enabled, enabled.pressed),
                ("disabled", disabled, None),
            ];
            div()
                .flex()
                .items_center()
                .gap(theme.spacing.sm)
                .child(div().w(px(96.0)).child(caption(theme, name.to_string())))
                .children(looks.map(|(look, layers, ground)| {
                    div()
                        .h(theme.rows.default)
                        .w(px(112.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(theme.spacing.xs)
                        .rounded(theme.radius.radius)
                        .when_some(ground, |cell, surface| cell.bg(surface))
                        .text_size(theme.text.mono_font_size)
                        .text_color(layers.label)
                        .child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(CHECKBOX_SIZE)
                                .rounded(theme.radius.radius)
                                .border_1()
                                .border_color(layers.indicator.border)
                                .bg(layers.indicator.surface)
                                .when_some(layers.indicator.mark.icon(), |box_, icon| {
                                    box_.child(
                                        Icon::new(icon)
                                            .size(CHECKBOX_MARK_SIZE)
                                            .text_color(layers.indicator.mark_color),
                                    )
                                }),
                        )
                        .child(look)
                }))
                .child(
                    // The ring on its own, as in the button section: it is a
                    // different layer from the hover surface and both can be on.
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
            "the box is 14px at both densities · the row it sits in is not · \
             Tab reaches every box but the fourth in each row · Enter and \
             Space toggle through the same handler a click does",
        ))
        .into_any_element()
}

/// `Tooltip`: the popup, on a button and on a bare element.
///
/// Both go through the same path — `TooltipExt::ravel_tooltip`, which is what
/// `Button::tooltip` calls — so there is one delay and one appearance. Hover
/// and wait 500ms; move straight to the next trigger and the next one is
/// already up, because the overlay stays warm for 300ms.
///
/// **Escape does nothing here on macOS**, and not because the tooltip ignores
/// it: a bare Escape never reaches GPUI at all (`MED-APP-43`). The dismissal
/// path is written and unit-tested; the keystroke is swallowed one layer
/// below.
/// The input frame in the three states that can be staged, at both density
/// steps — plus the one that cannot be staged and matters most.
///
/// **Read the last field first.** Click into it and a `primary` ring appears;
/// Tab into it and the same ring appears. That is the deliberate difference
/// from the button section above, where a *click* paints no ring at all
/// because a button's ring is `focus_visible`. If a change ever makes the two
/// behave alike, this is where it shows.
fn input_section(theme: &RavelTheme, inputs: &GalleryInputs) -> AnyElement {
    let row = |title: &str, states: &StateRow, density: Density| {
        let dress = move |input: Input| match density {
            Density::Compact => input.compact(),
            Density::Default => input,
        };
        div()
            .flex()
            .flex_col()
            .gap(theme.spacing.xs)
            .child(caption(theme, title.to_string()))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(theme.spacing.sm)
                    .child(labelled(
                        theme,
                        "rest",
                        dress(Input::new(&states.rest)).w(px(140.0)),
                    ))
                    .child(labelled(
                        theme,
                        "invalid",
                        dress(Input::new(&states.invalid))
                            .invalid(true)
                            .w(px(140.0)),
                    ))
                    .child(labelled(
                        theme,
                        "disabled",
                        dress(Input::new(&states.disabled)).w(px(140.0)),
                    )),
            )
            .into_any_element()
    };

    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.md)
        .child(row("default · 24px", &inputs.text, Density::Default))
        .child(row(
            "compact · 20px",
            &inputs.text_compact,
            Density::Compact,
        ))
        .child(caption(
            theme,
            "focused · click or Tab into any field above. The ring is `focus`, \
             not `focus_visible`: unlike a Button, a *click* paints it.",
        ))
        .child(caption(
            theme,
            format!(
                "border · rest {} · invalid {} · focused {}",
                hex_color_string(input_layers(false, false, &theme.colors).rest.border),
                hex_color_string(input_layers(true, false, &theme.colors).rest.border),
                hex_color_string(
                    input_layers(false, false, &theme.colors)
                        .focused
                        .expect("an enabled, valid input has a focused layer")
                        .border
                ),
            ),
        ))
        .into_any_element()
}

/// The spin button, in the same three states and the same two steps.
///
/// The step buttons are ghost buttons: hover one and it takes a surface, press
/// it and it takes another. On the disabled row they do neither, which is UX
/// invariant 6 — a control that cannot act must not react.
fn number_input_section(theme: &RavelTheme, inputs: &GalleryInputs) -> AnyElement {
    let row = |title: &str, states: &StateRow, density: Density| {
        let dress = move |input: NumberInput| match density {
            Density::Compact => input.compact(),
            Density::Default => input,
        };
        div()
            .flex()
            .flex_col()
            .gap(theme.spacing.xs)
            .child(caption(theme, title.to_string()))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(theme.spacing.sm)
                    .child(labelled(
                        theme,
                        "rest",
                        dress(NumberInput::new(&states.rest)).w(px(120.0)),
                    ))
                    .child(labelled(
                        theme,
                        "invalid",
                        dress(NumberInput::new(&states.invalid))
                            .invalid(true)
                            .w(px(120.0)),
                    ))
                    .child(labelled(
                        theme,
                        "disabled",
                        dress(NumberInput::new(&states.disabled)).w(px(120.0)),
                    )),
            )
            .into_any_element()
    };

    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.md)
        .child(row("default · 24px", &inputs.number, Density::Default))
        .child(row(
            "compact · 20px",
            &inputs.number_compact,
            Density::Compact,
        ))
        .child(caption(
            theme,
            "step · the two arrows and the Up / Down keys reach one handler, so \
             a value cannot be stepped two different ways.",
        ))
        .into_any_element()
}

/// A widget with its state name under it.
fn labelled(theme: &RavelTheme, name: &str, widget: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.xs)
        .child(widget)
        .child(caption(theme, name.to_string()))
}

/// The colour the gallery's pickers start on, at `alpha`.
fn gallery_color(alpha: f32) -> gpui::Hsla {
    hsla_from_hsv(0.62, 0.65, 0.9, alpha)
}

/// The colour picker: the popup's four surfaces, and the swatch at two alphas.
///
/// The first picker is **open when the gallery loads**, because everything the
/// widget decides — the face's two gradients, the hue strip's six bands, the
/// alpha rail's fill, the hex field — lives inside the popup. It floats over
/// the sections below it; that is what a popup does.
///
/// The two bare swatches beside it are the alpha requirement: at 40% the slash
/// underlay shows through, which is what tells "this colour is transparent"
/// apart from "this colour is dark" — a distinction a compositor cannot afford
/// to lose.
fn color_picker_section(theme: &RavelTheme, inputs: &GalleryInputs) -> AnyElement {
    let colors = &theme.colors;
    div()
        .flex()
        .flex_col()
        .gap(theme.spacing.md)
        .child(
            div()
                .flex()
                .items_start()
                .gap(theme.spacing.lg)
                .child(labelled(
                    theme,
                    "open · Tab into the face, then ←→ / ↑↓, Shift for 10%",
                    ColorPicker::new(&inputs.color),
                ))
                .child(labelled(
                    theme,
                    "compact · click to open",
                    ColorPicker::new(&inputs.color_translucent).compact(),
                )),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(theme.spacing.lg)
                .child(labelled(
                    theme,
                    "swatch · opaque",
                    swatch(Some(gallery_color(1.0)), colors),
                ))
                .child(labelled(
                    theme,
                    "swatch · alpha 40%",
                    swatch(Some(gallery_color(0.4)), colors),
                ))
                .child(labelled(
                    theme,
                    "swatch · alpha 0%",
                    swatch(Some(with_alpha(gallery_color(1.0), 0.0)), colors),
                ))
                .child(labelled(theme, "swatch · no value", swatch(None, colors))),
        )
        .child(caption(
            theme,
            format!(
                "fill {} · underlay {} · the face is HSV; the state is HSL",
                hex_color_string(colors.slider_fill()),
                hex_color_string(colors.border)
            ),
        ))
        .into_any_element()
}

fn tooltip_section(theme: &RavelTheme, _inputs: &GalleryInputs) -> AnyElement {
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
                "hover and wait {}ms · move across within 300ms and the next is instant · \
                 the surface is background mixed {}% toward black, with a 1px border \
                 and no shadow",
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
    /// The states the two input sections paint. Built once, when the window
    /// opens: the theme toggle swaps colours, never the text being edited.
    inputs: GalleryInputs,
    /// The one overlay every tooltip in this window is shown through. The
    /// gallery is a bare GPUI window with no `gpui_component::Root`, so it
    /// installs and renders Ravel's own — without this the tooltip section
    /// would silently show nothing.
    tooltip_overlay: Entity<TooltipOverlay>,
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
                    .child(section(&theme, &self.inputs))
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
            .child(self.tooltip_overlay.clone())
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(gpui_kit_assets::Assets)
        .run(|cx: &mut App| {
            gpui_base::init(cx);
            // The two parameter editors label their own toolbars through `t!`.
            // Without a locale loaded those labels would come out as their
            // keys, which is exactly the sort of thing the gallery exists to
            // catch — so it loads the shipped ones, from the repository rather
            // than from an embedded asset, since an example has the tree.
            let locales =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
            if let Err(error) = ravel_i18n::init(&locales, "en") {
                eprintln!("gallery: no locales ({error}); labels show their keys");
            }
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
                |window, cx| {
                    cx.new(|cx| Gallery {
                        mode: STARTING_MODE,
                        inputs: GalleryInputs::new(window, cx),
                        tooltip_overlay: ravel_widgets::install_tooltip_overlay(window, cx),
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
