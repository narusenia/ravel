// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Ravel's theme schema, turned into the one gpui-component understands.
//!
//! Ravel's own tokens live in [`ravel_widgets::tokens`], which knows nothing
//! about gpui-component. The borrowed components — an `Input`, the window
//! `Root`, the eight one-off widgets — read `cx.theme()`, so somebody has to
//! keep feeding gpui-component's `Theme` from Ravel's values. That somebody is
//! this module, and the direction is fixed: **Ravel's schema is authoritative
//! and `ThemeConfig` is derived from it.** There is deliberately no conversion
//! the other way.
//!
//! Only the tokens Ravel's schema models are derived. gpui-component's schema
//! knows about roughly three dozen more colours (`popover.background`,
//! `scrollbar.thumb.background`, the tab and title-bar colours, the syntax
//! `highlight` block) that no Ravel code reads and that Ravel's schema does not
//! model. Those are carried across from the file untouched — dropping them
//! would repaint every borrowed component, and modelling them would grow
//! Ravel's schema by twenty-six colours nothing asks for.

use gpui_component::{ThemeConfig, ThemeMode, ThemeSet};
use ravel_widgets::tokens::{RavelTheme, ThemeFile, hex_color_string};

/// Derive gpui-component's theme from a Ravel one.
///
/// `unmodelled` supplies what Ravel's schema has no opinion about: the colours
/// listed in the module docs, plus `is_default`, `shadow` and `highlight`.
/// Everything Ravel does model is overwritten from `theme`, so a theme file that
/// leaves a Ravel colour out gets Ravel's built-in for it in **both** places
/// rather than Ravel's here and gpui-component's stock palette there.
pub fn derive_theme_config(theme: &RavelTheme, unmodelled: &ThemeConfig) -> ThemeConfig {
    let mut config = unmodelled.clone();
    config.name = theme.name.clone();
    config.mode = match theme.mode {
        ravel_widgets::tokens::ThemeMode::Light => ThemeMode::Light,
        ravel_widgets::tokens::ThemeMode::Dark => ThemeMode::Dark,
    };

    config.font_family = Some(theme.text.font_family.clone());
    config.font_size = Some(f32::from(theme.text.font_size));
    config.mono_font_family = Some(theme.text.mono_font_family.clone());
    config.mono_font_size = Some(f32::from(theme.text.mono_font_size));
    // gpui-component takes the radii as whole pixels.
    config.radius = Some(f32::from(theme.radius.radius).round() as usize);
    config.radius_lg = Some(f32::from(theme.radius.radius_lg).round() as usize);

    let colors = &theme.colors;
    let hex = |color| Some(hex_color_string(color).into());
    config.colors.background = hex(colors.background);
    config.colors.foreground = hex(colors.foreground);
    config.colors.border = hex(colors.border);
    config.colors.muted_foreground = hex(colors.muted_foreground);
    config.colors.accent = hex(colors.accent);
    config.colors.primary = hex(colors.primary);
    config.colors.secondary = hex(colors.secondary);
    config.colors.danger = hex(colors.danger);
    config.colors.info = hex(colors.info);
    config.colors.drop_target = hex(colors.drop_target);

    config
}

/// Read a theme file and hand back the same set as gpui-component JSON.
///
/// The registry only takes themes as text (`load_themes_from_str`), so the
/// derived configs go back through `serde_json` rather than being inserted
/// directly. That is the whole reason this returns a `String`.
///
/// The file is parsed twice, once under each schema, and the two theme lists are
/// paired by position — they are the same JSON array, so they cannot disagree on
/// length.
pub fn derive_theme_set_json(content: &str) -> anyhow::Result<String> {
    let unmodelled: ThemeSet = serde_json::from_str(content)?;
    let ravel: ThemeFile = serde_json::from_str(content)?;
    anyhow::ensure!(
        unmodelled.themes.len() == ravel.themes.len(),
        "the two schemas disagree on how many themes the file holds"
    );

    let themes = unmodelled
        .themes
        .iter()
        .zip(ravel.themes.iter())
        .map(|(unmodelled, spec)| derive_theme_config(&spec.resolve(), unmodelled))
        .collect();

    Ok(serde_json::to_string(&ThemeSet {
        name: unmodelled.name,
        author: unmodelled.author,
        url: unmodelled.url,
        themes,
    })?)
}
