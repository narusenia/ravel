// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The shipped `assets/themes/ravel.json`, read through Ravel's own schema.
//!
//! This is the asset the application actually loads, so a key renamed in the
//! schema or a value edited in the file fails here rather than after launch.

use std::path::Path;

use ravel_widgets::tokens::{
    Colors, Motion, Radii, RavelTheme, Rows, Spacing, ThemeFile, ThemeMode, Typography,
    hex_color_string,
};

fn shipped() -> ThemeFile {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes/ravel.json");
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&json).expect("the shipped theme parses under Ravel's schema")
}

fn resolved() -> Vec<RavelTheme> {
    shipped().themes.iter().map(|spec| spec.resolve()).collect()
}

#[test]
fn the_shipped_file_carries_both_modes_under_their_settings_names() {
    let themes = resolved();
    assert_eq!(themes.len(), 2, "one theme per mode");
    // The names the appearance settings default to
    // (`ravel_project::settings::DEFAULT_LIGHT_THEME` / `DEFAULT_DARK_THEME`).
    assert_eq!(themes[0].name, "Ravel Light");
    assert_eq!(themes[0].mode, ThemeMode::Light);
    assert_eq!(themes[1].name, "Ravel Dark");
    assert_eq!(themes[1].mode, ThemeMode::Dark);
}

#[test]
fn the_shipped_palettes_are_exactly_the_built_in_ones() {
    // The built-ins in `tokens.rs` are what a theme file's gaps are filled
    // from, so the two must not drift: editing one colour in either place
    // without the other fails here. That is also this unit's guarantee that
    // moving to Ravel's schema changed no colour — the built-ins were
    // transcribed from this file.
    let themes = resolved();
    assert_eq!(themes[0].colors, Colors::light());
    assert_eq!(themes[1].colors, Colors::dark());
}

#[test]
fn every_shipped_colour_survives_a_hex_round_trip() {
    // A lossy hex formatter would shift the palette by 1/255 the first time a
    // theme is derived, which nothing else would notice.
    for theme in resolved() {
        let c = theme.colors;
        for colour in [
            c.background,
            c.foreground,
            c.border,
            c.muted_foreground,
            c.accent,
            c.primary,
            c.secondary,
            c.danger,
            c.info,
            c.drop_target,
        ] {
            let text = hex_color_string(colour);
            assert_eq!(
                ravel_widgets::tokens::parse_hex_color(&text).expect(&text),
                colour,
                "{text} did not survive the round trip in {}",
                theme.name
            );
        }
    }
}

#[test]
fn the_shipped_file_sets_no_ravel_only_token() {
    // Spacing, row heights and motion are not written in the file: the values
    // live in `tokens.rs` and the asset would only duplicate them. If this
    // starts failing, the file grew a token section — fine, but then the
    // built-in defaults are no longer the single source and this test should
    // become an equality check against the file's values instead.
    for theme in resolved() {
        assert_eq!(theme.spacing, Spacing::default());
        assert_eq!(theme.rows, Rows::default());
        assert_eq!(theme.motion, Motion::default());
    }
}

#[test]
fn the_shipped_file_sets_typography_and_radii() {
    for theme in resolved() {
        assert_eq!(theme.text, Typography::default(), "{}", theme.name);
        assert_eq!(theme.radius, Radii::default(), "{}", theme.name);
    }
}
