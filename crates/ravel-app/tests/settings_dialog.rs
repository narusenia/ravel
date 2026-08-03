// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPUI integration coverage for the settings dialogs (REQ-PROJ-004).
//!
//! Three things are pinned here: the commands open and close a modal, the key
//! chord reaches the same command through the keybinding table, and opening a
//! dialog leaves the workspace's focus ownership alone — a dialog is not a
//! panel, so it must never repoint `FocusedPanelGlobal`.

use gpui::{AnyWindowHandle, Entity, Pixels, Size, TestAppContext, WindowHandle, px};
use gpui_component::{Root, WindowExt as _};
use ravel_app::panels;
use ravel_app::trace;
use ravel_app::window_host;
use ravel_app::workspace::{self, RavelWorkspace};
use ravel_ui::command::CommandId;
use ravel_ui::layout::PanelInstanceId;
use ravel_ui::panel::PanelKind;
use ravel_ui::shell::AppShell;

const WINDOW_SIZE: Size<Pixels> = Size {
    width: px(1000.0),
    height: px(700.0),
};

struct Harness {
    window: WindowHandle<Root>,
    workspace: Entity<RavelWorkspace>,
}

fn init_i18n() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    let _ = ravel_i18n::init(&dir, "en");
}

/// A real main window with the default keybindings bound, so the chord route
/// and the command route both exist.
///
/// Panels needing a GPU or a media backend are toggled out first to keep the
/// test headless.
fn open_workspace(cx: &mut TestAppContext) -> Harness {
    init_i18n();
    cx.update(|cx| {
        gpui_component::init(cx);
        ravel_app::project_state::disable_background_eval_for_tests();
        cx.set_global(panels::FocusedPanelGlobal(None));
        cx.set_global(panels::SelectedPropertiesTarget::default());
        cx.set_global(panels::CanvasSelection::default());
        cx.set_global(panels::ToolState::default());
        cx.set_global(panels::PlaybackPosition::default());
        cx.set_global(panels::ViewerFrame::default());
        trace::init(cx);
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
    cx.update(|cx| cx.bind_keys(workspace::build_keybindings(&shell)));

    let window = cx.open_window(WINDOW_SIZE, move |window, cx| {
        window_host::main_root(shell, window, cx)
    });
    let workspace = cx.update(|cx| workspace::session(cx).expect("the session is installed"));
    cx.run_until_parked();
    Harness { window, workspace }
}

fn dispatch(harness: &Harness, command: CommandId, cx: &mut TestAppContext) {
    AnyWindowHandle::from(harness.window)
        .update(cx, |_root, window, cx| {
            harness.workspace.update(cx, |workspace, cx| {
                workspace.dispatch_command(command, window, cx);
            });
        })
        .unwrap();
    // Draw the dialog: a body that panics while rendering has to fail here
    // rather than pass as "the dialog is open".
    cx.update(|cx| cx.refresh_windows());
    cx.run_until_parked();
}

fn has_dialog(harness: &Harness, cx: &mut TestAppContext) -> bool {
    AnyWindowHandle::from(harness.window)
        .update(cx, |_root, window, cx| window.has_active_dialog(cx))
        .unwrap()
}

fn close_dialog(harness: &Harness, cx: &mut TestAppContext) {
    cx.simulate_keystrokes(harness.window.into(), "escape");
    cx.run_until_parked();
}

fn focused_instance(cx: &mut TestAppContext) -> Option<PanelInstanceId> {
    cx.update(|cx| cx.global::<panels::FocusedPanelGlobal>().0)
}

/// Both screens open from their command and close again, and each one is a
/// modal rather than a window or a pane.
#[gpui::test]
fn each_settings_command_opens_and_closes_a_modal(cx: &mut TestAppContext) {
    let harness = open_workspace(cx);

    for command in [CommandId::AppPreferences, CommandId::ProjectSettings] {
        assert!(!has_dialog(&harness, cx));
        dispatch(&harness, command, cx);
        assert!(has_dialog(&harness, cx), "{command} must open its dialog");
        close_dialog(&harness, cx);
        assert!(
            !has_dialog(&harness, cx),
            "{command}'s dialog must close again"
        );
    }
}

/// The default chord reaches the same command through the keybinding table, so
/// the asset entry and the action table are wired to each other.
#[gpui::test]
fn the_preferences_chord_opens_the_dialog(cx: &mut TestAppContext) {
    let harness = open_workspace(cx);

    cx.simulate_keystrokes(harness.window.into(), "cmd-,");
    cx.run_until_parked();

    assert!(
        has_dialog(&harness, cx),
        "the default Preferences chord must open the dialog"
    );
    assert_eq!(
        cx.update(|cx| trace::execution_count(cx, CommandId::AppPreferences)),
        1,
        "the chord must dispatch the command exactly once"
    );
}

/// A dialog is not a panel: opening one leaves the focused panel instance where
/// it was, so every panel-scoped command still acts on the same target after the
/// dialog closes.
#[gpui::test]
fn opening_a_settings_dialog_keeps_the_focused_panel(cx: &mut TestAppContext) {
    let harness = open_workspace(cx);
    let instance = cx.update(|cx| {
        workspace::session(cx)
            .expect("the session is installed")
            .read(cx)
            .shell()
            .first_instance_of(PanelKind::Viewer)
            .expect("the Viewer is docked in the layout")
            .id
    });
    cx.update(|cx| cx.set_global(panels::FocusedPanelGlobal(Some(instance))));

    dispatch(&harness, CommandId::AppPreferences, cx);
    assert!(has_dialog(&harness, cx));
    assert_eq!(
        focused_instance(cx),
        Some(instance),
        "opening the dialog must not repoint FocusedPanelGlobal"
    );
    assert_eq!(
        cx.update(|cx| workspace::session(cx)
            .expect("the session is installed")
            .read(cx)
            .shell()
            .focused_panel()),
        Some(PanelKind::Viewer),
        "the shell's focused panel must survive the dialog"
    );

    close_dialog(&harness, cx);
    assert_eq!(
        focused_instance(cx),
        Some(instance),
        "closing the dialog must not repoint FocusedPanelGlobal either"
    );
}
