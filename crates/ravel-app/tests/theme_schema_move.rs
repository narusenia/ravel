// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The schema move must not repaint anything.
//!
//! `assets/themes/ravel.json` is now read through Ravel's own schema
//! (`ravel_widgets::tokens`) and turned back into a `gpui_component::ThemeConfig`
//! by `ravel_app::theme_tokens`. What that must not change is what the running
//! application is painted with: the ten colours the derived theme carries — read
//! by the components Ravel still borrows, and by Ravel's own panels through
//! `cx.tokens()` — the two font families, and the radii.
//!
//! The comparison is against [`PRE_MIGRATION`], the theme file as it stood
//! *before* the move — not against the file next to it. Three of the ten
//! colours (`danger`, `info`, `drop_target`) were not written in the old file at
//! all: they came out of gpui-component's fallback chain (`danger` from
//! `base.red`, `info` from `base.cyan`, `drop_target` from `primary` at 0.2
//! alpha), and the migration wrote the resolved values into the file so Ravel's
//! schema could own them. Comparing the new file with itself would prove
//! nothing about whether those three values are the right ones.

use std::rc::Rc;

use gpui::{Hsla, Pixels, SharedString, TestAppContext};
use gpui_component::{Theme, ThemeConfig, ThemeSet};
use ravel_app::theme_tokens::derive_theme_set_json;

/// `assets/themes/ravel.json` before the move to Ravel's schema, minus the
/// `highlight` block (no Ravel code and no borrowed component reads it).
const PRE_MIGRATION: &str = r##"
{
  "name": "Ravel",
  "themes": [
    {
      "name": "Ravel Light",
      "mode": "light",
      "is_default": true,
      "shadow": false,
      "font.size": 14,
      "font.family": "Geist",
      "mono_font.size": 12,
      "mono_font.family": "JetBrains Mono",
      "radius": 4,
      "radius.lg": 6,
      "colors": {
        "accent.background": "#E0E0E0",
        "accent.foreground": "#000000",
        "background": "#F9F9F9",
        "foreground": "#000000",
        "border": "#D2D2D2",
        "ring": "#5B6EE1",
        "danger.foreground": "#FFFFFF",
        "list.active.background": "#5B6EE115",
        "list.active.border": "#5B6EE1",
        "list.even.background": "#EFEFEF",
        "list.hover.background": "#E7E7E7",
        "muted.background": "#EAEAEA",
        "muted.foreground": "#707070",
        "popover.background": "#F5F5F5",
        "popover.foreground": "#000000",
        "primary.background": "#5B6EE1",
        "primary.foreground": "#FFFFFF",
        "scrollbar.background": "#FFFFFF00",
        "scrollbar.thumb.background": "#C8C8C8",
        "secondary.active.background": "#D9D9D9",
        "secondary.background": "#E0E0E0",
        "secondary.foreground": "#000000",
        "secondary.hover.background": "#E5E5E5",
        "tab.active.foreground": "#000000",
        "tab.background": "#E9E9E9",
        "tab.foreground": "#606060",
        "tab_bar.background": "#E9E9E9",
        "title_bar.background": "#FEFEFE",
        "title_bar.border": "#DADADA",
        "status_bar.background": "#E9E9E9",
        "base.yellow": "#B59A00",
        "base.red": "#d21f07",
        "base.blue": "#5B6EE1",
        "base.green": "#319a00",
        "base.magenta": "#9A0068",
        "base.cyan": "#007E8A"
      }
    },
    {
      "name": "Ravel Dark",
      "mode": "dark",
      "is_default": true,
      "shadow": false,
      "font.size": 14,
      "font.family": "Geist",
      "mono_font.size": 12,
      "mono_font.family": "JetBrains Mono",
      "radius": 4,
      "radius.lg": 6,
      "colors": {
        "accent.background": "#282629",
        "accent.foreground": "#CACCCA",
        "background": "#131313",
        "border": "#303030",
        "ring": "#5B6EE1",
        "foreground": "#DEDEDE",
        "list.even.background": "#232323",
        "list.active.background": "#5B6EE115",
        "list.active.border": "#5B6EE1",
        "muted.background": "#202020",
        "muted.foreground": "#9D9D9D",
        "popover.background": "#101010",
        "popover.foreground": "#CACCCA",
        "primary.background": "#5B6EE1",
        "primary.foreground": "#F2F9FF",
        "switch.background": "#393939",
        "switch.thumb.background": "#DAECFF",
        "scrollbar.background": "#13131300",
        "scrollbar.thumb.background": "#9F9F9F",
        "secondary.active.background": "#353535",
        "secondary.background": "#353535",
        "secondary.foreground": "#DEDEDE",
        "secondary.hover.background": "#35353599",
        "link": "#419CFF",
        "tab_bar.background": "#1C1C1E",
        "tab.active.background": "#131313",
        "tab.foreground": "#8F8F8F",
        "title_bar.background": "#1C1C1E",
        "selection.background": "#3F638B",
        "base.blue": "#5B6EE1",
        "base.cyan": "#07FDD3",
        "base.green": "#30D158",
        "base.magenta": "#A550A7",
        "base.red": "#FF5257",
        "base.yellow": "#FFC600"
      }
    }
  ]
}"##;

/// Everything the application actually paints from, out of a live `Theme`.
#[derive(Debug, PartialEq)]
struct Painted {
    is_dark: bool,
    // The ten colours Ravel's schema models. The panels read them through
    // `cx.tokens()`; these are what the *borrowed* components see.
    background: Hsla,
    foreground: Hsla,
    border: Hsla,
    muted_foreground: Hsla,
    accent: Hsla,
    primary: Hsla,
    secondary: Hsla,
    danger: Hsla,
    info: Hsla,
    drop_target: Hsla,
    font_family: SharedString,
    font_size: Pixels,
    mono_font_family: SharedString,
    mono_font_size: Pixels,
    radius: Pixels,
    radius_lg: Pixels,
}

fn painted(theme: &Theme) -> Painted {
    Painted {
        is_dark: theme.is_dark(),
        background: theme.colors.background,
        foreground: theme.colors.foreground,
        border: theme.colors.border,
        muted_foreground: theme.colors.muted_foreground,
        accent: theme.colors.accent,
        primary: theme.colors.primary,
        secondary: theme.colors.secondary,
        danger: theme.colors.danger,
        info: theme.colors.info,
        drop_target: theme.colors.drop_target,
        font_family: theme.font_family.clone(),
        font_size: theme.font_size,
        mono_font_family: theme.mono_font_family.clone(),
        mono_font_size: theme.mono_font_size,
        radius: theme.radius,
        radius_lg: theme.radius_lg,
    }
}

/// Wear `config` and read back what it paints, exactly as a launch does
/// (`app_settings::apply_resolved_appearance` fills both slots and then changes
/// the mode).
fn wear(config: &ThemeConfig, cx: &mut gpui::App) -> Painted {
    let mode = config.mode;
    let theme = Theme::global_mut(cx);
    theme.light_theme = Rc::new(config.clone());
    theme.dark_theme = Rc::new(config.clone());
    Theme::change(mode, None, cx);
    painted(Theme::global(cx))
}

fn shipped_json() -> String {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes/ravel.json");
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn themes(json: &str) -> Vec<ThemeConfig> {
    serde_json::from_str::<ThemeSet>(json)
        .expect("the theme set parses")
        .themes
}

#[gpui::test]
fn the_schema_move_repaints_nothing(cx: &mut TestAppContext) {
    let before = themes(PRE_MIGRATION);
    let after = themes(
        &derive_theme_set_json(&shipped_json()).expect("the shipped theme derives a config"),
    );
    assert_eq!(before.len(), 2);
    assert_eq!(after.len(), before.len(), "same themes, in the same order");

    cx.update(|cx| {
        gpui_component::init(cx);
        for (before, after) in before.iter().zip(after.iter()) {
            assert_eq!(before.name, after.name, "the settings name a theme by name");
            assert_eq!(before.mode, after.mode);
            let expected = wear(before, cx);
            let actual = wear(after, cx);
            assert_eq!(actual, expected, "{} changed", before.name);
        }
    });
}

#[gpui::test]
fn the_unmodelled_colours_survive_the_derivation(cx: &mut TestAppContext) {
    // The twenty-six colours Ravel's schema does not model are what the
    // borrowed components (`Input`, `Root`, the eight one-offs) paint with.
    // They are carried across from the file rather than derived, so a
    // derivation that forgot the passthrough would strand them on
    // gpui-component's stock palette without failing anything above.
    let before = themes(PRE_MIGRATION);
    let after = themes(
        &derive_theme_set_json(&shipped_json()).expect("the shipped theme derives a config"),
    );

    cx.update(|cx| {
        gpui_component::init(cx);
        for (before, after) in before.iter().zip(after.iter()) {
            let mode = before.mode;
            let theme = Theme::global_mut(cx);
            theme.light_theme = Rc::new(before.clone());
            theme.dark_theme = Rc::new(before.clone());
            Theme::change(mode, None, cx);
            let expected = [
                Theme::global(cx).colors.popover,
                Theme::global(cx).colors.scrollbar_thumb,
                Theme::global(cx).colors.tab_bar,
                Theme::global(cx).colors.title_bar,
                Theme::global(cx).colors.list_active,
                Theme::global(cx).colors.ring,
                Theme::global(cx).colors.muted,
            ];

            let theme = Theme::global_mut(cx);
            theme.light_theme = Rc::new(after.clone());
            theme.dark_theme = Rc::new(after.clone());
            Theme::change(mode, None, cx);
            let actual = [
                Theme::global(cx).colors.popover,
                Theme::global(cx).colors.scrollbar_thumb,
                Theme::global(cx).colors.tab_bar,
                Theme::global(cx).colors.title_bar,
                Theme::global(cx).colors.list_active,
                Theme::global(cx).colors.ring,
                Theme::global(cx).colors.muted,
            ];
            assert_eq!(actual, expected, "{} changed", before.name);
        }
    });
}

#[gpui::test]
fn a_theme_that_omits_a_colour_gets_ravels_built_in_not_gpui_components(cx: &mut TestAppContext) {
    // The point of deriving at all: one source for a missing colour. Ravel's
    // widgets read `Colors::dark().border`; a borrowed component reads
    // `cx.theme().colors.border`. Both must be the same value.
    const SPARSE: &str = r##"{"themes": [{"name": "Sparse", "mode": "dark"}]}"##;
    let derived = themes(&derive_theme_set_json(SPARSE).expect("a sparse theme derives"));

    cx.update(|cx| {
        gpui_component::init(cx);
        let painted = wear(&derived[0], cx);
        let built_in = ravel_widgets::tokens::Colors::dark();
        assert_eq!(painted.border, built_in.border);
        assert_eq!(painted.background, built_in.background);
        assert_eq!(painted.muted_foreground, built_in.muted_foreground);
        assert_eq!(painted.danger, built_in.danger);
        assert_eq!(painted.drop_target, built_in.drop_target);
    });
}
