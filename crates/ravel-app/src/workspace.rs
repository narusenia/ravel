// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPUI workspace: main window view, dock area, menus, and keybindings.

use gpui::*;
use gpui_component::dock::DockArea;
use gpui_component::dock::DockItem;

use crate::panels;

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

actions!(
    ravel,
    [
        Quit,
        About,
        NewProject,
        OpenProject,
        Save,
        SaveAs,
        Undo,
        Redo,
        Cut,
        Copy,
        Paste,
        ToggleTimeline,
        ToggleNodeGraph,
        ToggleViewer,
        ToggleProperties,
        PresetEdit,
        PresetNode,
        PresetColor,
        PresetMotion,
    ]
);

// ---------------------------------------------------------------------------
// RavelWorkspace
// ---------------------------------------------------------------------------

pub struct RavelWorkspace {
    dock_area: Entity<DockArea>,
}

impl RavelWorkspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dock_area = cx.new(|cx| DockArea::new("ravel_main", None, window, cx));
        Self { dock_area }
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

// ---------------------------------------------------------------------------
// Menus
// ---------------------------------------------------------------------------

pub fn build_menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Ravel".into(),
            items: vec![
                MenuItem::action("About Ravel", About),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Quit Ravel", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New Project", NewProject),
                MenuItem::action("Open…", OpenProject),
                MenuItem::separator(),
                MenuItem::action("Save", Save),
                MenuItem::action("Save As…", SaveAs),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Undo", Undo),
                MenuItem::action("Redo", Redo),
                MenuItem::separator(),
                MenuItem::action("Cut", Cut),
                MenuItem::action("Copy", Copy),
                MenuItem::action("Paste", Paste),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Timeline", ToggleTimeline),
                MenuItem::action("Node Graph", ToggleNodeGraph),
                MenuItem::action("Viewer", ToggleViewer),
                MenuItem::action("Properties", ToggleProperties),
            ],
        },
        Menu {
            name: "Workspace".into(),
            items: vec![
                MenuItem::action("Edit", PresetEdit),
                MenuItem::action("Node", PresetNode),
                MenuItem::action("Color", PresetColor),
                MenuItem::action("Motion", PresetMotion),
            ],
        },
    ]
}

// ---------------------------------------------------------------------------
// Keybindings
// ---------------------------------------------------------------------------

pub fn default_keybindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-n", NewProject, None),
        KeyBinding::new("cmd-o", OpenProject, None),
        KeyBinding::new("cmd-s", Save, None),
        KeyBinding::new("cmd-shift-s", SaveAs, None),
        KeyBinding::new("cmd-z", Undo, None),
        KeyBinding::new("cmd-shift-z", Redo, None),
        KeyBinding::new("cmd-x", Cut, None),
        KeyBinding::new("cmd-c", Copy, None),
        KeyBinding::new("cmd-v", Paste, None),
        KeyBinding::new("alt-1", ToggleTimeline, None),
        KeyBinding::new("alt-2", ToggleNodeGraph, None),
        KeyBinding::new("alt-3", ToggleViewer, None),
        KeyBinding::new("alt-4", ToggleProperties, None),
        KeyBinding::new("cmd-f1", PresetEdit, None),
        KeyBinding::new("cmd-f2", PresetNode, None),
        KeyBinding::new("cmd-f3", PresetColor, None),
        KeyBinding::new("cmd-f4", PresetMotion, None),
    ]
}
