// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The uniform window host.
//!
//! Every Ravel window is the same construction: a title bar, one layout tree
//! rendered by [`ravel_dock::DockRoot`], the command action handlers, and the
//! modal layers [`gpui_component::Root`] leaves to the host (without them an
//! opened dialog is live and invisible — the defect detached windows used to
//! have). [`WindowHost`] is that construction, addressed by the logical
//! [`WindowId`] of the window it renders; only the title bar's slots and the
//! close behaviour differ between the main window and a detached one.
//!
//! [`WindowRegistry`] maps logical window ids to the live GPUI handles and
//! hosts for *every* window, main window included, so window lifecycle (close
//! follow, minimize follow) and cross-window drag hit-testing resolve through
//! one table.
//!
//! No host owns session state. The shared [`AppShell`], document, playback, and
//! audio live in [`crate::workspace::RavelWorkspace`]; each host observes it,
//! re-renders the tree the shell now holds for its window, and routes command
//! actions back into it.

use std::collections::HashMap;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Icon, Root, Selectable as _, Sizable as _, TitleBar, WindowExt as _};
use ravel_dock::{DockEvent, DockRoot};
use ravel_i18n::t;
use ravel_ui::layout::{LayoutNode, PanelInstance, PanelInstanceId, WindowLayout};
use ravel_ui::panel::PanelKind;
use ravel_ui::shell::AppShell;
use ravel_ui::window::{WindowId, WindowPlacement};

use crate::assets::RavelIcon;
use crate::panels;
use crate::title_bar::RavelTitleBar;
use crate::workspace::{MainWorkspace, RavelWorkspace};

/// Size the main window opens at when no placement has been restored.
const MAIN_WINDOW_SIZE: Size<Pixels> = Size {
    width: px(1280.0),
    height: px(800.0),
};

/// Size a detached window opens at.
const DETACHED_WINDOW_SIZE: Size<Pixels> = Size {
    width: px(640.0),
    height: px(480.0),
};

/// Where a window opens: at its restored placement when one was recorded and
/// still describes an openable window, otherwise centered at `fallback`.
///
/// A persisted placement is plain text a user can edit, so it is only trusted
/// after [`WindowPlacement::is_usable`] — an off-screen or zero-sized record
/// would otherwise open a window nobody can reach (`LOW-APP-14`).
fn window_bounds_for(
    placement: Option<WindowPlacement>,
    fallback: Size<Pixels>,
    cx: &mut App,
) -> WindowBounds {
    let restored = placement
        .filter(WindowPlacement::is_usable)
        .map(|placement| Bounds {
            origin: point(px(placement.x), px(placement.y)),
            size: size(px(placement.width), px(placement.height)),
        })
        .filter(|bounds| on_a_connected_display(*bounds, cx));
    match restored {
        Some(bounds) => WindowBounds::Windowed(bounds),
        None => WindowBounds::Windowed(Bounds::centered(None, fallback, cx)),
    }
}

/// Whether `bounds` overlaps a display that is currently connected.
///
/// `is_usable` only says the record describes a window that could exist; it
/// cannot know about screens. A placement saved on an external monitor that has
/// since been unplugged is finite and large enough yet lands nowhere the user
/// can see or grab, so it is refused like any other unusable record. Any
/// overlap counts: a window hanging off an edge is still reachable.
fn on_a_connected_display(bounds: Bounds<Pixels>, cx: &App) -> bool {
    let displays = cx.displays();
    // No display information (headless, or a platform that reports none) is not
    // evidence against the placement.
    displays.is_empty()
        || displays
            .iter()
            .any(|display| display.bounds().intersects(&bounds))
}

/// Which window of the workspace a host renders.
///
/// The two roles share the whole frame; they differ in the title bar's slots
/// (only a detached window carries the always-on-top pin) and in what closing
/// the window means (the main window quits the session, a detached window is a
/// layout operation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowRole {
    /// `windows[0]` of the layout: the window the session's document lives in.
    Main,
    /// A window created by detaching panel instances out of another one.
    Detached,
}

// ---------------------------------------------------------------------------
// Logical window id ↔ GPUI window handle
// ---------------------------------------------------------------------------

/// One open window of the workspace.
struct OpenWindow {
    handle: AnyWindowHandle,
    host: WeakEntity<WindowHost>,
}

/// Live GPUI handles for the workspace's logical windows.
///
/// Durable shared state: the mapping exists for as long as the windows do. The
/// main window registers itself when [`main_root`] builds it, detached windows
/// when [`open`] creates them, and every window removes its entry when it
/// closes — a stale handle in this table is the desync `MED-APP-01` described.
#[derive(Default)]
pub struct WindowRegistry {
    windows: HashMap<WindowId, OpenWindow>,
    main: Option<WindowId>,
}

impl Global for WindowRegistry {}

impl WindowRegistry {
    /// The handle of a logical window, if it is open.
    pub fn handle(&self, id: WindowId) -> Option<AnyWindowHandle> {
        self.windows.get(&id).map(|open| open.handle)
    }

    /// The host rendering a logical window, if it is open.
    pub fn host(&self, id: WindowId) -> Option<WeakEntity<WindowHost>> {
        self.windows.get(&id).map(|open| open.host.clone())
    }

    /// The logical id of an open GPUI window, if it belongs to the workspace.
    pub fn window_id_of(&self, handle: AnyWindowHandle) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|(_, open)| open.handle == handle)
            .map(|(id, _)| *id)
    }

    /// The main window's logical id, once it has registered.
    pub fn main(&self) -> Option<WindowId> {
        self.main
    }

    /// Every open window except the main one, ordered by logical id.
    pub fn detached(&self) -> Vec<(WindowId, AnyWindowHandle)> {
        let mut out: Vec<_> = self
            .windows
            .iter()
            .filter(|(id, _)| Some(**id) != self.main)
            .map(|(id, open)| (*id, open.handle))
            .collect();
        out.sort_by_key(|(id, _)| *id);
        out
    }

    /// Every open window, ordered by logical id.
    fn all(&self) -> Vec<WindowId> {
        let mut out: Vec<_> = self.windows.keys().copied().collect();
        out.sort();
        out
    }

    /// Whether a logical window is currently open.
    pub fn contains(&self, id: WindowId) -> bool {
        self.windows.contains_key(&id)
    }

    /// Number of open windows in the table.
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// Whether no window is registered.
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
}

/// Records a window's handle and host under its logical id.
fn register(id: WindowId, handle: AnyWindowHandle, host: WeakEntity<WindowHost>, cx: &mut App) {
    cx.default_global::<WindowRegistry>()
        .windows
        .insert(id, OpenWindow { handle, host });
}

/// Drops a window from the table, returning its handle if it was open.
pub fn unregister(id: WindowId, cx: &mut App) -> Option<AnyWindowHandle> {
    let registry = cx.default_global::<WindowRegistry>();
    if registry.main == Some(id) {
        registry.main = None;
    }
    registry.windows.remove(&id).map(|open| open.handle)
}

/// On-screen bounds of a logical window.
///
/// Cross-window tab drags resolve their drop target by hit-testing the cursor
/// against these. Reading them updates the window, so this must not be called
/// from inside another window's update — see [`drop_dragged_tab`].
pub fn window_bounds(id: WindowId, cx: &mut App) -> Option<Bounds<Pixels>> {
    let handle = cx.try_global::<WindowRegistry>()?.handle(id)?;
    handle.update(cx, |_root, window, _cx| window.bounds()).ok()
}

// ---------------------------------------------------------------------------
// Window lifecycle
// ---------------------------------------------------------------------------

/// Opens the main window around a fresh session.
///
/// The main window opens at the placement the shell's layout carries, which is
/// the one restored from `layout.toml` when a previous session recorded it.
///
/// Returns the window handle, or the platform error when the window was
/// refused — the caller has nothing left to run in that case.
pub fn open_main(shell: AppShell, cx: &mut App) -> anyhow::Result<WindowHandle<Root>> {
    let window_bounds =
        window_bounds_for(shell.layout().main_window().placement, MAIN_WINDOW_SIZE, cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(window_bounds),
            titlebar: Some({
                let mut options = TitleBar::title_bar_options();
                options.title = Some(t!("app.title").into());
                options
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| main_root(shell, window, cx)),
    )
}

/// Builds the main window's root: the session, the globals that reach it, and
/// the host rendering the layout's main window.
///
/// Split out of [`open_main`] so tests can put the same construction inside a
/// test window.
pub fn main_root(shell: AppShell, window: &mut Window, cx: &mut Context<Root>) -> Root {
    let id = shell.layout().main_window().id;
    let root = shell.layout().main_window().root.clone();
    let session = cx.new(|cx| RavelWorkspace::new(shell, window, cx));
    // The session has to be reachable before the host is built: the host
    // observes it and renders the project name from it.
    cx.set_global(MainWorkspace::new(
        window.window_handle(),
        session.downgrade(),
    ));
    let host = cx.new(|cx| {
        WindowHost::new(
            HostSpec {
                id,
                root,
                always_on_top: false,
                role: WindowRole::Main,
                session: Some(session),
            },
            window,
            cx,
        )
    });
    // Every window of the workspace is in the handle registry, main included:
    // window lifecycle and cross-window drops resolve there.
    register(id, window.window_handle(), host.downgrade(), cx);
    cx.default_global::<WindowRegistry>().main = Some(id);
    root_with_app_font(host, window, cx)
}

/// Wraps a host in the gpui-component [`Root`] and gives the whole window the
/// Japanese font fallback.
///
/// It goes on the `Root` itself, not on the workspace below it: `Root` refines
/// its own style *after* applying the theme's family, and its tooltip and menu
/// overlays are its children rather than the host's. Styling anything lower
/// would leave those overlays falling back to the platform's Japanese face
/// instead of the bundled one.
///
/// Only the fallback is set, never the family. That keeps `Root`'s per-render
/// `font_family(cx.theme().font_family)` authoritative, so a theme switch or a
/// hot-reload still moves the whole window; a family captured here would
/// freeze at whatever the theme said when the window opened.
fn root_with_app_font(
    host: Entity<WindowHost>,
    window: &mut Window,
    cx: &mut Context<Root>,
) -> Root {
    let mut root = Root::new(host, window, cx);
    root.text_style().font_fallbacks = Some(crate::fonts::japanese_fallbacks());
    root
}

/// Opens an OS window hosting the logical window `layout`.
///
/// The window's model row is passed in rather than read from the shell because
/// the caller is usually inside the session entity's own update. Its
/// `always_on_top` flag and its `placement` are applied at open time: a
/// restored layout can hold windows that were pinned and positioned in an
/// earlier session.
///
/// Returns `false` when the platform refused the window. The layout then holds
/// a window nothing renders, so the caller has to put the instances back —
/// otherwise they are in no window at all and no close button can recover them.
#[must_use]
pub fn open(layout: &WindowLayout, cx: &mut App) -> bool {
    let id = layout.id;
    let root = layout.root.clone();
    let always_on_top = layout.always_on_top;
    let title = window_title(&root);
    let window_bounds = window_bounds_for(layout.placement, DETACHED_WINDOW_SIZE, cx);
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(window_bounds),
            titlebar: Some({
                let mut options = TitleBar::title_bar_options();
                options.title = Some(title.into());
                options
            }),
            ..Default::default()
        },
        |window, cx| {
            let host = cx.new(|cx| {
                WindowHost::new(
                    HostSpec {
                        id,
                        root,
                        always_on_top,
                        role: WindowRole::Detached,
                        session: None,
                    },
                    window,
                    cx,
                )
            });
            register(id, window.window_handle(), host.downgrade(), cx);
            cx.new(|cx| root_with_app_font(host, window, cx))
        },
    );
    match result {
        Ok(_handle) => true,
        Err(error) => {
            tracing::error!(%error, window = id.0, "failed to open a detached window");
            false
        }
    }
}

/// Opens the windows a restored layout brought with it (`layout.toml`, or a
/// project's embedded layout).
///
/// A window the platform refuses is absorbed back into the main tree: its panes
/// would otherwise live in a window nothing renders, with no close button to
/// recover them from — the same failure [`open`] guards against for detach.
pub fn open_restored(windows: &[WindowLayout], cx: &mut App) {
    for window in windows {
        if open(window, cx) {
            continue;
        }
        let id = window.id;
        update_shell(cx, move |shell| {
            if let Err(error) = shell.layout_mut().absorb_window(id) {
                tracing::warn!(
                    %error,
                    window = id.0,
                    "a refused restored window was not in the layout"
                );
            }
        });
    }
}

/// Closes the OS window of a logical window without going through the shell.
///
/// Used for closes the model already decided (reattach, main-window follow, a
/// window whose last area was moved away): the handle leaves the registry
/// first, so the window's own close handler recognizes the close as
/// programmatic and does not touch the layout again.
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

/// Gives the pane of `instance` the keyboard focus, wherever it is docked.
///
/// This is how a command that opened a panel hands it over: real GPUI focus is
/// the single source of truth for which panel is active — `FocusedPanelGlobal`
/// follows the focus events and the shell is re-synced from it before every
/// dispatch — so the host moves the focus and lets the resulting event update
/// both. Nothing writes a "focused" flag on the side.
///
/// The pane may sit in a window other than the one the command came from, and
/// the caller is inside a window's update either way, where updating a window
/// fails; the move is deferred past this cycle. The pane's view is built on
/// demand, so an instance the window has not rendered yet still takes the
/// focus and keeps it when the tree that holds it is drawn.
pub fn focus_pane(instance: PanelInstanceId, cx: &mut App) {
    cx.defer(move |cx| {
        let Some((window_id, instance)) =
            read_shell(cx, |shell| shell.layout().find_instance(instance)).flatten()
        else {
            return;
        };
        let Some((handle, host)) = cx
            .try_global::<WindowRegistry>()
            .and_then(|registry| Some((registry.handle(window_id)?, registry.host(window_id)?)))
        else {
            return;
        };
        let result = handle.update(cx, |_root, window, cx| {
            // A dialog is on top of the panes and owns the keyboard while it
            // is up. The panel still opens behind it — only the focus move is
            // dropped, so typing in the dialog is not yanked away mid-edit.
            // The pane is reachable by clicking it once the dialog closes.
            if window.has_active_dialog(cx) {
                return;
            }
            let panes = host.upgrade().map(|host| host.read(cx).panes());
            if let Some(panes) = panes {
                panes.focus_pane(&instance, window, cx);
            }
        });
        if let Err(error) = result {
            tracing::warn!(%error, window = window_id.0, "failed to focus an opened pane");
        }
    });
}

/// Mirrors the main window's minimize state onto every detached window.
///
/// GPUI exposes no window-hiding primitive, so following means minimizing the
/// detached windows too and restoring them with the main window.
pub fn set_detached_minimized(minimized: bool, cx: &mut App) {
    let (detached, main) = match cx.try_global::<WindowRegistry>() {
        Some(registry) => (
            registry.detached(),
            registry.main().and_then(|id| registry.handle(id)),
        ),
        None => return,
    };
    if detached.is_empty() {
        return;
    }
    // The caller observes the main window from inside its update, where a
    // nested update of another window fails; defer past this cycle.
    cx.defer(move |cx| {
        for (id, handle) in detached {
            let result = handle.update(cx, |_root, window, _cx| {
                if minimized {
                    window.minimize_window();
                } else {
                    window.activate_window();
                }
            });
            if let Err(error) = result {
                tracing::warn!(%error, window = id.0, "failed to follow the main window");
            }
        }
        // Restoring must not hand the keyboard to a detached window: the main
        // window is the one the user brought back.
        if !minimized
            && let Some(main) = main
            && let Err(error) = main.update(cx, |_root, window, _cx| window.activate_window())
        {
            tracing::warn!(%error, "failed to re-activate the main window");
        }
    });
}

/// A tab dragged out of its window: the layout already moved it into a window
/// of its own, which the host still has to open.
struct PendingDetach {
    /// The window the layout created (its tree is the dragged tab alone).
    window: WindowLayout,
    /// The dragged instance, so a refused window can be moved back.
    instance: PanelInstanceId,
}

/// Applies one dock interaction of window `id` to the shared layout.
///
/// Pure model work: opening and closing OS windows is the caller's, because
/// this runs inside the session entity's update. Detach requests are resolved
/// separately ([`drop_dragged_tab`]) — they need window bounds, which are only
/// readable outside another window's update.
fn apply_dock_event(shell: &mut AppShell, id: WindowId, event: &DockEvent) {
    let layout = shell.layout_mut();
    match event {
        DockEvent::SplitRatioChanged { path, ratio } => {
            if let Some(window) = layout.window_mut(id) {
                ravel_dock::set_ratio_at(&mut window.root, path, *ratio);
            }
        }
        DockEvent::TabActivated { instance } => {
            if let Some(window) = layout.window_mut(id) {
                ravel_dock::activate_tab(&mut window.root, *instance);
            }
        }
        DockEvent::TabDropped {
            instance,
            anchor,
            zone,
        } => {
            report(ravel_dock::apply_tab_drop(
                layout, id, *instance, *anchor, *zone,
            ));
        }
        DockEvent::AreaActionRequested { instance, action } => {
            report(ravel_dock::apply_area_action(
                layout, id, *instance, *action,
            ));
        }
        // Resolved outside the shell update; see `drop_dragged_tab`.
        DockEvent::TabDetachRequested { .. } => {}
    }
}

/// Logs a rejected layout operation and reports whether it applied.
fn report(result: Result<(), ravel_ui::layout::LayoutError>) -> bool {
    if let Err(error) = result {
        tracing::warn!(%error, "dock interaction was rejected by the layout");
        return false;
    }
    true
}

/// Runs `f` after the current update cycle, with the application context.
///
/// Reading another window (its bounds) or opening one fails from inside a
/// window's update, so the interactions that need to are deferred through here.
fn defer_app(cx: &mut App, f: impl FnOnce(&mut App) + 'static) {
    cx.defer(f);
}

/// The workspace window whose on-screen bounds contain `position`, excluding
/// `source`.
///
/// GPUI exposes no stacking order, so overlapping candidates are resolved by
/// preferring the most recently created window (the highest logical id) — the
/// one the user most likely just placed there.
fn window_under(position: Point<Pixels>, source: WindowId, cx: &mut App) -> Option<WindowId> {
    let candidates = cx
        .try_global::<WindowRegistry>()
        .map(WindowRegistry::all)
        .unwrap_or_default();
    candidates
        .into_iter()
        .rev()
        .filter(|id| *id != source)
        .find(|id| window_bounds(*id, cx).is_some_and(|bounds| bounds.contains(&position)))
}

/// Resolves where a tab dragged out of window `source` was released and moves
/// it there: into the workspace window under the pointer, or into a new window
/// of its own.
///
/// Dragging the only tab of a detached window onto the desktop would destroy
/// that window and rebuild the same thing next to it, so it is left alone.
fn drop_dragged_tab(
    source: WindowId,
    instance: PanelInstanceId,
    position: Point<Pixels>,
    cx: &mut App,
) {
    if let Some(target) = window_under(position, source, cx) {
        // Without the target window's area geometry the tab joins that
        // window's first area; a precise landing needs the pointer to be
        // inside the window, which is the ordinary `TabDropped` path.
        let anchor = read_shell(cx, |shell| {
            shell
                .layout()
                .window(target)
                .and_then(|window| window.root.instances().first().map(|tab| tab.id))
        })
        .flatten();
        if let Some(anchor) = anchor {
            update_shell(cx, |shell| {
                report(shell.layout_mut().move_tab(instance, target, anchor));
            });
            return;
        }
    }
    // A lone tab in a detached window is already a window of its own.
    let is_main = cx
        .try_global::<WindowRegistry>()
        .and_then(WindowRegistry::main)
        == Some(source);
    let alone = read_shell(cx, |shell| {
        shell
            .layout()
            .window(source)
            .is_some_and(|window| window.root.instances().len() < 2)
    })
    .unwrap_or(true);
    if alone && !is_main {
        return;
    }
    let detached = update_shell(cx, |shell| {
        match shell.layout_mut().detach_to_window(instance) {
            Ok(new_id) => shell.layout().window(new_id).cloned(),
            Err(error) => {
                tracing::warn!(%error, "dragged-out tab could not become a window");
                None
            }
        }
    })
    .flatten();
    if let Some(window) = detached {
        open_detached_or_return(PendingDetach { window, instance }, source, cx);
    }
}

/// Opens the window a dragged-out tab was moved into, or moves the tab back to
/// `source` when the platform refuses the window.
///
/// Without the fallback the instance would live in a window nothing renders,
/// with no close button to recover it from.
fn open_detached_or_return(detach: PendingDetach, source: WindowId, cx: &mut App) {
    if open(&detach.window, cx) {
        return;
    }
    update_shell(cx, |shell| {
        let layout = shell.layout_mut();
        let anchor = layout
            .window(source)
            .and_then(|window| window.root.instances().first().map(|tab| tab.id));
        // Moving the tab back empties the refused window, which the model then
        // drops on its own.
        let returned =
            anchor.is_some_and(|anchor| report(layout.move_tab(detach.instance, source, anchor)));
        if !returned {
            report(layout.close_window(detach.window.id));
        }
    });
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
/// Every host reaches the session through here; the hosts then re-render from
/// their own observation of it, so no caller has to push trees around.
fn update_shell<R>(cx: &mut App, f: impl FnOnce(&mut AppShell) -> R) -> Option<R> {
    let session = crate::workspace::session(cx)?;
    let out = session.update(cx, |session, cx| {
        let out = f(&mut session.shell);
        cx.notify();
        crate::workspace::install_menus(&session.shell, cx);
        out
    });
    Some(out)
}

/// Reads the shared shell state, if the session that owns it is alive.
fn read_shell<R>(cx: &App, f: impl FnOnce(&AppShell) -> R) -> Option<R> {
    let session = crate::workspace::session(cx)?;
    Some(f(session.read(cx).shell()))
}

// ---------------------------------------------------------------------------
// WindowHost
// ---------------------------------------------------------------------------

/// What a host needs to render one window.
struct HostSpec {
    /// The logical window this host renders.
    id: WindowId,
    /// Its layout tree at construction time.
    root: LayoutNode,
    /// Whether the window floats above the others.
    always_on_top: bool,
    /// Which window of the workspace this is.
    role: WindowRole,
    /// The session, for the main window: it owns it, so the document, the
    /// playback clock, and the audio engine live exactly as long as the main
    /// window does. Detached hosts resolve the same session weakly.
    session: Option<Entity<RavelWorkspace>>,
}

/// One window of the workspace: title bar, dock, dialog layer, notification
/// layer, and the command action handlers.
///
/// The host owns no session state. It observes
/// [`crate::workspace::RavelWorkspace`] for the tree the shell now holds for
/// its window; panels resolve the document, playback, and audio through their
/// durable globals.
pub struct WindowHost {
    id: WindowId,
    role: WindowRole,
    /// The main window's strong reference to the session (see
    /// [`HostSpec::session`]). `None` in a detached window.
    #[allow(dead_code)]
    session: Option<Entity<RavelWorkspace>>,
    dock: Entity<DockRoot>,
    panes: std::rc::Rc<panels::PanelViews>,
    focus_handle: FocusHandle,
    /// Last title written to the OS window. The platform window list keeps the
    /// title it was opened with, so it has to be rewritten when the active tab
    /// changes; comparing against this keeps that to actual changes.
    os_title: String,
    /// The main window's centered label: the open project's display name. Kept
    /// in sync from the session so rendering stays a pure read.
    project_name: String,
    /// Whether this window floats above the others. The layout owns the state;
    /// this is the copy the pin renders from.
    always_on_top: bool,
    #[allow(dead_code)]
    dock_sub: Subscription,
    #[allow(dead_code)]
    session_sub: Option<Subscription>,
    #[allow(dead_code)]
    focus_sub: Subscription,
    /// Keeps this window's row in the layout carrying its live on-desktop
    /// bounds, so the next save writes where the window actually is.
    #[allow(dead_code)]
    bounds_sub: Subscription,
}

impl WindowHost {
    /// Builds the host for one logical window.
    ///
    /// `always_on_top` is the layout's flag for this window; applying it here
    /// is the only point where a window restored as pinned gets its level.
    fn new(spec: HostSpec, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let HostSpec {
            id,
            root,
            always_on_top,
            role,
            session: owned_session,
        } = spec;
        let panes = std::rc::Rc::new(panels::PanelViews::default());
        let os_title = window_title(&root);
        // The pane a detached window is opened around, so the window can hand
        // it the focus below.
        let opened_around = (role == WindowRole::Detached)
            .then(|| active_instance(&root))
            .flatten();
        let dock = cx.new(|cx| DockRoot::new(root, panes.clone(), cx));
        let dock_sub = cx.subscribe_in(
            &dock,
            window,
            |this, _dock, event: &DockEvent, window, cx| {
                this.on_dock_event(event, window, cx);
            },
        );
        // The shell owns the effective layout, so every change to it — a View
        // toggle, a preset switch, a drop in another window — arrives as a
        // notification from the session rather than as a call into this host.
        let session = owned_session
            .clone()
            .or_else(|| crate::workspace::session(cx));
        let session_sub = session.as_ref().map(|session| {
            cx.observe_in(session, window, |this, session, window, cx| {
                this.sync_from_session(&session, window, cx);
            })
        });
        // Only the main window's bar shows the project, and only that host is
        // built outside the session's own update — a detached window is opened
        // from inside it, where reading the session would panic.
        let project_name = match role {
            WindowRole::Main => session
                .as_ref()
                .map(|session| project_display_name(session, cx))
                .unwrap_or_else(|| crate::title_bar::project_display_name(None)),
            WindowRole::Detached => String::new(),
        };
        // Tab icons mark which pane holds the focus, and the tab bars are the
        // dock's to draw, so the frame repaints when the focus moves.
        let focus_sub = cx.observe_global::<panels::FocusedPanelGlobal>(|_this, cx| {
            cx.notify();
        });
        // Where the window sits is part of the workspace arrangement
        // (`LOW-APP-14`). Recording it as it moves keeps the model truthful
        // without any I/O per frame — the layout is written out at command and
        // shutdown boundaries instead.
        let bounds_sub = cx.observe_window_bounds(window, move |_this, window, cx| {
            let bounds = window.bounds();
            cx.defer(move |cx| crate::layout_persist::record_placement(id, bounds, cx));
        });
        let focus_handle = cx.focus_handle();
        // A detached window opens *around* a pane, so that pane takes the
        // focus — not the frame. `FocusedPanelGlobal` follows real focus
        // events, so a frame that kept the focus would leave the workspace with
        // no focused instance: `Cmd+Shift+R` right after `Cmd+Shift+D` would
        // find nothing to reattach until the user clicked the pane.
        match &opened_around {
            Some(instance) => panes.focus_pane(instance, window, cx),
            None => focus_handle.focus(window, cx),
        }
        if role == WindowRole::Detached {
            // The OS close button is the only user route out of a detached
            // window; it must reach the shell so the layout and the handle
            // registry cannot drift apart (MED-APP-01). The main window's own
            // close is the session's unsaved-changes guard.
            window.on_window_should_close(cx, move |_window, cx| on_should_close(id, cx));
        }
        // Only raise: on macOS `set_always_on_top(false)` writes
        // `NSNormalWindowLevel` unconditionally, so leaving an unpinned window
        // alone keeps whatever level it was created with.
        if always_on_top {
            window.set_always_on_top(true);
        }
        Self {
            id,
            role,
            session: owned_session,
            dock,
            panes,
            focus_handle,
            os_title,
            project_name,
            always_on_top,
            dock_sub,
            session_sub,
            focus_sub,
            bounds_sub,
        }
    }

    /// The logical window this host renders.
    pub fn window_id(&self) -> WindowId {
        self.id
    }

    /// This window's pane views. Handed out as a shared handle so a caller
    /// already holding the window can build or focus a pane without keeping a
    /// borrow of the host itself.
    fn panes(&self) -> std::rc::Rc<panels::PanelViews> {
        self.panes.clone()
    }

    /// The layout tree this window is currently rendering (exposed for tests:
    /// it is what the user sees, as opposed to what the model holds).
    pub fn rendered_tree(&self, cx: &App) -> LayoutNode {
        self.dock.read(cx).layout().clone()
    }

    /// Whether the pane of `kind`'s first instance in this window holds the
    /// focus (exposed for tests: a detached window focuses the pane it was
    /// opened around, which is what keeps `FocusedPanelGlobal` pointing at it).
    pub fn pane_is_focused(&self, kind: PanelKind, window: &Window, cx: &App) -> bool {
        self.dock
            .read(cx)
            .layout()
            .instances()
            .into_iter()
            .find(|instance| instance.kind == kind)
            .is_some_and(|instance| self.panes.pane_is_focused(instance.id, window))
    }

    /// Entity id of the cached pane view of `kind`'s first instance in this
    /// window (exposed for tests: a changed id means the pane was rebuilt and
    /// lost its view state).
    pub fn panel_view_id(&self, kind: PanelKind, cx: &App) -> Option<EntityId> {
        let instance = self
            .dock
            .read(cx)
            .layout()
            .instances()
            .into_iter()
            .find(|instance| instance.kind == kind)?;
        self.panes.view_id(instance.id)
    }

    /// Re-renders this window from the shell's current layout.
    ///
    /// The one place a host learns about model changes. A window the layout no
    /// longer holds has been closed by an operation elsewhere (its last tab was
    /// dragged into another window), so its OS window follows.
    fn sync_from_session(
        &mut self,
        session: &Entity<RavelWorkspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (root, live) = {
            let shell = session.read(cx).shell();
            (
                shell.layout().window(self.id).map(|w| w.root.clone()),
                shell
                    .layout()
                    .windows()
                    .iter()
                    .flat_map(|w| w.root.instances())
                    .collect::<Vec<_>>(),
            )
        };
        let Some(root) = root else {
            if self.role == WindowRole::Detached {
                close(self.id, cx);
            }
            return;
        };
        // An instance gone from the whole workspace can never come back, so its
        // view is dropped; one that only left this window (a detach) keeps its
        // view here, and gets it back on reattach.
        self.panes.retain(&live);
        if self.role == WindowRole::Main {
            let name = project_display_name(session, cx);
            if self.project_name != name {
                self.project_name = name;
                cx.notify();
            }
        }
        if self.dock.read(cx).layout() != &root {
            self.show_tree(root, window, cx);
        }
    }

    /// Applies a dock interaction to the shared layout. ravel-dock never writes
    /// the model itself, and the updated tree comes back through
    /// [`WindowHost::sync_from_session`].
    fn on_dock_event(&mut self, event: &DockEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let id = self.id;
        if let DockEvent::TabDetachRequested {
            instance,
            screen_position,
        } = event
        {
            let (instance, position) = (*instance, *screen_position);
            defer_app(cx, move |cx| {
                drop_dragged_tab(id, instance, position, cx);
            });
            return;
        }
        let event = event.clone();
        update_shell(cx, move |shell| apply_dock_event(shell, id, &event));
    }

    /// Renders an updated tree for this window and keeps the OS title with it.
    fn show_tree(&mut self, root: LayoutNode, window: &mut Window, cx: &mut Context<Self>) {
        // The title follows the active tab, and the OS keeps whatever it was
        // given at open time until it is written again. The main window's OS
        // title names the project instead, and the session maintains it.
        if self.role == WindowRole::Detached {
            let title = window_title(&root);
            if self.os_title != title {
                self.os_title = title;
                window.set_window_title(&self.os_title);
            }
        }
        self.dock.update(cx, |dock, cx| dock.set_layout(root, cx));
    }

    /// Window title bar: the shared component, with the slots this window's
    /// role fills. The main window names the application and the open project;
    /// a detached window names its panels and carries the always-on-top pin.
    fn render_title_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.role {
            WindowRole::Main => {
                crate::title_bar::render_main_title_bar(&self.project_name, cx).into_any_element()
            }
            WindowRole::Detached => RavelTitleBar::new(window_title(self.dock.read(cx).layout()))
                .trailing(
                    Button::new("window-always-on-top")
                        .xsmall()
                        .ghost()
                        .selected(self.always_on_top)
                        .icon(Icon::new(RavelIcon::AlwaysOnTop))
                        .tooltip(t!("window.always_on_top"))
                        .on_click(cx.listener(|this, _event, window, cx| {
                            this.toggle_always_on_top(window, cx);
                        })),
                )
                .into_any_element(),
        }
    }

    /// Flips this window's always-on-top state.
    ///
    /// The layout owns the state — one flag per window, so pinning one window
    /// says nothing about the others — and the platform call mirrors what the
    /// model now holds. It reaches `layout.toml` with the rest of the layout
    /// (`crate::layout_persist`), and [`WindowHost::new`] applies it again when
    /// a restored window opens.
    fn toggle_always_on_top(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.id;
        let pinned = update_shell(cx, move |shell| {
            shell.layout_mut().window_mut(id).map(|window| {
                window.always_on_top = !window.always_on_top;
                window.always_on_top
            })
        })
        .flatten();
        let Some(pinned) = pinned else {
            tracing::warn!(window = id.0, "pinned window is no longer in the layout");
            return;
        };
        self.always_on_top = pinned;
        window.set_always_on_top(pinned);
        cx.notify();
    }
}

impl Render for WindowHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // `Root` renders the view, the tooltip, and the native menu overlay,
        // but the modal layers are the host's to place: without these children
        // an opened Dialog is live and invisible.
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            // OS file drag-and-drop (REQ-UI-010): gpui translates a platform
            // file drop into an internal drag of `ExternalPaths`; accepting
            // it anywhere in any window routes the batch through the same
            // import path as File ▸ Import (one undo step).
            .can_drop(|value, _window, _cx| value.is::<ExternalPaths>())
            .on_drop(cx.listener(|_this, paths: &ExternalPaths, _window, cx| {
                crate::media::import::import_paths(paths.paths().to_vec(), cx);
            }))
            .child(self.render_title_bar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.dock.clone()),
            )
            .children(dialog_layer)
            .children(notification_layer);
        crate::workspace::with_command_handlers(root, cx)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Display name of the project the session has open.
fn project_display_name(session: &Entity<RavelWorkspace>, cx: &App) -> String {
    crate::title_bar::project_display_name(session.read(cx).project().read(cx).project_path())
}

/// What a window's title says, before any translation. Deciding this without
/// touching the locale catalog is what makes the rule testable headlessly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowTitle {
    /// The window holds no panel: the application name.
    App,
    /// The window holds exactly one panel: its name.
    Panel(PanelKind),
    /// The window holds several panels: how many.
    Panels(usize),
}

/// Which title a window showing `root` gets.
fn window_title_kind(root: &LayoutNode) -> WindowTitle {
    match root.instances().len() {
        0 => WindowTitle::App,
        1 => active_instance(root).map_or(WindowTitle::App, |instance| {
            WindowTitle::Panel(instance.kind)
        }),
        count => WindowTitle::Panels(count),
    }
}

/// Title for a window showing `root`, localized.
fn window_title(root: &LayoutNode) -> String {
    match window_title_kind(root) {
        WindowTitle::App => t!("app.title"),
        WindowTitle::Panel(kind) => panels::panel_display_name(kind),
        WindowTitle::Panels(count) => panel_count_title(&t!("window.panels"), count),
    }
}

/// Fills the `{count}` placeholder of the multi-panel window title.
///
/// The whole phrase is one locale key with the number substituted into it;
/// concatenating a translated fragment with a number would fix the word order
/// to English.
fn panel_count_title(pattern: &str, count: usize) -> String {
    pattern.replace("{count}", &count.to_string())
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

    #[test]
    fn one_panel_window_is_titled_after_that_panel() {
        let tree = LayoutNode::area(vec![instance(0, PanelKind::Viewer)]);
        assert_eq!(
            super::window_title_kind(&tree),
            super::WindowTitle::Panel(PanelKind::Viewer)
        );
    }

    #[test]
    fn several_panels_are_titled_by_their_count() {
        let tabs = LayoutNode::area(vec![
            instance(0, PanelKind::Viewer),
            instance(1, PanelKind::Timeline),
        ]);
        assert_eq!(
            super::window_title_kind(&tabs),
            super::WindowTitle::Panels(2)
        );
        // Split areas count too: the title describes the window, not the area.
        let split = LayoutNode::split(
            Orientation::Horizontal,
            0.5,
            tabs,
            LayoutNode::area(vec![instance(2, PanelKind::Outliner)]),
        );
        assert_eq!(
            super::window_title_kind(&split),
            super::WindowTitle::Panels(3)
        );
    }

    #[test]
    fn empty_window_is_titled_after_the_application() {
        assert_eq!(
            super::window_title_kind(&LayoutNode::area(Vec::new())),
            super::WindowTitle::App
        );
    }

    /// The multi-panel title has to read naturally in every locale, so the
    /// count is substituted into a whole translated phrase. Both catalogs must
    /// therefore carry the key *and* the placeholder.
    #[test]
    fn panel_count_pattern_is_localized_in_every_catalog() {
        for locale in ["en", "ja"] {
            let path = format!(
                "{}/../../assets/locales/{locale}.toml",
                env!("CARGO_MANIFEST_DIR")
            );
            let text = std::fs::read_to_string(&path).expect("locale catalog not found");
            let catalog = text.parse::<toml::Table>().expect("invalid TOML");
            let pattern = catalog
                .get("window")
                .and_then(|window| window.get("panels"))
                .and_then(|panels| panels.as_str())
                .unwrap_or_else(|| panic!("{locale}.toml is missing window.panels"))
                .to_owned();
            assert!(
                pattern.contains("{count}"),
                "{locale}.toml window.panels must interpolate the count: {pattern}"
            );
            let title = super::panel_count_title(&pattern, 3);
            assert!(title.contains('3'), "count is not substituted: {title}");
            assert!(
                !title.contains("{count}"),
                "placeholder survived substitution: {title}"
            );
        }
    }

    /// The pin's tooltip is user-visible text, so it must exist in both
    /// catalogs too (English is the fallback, Japanese is not enforced
    /// mechanically anywhere else).
    #[test]
    fn always_on_top_label_is_localized_in_every_catalog() {
        for locale in ["en", "ja"] {
            let path = format!(
                "{}/../../assets/locales/{locale}.toml",
                env!("CARGO_MANIFEST_DIR")
            );
            let text = std::fs::read_to_string(&path).expect("locale catalog not found");
            let catalog = text.parse::<toml::Table>().expect("invalid TOML");
            assert!(
                catalog
                    .get("window")
                    .and_then(|window| window.get("always_on_top"))
                    .and_then(|label| label.as_str())
                    .is_some_and(|label| !label.is_empty()),
                "{locale}.toml is missing window.always_on_top"
            );
        }
    }
}
