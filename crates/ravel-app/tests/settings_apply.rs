// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The settings apply path, end to end
//! (`docs/implementation/settings-screen-plan.md`, `SET-1`; `MED-APP-10`).
//!
//! These live in their own test binary because the i18n store is
//! process-global and switching the active locale would otherwise leak into
//! every other test of the lib binary — the same reason
//! `node_hover_popover.rs` is a separate integration test. The shipped
//! catalogs are loaded, not synthetic ones, so a missing `ja` key would fail
//! here rather than pass against a fixture.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gpui::{AppContext as _, TestAppContext};
use ravel_app::app_settings::{
    self, DEFAULT_LOCALE, LocaleOutcome, SettingsScope, read_global_settings_at,
};
use ravel_app::project::ProjectFile;
use ravel_app::project::settings::SettingsLayer;
use ravel_app::project_state::{ProjectState, disable_background_eval_for_tests};
use ravel_i18n::t;

/// The locale store is process-global and these tests switch it; serialize
/// them the way `ravel-i18n`'s own tests do.
///
/// Poisoning is recovered from rather than propagated: a failing assertion
/// would otherwise turn every later test in this binary into a lock panic and
/// bury the failure that actually mattered.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn locale_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales")
}

/// Write a global settings layer and resolve it exactly as a launch does.
fn start_with(text: &str, dir: &Path) -> LocaleOutcome {
    let path = dir.join("settings.toml");
    std::fs::write(&path, text).unwrap();
    let file = read_global_settings_at(Some(path));
    app_settings::apply_startup_locale(&locale_dir(), &file.resolved().locale)
}

/// The completion criterion for `MED-APP-10`: `locale = "ja"` in the global
/// settings file is all it takes for the UI text to come out Japanese.
#[test]
fn a_global_locale_setting_makes_the_ui_japanese() {
    let _lock = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();

    let outcome = start_with("locale = \"ja\"\n", dir.path());

    assert_eq!(outcome, LocaleOutcome::Applied("ja".to_string()));
    assert_eq!(ravel_i18n::current_locale(), "ja");
    assert_eq!(t!("menu.file.new"), "新規");
    assert_eq!(t!("menu.edit.undo"), "取り消し");
}

/// A locale no catalog provides is a warning and a fallback, never a failed
/// launch: the settings file may say anything.
#[test]
fn an_unknown_locale_falls_back_to_the_default() {
    let _lock = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();

    let outcome = start_with("locale = \"xx-Klingon\"\n", dir.path());

    assert!(
        matches!(outcome, LocaleOutcome::FellBack { ref requested, .. } if requested == "xx-Klingon"),
        "{outcome:?}"
    );
    assert_eq!(ravel_i18n::current_locale(), DEFAULT_LOCALE);
    assert_eq!(t!("menu.file.new"), "New");
}

/// A settings file that is absent or malformed also launches, in English.
#[test]
fn a_broken_settings_file_launches_on_the_default_locale() {
    let _lock = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempfile::tempdir().unwrap();

    for text in ["", "locale = ", "[playback\n", "locale = 7\n"] {
        let outcome = start_with(text, dir.path());
        assert_eq!(
            outcome,
            LocaleOutcome::Applied(DEFAULT_LOCALE.to_string()),
            "{text:?}"
        );
        assert_eq!(t!("menu.file.new"), "New", "{text:?}");
    }

    // No file at all is the ordinary first launch.
    let file = read_global_settings_at(Some(dir.path().join("absent.toml")));
    assert_eq!(file.resolved().locale, DEFAULT_LOCALE);
}

/// The layer direction, observed through behaviour rather than through the
/// resolver: a project that names a locale overrides the user's global choice
/// while it is open, and stops overriding it when a new project replaces it.
#[gpui::test]
fn a_project_locale_overrides_the_global_one_while_it_is_open(cx: &mut TestAppContext) {
    let _lock = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    disable_background_eval_for_tests();
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("settings.toml");
    std::fs::write(&global, "locale = \"en\"\n").unwrap();
    app_settings::apply_startup_locale(&locale_dir(), DEFAULT_LOCALE);

    // A saved project whose settings layer asks for Japanese.
    let project_path = dir.path().join("japanese.ravprj");
    let mut file = ProjectFile::new("japanese", "2026-01-01T00:00:00Z");
    file.settings.locale = Some("ja".into());
    file.save(&project_path).unwrap();

    let project = cx.new(ProjectState::new);
    cx.update(|cx| {
        cx.set_global(ravel_app::project_state::ProjectStateHandle(
            project.downgrade(),
        ));
        app_settings::install(read_global_settings_at(Some(global)), cx);
    });
    assert_eq!(t!("menu.file.new"), "New");

    project.update(cx, |project, cx| {
        project.load_project_from(project_path.clone(), cx)
    });
    cx.run_until_parked();
    assert_eq!(t!("menu.file.new"), "新規", "the project layer applied");
    assert_eq!(
        cx.update(|cx| app_settings::layer(SettingsScope::Project, cx))
            .locale,
        Some("ja".to_string())
    );

    // File ▸ New drops the project layer, so the user's own choice is back.
    project.update(cx, |project, cx| project.new_document(cx));
    cx.run_until_parked();
    assert_eq!(t!("menu.file.new"), "New");
    assert_eq!(
        cx.update(|cx| app_settings::layer(SettingsScope::Project, cx)),
        SettingsLayer::default()
    );
}

/// A project naming a locale the catalogs do not have keeps the language
/// already running, and the resolved settings say so.
///
/// The published locale has to name the one in force, not the one the file
/// asked for: the language control reads it, and a value the UI is not written
/// in would be offered as the current choice.
#[gpui::test]
fn an_unknown_project_locale_keeps_the_running_language(cx: &mut TestAppContext) {
    let _lock = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    disable_background_eval_for_tests();
    let dir = tempfile::tempdir().unwrap();
    let global = dir.path().join("settings.toml");
    std::fs::write(&global, "locale = \"ja\"\n").unwrap();
    app_settings::apply_startup_locale(&locale_dir(), DEFAULT_LOCALE);

    let project_path = dir.path().join("nonsense-locale.ravprj");
    let mut file = ProjectFile::new("nonsense", "2026-01-01T00:00:00Z");
    file.settings.locale = Some("xx".into());
    file.save(&project_path).unwrap();

    let project = cx.new(ProjectState::new);
    cx.update(|cx| {
        cx.set_global(ravel_app::project_state::ProjectStateHandle(
            project.downgrade(),
        ));
        app_settings::install(read_global_settings_at(Some(global)), cx);
    });
    assert_eq!(t!("menu.file.new"), "新規", "the global layer applied");

    project.update(cx, |project, cx| {
        project.load_project_from(project_path.clone(), cx)
    });
    cx.run_until_parked();

    assert_eq!(
        t!("menu.file.new"),
        "新規",
        "an unknown project locale must not change the language"
    );
    assert_eq!(
        cx.update(|cx| app_settings::resolved(cx)).locale,
        "ja",
        "the resolved locale must name the one in force, not the rejected one"
    );
    assert_eq!(
        cx.update(|cx| app_settings::layer(SettingsScope::Project, cx))
            .locale,
        Some("xx".to_string()),
        "the project layer still records what the file said"
    );
}

/// A project-layer edit is written by the next project save and read back on
/// open — the half of "one item update + save" that travels in the `.ravprj`.
#[gpui::test]
fn a_project_layer_edit_survives_a_save_and_open(cx: &mut TestAppContext) {
    let _lock = TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    disable_background_eval_for_tests();
    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().join("demo.ravprj");

    let project = cx.new(ProjectState::new);
    cx.update(|cx| {
        cx.set_global(ravel_app::project_state::ProjectStateHandle(
            project.downgrade(),
        ));
        app_settings::install(read_global_settings_at(None), cx);
        app_settings::update(
            SettingsScope::Project,
            |layer| layer.playback.frame_rate = Some("24".into()),
            cx,
        );
    });
    assert!(
        project.read_with(cx, |project, _| project.is_dirty()),
        "the unsaved settings change is an unsaved change"
    );

    project.update(cx, |project, cx| {
        project.save_project_to(project_path.clone(), None, cx)
    });
    cx.run_until_parked();
    assert!(!project.read_with(cx, |project, _| project.is_dirty()));

    let reopened = ProjectFile::load(&project_path).unwrap();
    assert_eq!(
        reopened.settings.playback.frame_rate.as_deref(),
        Some("24"),
        "the project layer reached the archive"
    );
    // And it applies again on open.
    project.update(cx, |project, cx| {
        project.load_project_from(project_path.clone(), cx)
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| app_settings::resolved(cx)).frame_rate, "24");
}
