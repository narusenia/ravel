// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Command-dispatch tracing.
//!
//! Records every point where a command enters or is executed by the dispatch
//! machinery so that undelivered and double-executed commands can be
//! distinguished. The recorder is a plain `Global<Vec<_>>` (bounded to the
//! most recent entries) so headless regression tests can assert on the exact
//! dispatch sequence; each entry is also mirrored to the
//! `ravel::command_trace` debug log target for live debugging.

use gpui::{App, Global};
use ravel_ui::command::CommandId;
use ravel_ui::panel::PanelKind;

/// Where in the dispatch machinery a trace entry was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceSource {
    /// App-level fallback `cx.on_action` handler.
    AppAction,
    /// Workspace-level `on_action` listener registered in `render()`.
    WorkspaceAction,
    /// Raw `on_key_down` handling inside a panel (bypasses the command system).
    PanelKeyDown,
}

/// One observed step of command dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEntry {
    /// Which dispatch path recorded this entry.
    pub source: TraceSource,
    /// The command involved, when the path knows it.
    pub command: Option<CommandId>,
    /// Value of `FocusedPanelGlobal` at the time of recording.
    pub focused_panel: Option<PanelKind>,
    /// Identifies the concrete handler (for humans reading logs).
    pub handler: &'static str,
    /// `CommandOutcome` (or panel-local effect) if the handler executed one.
    pub outcome: Option<String>,
}

/// Upper bound on retained entries; older entries are dropped first so the
/// recorder cannot grow without bound in a long-running session.
const MAX_TRACE_ENTRIES: usize = 256;

/// Global recorder. Recording is skipped entirely when the global has not
/// been installed.
#[derive(Default)]
pub struct CommandTrace(pub Vec<TraceEntry>);

impl Global for CommandTrace {}

/// Installs an empty recorder.
pub fn init(cx: &mut App) {
    cx.set_global(CommandTrace::default());
}

/// Records one dispatch step and mirrors it to the `ravel::command_trace`
/// log target (`RAVEL_LOG=ravel::command_trace=debug`).
pub fn record(cx: &mut App, entry: TraceEntry) {
    tracing::debug!(
        target: "ravel::command_trace",
        source = ?entry.source,
        command = entry.command.map(|c| c.as_str()),
        focused_panel = ?entry.focused_panel,
        handler = entry.handler,
        outcome = entry.outcome.as_deref(),
        "command dispatch step"
    );
    if cx.has_global::<CommandTrace>() {
        let trace = cx.global_mut::<CommandTrace>();
        if trace.0.len() >= MAX_TRACE_ENTRIES {
            trace.0.remove(0);
        }
        trace.0.push(entry);
    }
}

/// Convenience: reads `FocusedPanelGlobal` without requiring it to exist.
pub fn focused_panel(cx: &App) -> Option<PanelKind> {
    cx.try_global::<crate::panels::FocusedPanelGlobal>()
        .and_then(|g| g.0)
}

/// Number of times `command` was actually executed (i.e. reached a handler
/// that ran `AppShell::handle_command` or an equivalent panel-local effect).
pub fn execution_count(cx: &App, command: CommandId) -> usize {
    cx.try_global::<CommandTrace>()
        .map(|t| {
            t.0.iter()
                .filter(|e| {
                    e.command == Some(command)
                        && e.outcome.is_some()
                        && matches!(
                            e.source,
                            TraceSource::WorkspaceAction | TraceSource::PanelKeyDown
                        )
                })
                .count()
        })
        .unwrap_or(0)
}
