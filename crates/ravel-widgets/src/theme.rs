// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! How a widget reaches the tokens.
//!
//! [`tokens`](crate::tokens) defines what the tokens *are*; this module is the
//! one path from a widget's `render` to the set that is in force. It is the
//! counterpart of `gpui_component::ActiveTheme` (`cx.theme()`) for Ravel's own
//! schema, and it is deliberately named so the two can be imported side by
//! side while the panels still borrow components: `cx.tokens()` here,
//! `cx.theme()` there.
//!
//! **This is the minimum reach, not the wiring.** Feeding the global from the
//! user's theme file, and repainting the panels from it, is `UIX-3`'s work.
//! What lives here is only what a widget cannot be written without.

use std::sync::OnceLock;

use gpui::{App, Global};

use crate::tokens::{RavelTheme, ThemeSpec};

/// The token set in force, for one application.
struct ActiveRavelTheme(RavelTheme);

impl Global for ActiveRavelTheme {}

/// Install the tokens every Ravel widget paints from, replacing any previous
/// set.
///
/// Call it once at startup and again whenever the appearance changes; the
/// widgets read it on their next paint, so the caller's only other duty is the
/// `cx.refresh_windows()` it already owes for the components that borrow
/// gpui-component's theme.
pub fn set_active_tokens(theme: RavelTheme, cx: &mut App) {
    project_onto_gpui_base(&theme, cx);
    cx.set_global(ActiveRavelTheme(theme));
}

/// Copy the tokens `gpui-base` paints text with onto its own palette.
///
/// **Ravel's schema stays authoritative; this is one more derivation from it**,
/// the same direction `ravel-app` derives gpui-component's `ThemeConfig`. It
/// exists because one borrowed part paints *ink* rather than a frame:
/// `InputState` lays out the glyphs, the caret and the selection itself, and it
/// resolves their colours from `gpui_base::Theme` on every frame
/// (`InputEditorStyle::resolved`). Nothing wrote that global, so it fell back
/// to `gpui-base`'s own light default — which is invisible-to-illegible on
/// Ravel's dark background.
///
/// The alternative was for [`crate::Input`] to project the style itself, which
/// is what gpui-component does: it calls `set_editor_style` from `render`,
/// every frame, on the state it was handed. That is a state mutation inside
/// `render`, and doing it here instead — once, where the palette actually
/// changes — is both correct and cheaper.
///
/// **Only the names that mean the same thing in both schemas are copied.** The
/// caret and the text selection are deliberately *not* among them: `gpui-base`
/// derives the caret from `foreground` and the selection from `accent` at 40%
/// alpha, with its own documented reason ("a selection must not hide the
/// glyphs it selects"), so feeding it Ravel's `foreground` and `accent` gets
/// Ravel's selection and caret out of Ravel's tokens without this crate
/// inventing two colours the theme schema does not model.
fn project_onto_gpui_base(theme: &RavelTheme, cx: &mut App) {
    let colors = &theme.colors;
    let base = gpui_base::Theme::global_mut(cx);

    base.appearance = match theme.mode {
        crate::tokens::ThemeMode::Dark => gpui_base::ThemeAppearance::Dark,
        crate::tokens::ThemeMode::Light => gpui_base::ThemeAppearance::Light,
    };

    let base = &mut base.tokens.colors;
    base.background = colors.background;
    base.foreground = colors.foreground;
    // `surface` is what an input resolves its own background from, and in
    // Ravel an input sits on the panel's ground — see `input_layers`.
    base.surface = colors.background;
    base.surface_foreground = colors.foreground;
    base.muted_foreground = colors.muted_foreground;
    base.accent = colors.accent;
    base.accent_foreground = colors.readable_on(colors.accent);
    base.primary = colors.primary;
    base.primary_foreground = colors.readable_on(colors.primary);
    base.secondary = colors.secondary;
    base.secondary_foreground = colors.readable_on(colors.secondary);
    base.destructive = colors.danger;
    base.destructive_foreground = colors.readable_on(colors.danger);
    base.border = colors.border;
    base.input = colors.border;
    base.ring = colors.focus_ring();
}

/// Reach the tokens in force.
///
/// Implemented for [`App`], which is what a widget has in `render`; a
/// `Context<V>` reaches it through its deref.
pub trait ActiveTokens {
    /// The token set in force.
    fn tokens(&self) -> &RavelTheme;
}

impl ActiveTokens for App {
    fn tokens(&self) -> &RavelTheme {
        match self.try_global::<ActiveRavelTheme>() {
            Some(active) => &active.0,
            None => built_in(),
        }
    }
}

/// The light built-ins, for a process that never called
/// [`set_active_tokens`].
///
/// A missing global is not an error worth a panic: a test that builds one
/// widget, and a tool that renders a view without going through the
/// application's startup, both have every right to skip it — and a `render`
/// that panics takes the window with it. Falling back to the shipped light
/// palette makes those cases paint the baseline instead.
fn built_in() -> &'static RavelTheme {
    static BUILT_IN: OnceLock<RavelTheme> = OnceLock::new();
    BUILT_IN.get_or_init(|| ThemeSpec::default().resolve())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::{Colors, ThemeMode};

    #[test]
    fn the_fallback_is_the_shipped_light_palette() {
        assert_eq!(built_in().colors, Colors::light());
        assert_eq!(built_in().mode, ThemeMode::Light);
    }

    #[gpui::test]
    fn an_app_without_the_global_still_reads_the_built_ins(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert_eq!(cx.tokens().colors, Colors::light());
        });
    }

    #[gpui::test]
    fn setting_the_global_replaces_what_the_widgets_read(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let dark = ThemeSpec {
                mode: ThemeMode::Dark,
                ..ThemeSpec::default()
            }
            .resolve();
            set_active_tokens(dark, cx);
            assert_eq!(cx.tokens().colors, Colors::dark());

            set_active_tokens(ThemeSpec::default().resolve(), cx);
            assert_eq!(cx.tokens().colors, Colors::light());
        });
    }
}
