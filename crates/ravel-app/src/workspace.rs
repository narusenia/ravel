// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPUI workspace: thin rendering layer over the headless [`AppShell`].
//!
//! All command dispatch, panel visibility, preset switching, and keybinding
//! resolution is delegated to the ravel-ui headless shell. This module only
//! maps between GPUI's action/rendering system and that shell.

use std::sync::Arc;

use gpui::*;
use gpui_component::Root;
use gpui_component::dock::{
    DockArea, DockAreaState, DockItem, DockPlacement, PanelView, register_panel,
};
use ravel_i18n::t;
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

/// Register all panel types in the DockArea's PanelRegistry so that
/// `DockArea::load()` can reconstruct panels from serialized state.
pub fn register_panels(cx: &mut App) {
    for kind in PanelKind::ALL {
        let panel_id = kind.panel_id().to_string();
        register_panel(
            cx,
            &panel_id,
            move |_dock_area, _state, _info, _window, cx| match kind {
                PanelKind::Timeline => {
                    let entity = cx.new(panels::timeline::TimelineGpuiPanel::new);
                    Box::new(entity)
                }
                PanelKind::NodeGraph => {
                    let entity = cx.new(panels::node_editor::NodeEditorPanel::new);
                    Box::new(entity)
                }
                PanelKind::Properties => {
                    let entity = cx.new(panels::properties::PropertiesGpuiPanel::new);
                    Box::new(entity)
                }
                _ => {
                    let entity =
                        cx.new(|cx| panels::PlaceholderPanel::new(kind.panel_id(), Some(kind), cx));
                    Box::new(entity)
                }
            },
        );
    }
}

/// Register App-level action handlers that set a pending command global.
/// The actual command handling happens in RavelWorkspace::render().
pub fn register_action_handlers(cx: &mut App) {
    macro_rules! register {
        ($($Action:ident => $cmd:ident),+ $(,)?) => {
            $(cx.on_action(|_: &$Action, cx: &mut App| {
                let cmd = CommandId::$cmd;
                let overwritten = cx
                    .try_global::<PendingCommand>()
                    .and_then(|p| p.0)
                    .map(|prev| format!("overwrites pending {prev}"));
                crate::trace::record(cx, crate::trace::TraceEntry {
                    source: crate::trace::TraceSource::AppAction,
                    command: Some(cmd),
                    focused_panel: crate::trace::focused_panel(cx),
                    handler: "register_action_handlers",
                    outcome: overwritten,
                });
                if cmd == CommandId::FileQuit {
                    cx.quit();
                    return;
                }
                cx.set_global(PendingCommand(Some(cmd)));
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
                    gpui::MenuItem::action(t!($cmd.label_key()), $Action)
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
                name: t!(sub.label_key).into(),
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
        name: t!("app.title").into(),
        items: vec![
            gpui::MenuItem::action(t!(CommandId::HelpAbout.label_key()), HelpAbout),
            gpui::MenuItem::separator(),
            gpui::MenuItem::os_submenu("Services", SystemMenuType::Services),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action(t!(CommandId::FileQuit.label_key()), FileQuit),
        ],
        disabled: false,
    }];

    for menu in &bar.menus {
        gpui_menus.push(gpui::Menu {
            name: t!(menu.label_key).into(),
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
    panel_views: HashMap<PanelKind, Arc<dyn PanelView>>,
    pre_detach_snapshot: Option<DockAreaState>,
    detached_panels: std::collections::HashSet<PanelKind>,
    needs_full_rebuild: bool,
}

impl RavelWorkspace {
    pub fn new(shell: AppShell, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dock_area = cx.new(|cx| DockArea::new("ravel_main", None, window, cx));
        let focus_handle = cx.focus_handle();
        Self {
            dock_area,
            shell,
            focus_handle,
            panel_views: HashMap::new(),
            pre_detach_snapshot: None,
            detached_panels: std::collections::HashSet::new(),
            needs_full_rebuild: true,
        }
    }

    pub fn shell(&self) -> &AppShell {
        &self.shell
    }

    fn request_full_rebuild(&mut self) {
        self.needs_full_rebuild = true;
    }

    fn toggle_panel_in_dock(
        &mut self,
        panel: PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let visible = self.shell.visibility().is_visible(panel);
        if visible {
            let view = self
                .panel_views
                .entry(panel)
                .or_insert_with(|| panels::panel_for_kind(panel, window, cx))
                .clone();
            self.dock_area.update(cx, |area, cx| {
                area.add_panel(view, DockPlacement::Center, None, window, cx);
            });
        } else if let Some(view) = self.panel_views.get(&panel) {
            let view = view.clone();
            self.dock_area.update(cx, |area, cx| {
                area.remove_panel(view, DockPlacement::Center, window, cx);
            });
        }
        cx.set_menus(build_menus(&self.shell));
        cx.notify();
    }

    fn detach_panel_from_dock(
        &mut self,
        panel: PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.detached_panels.is_empty() {
            self.pre_detach_snapshot = Some(self.dock_area.read(cx).dump(cx));
        }
        self.detached_panels.insert(panel);
        self.reload_snapshot_without_detached(window, cx);
    }

    fn reattach_panel_to_dock(
        &mut self,
        panel: PanelKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.detached_panels.remove(&panel);
        self.reload_snapshot_without_detached(window, cx);
        if self.detached_panels.is_empty() {
            self.pre_detach_snapshot = None;
        }
        cx.set_menus(build_menus(&self.shell));
    }

    fn reload_snapshot_without_detached(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(snapshot) = &self.pre_detach_snapshot {
            let mut filtered = snapshot.clone();
            let excluded: std::collections::HashSet<String> = self
                .detached_panels
                .iter()
                .map(|k| k.panel_id().to_string())
                .collect();
            filter_panel_state(&mut filtered.center, &excluded);
            self.dock_area.update(cx, |area, cx| {
                let _ = area.load(filtered, window, cx);
            });
            self.refresh_panel_views(window, cx);
        }
        cx.notify();
    }

    fn refresh_panel_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel_views.clear();
        for kind in PanelKind::ALL {
            if self.shell.visibility().is_visible(kind) {
                let view = panels::panel_for_kind(kind, window, cx);
                self.panel_views.insert(kind, view);
            }
        }
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
                let inner = cx.new(|cx| {
                    let dock_area = cx.new(|cx| DockArea::new("detached_panel", None, window, cx));
                    let weak = dock_area.downgrade();
                    dock_area.update(cx, |area, cx| {
                        let item = DockItem::tabs(vec![panel_view], &weak, window, cx);
                        area.set_center(item, window, cx);
                    });
                    DetachedPanelView { dock_area }
                });
                cx.new(|cx| Root::new(inner, window, cx))
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

    fn dispatch_outcome(
        &mut self,
        cmd: CommandId,
        outcome: CommandOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            CommandOutcome::DetachPanel { panel, window_id } => {
                self.detach_panel_from_dock(panel, window, cx);
                Self::open_detached(panel, window_id, cx);
            }
            CommandOutcome::ReattachPanel {
                panel, window_id, ..
            } => {
                Self::close_detached(window_id, cx);
                self.reattach_panel_to_dock(panel, window, cx);
            }
            CommandOutcome::Handled => {
                if let Some(panels) = toggle_panels(cmd) {
                    for p in panels {
                        self.toggle_panel_in_dock(p, window, cx);
                    }
                } else if is_preset_switch(cmd) {
                    self.request_full_rebuild();
                }
            }
            CommandOutcome::Delegate(_) => {}
        }
        cx.notify();
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
    /// filtering panels by current visibility. Recreates all panel views.
    pub fn rebuild_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel_views.clear();
        let weak_dock = self.dock_area.downgrade();
        let layout = self.shell.presets().active().layout.clone();
        let visibility = self.shell.visibility().clone();
        let bounds = window.bounds();
        let available = size(bounds.size.width, bounds.size.height);

        let new_center = build_dock_item(
            &layout,
            &visibility,
            available,
            &weak_dock,
            &mut self.panel_views,
            window,
            cx,
        );

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
/// Recursively removes panels whose `panel_name` is in `excluded` from
/// a serialized [`PanelState`] tree, and prunes empty containers so that
/// no blank areas remain after `DockArea::load`.
fn filter_panel_state(
    state: &mut gpui_component::dock::PanelState,
    excluded: &std::collections::HashSet<String>,
) {
    for child in &mut state.children {
        filter_panel_state(child, excluded);
    }
    let sizes = state.info.sizes().cloned();
    let mut new_sizes: Option<Vec<gpui::Pixels>> = None;
    if let Some(ref sizes) = sizes {
        let mut filtered_sizes = Vec::new();
        for (i, child) in state.children.iter().enumerate() {
            if !excluded.contains(&child.panel_name)
                && !is_empty_container(child)
                && let Some(s) = sizes.get(i)
            {
                filtered_sizes.push(*s);
            }
        }
        new_sizes = Some(filtered_sizes);
    }
    state
        .children
        .retain(|child| !excluded.contains(&child.panel_name) && !is_empty_container(child));
    if let Some(sizes) = new_sizes
        && let gpui_component::dock::PanelInfo::Stack {
            sizes: ref mut s, ..
        } = state.info
    {
        *s = sizes;
    }
}

fn is_empty_container(state: &gpui_component::dock::PanelState) -> bool {
    let is_container = matches!(
        state.info,
        gpui_component::dock::PanelInfo::Stack { .. }
            | gpui_component::dock::PanelInfo::Tabs { .. }
    );
    is_container && state.children.is_empty()
}

fn build_dock_item(
    node: &LayoutNode,
    visibility: &PanelVisibility,
    available: Size<Pixels>,
    weak_dock: &WeakEntity<DockArea>,
    panel_views: &mut HashMap<PanelKind, Arc<dyn PanelView>>,
    window: &mut Window,
    cx: &mut App,
) -> Option<DockItem> {
    match node {
        LayoutNode::Leaf { panel } => {
            if visibility.is_visible(*panel) {
                let view = panels::panel_for_kind(*panel, window, cx);
                panel_views.insert(*panel, view.clone());
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

            let first_item = build_dock_item(
                first,
                visibility,
                first_available,
                weak_dock,
                panel_views,
                window,
                cx,
            );
            let second_item = build_dock_item(
                second,
                visibility,
                second_available,
                weak_dock,
                panel_views,
                window,
                cx,
            );

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

/// Maps a ViewToggle command to the PanelKind(s) it controls.
fn toggle_panels(cmd: CommandId) -> Option<Vec<PanelKind>> {
    match cmd {
        CommandId::ViewToggleOutliner => Some(vec![PanelKind::Outliner]),
        CommandId::ViewToggleTimeline => Some(vec![PanelKind::Timeline]),
        CommandId::ViewToggleNodeGraph => Some(vec![PanelKind::NodeGraph]),
        CommandId::ViewToggleViewer => Some(vec![PanelKind::Viewer]),
        CommandId::ViewToggleDopesheet => Some(vec![PanelKind::Dopesheet]),
        CommandId::ViewToggleProperties => Some(vec![PanelKind::Properties]),
        CommandId::ViewToggleCurveEditor => Some(vec![PanelKind::CurveEditor]),
        CommandId::ViewToggleScopes => Some(vec![
            PanelKind::Waveform,
            PanelKind::Vectorscope,
            PanelKind::Histogram,
            PanelKind::Parade,
        ]),
        _ => None,
    }
}

fn is_preset_switch(cmd: CommandId) -> bool {
    matches!(
        cmd,
        CommandId::WorkspaceEdit
            | CommandId::WorkspaceNode
            | CommandId::WorkspaceColor
            | CommandId::WorkspaceMotion
    )
}

impl Render for RavelWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(cmd) = cx.try_global::<PendingCommand>().and_then(|p| p.0) {
            cx.set_global(PendingCommand(None));
            match cmd {
                CommandId::EditUndo => {
                    cx.set_global(panels::PanelUndoRedo(Some(panels::UndoRedoSignal::Undo)));
                }
                CommandId::EditRedo => {
                    cx.set_global(panels::PanelUndoRedo(Some(panels::UndoRedoSignal::Redo)));
                }
                _ => {}
            }
            let focused = cx
                .try_global::<panels::FocusedPanelGlobal>()
                .and_then(|g| g.0);
            self.shell.set_focused_panel(focused);
            let outcome = self.shell.handle_command(cmd);
            crate::trace::record(
                cx,
                crate::trace::TraceEntry {
                    source: crate::trace::TraceSource::RenderPending,
                    command: Some(cmd),
                    focused_panel: focused,
                    handler: "RavelWorkspace::render",
                    outcome: Some(format!("{outcome:?}")),
                },
            );
            self.dispatch_outcome(cmd, outcome, window, cx);
        }

        if self.needs_full_rebuild {
            self.needs_full_rebuild = false;
            self.rebuild_layout(window, cx);
            cx.set_menus(build_menus(&self.shell));
        }
        self.focus_handle.focus(window, cx);

        macro_rules! action_handlers {
            ($el:expr, $cx:expr, $($Action:ident => $cmd:ident),+ $(,)?) => {{
                let mut el = $el;
                $(el = el.on_action($cx.listener(|this: &mut Self, _: &$Action, window, cx| {
                    let cmd = CommandId::$cmd;
                    if cmd == CommandId::FileQuit {
                        cx.quit();
                        return;
                    }
                    let focused = cx.try_global::<panels::FocusedPanelGlobal>()
                        .and_then(|g| g.0);
                    this.shell.set_focused_panel(focused);
                    let outcome = this.shell.handle_command(cmd);
                    crate::trace::record(cx, crate::trace::TraceEntry {
                        source: crate::trace::TraceSource::WorkspaceAction,
                        command: Some(cmd),
                        focused_panel: focused,
                        handler: "RavelWorkspace on_action",
                        outcome: Some(format!("{outcome:?}")),
                    });
                    this.dispatch_outcome(cmd, outcome, window, cx);
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
