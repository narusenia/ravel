// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPUI-based UI shell for Ravel.
//!
//! This crate implements the application shell: the workspace panel taxonomy,
//! workspace presets (Edit / Node / Color / Motion), the menu bar model, a
//! fully customizable keybinding system, multi-window / panel-detach
//! bookkeeping, and the Properties inspector shell. These pieces are kept
//! framework-agnostic and headless so they are unit-testable without a live
//! window.
//!
//! The live GPUI integration — `gpui::App` bootstrap, window creation, native
//! menu bar wiring, and per-panel views built on `gpui_component`'s dock/sheet
//! — is layered on top of this state in the application host (`ravel-app`).
//! [`AppShell`] is the headless state object that host drives: it owns the
//! workspace, keybindings, detached windows, and inspector, and exposes command
//! dispatch (`handle_command` / `handle_chord`) plus a live menu-bar builder.
//!
//! See `docs/specifications/ui-spec.md` and `docs/implementation/tasks/TASK-006.md`.

pub mod command;
pub mod keybindings;
pub mod menu;
pub mod panel;
pub mod panels;
pub mod preset;
pub mod shell;
pub mod window;

pub use command::CommandId;
pub use keybindings::{KeyBindings, KeyChord};
pub use menu::{Menu, MenuBar, MenuItem};
pub use panel::{PanelKind, PanelVisibility};
pub use preset::{BuiltinPreset, LayoutNode, Orientation, PresetLibrary, WorkspacePreset};
pub use shell::{AppShell, CommandOutcome};
pub use window::{WindowError, WindowId, WindowManager, WindowPlacement};
