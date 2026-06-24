// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPUI workspace: thin rendering layer over the headless [`AppShell`].
//!
//! All command dispatch, panel visibility, preset switching, and keybinding
//! resolution is delegated to the ravel-ui headless shell. This module only
//! maps between GPUI's action/rendering system and that shell.

use std::collections::HashMap;

use gpui::*;
use gpui_component::dock::{DockArea, DockItem};
use ravel_ui::command::CommandId;
use ravel_ui::keybindings::KeyChord;
use ravel_ui::panel::{PanelKind, PanelVisibility};
use ravel_ui::preset::{LayoutNode, Orientation};
use ravel_ui::shell::{AppShell, CommandOutcome};
use ravel_ui::window::WindowId;

use crate::panels;

// ---------------------------------------------------------------------------
// GPUI actions — one struct per CommandId variant
// ---------------------------------------------------------------------------

actions!(
    ravel,
    [
        FileNew,
        FileOpen,
        FileSave,
        FileSaveAs,
        FileQuit,
        EditUndo,
        EditRedo,
        EditCut,
        EditCopy,
        EditPaste,
        ViewToggleOutliner,
        ViewToggleTimeline,
        ViewToggleNodeGraph,
        ViewToggleViewer,
        ViewToggleDopesheet,
        ViewToggleProperties,
        ViewToggleCurveEditor,
        ViewToggleScopes,
        WorkspaceEdit,
        WorkspaceNode,
        WorkspaceColor,
        WorkspaceMotion,
        PanelDetach,
        PanelReattach,
        HelpAbout,
    ]
);

/// Global handle to the main workspace window.
///
/// Stored after the window is opened so that App-level action handlers can
/// reach the [`RavelWorkspace`] entity via [`WindowHandle::update`].
pub struct MainWindowHandle(pub WindowHandle<RavelWorkspace>);

impl Global for MainWindowHandle {}

/// Global registry of detached panel OS windows.
#[derive(Default)]
pub struct DetachedWindows {
    handles: HashMap<WindowId, AnyWindowHandle>,
}

impl Global for DetachedWindows {}

// ---------------------------------------------------------------------------
// Detached panel window
// ---------------------------------------------------------------------------

/// Root view for a detached panel window.
///
/// When the OS window is closed (and this entity is released), it triggers
/// reattach on the main window so the panel returns to its original position.
pub struct DetachedPanelView {
    #[allow(dead_code)]
    panel_kind: PanelKind,
    #[allow(dead_code)]
    ravel_window_id: WindowId,
    dock_area: Entity<DockArea>,
    /// Held to keep the on_release subscription alive.
    #[allow(dead_code)]
    _release_subscription: Subscription,
}

impl DetachedPanelView {
    fn new(
        panel_kind: PanelKind,
        ravel_window_id: WindowId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let dock_area = cx.new(|cx| DockArea::new("detached_panel", None, window, cx));
        let panel_view = panels::panel_for_kind(panel_kind, cx);
        let weak_dock = dock_area.downgrade();
        dock_area.update(cx, |area, cx| {
            let item = DockItem::tabs(vec![panel_view], &weak_dock, window, cx);
            area.set_center(item, window, cx);
        });

        // When this entity is released (window closed by user), reattach.
        let wid = ravel_window_id;
        let sub = cx.on_release(move |_this, cx| {
            reattach_on_close(wid, cx);
        });

        Self {
            panel_kind,
            ravel_window_id,
            dock_area,
            _release_subscription: sub,
        }
    }
}

impl Render for DetachedPanelView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.dock_area.clone())
    }
}

/// Called when a detached window is closed by the OS (user clicked the close
/// button or pressed Cmd+W). Restores the panel to the main window.
fn reattach_on_close(ravel_window_id: WindowId, cx: &mut App) {
    let Some(main_window) = cx.try_global::<MainWindowHandle>() else {
        return;
    };
    let wh = main_window.0;
    let _ = wh.update(cx, |workspace, window, cx| {
        // If already reattached (e.g. PanelReattach command ran first), skip.
        if let Some(_panel) = workspace.shell.reattach_window(ravel_window_id) {
            workspace.rebuild_layout(window, cx);
            cx.set_menus(build_menus(&workspace.shell));
        }
    });
    if cx.has_global::<DetachedWindows>() {
        cx.global_mut::<DetachedWindows>()
            .handles
            .remove(&ravel_window_id);
    }
}

/// Register on_action handlers for every command, routing through AppShell.
pub fn register_action_handlers(cx: &mut App) {
    macro_rules! register {
        ($($Action:ident => $cmd:ident),+ $(,)?) => {
            $(cx.on_action(|_: &$Action, cx: &mut App| {
                dispatch_command(CommandId::$cmd, cx);
            });)+
        };
    }
    register!(
        FileNew => FileNew,
        FileOpen => FileOpen,
        FileSave => FileSave,
        FileSaveAs => FileSaveAs,
        FileQuit => FileQuit,
        EditUndo => EditUndo,
        EditRedo => EditRedo,
        EditCut => EditCut,
        EditCopy => EditCopy,
        EditPaste => EditPaste,
        ViewToggleOutliner => ViewToggleOutliner,
        ViewToggleTimeline => ViewToggleTimeline,
        ViewToggleNodeGraph => ViewToggleNodeGraph,
        ViewToggleViewer => ViewToggleViewer,
        ViewToggleDopesheet => ViewToggleDopesheet,
        ViewToggleProperties => ViewToggleProperties,
        ViewToggleCurveEditor => ViewToggleCurveEditor,
        ViewToggleScopes => ViewToggleScopes,
        WorkspaceEdit => WorkspaceEdit,
        WorkspaceNode => WorkspaceNode,
        WorkspaceColor => WorkspaceColor,
        WorkspaceMotion => WorkspaceMotion,
        PanelDetach => PanelDetach,
        PanelReattach => PanelReattach,
        HelpAbout => HelpAbout,
    );
}

fn dispatch_command(cmd: CommandId, cx: &mut App) {
    if cmd == CommandId::FileQuit {
        cx.quit();
        return;
    }

    let Some(main_window) = cx.try_global::<MainWindowHandle>() else {
        tracing::debug!(command = cmd.as_str(), "no main window; command ignored");
        return;
    };
    let window_handle = main_window.0;

    // Dispatch command to the headless shell and retrieve the outcome.
    let outcome = match window_handle.update(cx, |workspace, _window, _cx| {
        workspace.shell.handle_command(cmd)
    }) {
        Ok(outcome) => outcome,
        Err(e) => {
            tracing::warn!(error = %e, command = cmd.as_str(), "failed to dispatch command");
            return;
        }
    };

    match outcome {
        CommandOutcome::Handled => {
            rebuild_main_window(&window_handle, cx);
        }
        CommandOutcome::DetachPanel { panel, window_id } => {
            open_detached_window(panel, window_id, cx);
            rebuild_main_window(&window_handle, cx);
        }
        CommandOutcome::ReattachPanel {
            panel: _,
            window_id,
        } => {
            close_detached_window(window_id, cx);
            rebuild_main_window(&window_handle, cx);
        }
        CommandOutcome::Delegate(cmd) => {
            tracing::debug!(command = cmd.as_str(), "command delegated to host");
        }
    }
}

/// Rebuild the main window layout and menus from the current shell state.
fn rebuild_main_window(handle: &WindowHandle<RavelWorkspace>, cx: &mut App) {
    let _ = handle.update(cx, |workspace, window, cx| {
        workspace.rebuild_layout(window, cx);
        let menus = build_menus(&workspace.shell);
        cx.set_menus(menus);
    });
}

/// Open a new OS window for a detached panel.
fn open_detached_window(panel: PanelKind, window_id: WindowId, cx: &mut App) {
    let title = panels::panel_display_name(panel);
    let window = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(800.0), px(600.0)),
                cx,
            ))),
            titlebar: Some(TitlebarOptions {
                title: Some(title.into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        |window, cx| cx.new(|cx| DetachedPanelView::new(panel, window_id, window, cx)),
    );

    match window {
        Ok(handle) => {
            if !cx.has_global::<DetachedWindows>() {
                cx.set_global(DetachedWindows::default());
            }
            cx.global_mut::<DetachedWindows>()
                .handles
                .insert(window_id, handle.into());
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to open detached panel window");
        }
    }
}

/// Close a detached OS window programmatically (for PanelReattach command).
fn close_detached_window(window_id: WindowId, cx: &mut App) {
    let handle = if cx.has_global::<DetachedWindows>() {
        cx.global_mut::<DetachedWindows>()
            .handles
            .remove(&window_id)
    } else {
        None
    };

    if let Some(handle) = handle {
        let _ = handle.update(cx, |_root, window, _cx| {
            window.remove_window();
        });
    }
}

/// Convert a ravel-ui KeyChord to the gpui keystroke string format.
///
/// ravel-ui: `Cmd+Shift+Z`  →  gpui: `cmd-shift-z`
fn chord_to_gpui_string(chord: &KeyChord) -> String {
    chord.to_string().replace('+', "-").to_lowercase()
}

// ---------------------------------------------------------------------------
// Keybindings — derived from the headless binding table
// ---------------------------------------------------------------------------

/// Build GPUI KeyBinding vec from the headless keybinding table.
pub fn build_keybindings(shell: &AppShell) -> Vec<KeyBinding> {
    macro_rules! bind {
        ($out:ident, $chord:expr, $cmd:expr, $($Action:ident => $cid:ident),+ $(,)?) => {
            match $cmd {
                $(CommandId::$cid => {
                    $out.push(KeyBinding::new(&$chord, $Action, None));
                })+
            }
        };
    }
    let mut out = Vec::new();
    for (chord, cmd) in shell.keybindings().iter() {
        let gpui_chord = chord_to_gpui_string(chord);
        bind!(out, gpui_chord, cmd,
            FileNew => FileNew,
            FileOpen => FileOpen,
            FileSave => FileSave,
            FileSaveAs => FileSaveAs,
            FileQuit => FileQuit,
            EditUndo => EditUndo,
            EditRedo => EditRedo,
            EditCut => EditCut,
            EditCopy => EditCopy,
            EditPaste => EditPaste,
            ViewToggleOutliner => ViewToggleOutliner,
            ViewToggleTimeline => ViewToggleTimeline,
            ViewToggleNodeGraph => ViewToggleNodeGraph,
            ViewToggleViewer => ViewToggleViewer,
            ViewToggleDopesheet => ViewToggleDopesheet,
            ViewToggleProperties => ViewToggleProperties,
            ViewToggleCurveEditor => ViewToggleCurveEditor,
            ViewToggleScopes => ViewToggleScopes,
            WorkspaceEdit => WorkspaceEdit,
            WorkspaceNode => WorkspaceNode,
            WorkspaceColor => WorkspaceColor,
            WorkspaceMotion => WorkspaceMotion,
            PanelDetach => PanelDetach,
            PanelReattach => PanelReattach,
            HelpAbout => HelpAbout,
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Menus — derived from the headless MenuBar model
// ---------------------------------------------------------------------------

/// Convert a headless MenuItem to a GPUI MenuItem.
fn convert_menu_item(item: &ravel_ui::menu::MenuItem) -> gpui::MenuItem {
    macro_rules! to_gpui_action {
        ($cmd:expr, $($Action:ident => $cid:ident),+ $(,)?) => {
            match $cmd {
                $(CommandId::$cid => {
                    gpui::MenuItem::action($cmd.label_key(), $Action)
                })+
            }
        };
    }
    match item {
        ravel_ui::menu::MenuItem::Action { command, .. } => {
            to_gpui_action!(*command,
                FileNew => FileNew,
                FileOpen => FileOpen,
                FileSave => FileSave,
                FileSaveAs => FileSaveAs,
                FileQuit => FileQuit,
                EditUndo => EditUndo,
                EditRedo => EditRedo,
                EditCut => EditCut,
                EditCopy => EditCopy,
                EditPaste => EditPaste,
                ViewToggleOutliner => ViewToggleOutliner,
                ViewToggleTimeline => ViewToggleTimeline,
                ViewToggleNodeGraph => ViewToggleNodeGraph,
                ViewToggleViewer => ViewToggleViewer,
                ViewToggleDopesheet => ViewToggleDopesheet,
                ViewToggleProperties => ViewToggleProperties,
                ViewToggleCurveEditor => ViewToggleCurveEditor,
                ViewToggleScopes => ViewToggleScopes,
                WorkspaceEdit => WorkspaceEdit,
                WorkspaceNode => WorkspaceNode,
                WorkspaceColor => WorkspaceColor,
                WorkspaceMotion => WorkspaceMotion,
                PanelDetach => PanelDetach,
                PanelReattach => PanelReattach,
                HelpAbout => HelpAbout,
            )
        }
        ravel_ui::menu::MenuItem::Separator => gpui::MenuItem::separator(),
        ravel_ui::menu::MenuItem::Submenu(sub) => {
            let items = sub.items.iter().map(convert_menu_item).collect();
            gpui::MenuItem::submenu(gpui::Menu {
                name: sub.label_key.into(),
                items,
            })
        }
    }
}

/// Build GPUI menus from the headless MenuBar model.
pub fn build_menus(shell: &AppShell) -> Vec<gpui::Menu> {
    let bar = shell.menu_bar();
    let mut gpui_menus = vec![gpui::Menu {
        name: "Ravel".into(),
        items: vec![
            gpui::MenuItem::action(CommandId::HelpAbout.label_key(), HelpAbout),
            gpui::MenuItem::separator(),
            gpui::MenuItem::os_submenu("Services", SystemMenuType::Services),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action(CommandId::FileQuit.label_key(), FileQuit),
        ],
    }];

    for menu in &bar.menus {
        gpui_menus.push(gpui::Menu {
            name: menu.label_key.into(),
            items: menu.items.iter().map(convert_menu_item).collect(),
        });
    }

    gpui_menus
}

// ---------------------------------------------------------------------------
// RavelWorkspace
// ---------------------------------------------------------------------------

pub struct RavelWorkspace {
    dock_area: Entity<DockArea>,
    shell: AppShell,
}

impl RavelWorkspace {
    pub fn new(shell: AppShell, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dock_area = cx.new(|cx| DockArea::new("ravel_main", None, window, cx));
        Self { dock_area, shell }
    }

    pub fn shell(&self) -> &AppShell {
        &self.shell
    }

    /// Tears down the current DockArea and rebuilds it from the active preset
    /// layout, filtering panels by current visibility.
    pub fn rebuild_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Create a fresh DockArea to replace the old one (avoids stale
        // left/right/bottom dock state from a previous preset).
        let dock_area = cx.new(|cx| DockArea::new("ravel_main", None, window, cx));
        self.dock_area = dock_area;

        let weak_dock = self.dock_area.downgrade();
        let layout = self.shell.presets().active().layout.clone();
        let visibility = self.shell.visibility().clone();

        if let Some(root) = build_dock_item(&layout, &visibility, &weak_dock, window, cx) {
            self.dock_area.update(cx, |area, cx| {
                area.set_center(root, window, cx);
            });
        }

        cx.notify();
    }
}

/// Recursively converts a [`LayoutNode`] tree into a [`DockItem`] tree,
/// skipping panels that are not visible.
///
/// Returns `None` when the entire subtree is hidden.
fn build_dock_item(
    node: &LayoutNode,
    visibility: &PanelVisibility,
    weak_dock: &WeakEntity<DockArea>,
    window: &mut Window,
    cx: &mut App,
) -> Option<DockItem> {
    match node {
        LayoutNode::Leaf { panel } => {
            if visibility.is_visible(*panel) {
                let view = panels::panel_for_kind(*panel, cx);
                Some(DockItem::tabs(vec![view], weak_dock, window, cx))
            } else {
                None
            }
        }
        LayoutNode::Split {
            orientation,
            first,
            second,
            ..
        } => {
            let first_item = build_dock_item(first, visibility, weak_dock, window, cx);
            let second_item = build_dock_item(second, visibility, weak_dock, window, cx);
            match (first_item, second_item) {
                (Some(f), Some(s)) => {
                    let axis = match orientation {
                        Orientation::Horizontal => Axis::Horizontal,
                        Orientation::Vertical => Axis::Vertical,
                    };
                    Some(DockItem::split(axis, vec![f, s], weak_dock, window, cx))
                }
                (Some(item), None) | (None, Some(item)) => Some(item),
                (None, None) => None,
            }
        }
    }
}

impl Render for RavelWorkspace {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.dock_area.clone())
    }
}
