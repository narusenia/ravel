// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Window lifecycle and layout rendering against the uniform window host.
//!
//! These cover the shell-visible half of the multi-window host: the handle
//! registry, the OS close button routing through `AppShell::close_window`
//! (MED-APP-01), panel views surviving a detach round trip, and the main
//! window re-rendering the tree the shell holds after a toggle or a preset
//! switch. Minimize follow and the dialog layers need a real platform window
//! and are verified on device.

use gpui::{
    InteractiveElement as _, ParentElement as _, SharedString, TestAppContext, VisualTestContext,
};
use gpui_component::Root;
use gpui_component::WindowExt as _;
use gpui_component::button::Button;
use ravel_app::panels;
use ravel_app::trace;
use ravel_app::window_host::{self, WindowRegistry};
use ravel_app::workspace;
use ravel_ui::command::CommandId;
use ravel_ui::panel::PanelKind;
use ravel_ui::shell::AppShell;
use std::time::Duration;

fn init_i18n() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    let _ = ravel_i18n::init(&dir, "en");
}

/// Builds a real main window. Panels needing a GPU or media backend
/// (NodeGraph) are toggled invisible first so the test stays headless.
fn open_workspace(cx: &mut TestAppContext) -> gpui::WindowHandle<Root> {
    init_i18n();
    cx.update(|cx| {
        gpui_component::init(cx);
        // A live eval worker thread would wake the deterministic test
        // scheduler from outside and fail the run.
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
    for (panel, toggle) in [
        (PanelKind::NodeGraph, CommandId::ViewToggleNodeGraph),
        (PanelKind::Timeline, CommandId::ViewToggleTimeline),
        (PanelKind::Properties, CommandId::ViewToggleProperties),
    ] {
        if shell.visibility().is_visible(panel) {
            shell.handle_command(toggle);
        }
    }

    cx.add_window(move |window, cx| window_host::main_root(shell, window, cx))
}

/// The shell state the session owns.
fn shell(cx: &mut TestAppContext) -> ravel_ui::shell::AppShell {
    cx.update(|cx| {
        workspace::session(cx)
            .expect("the session is installed")
            .read(cx)
            .shell()
            .clone()
    })
}

/// Entity ids of the main window's cached pane views, one per observed panel.
///
/// A changed id means the pane was rebuilt and lost its view state.
fn main_pane_views(observed: &[PanelKind], cx: &mut TestAppContext) -> Vec<Option<gpui::EntityId>> {
    cx.update(|cx| {
        let main = cx
            .global::<WindowRegistry>()
            .main()
            .expect("the main window is registered");
        let host = cx
            .global::<WindowRegistry>()
            .host(main)
            .expect("the main window has a host")
            .upgrade()
            .expect("the host is alive");
        let host = host.read(cx);
        observed
            .iter()
            .map(|kind| host.panel_view_id(*kind, cx))
            .collect()
    })
}

/// The panels the main window is actually *rendering*, in tree order.
fn main_dock_panels(cx: &mut TestAppContext) -> Vec<PanelKind> {
    cx.update(|cx| {
        let main = cx
            .global::<WindowRegistry>()
            .main()
            .expect("the main window is registered");
        let host = cx
            .global::<WindowRegistry>()
            .host(main)
            .expect("the main window has a host")
            .upgrade()
            .expect("the host is alive");
        host.read(cx).rendered_tree(cx).panels()
    })
}

/// Detaches the Viewer out of the main window and returns the detached
/// window's logical id and GPUI handle.
fn detach_viewer(
    cx: &mut TestAppContext,
    main: gpui::WindowHandle<Root>,
) -> (ravel_ui::window::WindowId, gpui::AnyWindowHandle) {
    // Focus is per panel instance: resolve the Viewer's through the layout,
    // as a click into that pane would.
    cx.update(|cx| {
        let instance = workspace::session(cx)
            .expect("the session is installed")
            .read(cx)
            .shell()
            .first_instance_of(PanelKind::Viewer)
            .expect("the Viewer is docked in the main window")
            .id;
        cx.set_global(panels::FocusedPanelGlobal(Some(instance)));
    });
    cx.dispatch_action(main.into(), workspace::PanelDetach);
    cx.run_until_parked();
    cx.update(|cx| {
        let detached = cx.global::<WindowRegistry>().detached();
        assert_eq!(detached.len(), 1, "detach must register one window handle");
        detached[0]
    })
}

/// The main window is in the registry under the layout's main window id, so
/// window lifecycle and cross-window drops have one table to resolve through.
#[gpui::test]
fn main_window_registers_its_handle(cx: &mut TestAppContext) {
    let window = open_workspace(cx);

    let (main_id, handle, len) = cx.update(|cx| {
        let registry = cx.global::<WindowRegistry>();
        (
            registry.main(),
            registry.handle(ravel_ui::window::WindowId(0)),
            registry.len(),
        )
    });
    assert_eq!(main_id, Some(ravel_ui::window::WindowId(0)));
    assert_eq!(handle, Some(window.into()));
    assert_eq!(len, 1, "only the main window is open");
    assert!(
        cx.update(|cx| cx.global::<WindowRegistry>().detached().is_empty()),
        "the main window is not a detached window"
    );
}

/// Closing a detached window with the OS close button goes through
/// `AppShell::close_window`: the window and its instances leave the layout,
/// focus that pointed into it is dropped, and no stale handle is left behind
/// (MED-APP-01).
#[gpui::test]
fn close_button_drops_the_window_from_shell_and_registry(cx: &mut TestAppContext) {
    let main = open_workspace(cx);
    let (detached_id, detached) = detach_viewer(cx, main);

    // The shell moved the Viewer instance into the new window.
    let (windows, viewer_in_main) = {
        let shell = shell(cx);
        (
            shell.layout().windows().len(),
            shell.visibility().is_visible(PanelKind::Viewer),
        )
    };
    assert_eq!(windows, 2, "detach must add a window to the layout");
    assert!(!viewer_in_main, "the Viewer left the main window");

    // The user clicks the detached window's close button.
    let mut detached_cx = VisualTestContext::from_window(detached, cx);
    assert!(
        detached_cx.simulate_close(),
        "the detached window must accept the close"
    );
    cx.run_until_parked();

    let (has_handle, detached_left) = cx.update(|cx| {
        let registry = cx.global::<WindowRegistry>();
        (registry.contains(detached_id), registry.detached().len())
    });
    let (windows, focused) = {
        let shell = shell(cx);
        (shell.layout().windows().len(), shell.focused_instance())
    };
    assert_eq!(windows, 1, "the closed window must leave the layout");
    assert!(!has_handle, "no stale handle may stay in the registry");
    assert_eq!(detached_left, 0);
    assert_eq!(
        focused, None,
        "focus pointing into the closed window must be dropped"
    );
    // The instances went with the window; the panel is toggleable again.
    let visible = shell(cx).visibility().is_visible(PanelKind::Viewer);
    assert!(!visible, "a closed detached window destroys its instances");
}

/// A close the model already decided (reattach) must not double-apply: the
/// handle is gone before the platform close, so the close handler is a no-op
/// and the layout keeps the absorbed instances.
#[gpui::test]
fn reattach_close_does_not_reapply_to_the_shell(cx: &mut TestAppContext) {
    let main = open_workspace(cx);
    let (detached_id, detached) = detach_viewer(cx, main);

    cx.dispatch_action(detached, workspace::PanelReattach);
    cx.run_until_parked();

    let has_handle = cx.update(|cx| cx.global::<WindowRegistry>().contains(detached_id));
    let (windows, viewer_in_main) = {
        let shell = shell(cx);
        (
            shell.layout().windows().len(),
            shell.visibility().is_visible(PanelKind::Viewer),
        )
    };
    assert_eq!(windows, 1, "reattach absorbed the detached window");
    assert!(!has_handle);
    assert!(viewer_in_main, "the Viewer is back in the main window");
}

/// A detached window focuses the pane it was opened around, not its own frame.
///
/// `FocusedPanelGlobal` follows real focus events, so a frame that kept the
/// focus would leave the workspace with no focused instance — `Cmd+Shift+R`
/// straight after `Cmd+Shift+D` then found nothing to reattach until the user
/// clicked into the pane (`MED-APP-22`).
#[gpui::test]
fn a_detached_window_focuses_the_pane_it_was_opened_around(cx: &mut TestAppContext) {
    let main = open_workspace(cx);
    let (detached_id, detached) = detach_viewer(cx, main);

    let host = cx.update(|cx| {
        cx.global::<WindowRegistry>()
            .host(detached_id)
            .expect("the detached window has a host")
    });
    let pane_has_focus = detached
        .update(cx, |_root, window, cx| {
            host.read_with(cx, |host, cx| {
                host.pane_is_focused(PanelKind::Viewer, window, cx)
            })
            .expect("the host entity is alive")
        })
        .expect("the detached window is open");
    assert!(
        pane_has_focus,
        "the moved pane holds the focus, so the workspace knows which instance is active"
    );
}

/// A dialog opened in a detached window is actually painted: the host places
/// the modal layers `Root` leaves to it, which the old detached view did not —
/// a dialog there was live and invisible.
#[gpui::test]
fn detached_window_paints_its_dialog_layer(cx: &mut TestAppContext) {
    let main = open_workspace(cx);
    let (_id, detached) = detach_viewer(cx, main);

    let mut detached_cx = VisualTestContext::from_window(detached, cx);
    detached_cx.update(|window, cx| {
        window.open_dialog(cx, |dialog, _window, _cx| {
            dialog
                .title(SharedString::from("probe"))
                .content(|body, _window, _cx| {
                    body.child(
                        Button::new("detached-dialog-probe")
                            .label(SharedString::from("probe"))
                            .debug_selector(|| "detached-dialog-probe".into()),
                    )
                })
        });
    });
    detached_cx
        .background_executor
        .advance_clock(*gpui_component::dialog::ANIMATION_DURATION + Duration::from_millis(50));
    detached_cx.update(|window, _cx| window.refresh());
    detached_cx.run_until_parked();

    assert!(
        detached_cx.debug_bounds("detached-dialog-probe").is_some(),
        "the detached window must render the dialog layer"
    );
}

/// A detach/reattach round trip must not rebuild the main window's panels:
/// the cached view entities are what carry per-panel view state.
#[gpui::test]
fn detach_round_trip_keeps_main_panel_views(cx: &mut TestAppContext) {
    let main = open_workspace(cx);
    cx.run_until_parked();

    let observed = [PanelKind::Outliner, PanelKind::Viewer, PanelKind::MediaBin];
    let before = main_pane_views(&observed, cx);
    assert!(
        before.iter().all(Option::is_some),
        "the preset's panels must be built before the round trip: {before:?}"
    );

    let (_id, detached) = detach_viewer(cx, main);
    cx.dispatch_action(detached, workspace::PanelReattach);
    cx.run_until_parked();

    let after = main_pane_views(&observed, cx);
    assert_eq!(
        before, after,
        "detach/reattach must reuse the cached panel views, not rebuild them"
    );
}

/// A View toggle reaches the rendered dock: the shell inserts the instance at
/// the panel's default slot and the main window's host re-renders the tree the
/// shell now holds — the toggle no longer depends on the active preset laying
/// the panel out (issue #181).
#[gpui::test]
fn view_toggle_retrees_the_main_window(cx: &mut TestAppContext) {
    let main = open_workspace(cx);
    cx.run_until_parked();
    assert!(
        !main_dock_panels(cx).contains(&PanelKind::Dopesheet),
        "the Edit preset does not lay out the Dopesheet"
    );

    cx.dispatch_action(main.into(), workspace::ViewToggleDopesheet);
    cx.run_until_parked();
    assert!(
        main_dock_panels(cx).contains(&PanelKind::Dopesheet),
        "the toggled-on panel must appear in the rendered tree"
    );

    cx.dispatch_action(main.into(), workspace::ViewToggleDopesheet);
    cx.run_until_parked();
    assert!(
        !main_dock_panels(cx).contains(&PanelKind::Dopesheet),
        "the toggled-off panel must leave the rendered tree"
    );
}

/// Switching preset replaces the main window's rendered tree with the preset's.
#[gpui::test]
fn preset_switch_retrees_the_main_window(cx: &mut TestAppContext) {
    let main = open_workspace(cx);
    cx.run_until_parked();
    assert!(main_dock_panels(cx).contains(&PanelKind::Outliner));

    cx.dispatch_action(main.into(), workspace::WorkspaceColor);
    cx.run_until_parked();

    let panels = main_dock_panels(cx);
    assert!(
        panels.contains(&PanelKind::Waveform),
        "the Color preset's scopes must be rendered: {panels:?}"
    );
    assert!(
        !panels.contains(&PanelKind::Outliner),
        "panels the new preset does not lay out must be gone: {panels:?}"
    );
}
