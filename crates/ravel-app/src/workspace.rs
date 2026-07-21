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

/// The single Command ↔ GPUI Action correspondence table.
///
/// Each [`CommandId`] variant has a GPUI action struct of the same name.
/// Every site that needs the full mapping (action declaration, app-level
/// registration, keybinding conversion, menu conversion, workspace action
/// handlers) defines a local macro and passes it here, so adding a command
/// means extending exactly this list (plus `CommandId` itself). The `match`
/// expressions generated from this table are exhaustive, so a missing entry
/// is a compile error.
macro_rules! for_each_command {
    ($m:ident) => {
        $m! {
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
            EditDelete,
            EditDuplicate,
            ViewToggleOutliner,
            ViewToggleTimeline,
            ViewToggleNodeGraph,
            ViewToggleViewer,
            ViewToggleDopesheet,
            ViewToggleProperties,
            ViewToggleCurveEditor,
            ViewToggleScopes,
            ViewFit,
            PlaybackToggle,
            PlaybackStop,
            FrameStepForward,
            FrameStepBackward,
            LayerAddSolid,
            LayerAddShape,
            LayerAddVideo,
            LayerAddNull,
            WorkspaceEdit,
            WorkspaceNode,
            WorkspaceColor,
            WorkspaceMotion,
            PanelDetach,
            PanelReattach,
            HelpAbout,
        }
    };
}

macro_rules! declare_actions {
    ($($Action:ident),+ $(,)?) => {
        actions!(ravel, [$($Action),+]);
    };
}
for_each_command!(declare_actions);

/// Every command mapped to a GPUI action, in table order.
///
/// Exposed so tests can detect a [`CommandId`] variant missing from (or
/// duplicated in) the mapping table.
pub fn mapped_commands() -> Vec<CommandId> {
    macro_rules! list {
        ($($Action:ident),+ $(,)?) => { vec![$(CommandId::$Action),+] };
    }
    for_each_command!(list)
}

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

/// Main workspace target used by App-level action handlers when the active
/// window did not handle an action itself.
#[derive(Clone)]
pub struct MainWorkspace {
    window: AnyWindowHandle,
    workspace: WeakEntity<RavelWorkspace>,
}

impl MainWorkspace {
    pub fn new(window: AnyWindowHandle, workspace: WeakEntity<RavelWorkspace>) -> Self {
        Self { window, workspace }
    }
}

impl Global for MainWorkspace {}

/// Register all panel types in the DockArea's PanelRegistry so that
/// `DockArea::load()` can reconstruct panels from serialized state.
pub fn register_panels(cx: &mut App) {
    for kind in PanelKind::ALL {
        let panel_id = kind.panel_id().to_string();
        register_panel(
            cx,
            &panel_id,
            move |_dock_area, _state, _info, window, cx| match kind {
                PanelKind::Timeline => {
                    let entity = cx.new(|cx| panels::timeline::TimelineGpuiPanel::new(window, cx));
                    Box::new(entity)
                }
                PanelKind::NodeGraph => {
                    let entity = cx.new(|cx| panels::node_editor::NodeEditorPanel::new(window, cx));
                    Box::new(entity)
                }
                PanelKind::Properties => {
                    let entity =
                        cx.new(|cx| panels::properties::PropertiesGpuiPanel::new(window, cx));
                    Box::new(entity)
                }
                PanelKind::Viewer => {
                    let entity = cx.new(|cx| panels::viewer::ViewerPanel::new(window, cx));
                    Box::new(entity)
                }
                _ => {
                    let entity = cx.new(|cx| {
                        panels::PlaceholderPanel::new(kind.panel_id(), Some(kind), window, cx)
                    });
                    Box::new(entity)
                }
            },
        );
    }
}

/// Register App-level fallback handlers for actions not handled by a window.
pub fn register_action_handlers(cx: &mut App) {
    macro_rules! register {
        ($($Action:ident),+ $(,)?) => {
            $(cx.on_action(|_: &$Action, cx: &mut App| {
                let cmd = CommandId::$Action;
                let target = cx.try_global::<MainWorkspace>().cloned();
                let outcome = match target {
                    Some(target) => match target.window.update(cx, |_root, window, cx| {
                        target.workspace.update(cx, |workspace, cx| {
                            workspace.dispatch_command(cmd, window, cx)
                        })
                    }) {
                        Ok(Ok(outcome)) => format!("dispatched: {outcome:?}"),
                        Ok(Err(error)) => format!("workspace unavailable: {error}"),
                        Err(error) => format!("main window unavailable: {error}"),
                    },
                    None => "main workspace not registered".to_string(),
                };
                crate::trace::record(cx, crate::trace::TraceEntry {
                    source: crate::trace::TraceSource::AppAction,
                    command: Some(cmd),
                    focused_panel: crate::trace::focused_panel(cx),
                    handler: "register_action_handlers",
                    outcome: Some(outcome),
                });
            });)+
        };
    }
    for_each_command!(register);
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

/// Build GPUI keybindings from the headless table and panel-local contexts.
pub fn build_keybindings(shell: &AppShell) -> Vec<KeyBinding> {
    let mut out = Vec::new();
    for (chord, cmd) in shell.keybindings().iter() {
        let gpui_chord = chord_to_gpui_string(chord);
        macro_rules! bind {
            ($($Action:ident),+ $(,)?) => {
                match cmd {
                    $(CommandId::$Action => {
                        out.push(KeyBinding::new(&gpui_chord, $Action, None));
                    })+
                }
            };
        }
        for_each_command!(bind);
    }
    out.extend([
        KeyBinding::new(
            "cmd-d",
            EditDuplicate,
            Some(panels::node_editor::KEY_CONTEXT),
        ),
        KeyBinding::new("f", ViewFit, Some(panels::node_editor::KEY_CONTEXT)),
        KeyBinding::new("delete", EditDelete, Some(panels::node_editor::KEY_CONTEXT)),
        KeyBinding::new(
            "backspace",
            EditDelete,
            Some(panels::node_editor::KEY_CONTEXT),
        ),
        KeyBinding::new("delete", EditDelete, Some(panels::timeline::KEY_CONTEXT)),
        KeyBinding::new("backspace", EditDelete, Some(panels::timeline::KEY_CONTEXT)),
    ]);
    out
}

// ---------------------------------------------------------------------------
// Menus — derived from the headless MenuBar model
// ---------------------------------------------------------------------------

/// Convert a headless MenuItem to a GPUI MenuItem.
fn convert_menu_item(item: &ravel_ui::menu::MenuItem) -> gpui::MenuItem {
    match item {
        ravel_ui::menu::MenuItem::Action { command, .. } => {
            let command = *command;
            macro_rules! to_gpui_action {
                ($($Action:ident),+ $(,)?) => {
                    match command {
                        $(CommandId::$Action => {
                            gpui::MenuItem::action(t!(command.label_key()), $Action)
                        })+
                    }
                };
            }
            for_each_command!(to_gpui_action)
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
    playback: Entity<crate::playback::PlaybackController>,
    project: Entity<crate::project_state::ProjectState>,
    /// Last OS window title we applied; project observers compare against
    /// it so a title write (and workspace re-render) only happens when the
    /// project path actually changes, not on every document edit.
    window_title: String,
    #[allow(dead_code)]
    title_sub: Subscription,
}

impl RavelWorkspace {
    pub fn new(shell: AppShell, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dock_area = cx.new(|cx| DockArea::new("ravel_main", None, window, cx));
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        let project = cx.new(crate::project_state::ProjectState::new);
        cx.set_global(crate::project_state::ProjectStateHandle(
            project.downgrade(),
        ));
        let playback = cx.new(|_| crate::playback::PlaybackController::new());
        cx.set_global(crate::playback::PlaybackControllerHandle(
            playback.downgrade(),
        ));

        // Keep the OS window title (and the title-bar project name) in
        // sync with the open project. Project state notifies on every
        // document edit, so only act when the derived title changes
        // (open / save-as / new project).
        let window_title = crate::title_bar::window_title(project.read(cx).project_path());
        window.set_window_title(&window_title);
        let title_sub = cx.observe_in(&project, window, |this, project, window, cx| {
            let title = crate::title_bar::window_title(project.read(cx).project_path());
            if this.window_title != title {
                this.window_title = title;
                window.set_window_title(&this.window_title);
                cx.notify();
            }
        });

        Self {
            dock_area,
            shell,
            focus_handle,
            panel_views: HashMap::new(),
            pre_detach_snapshot: None,
            detached_panels: std::collections::HashSet::new(),
            needs_full_rebuild: true,
            playback,
            project,
            window_title,
            title_sub,
        }
    }

    pub fn shell(&self) -> &AppShell {
        &self.shell
    }

    /// The playback transport controller (exposed for tests).
    pub fn playback(&self) -> &Entity<crate::playback::PlaybackController> {
        &self.playback
    }

    /// The app-wide document state (exposed for tests).
    pub fn project(&self) -> &Entity<crate::project_state::ProjectState> {
        &self.project
    }

    fn request_full_rebuild(&mut self) {
        self.needs_full_rebuild = true;
    }

    /// Dispatches one command from a GPUI action callback.
    pub fn dispatch_command(
        &mut self,
        cmd: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> CommandOutcome {
        let focused = cx
            .try_global::<panels::FocusedPanelGlobal>()
            .and_then(|global| global.0);
        self.shell.set_focused_panel(focused);
        let outcome = self.shell.handle_command(cmd);
        crate::trace::record(
            cx,
            crate::trace::TraceEntry {
                source: crate::trace::TraceSource::WorkspaceAction,
                command: Some(cmd),
                focused_panel: focused,
                handler: "RavelWorkspace::dispatch_command",
                outcome: Some(format!("{outcome:?}")),
            },
        );
        self.dispatch_outcome(cmd, outcome.clone(), window, cx);
        outcome
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
        if cmd == CommandId::FileQuit {
            cx.quit();
            return;
        }

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
            CommandOutcome::Delegate(cmd) => match cmd {
                CommandId::PlaybackToggle
                | CommandId::PlaybackStop
                | CommandId::FrameStepForward
                | CommandId::FrameStepBackward => {
                    self.playback.update(cx, |playback, cx| {
                        playback.handle_command(cmd, cx);
                    });
                }
                // Layer creation from builtin templates (REQ-LAYER-008).
                CommandId::LayerAddSolid
                | CommandId::LayerAddShape
                | CommandId::LayerAddVideo
                | CommandId::LayerAddNull => {
                    if let Some(key) = cmd.layer_template_key() {
                        self.project.update(cx, |project, cx| {
                            project.add_layer_from_template(key, cx);
                        });
                    }
                }
                // Document-level undo/redo (REQ-LAYER-009): reached when no
                // focused panel intercepted the edit action.
                CommandId::EditUndo => {
                    self.project.update(cx, |project, cx| {
                        project.undo(cx);
                    });
                }
                CommandId::EditRedo => {
                    self.project.update(cx, |project, cx| {
                        project.redo(cx);
                    });
                }
                // Project persistence (File menu). The project entity is the
                // same one panels resolve through `ProjectStateHandle`.
                CommandId::FileNew => {
                    self.project.update(cx, |project, cx| {
                        project.new_document(cx);
                    });
                }
                CommandId::FileSave => {
                    let path = self
                        .project
                        .read(cx)
                        .project_path()
                        .map(std::path::Path::to_path_buf);
                    match path {
                        Some(path) => {
                            self.project.update(cx, |project, cx| {
                                project.save_project_to(path, cx);
                            });
                        }
                        // Never saved: Save behaves as Save As.
                        None => self.prompt_save_as(cx),
                    }
                }
                CommandId::FileSaveAs => self.prompt_save_as(cx),
                CommandId::FileOpen => self.prompt_open(cx),
                _ => {}
            },
        }
        cx.notify();
    }

    /// File ▸ Save As…: prompt for a destination path, then save through
    /// [`crate::project_state::ProjectState`]. Cancelling the dialog is a
    /// no-op.
    fn prompt_save_as(&mut self, cx: &mut Context<Self>) {
        let dir = self
            .project
            .read(cx)
            .project_path()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        let receiver = cx.prompt_for_new_path(&dir, Some("project.ravprj"));
        let project = self.project.downgrade();
        cx.spawn(async move |_this, cx| match receiver.await {
            Ok(Ok(Some(path))) => {
                let path = with_ravprj_extension(path);
                if project
                    .update(cx, |project, cx| {
                        project.save_project_to(path, cx);
                    })
                    .is_err()
                {
                    tracing::warn!("project state dropped before Save As completed");
                }
            }
            // The dialog was cancelled (or the app is shutting down).
            Ok(Ok(None)) | Err(_) => {}
            Ok(Err(err)) => tracing::error!(%err, "save dialog failed"),
        })
        .detach();
    }

    /// File ▸ Open…: prompt for a `.ravprj` to load. Cancelling the dialog is
    /// a no-op.
    fn prompt_open(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        let project = self.project.downgrade();
        cx.spawn(async move |_this, cx| match receiver.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next()
                    && project
                        .update(cx, |project, cx| {
                            project.load_project_from(path, cx);
                        })
                        .is_err()
                {
                    tracing::warn!("project state dropped before Open completed");
                }
            }
            // The dialog was cancelled (or the app is shutting down).
            Ok(Ok(None)) | Err(_) => {}
            Ok(Err(err)) => tracing::error!(%err, "open dialog failed"),
        })
        .detach();
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
            // Reattach can be dispatched from the detached window itself, so
            // that window may still be on the update stack; updating it here
            // would fail and leak the window. Defer past the current cycle.
            cx.defer(move |cx| {
                if let Err(e) = handle.update(cx, |_view, window, _cx| {
                    window.remove_window();
                }) {
                    eprintln!("[ravel] failed to close detached window: {e}");
                }
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

/// Ensure a save path carries the `.ravprj` extension (appending or
/// replacing whatever the dialog returned).
fn with_ravprj_extension(path: std::path::PathBuf) -> std::path::PathBuf {
    if path.extension().is_some_and(|ext| ext == "ravprj") {
        path
    } else {
        path.with_extension("ravprj")
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
        if self.needs_full_rebuild {
            self.needs_full_rebuild = false;
            self.rebuild_layout(window, cx);
            cx.set_menus(build_menus(&self.shell));
        }
        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .track_focus(&self.focus_handle)
            .child(crate::title_bar::render_title_bar(self, cx))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .child(self.dock_area.clone()),
            );

        macro_rules! action_handlers {
            ($($Action:ident),+ $(,)?) => {{
                let mut el = root;
                $(el = el.on_action(cx.listener(|this: &mut Self, _: &$Action, window, cx| {
                    this.dispatch_command(CommandId::$Action, window, cx);
                }));)+
                el
            }};
        }

        for_each_command!(action_handlers)
    }
}

#[cfg(test)]
mod tests {
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back
    // to the built-in one so `#[test]` resolves to the real one.
    use core::prelude::v1::test;

    #[test]
    fn save_path_extension_is_completed() {
        assert_eq!(
            super::with_ravprj_extension(std::path::PathBuf::from("/tmp/demo")),
            std::path::PathBuf::from("/tmp/demo.ravprj")
        );
        assert_eq!(
            super::with_ravprj_extension(std::path::PathBuf::from("/tmp/demo.ravprj")),
            std::path::PathBuf::from("/tmp/demo.ravprj")
        );
        assert_eq!(
            super::with_ravprj_extension(std::path::PathBuf::from("/tmp/demo.txt")),
            std::path::PathBuf::from("/tmp/demo.ravprj")
        );
    }
}
