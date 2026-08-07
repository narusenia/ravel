// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `ravel-cli` — rendering a Ravel project without a window.
//!
//! # A separate binary, not a subcommand of `ravel`
//!
//! The GUI binary links GPUI; this one must not, and "must not" is worth a
//! compile-time guarantee rather than a rule about which branch calls
//! `application()`. So the CLI is its own crate whose dependency list simply
//! does not contain `gpui`, exactly as `ravel-project` is GUI-free by
//! construction (`docs/implementation/render-export-plan.md`, "CLI は GPUI に
//! リンクしない別バイナリにする").
//!
//! # The shape, and why
//!
//! ```text
//!   argv ─▶ args ─▶ plan_render ─▶ RenderPlan ─▶ execute ─▶ RenderQueue
//!                        ▲                          │
//!                  loaded project              RenderEvent
//!                                                   ▼
//!                                              Reporter (bar / JSON / quiet)
//! ```
//!
//! Two seams matter beyond tidiness:
//!
//! * **[`plan::plan_render`] is a function of arguments and a document**, so
//!   everything that can refuse a render is decided before a job exists. The
//!   interactive mode (`EXPORT-7`) builds the same
//!   [`RenderArgs`](args::RenderArgs) from answers and calls it after each
//!   one, instead of re-deriving the same checks;
//! * **[`execute::execute`] is generic over the evaluation hooks**, so the
//!   binary passes the GPU ones and the tests pass a stub. That is what lets
//!   the guarantees the plan names — absolute frame numbering, split-range
//!   equivalence, no partial output after an interrupt — be tested without a
//!   device. The hooks arrive as a *factory* rather than as a value, which
//!   is what keeps the device out of the picture until every refusal the
//!   arguments and the project can produce has already happened.
//!
//! Nothing here writes to the project. A `.ravprj` from an older format
//! migrates in memory and is never saved back: a render is a read.

pub mod args;
pub mod error;
pub mod execute;
pub mod listing;
pub mod params;
pub mod plan;
pub mod report;

use std::path::{Path, PathBuf};

use ravel_core::runtime::eval_service::EvalWorkerHooks;
use ravel_core::runtime::{CONFLICT_SAMPLE, OverwritePolicy};
use ravel_gpu::GpuContext;
use ravel_media::encode::available_encoders;
use ravel_nodes::GpuEvalHooks;
use ravel_project::ProjectFile;

use crate::args::{Cli, Command, ListCommand, RenderArgs};
use crate::error::{CliError, EXIT_OK};
use crate::execute::CancelFlag;
use crate::plan::RenderPlan;
use crate::report::{Reporter, Summary, warning_text};

/// Where the locale catalogs are, searched the way the GUI binary searches.
///
/// The extra `../../assets/locales` candidate covers a binary run straight
/// out of `target/<profile>/`, which is how every test and every
/// `cargo run -p ravel-cli` invokes it. `RAVEL_LOCALE_DIR` overrides
/// everything, for a packaged layout none of these guesses fit.
pub fn locale_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RAVEL_LOCALE_DIR") {
        return PathBuf::from(dir);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_dir = exe.parent().unwrap_or(exe.as_path());
    let candidates = [
        exe_dir.join("../Resources/locales"),
        exe_dir.join("assets/locales"),
        // `target/<profile>/ravel-cli` → the workspace root.
        exe_dir.join("../../assets/locales"),
        PathBuf::from("assets/locales"),
    ];
    candidates
        .into_iter()
        .find(|path| path.is_dir())
        .unwrap_or_else(|| PathBuf::from("assets/locales"))
}

/// Load the catalogs, falling back to raw keys when they cannot be found.
///
/// A missing catalog is not a reason to refuse to render: `t!` returns the
/// key, which is ugly but harmless, and the exit codes — the part a script
/// depends on — are unaffected.
pub fn init_locale() {
    if let Err(error) = ravel_i18n::init(&locale_dir(), "en") {
        tracing::warn!(%error, "locale catalogs unavailable; messages will show their keys");
    }
}

/// Read a project. **Never writes**, including when the file is an older
/// format version: migration happens in memory.
pub fn load_project(path: &Path) -> Result<ProjectFile, CliError> {
    ProjectFile::load(path).map_err(|error| CliError::Load {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })
}

/// The project root relative asset paths resolve against.
fn project_root(path: &Path) -> Option<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

/// Plan a render from arguments and the project they name.
pub fn plan_from_args(args: &RenderArgs) -> Result<RenderPlan, CliError> {
    let project = load_project(&args.project)?;
    plan::plan_render(
        args,
        &project.document,
        project_root(&args.project),
        &available_encoders(),
    )
}

/// Refuse a render that would land on files that are already there.
///
/// The worker performs this check too, and its copy is the authoritative one:
/// it runs at the instant the job starts, which is the only moment the answer
/// is not already stale. This one runs earlier so the refusal does not queue
/// behind building a GPU context — on a machine with no adapter that would
/// report "no usable GPU" for a question that has nothing to do with one.
/// Both call [`RenderOutput::conflicts`](ravel_core::runtime::RenderOutput::conflicts),
/// so "already there" has one definition rather than two.
fn refuse_existing_output(plan: &RenderPlan) -> Result<(), CliError> {
    if plan.overwrite != OverwritePolicy::Refuse {
        return Ok(());
    }
    let conflicts = plan.render_output().conflicts(plan.range.clone());
    if conflicts.is_empty() {
        return Ok(());
    }
    let total = conflicts.len();
    let mut sample = conflicts;
    sample.truncate(CONFLICT_SAMPLE);
    Err(CliError::OutputExists {
        first: sample.first().cloned().unwrap_or_default(),
        total,
        sample,
    })
}

/// Render with evaluation hooks built by `hooks`.
///
/// Split from [`render`] so a test can supply hooks that need no GPU while
/// exercising the same planning, the same worker and the same encoder.
///
/// `hooks` is a factory rather than a value because **the order is the
/// guarantee**: the project is loaded, the plan is decided and the output is
/// checked first, and only then is anything expensive built. Handing over a
/// constructed `H` would let the caller pay for a device before the CLI knows
/// whether it has a render to run — which is how a headless machine came to
/// report a misspelled `--param` as "no usable GPU adapter", collapsing every
/// classified exit code into `1`.
pub fn render_with_hooks<H, F>(
    args: &RenderArgs,
    hooks: F,
    cancel: &CancelFlag,
    reporter: &mut dyn Reporter,
) -> Result<Summary, CliError>
where
    H: EvalWorkerHooks,
    F: FnOnce() -> Result<H, CliError>,
{
    let plan = plan_from_args(args)?;
    refuse_existing_output(&plan)?;
    for warning in &plan.warnings {
        let (id, message) = warning_text(warning);
        reporter.note(id, &message);
    }
    let frames = execute::execute(hooks()?, &plan, cancel, reporter)?;
    Ok(Summary {
        frames,
        directory: plan.output.directory().to_path_buf(),
        first: plan.output.frame_path(plan.range.start),
        last: plan.output.frame_path(plan.range.end.saturating_sub(1)),
    })
}

/// Render with the real GPU-backed hooks — the same ones the application's
/// interactive evaluator uses, which is what makes "the CLI and the export
/// UI go through one path" true rather than aspirational.
pub fn render(
    args: &RenderArgs,
    cancel: &CancelFlag,
    reporter: &mut dyn Reporter,
) -> Result<Summary, CliError> {
    render_with_hooks(
        args,
        || {
            let gpu =
                GpuContext::new_blocking().map_err(|error| CliError::Gpu(error.to_string()))?;
            Ok(GpuEvalHooks::new(gpu))
        },
        cancel,
        reporter,
    )
}

/// Run one command line and return the process exit code.
pub fn run(cli: Cli) -> u8 {
    match cli.command {
        Command::Render(args) => run_render(&args),
        Command::List(command) => match run_list(command) {
            Ok(json) => {
                println!("{json}");
                EXIT_OK
            }
            Err(error) => {
                eprintln!("{}", error.localized());
                error.code()
            }
        },
    }
}

fn run_list(command: ListCommand) -> Result<String, CliError> {
    match command {
        ListCommand::Comps(arg) => listing::comps_json(&load_project(&arg.project)?.document),
        ListCommand::Params(arg) => listing::params_json(&load_project(&arg.project)?.document),
        ListCommand::Codecs => listing::codecs_json(&available_encoders()),
    }
}

fn run_render(args: &RenderArgs) -> u8 {
    let mut reporter = report::reporter(args.progress);
    let cancel = CancelFlag::new();

    install_interrupt_handler(&cancel);

    match render(args, &cancel, reporter.as_mut()) {
        Ok(summary) => {
            reporter.success(&summary);
            EXIT_OK
        }
        Err(error) => {
            reporter.failure(&error);
            error.code()
        }
    }
}

/// Turn Ctrl-C into a cancellation instead of a `SIGKILL`-shaped exit.
///
/// The difference is the deliverable: an abrupt exit leaves however many
/// frames were written, and the plan's guarantee is that an interrupted
/// render leaves nothing. The handler therefore only raises the flag; the
/// worker stops at the next frame boundary and the encoder removes what it
/// made.
///
/// A handler that cannot be installed is logged rather than fatal — the
/// render is still correct, it just cannot be interrupted cleanly.
fn install_interrupt_handler(cancel: &CancelFlag) {
    let cancel = cancel.clone();
    if let Err(error) = ctrlc::set_handler(move || cancel.request()) {
        tracing::warn!(%error, "interrupt handler unavailable; Ctrl-C will not clean up");
    }
}
