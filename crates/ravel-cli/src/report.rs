// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Saying what is happening: a progress bar for a person, JSON lines for a
//! script, and nothing at all when asked.
//!
//! The arithmetic behind a progress line — frames written of frames asked
//! for — is **not** here. It is
//! [`JobProgress`](ravel_core::runtime::JobProgress) in `ravel-core`, which
//! the render queue panel (`EXPORT-5`) reads too. What lives here is only
//! the choice of words and where they go, which is genuinely per-front-end.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use indicatif::{ProgressBar, ProgressStyle};
use ravel_core::runtime::JobProgress;
use ravel_i18n::t;

use crate::args::ProgressMode;
use crate::error::CliError;
use crate::plan::Warning;

/// What a finished render produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub frames: u64,
    pub directory: PathBuf,
    pub first: PathBuf,
    pub last: PathBuf,
}

/// Where the narration goes.
///
/// Every method has a default, so a mode that says nothing implements
/// nothing.
pub trait Reporter {
    /// Something the user should know that does not stop the render.
    fn note(&mut self, _id: &str, _message: &str) {}
    /// The job moved.
    fn update(&mut self, _progress: &JobProgress) {}
    /// The render finished and the files are on disk.
    fn success(&mut self, _summary: &Summary) {}
    /// The render did not happen, or did not finish.
    fn failure(&mut self, _error: &CliError) {}
}

/// Build the reporter `mode` asks for. `Auto` follows the terminal: a bar
/// when someone is watching, JSON lines when the output is being read by
/// something.
pub fn reporter(mode: ProgressMode) -> Box<dyn Reporter> {
    match resolve(mode, std::io::stdout().is_terminal()) {
        ProgressMode::Bar => Box::new(HumanReporter::default()),
        ProgressMode::Json => Box::new(JsonReporter),
        ProgressMode::Quiet => Box::new(QuietReporter),
        ProgressMode::Auto => unreachable!("resolved above"),
    }
}

/// Turn `Auto` into a concrete mode.
///
/// The question is about **stdout**, not stderr, because stdout is the stream
/// whose contents differ between the two answers: `HumanReporter::success`
/// prints a localized sentence there and `JsonReporter` prints a record.
/// `ravel-cli render … > result.txt` leaves stderr attached to the terminal,
/// so asking stderr would put prose in the file a script is about to parse —
/// which is the CLI's whole purpose. Where the *bar* goes is a separate
/// question, already answered: stderr, always.
fn resolve(mode: ProgressMode, stdout_is_terminal: bool) -> ProgressMode {
    match mode {
        ProgressMode::Auto if stdout_is_terminal => ProgressMode::Bar,
        ProgressMode::Auto => ProgressMode::Json,
        other => other,
    }
}

/// Write one line to stdout, ignoring a write that cannot land.
///
/// **Every stdout write of this crate goes through here.** `println!` panics
/// on a write error, and Rust ignores `SIGPIPE`, so a consumer that leaves
/// early — `ravel-cli list codecs | head -1`, a `jq -e` that exits on the
/// first match, a closed pager — would turn a command that did its job into
/// a panic with status 101. The exit code is this crate's contract with the
/// script calling it, and a broken pipe is the reader's decision, not a
/// render failure.
pub fn print_line(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{text}");
    let _ = stdout.flush();
}

/// The identifier and the sentence for a warning.
///
/// The identifier is stable and is what a script matches on; the sentence is
/// localized and is what a person reads.
pub fn warning_text(warning: &Warning) -> (&'static str, String) {
    match warning {
        Warning::AudioNotRendered { layers } => (
            "audio-not-rendered",
            t!("cli.warn.audio_not_rendered").replace("{count}", &layers.to_string()),
        ),
        Warning::BindingIssue { detail } => (
            "binding-issue",
            t!("cli.warn.binding_issue").replace("{detail}", detail),
        ),
    }
}

// ===========================================================================
// For a person
// ===========================================================================

/// A progress bar on stderr, so a redirected stdout stays clean.
#[derive(Default)]
struct HumanReporter {
    bar: Option<ProgressBar>,
}

impl Reporter for HumanReporter {
    fn note(&mut self, _id: &str, message: &str) {
        match &self.bar {
            // `suspend` keeps the bar from being torn in half by the line.
            Some(bar) => bar.suspend(|| eprintln!("{message}")),
            None => eprintln!("{message}"),
        }
    }

    fn update(&mut self, progress: &JobProgress) {
        let bar = self.bar.get_or_insert_with(|| {
            let bar = ProgressBar::new(progress.total_frames());
            // No template literal in the locale catalogs: this is indicatif's
            // layout language, not a sentence.
            if let Ok(style) = ProgressStyle::with_template("{bar:32} {pos}/{len} {msg}") {
                bar.set_style(style);
            }
            bar
        });
        bar.set_length(progress.total_frames());
        bar.set_position(progress.rendered());
        if progress.is_finished() {
            bar.finish_and_clear();
        }
    }

    fn success(&mut self, summary: &Summary) {
        if let Some(bar) = self.bar.take() {
            bar.finish_and_clear();
        }
        print_line(
            &t!("cli.result.completed")
                .replace("{count}", &summary.frames.to_string())
                .replace("{path}", &summary.directory.display().to_string()),
        );
    }

    fn failure(&mut self, error: &CliError) {
        if let Some(bar) = self.bar.take() {
            bar.finish_and_clear();
        }
        eprintln!("{}", error.localized());
    }
}

// ===========================================================================
// For a script
// ===========================================================================

/// One JSON object per line on stdout.
///
/// Line-delimited rather than one document, because a render is a stream:
/// a consumer reads a line per frame as it happens instead of waiting for
/// the array to close.
struct JsonReporter;

impl JsonReporter {
    fn emit(value: serde_json::Value) {
        print_line(&value.to_string());
    }
}

impl Reporter for JsonReporter {
    fn note(&mut self, id: &str, message: &str) {
        Self::emit(serde_json::json!({
            "event": "note",
            "id": id,
            "message": message,
        }));
    }

    fn update(&mut self, progress: &JobProgress) {
        Self::emit(serde_json::json!({
            "event": "progress",
            "job": progress.job().raw(),
            "frame": progress.last_frame(),
            "rendered": progress.rendered(),
            "total_frames": progress.total_frames(),
        }));
    }

    fn success(&mut self, summary: &Summary) {
        // `display()` rather than the paths themselves: `PathBuf`'s
        // `Serialize` fails on a path that is not UTF-8, and `json!` panics
        // on a value it cannot serialize. Panicking *after* the frames are
        // written would throw away a render that succeeded.
        Self::emit(serde_json::json!({
            "event": "completed",
            "frames": summary.frames,
            "directory": summary.directory.display().to_string(),
            "first": summary.first.display().to_string(),
            "last": summary.last.display().to_string(),
        }));
    }

    fn failure(&mut self, error: &CliError) {
        Self::emit(serde_json::json!({
            "event": "failed",
            "error": error.id(),
            "exit_code": error.code(),
            "message": error.localized(),
            "detail": error.to_string(),
        }));
    }
}

// ===========================================================================
// For nobody
// ===========================================================================

/// Silent but for the failure, which a caller that asked for quiet still has
/// to be able to see without parsing an exit code.
struct QuietReporter;

impl Reporter for QuietReporter {
    fn failure(&mut self, error: &CliError) {
        eprintln!("{}", error.localized());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both warnings carry an identifier a script can match without reading
    /// the sentence, which is the whole reason the identifier exists.
    #[test]
    fn every_warning_has_a_stable_identifier() {
        assert_eq!(
            warning_text(&Warning::AudioNotRendered { layers: 2 }).0,
            "audio-not-rendered"
        );
        assert_eq!(
            warning_text(&Warning::BindingIssue { detail: "x".into() }).0,
            "binding-issue"
        );
    }

    /// A redirected stdout is a machine reading it, whatever stderr is doing.
    #[test]
    fn auto_follows_stdout_and_nothing_else() {
        assert_eq!(resolve(ProgressMode::Auto, false), ProgressMode::Json);
        assert_eq!(resolve(ProgressMode::Auto, true), ProgressMode::Bar);
    }

    /// An explicit mode is never second-guessed: `--progress bar` into a pipe
    /// is a person watching through `less`, not a mistake to correct.
    #[test]
    fn an_explicit_mode_survives_the_terminal_question() {
        for mode in [ProgressMode::Bar, ProgressMode::Json, ProgressMode::Quiet] {
            assert_eq!(resolve(mode, true), mode);
            assert_eq!(resolve(mode, false), mode);
        }
    }
}
