// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Tests verifying the headless contracts that `dispatch_command` relies on:
//! panel visibility toggling, preset switching, and menu rebuilding.
//!
//! These live in an integration test file to avoid GPUI proc-macro recursion
//! limits within the main crate. They exercise `ravel_ui` types directly.

use ravel_ui::command::CommandId;
use ravel_ui::keybindings::parser::default_bindings;
use ravel_ui::menu::MenuItem;
use ravel_ui::panel::{PanelKind, PanelVisibility};
use ravel_ui::preset::{BuiltinPreset, LayoutNode, Orientation};
use ravel_ui::shell::{AppShell, CommandOutcome};

fn shell() -> AppShell {
    AppShell::new(BuiltinPreset::Edit, default_bindings())
}

// ---------------------------------------------------------------------------
// Panel toggle
// ---------------------------------------------------------------------------

#[test]
fn toggle_command_updates_visibility_and_menus() {
    let mut s = shell();
    assert!(s.visibility().is_visible(PanelKind::Timeline));

    let outcome = s.handle_command(CommandId::ViewToggleTimeline);
    assert_eq!(outcome, CommandOutcome::Handled);
    assert!(!s.visibility().is_visible(PanelKind::Timeline));

    // Menu bar must reflect the new state.
    let bar = s.menu_bar();
    let view = bar.menu("menu.view").unwrap();
    let timeline_unchecked = view.items.iter().any(|i| {
        matches!(
            i,
            MenuItem::Action {
                command: CommandId::ViewToggleTimeline,
                check: Some(false),
            }
        )
    });
    assert!(
        timeline_unchecked,
        "View menu should show unchecked Timeline"
    );
}

#[test]
fn toggle_round_trip_restores_state() {
    let mut s = shell();
    s.handle_command(CommandId::ViewToggleTimeline);
    assert!(!s.visibility().is_visible(PanelKind::Timeline));
    s.handle_command(CommandId::ViewToggleTimeline);
    assert!(s.visibility().is_visible(PanelKind::Timeline));
}

// ---------------------------------------------------------------------------
// Preset switching
// ---------------------------------------------------------------------------

#[test]
fn preset_switch_resets_visibility_and_menus() {
    let mut s = shell();
    assert_eq!(s.presets().active_builtin(), Some(BuiltinPreset::Edit));

    let outcome = s.handle_command(CommandId::WorkspaceColor);
    assert_eq!(outcome, CommandOutcome::Handled);
    assert_eq!(s.presets().active_builtin(), Some(BuiltinPreset::Color));

    // Scopes must now be visible (Color preset includes them).
    assert!(s.visibility().is_visible(PanelKind::Waveform));
    assert!(s.visibility().is_visible(PanelKind::Vectorscope));

    // Timeline must be hidden (not in Color preset).
    assert!(!s.visibility().is_visible(PanelKind::Timeline));

    // Workspace menu must check Color.
    let bar = s.menu_bar();
    let ws = bar.menu("menu.workspace").unwrap();
    let color_checked = ws.items.iter().any(|i| {
        matches!(
            i,
            MenuItem::Action {
                command: CommandId::WorkspaceColor,
                check: Some(true),
            }
        )
    });
    assert!(color_checked, "Workspace menu should check Color");
}

#[test]
fn preset_switch_then_toggle_preserves_override() {
    let mut s = shell();
    s.handle_command(CommandId::WorkspaceColor);
    // Waveform is visible in Color preset. Toggle it off.
    s.handle_command(CommandId::ViewToggleScopes);
    assert!(!s.visibility().is_visible(PanelKind::Waveform));

    // Switching back to Color must re-show scopes.
    s.handle_command(CommandId::WorkspaceColor);
    assert!(s.visibility().is_visible(PanelKind::Waveform));
}

// ---------------------------------------------------------------------------
// Layout <-> visibility contract
// ---------------------------------------------------------------------------

#[test]
fn every_preset_layout_has_visible_panels() {
    for preset in BuiltinPreset::ALL {
        let s = AppShell::new(preset, default_bindings());
        let layout = &s.presets().active().layout;
        let visible: Vec<_> = layout
            .panels()
            .into_iter()
            .filter(|p| s.visibility().is_visible(*p))
            .collect();
        assert!(
            !visible.is_empty(),
            "{preset:?} layout has no visible panels"
        );
    }
}

#[test]
fn visibility_filtering_hides_all_panels() {
    let layout = LayoutNode::split(
        Orientation::Horizontal,
        0.5,
        LayoutNode::leaf(PanelKind::Viewer),
        LayoutNode::leaf(PanelKind::Timeline),
    );
    let visibility = PanelVisibility::new();
    let visible: Vec<_> = layout
        .panels()
        .into_iter()
        .filter(|p| visibility.is_visible(*p))
        .collect();
    assert!(visible.is_empty());
}

#[test]
fn visibility_filtering_partial_tree() {
    let layout = LayoutNode::split(
        Orientation::Horizontal,
        0.5,
        LayoutNode::leaf(PanelKind::Viewer),
        LayoutNode::leaf(PanelKind::Timeline),
    );
    let visibility = PanelVisibility::with_visible([PanelKind::Viewer]);
    let visible: Vec<_> = layout
        .panels()
        .into_iter()
        .filter(|p| visibility.is_visible(*p))
        .collect();
    assert_eq!(visible, vec![PanelKind::Viewer]);
}

// ---------------------------------------------------------------------------
// panel_display_name
// ---------------------------------------------------------------------------

#[test]
fn all_panel_kinds_have_unique_display_names() {
    use ravel_app::panels::panel_display_name;
    use std::collections::HashSet;

    let mut seen = HashSet::new();
    for kind in PanelKind::ALL {
        let name = panel_display_name(kind);
        assert!(
            seen.insert(name.clone()),
            "duplicate display name {name:?} for {kind:?}"
        );
    }
    assert_eq!(seen.len(), PanelKind::ALL.len());
}

#[test]
fn display_names_are_non_empty() {
    use ravel_app::panels::panel_display_name;

    for kind in PanelKind::ALL {
        let name = panel_display_name(kind);
        assert!(!name.is_empty(), "empty display name for {kind:?}");
    }
}
