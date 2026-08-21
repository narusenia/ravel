// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Playback transport dispatch tests
//! (`docs/implementation/done/playback-foundation-plan.md`, unit 2).
//!
//! Transport commands must reach the [`PlaybackController`] through the
//! single command path (GPUI action → workspace dispatch → shell delegate),
//! and controller position changes must drive the Timeline panel's playhead.

use gpui::{AppContext as _, TestAppContext};
use gpui_component::Root;
use ravel_app::panels;
use ravel_app::playback::PlaybackController;
use ravel_app::project_state::{ProjectState, ProjectStateHandle, VIEWER_INPUT_SETTLE};
use ravel_app::trace;
use ravel_app::window_host;
use ravel_app::workspace;
use ravel_core::runtime::playback::PlaybackState;
use ravel_ui::command::CommandId;
use ravel_ui::panel::PanelKind;
use ravel_ui::panels::viewer::ViewerResolution;
use ravel_ui::shell::AppShell;

fn init_i18n() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    let _ = ravel_i18n::init(&dir, "en");
}

fn init_globals(cx: &mut gpui::App) {
    // A live eval worker thread would wake the deterministic test
    // scheduler from outside and fail the run.
    ravel_app::project_state::disable_background_eval_for_tests();
    cx.set_global(panels::FocusedPanelGlobal(None));
    cx.set_global(panels::SelectedPropertiesTarget::default());
    cx.set_global(panels::CanvasSelection::default());
    trace::init(cx);
}

/// The app-wide document state the panels resolve through
/// `ProjectStateHandle`. The caller must keep the returned entity alive for
/// the test — the global only holds a weak handle, exactly like production.
fn init_project_state(cx: &mut TestAppContext) -> gpui::Entity<ProjectState> {
    cx.update(|cx| {
        let project = cx.new(ProjectState::new);
        cx.set_global(ProjectStateHandle(project.downgrade()));
        project
    })
}

/// Builds a real main window with GPU/canvas-heavy panels hidden, mirroring
/// the command-dispatch test harness.
fn open_workspace(cx: &mut TestAppContext) -> gpui::WindowHandle<Root> {
    init_i18n();
    cx.update(|cx| {
        gpui_component::init(cx);
        init_globals(cx);
        workspace::register_action_handlers(cx);
    });

    let mut shell = AppShell::default();
    for panel in [
        PanelKind::NodeGraph,
        PanelKind::Timeline,
        PanelKind::Properties,
    ] {
        if shell.visibility().is_visible(panel) {
            let toggle = match panel {
                PanelKind::NodeGraph => CommandId::ViewToggleNodeGraph,
                PanelKind::Timeline => CommandId::ViewToggleTimeline,
                _ => CommandId::ViewToggleProperties,
            };
            shell.handle_command(toggle);
        }
    }

    cx.update(|cx| {
        cx.bind_keys(workspace::build_keybindings(&shell));
    });

    cx.add_window(move |window, cx| window_host::main_root(shell, window, cx))
}

fn transport_state(cx: &mut TestAppContext) -> (u64, PlaybackState) {
    cx.update(|cx| {
        let session = workspace::session(cx).expect("the session is installed");
        let playback = session.read(cx).playback().read(cx);
        let transport = playback.transport();
        (transport.current_frame(), transport.state())
    })
}

/// Frame steps dispatched as GPUI actions move the transport one frame at a
/// time and leave the clock paused.
#[gpui::test]
fn frame_step_actions_move_the_transport(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    cx.dispatch_action(window.into(), workspace::FrameStepForward);
    cx.dispatch_action(window.into(), workspace::FrameStepForward);
    cx.dispatch_action(window.into(), workspace::FrameStepBackward);

    assert_eq!(transport_state(cx), (1, PlaybackState::Paused));
}

/// Toggle starts playback; stop rewinds to frame 0 and fully stops.
#[gpui::test]
fn toggle_and_stop_actions_drive_the_clock(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    cx.dispatch_action(window.into(), workspace::PlaybackToggle);
    let (_, state) = transport_state(cx);
    assert_eq!(state, PlaybackState::Playing);

    cx.dispatch_action(window.into(), workspace::PlaybackStop);
    assert_eq!(transport_state(cx), (0, PlaybackState::Stopped));
}

/// The default keybindings reach the transport through the same single
/// dispatch path (Space toggles, arrows step).
#[gpui::test]
fn default_chords_dispatch_transport_commands(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    cx.simulate_keystrokes(window.into(), "right right left");
    assert_eq!(transport_state(cx), (1, PlaybackState::Paused));

    cx.simulate_keystrokes(window.into(), "space");
    let (_, state) = transport_state(cx);
    assert_eq!(state, PlaybackState::Playing);

    cx.simulate_keystrokes(window.into(), "k");
    assert_eq!(transport_state(cx), (0, PlaybackState::Stopped));
}

/// Controller position changes drive the live Timeline panel's playhead and
/// adopt the panel composition's frame rate and duration.
#[gpui::test]
fn transport_moves_the_timeline_playhead(cx: &mut TestAppContext) {
    init_i18n();
    cx.update(|cx| {
        gpui_component::init(cx);
        init_globals(cx);
    });

    let _project = init_project_state(cx);
    let timeline = cx.add_window(|window, cx| {
        panels::timeline::TimelineGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
    });
    let controller = cx.update(|cx| cx.new(|_| PlaybackController::new()));

    cx.update(|cx| {
        controller.update(cx, |controller, cx| {
            controller.handle_command(CommandId::FrameStepForward, cx);
            controller.handle_command(CommandId::FrameStepForward, cx);
        });
    });

    let playhead = timeline
        .update(cx, |timeline, _window, _cx| timeline.playhead())
        .unwrap();
    assert_eq!(playhead, 2);

    // The clock adopted the active composition's parameters (30 fps, 300 f).
    cx.update(|cx| {
        let transport = controller.read(cx).transport();
        assert_eq!(transport.fps(), ravel_core::types::FrameRate::new(30, 1));
    });
}

/// Every transport position change records the shared playback position, so
/// selection-driven evaluations use the frame under the playhead
/// (`docs/implementation/done/playback-foundation-plan.md`, unit 3).
#[gpui::test]
fn transport_records_the_shared_playback_position(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    cx.dispatch_action(window.into(), workspace::FrameStepForward);
    cx.dispatch_action(window.into(), workspace::FrameStepForward);

    let position = cx.update(|cx| *cx.global::<panels::PlaybackPosition>());
    assert_eq!(position.frame, 2);
    assert_eq!(position.fps, ravel_core::types::FrameRate::new(30, 1));

    cx.dispatch_action(window.into(), workspace::PlaybackStop);
    let position = cx.update(|cx| *cx.global::<panels::PlaybackPosition>());
    assert_eq!(position.frame, 0);
}

/// A ruler scrub delegates the seek while the Timeline panel is still on the
/// entity update stack; the controller must seek the clock without touching
/// the timeline entity (reading it back panics with "already being updated").
#[gpui::test]
fn seek_from_timeline_updates_the_clock_only(cx: &mut TestAppContext) {
    init_i18n();
    cx.update(|cx| {
        gpui_component::init(cx);
        init_globals(cx);
    });

    let _project = init_project_state(cx);
    let timeline = cx.add_window(|window, cx| {
        panels::timeline::TimelineGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
    });
    let controller = cx.update(|cx| cx.new(|_| PlaybackController::new()));

    // Mirror the production nesting: the seek runs inside the timeline
    // panel's own update, exactly like `scrub_playhead`.
    timeline
        .update(cx, |timeline, _window, cx| {
            let (fps, duration) = timeline
                .composition_params()
                .expect("the active composition");
            controller.update(cx, |controller, cx| {
                controller.seek_from_timeline(42, fps, duration, cx);
            });
        })
        .unwrap();

    cx.update(|cx| {
        let transport = controller.read(cx).transport();
        assert_eq!(transport.current_frame(), 42);
    });
    // The panel's own playhead is untouched by the seek path.
    let playhead = timeline
        .update(cx, |timeline, _window, _cx| timeline.playhead())
        .unwrap();
    assert_eq!(playhead, 0);
}

/// Scrubbing the playhead is an input gesture and must lower the preview
/// factor; the transport's own position publishes must not (`VRES-4`).
///
/// Both routes end in `publish_position`, so the signal has to sit in
/// `seek_from_timeline` alone. Move it one function deeper and the picture
/// degrades for the whole duration of a play — and a frame step would pay for
/// two evaluations instead of one. Neither is visible from the scrub half of
/// this test, which is why both halves are here.
#[gpui::test]
fn a_ruler_scrub_lowers_the_preview_factor_and_a_transport_publish_does_not(
    cx: &mut TestAppContext,
) {
    init_i18n();
    cx.update(|cx| {
        gpui_component::init(cx);
        init_globals(cx);
    });

    let project = init_project_state(cx);
    let timeline = cx.add_window(|window, cx| {
        panels::timeline::TimelineGpuiPanel::new(ravel_ui::layout::PanelInstanceId(0), window, cx)
    });
    let controller = cx.update(|cx| cx.new(|_| PlaybackController::new()));

    // `Full` is the only selection where a one-step drop is observable.
    cx.update(|cx| {
        project.update(cx, |project, cx| {
            project.set_viewer_resolution(ViewerResolution::Full, cx);
        });
    });

    // Same nesting as production: the scrub runs inside the timeline panel's
    // own update.
    timeline
        .update(cx, |timeline, _window, cx| {
            let (fps, duration) = timeline
                .composition_params()
                .expect("the active composition");
            controller.update(cx, |controller, cx| {
                controller.seek_from_timeline(42, fps, duration, cx);
            });
        })
        .unwrap();

    cx.update(|cx| {
        let project = project.read(cx);
        // The selection is untouched; only what the viewer evaluates at moved.
        assert_eq!(project.viewer_resolution(), ViewerResolution::Full);
        assert_eq!(
            project.effective_viewer_resolution(),
            ViewerResolution::Half,
            "a ruler scrub did not lower the preview factor"
        );
    });

    cx.executor().advance_clock(VIEWER_INPUT_SETTLE * 2);

    let after_scrub = cx.update(|cx| {
        let project = project.read(cx);
        assert_eq!(
            project.effective_viewer_resolution(),
            ViewerResolution::Full,
            "the factor never came back after the scrub"
        );
        project.viewer_eval_requests()
    });

    // A frame step is the whole of `publish` → `publish_position`, which is
    // also the only route the playback tick loop reaches evaluation by. It
    // must evaluate at the selected factor and arm no settle timer.
    cx.update(|cx| {
        controller.update(cx, |controller, cx| {
            controller.handle_command(CommandId::FrameStepForward, cx);
        });
    });
    cx.executor().advance_clock(VIEWER_INPUT_SETTLE * 2);

    cx.update(|cx| {
        let project = project.read(cx);
        assert_eq!(
            project.effective_viewer_resolution(),
            ViewerResolution::Full,
            "a transport publish lowered the preview factor, so playback would \
             run the whole way at a degraded resolution"
        );
        assert_eq!(
            project.viewer_eval_requests(),
            after_scrub + 1,
            "the step's own evaluation only"
        );
    });
}
