// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The composition settings form shared by the New Composition and
//! Composition Settings dialogs (REQ-UI-013).
//!
//! The rows are generated from
//! [`ravel_ui::properties::composition::composition_fields`] — the same field
//! model the Properties panel renders — so the two paths cannot drift apart in
//! which settings they expose or in what order. The widgets differ from the
//! panel's on purpose: a dialog is where a user *types* a resolution, so the
//! numbers are text inputs rather than the panel's drag-to-scrub rows.
//!
//! The form owns no document state. It reads its initial values from the
//! settings it is built with and hands the edited settings back on demand, so
//! nothing reaches the document until the dialog is confirmed — a cancelled
//! New Composition leaves no undo step behind.

use gpui::*;
use gpui_component::color_picker::{ColorPicker, ColorPickerState};
use gpui_component::input::{InputState, NumberInput};
use gpui_component::{ActiveTheme, Sizable as _};
use ravel_i18n::t;
use ravel_ui::document::CompositionSettings;
use ravel_ui::properties::PropertyField;
use ravel_ui::properties::composition::{
    self, FIELD_BACKGROUND, FIELD_DURATION, FIELD_FRAME_RATE, FIELD_HEIGHT, FIELD_NAME,
    FIELD_WIDTH, composition_fields,
};

/// Localized label for a composition field key (the Properties panel resolves
/// the same `properties.field.*` keys).
fn field_label(key: &str) -> String {
    let lookup = format!("properties.field.{key}");
    let translated = ravel_i18n::translate(&lookup);
    if translated == lookup {
        key.to_string()
    } else {
        translated
    }
}

pub struct CompositionForm {
    /// Values the form was opened with. Numeric text that cannot be parsed
    /// falls back to these instead of to a zero.
    initial: CompositionSettings,
    name: Entity<InputState>,
    width: Entity<InputState>,
    height: Entity<InputState>,
    frame_rate: Entity<InputState>,
    duration: Entity<InputState>,
    background: Entity<ColorPickerState>,
    focus_handle: FocusHandle,
}

impl CompositionForm {
    pub fn new(initial: CompositionSettings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = |value: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).default_value(value))
        };
        let name = text(initial.name.clone(), window, cx);
        let width = text(initial.resolution.0.to_string(), window, cx);
        let height = text(initial.resolution.1.to_string(), window, cx);
        let frame_rate = text(format_fps(initial.frame_rate.as_f64()), window, cx);
        let duration = text(initial.duration_frames.to_string(), window, cx);
        let background_value = hsla_from_rgba(initial.background_color);
        let background =
            cx.new(|cx| ColorPickerState::new(window, cx).default_value(background_value));
        // Focus stays with the dialog's own focus trap: a form must not grab
        // focus while it is being constructed (`.agents/rules/gpui.md`).
        let focus_handle = cx.focus_handle();

        Self {
            initial,
            name,
            width,
            height,
            frame_rate,
            duration,
            background,
            focus_handle,
        }
    }

    /// The edited settings, clamped to a constructible composition. Blank or
    /// unparsable entries keep the value the form opened with.
    pub fn settings(&self, cx: &App) -> CompositionSettings {
        let name = self.name.read(cx).value().trim().to_string();
        let name = if name.is_empty() {
            self.initial.name.clone()
        } else {
            name
        };
        let fps = self
            .frame_rate
            .read(cx)
            .value()
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|fps| *fps > 0.0)
            .map(composition::frame_rate_from_fps)
            .unwrap_or(self.initial.frame_rate);
        CompositionSettings {
            name,
            resolution: (
                self.parse_u32(&self.width, self.initial.resolution.0, cx),
                self.parse_u32(&self.height, self.initial.resolution.1, cx),
            ),
            frame_rate: fps,
            duration_frames: u64::from(self.parse_u32(
                &self.duration,
                self.initial.duration_frames.min(u32::MAX as u64) as u32,
                cx,
            )),
            background_color: self
                .background
                .read(cx)
                .value()
                .map(rgba_from_hsla)
                .unwrap_or(self.initial.background_color),
        }
        .sanitized()
    }

    fn parse_u32(&self, input: &Entity<InputState>, fallback: u32, cx: &App) -> u32 {
        input
            .read(cx)
            .value()
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(fallback)
    }

    fn row(&self, key: &str, control: AnyElement, cx: &App) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .py(px(3.0))
            .child(
                // The label column is a fixed width, so a long translation has
                // to ellipsize rather than wrap the row onto a second line.
                div()
                    .w(px(120.0))
                    .flex_shrink_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().colors.muted_foreground)
                    .child(SharedString::from(field_label(key))),
            )
            .child(div().flex_grow().child(control))
    }
}

impl Focusable for CompositionForm {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CompositionForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut form = div()
            .flex()
            .flex_col()
            .w_full()
            .gap_1()
            .track_focus(&self.focus_handle);

        // Driven by the shared field model: a field added there shows up here
        // (as a labelled placeholder until it gets a widget) instead of being
        // silently dropped from the dialog.
        for field in composition_fields(&self.initial) {
            let key = field.key().to_string();
            let control: AnyElement = match key.as_str() {
                FIELD_NAME => gpui_component::input::Input::new(&self.name)
                    .small()
                    .into_any_element(),
                FIELD_WIDTH => NumberInput::new(&self.width).small().into_any_element(),
                FIELD_HEIGHT => NumberInput::new(&self.height).small().into_any_element(),
                FIELD_FRAME_RATE => NumberInput::new(&self.frame_rate)
                    .small()
                    .into_any_element(),
                FIELD_DURATION => NumberInput::new(&self.duration).small().into_any_element(),
                FIELD_BACKGROUND => ColorPicker::new(&self.background)
                    .small()
                    .into_any_element(),
                _ => div()
                    .text_xs()
                    .text_color(cx.theme().colors.muted_foreground)
                    .child(SharedString::from(placeholder_value(&field)))
                    .into_any_element(),
            };
            form = form.child(self.row(&key, control, cx));
        }
        form
    }
}

/// Display text for a field the dialog has no widget for yet.
fn placeholder_value(field: &PropertyField) -> String {
    match field {
        PropertyField::String { value, .. } | PropertyField::ReadOnly { value, .. } => {
            value.clone()
        }
        PropertyField::Float { value, .. } => format!("{value}"),
        PropertyField::Int { value, .. } => format!("{value}"),
        PropertyField::Bool { value, .. } => format!("{value}"),
        PropertyField::Enum { value, .. } => value.clone(),
        PropertyField::Color { r, g, b, a, .. } => format!("{r}, {g}, {b}, {a}"),
        PropertyField::Vector { components, .. } => format!("{components:?}"),
    }
}

/// Frame rates display as integers when they are whole (30, not 30.00) and
/// with two decimals otherwise (29.97).
fn format_fps(fps: f64) -> String {
    if (fps.round() - fps).abs() < 1e-6 {
        format!("{}", fps.round() as u64)
    } else {
        format!("{fps:.2}")
    }
}

fn hsla_from_rgba(color: ravel_core::types::Color) -> Hsla {
    Hsla::from(Rgba {
        r: color.r,
        g: color.g,
        b: color.b,
        a: color.a,
    })
}

fn rgba_from_hsla(hsla: Hsla) -> ravel_core::types::Color {
    let rgba = Rgba::from(hsla);
    ravel_core::types::Color::new(rgba.r, rgba.g, rgba.b, rgba.a)
}

/// Title of the dialog that hosts the form.
pub fn new_composition_title() -> SharedString {
    SharedString::from(t!("composition.dialog.new_title"))
}

pub fn settings_title() -> SharedString {
    SharedString::from(t!("composition.dialog.settings_title"))
}

#[cfg(test)]
mod tests {
    use super::*;
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back to
    // the built-in one so `#[test]` and `#[gpui::test]` both resolve to the
    // real ones (otherwise `#[gpui::test]` expands into itself).
    use core::prelude::v1::test;
    use ravel_core::types::{Color, FrameRate};

    fn settings() -> CompositionSettings {
        CompositionSettings {
            name: "Comp 1".into(),
            resolution: (1920, 1080),
            frame_rate: FrameRate::new(30, 1),
            duration_frames: 300,
            background_color: Color::BLACK,
        }
    }

    #[test]
    fn whole_frame_rates_display_without_decimals() {
        assert_eq!(format_fps(30.0), "30");
        assert_eq!(format_fps(24.0), "24");
        assert_eq!(
            format_fps(FrameRate::new(30_000, 1001).as_f64()),
            "29.97",
            "broadcast rates keep two decimals"
        );
    }

    #[test]
    fn color_conversion_round_trips() {
        let color = Color::new(0.25, 0.5, 0.75, 1.0);
        let back = rgba_from_hsla(hsla_from_rgba(color));
        assert!((back.r - color.r).abs() < 0.01);
        assert!((back.g - color.g).abs() < 0.01);
        assert!((back.b - color.b).abs() < 0.01);
        assert!((back.a - color.a).abs() < 0.01);
    }

    #[gpui::test]
    fn the_form_returns_its_initial_settings_untouched(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| CompositionForm::new(settings(), window, cx));
        window
            .update(cx, |form, _window, cx| {
                assert_eq!(form.settings(cx), settings());
            })
            .unwrap();
    }

    #[gpui::test]
    fn typed_values_are_parsed_and_blanks_fall_back(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let window = cx.add_window(|window, cx| CompositionForm::new(settings(), window, cx));
        window
            .update(cx, |form, window, cx| {
                form.name
                    .update(cx, |state, cx| state.set_value("Shot 2", window, cx));
                form.width
                    .update(cx, |state, cx| state.set_value("1280", window, cx));
                form.height
                    .update(cx, |state, cx| state.set_value("720", window, cx));
                form.frame_rate
                    .update(cx, |state, cx| state.set_value("29.97", window, cx));
                form.duration
                    .update(cx, |state, cx| state.set_value("120", window, cx));

                let edited = form.settings(cx);
                assert_eq!(edited.name, "Shot 2");
                assert_eq!(edited.resolution, (1280, 720));
                assert_eq!(edited.frame_rate, FrameRate::new(30_000, 1001));
                assert_eq!(edited.duration_frames, 120);
            })
            .unwrap();

        // Blank and unparsable entries keep the values the form opened with,
        // so a half-typed field can never build a 0×0 composition.
        window
            .update(cx, |form, window, cx| {
                form.name.update(cx, |s, cx| s.set_value("   ", window, cx));
                form.width.update(cx, |s, cx| s.set_value("", window, cx));
                form.height.update(cx, |s, cx| s.set_value("", window, cx));
                form.frame_rate
                    .update(cx, |s, cx| s.set_value("0", window, cx));
                form.duration
                    .update(cx, |s, cx| s.set_value("0", window, cx));

                let edited = form.settings(cx);
                assert_eq!(edited.name, "Comp 1");
                assert_eq!(edited.resolution, (1920, 1080));
                assert_eq!(edited.frame_rate, FrameRate::new(30, 1));
                assert_eq!(edited.duration_frames, 300);
            })
            .unwrap();
    }
}
