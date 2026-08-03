// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Verifies the single Command ↔ GPUI Action table in `ravel_app::workspace`
//! covers every `CommandId` exactly once (Phase 1 of the command/focus
//! refactor plan). The table's generated `match` expressions already make a
//! *missing* entry a compile error; this test additionally catches duplicates
//! and ordering drift against the canonical `CommandId` table.

use ravel_app::panels::node_editor::KEY_CONTEXT;
use ravel_app::workspace::{build_keybindings, build_menus, mapped_commands};
use ravel_ui::command::CommandId;
use ravel_ui::shell::AppShell;

#[test]
fn every_command_id_is_mapped_to_exactly_one_action() {
    let mapped = mapped_commands();
    let all: Vec<CommandId> = CommandId::all().collect();

    for cmd in &all {
        let count = mapped.iter().filter(|m| *m == cmd).count();
        assert_eq!(count, 1, "{cmd} must appear exactly once in the table");
    }
    assert_eq!(
        mapped.len(),
        all.len(),
        "action table and CommandId table must have the same size"
    );
}

#[test]
fn action_table_follows_command_id_declaration_order() {
    let mapped = mapped_commands();
    let all: Vec<CommandId> = CommandId::all().collect();
    assert_eq!(
        mapped, all,
        "keep the workspace action table in CommandId declaration order"
    );
}

/// The GPUI action `for_each_command!` generates for a command, named the way
/// `Action::name()` reports it.
fn action_name(command: CommandId) -> String {
    format!("ravel::{command:?}")
}

/// Every action name reachable from the platform menu bar.
fn menu_action_names(shell: &AppShell) -> Vec<String> {
    fn walk(items: &[gpui::MenuItem], out: &mut Vec<String>) {
        for item in items {
            match item {
                gpui::MenuItem::Action { action, .. } => out.push(action.name().to_owned()),
                gpui::MenuItem::Submenu(menu) => walk(&menu.items, out),
                gpui::MenuItem::Separator | gpui::MenuItem::SystemMenu(_) => {}
            }
        }
    }

    let mut out = Vec::new();
    for menu in build_menus(shell) {
        walk(&menu.items, &mut out);
    }
    out
}

/// The three routes into a command name the same action: a menu entry, a chord
/// from the keybinding asset, and the action table itself. A command wired into
/// only two of them is reachable but lands somewhere else — or nowhere.
#[test]
fn menus_keybindings_and_the_action_table_name_the_same_actions() {
    let shell = AppShell::default();
    let table = mapped_commands();
    let menu_actions = menu_action_names(&shell);
    let binding_actions: Vec<&str> = build_keybindings(&shell)
        .iter()
        .map(|binding| binding.action().name())
        .collect();

    for command in shell.menu_bar().commands() {
        assert!(
            table.contains(&command),
            "{command} is in the menu bar but not in the action table"
        );
        assert!(
            menu_actions.contains(&action_name(command)),
            "the menu entry for {command} does not dispatch {}",
            action_name(command)
        );
    }

    for (chord, command) in shell.keybindings().iter() {
        assert!(
            table.contains(&command),
            "{chord} binds {command}, which is not in the action table"
        );
        assert!(
            binding_actions.contains(&action_name(command).as_str()),
            "{chord} does not bind {}",
            action_name(command)
        );
    }

    // The settings screens are reachable both ways (REQ-PROJ-004): Preferences
    // by menu and chord, Project Settings by menu only.
    assert!(menu_actions.contains(&action_name(CommandId::AppPreferences)));
    assert!(menu_actions.contains(&action_name(CommandId::ProjectSettings)));
    assert!(binding_actions.contains(&action_name(CommandId::AppPreferences).as_str()));
}

#[test]
fn node_editor_keybindings_are_context_scoped() {
    let bindings = build_keybindings(&AppShell::default());
    let scoped: Vec<_> = bindings
        .iter()
        .filter(|binding| {
            binding
                .predicate()
                .is_some_and(|predicate| predicate.to_string() == KEY_CONTEXT)
        })
        .map(|binding| {
            let keystroke = binding
                .keystrokes()
                .first()
                .expect("node editor bindings should have one keystroke")
                .inner();
            (
                keystroke.key.as_str(),
                keystroke.modifiers.platform,
                keystroke.modifiers.modified(),
                binding.action().name(),
            )
        })
        .collect();

    assert_eq!(
        scoped,
        [
            ("d", true, true, "ravel::EditDuplicate"),
            ("f", false, false, "ravel::ViewFit"),
            ("tab", false, false, "ravel::NodeSearchPalette"),
            ("delete", false, false, "ravel::EditDelete"),
            ("backspace", false, false, "ravel::EditDelete"),
        ]
    );
}
