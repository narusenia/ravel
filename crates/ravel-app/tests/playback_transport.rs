// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Playback transport dispatch tests
//! (`docs/implementation/playback-foundation-plan.md`, unit 2).
//!
//! Transport commands must reach the [`PlaybackController`] through the
//! single command path (GPUI action → workspace dispatch → shell delegate),
//! and controller position changes must drive the Timeline panel's playhead.

use gpui::{AppContext as _, TestAppContext};
use ravel_app::panels;
use ravel_app::playback::PlaybackController;
use ravel_app::trace;
use ravel_app::workspace::{self, MainWorkspace, RavelWorkspace};
use ravel_core::runtime::playback::PlaybackState;
use ravel_ui::command::CommandId;
use ravel_ui::panel::PanelKind;
use ravel_ui::shell::AppShell;

fn init_i18n() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    let _ = ravel_i18n::init(&dir, "en");
}

fn init_globals(cx: &mut gpui::App) {
    // A live eval worker thread would wake the deterministic test
    // scheduler from outside and fail the run.
    panels::node_editor::disable_background_eval_for_tests();
    cx.set_global(panels::FocusedPanelGlobal(None));
    cx.set_global(panels::SelectedPropertiesTarget::default());
    cx.set_global(workspace::DetachedWindowHandles(Default::default()));
    trace::init(cx);
}

/// Builds a real `RavelWorkspace` window with GPU/canvas-heavy panels hidden,
/// mirroring the command-dispatch test harness.
fn open_workspace(cx: &mut TestAppContext) -> gpui::WindowHandle<RavelWorkspace> {
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

    let window = cx.add_window(move |window, cx| RavelWorkspace::new(shell, window, cx));
    cx.update(|cx| {
        let workspace = window
            .entity(cx)
            .expect("workspace window should have a root entity");
        cx.set_global(MainWorkspace::new(window.into(), workspace.downgrade()));
    });
    window
}

fn transport_state(
    window: gpui::WindowHandle<RavelWorkspace>,
    cx: &mut TestAppContext,
) -> (u64, PlaybackState) {
    cx.update(|cx| {
        let playback = window
            .entity(cx)
            .expect("workspace window should have a root entity")
            .read(cx)
            .playback()
            .read(cx);
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

    assert_eq!(transport_state(window, cx), (1, PlaybackState::Paused));
}

/// Toggle starts playback; stop rewinds to frame 0 and fully stops.
#[gpui::test]
fn toggle_and_stop_actions_drive_the_clock(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    cx.dispatch_action(window.into(), workspace::PlaybackToggle);
    let (_, state) = transport_state(window, cx);
    assert_eq!(state, PlaybackState::Playing);

    cx.dispatch_action(window.into(), workspace::PlaybackStop);
    assert_eq!(transport_state(window, cx), (0, PlaybackState::Stopped));
}

/// The default keybindings reach the transport through the same single
/// dispatch path (Space toggles, arrows step).
#[gpui::test]
fn default_chords_dispatch_transport_commands(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    cx.simulate_keystrokes(window.into(), "right right left");
    assert_eq!(transport_state(window, cx), (1, PlaybackState::Paused));

    cx.simulate_keystrokes(window.into(), "space");
    let (_, state) = transport_state(window, cx);
    assert_eq!(state, PlaybackState::Playing);

    cx.simulate_keystrokes(window.into(), "k");
    assert_eq!(transport_state(window, cx), (0, PlaybackState::Stopped));
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

    let timeline = cx.add_window(panels::timeline::TimelineGpuiPanel::new);
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

    // The clock adopted the demo composition's parameters (30 fps, 300 f).
    cx.update(|cx| {
        let transport = controller.read(cx).transport();
        assert_eq!(transport.fps(), ravel_core::types::FrameRate::new(30, 1));
    });
}

/// Every transport position change records the shared playback position, so
/// selection-driven evaluations use the frame under the playhead
/// (`docs/implementation/playback-foundation-plan.md`, unit 3).
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

    let timeline = cx.add_window(panels::timeline::TimelineGpuiPanel::new);
    let controller = cx.update(|cx| cx.new(|_| PlaybackController::new()));

    // Mirror the production nesting: the seek runs inside the timeline
    // panel's own update, exactly like `scrub_playhead`.
    timeline
        .update(cx, |timeline, _window, cx| {
            let (fps, duration) = timeline.composition_params();
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
