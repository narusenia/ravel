// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The startup splash window.
//!
//! The splash exists because the work between the process starting and the
//! main window appearing is not instantaneous: themes, settings, keybindings,
//! and the saved workspace all come off disk first. It covers that gap and
//! says which of those steps is running.
//!
//! Almost nothing here is drawn. `assets/splash/splash@2x.png` is a finished
//! brand asset — logo lockup, tagline, and copyright line are all baked into
//! the artwork at 2× — and the application contributes exactly two lines of
//! text over it: the progress line ([`StartupStage`]) and the version line
//! ([`version_line`]). Anything else that should appear on the splash belongs
//! in the artwork, not in this file.
//!
//! The window deliberately owns no session state and observes nothing. It is
//! opened before the settings are installed and dismissed as soon as the main
//! window exists, so there is nothing for it to react to.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, AssetSource as _, Bounds, Context, Entity, IntoElement,
    ParentElement as _, Pixels, Render, RenderImage, SharedString, Size, Styled as _, Window,
    WindowBounds, WindowHandle, WindowOptions, div, img, px, rgb,
};
use gpui_component::Root;
use ravel_i18n::t;
use smallvec::SmallVec;

/// The splash window's logical size.
///
/// The artwork is a 1680×1092 PNG — exactly 2× this — so it lands pixel-exact
/// on a Retina display and scales down cleanly on a 1× one.
pub const SPLASH_SIZE: Size<Pixels> = Size {
    width: px(840.0),
    height: px(546.0),
};

/// The embedded artwork, resolved through [`crate::assets::RavelAssets`].
const SPLASH_ARTWORK: &str = "splash/splash@2x.png";

/// Left margin of both text lines, matching the artwork's own left margin
/// (48 px in the 2× PNG).
const TEXT_LEFT: Pixels = px(24.0);

/// Top of the progress line's text box.
///
/// The logo lockup in the artwork ends at y=308.5 logical (its tagline runs
/// 300–308.5), so the progress line sits just below it with a small gap
/// rather than at the 305 the design brief names — 305 would have put the
/// glyphs through the tagline.
const PROGRESS_TOP: Pixels = px(322.0);

/// Distance from the window's bottom edge to the bottom of the version line's
/// text box.
///
/// The baked-in copyright line occupies 25–35.5 logical px from the bottom, so
/// the version line stacks directly above it and the two read as one footer
/// block.
const VERSION_BOTTOM: Pixels = px(40.0);

/// Size of both text lines, matching the artwork's copyright line (a 16 px cap
/// height in the 2× PNG).
const TEXT_SIZE: Pixels = px(11.0);

/// The artwork's dark ink, used for the progress line.
const INK: u32 = 0x2e2e2e;

/// The artwork's secondary grey, used for the version line so it matches the
/// copyright line it sits on top of.
const INK_MUTED: u32 = 0x737373;

/// The family the splash names directly.
///
/// Every other surface takes its family from the theme
/// (`cx.theme().font_family`), but the splash is painted before
/// `load_ravel_themes` has run — covering exactly that work is the splash's
/// job — so there is no Ravel theme to ask yet. The splash is a fixed brand
/// asset rather than a themed surface, so this states what the artwork was set
/// in; it does not fork the theme's authority over the shell's font.
const SPLASH_FONT_FAMILY: &str = "Geist";

/// How long each stage's label stays up at minimum.
///
/// Two things make this a number rather than zero. Without any pause the
/// stages finish inside a single frame and the progress line is never painted
/// at all — awaiting a timer is what returns the main thread to the platform
/// so the frame can be drawn. And with only a frame's worth of pause the
/// labels are a flicker: on this machine the four stages' real work totals
/// well under a second, so a label that is up for 16 ms cannot be read.
///
/// A legibility floor, not a fake progress bar: a stage that takes longer than
/// this simply takes longer and its label is up for the whole of it. Four
/// stages puts the floor on the whole splash at about a second. Tune it here
/// if the artwork or the stage list changes; nothing else reads it.
const STAGE_DWELL: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------
// Startup stages
// ---------------------------------------------------------------------------

/// A step of startup the splash names while it runs.
///
/// The order is the order startup performs them in — [`ALL`] is what the
/// bootstrap loop iterates, so this list *is* the sequence rather than a
/// description of one.
///
/// Only the disk-bound steps that happen *after* the splash is up are here.
/// The locale and the font registration are not stages: without them there is
/// no translated label to show and no face to draw it with, so they run before
/// the window opens.
///
/// [`ALL`]: StartupStage::ALL
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupStage {
    /// `load_ravel_themes`: fills the theme registry from `assets/themes`.
    Themes,
    /// `app_settings::install`: publishes the resolved settings and puts the
    /// appearance in force.
    Settings,
    /// `keybindings::read_keybindings` and `workspace::build_keybindings`: the
    /// user's overrides laid over the bundled defaults.
    Keybindings,
    /// `layout_persist::install` and `restore_into`: the previous session's
    /// workspace arrangement.
    Layout,
}

impl StartupStage {
    /// Every stage, in the order startup runs them.
    pub const ALL: [StartupStage; 4] = [
        Self::Themes,
        Self::Settings,
        Self::Keybindings,
        Self::Layout,
    ];

    /// The locale key of this stage's progress label.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Themes => "splash.themes",
            Self::Settings => "splash.settings",
            Self::Keybindings => "splash.keybindings",
            Self::Layout => "splash.layout",
        }
    }

    /// This stage's progress label in the active locale.
    pub fn label(self) -> String {
        t!(self.label_key())
    }
}

/// The version line the splash draws, above the artwork's copyright line.
///
/// Built from the crate version so a release bump moves it on its own; the
/// design mock's placeholder string is not repeated here.
pub fn version_line() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

// ---------------------------------------------------------------------------
// Handing off to the main window
// ---------------------------------------------------------------------------

/// Opens the main window and only then dismisses the splash. Reports whether
/// the main window opened.
///
/// All three steps live in one function because their order is load-bearing
/// and nothing at the call site makes that visible. Ravel runs with
/// [`gpui::QuitMode::LastWindowClosed`], so the process ends the instant no
/// window is open: dismissing the splash while it is still the only window
/// quits the application in the middle of startup. Keeping the sequence here
/// means no caller can get it wrong, and the callbacks make the order itself
/// observable to a test.
///
/// `open_main` reports whether the platform gave us a window. When it did not
/// there is nothing left to run, so the splash is dismissed and `quit` is
/// called — the same path the synchronous bootstrap took before the splash
/// existed.
///
/// Generic over the context so the order can be tested against a recorder
/// rather than a live [`App`]; the caller threads `&mut App` through it.
pub fn hand_off_to_main<C>(
    cx: &mut C,
    open_main: impl FnOnce(&mut C) -> bool,
    dismiss_splash: impl FnOnce(&mut C),
    quit: impl FnOnce(&mut C),
) -> bool {
    let opened = open_main(cx);
    dismiss_splash(cx);
    if !opened {
        quit(cx);
    }
    opened
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// The splash view: the artwork plus the two lines the application owns.
pub struct SplashScreen {
    stage: StartupStage,
    version: SharedString,
    /// Decoded up front rather than named as a path. `img("<path>")` resolves
    /// through GPUI's *asynchronous* image cache, so the artwork lands a frame
    /// or more after the window does — at this window's roughly one-second
    /// lifetime that is a visible white card at launch, which is the one thing
    /// a brand splash must not be. `None` if the artwork could not be decoded;
    /// the text still draws on white.
    artwork: Option<Arc<RenderImage>>,
}

impl SplashScreen {
    fn new() -> Self {
        Self {
            stage: StartupStage::ALL[0],
            version: version_line().into(),
            artwork: decode_artwork(),
        }
    }
}

/// Decodes the embedded artwork into the straight-alpha BGRA [`RenderImage`]
/// the `img` element consumes.
///
/// The bytes come from the one asset source rather than a second
/// `include_bytes!`, so the PNG is in the binary once and `assets.rs` stays
/// the only place that says what is embedded. The conversion is the same one
/// the MediaBin thumbnails use.
fn decode_artwork() -> Option<Arc<RenderImage>> {
    let bytes = match crate::assets::RavelAssets.load(SPLASH_ARTWORK) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            tracing::warn!(path = SPLASH_ARTWORK, "the splash artwork is not embedded");
            return None;
        }
        Err(error) => {
            tracing::warn!(%error, path = SPLASH_ARTWORK, "could not read the splash artwork");
            return None;
        }
    };
    let mut pixels = match image::load_from_memory(&bytes) {
        Ok(image) => image.into_rgba8(),
        Err(error) => {
            tracing::warn!(%error, path = SPLASH_ARTWORK, "the splash artwork is not a readable image");
            return None;
        }
    };
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(SmallVec::from_elem(
        image::Frame::new(pixels),
        1,
    ))))
}

impl Render for SplashScreen {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            // The artwork is opaque white; painting the same white underneath
            // keeps the window from flashing while the image decodes.
            .bg(gpui::white())
            .font_family(SPLASH_FONT_FAMILY)
            .text_size(TEXT_SIZE)
            .when_some(self.artwork.clone(), |this, artwork| {
                this.child(img(artwork).absolute().inset_0().size_full())
            })
            .child(
                div()
                    .absolute()
                    .left(TEXT_LEFT)
                    .top(PROGRESS_TOP)
                    .text_color(rgb(INK))
                    .child(SharedString::from(self.stage.label())),
            )
            .child(
                div()
                    .absolute()
                    .left(TEXT_LEFT)
                    .bottom(VERSION_BOTTOM)
                    .text_color(rgb(INK_MUTED))
                    .child(self.version.clone()),
            )
    }
}

/// A live splash window.
pub struct Splash {
    window: WindowHandle<Root>,
    screen: Entity<SplashScreen>,
}

impl Splash {
    /// Puts `stage`'s label on the progress line.
    ///
    /// Only marks the view dirty; the frame it lands on is painted once the
    /// caller yields the main thread (see [`stage_break`]).
    pub fn show_stage(&self, stage: StartupStage, cx: &mut App) {
        self.screen.update(cx, |screen, cx| {
            screen.stage = stage;
            cx.notify();
        });
    }

    /// Closes the splash window.
    ///
    /// Consumes the handle: a splash that has been dismissed must not be
    /// reachable, because the next window count of zero quits the application.
    pub fn dismiss(self, cx: &mut App) {
        if let Err(error) = self
            .window
            .update(cx, |_root, window, _cx| window.remove_window())
        {
            tracing::warn!(%error, "the splash window was already gone");
        }
    }
}

/// Opens the splash window, centered, without a title bar.
///
/// Returns `None` when the platform refuses the window. That is not fatal and
/// must not abort the launch: the splash is a progress report, and startup has
/// to reach the main window with or without one.
///
/// Must be called after `gpui_component::init` and `fonts::init` (there is
/// otherwise no theme for [`Root`] to read and no bundled face to draw with),
/// and after the locale is active (the progress labels come from the catalog).
pub fn open(cx: &mut App) -> Option<Splash> {
    let bounds = Bounds::centered(None, SPLASH_SIZE, cx);
    let mut screen = None;
    let window = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            // No title bar at all. On macOS `titlebar: None` also drops the
            // traffic lights and the resize mask, which is what a splash
            // wants: the artwork is a fixed-size composition and there is no
            // window management to offer for a window that lives for a few
            // hundred milliseconds.
            titlebar: None,
            is_resizable: false,
            is_minimizable: false,
            is_movable: false,
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                let view = cx.new(|_| SplashScreen::new());
                screen = Some(view.clone());
                Root::new(view, window, cx)
            })
        },
    );
    match (window, screen) {
        (Ok(window), Some(screen)) => Some(Splash { window, screen }),
        (Ok(_), None) => {
            // Unreachable: the builder above always assigns `screen`.
            tracing::error!("the splash window opened without a view");
            None
        }
        (Err(error), _) => {
            tracing::warn!(%error, "could not open the splash window; starting without it");
            None
        }
    }
}

/// Yields the main thread long enough for the frame carrying the new progress
/// label to be painted.
///
/// Awaiting a *background* timer is what makes this work: it returns control
/// to the platform's event loop, which then services the redraw
/// [`Splash::show_stage`] asked for. Without a yield every stage would run
/// inside the one update that opened the window, and the progress line would
/// go from empty to gone without ever being drawn.
pub async fn stage_break(cx: &gpui::AsyncApp) {
    cx.background_executor().timer(STAGE_DWELL).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bootstrap loop iterates `ALL`, so this order is the startup order.
    /// Themes before settings is the one hard dependency: the appearance
    /// `app_settings::install` puts in force names a theme that has to be in
    /// the registry already.
    #[test]
    fn stages_run_in_the_documented_order() {
        assert_eq!(
            StartupStage::ALL,
            [
                StartupStage::Themes,
                StartupStage::Settings,
                StartupStage::Keybindings,
                StartupStage::Layout,
            ]
        );
    }

    /// Every stage carries its own label. A copy-pasted key would show the
    /// wrong step's name for the whole of a stage and nothing else would
    /// notice.
    #[test]
    fn every_stage_has_a_distinct_label_key() {
        let mut keys: Vec<&str> = StartupStage::ALL.iter().map(|s| s.label_key()).collect();
        assert_eq!(keys.len(), StartupStage::ALL.len());
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(
            keys.len(),
            StartupStage::ALL.len(),
            "two stages share a label key"
        );
        for key in keys {
            assert!(
                key.starts_with("splash."),
                "{key} is not in the splash section"
            );
        }
    }

    /// Every shipped catalog has to carry every stage's label. `t!` falls
    /// back to English, so a missing Japanese key is invisible both in the
    /// running application and to any assertion on `label()` — only the
    /// catalog files themselves can report it.
    #[test]
    fn every_catalog_carries_every_stage_label() {
        let dir =
            std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/locales"));
        let catalogs: Vec<(String, toml::Table)> = std::fs::read_dir(dir)
            .expect("the locale directory is shipped")
            .map(|entry| entry.expect("readable locale directory").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .map(|path| {
                let locale = path
                    .file_stem()
                    .expect("a .toml file has a stem")
                    .to_string_lossy()
                    .into_owned();
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{} not readable: {e}", path.display()));
                let table = text
                    .parse::<toml::Table>()
                    .unwrap_or_else(|e| panic!("{} is invalid TOML: {e}", path.display()));
                (locale, table)
            })
            .collect();
        assert!(catalogs.len() >= 2, "the shipped catalogs went missing");

        for (locale, catalog) in catalogs {
            for stage in StartupStage::ALL {
                let leaf = stage.label_key().trim_start_matches("splash.");
                let label = catalog
                    .get("splash")
                    .and_then(|section| section.get(leaf))
                    .and_then(toml::Value::as_str);
                assert!(
                    label.is_some_and(|s| !s.trim().is_empty()),
                    "{locale}.toml has no splash.{leaf}"
                );
            }
        }
    }

    /// The version line comes from the crate version, so it moves with a
    /// release bump. The design mock carried a placeholder ("v0.0.0-pr-alpha")
    /// that must never end up hardcoded here.
    #[test]
    fn version_line_is_the_crate_version() {
        let line = version_line();
        assert_eq!(line, format!("v{}", env!("CARGO_PKG_VERSION")));
        assert!(line.starts_with('v'), "{line} has no v prefix");
        let number = &line[1..];
        assert!(
            number.split('.').count() >= 3,
            "{line} is not a semantic version"
        );
        assert!(
            number.starts_with(|c: char| c.is_ascii_digit()),
            "{line} does not start with a version number"
        );
    }

    /// The invariant `QuitMode::LastWindowClosed` imposes: while the splash is
    /// the only open window, closing it ends the process. Swap the two calls
    /// in `hand_off_to_main` and this test reports the launch-then-quit bug.
    #[test]
    fn the_main_window_opens_before_the_splash_is_dismissed() {
        let mut log: Vec<&str> = Vec::new();
        let opened = hand_off_to_main(
            &mut log,
            |log| {
                log.push("open_main");
                true
            },
            |log| log.push("dismiss_splash"),
            |log| log.push("quit"),
        );
        assert!(opened);
        assert_eq!(log, ["open_main", "dismiss_splash"]);
    }

    /// A refused main window still dismisses the splash — leaving it up would
    /// show brand artwork with no application behind it — and still quits,
    /// which is what the synchronous bootstrap did before the splash existed.
    #[test]
    fn a_refused_main_window_dismisses_the_splash_and_quits() {
        let mut log: Vec<&str> = Vec::new();
        let opened = hand_off_to_main(
            &mut log,
            |log| {
                log.push("open_main");
                false
            },
            |log| log.push("dismiss_splash"),
            |log| log.push("quit"),
        );
        assert!(!opened);
        assert_eq!(log, ["open_main", "dismiss_splash", "quit"]);
    }

    /// The artwork is authored at exactly 2× the window, so it lands
    /// pixel-exact on a Retina display. A window size that stops being half
    /// the PNG's would resample the whole brand asset.
    #[test]
    fn the_window_is_half_the_artwork() {
        let bytes = crate::assets::RavelAssets
            .load(SPLASH_ARTWORK)
            .expect("the asset source is readable")
            .expect("the splash artwork is embedded");
        // PNG IHDR: 8-byte signature, 4-byte length, 4-byte type, then the
        // big-endian width and height.
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        assert_eq!(width as f32, f32::from(SPLASH_SIZE.width) * 2.0);
        assert_eq!(height as f32, f32::from(SPLASH_SIZE.height) * 2.0);
    }

    /// Both text lines have to fall inside the window, clear of the artwork's
    /// own type. The logo lockup ends at y=308.5 and the baked-in copyright
    /// line starts 35.5 px up from the bottom.
    #[test]
    fn the_text_lines_clear_the_artwork() {
        let progress_top = f32::from(PROGRESS_TOP);
        let logo_lockup_bottom = 308.5;
        assert!(
            progress_top > logo_lockup_bottom,
            "the progress line runs through the logo lockup"
        );

        let version_bottom_from_top = f32::from(SPLASH_SIZE.height) - f32::from(VERSION_BOTTOM);
        assert!(
            version_bottom_from_top > progress_top + f32::from(TEXT_SIZE),
            "the version line overlaps the progress line"
        );
        let copyright_top_from_top = f32::from(SPLASH_SIZE.height) - 35.5;
        assert!(
            version_bottom_from_top <= copyright_top_from_top,
            "the version line runs through the baked-in copyright line"
        );
    }
}
