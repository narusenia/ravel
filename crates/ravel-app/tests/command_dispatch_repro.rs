// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Regression tests for the command/focus refactor.
//!
//! Dispatch tests assert the Phase 2 behavior and focus tests cover Phase 3.

use gpui::{Context, Empty, Focusable, Render, TestAppContext, Window};
use ravel_app::panels;
use ravel_app::trace::{self, CommandTrace, TraceSource};
use ravel_app::workspace::{self, MainWorkspace, RavelWorkspace};
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
    cx.set_global(workspace::DetachedWindowHandles(Default::default()));
    trace::init(cx);
}

/// Two App-level fallback actions are routed immediately and each executes
/// exactly once in the main workspace.
#[gpui::test]
fn two_app_level_actions_each_execute_exactly_once(cx: &mut TestAppContext) {
    let _main_window = open_workspace(cx);

    // A window whose root handles nothing, so actions bubble to App level —
    // the same route detached panel windows use.
    let window = cx.add_window(|_, _| BareView);

    cx.dispatch_action(window.into(), workspace::EditCopy);
    cx.dispatch_action(window.into(), workspace::EditUndo);

    let (copy_executions, undo_executions, app_commands) = cx.update(|cx| {
        let app_commands = cx
            .global::<CommandTrace>()
            .0
            .iter()
            .filter(|entry| entry.source == TraceSource::AppAction)
            .filter_map(|entry| entry.command)
            .collect::<Vec<_>>();
        (
            trace::execution_count(cx, CommandId::EditCopy),
            trace::execution_count(cx, CommandId::EditUndo),
            app_commands,
        )
    });

    assert_eq!(copy_executions, 1);
    assert_eq!(undo_executions, 1);
    assert_eq!(app_commands, [CommandId::EditCopy, CommandId::EditUndo]);
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

    let window = cx.add_window(move |window, cx| RavelWorkspace::new(shell, window, cx));
    cx.update(|cx| {
        let workspace = window
            .entity(cx)
            .expect("workspace window should have a root entity");
        cx.set_global(MainWorkspace::new(window.into(), workspace.downgrade()));
    });
    window
}

/// The workspace handles EditUndo once and emits the temporary panel signal.
#[gpui::test]
fn workspace_handles_edit_undo_exactly_once(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    cx.update(|cx| cx.set_global(panels::FocusedPanelGlobal(Some(PanelKind::Viewer))));
    cx.simulate_keystrokes(window.into(), "cmd-z");

    let (entries, undo_signal, shell_focused_panel) = cx.update(|cx| {
        let entries = cx.global::<CommandTrace>().0.clone();
        let undo_signal = cx.try_global::<panels::PanelUndoRedo>().and_then(|g| g.0);
        let shell_focused_panel = window
            .entity(cx)
            .expect("workspace window should have a root entity")
            .read(cx)
            .shell()
            .focused_panel();
        (entries, undo_signal, shell_focused_panel)
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
    assert_eq!(
        (workspace_hits, app_hits),
        (1, 0),
        "expected one exclusive workspace dispatch; trace: {entries:#?}"
    );
    assert_eq!(
        undo_signal,
        Some(panels::UndoRedoSignal::Undo),
        "workspace dispatch should deliver the temporary undo signal"
    );
    assert_eq!(
        shell_focused_panel,
        Some(PanelKind::Viewer),
        "workspace dispatch should sync the shell from the focus global"
    );
}

/// Rendering the workspace does not take focus back from a panel or child input.
#[gpui::test]
fn panel_focus_survives_workspace_render(cx: &mut TestAppContext) {
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

    // Trigger another frame; render() must not move focus.
    cx.update(|cx| cx.refresh_windows());
    cx.run_until_parked();

    let panel_still_focused = window
        .update(cx, |_workspace, window, _cx| {
            panel_handle.is_focused(window)
        })
        .unwrap();

    assert!(
        panel_still_focused,
        "workspace rendering should preserve the panel's focus"
    );
}

/// The shared panel focus state follows GPUI focus events, not click history.
#[gpui::test]
fn focused_panel_global_tracks_panel_focus_handle(cx: &mut TestAppContext) {
    init_i18n();
    cx.update(|cx| {
        gpui_component::init(cx);
        init_globals(cx);
    });

    let window = cx.add_window(|window, cx| {
        panels::PlaceholderPanel::new("viewer", Some(PanelKind::Viewer), window, cx)
    });
    window
        .update(cx, |_panel, window, _cx| window.activate_window())
        .unwrap();
    cx.update(|cx| cx.refresh_windows());
    cx.run_until_parked();

    window
        .update(cx, |panel, window, cx| {
            panel.focus_handle(cx).focus(window, cx);
        })
        .unwrap();
    cx.update(|cx| cx.refresh_windows());
    cx.run_until_parked();

    let focused = cx.update(|cx| cx.global::<panels::FocusedPanelGlobal>().0);
    assert_eq!(focused, Some(PanelKind::Viewer));
}
