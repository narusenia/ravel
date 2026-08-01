// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The shared session: command dispatch and project state over the headless
//! [`AppShell`].
//!
//! All command dispatch, panel placement, preset switching, and keybinding
//! resolution is delegated to the ravel-ui headless shell. [`RavelWorkspace`]
//! owns that shell plus the state every window shows (document, playback,
//! audio) and executes the commands the shell delegates back to the host.
//!
//! It renders nothing: windows are rendered by [`crate::window_host`], which
//! reaches the session through the [`MainWorkspace`] global and routes command
//! actions into [`RavelWorkspace::dispatch_command`] — still the only caller of
//! [`AppShell::handle_command`].

use gpui::*;
use gpui_component::WindowExt as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::DialogFooter;
use gpui_component::notification::{Notification, NotificationType};
use ravel_i18n::t;
use ravel_ui::command::CommandId;
use ravel_ui::document::next_composition_name;
use ravel_ui::keybindings::KeyChord;
use ravel_ui::shell::{AppShell, CommandOutcome};

use crate::composition_form::CompositionForm;
use crate::panels;

/// The composition settings *value* type. `CompositionSettings` in this file is
/// the GPUI action generated for [`CommandId::CompositionSettings`], so the
/// data type it collides with is aliased here.
type CompositionSettingsValue = ravel_ui::document::CompositionSettings;

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
            FileImport,
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
            KeyframeInterpolationBezier,
            KeyframeInterpolationLinear,
            KeyframeInterpolationStep,
            ViewToggleOutliner,
            ViewToggleTimeline,
            ViewToggleNodeGraph,
            ViewToggleViewer,
            ViewToggleDopesheet,
            ViewToggleProperties,
            ViewToggleCurveEditor,
            ViewToggleScopes,
            ViewToggleMediaBin,
            ViewFit,
            PlaybackToggle,
            PlaybackStop,
            FrameStepForward,
            FrameStepBackward,
            CompositionNew,
            CompositionSettings,
            CompositionDuplicate,
            CompositionDelete,
            LayerAddSolid,
            LayerAddShape,
            LayerAddVideo,
            LayerAddAudio,
            LayerAddNull,
            WorkspaceEdit,
            WorkspaceNode,
            WorkspaceColor,
            WorkspaceMotion,
            WorkspaceManageLayouts,
            ToolSelect,
            ToolPen,
            ToolRect,
            ToolEllipse,
            ToolHand,
            ToolZoom,
            NodeSearchPalette,
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

/// Main workspace target used by App-level action handlers when the active
/// window did not handle an action itself.
///
/// It is also how [`crate::window_host`] reaches the shared session: every
/// window renders the state this one entity owns.
#[derive(Clone)]
pub struct MainWorkspace {
    window: AnyWindowHandle,
    workspace: WeakEntity<RavelWorkspace>,
}

impl MainWorkspace {
    pub fn new(window: AnyWindowHandle, workspace: WeakEntity<RavelWorkspace>) -> Self {
        Self { window, workspace }
    }

    /// The workspace entity that owns the shared shell state.
    pub fn workspace(&self) -> WeakEntity<RavelWorkspace> {
        self.workspace.clone()
    }
}

impl Global for MainWorkspace {}

/// The live session entity, if one has been installed.
///
/// Every window resolves the shared shell, document, and playback state through
/// here rather than owning any of it.
pub fn session(cx: &App) -> Option<Entity<RavelWorkspace>> {
    cx.try_global::<MainWorkspace>()?.workspace.upgrade()
}

/// Dispatches `cmd` into the session from a window's action handler.
///
/// The window that received the action supplies the [`Window`], so dialogs and
/// notifications a command raises appear where the user invoked it.
fn dispatch_in_session<T: 'static>(cmd: CommandId, window: &mut Window, cx: &mut Context<T>) {
    let Some(session) = session(cx) else {
        let focused_instance = crate::trace::focused_instance(cx);
        crate::trace::record(
            cx,
            crate::trace::TraceEntry {
                source: crate::trace::TraceSource::WorkspaceAction,
                command: Some(cmd),
                focused_instance,
                handler: "window_host",
                outcome: Some("session not registered".to_string()),
            },
        );
        return;
    };
    session.update(cx, |session, cx| {
        session.dispatch_command(cmd, window, cx);
    });
}

/// Attaches one action handler per [`CommandId`] to a window's root element.
///
/// Generated from the single command table, so every window offers the same
/// commands and each of them lands in [`RavelWorkspace::dispatch_command`].
/// Panel-local handlers still win: GPUI stops at the nearest handler, and this
/// is the outermost one in the window.
pub fn with_command_handlers<T: 'static, E: InteractiveElement>(el: E, cx: &mut Context<T>) -> E {
    macro_rules! action_handlers {
        ($($Action:ident),+ $(,)?) => {{
            let mut el = el;
            $(el = el.on_action(cx.listener(|_this: &mut T, _: &$Action, window, cx| {
                dispatch_in_session(CommandId::$Action, window, cx);
            }));)+
            el
        }};
    }
    for_each_command!(action_handlers)
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
                    focused_instance: crate::trace::focused_instance(cx),
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
                        // Workspace commands must yield to focused text inputs,
                        // whose own Input-context actions own arrows, editing,
                        // clipboard shortcuts, and Space while typing.
                        out.push(KeyBinding::new(&gpui_chord, $Action, Some("!Input")));
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
        KeyBinding::new(
            "tab",
            NodeSearchPalette,
            Some(panels::node_editor::KEY_CONTEXT),
        ),
        KeyBinding::new("delete", EditDelete, Some(panels::node_editor::KEY_CONTEXT)),
        KeyBinding::new(
            "backspace",
            EditDelete,
            Some(panels::node_editor::KEY_CONTEXT),
        ),
        KeyBinding::new("delete", EditDelete, Some(panels::timeline::KEY_CONTEXT)),
        KeyBinding::new("backspace", EditDelete, Some(panels::timeline::KEY_CONTEXT)),
        // Tool shortcuts (Viewer key context, REQ-UI-011 unit 2).
        KeyBinding::new("v", ToolSelect, Some(panels::viewer::KEY_CONTEXT)),
        KeyBinding::new("p", ToolPen, Some(panels::viewer::KEY_CONTEXT)),
        KeyBinding::new("r", ToolRect, Some(panels::viewer::KEY_CONTEXT)),
        KeyBinding::new("e", ToolEllipse, Some(panels::viewer::KEY_CONTEXT)),
        KeyBinding::new("h", ToolHand, Some(panels::viewer::KEY_CONTEXT)),
        KeyBinding::new("z", ToolZoom, Some(panels::viewer::KEY_CONTEXT)),
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

/// The shared session behind every window: the headless shell plus the state
/// the windows display.
///
/// It owns no view and renders nothing — [`crate::window_host::WindowHost`]
/// draws the windows and observes this entity for changes.
pub struct RavelWorkspace {
    pub shell: AppShell,
    playback: Entity<crate::playback::PlaybackController>,
    project: Entity<crate::project_state::ProjectState>,
    /// Strong owner of the audio service; dropping the workspace on window
    /// close shuts the engine down (its `Drop` joins the prep thread).
    #[allow(dead_code)]
    audio: Entity<crate::audio::AudioService>,
    #[allow(dead_code)]
    audio_event_sub: Subscription,
    /// Last OS window title we applied; project observers compare against
    /// it so a title write (and workspace re-render) only happens when the
    /// project path actually changes, not on every document edit.
    window_title: String,
    #[allow(dead_code)]
    title_sub: Subscription,
    #[allow(dead_code)]
    project_event_sub: Subscription,
    /// Keeps detached windows following the main window's minimize state.
    #[allow(dead_code)]
    minimize_sub: Subscription,
    /// Applies a newly opened project's embedded layout, if it has one.
    #[allow(dead_code)]
    document_replaced_sub: Subscription,
}

/// Destructive action resumed after the user resolves unsaved changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingProjectAction {
    New,
    Open,
    Quit,
    CloseWindow,
}

impl RavelWorkspace {
    /// Builds the session inside the main window.
    ///
    /// The window is the main one because the observers registered here (OS
    /// title, notifications, minimize follow) belong to it; the session itself
    /// is window-independent and outlives every individual pane.
    pub fn new(shell: AppShell, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let project = cx.new(crate::project_state::ProjectState::new);
        cx.set_global(crate::project_state::ProjectStateHandle(
            project.downgrade(),
        ));
        let playback = cx.new(|_| crate::playback::PlaybackController::new());
        cx.set_global(crate::playback::PlaybackControllerHandle(
            playback.downgrade(),
        ));
        // Audio playback (audio-plan unit 3): owns the optional output
        // engine and the document→mixer diff. The engine starts lazily on
        // the first audio layer, so sessions without audio never open a
        // device; dropping the workspace (window close) shuts it down.
        let audio = cx.new(|_| crate::audio::AudioService::new());
        cx.set_global(crate::audio::AudioServiceHandle(audio.downgrade()));
        let audio_event_sub = cx.subscribe_in(
            &audio,
            window,
            |_this, _audio, event: &crate::audio::AudioServiceEvent, window, cx| {
                show_audio_event(event, window, cx);
            },
        );

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
        let project_event_sub = cx.subscribe_in(
            &project,
            window,
            |_this, _project, event: &crate::project_state::ProjectEvent, window, cx| {
                show_project_event(event, window, cx);
            },
        );
        // A project may ship its own arrangement (DOCK-9). Applying it is the
        // session's business, so the document state only announces it.
        let document_replaced_sub = cx.subscribe_in(
            &project,
            window,
            |this, _project, event: &crate::project_state::DocumentReplaced, _window, cx| {
                this.apply_project_layout(event.workspace_layout.as_ref(), cx);
            },
        );
        if let Some(error) = project.read(cx).startup_gpu_error().map(str::to_owned) {
            cx.defer_in(window, move |_this, window, cx| {
                show_project_event(
                    &crate::project_state::ProjectEvent::GpuInitializationFailed { error },
                    window,
                    cx,
                );
            });
        }

        let workspace = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.should_close_window(window, cx)
                })
                .unwrap_or(true)
        });

        // Detached windows follow the main window out of sight and back
        // (gpui exposes no hide, so following means minimizing them too).
        let minimize_sub = cx.observe_window_minimized(window, |minimized, _this, _window, cx| {
            crate::window_host::set_detached_minimized(minimized, cx);
        });

        Self {
            shell,
            playback,
            project,
            audio,
            audio_event_sub,
            window_title,
            title_sub,
            project_event_sub,
            minimize_sub,
            document_replaced_sub,
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

    /// Dispatches one command from a GPUI action callback.
    ///
    /// This is the only place [`AppShell::handle_command`] is called: every
    /// window's action handlers and the App-level fallback route here.
    pub fn dispatch_command(
        &mut self,
        cmd: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> CommandOutcome {
        let focused = cx
            .try_global::<panels::FocusedPanelGlobal>()
            .and_then(|global| global.0);
        self.shell.set_focused_instance(focused);
        let outcome = self.shell.handle_command(cmd);
        crate::trace::record(
            cx,
            crate::trace::TraceEntry {
                source: crate::trace::TraceSource::WorkspaceAction,
                command: Some(cmd),
                focused_instance: focused,
                handler: "RavelWorkspace::dispatch_command",
                outcome: Some(format!("{outcome:?}")),
            },
        );
        self.dispatch_outcome(cmd, outcome.clone(), window, cx);
        // The View and Workspace menus carry live checkboxes, and any command
        // may have moved a panel or switched a preset.
        cx.set_menus(build_menus(&self.shell));
        // Any command may have changed the arrangement, so this is where it
        // reaches disk. Unchanged documents write nothing, so the commands that
        // are not layout changes cost one serialization and no I/O.
        crate::layout_persist::save(&self.shell, cx);
        cx.notify();
        outcome
    }

    /// Saves the main window's arrangement as a named layout and persists the
    /// library (REQ-UI-005).
    pub fn save_current_layout_as(&mut self, name: String, cx: &mut Context<Self>) {
        self.shell.save_layout_as(name);
        crate::layout_persist::save(&self.shell, cx);
        cx.notify();
    }

    /// Applies a saved named layout to the main window.
    pub fn apply_custom_layout(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Err(error) = self.shell.apply_custom_layout(name) {
            tracing::warn!(%error, name, "could not apply the named layout");
            return;
        }
        cx.set_menus(build_menus(&self.shell));
        crate::layout_persist::save(&self.shell, cx);
        cx.notify();
    }

    /// Forgets a saved named layout.
    pub fn remove_custom_layout(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.shell.remove_custom_layout(name) {
            return;
        }
        crate::layout_persist::save(&self.shell, cx);
        cx.notify();
    }

    /// Workspace ▸ Manage Layouts…: save, apply, or forget a named layout, and
    /// set whether saved projects embed the current one.
    fn prompt_workspace_layouts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let session = cx.entity().downgrade();
        let form =
            cx.new(|cx| crate::workspace_layouts::WorkspaceLayoutsForm::new(session, window, cx));
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let content = form.clone();
            dialog
                .title(SharedString::from(t!("workspace.layouts.title")))
                .w(px(420.0))
                .content(move |body, _window, _cx| body.child(content.clone()))
                .footer(
                    DialogFooter::new().child(
                        Button::new("workspace-layouts-close")
                            .primary()
                            .label(SharedString::from(t!("ui.ok")))
                            .on_click(|_event, window, cx| window.close_dialog(cx)),
                    ),
                )
        });
    }

    /// The document to embed in the next project save, or `None` while the
    /// opt-in is off.
    fn layout_to_embed(&self, cx: &App) -> Option<ravel_ui::layout_doc::LayoutDocument> {
        crate::layout_persist::document_for_embedding(self.shell.layout(), cx)
    }

    /// Puts the session on the layout a just-opened project calls for.
    ///
    /// A project that embedded a layout gets it for this session only — the
    /// store refuses to fold it into the user's own default, so alternating
    /// between projects that embed one and projects that do not leaves
    /// `layout.toml` untouched. A project that embedded none usually changes
    /// nothing at all; it only forces a change when the *previous* project had
    /// one, in which case the session returns to the user's own arrangement.
    fn apply_project_layout(
        &mut self,
        embedded: Option<&ravel_ui::layout_doc::LayoutDocument>,
        cx: &mut Context<Self>,
    ) {
        let embedded = embedded.map(|document| &document.layout);
        let Some(target) = crate::layout_persist::layout_for_project(embedded, cx) else {
            return;
        };
        // The adopted layout assigns its own window ids, so the outgoing
        // detached windows would have nothing left to close them: close them
        // against the ids they still have.
        crate::window_host::close_all_detached(cx);
        let opened = self.shell.restore_layout(&target);
        cx.set_menus(build_menus(&self.shell));
        cx.notify();
        // Opening a window from inside this entity's update is not allowed, and
        // the hosts have to re-render from the new layout first anyway.
        cx.defer(move |cx| crate::window_host::open_restored(&opened, cx));
    }

    /// Undoes a detach whose window never opened: the instances go back to the
    /// main window's tree, which its host re-renders.
    fn restore_unopened_window(&mut self, window_id: ravel_ui::window::WindowId) {
        if let Err(error) = self.shell.layout_mut().absorb_window(window_id) {
            tracing::warn!(%error, window = window_id.0, "unopened window was not in the layout");
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
            self.request_project_action(PendingProjectAction::Quit, window, cx);
            return;
        }

        match outcome {
            CommandOutcome::DetachPanel { window_id, .. } => {
                let opened = self
                    .shell
                    .layout()
                    .window(window_id)
                    .cloned()
                    .is_some_and(|detached| crate::window_host::open(&detached, cx));
                if !opened {
                    // Nothing renders the moved instance now. Absorb the window
                    // back into the main tree, or the panel would exist only in
                    // the layout, with no window and no close button to
                    // recover it from.
                    self.restore_unopened_window(window_id);
                }
            }
            CommandOutcome::ReattachPanel { window_id, .. } => {
                // The instances are already back in the main window's tree; its
                // host re-renders them when this update notifies.
                crate::window_host::close(window_id, cx);
            }
            // Panel toggles and preset switches are layout changes: the shell
            // owns the effective layout and every window host re-renders the
            // tree it was given.
            CommandOutcome::Handled => {}
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
                | CommandId::LayerAddAudio
                | CommandId::LayerAddNull => {
                    if let Some(key) = cmd.layer_template_key() {
                        let layer = self
                            .project
                            .update(cx, |project, cx| project.add_layer_from_template(key, cx));
                        if let Some(layer) = layer
                            && let Some(timeline) = cx
                                .try_global::<crate::panels::TimelinePanelHandle>()
                                .and_then(|handle| handle.0.upgrade())
                        {
                            cx.defer(move |cx| {
                                timeline.update(cx, |timeline, cx| {
                                    timeline.select_layer(layer, cx);
                                });
                            });
                        }
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
                    self.request_project_action(PendingProjectAction::New, window, cx);
                }
                CommandId::FileSave => {
                    let path = self
                        .project
                        .read(cx)
                        .project_path()
                        .map(std::path::Path::to_path_buf);
                    match path {
                        Some(path) => {
                            let layout = self.layout_to_embed(cx);
                            self.project.update(cx, |project, cx| {
                                project.save_project_to(path, layout, cx);
                            });
                        }
                        // Never saved: Save behaves as Save As.
                        None => self.prompt_save_as(cx),
                    }
                }
                CommandId::FileSaveAs => self.prompt_save_as(cx),
                CommandId::FileImport => self.prompt_import(cx),
                CommandId::FileOpen => {
                    self.request_project_action(PendingProjectAction::Open, window, cx);
                }
                // Composition management (REQ-UI-013).
                CommandId::CompositionNew => self.prompt_new_composition(window, cx),
                CommandId::CompositionSettings => self.prompt_composition_settings(window, cx),
                CommandId::CompositionDuplicate => {
                    if let Some(comp) = panels::command_target_composition(cx) {
                        self.project.update(cx, |project, cx| {
                            project.duplicate_composition(comp, cx);
                        });
                    }
                }
                CommandId::CompositionDelete => self.prompt_delete_composition(window, cx),
                // Named layouts (REQ-UI-005) plus the embed opt-in.
                CommandId::WorkspaceManageLayouts => self.prompt_workspace_layouts(window, cx),
                CommandId::ToolSelect
                | CommandId::ToolPen
                | CommandId::ToolRect
                | CommandId::ToolEllipse
                | CommandId::ToolHand
                | CommandId::ToolZoom => {
                    if let Some(tool) = ravel_ui::ToolKind::from_command(cmd) {
                        let mut state = cx
                            .try_global::<panels::ToolState>()
                            .cloned()
                            .unwrap_or_default();
                        state.active = tool;
                        cx.set_global(state);
                    }
                }
                _ => {}
            },
        }
    }

    // ----- destructive project-action guard -----------------------------------

    /// Tears the workspace down: detached windows are views onto this session,
    /// so the main window closing takes them with it, and the main window's own
    /// registry entry goes with it — a handle to a closed window must never
    /// stay reachable (`MED-APP-01` is exactly that class of bug).
    fn close_the_workspace(&mut self, cx: &mut Context<Self>) {
        // The last chance to record the arrangement: window moves and splitter
        // drags only update the model, and a task spawned from here would never
        // be polled once the process is on its way out.
        crate::layout_persist::save_blocking(&self.shell, cx);
        crate::window_host::close_all_detached(cx);
        crate::window_host::unregister(self.shell.layout().main_window().id, cx);
    }

    /// Whether the main window may close: an unsaved document raises the guard
    /// dialog and cancels the close instead.
    pub fn should_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.project.read(cx).is_dirty() {
            self.close_the_workspace(cx);
            return true;
        }
        self.prompt_unsaved_changes(PendingProjectAction::CloseWindow, window, cx);
        false
    }

    fn request_project_action(
        &mut self,
        action: PendingProjectAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project.read(cx).is_dirty() {
            self.prompt_unsaved_changes(action, window, cx);
        } else {
            self.perform_project_action(action, window, cx);
        }
    }

    fn perform_project_action(
        &mut self,
        action: PendingProjectAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            PendingProjectAction::New => {
                self.project.update(cx, |project, cx| {
                    project.new_document(cx);
                });
            }
            PendingProjectAction::Open => self.prompt_open(cx),
            PendingProjectAction::Quit => {
                // Quit does not go through the window close handler, so the
                // arrangement is recorded here too.
                crate::layout_persist::save_blocking(&self.shell, cx);
                cx.quit();
            }
            PendingProjectAction::CloseWindow => {
                self.close_the_workspace(cx);
                window.remove_window();
            }
        }
    }

    fn prompt_unsaved_changes(
        &mut self,
        action: PendingProjectAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }

        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let save_workspace = workspace.clone();
            let discard_workspace = workspace.clone();
            let button_workspace = workspace.clone();
            dialog
                .title(SharedString::from(t!("project.unsaved.title")))
                .w(px(448.0))
                .content(|body, _window, _cx| {
                    body.child(SharedString::from(t!("project.unsaved.message")))
                })
                // Enter chooses the safe default (Save); Escape and the close
                // affordances keep the default cancel behavior.
                .on_ok(move |_event, window, cx| {
                    if save_workspace
                        .update(cx, |workspace, cx| {
                            workspace.save_before_project_action(action, window, cx);
                        })
                        .is_err()
                    {
                        tracing::warn!(
                            "workspace dropped before the unsaved-changes save was requested"
                        );
                    }
                    true
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("unsaved-cancel")
                                .label(SharedString::from(t!("ui.cancel")))
                                .on_click(|_event, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("unsaved-discard")
                                .label(SharedString::from(t!("project.unsaved.discard")))
                                // Discard is the only footer button with no
                                // keyboard route (Enter saves, Escape
                                // cancels), so the integration test has to
                                // click it. The selector lets the test read
                                // the button's real bounds instead of
                                // hard-coding a coordinate that only holds
                                // for one platform's font metrics. Compiles
                                // to a no-op without `test-support`.
                                .debug_selector(|| "unsaved-discard".into())
                                .on_click(move |_event, window, cx| {
                                    window.close_dialog(cx);
                                    if discard_workspace
                                        .update(cx, |workspace, cx| {
                                            workspace.perform_project_action(action, window, cx);
                                        })
                                        .is_err()
                                    {
                                        tracing::warn!(
                                            "workspace dropped before unsaved changes were discarded"
                                        );
                                    }
                                }),
                        )
                        .child(
                            Button::new("unsaved-save")
                                .primary()
                                .label(SharedString::from(t!("project.unsaved.save")))
                                .on_click(move |_event, window, cx| {
                                    if button_workspace
                                        .update(cx, |workspace, cx| {
                                            workspace.save_before_project_action(
                                                action, window, cx,
                                            );
                                        })
                                        .is_err()
                                    {
                                        tracing::warn!(
                                            "workspace dropped before the unsaved-changes save was requested"
                                        );
                                    }
                                    window.close_dialog(cx);
                                }),
                        ),
                )
        });
    }

    fn save_before_project_action(
        &mut self,
        action: PendingProjectAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self
            .project
            .read(cx)
            .project_path()
            .map(std::path::Path::to_path_buf);
        match path {
            Some(path) => Self::queue_guarded_save(
                self.project.downgrade(),
                cx.entity().downgrade(),
                window.window_handle(),
                action,
                path,
                self.layout_to_embed(cx),
                cx,
            ),
            None => self.prompt_save_as_before(action, window.window_handle(), cx),
        }
    }

    fn queue_guarded_save<C: AppContext>(
        project: WeakEntity<crate::project_state::ProjectState>,
        workspace: WeakEntity<Self>,
        window_handle: AnyWindowHandle,
        action: PendingProjectAction,
        path: std::path::PathBuf,
        workspace_layout: Option<ravel_ui::layout_doc::LayoutDocument>,
        cx: &mut C,
    ) {
        if project
            .update(cx, |project, cx| {
                project.save_project_to_then(
                    path,
                    workspace_layout,
                    move |outcome, cx| {
                        if outcome != crate::project_state::SaveOutcome::Saved {
                            if outcome == crate::project_state::SaveOutcome::SavedButDirty {
                                tracing::warn!(
                                    "project changed while saving; destructive action cancelled"
                                );
                            }
                            if matches!(
                                outcome,
                                crate::project_state::SaveOutcome::Failed
                                    | crate::project_state::SaveOutcome::SavedButDirty
                            ) {
                                let _ = window_handle.update(cx, |_root, window, cx| {
                                    let _ = workspace.update(cx, |workspace, cx| {
                                        workspace.prompt_unsaved_changes(action, window, cx);
                                    });
                                });
                            }
                            return;
                        }
                        if window_handle
                            .update(cx, |_root, window, cx| {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.perform_project_action(action, window, cx);
                                })
                            })
                            .is_err()
                        {
                            tracing::warn!(
                                "window closed before the saved project action could continue"
                            );
                        }
                    },
                    cx,
                );
            })
            .is_err()
        {
            tracing::warn!("project state dropped before guarded save was queued");
        }
    }

    // ----- composition management (REQ-UI-013) --------------------------------

    /// Composition ▸ New…: collect the settings in a dialog and create the
    /// composition only when it is confirmed.
    ///
    /// Initial values come from the active composition, else the project
    /// defaults in `manifest.json`, else 1920×1080 / 30fps / 300f. Creating on
    /// confirm rather than up front keeps this one undo step instead of
    /// "create, then correct".
    fn prompt_new_composition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A new composition inherits the active one's format, else the
        // fallback (1920×1080 / 30fps / 300f). The `manifest.json` project
        // defaults are not consulted: `ProjectState` does not retain the
        // loaded manifest, and its defaults are these same values.
        let name = next_composition_name(self.project.read(cx).document());
        let initial = match self.project.read(cx).active_composition(cx) {
            Some(active) => CompositionSettingsValue {
                name,
                ..CompositionSettingsValue::from_composition(active)
            },
            None => CompositionSettingsValue::fallback(name),
        };
        self.open_composition_dialog(
            initial,
            SharedString::from(t!("composition.dialog.new_title")),
            SharedString::from(t!("composition.dialog.create")),
            |project, settings, cx| {
                project.create_composition(settings, cx);
            },
            window,
            cx,
        );
    }

    /// Composition ▸ Settings…: edit the target composition's settings in a
    /// dialog. The Properties panel edits the same fields continuously; this is
    /// the explicit, one-undo-step path.
    fn prompt_composition_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comp) = panels::command_target_composition(cx) else {
            return;
        };
        let Some(initial) = self
            .project
            .read(cx)
            .document()
            .get_composition(comp)
            .map(|comp| CompositionSettingsValue::from_composition(comp))
        else {
            return;
        };
        self.open_composition_dialog(
            initial,
            SharedString::from(t!("composition.dialog.settings_title")),
            SharedString::from(t!("ui.ok")),
            move |project, settings, cx| {
                project.apply_composition_settings(comp, settings, cx);
            },
            window,
            cx,
        );
    }

    /// Open a composition dialog around [`CompositionForm`] and run `confirm`
    /// with the edited settings when it is accepted.
    ///
    /// The document is touched only on confirm, so a cancelled dialog leaves no
    /// undo step behind. A plain `Dialog` renders no buttons of its own (unlike
    /// `AlertDialog`), so the footer is built here.
    fn open_composition_dialog(
        &mut self,
        initial: CompositionSettingsValue,
        title: SharedString,
        ok_label: SharedString,
        confirm: impl Fn(
            &mut crate::project_state::ProjectState,
            CompositionSettingsValue,
            &mut Context<crate::project_state::ProjectState>,
        ) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let form = cx.new(|cx| CompositionForm::new(initial, window, cx));
        let project = self.project.downgrade();
        let confirm = std::rc::Rc::new(confirm);
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let content = form.clone();
            let ok_form = form.clone();
            let project = project.clone();
            let confirm = confirm.clone();
            let cancel_label = SharedString::from(t!("ui.cancel"));
            dialog
                .title(title.clone())
                .w(px(360.0))
                .content(move |body, _window, _cx| body.child(content.clone()))
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("composition-dialog-cancel")
                                .label(cancel_label)
                                .on_click(|_event, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("composition-dialog-ok")
                                .primary()
                                .label(ok_label.clone())
                                .on_click(move |_event, window, cx| {
                                    let settings = ok_form.read(cx).settings(cx);
                                    if project
                                        .update(cx, |project, cx| confirm(project, settings, cx))
                                        .is_err()
                                    {
                                        tracing::warn!(
                                            "project state dropped before the composition dialog was confirmed"
                                        );
                                    }
                                    window.close_dialog(cx);
                                }),
                        ),
                )
        });
    }

    /// Composition ▸ Delete: a composition holding layers is confirmed first;
    /// an empty one is deleted straight away (undo restores either).
    fn prompt_delete_composition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comp) = panels::command_target_composition(cx) else {
            return;
        };
        let layer_count = self
            .project
            .read(cx)
            .document()
            .get_composition(comp)
            .map(|comp| comp.layer_count())
            .unwrap_or(0);
        if layer_count == 0 {
            self.project.update(cx, |project, cx| {
                project.delete_composition(comp, cx);
            });
            return;
        }

        let project = self.project.downgrade();
        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            let project = project.clone();
            alert
                .confirm()
                .title(SharedString::from(t!("composition.dialog.delete_title")))
                .description(SharedString::from(t!("composition.dialog.delete_message")))
                .show_cancel(true)
                .on_ok(move |_event, _window, cx| {
                    if project
                        .update(cx, |project, cx| {
                            project.delete_composition(comp, cx);
                        })
                        .is_err()
                    {
                        tracing::warn!("project state dropped before the delete was confirmed");
                    }
                    true
                })
        });
    }

    /// File ▸ Save As…: prompt for a destination path, then save through
    /// [`crate::project_state::ProjectState`]. Cancelling the dialog is a
    /// no-op.
    fn prompt_save_as(&mut self, cx: &mut Context<Self>) {
        self.prompt_save_as_with_continuation(None, cx);
    }

    fn prompt_save_as_before(
        &mut self,
        action: PendingProjectAction,
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        self.prompt_save_as_with_continuation(Some((action, window_handle)), cx);
    }

    fn prompt_save_as_with_continuation(
        &mut self,
        continuation: Option<(PendingProjectAction, AnyWindowHandle)>,
        cx: &mut Context<Self>,
    ) {
        let dir = self
            .project
            .read(cx)
            .project_path()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        let receiver = cx.prompt_for_new_path(&dir, Some("project.ravprj"));
        let project = self.project.downgrade();
        cx.spawn(async move |this, cx| match receiver.await {
            Ok(Ok(Some(path))) => {
                let path = with_ravprj_extension(path);
                // The dialog was open while the user could still rearrange
                // panels, so the layout to embed is read now rather than
                // before the prompt.
                let layout = this
                    .update(cx, |this, cx| this.layout_to_embed(cx))
                    .ok()
                    .flatten();
                match continuation {
                    Some((action, window_handle)) => Self::queue_guarded_save(
                        project,
                        this,
                        window_handle,
                        action,
                        path,
                        layout,
                        cx,
                    ),
                    None => {
                        if project
                            .update(cx, |project, cx| {
                                project.save_project_to(path, layout, cx);
                            })
                            .is_err()
                        {
                            tracing::warn!("project state dropped before Save As completed");
                        }
                    }
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

    /// File ▸ Import…: pick one or more media files and import them into the
    /// project. Multi-select is allowed; the whole batch becomes one undo
    /// step inside [`crate::media::import`]. Cancelling is a no-op.
    fn prompt_import(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn(async move |_this, cx| match receiver.await {
            Ok(Ok(Some(paths))) => {
                cx.update(|cx| crate::media::import::import_paths(paths, cx));
            }
            // The dialog was cancelled (or the app is shutting down).
            Ok(Ok(None)) | Err(_) => {}
            Ok(Err(err)) => tracing::error!(%err, "import dialog failed"),
        })
        .detach();
    }
}

fn show_project_event(
    event: &crate::project_state::ProjectEvent,
    window: &mut Window,
    cx: &mut App,
) {
    use crate::project_state::ProjectEvent;

    let (kind, title, message) = match event {
        ProjectEvent::GpuInitializationFailed { error } => (
            NotificationType::Error,
            t!("project.notice.gpu_title"),
            format!("{}\n{error}", t!("project.notice.gpu_message")),
        ),
        ProjectEvent::SaveFailed { path, error } => (
            NotificationType::Error,
            t!("project.notice.save_title"),
            format!(
                "{}\n{}\n{error}",
                t!("project.notice.save_message"),
                path.display()
            ),
        ),
        ProjectEvent::SaveChangedDuringWrite { path } => (
            NotificationType::Warning,
            t!("project.notice.save_dirty_title"),
            format!(
                "{}\n{}",
                t!("project.notice.save_dirty_message"),
                path.display()
            ),
        ),
        ProjectEvent::OpenFailed {
            path,
            error,
            too_new,
        } => (
            NotificationType::Error,
            if *too_new {
                t!("project.notice.open_too_new_title")
            } else {
                t!("project.notice.open_title")
            },
            format!(
                "{}\n{}\n{error}",
                if *too_new {
                    t!("project.notice.open_too_new_message")
                } else {
                    t!("project.notice.open_message")
                },
                path.display()
            ),
        ),
        ProjectEvent::BackupRecovered { path, backup } => (
            NotificationType::Warning,
            t!("project.notice.recovered_title"),
            format!(
                "{}\n{}\n{}: {}",
                t!("project.notice.recovered_message"),
                path.display(),
                t!("project.notice.backup_path"),
                backup.display()
            ),
        ),
        ProjectEvent::MediaImportSkipped { failures } => {
            let details = failures
                .iter()
                .map(|failure| format!("{}: {}", failure.path.display(), failure.reason))
                .collect::<Vec<_>>()
                .join("\n");
            (
                NotificationType::Warning,
                t!("project.notice.import_title"),
                format!(
                    "{} ({})\n{details}",
                    t!("project.notice.import_message"),
                    failures.len()
                ),
            )
        }
    };
    window.push_notification(
        Notification::new()
            .with_type(kind)
            .title(SharedString::from(title))
            .message(SharedString::from(message))
            .autohide(false),
        cx,
    );
}

fn show_audio_event(event: &crate::audio::AudioServiceEvent, window: &mut Window, cx: &mut App) {
    let crate::audio::AudioServiceEvent::PreparationFailed { asset_id, error } = event;
    window.push_notification(
        Notification::new()
            .with_type(NotificationType::Warning)
            .title(SharedString::from(t!("audio.notice.prepare_title")))
            .message(SharedString::from(format!(
                "{}\n{asset_id}\n{error}",
                t!("audio.notice.prepare_message")
            )))
            .autohide(false),
        cx,
    );
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

#[cfg(test)]
mod tests {
    // `use gpui::*` pulls in gpui's `test` attribute macro; shadow it back
    // to the built-in one so `#[test]` resolves to the real one.
    use core::prelude::v1::test;

    #[test]
    fn playback_arrow_bindings_yield_to_text_inputs() {
        let bindings = super::build_keybindings(&ravel_ui::shell::AppShell::default());
        let step_forward = bindings
            .iter()
            .find(|binding| binding.action().as_any().is::<super::FrameStepForward>())
            .expect("default step-forward binding");

        assert_eq!(
            step_forward.predicate().unwrap().to_string(),
            "!Input",
            "workspace playback must not consume an Input's Right arrow"
        );
    }

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
