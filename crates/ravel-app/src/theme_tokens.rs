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

use std::collections::HashMap;

use gpui::{App, Global};
use gpui_component::{ThemeConfig, ThemeMode, ThemeSet};
use ravel_widgets::tokens::{self, RavelTheme, ThemeFile, hex_color_string};

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

// ---------------------------------------------------------------------------
// The Ravel side of the same themes
// ---------------------------------------------------------------------------

/// The Ravel themes read out of the themes directory, by name and mode.
///
/// The registry exists because the derivation above is one-way and stays that
/// way: `ThemeRegistry` holds the *derived* `ThemeConfig`s, which is what the
/// borrowed components need, and there is deliberately no code that turns one
/// back into a [`RavelTheme`]. Ravel's own widgets need the Ravel form of
/// whichever theme is being worn, so the resolved themes are kept here as the
/// file is read rather than reconstructed later.
///
/// Keyed by name **and** mode: a file may name its light and dark themes the
/// same thing, and the appearance settings pick one per mode.
#[derive(Default)]
pub struct RavelThemes(HashMap<(String, tokens::ThemeMode), RavelTheme>);

impl Global for RavelThemes {}

/// Record the Ravel themes a theme file holds.
///
/// Called beside [`derive_theme_set_json`] on the same text, so the two
/// registries hold the same set. A file this fails on is one
/// `derive_theme_set_json` also fails on, and the caller already logs and
/// skips it.
pub fn register_ravel_themes(content: &str, cx: &mut App) -> anyhow::Result<()> {
    let file: ThemeFile = serde_json::from_str(content)?;
    let registry = cx.default_global::<RavelThemes>();
    for spec in &file.themes {
        registry
            .0
            .insert((spec.name.clone(), spec.mode), spec.resolve());
    }
    Ok(())
}

/// Install the tokens Ravel's own widgets paint from.
///
/// `name` and `mode` are what gpui-component's `Theme` ended up wearing, so
/// the two halves of the appearance cannot disagree. A name the registry does
/// not carry falls back to Ravel's built-in palette for that mode — the same
/// answer the theme lookup gives when a settings file names a theme that is no
/// longer installed, and the reason this cannot leave the widgets unpainted.
pub fn apply_ravel_theme(name: &str, mode: tokens::ThemeMode, cx: &mut App) {
    let resolved = cx
        .try_global::<RavelThemes>()
        .and_then(|registry| registry.0.get(&(name.to_string(), mode)))
        .cloned()
        .unwrap_or_else(|| {
            tokens::ThemeSpec {
                name: name.to_string(),
                mode,
                ..tokens::ThemeSpec::default()
            }
            .resolve()
        });
    ravel_widgets::set_active_tokens(resolved, cx);
}

#[cfg(test)]
mod ravel_theme_tests {
    use super::*;

    const FILE: &str = r##"{
        "name": "Set",
        "themes": [
            {"name": "T", "mode": "light", "colors": {"primary.background": "#010203"}},
            {"name": "T", "mode": "dark", "colors": {"primary.background": "#040506"}}
        ]
    }"##;

    #[gpui::test]
    fn the_registry_keeps_one_theme_per_name_and_mode(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_ravel_themes(FILE, cx).expect("the file parses");

            apply_ravel_theme("T", tokens::ThemeMode::Light, cx);
            assert_eq!(
                ravel_widgets::ActiveTokens::tokens(&*cx).colors.primary,
                tokens::parse_hex_color("#010203").unwrap(),
            );

            // Same name, other mode: the two must not collide, which keying by
            // name alone would make them do.
            apply_ravel_theme("T", tokens::ThemeMode::Dark, cx);
            assert_eq!(
                ravel_widgets::ActiveTokens::tokens(&*cx).colors.primary,
                tokens::parse_hex_color("#040506").unwrap(),
            );
        });
    }

    #[gpui::test]
    fn an_unknown_name_falls_back_to_the_built_ins_for_that_mode(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            register_ravel_themes(FILE, cx).expect("the file parses");
            apply_ravel_theme("gone", tokens::ThemeMode::Dark, cx);
            assert_eq!(
                ravel_widgets::ActiveTokens::tokens(&*cx).colors,
                tokens::Colors::dark(),
            );
        });
    }
}
