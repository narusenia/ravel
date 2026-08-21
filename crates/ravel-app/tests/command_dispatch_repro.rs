// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Regression tests for the command/focus refactor.
//!
//! Dispatch tests assert the Phase 2 behavior, focus tests cover Phase 3, and
//! the reload/rebuild tests cover the Phase 6 regression matrix.

use gpui::{Context, Empty, Entity, Focusable, Render, TestAppContext, Window};
use gpui_component::Root;
use ravel_app::panels;
use ravel_app::trace::{self, CommandTrace, TraceSource};
use ravel_app::window_host::{self, WindowRegistry};
use ravel_app::workspace::{self, RavelWorkspace};
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
    // A live eval worker thread would wake the deterministic test
    // scheduler from outside and fail the run.
    ravel_app::project_state::disable_background_eval_for_tests();
    cx.set_global(panels::FocusedPanelGlobal(None));
    cx.set_global(panels::SelectedPropertiesTarget::default());
    cx.set_global(panels::CanvasSelection::default());
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

/// The View toggle for the node bodies' parameter values reaches the UI-state
/// global through the ordinary command route (`PGRP-5`).
///
/// The panel-level test drives `panels::toggle_node_param_values` directly, so
/// it cannot see a dispatch arm that stopped calling it. This one dispatches
/// the action a menu click dispatches.
#[gpui::test]
fn the_view_toggle_flips_the_node_parameter_values_global(cx: &mut TestAppContext) {
    let _main_window = open_workspace(cx);
    let window = cx.add_window(|_, _| BareView);

    assert!(
        cx.update(|cx| panels::show_node_param_values(cx)),
        "drawn is the default"
    );

    cx.dispatch_action(window.into(), workspace::ViewToggleNodeParamValues);
    assert!(
        !cx.update(|cx| panels::show_node_param_values(cx)),
        "the command hid the rows"
    );

    cx.dispatch_action(window.into(), workspace::ViewToggleNodeParamValues);
    assert!(
        cx.update(|cx| panels::show_node_param_values(cx)),
        "and brought them back"
    );
}

/// Builds a real main window. Panels needing a GPU or media backend
/// (NodeGraph) are toggled invisible first so the test stays headless.
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

/// The shared session every window dispatches into.
fn session(cx: &mut TestAppContext) -> Entity<RavelWorkspace> {
    cx.update(|cx| workspace::session(cx).expect("the session is installed"))
}

/// Focuses the first instance of `kind`, as clicking into that pane would.
///
/// Focus is per panel instance since the dock cutover, so a test that means
/// "the Viewer is focused" resolves the instance through the layout.
fn focus_panel(kind: PanelKind, cx: &mut TestAppContext) {
    cx.update(|cx| {
        let instance = workspace::session(cx)
            .expect("the session is installed")
            .read(cx)
            .shell()
            .first_instance_of(kind)
            .expect("the panel is docked in the layout")
            .id;
        cx.set_global(panels::FocusedPanelGlobal(Some(instance)));
    });
}

/// Without a focused panel handler, the workspace handles EditUndo once.
#[gpui::test]
fn workspace_handles_edit_undo_exactly_once(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    focus_panel(PanelKind::Viewer, cx);
    // `secondary-`, not `cmd-`: the binding is registered that way so the
    // primary modifier is Cmd on macOS and Control elsewhere, and a
    // literal `cmd-` here would miss it off macOS.
    cx.simulate_keystrokes(window.into(), "secondary-z");

    let (entries, undo_executions, shell_focused_panel) = cx.update(|cx| {
        let entries = cx.global::<CommandTrace>().0.clone();
        let undo_executions = trace::execution_count(cx, CommandId::EditUndo);
        let shell_focused_panel = workspace::session(cx)
            .expect("the session is installed")
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

    let instance = ravel_ui::layout::PanelInstanceId(0);
    let window = cx.add_window(|window, cx| {
        panels::PlaceholderPanel::new(instance, PanelKind::Viewer, window, cx)
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
    assert_eq!(focused, Some(instance));
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
    let session = session(cx);
    session.update(cx, |workspace, _cx| {
        workspace.shell.set_keybindings(bindings);
    });
    cx.update(|cx| {
        let shell_bindings = session.read(cx).shell().clone();
        cx.clear_key_bindings();
        cx.bind_keys(workspace::build_keybindings(&shell_bindings));
    });

    cx.simulate_keystrokes(window.into(), "secondary-u");
    // The old chord must no longer fire; the new one fires exactly once.
    cx.simulate_keystrokes(window.into(), "secondary-z");

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

    cx.simulate_keystrokes(window.into(), "secondary-c");

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
        focus_panel(panel, cx);
        cx.simulate_keystrokes(window.into(), "secondary-z");
        let synced = cx.update(|cx| {
            workspace::session(cx)
                .expect("the session is installed")
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

/// Reattaching from the detached window itself must close that window.
/// The close is deferred because the detached window is still on the update
/// stack when the app-level fallback routes the command to the workspace.
#[gpui::test]
fn reattach_from_detached_window_closes_it(cx: &mut TestAppContext) {
    let main_window = open_workspace(cx);
    let baseline_windows = cx.update(|cx| cx.windows().len());

    focus_panel(PanelKind::Viewer, cx);
    cx.dispatch_action(main_window.into(), workspace::PanelDetach);
    cx.run_until_parked();

    let detached = cx.update(|cx| {
        let detached = cx.global::<WindowRegistry>().detached();
        assert_eq!(detached.len(), 1, "detach must register one window handle");
        detached[0].1
    });
    assert_eq!(
        cx.update(|cx| cx.windows().len()),
        baseline_windows + 1,
        "detach must open a new OS window"
    );

    // Dispatch the reattach from the detached window — the route that used to
    // leak it.
    cx.dispatch_action(detached, workspace::PanelReattach);
    cx.run_until_parked();

    let handles_left = cx.update(|cx| cx.global::<WindowRegistry>().detached().len());
    assert_eq!(handles_left, 0, "reattach must drop the window handle");
    assert_eq!(
        cx.update(|cx| cx.windows().len()),
        baseline_windows,
        "reattach must close the detached window"
    );
}
