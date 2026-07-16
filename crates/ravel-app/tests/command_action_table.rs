// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Verifies the single Command ↔ GPUI Action table in `ravel_app::workspace`
//! covers every `CommandId` exactly once (Phase 1 of the command/focus
//! refactor plan). The table's generated `match` expressions already make a
//! *missing* entry a compile error; this test additionally catches duplicates
//! and ordering drift against the canonical `CommandId` table.

use ravel_app::workspace::mapped_commands;
use ravel_ui::command::CommandId;

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
