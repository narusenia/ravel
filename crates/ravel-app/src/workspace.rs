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
use ravel_ui::shell::AppShell;

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
    tracing::debug!(command = cmd.as_str(), "command dispatched");
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

    pub fn setup_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let weak_dock = self.dock_area.downgrade();

        let timeline = panels::placeholder_panel("Timeline", cx);
        let node_graph = panels::placeholder_panel("Node Graph", cx);
        let viewer = panels::placeholder_panel("Viewer", cx);
        let properties = panels::placeholder_panel("Properties", cx);

        let center = DockItem::split(
            Axis::Horizontal,
            vec![
                DockItem::tabs(vec![viewer], &weak_dock, window, cx),
                DockItem::tabs(vec![node_graph], &weak_dock, window, cx),
            ],
            &weak_dock,
            window,
            cx,
        );

        let root = DockItem::split(
            Axis::Vertical,
            vec![
                center,
                DockItem::tabs(vec![timeline], &weak_dock, window, cx),
            ],
            &weak_dock,
            window,
            cx,
        );

        self.dock_area.update(cx, |area, cx| {
            area.set_center(root, window, cx);
            area.set_right_dock(
                DockItem::tabs(vec![properties], &weak_dock, window, cx),
                Some(px(280.0)),
                true,
                window,
                cx,
            );
        });
    }
}

impl Render for RavelWorkspace {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.dock_area.clone())
    }
}
