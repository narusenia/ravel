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
    let mode = match mode {
        ProgressMode::Auto if std::io::stderr().is_terminal() => ProgressMode::Bar,
        ProgressMode::Auto => ProgressMode::Json,
        other => other,
    };
    match mode {
        ProgressMode::Bar => Box::new(HumanReporter::default()),
        ProgressMode::Json => Box::new(JsonReporter),
        ProgressMode::Quiet => Box::new(QuietReporter),
        ProgressMode::Auto => unreachable!("resolved above"),
    }
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
        println!(
            "{}",
            t!("cli.result.completed")
                .replace("{count}", &summary.frames.to_string())
                .replace("{path}", &summary.directory.display().to_string())
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
        let mut stdout = std::io::stdout().lock();
        // A closed pipe (`| head`) is not a render failure, so it is ignored
        // rather than propagated.
        let _ = writeln!(stdout, "{value}");
        let _ = stdout.flush();
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
        Self::emit(serde_json::json!({
            "event": "completed",
            "frames": summary.frames,
            "directory": summary.directory,
            "first": summary.first,
            "last": summary.last,
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
}
