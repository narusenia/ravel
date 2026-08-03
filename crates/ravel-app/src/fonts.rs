// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bundled application fonts.
//!
//! Ravel ships its own faces so every platform renders the same shell: Geist
//! for the interface, JetBrains Mono for code and monospaced readouts, and
//! Noto Sans JP for Japanese. All three are SIL OFL 1.1 and their license
//! texts travel with the files in `assets/fonts/`.
//!
//! The two primary family names live in `assets/themes/ravel.json`
//! (`font.family` / `mono_font.family`) so the theme stays the single source
//! of truth. The Japanese fallback cannot be expressed there — a
//! gpui-component theme holds one family name per role — so [`ui_font`] and
//! [`mono_font`] rebuild the theme's family into a [`Font`] carrying
//! [`FontFallbacks`]. The window root is styled with [`ui_font`], which is
//! what makes the fallback reach the whole element tree: `Styled::font_family`
//! (the call gpui-component widgets make) replaces the family without
//! clearing the inherited fallbacks. Code that shapes text itself — the
//! canvas painters in the node editor, timeline, and curve widgets — inherits
//! nothing and has to build its `TextRun` font from these helpers.

use std::borrow::Cow;
use std::sync::LazyLock;

use gpui::{App, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, SharedString};
use gpui_component::ActiveTheme as _;

/// The family Japanese text falls back to, ahead of the platform cascade.
///
/// Not a theme field: it is the second entry of every font Ravel builds, not
/// a role a theme picks.
pub const JAPANESE_FALLBACK_FAMILY: &str = "Noto Sans JP";

/// Faces embedded in the binary, registered once at startup by [`init`].
///
/// Geist carries four weights because the shell asks for `SEMIBOLD` and the
/// platform would otherwise synthesize it. Noto Sans JP carries only Regular
/// and Bold: each weight costs ~4.5 MB, nothing requests a Japanese medium,
/// and leaving Medium out makes a `SEMIBOLD` run resolve to Bold — the closer
/// match to Geist SemiBold on the same line.
const EMBEDDED_FONTS: &[&[u8]] = &[
    include_bytes!("../../../assets/fonts/Geist-Regular.ttf"),
    include_bytes!("../../../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../../../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/Geist-Bold.ttf"),
    include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../../../assets/fonts/JetBrainsMono-Bold.ttf"),
    include_bytes!("../../../assets/fonts/NotoSansJP-Regular.otf"),
    include_bytes!("../../../assets/fonts/NotoSansJP-Bold.otf"),
];

/// Registers the bundled faces with the platform text system.
///
/// Must run before the theme is applied: the theme names families that only
/// exist once they are registered. A failure is logged rather than fatal —
/// the platform then resolves the theme's families itself and the shell falls
/// back to whatever it finds.
pub fn init(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> =
        EMBEDDED_FONTS.iter().copied().map(Cow::Borrowed).collect();
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        tracing::error!(
            %error,
            "failed to register the bundled fonts; the theme's families resolve through the platform instead"
        );
    }
}

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

/// The theme's UI family with the Japanese fallback attached.
pub fn ui_font(cx: &App) -> Font {
    with_japanese_fallback(cx.theme().font_family.clone())
}

/// The theme's monospace family with the Japanese fallback attached.
pub fn mono_font(cx: &App) -> Font {
    with_japanese_fallback(cx.theme().mono_font_family.clone())
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

    /// The embedded bytes have to be real font files: `add_fonts` would only
    /// fail at runtime, long after the build that dropped or truncated one.
    #[test]
    fn every_embedded_font_is_a_font_file() {
        for bytes in EMBEDDED_FONTS {
            assert!(
                bytes.len() > 1024,
                "an embedded font is too small to be a font file"
            );
            // `true`/`\0\u{1}\0\0` for TrueType outlines, `OTTO` for CFF ones.
            let tag = &bytes[..4];
            assert!(
                tag == b"true" || tag == b"OTTO" || tag == [0x00, 0x01, 0x00, 0x00],
                "unexpected sfnt tag {tag:?}"
            );
        }
    }

    /// The license texts are what make redistributing the faces legal, so a
    /// missing one is a release blocker, not a documentation nit.
    #[test]
    fn license_is_vendored_alongside_the_fonts() {
        for name in [
            "LICENSE-Geist.txt",
            "LICENSE-JetBrainsMono.txt",
            "LICENSE-NotoSansJP.txt",
        ] {
            let path =
                std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts"))
                    .join(name);
            assert!(path.exists(), "missing font license: {}", path.display());
        }
    }
}
