// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPUI workspace: thin rendering layer over the headless [`AppShell`].
//!
//! All command dispatch, panel visibility, preset switching, and keybinding
//! resolution is delegated to the ravel-ui headless shell. This module only
//! maps between GPUI's action/rendering system and that shell.

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

use std::collections::HashMap;

/// Tracks open detached OS windows so they can be closed on reattach.
pub struct DetachedWindowHandles(pub HashMap<WindowId, AnyWindowHandle>);
impl Global for DetachedWindowHandles {}

/// Simple root view for a detached panel window.
struct DetachedPanelView {
    dock_area: Entity<DockArea>,
}

impl Render for DetachedPanelView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.dock_area.clone())
    }
}

/// Pending command set by App-level action handlers, picked up by
/// RavelWorkspace::render() on the next frame.
pub struct PendingCommand(pub Option<CommandId>);
impl Global for PendingCommand {}

/// Register App-level action handlers that set a pending command global.
/// The actual command handling happens in RavelWorkspace::render().
pub fn register_action_handlers(cx: &mut App) {
    macro_rules! register {
        ($($Action:ident => $cmd:ident),+ $(,)?) => {
            $(cx.on_action(|_: &$Action, cx: &mut App| {
                let cmd = CommandId::$cmd;
                if cmd == CommandId::FileQuit {
                    cx.quit();
                    return;
                }
                cx.set_global(PendingCommand(Some(cmd)));
                // Trigger a redraw so render() picks up the command
                cx.refresh_windows();
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
                disabled: false,
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
        disabled: false,
    }];

    for menu in &bar.menus {
        gpui_menus.push(gpui::Menu {
            name: menu.label_key.into(),
            items: menu.items.iter().map(convert_menu_item).collect(),
            disabled: false,
        });
    }

    gpui_menus
}

// ---------------------------------------------------------------------------
// RavelWorkspace
// ---------------------------------------------------------------------------

pub struct RavelWorkspace {
    dock_area: Entity<DockArea>,
    pub shell: AppShell,
    focus_handle: FocusHandle,
    needs_rebuild: bool,
}

impl RavelWorkspace {
    pub fn new(shell: AppShell, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dock_area = cx.new(|cx| DockArea::new("ravel_main", None, window, cx));
        let focus_handle = cx.focus_handle();
        Self {
            dock_area,
            shell,
            focus_handle,
            needs_rebuild: true,
        }
    }

    pub fn shell(&self) -> &AppShell {
        &self.shell
    }

    fn open_detached(panel: PanelKind, window_id: WindowId, cx: &mut App) {
        let title = panels::panel_display_name(panel);
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(640.0), px(480.0)),
                    cx,
                ))),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let panel_view = panels::panel_for_kind(panel, window, cx);
                cx.new(|cx| {
                    let dock_area = cx.new(|cx| DockArea::new("detached_panel", None, window, cx));
                    let weak = dock_area.downgrade();
                    dock_area.update(cx, |area, cx| {
                        let item = DockItem::tabs(vec![panel_view], &weak, window, cx);
                        area.set_center(item, window, cx);
                    });
                    DetachedPanelView { dock_area }
                })
            },
        );
        match result {
            Ok(handle) => {
                if cx.has_global::<DetachedWindowHandles>() {
                    cx.global_mut::<DetachedWindowHandles>()
                        .0
                        .insert(window_id, handle.into());
                }
            }
            Err(e) => eprintln!("[ravel] failed to open detached window: {e}"),
        }
    }

    fn close_detached(window_id: WindowId, cx: &mut App) {
        let handle = if cx.has_global::<DetachedWindowHandles>() {
            cx.global_mut::<DetachedWindowHandles>()
                .0
                .remove(&window_id)
        } else {
            None
        };
        if let Some(handle) = handle {
            let _ = handle.update(cx, |_view, window, _cx| {
                window.remove_window();
            });
        }
    }

    /// Rebuilds the DockArea center content from the active preset layout,
    /// filtering panels by current visibility. Reuses the existing DockArea
    /// entity to preserve the gpui focus/dispatch tree.
    pub fn rebuild_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let weak_dock = self.dock_area.downgrade();
        let layout = self.shell.presets().active().layout.clone();
        let visibility = self.shell.visibility().clone();
        let bounds = window.bounds();
        let available = size(bounds.size.width, bounds.size.height);

        let new_center = build_dock_item(&layout, &visibility, available, &weak_dock, window, cx);

        self.dock_area.update(cx, |area, cx| {
            if let Some(root) = new_center {
                area.set_center(root, window, cx);
            }
        });

        cx.notify();
    }
}

/// Recursively converts a [`LayoutNode`] tree into a [`DockItem`] tree,
/// skipping panels that are not visible. Uses `available` (pixels) to convert
/// the layout ratio into concrete sizes for `DockItem::split_with_sizes`.
fn build_dock_item(
    node: &LayoutNode,
    visibility: &PanelVisibility,
    available: Size<Pixels>,
    weak_dock: &WeakEntity<DockArea>,
    window: &mut Window,
    cx: &mut App,
) -> Option<DockItem> {
    match node {
        LayoutNode::Leaf { panel } => {
            if visibility.is_visible(*panel) {
                let view = panels::panel_for_kind(*panel, window, cx);
                Some(DockItem::tabs(vec![view], weak_dock, window, cx))
            } else {
                None
            }
        }
        LayoutNode::Split {
            orientation,
            ratio,
            first,
            second,
        } => {
            let axis = match orientation {
                Orientation::Horizontal => Axis::Horizontal,
                Orientation::Vertical => Axis::Vertical,
            };
            let total = match axis {
                Axis::Horizontal => available.width,
                Axis::Vertical => available.height,
            };
            let first_size = total * *ratio;
            let second_size = total * (1.0 - *ratio);

            let first_available = match axis {
                Axis::Horizontal => size(first_size, available.height),
                Axis::Vertical => size(available.width, first_size),
            };
            let second_available = match axis {
                Axis::Horizontal => size(second_size, available.height),
                Axis::Vertical => size(available.width, second_size),
            };

            let first_item =
                build_dock_item(first, visibility, first_available, weak_dock, window, cx);
            let second_item =
                build_dock_item(second, visibility, second_available, weak_dock, window, cx);

            match (first_item, second_item) {
                (Some(f), Some(s)) => Some(DockItem::split_with_sizes(
                    axis,
                    vec![f, s],
                    vec![Some(first_size), Some(second_size)],
                    weak_dock,
                    window,
                    cx,
                )),
                (Some(item), None) | (None, Some(item)) => Some(item),
                (None, None) => None,
            }
        }
    }
}

impl Render for RavelWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Process any pending command from App-level action handlers
        if let Some(cmd) = cx.try_global::<PendingCommand>().and_then(|p| p.0) {
            cx.set_global(PendingCommand(None));
            // Sync focused panel
            let focused = cx
                .try_global::<panels::FocusedPanelGlobal>()
                .and_then(|g| g.0);
            self.shell.set_focused_panel(focused);
            let outcome = self.shell.handle_command(cmd);
            match &outcome {
                CommandOutcome::DetachPanel { panel, window_id } => {
                    Self::open_detached(*panel, *window_id, cx);
                }
                CommandOutcome::ReattachPanel { window_id, .. } => {
                    Self::close_detached(*window_id, cx);
                }
                _ => {}
            }
            self.needs_rebuild = true;
        }

        if self.needs_rebuild {
            self.needs_rebuild = false;
            self.rebuild_layout(window, cx);
            cx.set_menus(build_menus(&self.shell));
        }
        self.focus_handle.focus(window, cx);

        macro_rules! action_handlers {
            ($el:expr, $cx:expr, $($Action:ident => $cmd:ident),+ $(,)?) => {{
                let mut el = $el;
                $(el = el.on_action($cx.listener(|this: &mut Self, _: &$Action, _window, cx| {
                    let cmd = CommandId::$cmd;
                    if cmd == CommandId::FileQuit {
                        cx.quit();
                        return;
                    }
                    // Sync focused panel from global before dispatch
                    let focused = cx.try_global::<panels::FocusedPanelGlobal>()
                        .and_then(|g| g.0);
                    this.shell.set_focused_panel(focused);
                    let outcome = this.shell.handle_command(cmd);
                    match outcome {
                        CommandOutcome::DetachPanel { panel, window_id } => {
                            Self::open_detached(panel, window_id, cx);
                        }
                        CommandOutcome::ReattachPanel { window_id, .. } => {
                            Self::close_detached(window_id, cx);
                        }
                        _ => {}
                    }
                    this.needs_rebuild = true;
                    cx.notify();
                }));)+
                el
            }};
        }

        let root = div()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(self.dock_area.clone());

        action_handlers!(root, cx,
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
}
