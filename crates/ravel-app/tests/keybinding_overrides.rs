// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The regression fence around user keybinding overrides (`SET-5`).
//!
//! `MED-APP-16` was asset-derived bindings registered with **no** key context:
//! `default.toml` binds `Left` / `Right` to frame stepping, a context-free
//! binding matches in every context, and so a focused text field lost its
//! arrows to the transport. Phase A fixed it by giving every asset binding
//! `Some("!Input")`.
//!
//! A user override file is a second source of bare single-key chords, which is
//! exactly the shape that caused the bug. What keeps it safe is that user
//! bindings take the *same* route — into [`AppShell`], then through
//! `build_keybindings` — rather than being bound separately. These tests pin
//! that: no `KeyBinding` anywhere in the table is context-free, every binding
//! that came from the binding *set* (asset or user) carries `!Input`, and the
//! count matches the set exactly, so none can slip out through a new branch.
//!
//! # Why the predicate, and not a caret
//!
//! These tests read `KeyBinding::predicate()` rather than focusing a text input
//! and checking that the caret moved. That is deliberate: what has to hold in
//! *this* repository is "no binding is registered without a context". Whether a
//! context-scoped binding then loses to a focused `Input` is gpui-component's
//! behaviour, tested there, and re-testing it here would pin someone else's
//! implementation while leaving our own invariant unpinned.
//!
//! The predicate check is not vacuous, and that was measured rather than
//! assumed. To reproduce: in `crates/ravel-app/src/workspace.rs`, change the
//! asset loop's `Some("!Input")` back to `None` — the shape `MED-APP-16`
//! reported — and run `cargo test -p ravel-app --test keybinding_overrides`.
//! `no_binding_is_registered_without_a_context` fails with
//! `'s' is bound with no key context`, and
//! `asset_and_user_bindings_are_both_scoped_out_of_text_input` fails with
//! `left: 0, right: 30`. Restore the line afterwards.

use gpui::TestAppContext;
use gpui_component::Root;
use ravel_app::keybindings::{LoadedKeybindings, read_keybindings_at};
use ravel_app::workspace::{self, PANEL_BINDINGS, build_keybindings, panel_bound_commands};
use ravel_app::{panels, trace, window_host};
use ravel_ui::command::CommandId;
use ravel_ui::keybindings::KeyChord;
use ravel_ui::panel::PanelKind;
use ravel_ui::shell::AppShell;

/// The predicate every binding derived from the binding set must carry.
const NOT_INPUT: &str = "!Input";

/// The panel key contexts the code-side bindings use. Everything else in the
/// table has to be `!Input`.
fn panel_contexts() -> [&'static str; 3] {
    [
        panels::node_editor::KEY_CONTEXT,
        panels::timeline::KEY_CONTEXT,
        panels::viewer::KEY_CONTEXT,
    ]
}

fn loaded_from(dir: &tempfile::TempDir, text: &str) -> LoadedKeybindings {
    let path = dir.path().join("keybindings.toml");
    std::fs::write(&path, text).expect("the temp dir is writable");
    read_keybindings_at(Some(path))
}

fn shell_with(loaded: &LoadedKeybindings) -> AppShell {
    let mut shell = AppShell::default();
    shell.set_keybindings(loaded.bindings().clone());
    shell
}

/// `(first keystroke's key, predicate)` for every binding in the table.
fn bindings_of(shell: &AppShell) -> Vec<(String, Option<String>)> {
    build_keybindings(shell)
        .iter()
        .map(|binding| {
            let key = binding
                .keystrokes()
                .first()
                .expect("every binding has at least one keystroke")
                .inner()
                .key
                .clone();
            (key, binding.predicate().map(|p| p.to_string()))
        })
        .collect()
}

/// The keys bound with `!Input`, which is what a focused text input is
/// protected from.
fn keys_scoped_out_of_input(shell: &AppShell) -> Vec<String> {
    bindings_of(shell)
        .into_iter()
        .filter(|(_, predicate)| predicate.as_deref() == Some(NOT_INPUT))
        .map(|(key, _)| key)
        .collect()
}

/// Nothing in the table is context-free. A binding with `None` matches in every
/// context, including a focused text input, and that is precisely `MED-APP-16`.
///
/// Asserted for the defaults *and* for a user file, because a user override is
/// the second way a bare single key gets into the table.
#[test]
fn no_binding_is_registered_without_a_context() {
    let dir = tempfile::tempdir().unwrap();
    let cases = [
        read_keybindings_at(None),
        loaded_from(
            &dir,
            r#"
            [playback]
            step_forward = "Up"
            step_backward = "Down"
            toggle = "Enter"

            [edit]
            undo = "Backspace"
        "#,
        ),
    ];

    for loaded in &cases {
        let shell = shell_with(loaded);
        let allowed: Vec<&str> = std::iter::once(NOT_INPUT).chain(panel_contexts()).collect();
        for (key, predicate) in bindings_of(&shell) {
            let predicate = predicate.unwrap_or_else(|| {
                panic!("'{key}' is bound with no key context (MED-APP-16 regression)")
            });
            assert!(
                allowed.contains(&predicate.as_str()),
                "'{key}' is bound in the unexpected context '{predicate}'"
            );
        }
    }
}

/// Every binding that came from the binding set carries `!Input` — asset and
/// user alike, because they travel the same route.
///
/// The count is asserted against the set's own size, so a future branch in
/// `build_keybindings` that registers some subset differently fails here rather
/// than quietly reopening the bug for those chords.
#[test]
fn asset_and_user_bindings_are_both_scoped_out_of_text_input() {
    let dir = tempfile::tempdir().unwrap();

    let defaults = read_keybindings_at(None);
    let shell = shell_with(&defaults);
    let scoped = keys_scoped_out_of_input(&shell);
    assert_eq!(
        scoped.len(),
        shell.keybindings().len(),
        "every asset binding must be scoped out of text input"
    );
    // The two chords the bug was reported against.
    for key in ["left", "right"] {
        assert!(scoped.contains(&key.to_owned()), "'{key}' must be scoped");
    }

    // A user file whose chords are all bare keys an Input handles itself.
    let overridden = loaded_from(
        &dir,
        r#"
        [playback]
        step_forward = "Up"
        step_backward = "Down"

        [edit]
        copy = "Home"
        paste = "End"
    "#,
    );
    let shell = shell_with(&overridden);
    let scoped = keys_scoped_out_of_input(&shell);
    assert_eq!(
        scoped.len(),
        shell.keybindings().len(),
        "every user binding must be scoped out of text input too"
    );
    for key in ["up", "down", "home", "end"] {
        assert!(
            scoped.contains(&key.to_owned()),
            "the user chord '{key}' must be scoped out of text input"
        );
    }
    // The chords the user moved off are gone rather than left context-free.
    let all_keys: Vec<String> = bindings_of(&shell).into_iter().map(|(k, _)| k).collect();
    assert!(!all_keys.contains(&"right".to_owned()));
    assert!(!all_keys.contains(&"left".to_owned()));
}

/// The code-side table, pinned verbatim.
///
/// `PANEL_BINDINGS` is read by two places that must not disagree — GPUI
/// registration and the Preferences list — so an accidental edit has to fail
/// here rather than quietly drop a shortcut from one of them.
#[test]
fn the_panel_binding_table_is_the_code_side_shortcut_set() {
    let listed: Vec<(CommandId, &str, PanelKind)> = PANEL_BINDINGS
        .iter()
        .map(|binding| (binding.command, binding.chord, binding.panel))
        .collect();

    assert_eq!(
        listed,
        [
            (CommandId::EditDuplicate, "Cmd+D", PanelKind::NodeGraph),
            (CommandId::ViewFit, "F", PanelKind::NodeGraph),
            (CommandId::NodeSearchPalette, "Tab", PanelKind::NodeGraph),
            (CommandId::EditDelete, "Delete", PanelKind::NodeGraph),
            (CommandId::EditDelete, "Backspace", PanelKind::NodeGraph),
            (CommandId::NodeAutoLayout, "L", PanelKind::NodeGraph),
            (CommandId::EditDelete, "Delete", PanelKind::Timeline),
            (CommandId::EditDelete, "Backspace", PanelKind::Timeline),
            (CommandId::EditDuplicate, "Cmd+D", PanelKind::Timeline),
            (CommandId::ToolSelect, "V", PanelKind::Viewer),
            (CommandId::ToolPen, "P", PanelKind::Viewer),
            (CommandId::ToolRect, "R", PanelKind::Viewer),
            (CommandId::ToolEllipse, "E", PanelKind::Viewer),
            (CommandId::ToolHand, "H", PanelKind::Viewer),
            (CommandId::ToolZoom, "Z", PanelKind::Viewer),
        ]
    );
}

/// Every chord in the table parses, so `build_keybindings` never has to drop one.
///
/// This is what makes the `tracing::error!` branch there unreachable in a
/// checked-in tree, the same bargain `default_bindings()` strikes with the
/// bundled asset.
#[test]
fn panel_binding_chords_parse() {
    for binding in PANEL_BINDINGS {
        let chord = binding
            .chord
            .parse::<KeyChord>()
            .unwrap_or_else(|error| panic!("{}: {error}", binding.chord));
        assert_eq!(
            chord.to_string(),
            binding.chord,
            "write the chord the way KeyChord renders it, so the list matches the table"
        );
    }
}

/// The `panel` field names the panel whose `KEY_CONTEXT` the `context` field
/// holds. The two are separate because registration needs the raw context
/// string while the list needs a localizable panel name.
#[test]
fn panel_bindings_name_their_panels_key_context() {
    for binding in PANEL_BINDINGS {
        let expected = match binding.panel {
            PanelKind::NodeGraph => panels::node_editor::KEY_CONTEXT,
            PanelKind::Timeline => panels::timeline::KEY_CONTEXT,
            PanelKind::Viewer => panels::viewer::KEY_CONTEXT,
            other => panic!("{other:?} has no key context to bind to"),
        };
        assert_eq!(binding.context, expected, "{:?}", binding.command);
    }
}

/// The reserved list is derived from the table, never listed twice.
#[test]
fn panel_bound_commands_are_exactly_the_table_commands() {
    let reserved = panel_bound_commands();
    for binding in PANEL_BINDINGS {
        assert!(reserved.contains(&binding.command), "{:?}", binding.command);
    }
    assert_eq!(
        reserved.len(),
        PANEL_BINDINGS
            .iter()
            .map(|binding| binding.command)
            .collect::<std::collections::HashSet<_>>()
            .len()
    );
    // Commands bound by the asset, and commands bound nowhere, are both
    // absent: only a *panel-scoped* command is reserved.
    assert!(!reserved.contains(&CommandId::FileSave));
    assert!(!reserved.contains(&CommandId::CompositionNew));
}

/// A real main window with a given binding set bound, panels needing a GPU or a
/// media backend toggled out so the test stays headless.
fn open_workspace(loaded: &LoadedKeybindings, cx: &mut TestAppContext) -> gpui::WindowHandle<Root> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
    let _ = ravel_i18n::init(&dir, "en");
    cx.update(|cx| {
        gpui_component::init(cx);
        ravel_app::project_state::disable_background_eval_for_tests();
        cx.set_global(panels::FocusedPanelGlobal(None));
        cx.set_global(panels::SelectedPropertiesTarget::default());
        cx.set_global(panels::CanvasSelection::default());
        trace::init(cx);
        workspace::register_action_handlers(cx);
    });

    let mut shell = shell_with(loaded);
    for panel in [
        PanelKind::NodeGraph,
        PanelKind::Timeline,
        PanelKind::Properties,
    ] {
        if shell.visibility().is_visible(panel) {
            let toggle = match panel {
                PanelKind::NodeGraph => CommandId::ViewToggleNodeGraph,
                PanelKind::Timeline => CommandId::ViewToggleTimeline,
                _ => CommandId::ViewToggleProperties,
            };
            shell.handle_command(toggle);
        }
    }
    cx.update(|cx| cx.bind_keys(build_keybindings(&shell)));

    cx.add_window(move |window, cx| window_host::main_root(shell, window, cx))
}

/// The override reaches the command, and the chord it replaced no longer does.
///
/// This is the end of the route the unit adds: file → overlay → `AppShell` →
/// `build_keybindings` → `cx.bind_keys` → dispatch. A load that produced the
/// right `KeyBindings` but never reached the key map would pass every headless
/// test above and fail here.
#[gpui::test]
fn a_user_chord_dispatches_the_command_and_the_default_one_stops(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let loaded = loaded_from(
        &dir,
        r#"
        [playback]
        step_forward = "Cmd+Alt+Right"
    "#,
    );
    let window = open_workspace(&loaded, cx);

    cx.simulate_keystrokes(window.into(), "cmd-alt-right");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| trace::execution_count(cx, CommandId::FrameStepForward)),
        1,
        "the user's chord must dispatch the command it names"
    );

    cx.simulate_keystrokes(window.into(), "right");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| trace::execution_count(cx, CommandId::FrameStepForward)),
        1,
        "the default chord the user moved off must no longer dispatch"
    );
}
