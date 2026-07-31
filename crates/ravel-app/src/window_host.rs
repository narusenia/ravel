// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The uniform window host.
//!
//! Every Ravel window is the same construction: a title bar, one layout tree
//! rendered by [`ravel_dock::DockRoot`], and the modal layers
//! [`gpui_component::Root`] leaves to the host (without them an opened dialog
//! is live and invisible — the defect detached windows used to have).
//! [`WindowHost`] is that construction, addressed by the logical [`WindowId`]
//! of the window it renders.
//!
//! [`WindowRegistry`] maps logical window ids to the live GPUI handles for
//! *every* window, main window included, so window lifecycle (close follow,
//! minimize follow) and later cross-window drag hit-testing resolve through one
//! table.
//!
//! The main window still renders through [`crate::workspace::RavelWorkspace`]
//! and `gpui_component::dock` until the cutover; today only detached windows
//! are hosted here.

use std::cell::RefCell;
use std::collections::HashMap;

use gpui::*;
use gpui_component::{ActiveTheme as _, Root, TitleBar};
use ravel_dock::{DockEvent, DockRoot, PaneContent};
use ravel_i18n::t;
use ravel_ui::layout::{LayoutNode, PanelInstance, PanelInstanceId};
use ravel_ui::panel::PanelKind;
use ravel_ui::shell::AppShell;
use ravel_ui::window::WindowId;

use crate::panels;
use crate::workspace::MainWorkspace;

// ---------------------------------------------------------------------------
// Logical window id ↔ GPUI window handle
// ---------------------------------------------------------------------------

/// Live GPUI handles for the workspace's logical windows.
///
/// Durable shared state: the mapping exists for as long as the windows do. The
/// main window registers itself when [`crate::workspace::RavelWorkspace`] is
/// constructed, detached windows when [`open`] creates them, and every window
/// removes its entry when it closes — a stale handle in this table is the
/// desync `MED-APP-01` described.
#[derive(Default)]
pub struct WindowRegistry {
    handles: HashMap<WindowId, AnyWindowHandle>,
    main: Option<WindowId>,
}

impl Global for WindowRegistry {}

impl WindowRegistry {
    /// The handle of a logical window, if it is open.
    pub fn handle(&self, id: WindowId) -> Option<AnyWindowHandle> {
        self.handles.get(&id).copied()
    }

    /// The logical id of an open GPUI window, if it belongs to the workspace.
    pub fn window_id_of(&self, handle: AnyWindowHandle) -> Option<WindowId> {
        self.handles
            .iter()
            .find(|(_, open)| **open == handle)
            .map(|(id, _)| *id)
    }

    /// The main window's logical id, once it has registered.
    pub fn main(&self) -> Option<WindowId> {
        self.main
    }

    /// Every open window except the main one, ordered by logical id.
    pub fn detached(&self) -> Vec<(WindowId, AnyWindowHandle)> {
        let mut out: Vec<_> = self
            .handles
            .iter()
            .filter(|(id, _)| Some(**id) != self.main)
            .map(|(id, handle)| (*id, *handle))
            .collect();
        out.sort_by_key(|(id, _)| *id);
        out
    }

    /// Whether a logical window is currently open.
    pub fn contains(&self, id: WindowId) -> bool {
        self.handles.contains_key(&id)
    }

    /// Number of open windows in the table.
    pub fn len(&self) -> usize {
        self.handles.len()
    }

    /// Whether no window is registered.
    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }
}

/// Records the main window's handle under its logical id.
pub fn register_main(id: WindowId, handle: AnyWindowHandle, cx: &mut App) {
    let registry = cx.default_global::<WindowRegistry>();
    registry.main = Some(id);
    registry.handles.insert(id, handle);
}

/// Records a window handle under its logical id.
pub fn register(id: WindowId, handle: AnyWindowHandle, cx: &mut App) {
    cx.default_global::<WindowRegistry>()
        .handles
        .insert(id, handle);
}

/// Drops a window from the table, returning its handle if it was open.
pub fn unregister(id: WindowId, cx: &mut App) -> Option<AnyWindowHandle> {
    let registry = cx.default_global::<WindowRegistry>();
    if registry.main == Some(id) {
        registry.main = None;
    }
    registry.handles.remove(&id)
}

/// On-screen bounds of a logical window.
///
/// Cross-window tab drags resolve their drop target by hit-testing the cursor
/// against these.
pub fn window_bounds(id: WindowId, cx: &mut App) -> Option<Bounds<Pixels>> {
    let handle = cx.try_global::<WindowRegistry>()?.handle(id)?;
    handle.update(cx, |_root, window, _cx| window.bounds()).ok()
}

// ---------------------------------------------------------------------------
// Window lifecycle
// ---------------------------------------------------------------------------

/// Opens an OS window hosting the logical window `id`, rendering `root`.
///
/// `root` is passed in rather than read from the shell because the caller is
/// usually inside the workspace entity's own update.
pub fn open(id: WindowId, root: LayoutNode, cx: &mut App) {
    let title = window_title(&root);
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(640.0), px(480.0)),
                cx,
            ))),
            titlebar: Some({
                let mut options = TitleBar::title_bar_options();
                options.title = Some(title.into());
                options
            }),
            ..Default::default()
        },
        |window, cx| {
            let host = cx.new(|cx| WindowHost::new(id, root, window, cx));
            cx.new(|cx| Root::new(host, window, cx))
        },
    );
    match result {
        Ok(handle) => register(id, handle.into(), cx),
        Err(error) => {
            tracing::error!(%error, window = id.0, "failed to open a detached window");
        }
    }
}

/// Closes the OS window of a logical window without going through the shell.
///
/// Used for closes the model already decided (reattach, main-window follow):
/// the handle leaves the registry first, so the window's own close handler
/// recognizes the close as programmatic and does not touch the layout again.
pub fn close(id: WindowId, cx: &mut App) {
    let Some(handle) = unregister(id, cx) else {
        return;
    };
    // The close can be requested from the closing window itself (reattach
    // dispatched inside it), so that window may still be on the update stack;
    // updating it here would fail and leak the window. Defer past this cycle.
    cx.defer(move |cx| {
        if let Err(error) = handle.update(cx, |_root, window, _cx| window.remove_window()) {
            tracing::warn!(%error, "failed to close a detached window");
        }
    });
}

/// Closes every detached window. The main window closing takes its detached
/// windows with it.
pub fn close_all_detached(cx: &mut App) {
    let detached = cx
        .try_global::<WindowRegistry>()
        .map(WindowRegistry::detached)
        .unwrap_or_default();
    for (id, _) in detached {
        close(id, cx);
    }
}

/// Mirrors the main window's minimize state onto every detached window.
///
/// GPUI exposes no window-hiding primitive, so following means minimizing the
/// detached windows too and restoring them with the main window. The caller
/// re-activates the main window afterwards so restoring does not hand focus to
/// a detached window.
pub fn set_detached_minimized(minimized: bool, cx: &mut App) {
    let detached = cx
        .try_global::<WindowRegistry>()
        .map(WindowRegistry::detached)
        .unwrap_or_default();
    for (id, handle) in detached {
        let result = handle.update(cx, |_root, window, _cx| {
            if minimized {
                window.minimize_window();
            } else {
                window.activate_window();
            }
        });
        if let Err(error) = result {
            tracing::warn!(%error, window = id.0, "detached window is gone; dropping its handle");
            unregister(id, cx);
        }
    }
}

/// Handles the OS close button of a detached window.
///
/// The close is a model operation: the window leaves the layout with its
/// instances (multiple instances per panel make the discard lossless) and the
/// shell drops focus that pointed into it. Returning `true` lets the platform
/// close the window.
fn on_should_close(id: WindowId, cx: &mut App) -> bool {
    if unregister(id, cx).is_none() {
        // Already unregistered: the close was decided by the model (reattach,
        // main-window follow) and the layout is up to date.
        return true;
    }
    update_shell(cx, |shell| {
        if let Err(error) = shell.close_window(id) {
            tracing::warn!(%error, window = id.0, "closed window was not in the layout");
        }
    });
    true
}

// ---------------------------------------------------------------------------
// Shared session state
// ---------------------------------------------------------------------------

/// Runs `f` against the shared shell state and refreshes the menu bar.
///
/// The shell lives in the main window's workspace entity until the cutover;
/// panels, dialogs, and notifications in a hosted window resolve their state
/// through the durable globals instead, so this is the host's only dependency
/// on it.
fn update_shell<R>(cx: &mut App, f: impl FnOnce(&mut AppShell) -> R) -> Option<R> {
    let workspace = cx.try_global::<MainWorkspace>()?.workspace();
    let out = workspace
        .update(cx, |workspace, cx| {
            let out = f(&mut workspace.shell);
            cx.notify();
            out
        })
        .ok()?;
    if let Some(menus) = read_shell(cx, crate::workspace::build_menus) {
        cx.set_menus(menus);
    }
    Some(out)
}

/// Reads the shared shell state, if the workspace that owns it is alive.
fn read_shell<R>(cx: &App, f: impl FnOnce(&AppShell) -> R) -> Option<R> {
    let workspace = cx.try_global::<MainWorkspace>()?.workspace().upgrade()?;
    Some(f(workspace.read(cx).shell()))
}

// ---------------------------------------------------------------------------
// WindowHost
// ---------------------------------------------------------------------------

/// One window of the workspace: title bar, dock, dialog layer, notification
/// layer.
///
/// The host owns no session state. Panels resolve the document, playback, and
/// audio through their durable globals, so a hosted window renders without the
/// main window's workspace entity.
pub struct WindowHost {
    id: WindowId,
    dock: Entity<DockRoot>,
    focus_handle: FocusHandle,
    #[allow(dead_code)]
    dock_sub: Subscription,
}

impl WindowHost {
    /// Builds the host for logical window `id` rendering `root`.
    pub fn new(
        id: WindowId,
        root: LayoutNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let panes = std::rc::Rc::new(HostPanes::default());
        let dock = cx.new(|_cx| DockRoot::new(root, panes));
        let dock_sub = cx.subscribe(&dock, |this, _dock, event: &DockEvent, cx| {
            this.on_dock_event(event, cx);
        });
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        // The OS close button is the only user route out of this window; it
        // must reach the shell so the layout and the handle registry cannot
        // drift apart (MED-APP-01).
        window.on_window_should_close(cx, move |_window, cx| on_should_close(id, cx));
        Self {
            id,
            dock,
            focus_handle,
            dock_sub,
        }
    }

    /// The logical window this host renders.
    pub fn window_id(&self) -> WindowId {
        self.id
    }

    /// Applies a dock interaction to the shared layout and pushes the updated
    /// tree back — ravel-dock never writes the model itself.
    fn on_dock_event(&mut self, event: &DockEvent, cx: &mut Context<Self>) {
        let id = self.id;
        let event = event.clone();
        let updated = update_shell(cx, move |shell| {
            let window = shell.layout_mut().window_mut(id)?;
            let applied = match &event {
                DockEvent::SplitRatioChanged { path, ratio } => {
                    ravel_dock::set_ratio_at(&mut window.root, path, *ratio)
                }
                DockEvent::TabActivated { instance } => {
                    ravel_dock::activate_tab(&mut window.root, *instance)
                }
            };
            applied.then(|| window.root.clone())
        })
        .flatten();
        if let Some(root) = updated {
            self.dock.update(cx, |dock, cx| dock.set_layout(root, cx));
        }
    }

    /// Window title bar. Sharing one component with the main window's bar
    /// (and the always-on-top pin) is DOCK-7's work; this is the minimum that
    /// keeps a hosted window's chrome consistent with the main window's.
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = window_title(self.dock.read(cx).layout());
        TitleBar::new().child(
            div()
                .id("window-host-title")
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().colors.muted_foreground)
                        .child(title),
                ),
        )
    }
}

impl Render for WindowHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // `Root` renders the view, the tooltip, and the native menu overlay,
        // but the modal layers are the host's to place: without these children
        // an opened Dialog is live and invisible.
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .track_focus(&self.focus_handle)
            .child(self.render_title_bar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.dock.clone()),
            )
            .children(dialog_layer)
            .children(notification_layer)
    }
}

// ---------------------------------------------------------------------------
// Pane contents
// ---------------------------------------------------------------------------

/// One cached pane view, keyed by the instance it belongs to.
struct CachedPane {
    kind: PanelKind,
    view: AnyView,
}

/// Supplies pane contents for a hosted window.
///
/// [`PaneContent::view`] must return a stable view per instance id: the cache
/// is what keeps a pane's view state (scroll, zoom, selection) alive across
/// tab switches, splitter drags, and every other re-render.
#[derive(Default)]
struct HostPanes {
    views: RefCell<HashMap<PanelInstanceId, CachedPane>>,
}

impl PaneContent for HostPanes {
    fn tab_title(&self, instance: &PanelInstance, _window: &Window, _cx: &App) -> SharedString {
        panels::panel_display_name(instance.kind).into()
    }

    fn view(&self, instance: &PanelInstance, window: &mut Window, cx: &mut App) -> AnyView {
        let cached = self
            .views
            .borrow()
            .get(&instance.id)
            .filter(|cached| cached.kind == instance.kind)
            .map(|cached| cached.view.clone());
        if let Some(view) = cached {
            return view;
        }
        let view = panels::panel_for_kind(instance.kind, window, cx).view();
        self.views.borrow_mut().insert(
            instance.id,
            CachedPane {
                kind: instance.kind,
                view: view.clone(),
            },
        );
        view
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Title for a window showing `root`: the active pane's panel name, falling
/// back to the application name for an empty tree. Multi-tab window titles are
/// DOCK-7's.
fn window_title(root: &LayoutNode) -> String {
    active_instance(root)
        .map(|instance| panels::panel_display_name(instance.kind))
        .unwrap_or_else(|| t!("app.title"))
}

/// The active instance of the tree's first area, in tree order.
fn active_instance(root: &LayoutNode) -> Option<PanelInstance> {
    match root {
        LayoutNode::Area { tabs, active } => tabs.get(*active).or_else(|| tabs.first()).cloned(),
        LayoutNode::Split { first, second, .. } => {
            active_instance(first).or_else(|| active_instance(second))
        }
    }
}

#[cfg(test)]
mod tests {
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back
    // to the built-in one so `#[test]` resolves to the real one.
    use core::prelude::v1::test;

    use ravel_ui::layout::{LayoutNode, Orientation, PanelInstance, PanelInstanceId};
    use ravel_ui::panel::PanelKind;

    fn instance(id: u64, kind: PanelKind) -> PanelInstance {
        PanelInstance::new(PanelInstanceId(id), kind)
    }

    #[test]
    fn active_instance_prefers_the_active_tab_of_the_first_area() {
        let mut area = LayoutNode::area(vec![
            instance(0, PanelKind::Viewer),
            instance(1, PanelKind::Timeline),
        ]);
        if let LayoutNode::Area { active, .. } = &mut area {
            *active = 1;
        }
        let tree = LayoutNode::split(
            Orientation::Horizontal,
            0.5,
            area,
            LayoutNode::area(vec![instance(2, PanelKind::Outliner)]),
        );
        assert_eq!(
            super::active_instance(&tree).map(|i| i.kind),
            Some(PanelKind::Timeline)
        );
    }

    #[test]
    fn empty_tree_has_no_active_instance() {
        assert!(super::active_instance(&LayoutNode::area(Vec::new())).is_none());
    }
}
