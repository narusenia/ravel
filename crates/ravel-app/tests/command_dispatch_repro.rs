// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Regression tests for the command/focus refactor.
//!
//! Dispatch tests assert the Phase 2 behavior, focus tests cover Phase 3, and
//! the reload/rebuild tests cover the Phase 6 regression matrix.

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

/// Without a focused panel handler, the workspace handles EditUndo once.
#[gpui::test]
fn workspace_handles_edit_undo_exactly_once(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    cx.update(|cx| cx.set_global(panels::FocusedPanelGlobal(Some(PanelKind::Viewer))));
    cx.simulate_keystrokes(window.into(), "cmd-z");

    let (entries, undo_executions, shell_focused_panel) = cx.update(|cx| {
        let entries = cx.global::<CommandTrace>().0.clone();
        let undo_executions = trace::execution_count(cx, CommandId::EditUndo);
        let shell_focused_panel = window
            .entity(cx)
            .expect("workspace window should have a root entity")
            .read(cx)
            .shell()
            .focused_panel();
        (entries, undo_executions, shell_focused_panel)
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
    assert_eq!(undo_executions, 1, "EditUndo should execute exactly once");
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

/// After reloading keybindings from TOML, the new chord dispatches through the
/// same single path.
#[gpui::test]
fn rebound_toml_chord_dispatches_once(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    // Rebind undo to Cmd+U, as a keybinding file reload would.
    let custom = r#"
[meta]
name = "Test"

[edit]
undo = "Cmd+U"
"#;
    let bindings = ravel_ui::keybindings::parser::parse_toml(custom)
        .expect("custom keybinding TOML should parse");
    window
        .update(cx, |workspace, _window, _cx| {
            workspace.shell.set_keybindings(bindings);
        })
        .unwrap();
    cx.update(|cx| {
        let shell_bindings = window
            .entity(cx)
            .expect("workspace window should have a root entity")
            .read(cx)
            .shell()
            .clone();
        cx.clear_key_bindings();
        cx.bind_keys(workspace::build_keybindings(&shell_bindings));
    });

    cx.simulate_keystrokes(window.into(), "cmd-u");
    // The old chord must no longer fire; the new one fires exactly once.
    cx.simulate_keystrokes(window.into(), "cmd-z");

    let undo_executions = cx.update(|cx| trace::execution_count(cx, CommandId::EditUndo));
    assert_eq!(
        undo_executions, 1,
        "exactly the rebound chord should dispatch EditUndo"
    );
}

/// A preset switch rebuilds the dock layout; the workspace action handlers
/// must not double up afterwards.
#[gpui::test]
fn layout_rebuild_does_not_duplicate_handlers(cx: &mut TestAppContext) {
    let window = open_workspace(cx);
    cx.run_until_parked();

    // Switch preset (full layout rebuild on the next frame), then render.
    cx.dispatch_action(window.into(), workspace::WorkspaceNode);
    cx.update(|cx| cx.refresh_windows());
    cx.run_until_parked();

    cx.simulate_keystrokes(window.into(), "cmd-c");

    let (copy_executions, entries) = cx.update(|cx| {
        (
            trace::execution_count(cx, CommandId::EditCopy),
            cx.global::<CommandTrace>().0.clone(),
        )
    });
    assert_eq!(
        copy_executions, 1,
        "EditCopy must dispatch exactly once after a layout rebuild; trace: {entries:#?}"
    );
}

/// Commands dispatched after switching panels target the newly focused panel.
#[gpui::test]
fn dispatch_follows_panel_switch(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    for panel in [PanelKind::Viewer, PanelKind::Outliner] {
        cx.update(|cx| cx.set_global(panels::FocusedPanelGlobal(Some(panel))));
        cx.simulate_keystrokes(window.into(), "cmd-z");
        let synced = cx.update(|cx| {
            window
                .entity(cx)
                .expect("workspace window should have a root entity")
                .read(cx)
                .shell()
                .focused_panel()
        });
        assert_eq!(
            synced,
            Some(panel),
            "dispatch must target the panel focused at dispatch time"
        );
    }
}
