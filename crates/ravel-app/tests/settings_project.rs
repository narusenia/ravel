// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Project settings, observed on the compositions they decide
//! (`docs/implementation/settings-screen-plan.md`, `SET-6`; REQ-PROJ-004).
//!
//! The only setting on this screen is the default frame rate, and every
//! assertion here reads a frame rate that ended up on a composition or in a
//! `.ravprj` rather than the state of a control. What the plan asks for is that
//! the setting *decides* something.
//!
//! Two of these are the direction checks the layer model exists for, and they
//! are the reason this file is worth keeping: the project layer has to beat the
//! global one (a preference winning over a project would make REQ-PROJ-004's
//! "project-specific settings override global settings" false), and the reset
//! control has to drop the project layer's value rather than write the default
//! into it.

use gpui::{
    AnyWindowHandle, AppContext as _, Context, IntoElement, Pixels, Render, Size, Styled as _,
    TestAppContext, Window, div, px,
};
use gpui_component::setting::AnySettingField as _;
use ravel_app::app_settings::{self, SettingsScope, read_global_settings_at};
use ravel_app::project_state::{
    ProjectState, ProjectStateHandle, disable_background_eval_for_tests,
};
use ravel_app::settings_dialog::{SettingsPageKind, fields_for};
use ravel_core::types::FrameRate;
use ravel_project::ProjectFile;

/// Any window will do; a field's reset only needs one to exist.
const WINDOW_SIZE: Size<Pixels> = Size {
    width: px(400.0),
    height: px(300.0),
};

/// The i18n key of the row under test — the same handle the dialog builds its
/// `SettingItem` from.
const FRAME_RATE_ROW: &str = "settings.project.frame_rate";

/// A session with a project, and a global settings layer read from `text`.
///
/// The project is created **before** the settings are installed on purpose: its
/// startup document is then at the built-in 30 fps, which is what makes a
/// setting of 24 fps distinguishable from inheritance.
fn start(text: &str, cx: &mut TestAppContext) -> (gpui::Entity<ProjectState>, tempfile::TempDir) {
    disable_background_eval_for_tests();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    std::fs::write(&path, text).unwrap();

    let project = cx.new(ProjectState::new);
    cx.update(|cx| {
        cx.set_global(ProjectStateHandle(project.downgrade()));
        cx.set_global(ravel_app::panels::SelectedPropertiesTarget::default());
        app_settings::install(read_global_settings_at(Some(path)), cx);
    });
    cx.run_until_parked();
    (project, dir)
}

/// Change the project layer the way the Project Settings row does.
fn set_project_frame_rate(rate: &str, cx: &mut TestAppContext) {
    let rate = rate.to_string();
    cx.update(|cx| {
        app_settings::update(
            SettingsScope::Project,
            |layer| layer.playback.frame_rate = Some(rate),
            cx,
        )
    });
    cx.run_until_parked();
}

/// Stop inheriting: with no active composition a new one has nothing to copy,
/// which is the state the default frame rate is for.
fn deactivate_composition(project: &gpui::Entity<ProjectState>, cx: &mut TestAppContext) {
    project.update(cx, |project, cx| project.set_active_composition(None, cx));
}

/// The frame rate `Composition ▸ New…` would open on.
fn new_composition_rate(
    project: &gpui::Entity<ProjectState>,
    cx: &mut TestAppContext,
) -> FrameRate {
    project.read_with(cx, |project, cx| {
        project.new_composition_defaults(cx).frame_rate
    })
}

/// Whether the Project page's frame rate row offers a reset.
fn row_is_resettable(cx: &mut TestAppContext) -> bool {
    cx.update(|cx| {
        fields_for(SettingsPageKind::Project, cx)
            .into_iter()
            .find(|page_field| page_field.title_key == FRAME_RATE_ROW)
            .expect("the Project page has a default frame rate row")
            .field
            .is_resettable(cx)
    })
}

/// Invoke the reset the dialog would invoke for the frame rate row.
fn reset_row(window: AnyWindowHandle, cx: &mut TestAppContext) {
    window
        .update(cx, |_view, window, cx| {
            let page_field = fields_for(SettingsPageKind::Project, cx)
                .into_iter()
                .find(|page_field| page_field.title_key == FRAME_RATE_ROW)
                .expect("the Project page has a default frame rate row");
            page_field.field.reset(window, cx);
        })
        .expect("the window is open");
    cx.run_until_parked();
}

/// The completion criterion, in both directions of the precedence: the setting
/// decides a new composition's frame rate **only** when there is nothing to
/// inherit, and the composition being edited wins while there is.
///
/// Both halves matter. Without the first the setting is unwired; without the
/// second this change would have quietly broken the inheritance
/// `composition_management` relies on.
#[gpui::test]
fn the_default_frame_rate_applies_only_where_nothing_is_inherited(cx: &mut TestAppContext) {
    let (project, _dir) = start("[playback]\nframe_rate = \"24\"\n", cx);
    assert_eq!(
        cx.update(|cx| app_settings::default_frame_rate(cx)),
        FrameRate::new(24, 1),
        "the settings file decides the default"
    );

    // The startup document's composition is active and is at 30 fps.
    assert_eq!(
        new_composition_rate(&project, cx),
        FrameRate::new(30, 1),
        "an active composition's format beats the project-wide default"
    );

    deactivate_composition(&project, cx);
    assert_eq!(
        new_composition_rate(&project, cx),
        FrameRate::new(24, 1),
        "with nothing to inherit, the default frame rate applies"
    );

    // And it follows a later edit rather than being read once.
    set_project_frame_rate("23.976", cx);
    assert_eq!(
        new_composition_rate(&project, cx),
        FrameRate::new(24_000, 1001),
        "the broadcast rate stays exact rather than becoming 23976/1000"
    );
}

/// `File ▸ New` builds its root composition from the setting too: that document
/// has no composition to inherit from by definition.
///
/// The project layer is dropped along with the project it belonged to, so the
/// rate that applies here is the global one — a closing project must not decide
/// the format of the project that replaces it.
#[gpui::test]
fn a_new_document_takes_its_root_composition_from_the_setting(cx: &mut TestAppContext) {
    let (project, _dir) = start("[playback]\nframe_rate = \"25\"\n", cx);
    set_project_frame_rate("50", cx);

    project.update(cx, |project, cx| project.new_document(cx));
    cx.run_until_parked();

    let root = project.read_with(cx, |project, _| {
        let document = project.document();
        document
            .get_composition(document.root_comp.expect("a new document has a root comp"))
            .expect("the root composition exists")
            .frame_rate
    });
    assert_eq!(
        root,
        FrameRate::new(25, 1),
        "the new root composition starts at the global default, not the closed project's"
    );
    assert_eq!(
        cx.update(|cx| app_settings::layer(SettingsScope::Project, cx))
            .playback
            .frame_rate,
        None,
        "the previous project's override stopped applying with it"
    );
}

/// **The regression this unit exists to leave behind.** Resolution runs
/// `default → global → project`, so a project's default frame rate has to beat
/// the preference, not lose to it (REQ-PROJ-004). Reversing the two layers would
/// still pass every "the setting applies" test — only this one fails.
#[gpui::test]
fn the_project_layer_overrides_the_global_default_frame_rate(cx: &mut TestAppContext) {
    let (project, _dir) = start("[playback]\nframe_rate = \"24\"\n", cx);
    deactivate_composition(&project, cx);
    assert_eq!(new_composition_rate(&project, cx), FrameRate::new(24, 1));

    set_project_frame_rate("30", cx);

    assert_eq!(
        cx.update(|cx| app_settings::resolved(cx)).frame_rate,
        "30",
        "the project layer wins the merge"
    );
    assert_eq!(
        new_composition_rate(&project, cx),
        FrameRate::new(30, 1),
        "and it is the rate a new composition is actually built at"
    );
    assert_eq!(
        cx.update(|cx| app_settings::layer(SettingsScope::Global, cx))
            .playback
            .frame_rate
            .as_deref(),
        Some("24"),
        "the preference is untouched: the project overrode it, it did not replace it"
    );

    // The row is bound to the project layer, which is what makes the override
    // possible: a value in the global layer is not this screen's to reset.
    assert!(row_is_resettable(cx));
}

/// "Reset to default" removes the project layer's value, so the global one comes
/// back in force — it does not write the global value into the project as a new
/// override (which is what `default_value()` would do, and why the plan bans it).
#[gpui::test]
fn resetting_the_row_drops_the_project_override_and_the_preference_returns(
    cx: &mut TestAppContext,
) {
    let (project, _dir) = start("[playback]\nframe_rate = \"24\"\n", cx);
    deactivate_composition(&project, cx);
    let window: AnyWindowHandle = cx.open_window(WINDOW_SIZE, |_window, _cx| Blank).into();

    assert!(
        !row_is_resettable(cx),
        "a preference is not an override this screen holds, so there is nothing to reset"
    );

    set_project_frame_rate("30", cx);
    assert!(row_is_resettable(cx));

    reset_row(window, cx);

    assert_eq!(
        cx.update(|cx| app_settings::layer(SettingsScope::Project, cx))
            .playback
            .frame_rate,
        None,
        "the override is gone rather than set to the value below it"
    );
    assert_eq!(
        new_composition_rate(&project, cx),
        FrameRate::new(24, 1),
        "the preference is in force again"
    );
    assert!(!row_is_resettable(cx));
}

/// An unsaved project keeps the change in memory and hands it to the next save;
/// there is no separate write path for it (`SET-1`'s
/// `mark_settings_changed` → the save carries the layer). Reopening the file
/// puts it back in force.
#[gpui::test]
fn a_project_frame_rate_set_before_the_first_save_lands_in_the_ravprj(cx: &mut TestAppContext) {
    let (project, dir) = start("", cx);
    let path = dir.path().join("demo.ravprj");

    set_project_frame_rate("50", cx);
    assert!(
        project.read_with(cx, |project, _| project.is_dirty()),
        "an unsaved settings change is an unsaved change"
    );

    project.update(cx, |project, cx| {
        project.save_project_to(path.clone(), None, cx)
    });
    cx.run_until_parked();
    assert!(!project.read_with(cx, |project, _| project.is_dirty()));

    assert_eq!(
        ProjectFile::load(&path)
            .unwrap()
            .settings
            .playback
            .frame_rate
            .as_deref(),
        Some("50"),
        "the project layer travelled with the archive"
    );

    // Round-trip: opening the file applies it again, and a composition created
    // with nothing active is built at that rate.
    project.update(cx, |project, cx| project.new_document(cx));
    cx.run_until_parked();
    project.update(cx, |project, cx| project.load_project_from(path, cx));
    cx.run_until_parked();
    deactivate_composition(&project, cx);
    assert_eq!(new_composition_rate(&project, cx), FrameRate::new(50, 1));
}

/// A window has to have a root; this one has nothing else to do.
struct Blank;

impl Render for Blank {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}
