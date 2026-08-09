// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Preferences ▸ General, observed on the behaviour the two rows decide
//! (`docs/implementation/settings-screen-plan.md`, `SET-16`; REQ-PROJ-004).
//!
//! The behaviour and the defaults were already wired; what this unit added is
//! the exposure. So every assertion here reads a playhead frame, a document, or
//! the settings file — never the state of a switch. A row that renders and
//! writes nothing would pass a control-shaped test and fail all of these.
//!
//! Both rows write the **global** layer: they are preferences, not project
//! settings. `startup.create_composition` in particular decides what a document
//! being built contains, and a document being built has no project layer to read
//! yet.

use gpui::{
    AnyWindowHandle, AppContext as _, Context, IntoElement, Pixels, Render, Size, Styled as _,
    TestAppContext, Window, div, px,
};
use ravel_app::app_settings::{self, SettingsScope, read_global_settings_at};
use ravel_app::playback::PlaybackController;
use ravel_app::project_state::{
    ProjectState, ProjectStateHandle, disable_background_eval_for_tests,
};
use ravel_app::settings_dialog::{SettingsPageKind, fields_for};
use ravel_ui::command::CommandId;

/// Any window will do; a field's reset only needs one to exist.
const WINDOW_SIZE: Size<Pixels> = Size {
    width: px(400.0),
    height: px(300.0),
};

const STOP_ROW: &str = "settings.general.stop_returns_to_play_start";
const STARTUP_ROW: &str = "settings.general.startup_create_composition";

/// A session with a project and an empty global settings file, plus the path
/// that file lives at — re-reading it is how "survives a relaunch" is checked.
fn start(
    cx: &mut TestAppContext,
) -> (
    gpui::Entity<ProjectState>,
    tempfile::TempDir,
    std::path::PathBuf,
) {
    disable_background_eval_for_tests();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config").join("settings.toml");

    let project = cx.new(ProjectState::new);
    cx.update(|cx| {
        cx.set_global(ProjectStateHandle(project.downgrade()));
        cx.set_global(ravel_app::panels::SelectedPropertiesTarget::default());
        app_settings::install(read_global_settings_at(Some(path.clone())), cx);
    });
    cx.run_until_parked();
    (project, dir, path)
}

/// Set one of the two switches the way its row does.
fn set_stop_returns_to_play_start(value: bool, cx: &mut TestAppContext) {
    cx.update(|cx| {
        app_settings::update(
            SettingsScope::Global,
            |layer| layer.playback.stop_returns_to_play_start = Some(value),
            cx,
        );
    });
    cx.run_until_parked();
}

fn set_startup_creates_composition(value: bool, cx: &mut TestAppContext) {
    cx.update(|cx| {
        app_settings::update(
            SettingsScope::Global,
            |layer| layer.startup.create_composition = Some(value),
            cx,
        );
    });
    cx.run_until_parked();
}

/// Whether the row titled `title_key` offers a reset.
fn row_is_resettable(title_key: &str, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| {
        fields_for(SettingsPageKind::General, cx)
            .into_iter()
            .find(|page_field| page_field.title_key == title_key)
            .unwrap_or_else(|| panic!("the General page has no row {title_key:?}"))
            .field
            .any()
            .is_resettable(cx)
    })
}

/// Invoke the reset the dialog would invoke for that row.
fn reset_row(window: AnyWindowHandle, title_key: &str, cx: &mut TestAppContext) {
    window
        .update(cx, |_view, window, cx| {
            let page_field = fields_for(SettingsPageKind::General, cx)
                .into_iter()
                .find(|page_field| page_field.title_key == title_key)
                .unwrap_or_else(|| panic!("the General page has no row {title_key:?}"));
            page_field.field.any().reset(window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// Play from frame 3, then stop, and report where the playhead landed.
///
/// The controller is driven through `handle_command`, which is where the
/// resolved setting is read — the transport itself takes the flag as an
/// argument and cannot tell whether anything is wired to it.
fn frame_after_playing_from_3_and_stopping(cx: &mut TestAppContext) -> u64 {
    let controller = cx.new(|_| PlaybackController::new());
    controller.update(cx, |controller, cx| {
        for _ in 0..3 {
            controller.handle_command(CommandId::FrameStepForward, cx);
        }
        controller.handle_command(CommandId::PlaybackToggle, cx);
        controller.handle_command(CommandId::PlaybackStop, cx);
    });
    controller.read_with(cx, |controller, _| controller.transport().current_frame())
}

/// **The completion criterion for the playback row**: flipping the setting
/// changes where Stop leaves the playhead. Off rewinds to the first frame, on
/// returns to the frame playback started from.
#[gpui::test]
fn the_stop_setting_decides_where_the_playhead_lands(cx: &mut TestAppContext) {
    let (_project, _dir, _path) = start(cx);

    assert_eq!(
        frame_after_playing_from_3_and_stopping(cx),
        0,
        "the default is what Ravel has always done: Stop rewinds"
    );

    set_stop_returns_to_play_start(true, cx);
    assert_eq!(
        frame_after_playing_from_3_and_stopping(cx),
        3,
        "with the preference on, Stop returns to where playback began"
    );
}

/// **The completion criterion for the startup row**: flipping the setting
/// changes what `File ▸ New` builds.
#[gpui::test]
fn the_startup_setting_decides_whether_a_new_document_has_a_composition(cx: &mut TestAppContext) {
    let (project, _dir, _path) = start(cx);

    project.update(cx, |project, cx| project.new_document(cx));
    cx.run_until_parked();
    assert_eq!(
        project.read_with(cx, |project, _| project.document().compositions.len()),
        1,
        "the default is what Ravel has always done: one empty composition"
    );

    set_startup_creates_composition(false, cx);
    project.update(cx, |project, cx| project.new_document(cx));
    cx.run_until_parked();
    project.read_with(cx, |project, _| {
        let document = project.document();
        assert!(document.compositions.is_empty());
        assert_eq!(document.root_comp, None);
    });
}

/// Both switches write the global layer, so the change is in the file the next
/// launch reads — the criterion is "survives a restart", not "is held in
/// memory".
#[gpui::test]
fn both_switches_survive_a_relaunch(cx: &mut TestAppContext) {
    let (_project, _dir, path) = start(cx);

    set_stop_returns_to_play_start(true, cx);
    set_startup_creates_composition(false, cx);

    let reread = read_global_settings_at(Some(path)).resolved();
    assert!(reread.stop_returns_to_play_start);
    assert!(!reread.startup_creates_composition);
}

/// "Reset to default" removes the value from the layer rather than writing the
/// default back as an explicit choice (which is what `default_value()` would do,
/// and why the plan bans it). A file that no longer mentions the setting is the
/// observable difference.
#[gpui::test]
fn resetting_a_row_drops_the_override_rather_than_writing_the_default(cx: &mut TestAppContext) {
    let (_project, _dir, path) = start(cx);
    let window: AnyWindowHandle = cx.open_window(WINDOW_SIZE, |_window, _cx| Blank).into();

    assert!(
        !row_is_resettable(STOP_ROW, cx) && !row_is_resettable(STARTUP_ROW, cx),
        "an untouched preference is not an override, so there is nothing to reset"
    );

    // Set each to the value the defaults already hold: the layer now carries an
    // explicit value, which is a different state from "not overridden" even
    // though nothing observable changed.
    set_stop_returns_to_play_start(false, cx);
    set_startup_creates_composition(true, cx);
    assert!(row_is_resettable(STOP_ROW, cx) && row_is_resettable(STARTUP_ROW, cx));

    reset_row(window, STOP_ROW, cx);
    reset_row(window, STARTUP_ROW, cx);

    let layer = cx.update(|cx| app_settings::layer(SettingsScope::Global, cx));
    assert_eq!(layer.playback.stop_returns_to_play_start, None);
    assert_eq!(layer.startup.create_composition, None);
    assert!(!row_is_resettable(STOP_ROW, cx) && !row_is_resettable(STARTUP_ROW, cx));

    let text = std::fs::read_to_string(&path).expect("the global layer was written");
    assert!(
        !text.contains("stop_returns_to_play_start") && !text.contains("create_composition"),
        "the reset removed the values instead of writing the defaults back: {text}"
    );
}

/// A window has to have a root; this one has nothing else to do.
struct Blank;

impl Render for Blank {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}
