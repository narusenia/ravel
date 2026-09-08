// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bundled application fonts.
//!
//! Ravel ships its own faces so every platform renders the same shell: Geist
//! for the interface, JetBrains Mono for code and monospaced readouts, and
//! Noto Sans JP for Japanese. All three are SIL OFL 1.1 and their license
//! texts travel with the files in `assets/fonts/`.
//!
//! **This module owns the bytes; `ravel_widgets::fonts` owns the families.**
//! The two primary family names are tokens, so building a [`Font`] from them
//! belongs where the widgets can reach it — and the parameter editors that
//! shape their own text now live in that crate. Everything it exposes is
//! re-exported below, so a panel's `crate::fonts::mono_font(cx)` is unchanged.

use std::borrow::Cow;

use gpui::App;

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

pub use ravel_widgets::fonts::{JAPANESE_FALLBACK_FAMILY, japanese_fallbacks, mono_font, ui_font};

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
