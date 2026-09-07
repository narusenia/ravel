// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's design tokens: the single source for colors, spacing, density,
//! typography, motion and corner radii.
//!
//! Two forms live here, because a theme file and the code that paints from it
//! want different things:
//!
//! - the **wire form** ([`ThemeFile`], [`ThemeSpec`] and the `*Spec` structs) is
//!   what a user writes as JSON. Every field is optional, so a file naming one
//!   key is as valid as a complete one;
//! - the **resolved form** ([`RavelTheme`]) is what the widgets read. It has no
//!   `Option`: [`ThemeSpec::resolve`] fills every gap from the built-in palette
//!   for that mode.
//!
//! # Why Ravel owns the schema
//!
//! gpui-component's theme schema stops at colors, one font size and a radius.
//! Spacing on a 4px step and named durations have nowhere to live in it, and
//! adding them to the fork would collide on every upstream follow
//! (`docs/implementation/ui-component-layer-plan.md`). Ravel's schema is
//! therefore authoritative, and `gpui_component::ThemeConfig` is *derived* from
//! it by the application host — never the other way round. This crate does not
//! depend on gpui-component and must not start to.
//!
//! # How a broken value is treated
//!
//! Per key, not per file. A color string that is not valid hex is dropped and
//! the built-in value for that key is used, so one typo costs one color rather
//! than the whole theme. Malformed JSON, or a value of the wrong JSON *type*,
//! still fails the file — which is what the loader wants, since it already logs
//! and skips a theme file it cannot read without harming the others.

use std::time::Duration;

use gpui::{Hsla, Pixels, Rgba, SharedString, px};
use serde::{Deserialize, Deserializer};

/// Which of the two palettes a theme is written for.
///
/// Mirrors `gpui_component::ThemeMode` in meaning but not in type: this crate
/// does not depend on gpui-component.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Light,
    Dark,
}

impl ThemeMode {
    /// Whether this is the dark palette.
    pub fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }
}

// ---------------------------------------------------------------------------
// Wire form
// ---------------------------------------------------------------------------

/// One theme file: a named set holding one theme per mode.
///
/// The container matches the shape the shipped `assets/themes/ravel.json`
/// already uses (`name` / `author` / `url` / `themes`), so one file carries both
/// the light and the dark theme.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ThemeFile {
    /// Name of the set, for the author's benefit; not a theme name.
    pub name: String,
    pub author: Option<String>,
    pub url: Option<String>,
    /// The themes in the file, in file order.
    pub themes: Vec<ThemeSpec>,
}

/// One theme as written in JSON. Every field is optional.
///
/// The dotted key names (`font.size`, `radius.lg`, and the color names in
/// [`ColorSpec`]) are deliberate: they are the names the existing theme files
/// use, so moving to this schema does not rewrite anyone's file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ThemeSpec {
    /// The name shown in the appearance settings.
    pub name: String,
    pub mode: ThemeMode,
    pub colors: ColorSpec,

    #[serde(rename = "font.family")]
    pub font_family: Option<String>,
    #[serde(rename = "font.size")]
    pub font_size: Option<f32>,
    #[serde(rename = "mono_font.family")]
    pub mono_font_family: Option<String>,
    #[serde(rename = "mono_font.size")]
    pub mono_font_size: Option<f32>,

    #[serde(rename = "radius")]
    pub radius: Option<f32>,
    #[serde(rename = "radius.lg")]
    pub radius_lg: Option<f32>,

    pub spacing: SpacingSpec,
    pub row: RowSpec,
    pub motion: MotionSpec,
}

/// The colors a theme may set.
///
/// These ten are the ones Ravel's own panels read; the rest of what
/// gpui-component's schema knows about is not modelled here (see
/// [`ThemeSpec::resolve`]).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ColorSpec {
    #[serde(rename = "background", deserialize_with = "lenient_color")]
    pub background: Option<Hsla>,
    #[serde(rename = "foreground", deserialize_with = "lenient_color")]
    pub foreground: Option<Hsla>,
    #[serde(rename = "border", deserialize_with = "lenient_color")]
    pub border: Option<Hsla>,
    #[serde(rename = "muted.foreground", deserialize_with = "lenient_color")]
    pub muted_foreground: Option<Hsla>,
    #[serde(rename = "accent.background", deserialize_with = "lenient_color")]
    pub accent: Option<Hsla>,
    #[serde(rename = "primary.background", deserialize_with = "lenient_color")]
    pub primary: Option<Hsla>,
    #[serde(rename = "secondary.background", deserialize_with = "lenient_color")]
    pub secondary: Option<Hsla>,
    #[serde(rename = "danger.background", deserialize_with = "lenient_color")]
    pub danger: Option<Hsla>,
    #[serde(rename = "info.background", deserialize_with = "lenient_color")]
    pub info: Option<Hsla>,
    #[serde(rename = "drop_target.background", deserialize_with = "lenient_color")]
    pub drop_target: Option<Hsla>,
}

/// Spacing steps, in pixels. Multiples of 4.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SpacingSpec {
    pub xs: Option<f32>,
    pub sm: Option<f32>,
    pub md: Option<f32>,
    pub lg: Option<f32>,
}

/// Row heights, in pixels.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RowSpec {
    pub compact: Option<f32>,
    pub default: Option<f32>,
    pub header: Option<f32>,
}

/// Feedback durations, in milliseconds.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MotionSpec {
    #[serde(rename = "feedback.in")]
    pub feedback_in: Option<u64>,
    #[serde(rename = "feedback.out")]
    pub feedback_out: Option<u64>,
}

// ---------------------------------------------------------------------------
// Resolved form
// ---------------------------------------------------------------------------

/// A theme with every token filled in — what the widgets read.
#[derive(Debug, Clone, PartialEq)]
pub struct RavelTheme {
    pub name: SharedString,
    pub mode: ThemeMode,
    pub colors: Colors,
    pub spacing: Spacing,
    pub rows: Rows,
    pub text: Typography,
    pub motion: Motion,
    pub radius: Radii,
}

/// The ten colors Ravel's panels paint with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colors {
    pub background: Hsla,
    pub foreground: Hsla,
    pub border: Hsla,
    pub muted_foreground: Hsla,
    pub accent: Hsla,
    pub primary: Hsla,
    pub secondary: Hsla,
    pub danger: Hsla,
    pub info: Hsla,
    pub drop_target: Hsla,
}

/// Spacing steps. One 4px step apart, four steps — enough for the panels that
/// exist. Add a step when a layout needs one, not before.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spacing {
    pub xs: Pixels,
    pub sm: Pixels,
    pub md: Pixels,
    pub lg: Pixels,
}

/// Row heights. Two, by purpose: `compact` for a row that shows one value,
/// `default` for a row in a list. `header` matches `default` so a panel header
/// and its first row sit on one step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rows {
    pub compact: Pixels,
    pub default: Pixels,
    pub header: Pixels,
}

/// Font families and sizes.
#[derive(Debug, Clone, PartialEq)]
pub struct Typography {
    pub font_family: SharedString,
    pub font_size: Pixels,
    pub mono_font_family: SharedString,
    pub mono_font_size: Pixels,
}

/// The only two durations Ravel animates over.
///
/// Both describe *state* feedback — a hover settling in, a drop target going
/// out. Values and layout never animate (UX invariant 11), so there is nothing
/// else to name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Motion {
    pub feedback_in: Duration,
    pub feedback_out: Duration,
}

/// Corner radii.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Radii {
    pub radius: Pixels,
    pub radius_lg: Pixels,
}

impl Colors {
    /// Ravel's light palette.
    pub fn light() -> Self {
        Self {
            background: hex("#F9F9F9"),
            foreground: hex("#000000"),
            border: hex("#D2D2D2"),
            muted_foreground: hex("#707070"),
            accent: hex("#E0E0E0"),
            primary: hex("#5B6EE1"),
            secondary: hex("#E0E0E0"),
            danger: hex("#d21f07"),
            info: hex("#007E8A"),
            drop_target: hex("#5B6EE133"),
        }
    }

    /// Ravel's dark palette.
    pub fn dark() -> Self {
        Self {
            background: hex("#131313"),
            foreground: hex("#DEDEDE"),
            border: hex("#303030"),
            muted_foreground: hex("#9D9D9D"),
            accent: hex("#282629"),
            primary: hex("#5B6EE1"),
            secondary: hex("#353535"),
            danger: hex("#FF5257"),
            info: hex("#07FDD3"),
            drop_target: hex("#5B6EE133"),
        }
    }

    /// The palette for `mode`.
    pub fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self::light(),
            ThemeMode::Dark => Self::dark(),
        }
    }
}

impl Default for Spacing {
    fn default() -> Self {
        Self {
            xs: px(4.0),
            sm: px(8.0),
            md: px(12.0),
            lg: px(16.0),
        }
    }
}

impl Default for Rows {
    fn default() -> Self {
        Self {
            compact: px(20.0),
            default: px(24.0),
            header: px(24.0),
        }
    }
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            font_family: "Geist".into(),
            font_size: px(14.0),
            mono_font_family: "JetBrains Mono".into(),
            mono_font_size: px(12.0),
        }
    }
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            feedback_in: Duration::from_millis(120),
            feedback_out: Duration::from_millis(180),
        }
    }
}

impl Default for Radii {
    fn default() -> Self {
        Self {
            radius: px(4.0),
            radius_lg: px(6.0),
        }
    }
}

impl ThemeSpec {
    /// Fill every unset token from the built-ins for this theme's mode.
    ///
    /// Colors fall back per key rather than per palette: a file that sets only
    /// `border` keeps Ravel's own nine other colors instead of inheriting a
    /// half-built palette.
    pub fn resolve(&self) -> RavelTheme {
        let base = Colors::for_mode(self.mode);
        let spacing = Spacing::default();
        let rows = Rows::default();
        let text = Typography::default();
        let motion = Motion::default();
        let radius = Radii::default();

        RavelTheme {
            name: SharedString::from(self.name.clone()),
            mode: self.mode,
            colors: Colors {
                background: self.colors.background.unwrap_or(base.background),
                foreground: self.colors.foreground.unwrap_or(base.foreground),
                border: self.colors.border.unwrap_or(base.border),
                muted_foreground: self
                    .colors
                    .muted_foreground
                    .unwrap_or(base.muted_foreground),
                accent: self.colors.accent.unwrap_or(base.accent),
                primary: self.colors.primary.unwrap_or(base.primary),
                secondary: self.colors.secondary.unwrap_or(base.secondary),
                danger: self.colors.danger.unwrap_or(base.danger),
                info: self.colors.info.unwrap_or(base.info),
                drop_target: self.colors.drop_target.unwrap_or(base.drop_target),
            },
            spacing: Spacing {
                xs: self.spacing.xs.map_or(spacing.xs, px),
                sm: self.spacing.sm.map_or(spacing.sm, px),
                md: self.spacing.md.map_or(spacing.md, px),
                lg: self.spacing.lg.map_or(spacing.lg, px),
            },
            rows: Rows {
                compact: self.row.compact.map_or(rows.compact, px),
                default: self.row.default.map_or(rows.default, px),
                header: self.row.header.map_or(rows.header, px),
            },
            text: Typography {
                font_family: self
                    .font_family
                    .clone()
                    .map_or(text.font_family, SharedString::from),
                font_size: self.font_size.map_or(text.font_size, px),
                mono_font_family: self
                    .mono_font_family
                    .clone()
                    .map_or(text.mono_font_family, SharedString::from),
                mono_font_size: self.mono_font_size.map_or(text.mono_font_size, px),
            },
            motion: Motion {
                feedback_in: self
                    .motion
                    .feedback_in
                    .map_or(motion.feedback_in, Duration::from_millis),
                feedback_out: self
                    .motion
                    .feedback_out
                    .map_or(motion.feedback_out, Duration::from_millis),
            },
            radius: Radii {
                radius: self.radius.map_or(radius.radius, px),
                radius_lg: self.radius_lg.map_or(radius.radius_lg, px),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Hex colors
// ---------------------------------------------------------------------------

/// Why a hex color string could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HexColorError {
    /// Not `#` followed by 3, 4, 6 or 8 hex digits.
    Shape,
    /// A digit outside `0-9a-fA-F`.
    Digit,
}

impl std::fmt::Display for HexColorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shape => f.write_str("expected #RGB, #RGBA, #RRGGBB or #RRGGBBAA"),
            Self::Digit => f.write_str("not a hexadecimal digit"),
        }
    }
}

impl std::error::Error for HexColorError {}

/// Read `#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`.
///
/// **Alpha is last and two digits**, matching CSS, `gpui::Rgba`'s own parser and
/// every value in the shipped theme (`"#5B6EE115"` is the Ravel blue at 21/255
/// alpha, not a dark blue on a `0x5B` alpha). Getting that backwards would
/// change every translucent color in the app without failing anything, which is
/// why [`hex_round_trip`](self) is tested in both directions.
pub fn parse_hex_color(value: &str) -> Result<Hsla, HexColorError> {
    let digits = value
        .trim()
        .strip_prefix('#')
        .ok_or(HexColorError::Shape)?
        .as_bytes();
    // The short forms repeat each digit, as CSS does: #abc == #aabbcc.
    let byte = |i: usize| -> Result<f32, HexColorError> {
        let pair = match digits.len() {
            3 | 4 => [digits[i], digits[i]],
            6 | 8 => [digits[i * 2], digits[i * 2 + 1]],
            _ => return Err(HexColorError::Shape),
        };
        let text = std::str::from_utf8(&pair).map_err(|_| HexColorError::Digit)?;
        let raw = u8::from_str_radix(text, 16).map_err(|_| HexColorError::Digit)?;
        Ok(f32::from(raw) / 255.0)
    };

    match digits.len() {
        3 | 4 | 6 | 8 => {}
        _ => return Err(HexColorError::Shape),
    }
    let has_alpha = digits.len() == 4 || digits.len() == 8;
    let rgba = Rgba {
        r: byte(0)?,
        g: byte(1)?,
        b: byte(2)?,
        a: if has_alpha { byte(3)? } else { 1.0 },
    };
    Ok(rgba.into())
}

/// Write a color back as `#RRGGBB`, or `#RRGGBBAA` when it is translucent.
///
/// Rounds each channel rather than truncating, so
/// `parse_hex_color(&hex_color_string(c))` returns `c` for every color that came
/// from a hex string in the first place. Truncating loses a step on most values
/// and would drift the palette by 1/255 per conversion.
pub fn hex_color_string(color: Hsla) -> String {
    let rgba: Rgba = color.into();
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if rgba.a >= 1.0 {
        format!(
            "#{:02X}{:02X}{:02X}",
            channel(rgba.r),
            channel(rgba.g),
            channel(rgba.b)
        )
    } else {
        format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            channel(rgba.r),
            channel(rgba.g),
            channel(rgba.b),
            channel(rgba.a)
        )
    }
}

/// A color literal from Ravel's own palettes.
///
/// Panics on a bad literal, which is a bug in this file and nothing a theme file
/// can reach; `builtin_palettes_are_valid_hex` keeps that honest.
fn hex(literal: &str) -> Hsla {
    match parse_hex_color(literal) {
        Ok(color) => color,
        Err(error) => panic!("built-in palette literal {literal:?}: {error}"),
    }
}

/// Read an optional color, treating an unreadable one as unset.
///
/// The key-level fallback from the module docs lives here: the value is taken as
/// a string first, so a bad hex string costs that one color while a value that
/// is not a string at all still fails the file.
fn lenient_color<'de, D>(deserializer: D) -> Result<Option<Hsla>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(text) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    Ok(parse_hex_color(&text).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: &str) -> ThemeSpec {
        serde_json::from_str(json).expect("the test JSON parses")
    }

    #[test]
    fn builtin_palettes_are_valid_hex() {
        // `hex` panics on a bad literal; naming both palettes runs every one.
        let _ = Colors::light();
        let _ = Colors::dark();
    }

    #[test]
    fn six_digit_hex_is_opaque_and_eight_digit_puts_alpha_last() {
        let opaque = parse_hex_color("#5B6EE1").expect("six digits");
        let translucent = parse_hex_color("#5B6EE115").expect("eight digits");

        // Same color, different alpha: the last two digits are the alpha, and
        // reading them as a leading channel would move the hue instead.
        assert_eq!(opaque.h, translucent.h);
        assert_eq!(opaque.s, translucent.s);
        assert_eq!(opaque.l, translucent.l);
        assert_eq!(opaque.a, 1.0);
        assert_eq!(translucent.a, 21.0 / 255.0);

        // And the channels are where they should be, not rotated: reading the
        // pairs in another order would put them back out in another order.
        assert_eq!(hex_color_string(opaque), "#5B6EE1");
        assert_eq!(hex_color_string(translucent), "#5B6EE115");
    }

    #[test]
    fn short_hex_repeats_each_digit() {
        assert_eq!(
            parse_hex_color("#abc").expect("three digits"),
            parse_hex_color("#aabbcc").expect("six digits")
        );
        assert_eq!(
            parse_hex_color("#abc8").expect("four digits"),
            parse_hex_color("#aabbcc88").expect("eight digits")
        );
    }

    #[test]
    fn hex_round_trips_through_the_formatter() {
        // Every channel value that appears in the shipped theme, plus the
        // extremes: a rounding bug in either direction shifts one of these.
        for literal in [
            "#000000",
            "#FFFFFF",
            "#5B6EE1",
            "#5B6EE115",
            "#5B6EE133",
            "#F9F9F9",
            "#D2D2D2",
            "#707070",
            "#E0E0E0",
            "#131313",
            "#DEDEDE",
            "#303030",
            "#9D9D9D",
            "#282629",
            "#353535",
            "#D21F07",
            "#FF5257",
            "#007E8A",
            "#07FDD3",
            "#35353599",
            "#FFFFFF00",
            "#13131300",
        ] {
            let color = parse_hex_color(literal).expect(literal);
            assert_eq!(hex_color_string(color), literal, "round trip of {literal}");
            assert_eq!(
                parse_hex_color(&hex_color_string(color)).expect(literal),
                color,
                "reparse of {literal}"
            );
        }
    }

    #[test]
    fn bad_hex_is_rejected_by_shape_and_by_digit() {
        assert_eq!(parse_hex_color("5B6EE1"), Err(HexColorError::Shape));
        assert_eq!(parse_hex_color("#5B6EE"), Err(HexColorError::Shape));
        assert_eq!(parse_hex_color("#"), Err(HexColorError::Shape));
        assert_eq!(parse_hex_color("#ZZZZZZ"), Err(HexColorError::Digit));
        assert_eq!(parse_hex_color("#5b6ee1"), parse_hex_color("#5B6EE1"));
    }

    #[test]
    fn empty_json_resolves_to_the_light_built_ins() {
        let theme = spec("{}").resolve();
        assert_eq!(theme.mode, ThemeMode::Light);
        assert_eq!(theme.colors, Colors::light());
        assert_eq!(theme.spacing, Spacing::default());
        assert_eq!(theme.rows, Rows::default());
        assert_eq!(theme.text, Typography::default());
        assert_eq!(theme.motion, Motion::default());
        assert_eq!(theme.radius, Radii::default());
    }

    #[test]
    fn one_key_changes_that_key_only() {
        let theme = spec(r##"{"colors": {"border": "#123456"}}"##).resolve();
        assert_eq!(theme.colors.border, parse_hex_color("#123456").unwrap());
        assert_eq!(
            Colors {
                border: Colors::light().border,
                ..theme.colors
            },
            Colors::light(),
            "the other nine colors stay at the built-ins"
        );
        assert_eq!(theme.rows, Rows::default());
    }

    #[test]
    fn mode_picks_the_palette_the_gaps_are_filled_from() {
        assert_eq!(spec(r#"{"mode": "dark"}"#).resolve().colors, Colors::dark());
        assert_eq!(
            spec(r#"{"mode": "light"}"#).resolve().colors,
            Colors::light()
        );
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // Both a key the schema never had and one gpui-component's schema owns
        // but Ravel's does not model. The shipped file is full of the latter.
        let theme = spec(
            r##"{
                 "name": "T",
                 "is_default": true,
                 "shadow": false,
                 "not_a_token": 7,
                 "highlight": {"editor.foreground": "#000000"},
                 "colors": {"popover.background": "#123456", "base.red": "#ff0000"}
               }"##,
        )
        .resolve();
        assert_eq!(theme.name, "T");
        assert_eq!(theme.colors, Colors::light());
    }

    #[test]
    fn invalid_hex_falls_back_for_that_key_alone() {
        let theme =
            spec(r##"{"colors": {"border": "not a color", "foreground": "#00FF00"}}"##).resolve();
        assert_eq!(theme.colors.border, Colors::light().border);
        assert_eq!(theme.colors.foreground, parse_hex_color("#00FF00").unwrap());
    }

    #[test]
    fn a_color_of_the_wrong_json_type_fails_the_file() {
        // The other half of the rule in the module docs: a typo in the hex
        // string is per-key, a value that is not a string at all is not.
        let error = serde_json::from_str::<ThemeSpec>(r#"{"colors": {"border": 12}}"#);
        assert!(error.is_err(), "a numeric color is a file-level error");
        assert!(serde_json::from_str::<ThemeFile>("{").is_err());
    }

    #[test]
    fn every_token_can_be_set_from_json() {
        let theme = spec(
            r##"{
                 "name": "Full",
                 "mode": "dark",
                 "font.family": "F",
                 "font.size": 15,
                 "mono_font.family": "M",
                 "mono_font.size": 11,
                 "radius": 2,
                 "radius.lg": 3,
                 "spacing": {"xs": 1, "sm": 2, "md": 3, "lg": 5},
                 "row": {"compact": 8, "default": 13, "header": 21},
                 "motion": {"feedback.in": 34, "feedback.out": 55},
                 "colors": {
                   "background": "#010101", "foreground": "#020202",
                   "border": "#030303", "muted.foreground": "#040404",
                   "accent.background": "#050505", "primary.background": "#060606",
                   "secondary.background": "#070707", "danger.background": "#080808",
                   "info.background": "#090909", "drop_target.background": "#0A0A0A80"
                 }
               }"##,
        )
        .resolve();

        assert_eq!(theme.name, "Full");
        assert_eq!(theme.mode, ThemeMode::Dark);
        assert!(theme.mode.is_dark());
        assert_eq!(
            theme.text,
            Typography {
                font_family: "F".into(),
                font_size: px(15.0),
                mono_font_family: "M".into(),
                mono_font_size: px(11.0),
            }
        );
        assert_eq!(
            theme.radius,
            Radii {
                radius: px(2.0),
                radius_lg: px(3.0)
            }
        );
        assert_eq!(
            theme.spacing,
            Spacing {
                xs: px(1.0),
                sm: px(2.0),
                md: px(3.0),
                lg: px(5.0)
            }
        );
        assert_eq!(
            theme.rows,
            Rows {
                compact: px(8.0),
                default: px(13.0),
                header: px(21.0)
            }
        );
        assert_eq!(
            theme.motion,
            Motion {
                feedback_in: Duration::from_millis(34),
                feedback_out: Duration::from_millis(55),
            }
        );
        assert_eq!(theme.colors.background, parse_hex_color("#010101").unwrap());
        assert_eq!(
            theme.colors.drop_target,
            parse_hex_color("#0A0A0A80").unwrap()
        );
        assert_ne!(theme.colors, Colors::dark(), "nothing fell back");
    }

    #[test]
    fn default_spacing_walks_a_four_pixel_step() {
        let spacing = Spacing::default();
        for step in [spacing.xs, spacing.sm, spacing.md, spacing.lg] {
            assert_eq!(f32::from(step) % 4.0, 0.0, "{step:?} is not a 4px step");
        }
    }
}
