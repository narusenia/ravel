// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Appearance settings, observed on the theme rather than on the controls
//! (`docs/implementation/settings-screen-plan.md`, `SET-3`).
//!
//! Every assertion here reads `gpui_component::Theme` — the mode in force, the
//! two theme configs, and a colour out of the palette. That a dropdown exists
//! proves nothing; what the plan asks for is that choosing in it changes what
//! the next frame is painted with.
//!
//! The themes are loaded the way a launch loads them (the shipped
//! `assets/themes/ravel.json`, through `ThemeRegistry`), so a rename in that
//! asset fails here rather than silently degrading to the fallback.

use std::path::{Path, PathBuf};

use gpui::{
    AnyWindowHandle, Context, IntoElement, Pixels, Render, Size, Styled as _, TestAppContext,
    Window, div, px,
};
use gpui_component::setting::AnySettingField as _;
use gpui_component::{Theme, ThemeMode, ThemeRegistry};
use ravel_app::app_settings::{self, SettingsScope, read_global_settings_at};
use ravel_app::settings_dialog::{SettingsPageKind, fields_for};
use ravel_project::settings::{AppearanceMode, DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME};

/// Any window will do; a field's reset only needs one to exist.
const WINDOW_SIZE: Size<Pixels> = Size {
    width: px(400.0),
    height: px(300.0),
};

/// A theme set with two themes of its own, to choose *away* from the bundled
/// ones. Only the name, the mode and one colour matter: the colour is what
/// proves the palette actually changed.
const EXTRA_THEMES: &str = r##"{
  "name": "Test",
  "themes": [
    { "name": "Test Light", "mode": "light", "colors": { "background": "#123456" } },
    { "name": "Test Dark", "mode": "dark", "colors": { "background": "#654321" } }
  ]
}"##;

fn themes_json() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes/ravel.json");
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Bring up the appearance path as a launch does: themes in the registry, then
/// the settings file installed (which applies the appearance).
fn start(settings_toml: &str, dir: &Path, cx: &mut TestAppContext) -> PathBuf {
    let path = dir.join("settings.toml");
    std::fs::write(&path, settings_toml).unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        ThemeRegistry::global_mut(cx)
            .load_themes_from_str(&themes_json())
            .expect("the shipped Ravel theme parses");
        ThemeRegistry::global_mut(cx)
            .load_themes_from_str(EXTRA_THEMES)
            .expect("the test themes parse");
        app_settings::install(read_global_settings_at(Some(path.clone())), cx);
    });
    cx.run_until_parked();
    path
}

fn set_appearance(
    edit: impl FnOnce(&mut ravel_project::settings::AppearanceLayer),
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        app_settings::update(
            SettingsScope::Global,
            |layer| edit(&mut layer.appearance),
            cx,
        )
    });
    cx.run_until_parked();
}

fn mode(cx: &mut TestAppContext) -> ThemeMode {
    cx.update(|cx| Theme::global(cx).mode)
}

fn theme_names(cx: &mut TestAppContext) -> (String, String) {
    cx.update(|cx| {
        let theme = Theme::global(cx);
        (
            theme.light_theme.name.to_string(),
            theme.dark_theme.name.to_string(),
        )
    })
}

/// The colour in force, which is what a repaint would use.
fn background(cx: &mut TestAppContext) -> gpui::Hsla {
    cx.update(|cx| Theme::global(cx).colors.background)
}

/// The completion criterion for the mode control: the palette follows it. Not
/// "the setting was stored" — the colour the next draw uses changes.
#[gpui::test]
fn the_theme_mode_decides_the_palette(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    start("", dir.path(), cx);

    // The default follows the OS, which is the behaviour a user without a
    // settings file has always had.
    let system = ThemeMode::from(cx.update(|cx| cx.window_appearance()));
    assert_eq!(mode(cx), system, "an unset mode follows the system");

    set_appearance(|a| a.theme_mode = Some(AppearanceMode::Dark), cx);
    assert_eq!(mode(cx), ThemeMode::Dark);
    let dark_background = background(cx);

    set_appearance(|a| a.theme_mode = Some(AppearanceMode::Light), cx);
    assert_eq!(mode(cx), ThemeMode::Light);
    assert_ne!(
        background(cx),
        dark_background,
        "the palette in force has to change with the mode, not just the setting"
    );

    // And a mode named in the settings file is in force from the first frame.
    let other = tempfile::tempdir().unwrap();
    start("[appearance]\ntheme_mode = \"dark\"\n", other.path(), cx);
    assert_eq!(
        mode(cx),
        ThemeMode::Dark,
        "the file's mode applies at start"
    );
}

/// Choosing a theme replaces the config of that slot — and only that slot, so
/// the other mode keeps the theme the user picked for it.
#[gpui::test]
fn choosing_a_theme_swaps_the_light_and_dark_slots(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    start("[appearance]\ntheme_mode = \"light\"\n", dir.path(), cx);
    assert_eq!(
        theme_names(cx),
        (
            DEFAULT_LIGHT_THEME.to_string(),
            DEFAULT_DARK_THEME.to_string()
        ),
        "the bundled themes are what an unset choice resolves to"
    );
    let bundled_background = background(cx);

    set_appearance(|a| a.light_theme = Some("Test Light".into()), cx);
    assert_eq!(
        theme_names(cx),
        ("Test Light".to_string(), DEFAULT_DARK_THEME.to_string()),
        "the light slot changed and the dark slot did not"
    );
    assert_ne!(
        background(cx),
        bundled_background,
        "the chosen theme's colours are the ones in force"
    );

    set_appearance(|a| a.dark_theme = Some("Test Dark".into()), cx);
    assert_eq!(
        theme_names(cx),
        ("Test Light".to_string(), "Test Dark".to_string()),
        "both slots are held independently"
    );
    // The dark choice was not in force while the mode is light; switching to it
    // is what puts its palette on screen.
    let light_background = background(cx);
    set_appearance(|a| a.theme_mode = Some(AppearanceMode::Dark), cx);
    assert_ne!(background(cx), light_background);
    // Not just "it changed": the colour is the one `Test Dark` declares, which
    // is what ties the palette to the slot that was chosen rather than to any
    // other theme the mode switch might have reached for.
    assert_eq!(
        background(cx),
        gpui::rgb(0x654321).into(),
        "the dark slot's own palette is what dark mode paints"
    );
}

/// A settings file naming a theme that is not there — renamed, deleted, or a
/// typo — launches on the bundled theme with a warning. Falling back to Ravel's
/// own theme rather than to gpui-component's stock palette keeps the app
/// looking like itself.
#[gpui::test]
fn a_theme_name_no_theme_carries_falls_back(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    start(
        "[appearance]\ntheme_mode = \"light\"\nlight_theme = \"Deleted Theme\"\ndark_theme = \"Also Gone\"\n",
        dir.path(),
        cx,
    );

    assert_eq!(
        theme_names(cx),
        (
            DEFAULT_LIGHT_THEME.to_string(),
            DEFAULT_DARK_THEME.to_string()
        ),
        "an unknown theme name falls back to the bundled theme for its mode"
    );
    // The settings keep naming what the file asked for: the themes directory is
    // read asynchronously, so a theme that arrives later must still be applied
    // rather than forgotten.
    let resolved = cx.update(|cx| app_settings::resolved(cx));
    assert_eq!(resolved.light_theme, "Deleted Theme");
    assert_eq!(
        cx.update(|cx| app_settings::layer(SettingsScope::Global, cx))
            .appearance
            .light_theme
            .as_deref(),
        Some("Deleted Theme")
    );

    // And that is what happens when it does arrive. The themes directory is read
    // asynchronously and re-read on every file change, so this has to follow the
    // *registry* — nothing calls the apply path here, and gpui-component's own
    // observer could not recover it (it re-resolves from the names the `Theme`
    // holds, which after the fallback above are the fallback's).
    cx.update(|cx| {
        ThemeRegistry::global_mut(cx)
            .load_themes_from_str(
                r#"{ "name": "Late", "themes": [ { "name": "Deleted Theme", "mode": "light" } ] }"#,
            )
            .expect("the late theme parses");
    });
    cx.run_until_parked();
    assert_eq!(
        theme_names(cx).0,
        "Deleted Theme",
        "a theme the registry only learns about later still applies"
    );
}

/// A hand-edited file can name a dark theme for the light slot. The dropdowns
/// cannot produce that, but the file can, and honouring it would paint a dark
/// palette every time the user switched to light — so the slot's mode wins and
/// the theme is refused.
#[gpui::test]
fn a_theme_built_for_the_other_mode_is_refused(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    start(
        &format!(
            "[appearance]\ntheme_mode = \"light\"\nlight_theme = \"{DEFAULT_DARK_THEME}\"\ndark_theme = \"Test Light\"\n"
        ),
        dir.path(),
        cx,
    );

    assert_eq!(
        theme_names(cx),
        (
            DEFAULT_LIGHT_THEME.to_string(),
            DEFAULT_DARK_THEME.to_string()
        ),
        "each slot falls back to the bundled theme of its own mode"
    );
}

/// The reset control, exercised through the closures the dialog binds
/// (`on_reset`) rather than through the settings API they call.
///
/// This is the part the plan gives up `default_value` for, so a field wired to
/// the wrong layer or the wrong member has to fail somewhere: `is_dirty` has to
/// answer "this layer holds a value" per field, and `reset` has to remove that
/// one value — leaving the other fields' overrides alone and, crucially,
/// *removing* it rather than writing today's default back as an explicit choice.
#[gpui::test]
fn the_reset_control_clears_one_field_at_a_time(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = start("", dir.path(), cx);
    // A field's reset takes a `Window`, the way it would be handed one by a
    // click; nothing about this view matters beyond its existence.
    let window: AnyWindowHandle = cx.open_window(WINDOW_SIZE, |_window, _cx| Blank).into();

    assert_eq!(
        dirty_fields(cx),
        Vec::<&str>::new(),
        "nothing is customized on a fresh settings file, so no field offers a reset"
    );

    set_appearance(
        |a| {
            a.theme_mode = Some(AppearanceMode::Dark);
            a.light_theme = Some("Test Light".into());
        },
        cx,
    );
    assert_eq!(
        dirty_fields(cx),
        vec![
            "settings.appearance.mode",
            "settings.appearance.light_theme"
        ],
        "is_dirty follows the layer field by field, not the resolved value"
    );

    // Reset the light theme only.
    reset_field(window, "settings.appearance.light_theme", cx);
    let layer = cx.update(|cx| app_settings::layer(SettingsScope::Global, cx));
    assert_eq!(layer.appearance.light_theme, None, "its override is gone");
    assert_eq!(
        layer.appearance.theme_mode,
        Some(AppearanceMode::Dark),
        "and the other field's override is untouched"
    );
    assert_eq!(
        theme_names(cx).0,
        DEFAULT_LIGHT_THEME,
        "the bundled theme is back in force"
    );
    assert_eq!(dirty_fields(cx), vec!["settings.appearance.mode"]);

    // Reset the mode too: the appearance follows the system again.
    reset_field(window, "settings.appearance.mode", cx);
    assert_eq!(
        cx.update(|cx| app_settings::layer(SettingsScope::Global, cx))
            .appearance
            .theme_mode,
        None
    );
    assert_eq!(
        mode(cx),
        ThemeMode::from(cx.update(|cx| cx.window_appearance())),
        "with no override the mode follows the system again"
    );
    assert_eq!(dirty_fields(cx), Vec::<&str>::new());

    // The file records the removal rather than the default value, so a later
    // change to what the default *is* still reaches this user.
    let written = std::fs::read_to_string(&path).unwrap();
    let reread = read_global_settings_at(Some(path)).resolved();
    assert!(
        !written.contains("theme_mode"),
        "the reset value is gone from the file, not written out: {written}"
    );
    assert_eq!(reread.theme_mode, AppearanceMode::System);
    assert_eq!(reread.light_theme, DEFAULT_LIGHT_THEME);
}

/// The title keys of the Appearance fields that currently offer a reset, in page
/// order — read from the fields themselves.
///
/// Two names for one thing, both of them `gpui_component`'s: the closure is
/// handed in as `on_reset`'s `is_dirty` argument and read back through
/// `AnySettingField::is_resettable`. This file says `is_dirty` when it means the
/// closure the dialog supplied and `is_resettable` when it means the call that
/// asks it.
fn dirty_fields(cx: &mut TestAppContext) -> Vec<&'static str> {
    cx.update(|cx| {
        fields_for(SettingsPageKind::Appearance, cx)
            .into_iter()
            .filter(|page_field| page_field.field.is_resettable(cx))
            .map(|page_field| page_field.title_key)
            .collect()
    })
}

/// Invoke the reset the dialog would invoke for the field titled `title_key`.
fn reset_field(window: AnyWindowHandle, title_key: &str, cx: &mut TestAppContext) {
    window
        .update(cx, |_view, window, cx| {
            let page_field = fields_for(SettingsPageKind::Appearance, cx)
                .into_iter()
                .find(|page_field| page_field.title_key == title_key)
                .unwrap_or_else(|| panic!("the Appearance page has no field {title_key:?}"));
            page_field.field.reset(window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// A window has to have a root; this one has nothing else to do.
struct Blank;

impl Render for Blank {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}
