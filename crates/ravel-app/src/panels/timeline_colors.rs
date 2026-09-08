// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The Timeline's functional colours: the curve ramp and the cache band.
//!
//! **Neither is a palette step** (UX invariant 12's named exception). The ramp
//! exists to tell several curves apart on one graph, and the cache band exists
//! to say which frames are already rendered — jobs a theme colour cannot do,
//! because both are read as *values* rather than as chrome.
//!
//! They used to come from gpui-component's `chart_1..5` and `success`, which is
//! why they are here: Ravel's schema models neither, and the borrowed theme is
//! no longer where the panels read their colours from.

use gpui::{Hsla, hsla};
use ravel_widgets::tokens::Colors;

/// The curve ramp's hue and saturation.
///
/// One hue, five lightnesses — the shape gpui-component's `chart_1..5`
/// fallbacks had (`base.blue` lightened and darkened in 20% steps), kept
/// because it is the right shape: several curves on one graph are compared by
/// *position*, and five different hues would make the eye compare colours
/// instead. This is `#5B6EE1` — the shipped `base.blue`, which is also
/// `primary` — expressed in HSL so the steps can be written down.
const CURVE_HUE: f32 = 0.643;
const CURVE_SATURATION: f32 = 0.690;

/// The five lightnesses, brightest first: `0.620` (the base) scaled by
/// 1.4 / 1.2 / 1.0 / 0.8 / 0.6.
const CURVE_LIGHTNESS: [f32; RAMP_STEPS] = [0.867, 0.744, 0.620, 0.496, 0.372];

/// How many steps the ramp has before it repeats.
pub const RAMP_STEPS: usize = 5;

/// Step `step` of the curve ramp, wrapping after [`RAMP_STEPS`].
///
/// Also what the ruler's own marks are drawn from — the loop range takes step 0
/// and the beat grid step 1 — because those need a colour that is *not*
/// mistakable for the playhead (`primary`) or for a layer bar (`accent`), which
/// is the same requirement the curves have.
pub fn ramp(step: usize) -> Hsla {
    hsla(
        CURVE_HUE,
        CURVE_SATURATION,
        CURVE_LIGHTNESS[step % RAMP_STEPS],
        1.0,
    )
}

/// The bright green the theme file shipped for the dark palette (`#30D158`).
const CACHE_BAND_BRIGHT: Hsla = hsla(0.375, 0.636, 0.504, 1.0);
/// The dark green it shipped for the light palette (`#319a00`).
const CACHE_BAND_DARK: Hsla = hsla(0.281, 1.0, 0.302, 1.0);

/// The cache band: green, because "this frame is already rendered" is the one
/// piece of good news the ruler reports.
///
/// **Two greens, chosen by contrast rather than by mode.** The band is a solid
/// fill that only has to be *seen*, and a single green cannot do that in both
/// palettes: the bright one measures about 1.5:1 on a light ruler where the
/// dark one measures about 2.8:1, and the ratio inverts on a dark one. The
/// theme file shipped both for exactly this reason.
///
/// Picking by [`Colors::more_visible_on`] rather than by `ThemeMode` keeps the
/// module free of a mode branch, and means a theme whose `background` does not
/// match its declared mode still gets the visible green.
pub fn cache_band(colors: &Colors) -> Hsla {
    Colors::more_visible_on(colors.background, CACHE_BAND_BRIGHT, CACHE_BAND_DARK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ramp_walks_five_distinct_steps_and_then_repeats() {
        let steps: Vec<Hsla> = (0..RAMP_STEPS).map(ramp).collect();
        for (i, a) in steps.iter().enumerate() {
            for b in steps.iter().skip(i + 1) {
                assert_ne!(a, b, "two curves on one graph would share a colour");
            }
        }
        // Monotone, so a reader can order the curves by weight rather than
        // having to learn five arbitrary colours.
        assert!(
            steps.windows(2).all(|w| w[0].lightness > w[1].lightness),
            "the ramp has to descend"
        );
        assert_eq!(ramp(RAMP_STEPS), ramp(0), "the ramp wraps");
    }

    #[test]
    fn the_cache_band_is_green_and_not_the_ramp() {
        // Green as painted, not as written: a band that drifted blue would read
        // as a ruler mark, and one that drifted yellow as a warning.
        for colors in [Colors::light(), Colors::dark()] {
            let band = cache_band(&colors);
            let rgba = gpui::hsla_to_rgba(band);
            assert!(rgba.color.green > rgba.color.red);
            assert!(rgba.color.green > rgba.color.blue);
            assert!((0..RAMP_STEPS).all(|step| ramp(step) != band));
        }

        // And the two palettes do not get the same green: one green cannot be
        // seen on both grounds, which is why the theme file shipped a pair.
        assert_ne!(
            cache_band(&Colors::light()),
            cache_band(&Colors::dark()),
            "a single green would vanish on one of the two rulers"
        );
    }
}
