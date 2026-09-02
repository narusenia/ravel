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

use std::collections::HashSet;

use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::DialogFooter;
use gpui_component::menu::{APP_MENU_BAR_CONTEXT, AppMenuBar, POPUP_MENU_CONTEXT};
use gpui_component::notification::{Notification, NotificationType};
use gpui_component::{GlobalState, WindowExt as _};
use ravel_i18n::t;
use ravel_ui::command::CommandId;
use ravel_ui::keybindings::KeyChord;
use ravel_ui::panel::PanelKind;
use ravel_ui::shell::{AppShell, CommandOutcome};

use crate::composition_form::CompositionForm;
use crate::panels;
use crate::settings_dialog::{SettingsDialog, SettingsScope};

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
            FileExport,
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
            AppPreferences,
            KeyframeInterpolationBezier,
            KeyframeInterpolationLinear,
            KeyframeInterpolationStep,
            TimelineRevealAnimated,
            TimelineRevealAnimatedAdd,
            TimelineRevealAnchorPoint,
            TimelineRevealAnchorPointAdd,
            TimelineRevealPosition,
            TimelineRevealPositionAdd,
            TimelineRevealScale,
            TimelineRevealScaleAdd,
            TimelineRevealRotation,
            TimelineRevealRotationAdd,
            TimelineRevealOpacity,
            TimelineRevealOpacityAdd,
            TimelineRevealAudioGain,
            TimelineRevealAudioGainAdd,
            TimelineRevealModified,
            TimelineRevealModifiedAdd,
            TimelineRevealExpression,
            TimelineRevealExpressionAdd,
            TimelineSplitLayer,
            TimelineAlignLayerStart,
            TimelineAlignLayerEnd,
            TimelineGoToLayerIn,
            TimelineGoToLayerOut,
            ViewToggleOutliner,
            ViewToggleTimeline,
            ViewToggleNodeGraph,
            ViewToggleViewer,
            ViewToggleDopesheet,
            ViewToggleProperties,
            ViewToggleCurveEditor,
            ViewToggleScopes,
            ViewToggleMediaBin,
            ViewToggleTextEditor,
            ViewToggleShaderEditor,
            ViewToggleLuaConsole,
            ViewToggleRenderQueue,
            ViewToggleAttributeSpreadsheet,
            ViewToggleNodeParamValues,
            ViewCyclePreviewResolution,
            ViewerChannelRgb,
            ViewerChannelRed,
            ViewerChannelGreen,
            ViewerChannelBlue,
            ViewerChannelAlpha,
            ViewerPixelReadout,
            ViewerPixelReadoutFormat,
            ViewFit,
            PlaybackToggle,
            PlaybackStop,
            FrameStepForward,
            FrameStepBackward,
            PlaybackLoopIn,
            PlaybackLoopOut,
            PlaybackLoopClear,
            CompositionNew,
            CompositionSettings,
            CompositionDuplicate,
            CompositionDelete,
            ProjectSettings,
            ProjectExposedParameters,
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
            ToolPolygon,
            ToolStar,
            ToolHand,
            ToolZoom,
            NodeSearchPalette,
            NodeCollapseToSubnet,
            NodeExtractSubnet,
            NodeAutoLayout,
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

/// The GPUI action that carries `cmd`, generated from the same table the
/// actions are declared from.
///
/// A menu that wants to send a command through the focus hierarchy uses this
/// rather than naming an action type itself: a second Command↔Action list
/// would be exactly the drift the table exists to prevent
/// (`.agents/rules/gpui.md`).
pub fn command_action(cmd: CommandId) -> Box<dyn Action> {
    macro_rules! map {
        ($($Action:ident),+ $(,)?) => {
            match cmd { $(CommandId::$Action => Box::new($Action),)+ }
        };
    }
    for_each_command!(map)
}

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
/// ravel-ui: `Cmd+Shift+Z`  →  gpui: `secondary-shift-z`
///
/// **The primary modifier is written `secondary-`, not `cmd-`.** `KeyChord`
/// spells it `Cmd` on every platform ([`KeyChord`]'s `Display`), but to gpui
/// `cmd` / `super` / `win` all mean `Modifiers::platform`, which on Windows is
/// the Windows key — so `Cmd+S` became `Win+S` there and no shortcut in the
/// application fired. gpui's own `secondary-` resolves to the platform
/// modifier on macOS and to Control everywhere else, which is exactly what
/// `KeyChord::command` means (`keybindings/mod.rs`: "rendered as `Cmd` on
/// macOS and `Ctrl` on Windows/Linux").
///
/// `control` stays `ctrl-`. A chord carrying both would collapse to one
/// modifier off macOS; nothing in the assets or in [`PANEL_BINDINGS`] does
/// that, and `a_chord_cannot_hold_both_primary_modifiers` keeps it that way.
fn chord_to_gpui_string(chord: &KeyChord) -> String {
    chord
        .to_string()
        .replace('+', "-")
        .to_lowercase()
        .replace("cmd-", "secondary-")
}

// ---------------------------------------------------------------------------
// Keybindings — derived from the headless binding table
// ---------------------------------------------------------------------------

/// A binding that exists only in code, scoped to a panel's key context.
///
/// The keybinding assets have no way to express a key context
/// (`docs/dev/add-command.md`), so a shortcut that must only fire inside one
/// panel cannot live in `default.toml` — and, for the same reason, cannot be
/// reassigned from a user file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelBinding {
    /// The command the chord dispatches.
    pub command: CommandId,
    /// The chord, written the way the keybinding assets write it (`"Cmd+D"`).
    pub chord: &'static str,
    /// The panel the context belongs to, for naming it in the UI.
    pub panel: PanelKind,
    /// The GPUI key context the binding is scoped to. Paired with `panel` by
    /// `panel_bindings_name_their_panels_key_context`.
    pub context: &'static str,
}

/// Every binding that exists only in code.
///
/// **One table, two readers**: [`build_keybindings`] registers it with GPUI and
/// [`crate::keybindings`] shows it in the Preferences list, which is also what
/// lets that list say a command like `tool.pen` is bound rather than reporting
/// it as unassigned. A second copy of this information would drift, for the same
/// reason `for_each_command!` is a single table.
///
/// It is also the reserved list a user keybinding file is refused
/// ([`panel_bound_commands`]): these chords are context-scoped by design, and
/// re-binding them from a file would silently promote them to global.
///
/// Order is the order GPUI receives them, which
/// `node_editor_keybindings_are_context_scoped` asserts verbatim.
pub const PANEL_BINDINGS: &[PanelBinding] = &[
    PanelBinding {
        command: CommandId::EditDuplicate,
        chord: "Cmd+D",
        panel: PanelKind::NodeGraph,
        context: panels::node_editor::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::ViewFit,
        chord: "F",
        panel: PanelKind::NodeGraph,
        context: panels::node_editor::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::NodeSearchPalette,
        chord: "Tab",
        panel: PanelKind::NodeGraph,
        context: panels::node_editor::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::EditDelete,
        chord: "Delete",
        panel: PanelKind::NodeGraph,
        context: panels::node_editor::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::EditDelete,
        chord: "Backspace",
        panel: PanelKind::NodeGraph,
        context: panels::node_editor::KEY_CONTEXT,
    },
    // Houdini's alignment key, context-scoped for the same reason as the rest
    // of this block: it acts on the node editor's selection and means nothing
    // anywhere else.
    PanelBinding {
        command: CommandId::NodeAutoLayout,
        chord: "L",
        panel: PanelKind::NodeGraph,
        context: panels::node_editor::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::EditDelete,
        chord: "Delete",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::EditDelete,
        chord: "Backspace",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    // The same chord the node editor uses, because it is the same command on
    // the same kind of selection — duplicating layers is what Cmd+D means with
    // the Timeline focused.
    PanelBinding {
        command: CommandId::EditDuplicate,
        chord: "Cmd+D",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    // After Effects' reveal keys, Timeline key context
    // (`refactor-plan-0808.md`, unit 5). The unmodified chord replaces the
    // current filter, its `Shift` twin adds to it, and `UU` / `EE` — which a
    // `KeyChord` cannot express — are `Alt+U` / `Alt+E`. They collide with
    // nothing: the Viewer's tool keys and the node editor's `L` live in other
    // key contexts.
    PanelBinding {
        command: CommandId::TimelineRevealAnimated,
        chord: "U",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealAnimatedAdd,
        chord: "Shift+U",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealAnchorPoint,
        chord: "A",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealAnchorPointAdd,
        chord: "Shift+A",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealPosition,
        chord: "P",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealPositionAdd,
        chord: "Shift+P",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealScale,
        chord: "S",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealScaleAdd,
        chord: "Shift+S",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealRotation,
        chord: "R",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealRotationAdd,
        chord: "Shift+R",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealOpacity,
        chord: "T",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealOpacityAdd,
        chord: "Shift+T",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealAudioGain,
        chord: "L",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealAudioGainAdd,
        chord: "Shift+L",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealModified,
        chord: "Alt+U",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealModifiedAdd,
        chord: "Alt+Shift+U",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealExpression,
        chord: "Alt+E",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineRevealExpressionAdd,
        chord: "Alt+Shift+E",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    // After Effects' playhead-relative layer timing (`refactor-plan-0808.md`,
    // unit 7). Every one of them reads the Timeline's layer selection and its
    // playhead, so they are context-scoped rather than asset bindings: `[`,
    // `]`, `I` and `O` unqualified would fire from the Viewer and the node
    // editor as well.
    //
    // `Cmd+Shift+D` **shadows the global `panel.detach`** while the Timeline
    // holds focus (a deeper key context wins). Split is what that chord means
    // in an AE-shaped timeline, detaching still works from every other panel,
    // and `panel.detach` is an asset binding a user can move.
    PanelBinding {
        command: CommandId::TimelineSplitLayer,
        chord: "Cmd+Shift+D",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineAlignLayerStart,
        chord: "[",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineAlignLayerEnd,
        chord: "]",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineGoToLayerIn,
        chord: "I",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::TimelineGoToLayerOut,
        chord: "O",
        panel: PanelKind::Timeline,
        context: panels::timeline::KEY_CONTEXT,
    },
    // Tool shortcuts (Viewer key context, REQ-UI-011 unit 2).
    PanelBinding {
        command: CommandId::ToolSelect,
        chord: "V",
        panel: PanelKind::Viewer,
        context: panels::viewer::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::ToolPen,
        chord: "P",
        panel: PanelKind::Viewer,
        context: panels::viewer::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::ToolRect,
        chord: "R",
        panel: PanelKind::Viewer,
        context: panels::viewer::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::ToolEllipse,
        chord: "E",
        panel: PanelKind::Viewer,
        context: panels::viewer::KEY_CONTEXT,
    },
    // Polygon and Star are radially symmetric shape tools; `G` (polyGon, since
    // `P` is the Pen) and `S` are free of every other Viewer chord and of every
    // global binding.
    PanelBinding {
        command: CommandId::ToolPolygon,
        chord: "G",
        panel: PanelKind::Viewer,
        context: panels::viewer::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::ToolStar,
        chord: "S",
        panel: PanelKind::Viewer,
        context: panels::viewer::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::ToolHand,
        chord: "H",
        panel: PanelKind::Viewer,
        context: panels::viewer::KEY_CONTEXT,
    },
    PanelBinding {
        command: CommandId::ToolZoom,
        chord: "Z",
        panel: PanelKind::Viewer,
        context: panels::viewer::KEY_CONTEXT,
    },
];

/// The commands [`PANEL_BINDINGS`] binds, which a user keybinding file must not
/// reassign.
///
/// Derived from the table rather than listed again, so a panel shortcut added
/// there becomes un-overridable in the same edit.
pub fn panel_bound_commands() -> HashSet<CommandId> {
    PANEL_BINDINGS
        .iter()
        .map(|binding| binding.command)
        .collect()
}

/// Where a modal's top edge goes, as a share of the viewport height.
///
/// gpui-component defaults to `1/10`, which reads as pinned to the top of the
/// window rather than presented in it. A dialog cannot be *centred* from here:
/// its height is content-driven and only known after layout, and `margin_top`
/// is the only lever the widget offers. A quarter down puts a typical dialog
/// close to the optical centre while leaving room for one three times taller —
/// past that the default is the safer number, which is why the settings screen
/// keeps it.
const DIALOG_TOP_FRACTION: f32 = 4.0;

/// The `margin_top` for a dialog whose height is a modest share of the window.
fn dialog_margin_top(window: &Window) -> Pixels {
    window.viewport_size().height / DIALOG_TOP_FRACTION
}

/// The context predicate every asset-derived workspace binding carries.
///
/// A workspace command must yield to whatever owns the keyboard right now:
///
/// - a focused text input, whose own `Input`-context actions own the arrows,
///   editing, the clipboard chords and Space while typing;
/// - an open menu, whose `PopupMenu` / `AppMenuBar` actions own the arrows,
///   Enter and Escape.
///
/// Yielding has to be spelled out here because gpui resolves a tie by
/// **registration order** (`Keymap::bindings_for_input` sorts by context depth,
/// then by binding index) and Ravel binds after `gpui_component::init`. Both
/// predicates match at the menu's own node, so without the negation the
/// workspace binding wins and `MED-APP-31`'s symptom appears: arrows step the
/// playhead instead of walking the menu, and the menu closes under the user.
///
/// A negated context disables the binding while that context is anywhere in
/// the stack, so **no** workspace chord fires while a menu is open — Space
/// does not toggle playback either. That is the intent: an open menu is modal
/// to the keyboard.
pub fn workspace_binding_context() -> String {
    yield_to_open_menus("!Input")
}

/// `context` narrowed so it stops matching while a text input or a menu owns
/// the keyboard.
///
/// A panel-scoped binding needs the `!Input` half as much as a workspace one
/// does: the Timeline alone binds `U` / `A` / `P` / `S` / `R` / `T` / `L`,
/// `I`, `O`, `[` and `]`, and it also hosts the timecode field, the tempo
/// fields and the inline value editors. Without it, typing a letter into one
/// of those fires the panel's command instead. That is `MED-APP-16` again,
/// one context deeper than where it was first fixed.
pub fn panel_binding_context(context: &str) -> String {
    yield_to_open_menus(&format!("{context} && !Input"))
}

/// `context` narrowed so it stops matching while a menu is open.
///
/// Applies to the panel-scoped bindings too: a popup is a child of the panel
/// that opened it, so the panel's own key context is still on the stack while
/// its menu is up, and `L` would lay the graph out behind an open menu.
fn yield_to_open_menus(context: &str) -> String {
    format!("{context} && !{POPUP_MENU_CONTEXT} && !{APP_MENU_BAR_CONTEXT}")
}

/// Build GPUI keybindings from the headless table and panel-local contexts.
pub fn build_keybindings(shell: &AppShell) -> Vec<KeyBinding> {
    let mut out = Vec::new();
    let context = workspace_binding_context();
    for (chord, cmd) in shell.keybindings().iter() {
        let gpui_chord = chord_to_gpui_string(chord);
        macro_rules! bind {
            ($($Action:ident),+ $(,)?) => {
                match cmd {
                    $(CommandId::$Action => {
                        out.push(KeyBinding::new(&gpui_chord, $Action, Some(&context)));
                    })+
                }
            };
        }
        for_each_command!(bind);
    }
    for binding in PANEL_BINDINGS {
        // A chord the table spells wrong would otherwise vanish silently. It
        // cannot happen in a checked-in table — `panel_binding_chords_parse`
        // fails the suite first — so this only has to be loud, not fatal.
        let Ok(chord) = binding.chord.parse::<KeyChord>() else {
            tracing::error!(
                chord = binding.chord,
                command = %binding.command,
                "PANEL_BINDINGS holds an unparseable chord; the shortcut is not registered"
            );
            continue;
        };
        let gpui_chord = chord_to_gpui_string(&chord);
        let context = panel_binding_context(binding.context);
        macro_rules! bind_panel {
            ($($Action:ident),+ $(,)?) => {
                match binding.command {
                    $(CommandId::$Action => {
                        out.push(KeyBinding::new(&gpui_chord, $Action, Some(&context)));
                    })+
                }
            };
        }
        for_each_command!(bind_panel);
    }
    out
}

// ---------------------------------------------------------------------------
// Menus — derived from the headless MenuBar model
// ---------------------------------------------------------------------------

/// Convert a headless MenuItem to a GPUI MenuItem.
fn convert_menu_item(item: &ravel_ui::menu::MenuItem) -> gpui::MenuItem {
    match item {
        ravel_ui::menu::MenuItem::Action { command, check } => {
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
            let item: gpui::MenuItem = for_each_command!(to_gpui_action);
            // The checkbox the headless model tracks (panel toggles, the active
            // workspace preset). `gpui::MenuItem::Action` carries it, the macOS
            // menu draws it as an item state, and `Menu::owned` carries it into
            // the in-window bar — so this one conversion is the only place the
            // check has to be honoured.
            match check {
                Some(checked) => item.checked(*checked),
                None => item,
            }
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

/// The in-window application menu bar, off macOS. See [`install_menus`].
struct AppMenuBarGlobal(Entity<AppMenuBar>);

impl Global for AppMenuBarGlobal {}

/// The in-window menu bar entity, once [`install_menus`] has created it.
///
/// Durable shared application state — one bar for the application's lifetime,
/// like [`crate::window_host::WindowRegistry`] — so a Global is the right
/// mechanism here.
pub(crate) fn app_menu_bar(cx: &App) -> Option<Entity<AppMenuBar>> {
    Some(cx.try_global::<AppMenuBarGlobal>()?.0.clone())
}

/// Publishes one menu snapshot to every place a menu is drawn.
///
/// The single entry point for menus: `App::set_menus` for the macOS menu bar,
/// and gpui-component's [`GlobalState`] snapshot for [`AppMenuBar`], the
/// in-window bar the title bar draws where no OS menu bar exists (`set_menus`
/// is implemented on macOS and the test platform only, so on Windows and Linux
/// it reaches nothing and the whole hierarchy would be unreachable).
///
/// Both are fed from the same [`build_menus`] — one menu table, one
/// Command↔Action mapping, only the drawing differs. Unlike the macOS bar the
/// in-window one holds a snapshot, so every caller that used to refresh
/// `set_menus` has to come through here.
pub fn install_menus(shell: &AppShell, cx: &mut App) {
    // `gpui::Menu` owns boxed actions and is not `Clone`, so the two consumers
    // each get their own build.
    cx.set_menus(build_menus(shell));
    // The synthetic macOS application menu `build_menus` prepends is dropped
    // here: `FileQuit` already sits in the headless File menu and `HelpAbout`
    // in Help (`ravel_ui::menu`), so keeping it would show Quit and About
    // twice in the in-window bar.
    let owned = build_menus(shell)
        .into_iter()
        .skip(1)
        .map(gpui::Menu::owned)
        .collect();
    GlobalState::global_mut(cx).set_app_menus(owned);

    let bar = match app_menu_bar(cx) {
        Some(bar) => bar,
        None => {
            let bar = AppMenuBar::new(cx);
            cx.set_global(AppMenuBarGlobal(bar.clone()));
            bar
        }
    };
    bar.update(cx, |bar, cx| bar.reload(cx));
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
    /// Strong owner of the render queue; dropping the workspace on window
    /// close cancels what it was still working on (see
    /// [`crate::export::RenderService`]'s note on a discarded queue).
    render: Entity<crate::export::RenderService>,
    #[allow(dead_code)]
    render_event_sub: Subscription,
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
    /// Rebuilds the menu bar after a settings change (a language switch).
    #[allow(dead_code)]
    settings_sub: Subscription,
}

/// Destructive action resumed after the user resolves unsaved changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingProjectAction {
    New,
    Open,
    Quit,
    CloseWindow,
}

/// The window renderer's own GPU context, when it has one to share.
///
/// `None` on a renderer that is not wgpu-backed (macOS is Metal-native, and
/// Windows without the `wgpu` feature is D3D11-native), before the renderer has
/// acquired its context, and while a lost device recovers. Every one of those
/// leaves Ravel to choose its own device, which is what it did before this
/// existed.
///
/// The fork hands the four objects back through `Box<dyn Any>` so that `gpui`
/// need not name `wgpu` in its own signatures; unpacking them here is the price
/// of that, and a failed downcast means the fork changed shape rather than that
/// the device is unavailable — hence the log.
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
fn host_gpu_context(window: &Window, cx: &mut gpui::App) -> Option<ravel_gpu::GpuContext> {
    use std::sync::Arc;

    if window.gpu_device_lost().unwrap_or(false) {
        return None;
    }
    let boxed = window.gpu_context_full()?;
    let Ok(parts) = boxed.downcast::<(
        wgpu::Instance,
        wgpu::Adapter,
        Arc<wgpu::Device>,
        Arc<wgpu::Queue>,
    )>() else {
        tracing::warn!("the renderer's GPU context had an unexpected shape");
        return None;
    };
    let (instance, adapter, device, queue) = *parts;
    // `context_from_wgpu` takes the objects by value; the renderer keeps its
    // own `Arc` clones, so both sides stay alive independently.
    let context = ravel_gpu::interop::context_from_wgpu(
        instance,
        adapter,
        (*device).clone(),
        (*queue).clone(),
    );
    cx.set_global(AdoptedHostDevice((*device).clone()));
    Some(context)
}

/// The device Ravel adopted from the window renderer, kept so the viewer can
/// tell it apart from the one the renderer is using *now*.
///
/// **A recovered renderer is a different device.** `WgpuRenderer::recover()`
/// builds a whole new `WgpuContext` after a device loss, while Ravel keeps the
/// one it adopted at startup; `gpu_device_lost()` then reads `false` again and
/// says nothing about the swap. Handing the renderer a texture from the dead
/// device is undefined and trips wgpu's uncaptured-error handler, so the viewer
/// compares identity against this before every surface paint.
///
/// Durable, window-independent state for the whole session — the pipeline is
/// built on this device once and never rebuilt (`issues/high/HIGH-33`).
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
pub struct AdoptedHostDevice(pub wgpu::Device);

#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
impl gpui::Global for AdoptedHostDevice {}

/// Whether the renderer is still on the device Ravel adopted.
///
/// `false` when it never adopted one, when the renderer has no context to
/// report, or when it came back from a loss on a new device.
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
pub fn host_device_unchanged(window: &Window, cx: &gpui::App) -> bool {
    host_context(window, cx) == HostContext::Same
}

/// The renderer's context right now, placed against the device Ravel adopted.
///
/// The one reader of the fork's context on the observation side — the surface
/// paint guard and the recovery coordinator ask the same question and must not
/// answer it two ways. [`host_gpu_context`] unpacks the same tuple because it
/// needs all four objects, not just the identity.
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
fn host_context(window: &Window, cx: &gpui::App) -> HostContext {
    use std::sync::Arc;

    let Some(boxed) = window.gpu_context_full() else {
        return HostContext::Absent;
    };
    let Ok(parts) = boxed.downcast::<(
        wgpu::Instance,
        wgpu::Adapter,
        Arc<wgpu::Device>,
        Arc<wgpu::Queue>,
    )>() else {
        // Not logged here: this runs on every surface paint and on every poll,
        // and the adoption path already reports an unexpected shape once. A
        // shape this code cannot read is a context it cannot have, which is
        // what `Absent` means — treating it as a *replacement* would ask the
        // adoption path to unpack the same tuple it just failed on.
        return HostContext::Absent;
    };
    match cx.try_global::<AdoptedHostDevice>() {
        Some(adopted) if *parts.2 == adopted.0 => HostContext::Same,
        _ => HostContext::Replaced,
    }
}

/// Whether the renderer Ravel adopted its device from reports that device lost.
///
/// This is an observation helper for the existing Viewer surface fallback; it
/// does not alter the pure paint guard.
///
/// **A device identity mismatch is deliberately not a loss here.** The surface
/// guard treats it as one because painting across devices is unsafe either way,
/// but a second window on another GPU mismatches from the start and its device
/// never died — announcing a loss there would tell the user to restart over a
/// perfectly healthy session. The renderer's own flag is the only signal that
/// says "this device died", so a loss that flips and recovers between two
/// paints goes unannounced rather than announced wrongly. `GPULOSS-3` replaces
/// this with a real epoch once the adopted device can be re-adopted.
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
pub fn host_device_loss_detected(window: &Window, cx: &gpui::App) -> bool {
    cx.try_global::<AdoptedHostDevice>().is_some() && window.gpu_device_lost().unwrap_or(false)
}

/// The renderer's context, as the recovery coordinator sees it next to the
/// device Ravel adopted (`GPULOSS-3`).
///
/// Platform-independent on purpose: reading it needs the fork's
/// `cfg(linux / freebsd / windows)` methods, but *deciding* on it does not,
/// and the decision is the part that can be tested on the machine this is
/// written on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostContext {
    /// The renderer has no context to report: it has not acquired one, it is
    /// not wgpu-backed, a lost device is still recovering — or it handed back
    /// a shape this code cannot read.
    Absent,
    /// The device Ravel adopted, and evaluates on.
    Same,
    /// A device other than the adopted one. After a recovery that is the
    /// replacement; it is equally what a window on a second GPU reports, and
    /// what a session that adopted no device at all sees, so on its own it is
    /// not a reason to do anything.
    Replaced,
}

/// One poll's reading of the window Ravel adopted its device from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostDeviceObservation {
    /// Whether Ravel is evaluating on a device this window's renderer lent it.
    pub adopted: bool,
    /// The renderer's own device-lost flag (`None`, "the backend cannot know",
    /// read as `false` — the same asymmetry [`host_device_loss_detected`]
    /// uses).
    pub lost: bool,
    /// Which device the renderer reports right now.
    pub context: HostContext,
}

/// What one observation asks the recovery coordinator to do (`GPULOSS-3`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRecoveryStep {
    /// Nothing: either Ravel evaluates on a device of its own, or the renderer
    /// is still on the one it lent.
    Nothing,
    /// Keep the zero-copy surface off, and ask the window to redraw.
    ///
    /// The redraw is not cosmetic, and the reason differs by platform:
    ///
    /// * **Linux / FreeBSD**: the renderer is rebuilt *inside* the draw
    ///   (`WgpuRenderer::recover` is called from `PlatformWindow::draw` in the
    ///   x11 and wayland windows), so an idle window with a lost device never
    ///   recovers on its own and never produces the replacement this step
    ///   waits for;
    /// * **Windows**: the rebuild happens earlier, in the device-lost window
    ///   message, but the fork then sets `force_render_after_recovery` so the
    ///   *next* draw is the one that presents the recovered renderer. An idle
    ///   window has no next draw until something asks for one.
    ///
    /// Either way the redraw is what moves the recovery forward, which is why
    /// this step owns it rather than leaving it to whoever repaints next.
    Suspend,
    /// Adopt the device the renderer came back on and rebuild the evaluation
    /// pipeline against it.
    Readopt,
}

/// Decide what to do about the adopted device from one observation.
///
/// Pure, and outside the platform `cfg` that everything reading a window is
/// inside, because a decision compiled on three operating systems Ravel is not
/// developed on is one no local test can reach. The order of the tests is the
/// design:
///
/// 1. **A session that adopted nothing is not this coordinator's business.**
///    It runs on a device Ravel created, whose loss travels through its own
///    callback and settles on the CPU fallback (`GPULOSS-4`) — and its
///    renderer reports [`HostContext::Replaced`] from the first frame, because
///    there is nothing for it to match. Testing identity before adoption would
///    read that as a recovery and move the whole pipeline onto the renderer's
///    device mid-session.
/// 2. **The renderer's own flag outranks the identity.** While it reads lost
///    the recovery has not finished, whatever context is reported alongside
///    it, so there is nothing safe to adopt yet.
/// 3. **The same device needs no swap.** A loss that flipped and recovered
///    onto the same device between two polls leaves the pipeline valid;
///    rebuilding it would throw away every cache in the session to arrive
///    back where it started.
///
/// [`HostContext::Absent`] suspends rather than does nothing: the renderer has
/// no device to report, so the one Ravel holds is not the renderer's current
/// one, and that is precisely when zero-copy is unsafe.
pub fn host_recovery_step(observation: HostDeviceObservation) -> HostRecoveryStep {
    if !observation.adopted {
        return HostRecoveryStep::Nothing;
    }
    if observation.lost {
        return HostRecoveryStep::Suspend;
    }
    match observation.context {
        HostContext::Absent => HostRecoveryStep::Suspend,
        HostContext::Same => HostRecoveryStep::Nothing,
        HostContext::Replaced => HostRecoveryStep::Readopt,
    }
}

/// How often the coordinator asks the adopted window's renderer where its
/// device stands (`GPULOSS-3`).
///
/// **A timer and not the paint**, because the paint stops. An idle window does
/// not repaint, the worker keeps submitting to the dead device for as long as
/// nobody looks, and the recovery itself needs a draw to make progress (see
/// [`HostRecoveryStep::Suspend`] for the per-platform detail) — so the loss of
/// an idle window is not repaired by anything either. The paint guard still
/// reacts within one frame when a frame is in hand (`ZC-8`); this covers the
/// case where nothing is being drawn at all.
///
/// One second, reasoned rather than measured: the events are a driver reset
/// (seconds), a renderer rebuild that needs a further draw, and an evaluation
/// pipeline rebuild that dwarfs a second on its own, so a shorter interval buys
/// nothing a user can perceive. A poll costs one flag read plus — only when the
/// renderer has a context — an `Arc` clone of four handles, so it is not worth
/// stretching further either.
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
const HOST_DEVICE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// One reading of the window Ravel adopted its device from.
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
fn observe_host_device(window: &Window, cx: &gpui::App) -> HostDeviceObservation {
    HostDeviceObservation {
        adopted: cx.try_global::<AdoptedHostDevice>().is_some(),
        lost: window.gpu_device_lost().unwrap_or(false),
        context: host_context(window, cx),
    }
}

/// Poll the window Ravel adopted its device from, and re-adopt the device its
/// renderer recovers onto (`GPULOSS-3`, `issues/high/HIGH-33`).
///
/// **This window and no other.** The device came from this renderer, so a
/// change of identity *here* is a recovery; the same reading from a second
/// window on another GPU is just a second GPU, and re-adopting that one would
/// migrate the whole session onto a device the first window cannot sample
/// (which is why [`host_device_loss_detected`] refuses to call a mismatch a
/// loss). Window lifecycle beyond this one is `GPULOSS-5`.
///
/// The task ends when the window does: the session's device is the one this
/// window lent, and `AdoptedHostDevice` outlives neither.
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
fn spawn_host_device_recovery(
    window: &Window,
    project: WeakEntity<crate::project_state::ProjectState>,
    cx: &mut Context<RavelWorkspace>,
) {
    let handle = window.window_handle();
    cx.spawn(async move |_this, cx| {
        loop {
            cx.background_executor()
                .timer(HOST_DEVICE_POLL_INTERVAL)
                .await;
            let polled = handle.update(cx, |_root, window, cx| {
                match host_recovery_step(observe_host_device(window, cx)) {
                    HostRecoveryStep::Nothing => Ok(()),
                    HostRecoveryStep::Suspend => {
                        // GPUI's recovery runs in the draw, so an idle window
                        // has to be asked for one or the replacement device
                        // this is waiting for is never built.
                        window.refresh();
                        project.update(cx, |project, cx| {
                            // Idempotent by construction:
                            // `configure_viewer_surface` compares before it
                            // writes, so repeating it every second is a load
                            // and a branch. No flag remembers that it was
                            // done — a second authority on a fact
                            // `ProjectState` already holds is the bug this
                            // shape avoids.
                            project.configure_viewer_surface(false, cx);
                        })
                    }
                    HostRecoveryStep::Readopt => match host_gpu_context(window, cx) {
                        // The adoption path, reused whole: it unpacks the
                        // renderer's four objects, wraps them with
                        // `interop::context_from_wgpu` and **replaces**
                        // `AdoptedHostDevice`, so the old device is dropped
                        // here rather than referenced for the rest of the
                        // session. A second implementation of those three
                        // steps is how the global would be left pointing at
                        // the dead one.
                        Some(gpu) => {
                            tracing::warn!(
                                "the window renderer recovered onto a new GPU device; rebuilding \
                                 the evaluation pipeline on it"
                            );
                            project.update(cx, |project, cx| {
                                // The `false` case (a swap already in flight)
                                // needs nothing from here: it is refused
                                // *inside*, where the capability is left off
                                // for the swap that is already running to
                                // restore.
                                project.recover_on_replacement_gpu(gpu, cx);
                            })
                        }
                        // The renderer changed shape, or gave up its context
                        // between the observation and this call. Either way
                        // there is nothing to adopt; the next poll reads a
                        // context that is `Absent` and suspends.
                        None => Ok(()),
                    },
                }
                .is_ok()
            });
            // The window closed, or the session went with it.
            if !matches!(polled, Ok(true)) {
                break;
            }
        }
    })
    .detach();
}

/// macOS has no wgpu renderer to adopt from — `gpui_macos` is Metal-native, so
/// Ravel picks its own device and `interop::context_from_native` checks the two
/// landed on the same one (`ZC-2`).
///
/// **And no way to ask that renderer about a loss.** `gpu_device_lost()` and
/// `gpu_context_full()` exist on the fork's `PlatformWindow` only under
/// `cfg(linux / freebsd / windows)`; `gpui_macos` implements neither, so there
/// is no call to make rather than an answer of `None`. Two consequences, and
/// this unit settles both on the safe side (`GPULOSS-4`):
///
/// * a loss or a recreation of GPUI's **own** Metal device is not detected by
///   Ravel at all, and nothing in the tree claims otherwise. Whether the fork
///   should expose a native loss status is `MED-APP-40`, deliberately left
///   open;
/// * there is no route to a replacement device either, which is why a
///   self-owned loss disables zero-copy and stays on the CPU fallback instead
///   of restarting the worker
///   ([`ProjectState::report_gpu_device_loss`](crate::project_state::ProjectState::report_gpu_device_loss)).
#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "windows")))]
fn host_gpu_context(_window: &Window, _cx: &mut gpui::App) -> Option<ravel_gpu::GpuContext> {
    None
}

impl RavelWorkspace {
    /// Builds the session inside the main window.
    ///
    /// The window is the main one because the observers registered here (OS
    /// title, notifications, minimize follow) belong to it; the session itself
    /// is window-independent and outlives every individual pane.
    pub fn new(shell: AppShell, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // REQ-GPU-001: run the evaluation pipeline on the window renderer's
        // own device when it has one to give. `GPUBK-9` fixed the entry point
        // for this years before there was a caller — this is that caller.
        let host_gpu = host_gpu_context(window, cx);
        let adopted_host_gpu = host_gpu.is_some();
        let project =
            cx.new(|cx| crate::project_state::ProjectState::new_on_host_gpu(host_gpu, cx));
        cx.set_global(crate::project_state::ProjectStateHandle(
            project.downgrade(),
        ));
        // Whether this window's renderer can sample Ravel's textures. **This
        // is the one place that differs per platform** — how the host's device
        // is obtained. Everything downstream (publishing a GPU frame, painting
        // it, retiring it, falling back) is common code.
        //
        // macOS asks the Metal renderer for its device and checks it is the
        // one Ravel runs on; the wgpu-backed platforms need no check because
        // the device came from the toolkit in the first place. Missing handles
        // and mismatches keep the CPU fallback available.
        //
        // **The macOS arm answers a question and keeps nothing.** The native
        // pointer is borrowed for the duration of `native_device_matches` and
        // never stored: the answer is a `bool`, so nothing on this side of
        // `ravel_gpu::interop` outlives the window that lent the handle
        // (`GPULOSS-4`). The wgpu-backed arm is the one that retains a device,
        // and it retains a `wgpu::Device` through the interop boundary rather
        // than a backend pointer.
        //
        // It is also the *only* GPU capability decision. Whether a lost device
        // may still be sampled is not re-decided here — `ProjectState` owns
        // that, and `configure_viewer_surface` refuses the `true` this hands it
        // once the device is gone.
        let capability = {
            #[cfg(target_os = "macos")]
            {
                if let Some(handles) = window.native_gpu_handles() {
                    let gpu = project.read(cx).gpu_context();
                    gpu.is_some_and(|gpu| {
                        ravel_gpu::interop::native_device_matches(
                            gpu,
                            ravel_gpu::interop::NativeApi::Metal,
                            handles.device(),
                        )
                    })
                } else {
                    false
                }
            }
            #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
            {
                // The context above is the renderer's, so adoption proves the
                // device identity. The surface path also retains each pooled
                // frame through GPUI's wgpu completion callback. If adoption
                // failed, or the backend reports a lost/unknown device, keep
                // the CPU fallback instead; `None` is deliberately treated as
                // lost for the same safe asymmetry used by the viewer.
                adopted_host_gpu && !window.gpu_device_lost().unwrap_or(true)
            }
            #[cfg(not(any(
                target_os = "macos",
                target_os = "linux",
                target_os = "freebsd",
                target_os = "windows"
            )))]
            {
                false
            }
        };
        tracing::info!(
            capability,
            adopted_host_gpu,
            "viewer GPU surface capability detected"
        );
        project.update(cx, |project, cx| {
            project.configure_viewer_surface(capability, cx);
        });
        // The recovery coordinator (`GPULOSS-3`), armed only where a
        // replacement device can actually be obtained *and* Ravel is running
        // on a device this renderer lent it. A session on its own device has
        // nothing here to coordinate: its loss goes through its own callback
        // and settles on the CPU fallback (`GPULOSS-4`).
        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
        if adopted_host_gpu {
            spawn_host_device_recovery(window, project.downgrade(), cx);
        }
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

        // The render queue (`render-export-plan.md`, unit 5): a second
        // evaluation worker on the shared device, spawned lazily on the first
        // export. Owned here so it outlives every panel — a render keeps
        // going when the render queue panel is closed.
        let render = cx.new(crate::export::RenderService::new);
        cx.set_global(crate::export::RenderServiceHandle(render.downgrade()));
        let render_event_sub = cx.subscribe_in(
            &render,
            window,
            |_this, _render, event: &crate::export::RenderServiceEvent, window, cx| {
                show_render_event(event, window, cx);
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

        // Both menu bars hold a snapshot with the labels already baked in, so a
        // language change cannot reach them by re-rendering. Rebuild them when
        // the settings global moves (`app_settings`), which is also the only
        // thing that can change the language.
        let settings_sub = cx.observe_global::<crate::app_settings::AppSettings>(|this, cx| {
            install_menus(&this.shell, cx);
            cx.notify();
        });

        Self {
            shell,
            playback,
            project,
            audio,
            audio_event_sub,
            render,
            render_event_sub,
            window_title,
            title_sub,
            project_event_sub,
            minimize_sub,
            document_replaced_sub,
            settings_sub,
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
        install_menus(&self.shell, cx);
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
        install_menus(&self.shell, cx);
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
        window.open_dialog(cx, move |dialog, window, _cx| {
            let content = form.clone();
            dialog
                .title(SharedString::from(t!("workspace.layouts.title")))
                .w(px(420.0))
                .margin_top(dialog_margin_top(window))
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

    /// Edit ▸ Preferences… and Composition ▸ Project Settings…: the settings
    /// screens (REQ-PROJ-004).
    ///
    /// One modal per screen, because the screen is what decides the settings
    /// layer its fields write to (see [`crate::settings_dialog`]). Edits apply
    /// as they are made and are not document edits, so the footer only closes:
    /// there is no OK to confirm and no Cancel to roll back.
    fn open_settings_dialog(
        &mut self,
        scope: SettingsScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let body = cx.new(|cx| SettingsDialog::new(scope, cx));
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let content = body.clone();
            // Translated inside the builder, which `Root` re-runs on every
            // render: the Language page is *in this dialog*, so its own title and
            // footer have to follow a switch made while it is open.
            dialog
                .title(SharedString::from(t!(scope.title_key())))
                .w(px(720.0))
                .content(move |body, _window, _cx| body.child(content.clone()))
                .footer(
                    DialogFooter::new().child(
                        Button::new("settings-dialog-close")
                            .primary()
                            .label(SharedString::from(t!("ui.close")))
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
        install_menus(&self.shell, cx);
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
            // A panel the user just opened is the one they mean to work in, so
            // the keyboard goes there. The move is a real GPUI focus change,
            // which is what repoints `FocusedPanelGlobal` and, through it, the
            // shell — the shell does not mark the instance focused on its own
            // (`MED-APP-24` was the two doing it separately and disagreeing).
            CommandOutcome::OpenPanel { instance } => {
                crate::window_host::focus_pane(instance, cx);
            }
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
                | CommandId::FrameStepBackward
                | CommandId::PlaybackLoopIn
                | CommandId::PlaybackLoopOut
                | CommandId::PlaybackLoopClear => {
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
                CommandId::FileExport => self.prompt_export(window, cx),
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
                // The settings screens (REQ-PROJ-004). The screen decides the
                // settings layer it writes, so the command picks the screen.
                CommandId::AppPreferences => {
                    self.open_settings_dialog(SettingsScope::Preferences, window, cx);
                }
                CommandId::ProjectSettings => {
                    self.open_settings_dialog(SettingsScope::Project, window, cx);
                }
                // The project's external parameter contract (REQ-PROJ-006).
                // The declarations are shown in Properties rather than in a
                // dialog because a subgraph template's declarations arrive in
                // the same list, and because exposing a parameter is done from
                // the parameter's own row in that panel.
                CommandId::ProjectExposedParameters => {
                    cx.set_global(panels::SelectedPropertiesTarget(
                        panels::PropertiesTarget::Project,
                    ));
                    if let CommandOutcome::OpenPanel { instance } =
                        self.shell.reveal_panel(PanelKind::Properties)
                    {
                        crate::window_host::focus_pane(instance, cx);
                    }
                }
                // The node bodies' parameter rows (PGRP-5). A display flag,
                // so it goes to the UI-state global rather than the document;
                // every Node Editor observes it and re-measures its nodes.
                CommandId::ViewToggleNodeParamValues => {
                    panels::toggle_node_param_values(cx);
                }
                // The viewer's preview resolution factor (REQ-UI-004). Handled
                // here rather than in the Viewer panel's `on_action`: the
                // factor belongs to `ProjectState`, which builds the
                // evaluation request, and it applies whichever panel has
                // focus — a Viewer-scoped handler would make the chord dead
                // while the user is in the Timeline.
                CommandId::ViewCyclePreviewResolution => {
                    self.project.update(cx, |project, cx| {
                        let next = project.viewer_resolution().cycled();
                        project.set_viewer_resolution(next, cx);
                    });
                }
                // The viewer's channel isolation (`INSP-2`). Here for the
                // same reason as the factor above: the mode lives on
                // `ProjectState`, which owns the cell the evaluation
                // worker's display transform reads, and it applies whichever
                // panel has focus.
                CommandId::ViewerChannelRgb
                | CommandId::ViewerChannelRed
                | CommandId::ViewerChannelGreen
                | CommandId::ViewerChannelBlue
                | CommandId::ViewerChannelAlpha => {
                    if let Some(channel) =
                        ravel_ui::panels::viewer::display_channel_from_command(cmd)
                    {
                        self.project.update(cx, |project, cx| {
                            project.set_display_channel(channel, cx);
                        });
                    }
                }
                // The viewer's pixel readout (`INSP-3`). The on/off goes to
                // `ProjectState` for the reason the channel does — it owns
                // the cell the worker's display transform reads — while the
                // scale is a Global, because printing a number differently
                // must not cost a transform.
                CommandId::ViewerPixelReadout => {
                    self.project.update(cx, |project, cx| {
                        let on = project.pixel_readout();
                        project.set_pixel_readout(!on, cx);
                    });
                }
                CommandId::ViewerPixelReadoutFormat => {
                    let next = cx
                        .try_global::<panels::ViewerReadoutFormat>()
                        .copied()
                        .unwrap_or_default()
                        .0
                        .toggled();
                    cx.set_global(panels::ViewerReadoutFormat(next));
                }
                // Named layouts (REQ-UI-005) plus the embed opt-in.
                CommandId::WorkspaceManageLayouts => self.prompt_workspace_layouts(window, cx),
                CommandId::ToolSelect
                | CommandId::ToolPen
                | CommandId::ToolRect
                | CommandId::ToolEllipse
                | CommandId::ToolPolygon
                | CommandId::ToolStar
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
        window.open_dialog(cx, move |dialog, window, _cx| {
            let save_workspace = workspace.clone();
            let discard_workspace = workspace.clone();
            let button_workspace = workspace.clone();
            dialog
                .title(SharedString::from(t!("project.unsaved.title")))
                .w(px(448.0))
                .margin_top(dialog_margin_top(window))
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
    /// settings' default frame rate over the 1920×1080 / 300f fallback
    /// (`ProjectState::new_composition_defaults` owns that precedence). Creating
    /// on confirm rather than up front keeps this one undo step instead of
    /// "create, then correct".
    ///
    /// The `manifest.json` project defaults are not consulted: `ProjectState`
    /// does not retain the loaded manifest, and the settings layer is where a
    /// project-wide default now lives (`SET-6`).
    fn prompt_new_composition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let initial = self.project.read(cx).new_composition_defaults(cx);
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
        window.open_dialog(cx, move |dialog, window, _cx| {
            let content = form.clone();
            let ok_form = form.clone();
            let project = project.clone();
            let confirm = confirm.clone();
            let cancel_label = SharedString::from(t!("ui.cancel"));
            dialog
                .title(title.clone())
                .w(px(360.0))
                .margin_top(dialog_margin_top(window))
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

    /// File ▸ Export…: collect a render request and hand it to the session's
    /// render queue (`render-export-plan.md`, unit 5).
    ///
    /// The dialog is opened around [`ExportForm`] exactly as the composition
    /// dialogs are opened around `CompositionForm`, with one difference: OK
    /// can **fail**. A form that does not resolve leaves the dialog open with
    /// the refusal under it, because closing it would make the user retype
    /// everything to fix one field.
    fn prompt_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        // Everything the dialog needs, read out in one borrow so the entity
        // is free again before the form is built.
        let opened = {
            let project = self.project.read(cx);
            let document = project.document();
            let mut comps: Vec<crate::export_dialog::CompChoice> = document
                .compositions
                .iter()
                .map(|(id, comp)| crate::export_dialog::CompChoice {
                    id: *id,
                    name: comp.name.clone(),
                    // Per entry, not once for the active composition: the
                    // dialog's picker can move, and the checkbox and the
                    // empty-composition refusal both have to follow it.
                    duration: comp.duration_frames,
                    has_audio: crate::export::composition_has_audio(document, *id),
                })
                .collect();
            // The map iterates in hash order; the picker shows the same order
            // every time rather than one that depends on insertion history.
            comps.sort_by_key(|comp| comp.id);
            panels::active_composition(cx)
                .filter(|id| document.get_composition(*id).is_some())
                .or_else(|| comps.first().map(|comp| comp.id))
                .map(|active| {
                    let comp = document
                        .get_composition(active)
                        .expect("checked by the filter above");
                    let initial = crate::export_dialog::initial_settings(
                        active,
                        &comp.name,
                        comp.duration_frames,
                        project.project_path(),
                    );
                    // `Document` is immutable-by-clone, so this is the
                    // snapshot the job renders: later edits to the session's
                    // copy cannot reach it.
                    (comps, initial, std::sync::Arc::new(document.clone()))
                })
        };
        let Some((comps, initial, document)) = opened else {
            // Nothing to render. The menu entry stays enabled — a project
            // with no composition is a state the user can leave — so say why
            // instead of doing nothing.
            show_export_failure(
                SharedString::from(t!("export.error.no_composition")),
                window,
                cx,
            );
            return;
        };

        let choices =
            crate::export_dialog::format_choices(&ravel_media::encode::available_encoders());
        let form = cx.new(|cx| {
            crate::export_dialog::ExportForm::new(
                comps,
                initial,
                choices,
                crate::export::AUDIO_DECODE_AVAILABLE,
                window,
                cx,
            )
        });
        let service = self.render.downgrade();
        window.open_dialog(cx, move |dialog, window, _cx| {
            let content = form.clone();
            let ok_form = form.clone();
            let document = document.clone();
            let service = service.clone();
            dialog
                .title(SharedString::from(t!("export.title")))
                .w(px(420.0))
                .margin_top(dialog_margin_top(window))
                .content(move |body, _window, _cx| body.child(content.clone()))
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("export-dialog-cancel")
                                .label(SharedString::from(t!("ui.cancel")))
                                .on_click(|_event, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("export-dialog-ok")
                                .primary()
                                .label(SharedString::from(t!("export.submit")))
                                .on_click({
                                    let document = document.clone();
                                    let service = service.clone();
                                    move |_event, window, cx| {
                                        let settings = ok_form.read(cx).settings(cx);
                                        let composition = ok_form.read(cx).composition_name(cx);
                                        let request = match settings.resolve() {
                                            Ok(request) => request,
                                            Err(error) => {
                                                let message = SharedString::from(t!(
                                                    error.message_key()
                                                ));
                                                ok_form.update(cx, |form, cx| {
                                                    form.show_error(message, cx)
                                                });
                                                return;
                                            }
                                        };
                                        let job = crate::export::ExportJob {
                                            request,
                                            document: document.clone(),
                                            composition,
                                        };
                                        if service
                                            .update(cx, |service, cx| service.submit(job, cx))
                                            .is_err()
                                        {
                                            tracing::warn!(
                                                "the render queue was dropped before the export dialog was confirmed"
                                            );
                                        }
                                        window.close_dialog(cx);
                                    }
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
        ProjectEvent::GpuDeviceLost => (
            NotificationType::Error,
            t!("project.notice.gpu_lost_title"),
            t!("project.notice.gpu_lost_message").to_string(),
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
        ProjectEvent::SettingsSaveFailed { path, error } => (
            NotificationType::Error,
            t!("project.notice.settings_save_title"),
            format!(
                "{}\n{}\n{error}",
                t!("project.notice.settings_save_message"),
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
        ProjectEvent::MediaRelinkFailed { failure } => (
            NotificationType::Warning,
            t!("project.notice.relink_title"),
            format!(
                "{}\n{}: {}",
                t!("project.notice.relink_message"),
                failure.path.display(),
                failure.reason
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
    let crate::audio::AudioServiceEvent::PreparationFailed { asset, error } = event;
    window.push_notification(
        Notification::new()
            .with_type(NotificationType::Warning)
            .title(SharedString::from(t!("audio.notice.prepare_title")))
            .message(SharedString::from(format!(
                "{}\n{asset}\n{error}",
                t!("audio.notice.prepare_message")
            )))
            .autohide(false),
        cx,
    );
}

/// Show what became of a render.
fn show_render_event(event: &crate::export::RenderServiceEvent, window: &mut Window, cx: &mut App) {
    match event {
        crate::export::RenderServiceEvent::Completed { directory, frames } => {
            window.push_notification(
                Notification::new()
                    .with_type(NotificationType::Success)
                    .title(SharedString::from(t!("export.notice.completed_title")))
                    // One phrase with both blanks filled, not a sentence
                    // assembled here: the count, the path and the words
                    // between them sit in whatever order the locale puts
                    // them. `ravel-cli` says the same thing through the same
                    // shape of key (`cli.result.completed`).
                    .message(SharedString::from(
                        t!("export.notice.completed")
                            .replace("{count}", &frames.to_string())
                            .replace("{path}", &directory.display().to_string()),
                    )),
                cx,
            );
        }
        crate::export::RenderServiceEvent::Failed { message } => {
            show_export_failure(message.clone(), window, cx);
        }
        // Not a refusal: the render is happening, and something about it is
        // worth knowing. Kept on screen (`autohide(false)`) for the same
        // reason `ravel-cli` prints its warnings — a silent deliverable is
        // discovered far too late otherwise.
        crate::export::RenderServiceEvent::Warning { message } => {
            window.push_notification(
                Notification::new()
                    .with_type(NotificationType::Warning)
                    .title(SharedString::from(t!("export.notice.warning_title")))
                    .message(message.clone())
                    .autohide(false),
                cx,
            );
        }
    }
}

/// A refusal the export path produces, as a notification.
///
/// Shared by the dialog's early exits and by the queue's failure events, so
/// one kind of problem reads the same wherever it was noticed.
fn show_export_failure(message: SharedString, window: &mut Window, cx: &mut App) {
    window.push_notification(
        Notification::new()
            .with_type(NotificationType::Error)
            .title(SharedString::from(t!("export.notice.failed_title")))
            .message(message)
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

    /// A workspace chord belongs to the workspace only while nothing else owns
    /// the keyboard. Text inputs own it while typing, and an open menu owns it
    /// while it is open (`MED-APP-31`) — gpui breaks the tie by registration
    /// order and Ravel binds last, so the yielding has to be in the predicate.
    #[test]
    fn playback_arrow_bindings_yield_to_text_inputs_and_open_menus() {
        let bindings = super::build_keybindings(&ravel_ui::shell::AppShell::default());
        let step_forward = bindings
            .iter()
            .find(|binding| binding.action().as_any().is::<super::FrameStepForward>())
            .expect("default step-forward binding");
        let predicate = step_forward.predicate().expect("bindings carry a context");

        let context = |name: &str| gpui::KeyContext::try_from(name).expect("context parses");
        assert!(
            predicate.eval(&[context("Workspace")]),
            "the workspace still owns the arrow when nothing else does"
        );
        for owner in ["Input", "PopupMenu", "AppMenuBar"] {
            assert!(
                !predicate.eval(&[context("Workspace"), context(owner)]),
                "{owner} owns its own arrows while it is focused"
            );
        }
    }

    /// The primary modifier must reach gpui as `secondary-`, which resolves to
    /// Cmd on macOS and Control elsewhere. Sending `cmd-` instead means
    /// `Modifiers::platform` on every platform — the Windows key on Windows,
    /// where it made every `Cmd+…` shortcut in the application dead.
    #[test]
    fn the_primary_modifier_reaches_gpui_as_secondary() {
        let chord: ravel_ui::keybindings::KeyChord = "Cmd+Shift+Z".parse().expect("chord parses");
        assert_eq!(super::chord_to_gpui_string(&chord), "secondary-shift-z");

        let plain: ravel_ui::keybindings::KeyChord = "Shift+A".parse().expect("chord parses");
        assert_eq!(
            super::chord_to_gpui_string(&plain),
            "shift-a",
            "a chord without the primary modifier is untouched"
        );

        let control: ravel_ui::keybindings::KeyChord = "Ctrl+A".parse().expect("chord parses");
        assert_eq!(
            super::chord_to_gpui_string(&control),
            "ctrl-a",
            "the literal Control key is not the primary modifier"
        );
    }

    /// `secondary` and `ctrl` collapse onto the same modifier off macOS, so a
    /// chord holding both would be two names for one keystroke. Nothing in the
    /// binding set does that; this fails if something starts to.
    #[test]
    fn a_chord_cannot_hold_both_primary_modifiers() {
        let shell = ravel_ui::shell::AppShell::default();
        for (chord, cmd) in shell.keybindings().iter() {
            let gpui = super::chord_to_gpui_string(chord);
            assert!(
                !(gpui.contains("secondary-") && gpui.contains("ctrl-")),
                "{cmd:?} binds {chord}, which is one keystroke off macOS"
            );
        }
        for binding in super::PANEL_BINDINGS {
            let gpui = binding.chord.replace('+', "-").to_lowercase();
            let gpui = gpui.replace("cmd-", "secondary-");
            assert!(
                !(gpui.contains("secondary-") && gpui.contains("ctrl-")),
                "{:?} binds {}, which is one keystroke off macOS",
                binding.command,
                binding.chord
            );
        }
    }

    /// `GPULOSS-3`: every reading of the adopted window's renderer, and the
    /// one thing the coordinator does about it.
    ///
    /// The whole table, because the platform arms that *read* the renderer are
    /// compiled only on Linux / FreeBSD / Windows and this decision is the one
    /// piece of the unit a test on any machine can hold still. Three rows
    /// carry the reasoning:
    ///
    /// * `adopted: false` never acts, even when the renderer reports another
    ///   device — that is a session evaluating on its own device, and the
    ///   mismatch is the normal reading, not a recovery;
    /// * `Same` while healthy does nothing — a loss that flipped and came back
    ///   onto the same device must not cost the session its caches;
    /// * `Absent` suspends and does not adopt — the renderer is mid-recovery
    ///   and has nothing to hand over yet.
    #[test]
    fn the_recovery_step_follows_the_renderer_reading() {
        use super::HostContext::{Absent, Replaced, Same};
        use super::HostRecoveryStep::{Nothing, Readopt, Suspend};

        let table = [
            // (adopted, lost, context, step)
            (false, false, Absent, Nothing),
            (false, false, Same, Nothing),
            (false, false, Replaced, Nothing),
            (false, true, Absent, Nothing),
            (false, true, Same, Nothing),
            (false, true, Replaced, Nothing),
            (true, false, Absent, Suspend),
            (true, false, Same, Nothing),
            (true, false, Replaced, Readopt),
            (true, true, Absent, Suspend),
            (true, true, Same, Suspend),
            (true, true, Replaced, Suspend),
        ];

        for (adopted, lost, context, expected) in table {
            let observation = super::HostDeviceObservation {
                adopted,
                lost,
                context,
            };
            assert_eq!(
                super::host_recovery_step(observation),
                expected,
                "{observation:?}"
            );
        }
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
