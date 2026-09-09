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

/// One theme, in both forms.
///
/// The derivation is one-way, so a theme that has been through it can no longer
/// say what it looked like in Ravel's schema. Both forms therefore travel
/// together from the one parse that produced them.
struct DerivedTheme {
    /// What the theme is keyed by: a file may name its light and dark themes
    /// the same thing.
    key: (String, tokens::ThemeMode),
    ravel: RavelTheme,
    derived: ThemeConfig,
}

/// Read a theme file under both schemas at once.
///
/// The file is parsed twice, once under each schema, and the two theme lists are
/// paired by position — they are the same JSON array, so they cannot disagree on
/// length. The returned [`ThemeSet`] carries the set's own metadata; its
/// `themes` are the file's underived ones, which the caller has no use for.
fn derive_file(content: &str) -> anyhow::Result<(ThemeSet, Vec<DerivedTheme>)> {
    let unmodelled: ThemeSet = serde_json::from_str(content)?;
    let ravel: ThemeFile = serde_json::from_str(content)?;
    anyhow::ensure!(
        unmodelled.themes.len() == ravel.themes.len(),
        "the two schemas disagree on how many themes the file holds"
    );

    let derived = unmodelled
        .themes
        .iter()
        .zip(ravel.themes.iter())
        .map(|(unmodelled, spec)| {
            let resolved = spec.resolve();
            DerivedTheme {
                key: (spec.name.clone(), spec.mode),
                derived: derive_theme_config(&resolved, unmodelled),
                ravel: resolved,
            }
        })
        .collect();
    Ok((unmodelled, derived))
}

/// Read a theme file and hand back the same set as gpui-component JSON.
///
/// The registry only takes themes as text (`load_themes_from_str`), so the
/// derived configs go back through `serde_json` rather than being inserted
/// directly. That is the whole reason this returns a `String`.
pub fn derive_theme_set_json(content: &str) -> anyhow::Result<String> {
    let (set, derived) = derive_file(content)?;
    Ok(serde_json::to_string(&ThemeSet {
        name: set.name,
        author: set.author,
        url: set.url,
        themes: derived.into_iter().map(|theme| theme.derived).collect(),
    })?)
}

// ---------------------------------------------------------------------------
// The Ravel side of the same themes
// ---------------------------------------------------------------------------

/// The themes read out of the themes directories, by name and mode.
///
/// Holds **both** forms of every theme: the [`RavelTheme`] Ravel's own widgets
/// paint from, and the `ThemeConfig` the borrowed components read. Two reasons
/// they live together rather than one form here and the other in
/// `ThemeRegistry`:
///
/// - the derivation is one-way and stays that way, so a `ThemeConfig` cannot be
///   turned back into a [`RavelTheme`] when a widget asks for one;
/// - **this set is rebuilt wholesale on every reload and the registry cannot
///   be.** `ThemeRegistry` only takes insertions (`load_themes_from_str` keeps
///   the first theme it sees under a name) and exposes no way to drop one, so
///   after a file is edited its entry there is the one from startup. This is
///   what an edited or deleted theme file is resolved against
///   (`app_settings::theme_named`); the registry is fed a copy for the
///   components and dialogs that read it directly.
///
/// Keyed by name **and** mode: a file may name its light and dark themes the
/// same thing, and the appearance settings pick one per mode.
#[derive(Default)]
pub struct RavelThemes(HashMap<(String, tokens::ThemeMode), DerivedTheme>);

impl Global for RavelThemes {}

impl RavelThemes {
    /// Add every theme one theme file holds, replacing any theme already here
    /// under the same name and mode.
    ///
    /// Replacing rather than keeping the first is what makes the user's themes
    /// directory win over the bundled one: the caller reads the directories in
    /// load order and the last file to claim a name is the one that keeps it
    /// (`crate::themes::load`).
    pub fn insert_file(&mut self, content: &str) -> anyhow::Result<()> {
        for theme in derive_file(content)?.1 {
            self.0.insert(theme.key.clone(), theme);
        }
        Ok(())
    }

    /// Publish this set as the one in force, **replacing** what was there.
    ///
    /// A reload builds a new set and installs it rather than adding to the
    /// installed one, which is the only way a theme whose file was deleted
    /// stops being offered.
    pub fn install(self, cx: &mut App) {
        cx.set_global(self);
    }

    /// gpui-component's form of the theme of that name and mode.
    pub fn config(&self, name: &str, mode: tokens::ThemeMode) -> Option<&ThemeConfig> {
        self.0
            .get(&(name.to_string(), mode))
            .map(|theme| &theme.derived)
    }

    /// Every theme as the JSON `gpui_component::ThemeRegistry` takes.
    ///
    /// Sorted by name and mode so a file that claims one name for both modes —
    /// which the registry, keyed by name alone, can only hold one of — resolves
    /// the same way on every load.
    pub fn registry_json(&self) -> anyhow::Result<String> {
        let mut keys: Vec<_> = self.0.keys().collect();
        keys.sort_by_key(|(name, mode)| (name.clone(), mode.is_dark()));
        Ok(serde_json::to_string(&ThemeSet {
            name: "Ravel".into(),
            author: None,
            url: None,
            themes: keys
                .into_iter()
                .map(|key| self.0[key].derived.clone())
                .collect(),
        })?)
    }
}

/// gpui-component's mode in Ravel's vocabulary.
pub fn ravel_mode(mode: ThemeMode) -> tokens::ThemeMode {
    match mode {
        ThemeMode::Light => tokens::ThemeMode::Light,
        ThemeMode::Dark => tokens::ThemeMode::Dark,
    }
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
        .and_then(|themes| themes.0.get(&(name.to_string(), mode)))
        .map(|theme| theme.ravel.clone())
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

    /// The set one theme file holds, installed the way the loader installs it.
    fn install(content: &str, cx: &mut App) {
        let mut themes = RavelThemes::default();
        themes.insert_file(content).expect("the file parses");
        themes.install(cx);
    }

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
            install(FILE, cx);

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
            install(FILE, cx);
            apply_ravel_theme("gone", tokens::ThemeMode::Dark, cx);
            assert_eq!(
                ravel_widgets::ActiveTokens::tokens(&*cx).colors,
                tokens::Colors::dark(),
            );
        });
    }
}
