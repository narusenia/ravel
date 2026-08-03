// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The language switch (`docs/implementation/settings-screen-plan.md`, `SET-4`).
//!
//! Its own test binary because the i18n store is process-global and these tests
//! move it — the same reason `settings_apply.rs` is separate — and a `TEST_LOCK`
//! inside the binary because gpui tests in one binary share that store.
//!
//! What a language switch has to do is repaint what is already on screen in the
//! new language. Text cannot be read back out of a painted frame (gpui's test
//! API exposes bounds, not text), so the strings are captured one step earlier,
//! at the two places they are produced: a view in a real window records the
//! string it rendered, and the settings dialog's labels are read off the fields
//! it builds per render. The dialog is painted in the same window throughout the
//! switch, so a field that panicked mid-switch would fail here too.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;

use gpui::{
    AnyWindowHandle, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Pixels,
    Render, SharedString, Size, Styled as _, TestAppContext, Window, div, px,
};
use gpui_component::Root;
use gpui_component::setting::{AnySettingField as _, SettingFieldType};
use ravel_app::app_settings::{self, SettingsScope, read_global_settings_at};
use ravel_app::settings_dialog::{
    SettingsDialog, SettingsPageKind, SettingsScope as SettingsScreen, fields_for, label_keys,
};
use ravel_i18n::t;

const WINDOW_SIZE: Size<Pixels> = Size {
    width: px(900.0),
    height: px(700.0),
};

/// Serializes the locale switches in this binary; poisoning is recovered from so
/// one failed assertion does not bury the rest.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn locale_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales")
}

/// A panel-sized view that records the translated string it rendered.
///
/// This is what "the panels change language" means in an automated test: the
/// recorded strings are the ones a reader would have seen, in the order the
/// window painted them.
struct LocaleProbe {
    rendered: Rc<RefCell<Vec<String>>>,
}

impl Render for LocaleProbe {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let text = t!("panel.outliner");
        self.rendered.borrow_mut().push(text.clone());
        div().w_full().child(SharedString::from(text))
    }
}

/// The window root: a panel and the Preferences screen side by side, so a switch
/// is observed while both are on screen.
struct TestRoot {
    probe: Entity<LocaleProbe>,
    dialog: Entity<SettingsDialog>,
}

impl Render for TestRoot {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.probe.clone())
            .child(self.dialog.clone())
    }
}

/// Bring up i18n and the settings global on `settings_toml`, and return the file
/// the global layer is written to.
fn start(settings_toml: &str, dir: &Path, cx: &mut TestAppContext) -> PathBuf {
    let path = dir.join("settings.toml");
    std::fs::write(&path, settings_toml).unwrap();
    app_settings::apply_startup_locale(&locale_dir(), "en");
    cx.update(|cx| {
        gpui_component::init(cx);
        app_settings::install(read_global_settings_at(Some(path.clone())), cx);
    });
    cx.run_until_parked();
    path
}

fn switch_to(locale: &str, cx: &mut TestAppContext) {
    let locale = locale.to_string();
    cx.update(|cx| {
        app_settings::update(
            SettingsScope::Global,
            |layer| layer.locale = Some(locale),
            cx,
        )
    });
    cx.run_until_parked();
}

/// Every label key the Preferences and Project screens render, titles included.
fn every_settings_label_key() -> Vec<&'static str> {
    [
        SettingsScreen::Preferences.title_key(),
        SettingsScreen::Project.title_key(),
    ]
    .into_iter()
    .chain(SettingsPageKind::ALL.iter().map(|page| page.label_key()))
    .chain(
        SettingsPageKind::ALL
            .iter()
            .flat_map(|page| label_keys(*page)),
    )
    .collect()
}

/// The completion criterion: switching the language repaints the panels that are
/// already open, in the new language — without anything writing a setting from
/// inside a `render`.
#[gpui::test]
fn a_language_switch_repaints_the_open_panels(cx: &mut TestAppContext) {
    let _lock = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    start("", dir.path(), cx);

    let rendered = Rc::new(RefCell::new(Vec::new()));
    let for_window = rendered.clone();
    let _window = cx.open_window(WINDOW_SIZE, move |window, cx| {
        let probe = cx.new(|_| LocaleProbe {
            rendered: for_window,
        });
        let dialog = cx.new(|cx| SettingsDialog::new(SettingsScreen::Preferences, cx));
        Root::new(cx.new(|_| TestRoot { probe, dialog }), window, cx)
    });
    cx.run_until_parked();

    assert_eq!(
        rendered.borrow().last().map(String::as_str),
        Some("Outliner"),
        "the window paints the active language to begin with"
    );
    let renders_before = rendered.borrow().len();
    // The dialog's own labels, as the fields it renders carry them.
    assert_eq!(
        theme_mode_option_labels(cx),
        vec!["System", "Light", "Dark"],
        "the settings screen starts out in the active language too"
    );

    switch_to("ja", cx);

    assert!(
        rendered.borrow().len() > renders_before,
        "the switch has to repaint the open window, not wait for the next event"
    );
    assert_eq!(
        rendered.borrow().last().map(String::as_str),
        Some("アウトライナー"),
        "the repaint has to produce the new language"
    );
    // The dialog that holds the language control is on screen throughout, and its
    // own text comes out of the same repaint: the labels its fields carry are
    // rebuilt from `t!` per render, so the switch reaches the control the user
    // just used, not only the panels behind it.
    assert_eq!(
        theme_mode_option_labels(cx),
        vec!["システム", "ライト", "ダーク"],
        "the settings screen's own labels have to follow the switch"
    );
}

/// The labels on the Appearance page's theme-mode dropdown — the dialog's own
/// text, read from the field the dialog renders.
fn theme_mode_option_labels(cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        let fields = fields_for(SettingsPageKind::Appearance, cx);
        let field = fields
            .into_iter()
            .find(|page_field| page_field.title_key == "settings.appearance.mode")
            .expect("the Appearance page has a theme mode field");
        match field.field.field_type() {
            SettingFieldType::Dropdown { options, .. } => options
                .iter()
                .map(|(_value, label)| label.to_string())
                .collect(),
            _ => panic!("the theme mode field has to be a dropdown"),
        }
    })
}

/// A switch is written to the global settings file and is still in force on the
/// next launch — read back through the same reader a launch uses.
#[gpui::test]
fn a_language_switch_survives_a_restart(cx: &mut TestAppContext) {
    let _lock = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = start("", dir.path(), cx);

    switch_to("ja", cx);
    assert_eq!(ravel_i18n::current_locale(), "ja");

    // What the next launch reads.
    let file = read_global_settings_at(Some(path.clone()));
    assert_eq!(file.resolved().locale, "ja");
    app_settings::apply_startup_locale(&locale_dir(), &file.resolved().locale);
    assert_eq!(t!("panel.outliner"), "アウトライナー");

    // And "reset to default" removes the line rather than writing the default
    // into it, so the next launch is back on the fallback locale.
    cx.update(|cx| app_settings::update(SettingsScope::Global, |layer| layer.locale = None, cx));
    cx.run_until_parked();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        !written.contains("locale"),
        "the reset locale is gone from the file: {written}"
    );
    assert_eq!(
        read_global_settings_at(Some(path)).resolved().locale,
        "en",
        "the next launch falls back to the default locale"
    );
    assert_eq!(t!("panel.outliner"), "Outliner");
}

/// The language field's own reset, invoked the way a click invokes it: `is_dirty`
/// reports the override the layer holds, and `reset` removes it — which for the
/// language means the UI goes back to the fallback locale rather than being
/// pinned to it by an explicit value.
#[gpui::test]
fn the_language_reset_control_drops_the_override(cx: &mut TestAppContext) {
    let _lock = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = start("", dir.path(), cx);
    let window: AnyWindowHandle = cx.open_window(WINDOW_SIZE, |_window, _cx| Blank).into();

    assert!(
        !cx.update(|cx| language_field(cx).is_resettable(cx)),
        "an untouched language offers no reset"
    );

    switch_to("ja", cx);
    assert!(
        cx.update(|cx| language_field(cx).is_resettable(cx)),
        "the override the switch wrote is what is_dirty reports"
    );

    window
        .update(cx, |_view, window, cx| {
            language_field(cx).reset(window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();

    assert_eq!(
        cx.update(|cx| app_settings::layer(SettingsScope::Global, cx))
            .locale,
        None,
        "reset removes the value from the layer"
    );
    assert_eq!(
        ravel_i18n::current_locale(),
        "en",
        "and the UI returns to the fallback language"
    );
    assert_eq!(t!("panel.outliner"), "Outliner");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        !written.contains("locale"),
        "the default is not written back as an explicit choice: {written}"
    );
}

/// The Language page's single field.
fn language_field(cx: &gpui::App) -> gpui_component::setting::SettingField<SharedString> {
    fields_for(SettingsPageKind::Language, cx)
        .into_iter()
        .next()
        .expect("the Language page has a field")
        .field
}

/// A window has to have a root; this one has nothing else to do.
struct Blank;

impl Render for Blank {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}

/// The settings screens are labelled in the active language too: the dialog that
/// holds the language control is not exempt from it, which is what makes a switch
/// made inside the dialog legible.
///
/// The labels are produced from these keys on every render (`groups_for`), so a
/// key that resolved to the old language — or to itself, a missing entry — is a
/// dialog stuck in the previous language.
#[gpui::test]
fn the_settings_screens_are_labelled_in_the_active_language(cx: &mut TestAppContext) {
    let _lock = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();
    start("", dir.path(), cx);

    let keys = every_settings_label_key();
    assert!(
        keys.len() > 6,
        "the screens must contribute their field labels, not only their titles"
    );
    let english: Vec<String> = keys.iter().map(|key| t!(key)).collect();

    switch_to("ja", cx);

    for (key, english) in keys.iter().zip(english) {
        let japanese = t!(key);
        assert_ne!(
            japanese, *key,
            "\"{key}\" has no entry in the active catalog"
        );
        assert_ne!(
            japanese, english,
            "\"{key}\" did not follow the language switch"
        );
    }
}
