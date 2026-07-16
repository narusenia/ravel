// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Phase 0 reproduction tests for the command/focus refactor.
//!
//! These tests pin down the *current* (broken) dispatch behavior so that the
//! refactor phases can prove they fixed it. Each test documents which failure
//! mode from `docs/implementation/gpui-command-focus-refactor-plan.md` it
//! reproduces. When a later phase fixes a failure mode, update the assertion
//! here to the desired behavior instead of deleting the test.

use gpui::{Context, Empty, Render, TestAppContext, Window};
use ravel_app::panels;
use ravel_app::trace::{self, CommandTrace, TraceSource};
use ravel_app::workspace::{self, PendingCommand, RavelWorkspace};
use ravel_ui::command::CommandId;
use ravel_ui::panel::PanelKind;
use ravel_ui::shell::AppShell;

/// Root view with no action handlers: actions dispatched into this window
/// reach only the App-level handlers, like a detached panel window today.
struct BareView;

impl Render for BareView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl gpui::IntoElement {
        Empty
    }
}

fn init_i18n() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    let _ = ravel_i18n::init(&dir, "en");
}

fn init_globals(cx: &mut gpui::App) {
    cx.set_global(panels::FocusedPanelGlobal(None));
    cx.set_global(panels::SelectedPropertiesTarget::default());
    cx.set_global(PendingCommand(None));
    cx.set_global(workspace::DetachedWindowHandles(Default::default()));
    trace::init(cx);
}

/// Failure mode: 「上書き」 — `PendingCommand` is a `Global<Option<CommandId>>`,
/// so two commands arriving before the next `render()` collapse into one; the
/// first is silently lost, never executed.
#[gpui::test]
fn pending_command_overwrite_loses_first_command(cx: &mut TestAppContext) {
    init_i18n();
    cx.update(|cx| {
        init_globals(cx);
        workspace::register_action_handlers(cx);
    });

    // A window whose root handles nothing, so actions bubble to App level —
    // the same route detached panel windows use.
    let window = cx.add_window(|_, _| BareView);

    cx.dispatch_action(window.into(), workspace::EditCopy);
    cx.dispatch_action(window.into(), workspace::EditUndo);

    let (pending, overwrites) = cx.update(|cx| {
        let pending = cx.global::<PendingCommand>().0;
        let overwrites = cx
            .global::<CommandTrace>()
            .0
            .iter()
            .filter(|e| e.source == TraceSource::AppAction && e.outcome.is_some())
            .count();
        (pending, overwrites)
    });

    // BROKEN TODAY: EditCopy was overwritten before any render() consumed it.
    assert_eq!(pending, Some(CommandId::EditUndo));
    assert_eq!(
        overwrites, 1,
        "the second app-level action should have overwritten the first pending command"
    );
}

/// Builds a real `RavelWorkspace` window. Panels needing a GPU or media
/// backend (NodeGraph) are toggled invisible first so the test stays headless.
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

    cx.add_window(move |window, cx| RavelWorkspace::new(shell, window, cx))
}

/// Failure mode: 「未配送」 — with focus inside the main window, EditUndo is
/// consumed by the workspace-level `on_action`, which ignores
/// `CommandOutcome::Delegate`. The App-level handler (the only path that turns
/// EditUndo into a `PanelUndoRedo` signal) never runs, so undo is dropped.
///
/// If GPUI instead runs both handlers, this test fails with 2 executions —
/// that is the 「二重実行」 failure mode. Either way Phase 2 must make this
/// exactly one execution that actually reaches a panel.
#[gpui::test]
fn keyboard_undo_in_main_window_is_swallowed_by_workspace(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    cx.simulate_keystrokes(window.into(), "cmd-z");

    let (entries, undo_signal) = cx.update(|cx| {
        let entries = cx.global::<CommandTrace>().0.clone();
        let undo_signal = cx.try_global::<panels::PanelUndoRedo>().and_then(|g| g.0);
        (entries, undo_signal)
    });

    let workspace_hits = entries
        .iter()
        .filter(|e| {
            e.source == TraceSource::WorkspaceAction && e.command == Some(CommandId::EditUndo)
        })
        .count();
    let app_hits = entries
        .iter()
        .filter(|e| e.source == TraceSource::AppAction && e.command == Some(CommandId::EditUndo))
        .count();
    let render_hits = entries
        .iter()
        .filter(|e| {
            e.source == TraceSource::RenderPending && e.command == Some(CommandId::EditUndo)
        })
        .count();

    // BROKEN TODAY: the workspace on_action consumes the action and discards
    // the Delegate outcome; no undo signal ever reaches a panel.
    assert_eq!(
        (workspace_hits, app_hits, render_hits),
        (1, 0, 0),
        "expected the workspace handler to swallow EditUndo exclusively; trace: {entries:#?}"
    );
    assert_eq!(
        undo_signal, None,
        "undo signal must not have been delivered (that is the bug)"
    );
}

/// Failure mode: 「誤った focus target」 — `RavelWorkspace::render()` refocuses
/// its own handle on every frame, so any focus a panel (or one of its input
/// widgets) took is stolen back on the next render.
#[gpui::test]
fn render_steals_focus_from_panels(cx: &mut TestAppContext) {
    let window = open_workspace(cx);
    cx.run_until_parked();

    // Focus some non-workspace handle, as a panel click would.
    let panel_handle = window
        .update(cx, |_workspace, window, cx| {
            let handle = cx.focus_handle();
            window.focus(&handle, cx);
            handle
        })
        .unwrap();

    // Trigger another frame; render() must not move focus, but today it does.
    cx.update(|cx| cx.refresh_windows());
    cx.run_until_parked();

    let panel_still_focused = window
        .update(cx, |_workspace, window, _cx| {
            panel_handle.is_focused(window)
        })
        .unwrap();

    // BROKEN TODAY: focus snapped back to the workspace root.
    assert!(
        !panel_still_focused,
        "expected render() to steal focus back to the workspace (current bug)"
    );
}
