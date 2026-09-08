// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The fonts the widgets shape text with, built from the tokens.
//!
//! The two primary family names are tokens ([`crate::tokens::Typography`]), so
//! the theme stays the single source of truth. The Japanese fallback cannot be
//! a token — a theme holds one family name per role, not a cascade — so
//! [`ui_font`] and [`mono_font`] rebuild the token's family into a [`Font`]
//! carrying [`FontFallbacks`].
//!
//! **This is the family, not the bytes.** Which faces exist is the host's
//! business: `ravel-app`'s `fonts::init` registers the embedded files and
//! re-exports everything here, so a panel keeps one import path. Naming a
//! family is not depending on a face — the same split `crate::icon::IconPath`
//! makes for SVGs.
//!
//! Code that shapes text itself — the canvas painters in the node editor, the
//! timeline, and the curve widgets — inherits nothing from the element tree and
//! has to build its `TextRun` font from these helpers. Styling the window root
//! with [`ui_font`] is what makes the fallback reach everything that *does*
//! inherit: `Styled::font_family` replaces the family without clearing the
//! inherited fallbacks.

use std::sync::LazyLock;

use gpui::{App, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, SharedString};

use crate::theme::ActiveTokens as _;

/// The family Japanese text falls back to, ahead of the platform cascade.
///
/// Not a token: it is the second entry of every font Ravel builds, not a role
/// a theme picks.
pub const JAPANESE_FALLBACK_FAMILY: &str = "Noto Sans JP";

/// The fallback list, built once.
///
/// `FontFallbacks` holds its families behind an `Arc`, so cloning this is a
/// pointer bump. Building it per call would allocate a `String` and a `Vec`
/// every time — the canvas painters ask for a font per drawn line, which is
/// per node, per port, and per parameter row of every frame.
static JAPANESE_FALLBACKS: LazyLock<FontFallbacks> =
    LazyLock::new(|| FontFallbacks::from_fonts(vec![JAPANESE_FALLBACK_FAMILY.to_owned()]));

/// The fallback list Ravel puts ahead of the platform cascade.
///
/// Applied to the window root so the whole element tree inherits it, and
/// folded into [`ui_font`] / [`mono_font`] for the canvas painters, which
/// inherit nothing.
pub fn japanese_fallbacks() -> FontFallbacks {
    JAPANESE_FALLBACKS.clone()
}

/// The tokens' UI family with the Japanese fallback attached.
pub fn ui_font(cx: &App) -> Font {
    with_japanese_fallback(cx.tokens().text.font_family.clone())
}

/// The tokens' monospace family with the Japanese fallback attached.
pub fn mono_font(cx: &App) -> Font {
    with_japanese_fallback(cx.tokens().text.mono_font_family.clone())
}

fn with_japanese_fallback(family: SharedString) -> Font {
    Font {
        family,
        features: FontFeatures::default(),
        fallbacks: Some(japanese_fallbacks()),
        weight: FontWeight::default(),
        style: FontStyle::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::Colors;

    /// Both helpers take their family from the tokens in force and neither
    /// drops the fallback — a font built without it renders Japanese as tofu on
    /// a canvas painter, which inherits nothing.
    #[gpui::test]
    fn both_fonts_come_from_the_tokens_and_keep_the_fallback(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let _ = Colors::light();
            for font in [ui_font(cx), mono_font(cx)] {
                assert!(font.fallbacks.is_some(), "the Japanese fallback is gone");
            }
            assert_eq!(ui_font(cx).family, cx.tokens().text.font_family);
            assert_eq!(mono_font(cx).family, cx.tokens().text.mono_font_family);
            assert_ne!(ui_font(cx).family, mono_font(cx).family);
        });
    }
}
