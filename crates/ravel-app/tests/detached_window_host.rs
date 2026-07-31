// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Detached window lifecycle against the uniform window host.
//!
//! These cover the shell-visible half of the multi-window host: the handle
//! registry, the OS close button routing through `AppShell::close_window`
//! (MED-APP-01), and panel views surviving a detach round trip. Minimize
//! follow and the dialog layers need a real platform window and are verified
//! on device.

use gpui::{
    InteractiveElement as _, ParentElement as _, SharedString, TestAppContext, VisualTestContext,
};
use gpui_component::WindowExt as _;
use gpui_component::button::Button;
use ravel_app::panels;
use ravel_app::trace;
use ravel_app::window_host::WindowRegistry;
use ravel_app::workspace::{self, MainWorkspace, RavelWorkspace};
use ravel_ui::command::CommandId;
use ravel_ui::panel::PanelKind;
use ravel_ui::shell::AppShell;
use std::time::Duration;

fn init_i18n() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    let _ = ravel_i18n::init(&dir, "en");
}

/// Builds a real `RavelWorkspace` window. Panels needing a GPU or media
/// backend (NodeGraph) are toggled invisible first so the test stays headless.
fn open_workspace(cx: &mut TestAppContext) -> gpui::WindowHandle<RavelWorkspace> {
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

    let window = cx.add_window(move |window, cx| RavelWorkspace::new(shell, window, cx));
    cx.update(|cx| {
        let workspace = window
            .entity(cx)
            .expect("workspace window should have a root entity");
        cx.set_global(MainWorkspace::new(window.into(), workspace.downgrade()));
    });
    window
}

/// Detaches the Viewer out of the main window and returns the detached
/// window's logical id and GPUI handle.
fn detach_viewer(
    cx: &mut TestAppContext,
    main: gpui::WindowHandle<RavelWorkspace>,
) -> (ravel_ui::window::WindowId, gpui::AnyWindowHandle) {
    cx.update(|cx| cx.set_global(panels::FocusedPanelGlobal(Some(PanelKind::Viewer))));
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
    let (windows, viewer_in_main) = cx.update(|cx| {
        let shell = main.entity(cx).unwrap().read(cx).shell().clone();
        (
            shell.layout().windows().len(),
            shell.visibility().is_visible(PanelKind::Viewer),
        )
    });
    assert_eq!(windows, 2, "detach must add a window to the layout");
    assert!(!viewer_in_main, "the Viewer left the main window");

    // The user clicks the detached window's close button.
    let mut detached_cx = VisualTestContext::from_window(detached, cx);
    assert!(
        detached_cx.simulate_close(),
        "the detached window must accept the close"
    );
    cx.run_until_parked();

    let (windows, has_handle, detached_left, focused) = cx.update(|cx| {
        let registry = cx.global::<WindowRegistry>();
        let (has_handle, detached_left) =
            (registry.contains(detached_id), registry.detached().len());
        let shell = main.entity(cx).unwrap().read(cx).shell().clone();
        (
            shell.layout().windows().len(),
            has_handle,
            detached_left,
            shell.focused_instance(),
        )
    });
    assert_eq!(windows, 1, "the closed window must leave the layout");
    assert!(!has_handle, "no stale handle may stay in the registry");
    assert_eq!(detached_left, 0);
    assert_eq!(
        focused, None,
        "focus pointing into the closed window must be dropped"
    );
    // The instances went with the window; the panel is toggleable again.
    let visible = cx.update(|cx| {
        main.entity(cx)
            .unwrap()
            .read(cx)
            .shell()
            .visibility()
            .is_visible(PanelKind::Viewer)
    });
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

    let (windows, has_handle, viewer_in_main) = cx.update(|cx| {
        let registry = cx.global::<WindowRegistry>();
        let has_handle = registry.contains(detached_id);
        let shell = main.entity(cx).unwrap().read(cx).shell().clone();
        (
            shell.layout().windows().len(),
            has_handle,
            shell.visibility().is_visible(PanelKind::Viewer),
        )
    });
    assert_eq!(windows, 1, "reattach absorbed the detached window");
    assert!(!has_handle);
    assert!(viewer_in_main, "the Viewer is back in the main window");
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
    let before = cx.update(|cx| {
        let workspace = main.entity(cx).unwrap();
        let workspace = workspace.read(cx);
        observed.map(|kind| workspace.panel_view_id(kind, cx))
    });
    assert!(
        before.iter().all(Option::is_some),
        "the preset's panels must be built before the round trip: {before:?}"
    );

    let (_id, detached) = detach_viewer(cx, main);
    cx.dispatch_action(detached, workspace::PanelReattach);
    cx.run_until_parked();

    let after = cx.update(|cx| {
        let workspace = main.entity(cx).unwrap();
        let workspace = workspace.read(cx);
        observed.map(|kind| workspace.panel_view_id(kind, cx))
    });
    assert_eq!(
        before, after,
        "detach/reattach must reuse the cached panel views, not rebuild them"
    );
}
