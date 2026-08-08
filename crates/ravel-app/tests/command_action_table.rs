// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Verifies the single Command ↔ GPUI Action table in `ravel_app::workspace`
//! covers every `CommandId` exactly once (Phase 1 of the command/focus
//! refactor plan). The table's generated `match` expressions already make a
//! *missing* entry a compile error; this test additionally catches duplicates
//! and ordering drift against the canonical `CommandId` table.
//!
//! It also pins the menu snapshot both bars are drawn from: `build_menus`
//! feeds the macOS menu bar directly and gpui-component's in-window
//! `AppMenuBar` through `Menu::owned`, so anything the conversion drops is
//! missing from both.

use ravel_i18n::t;

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

/// A menu tree flattened to a comparable shape: one entry per item, in order,
/// nesting spelled out by the submenu's own name.
#[derive(Debug, PartialEq)]
enum Flat {
    Action { label: String, checked: bool },
    Separator,
    Submenu(String, Vec<Flat>),
    Other,
}

fn flatten(items: &[gpui::MenuItem]) -> Vec<Flat> {
    items
        .iter()
        .map(|item| match item {
            gpui::MenuItem::Action { name, .. } => Flat::Action {
                label: name.to_string(),
                checked: item.is_checked(),
            },
            gpui::MenuItem::Separator => Flat::Separator,
            gpui::MenuItem::Submenu(menu) => {
                Flat::Submenu(menu.name.to_string(), flatten(&menu.items))
            }
            gpui::MenuItem::SystemMenu(_) => Flat::Other,
        })
        .collect()
}

fn flatten_owned(items: &[gpui::OwnedMenuItem]) -> Vec<Flat> {
    items
        .iter()
        .map(|item| match item {
            gpui::OwnedMenuItem::Action { name, checked, .. } => Flat::Action {
                label: name.to_string(),
                checked: *checked,
            },
            gpui::OwnedMenuItem::Separator => Flat::Separator,
            gpui::OwnedMenuItem::Submenu(menu) => {
                Flat::Submenu(menu.name.to_string(), flatten_owned(&menu.items))
            }
            gpui::OwnedMenuItem::SystemMenu(_) => Flat::Other,
        })
        .collect()
}

fn flatten_headless(items: &[ravel_ui::menu::MenuItem]) -> Vec<Flat> {
    items
        .iter()
        .map(|item| match item {
            ravel_ui::menu::MenuItem::Action { command, check } => Flat::Action {
                label: t!(command.label_key()).to_string(),
                checked: check.unwrap_or(false),
            },
            ravel_ui::menu::MenuItem::Separator => Flat::Separator,
            ravel_ui::menu::MenuItem::Submenu(menu) => Flat::Submenu(
                t!(menu.label_key).to_string(),
                flatten_headless(&menu.items),
            ),
        })
        .collect()
}

/// `AppMenuBar` reads a `Menu::owned` snapshot rather than the `Menu`s the
/// macOS bar gets. Names, separators, submenu nesting, and the checkboxes have
/// to survive that conversion, or the in-window bar quietly differs from the
/// OS one it is supposed to replace.
///
/// The expectation is the headless model itself, not another `build_menus`
/// call: comparing the conversion against its own output would pass with
/// `convert_menu_item` dropping every check or flattening every submenu.
#[test]
fn owned_menus_keep_names_structure_and_checkmarks() {
    let shell = AppShell::default();

    let expected: Vec<(String, Vec<Flat>)> = shell
        .menu_bar()
        .menus
        .iter()
        .map(|menu| {
            (
                t!(menu.label_key).to_string(),
                flatten_headless(&menu.items),
            )
        })
        .collect();
    // Past the synthetic application menu, which the in-window bar drops.
    let owned: Vec<(String, Vec<Flat>)> = build_menus(&shell)
        .into_iter()
        .skip(1)
        .map(gpui::Menu::owned)
        .map(|menu| (menu.name.to_string(), flatten_owned(&menu.items)))
        .collect();

    assert_eq!(owned, expected);

    // The macOS bar is handed the `Menu`s directly, so it has to agree too.
    let native: Vec<(String, Vec<Flat>)> = build_menus(&shell)
        .iter()
        .skip(1)
        .map(|menu| (menu.name.to_string(), flatten(&menu.items)))
        .collect();
    assert_eq!(native, expected);

    // Non-vacuous: the default shell has visible panels, so the View menu
    // carries checked toggles. Without this the comparison above would still
    // pass with `convert_menu_item` dropping every `check`.
    let checked = owned
        .iter()
        .flat_map(|(_, items)| items)
        .filter(|item| matches!(item, Flat::Action { checked: true, .. }))
        .count();
    assert!(
        checked > 0,
        "the default menu bar should carry at least one checked item"
    );
}

/// The in-window bar drops the synthetic macOS application menu, because Quit
/// and About already live in the headless File and Help menus.
#[test]
fn the_synthetic_application_menu_only_duplicates_headless_entries() {
    let shell = AppShell::default();
    let commands = shell.menu_bar().commands();
    assert!(commands.contains(&CommandId::FileQuit));
    assert!(commands.contains(&CommandId::HelpAbout));

    // `install_menus` drops it by skipping exactly one leading menu, so a
    // second synthetic one would reach the in-window bar unnoticed. Names
    // rather than a count: two synthetic menus and one headless menu fewer
    // would keep the count right.
    let menus = build_menus(&shell);
    assert_eq!(
        menus.first().map(|menu| menu.name.to_string()),
        Some(t!("app.title").to_string()),
        "the synthetic application menu should lead"
    );
    let rest: Vec<String> = menus.iter().skip(1).map(|m| m.name.to_string()).collect();
    let headless: Vec<String> = shell
        .menu_bar()
        .menus
        .iter()
        .map(|m| t!(m.label_key).to_string())
        .collect();
    assert_eq!(rest, headless);
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
