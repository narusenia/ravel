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
pub mod audio;
pub mod error;
pub mod execute;
pub mod interactive;
pub mod listing;
pub mod params;
pub mod plan;
pub mod report;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use ravel_core::cache_budget::{CacheBudgetConfig, SharedCacheBudget};
use ravel_core::runtime::eval_service::EvalWorkerHooks;
use ravel_core::runtime::{CONFLICT_SAMPLE, OverwritePolicy};
use ravel_gpu::GpuContext;
use ravel_media::encode::available_encoders;
use ravel_nodes::GpuEvalHooks;
use ravel_project::ProjectFile;
use ravel_project::settings::{self, SettingsLayer};

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
///
/// The anchoring rule itself is `ravel_project::project_root_of` — shared
/// with saving and with `AssetPath` resolution (REQ-PROJ-001), and not to be
/// re-derived here. What this adds is the working directory, because
/// `project_root_of` answers `None` for a path that names no directory at
/// all rather than silently rooting a *stored* reference at wherever the
/// process happens to be standing.
///
/// A command line is the case that reasoning does not cover:
/// `ravel-cli render project.ravprj` names a path **relative to the working
/// directory**, so that directory is not a guess, and without it every
/// `./relative` asset in the project stops resolving exactly when the user
/// is standing in the project's own folder. Making the path absolute first
/// and handing the result to the same rule is the whole fix.
fn project_root(path: &Path) -> Option<PathBuf> {
    match std::env::current_dir() {
        Ok(cwd) => ravel_project::project_root_of(&cwd.join(path)),
        // No working directory to be relative to: an absolute path still
        // anchors, a relative one has nothing to anchor against.
        Err(_) => ravel_project::project_root_of(path),
    }
}

/// Plan a render from arguments and the project they name.
pub fn plan_from_args(args: &RenderArgs) -> Result<RenderPlan, CliError> {
    load_plan(args).map(|(_, plan)| plan)
}

fn load_plan(args: &RenderArgs) -> Result<(ProjectFile, RenderPlan), CliError> {
    let project = load_project(&args.project)?;
    let plan = plan::plan_render(
        args,
        &project.document,
        project_root(&args.project).as_deref(),
        &available_encoders(),
    )?;
    Ok((project, plan))
}

fn cache_budget_for_layers(project: &ProjectFile, global: &SettingsLayer) -> CacheBudgetConfig {
    let resolved = project.resolved_settings(Some(global), None);
    settings::usable_cache_budget(&resolved)
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
    let conflicts = plan.conflicts();
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
///
/// It is handed the process's [`SharedCacheBudget`] because the hooks own
/// caches of their own — the texture pool and the shared decode cache — and
/// those have to answer to the same authority as the render worker's
/// evaluator (`cache-plan.md`, `CACHE-3`). Building the budget here rather
/// than inside the factory is what keeps the two halves the same one.
///
/// `global_settings` names the global settings file the budget is resolved
/// from, and is a parameter rather than the platform location because
/// otherwise every test of this function would render against whatever
/// `settings.toml` the developer happens to have. [`render`] passes the
/// platform location; `None` is "no global layer", which is also what an
/// absent file resolves to.
pub fn render_with_hooks<H, F>(
    args: &RenderArgs,
    global_settings: Option<&Path>,
    hooks: F,
    cancel: &CancelFlag,
    reporter: &mut dyn Reporter,
) -> Result<Summary, CliError>
where
    H: EvalWorkerHooks,
    F: FnOnce(&SharedCacheBudget) -> Result<H, CliError>,
{
    let (project, plan) = load_plan(args)?;
    refuse_existing_output(&plan)?;
    for warning in &plan.warnings {
        let (id, message) = warning_text(warning);
        reporter.note(id, &message);
    }

    // The device before anything is written. `hooks()` is the last refusal a
    // render can make — a machine with no adapter — and evaluating it as an
    // argument to `execute` would let it escape past the sound, which is
    // already on disk by then.
    // Resolve the same global → project settings layers the GUI uses, then
    // pass them through the shared cache validation before the budget is
    // handed to both the hooks and the evaluation worker.
    let global = settings::read_global_settings_at(global_settings);
    let budget = SharedCacheBudget::new(cache_budget_for_layers(&project, &global));
    let hooks = hooks(&budget)?;

    // Sound first: its warnings are worth having before an hour of frames,
    // and undoing one WAV is simpler than undoing however many frames the
    // worker wrote. See `audio`'s module docs.
    let audio = audio::render_audio(&plan, reporter)?;

    // Every exit from here to the `publish` below drops `audio`, which takes
    // its temporary file with it and never touches the soundtrack's real name
    // — so a failed or cancelled render leaves neither a soundtrack without
    // frames nor a `--overwrite` target destroyed for nothing.
    let frames = execute::execute(hooks, budget, &plan, cancel, reporter)?;
    let audio = audio.map(audio::PendingAudio::publish).transpose()?;

    Ok(Summary {
        frames,
        directory: plan.output.directory().to_path_buf(),
        first: plan.output.frame_path(plan.range.start),
        last: plan.output.frame_path(plan.range.end.saturating_sub(1)),
        audio,
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
        ravel_project::paths::global_settings_path().as_deref(),
        |budget| {
            let gpu =
                GpuContext::new_blocking().map_err(|error| CliError::Gpu(error.to_string()))?;
            Ok(GpuEvalHooks::with_budget(gpu, budget.clone()))
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
                report::print_line(&json);
                EXIT_OK
            }
            Err(error) => {
                eprintln!("{}", error.localized());
                error.code()
            }
        },
        Command::Interactive(arg) => run_interactive(&arg.project),
    }
}

/// Ask for a render and then run it exactly as `render` would have.
///
/// The gate comes first, before the project is even opened: a prompt with
/// nobody to answer it is a hang, and a hang is worse than any refusal this
/// crate can report. Everything after it is the non-interactive path —
/// [`run_render`] plans, refuses and renders the collected arguments with no
/// idea where they came from.
fn run_interactive(project: &Path) -> u8 {
    let outcome = interactive::gate(std::io::stdin().is_terminal()).and_then(|()| {
        let file = load_project(project)?;
        interactive::collect(
            &mut interactive::TerminalPrompt::default(),
            project,
            &file.document,
            project_root(project).as_deref(),
            &available_encoders(),
        )
    });
    match outcome {
        // The project is loaded a second time by the render itself. It is a
        // read either way, and going through the one entry point is what
        // makes "the session is a command line" true of the code and not
        // only of the arguments.
        Ok(args) => run_render(&args),
        Err(error) => {
            eprintln!("{}", error.localized());
            error.code()
        }
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

    let outcome =
        install_interrupt_handler(&cancel).and_then(|()| render(args, &cancel, reporter.as_mut()));
    match outcome {
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
/// A handler that cannot be installed is **fatal, before the render starts**.
/// Continuing would leave Ctrl-C on its default behaviour, which kills the
/// process where it stands and leaves however many frames were already
/// written — the exact outcome "an interrupted render leaves nothing behind"
/// promises will not happen. A guarantee that quietly downgrades to a warning
/// line is worse than no guarantee, so the run is refused instead.
fn install_interrupt_handler(cancel: &CancelFlag) -> Result<(), CliError> {
    let cancel = cancel.clone();
    ctrlc::set_handler(move || cancel.request())
        .map_err(|error| CliError::InterruptHandler(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare file name is the case `Path::parent` answers `""` for, and the
    /// case a user hits by standing in the project's own directory. It has to
    /// anchor at the working directory, not at "no root at all", or every
    /// `./relative` asset reference in that project stops resolving.
    #[test]
    fn a_project_named_without_a_directory_still_has_a_root() {
        let cwd = std::env::current_dir().expect("a working directory");
        assert_eq!(
            project_root(Path::new("project.ravprj")),
            Some(cwd.clone()),
            "a bare file name anchors at the working directory"
        );
        assert_eq!(
            project_root(Path::new("nested/project.ravprj")),
            Some(cwd.join("nested")),
            "a relative directory is absolutised, not left relative"
        );
        // Unix only: on Windows `/abs/…` has a root but no drive prefix, so
        // joining it onto the working directory keeps that drive and the
        // literal `/abs` is not what comes back. The rule being pinned here —
        // an already-absolute path keeps its own parent — belongs to
        // `project_root_of` and is unchanged by this function on any platform;
        // the two assertions above are the regression it exists to catch.
        #[cfg(unix)]
        assert_eq!(
            project_root(Path::new("/abs/project.ravprj")),
            Some(PathBuf::from("/abs")),
            "an absolute path keeps its own parent"
        );
    }

    #[test]
    fn a_headless_render_uses_persisted_global_and_project_cache_settings() {
        const MIB: u64 = 1024 * 1024;

        let dir = tempfile::tempdir().expect("tempdir");
        let global_path = dir.path().join("settings.toml");
        std::fs::write(
            &global_path,
            "[cache]\nvram_limit_mb = 64\nram_limit_mb = 256\nsim_reserve_ratio = 0.5\n",
        )
        .expect("global settings");
        let global = settings::read_global_settings_at(Some(&global_path));

        let mut project = ProjectFile::new("render", "2026-01-01T00:00:00Z");
        // The same key both layers state, so the assertion below fails if the
        // two are ever resolved in the other order.
        project.settings.cache.vram_limit_mb = Some(128);
        let budget = cache_budget_for_layers(&project, &global);

        assert_eq!(budget.vram_bytes, 128 * MIB, "the project layer wins");
        assert_eq!(
            budget.ram_bytes,
            256 * MIB,
            "the global layer still applies"
        );
        assert_eq!(budget.sim_reserve_ratio, 0.5);
    }

    #[test]
    fn a_headless_render_reuses_shared_cache_validation_rules() {
        const MIB: u64 = 1024 * 1024;

        let global = SettingsLayer::from_toml(
            "[cache]\nvram_limit_mb = 0\nsim_reserve_ratio = 2.0\nroot = \"relative/cache\"\n",
        )
        .expect("settings parse");
        let mut project = ProjectFile::new("render", "2026-01-01T00:00:00Z");
        project.settings.cache.ram_limit_mb = Some(99_999_999);
        let resolved = project.resolved_settings(Some(&global), None);
        let budget = cache_budget_for_layers(&project, &global);
        let defaults = ravel_project::settings::ResolvedSettings::default();

        assert_eq!(budget.vram_bytes, defaults.cache_vram_limit_mb * MIB);
        assert_eq!(budget.ram_bytes, defaults.cache_ram_limit_mb * MIB);
        assert_eq!(budget.sim_reserve_ratio, defaults.cache_sim_reserve_ratio);
        // The unusable `cache.root` reaches the resolved settings untouched and
        // changes nothing here: the CLI builds no disk cache yet (`CACHE-11`),
        // so it has no consumer of the setting. Whether a relative location is
        // refused is `ravel_project::settings::cache_root_setting`'s rule and is
        // pinned in that crate's own tests, not through this path.
        assert_eq!(resolved.cache_root.as_deref(), Some("relative/cache"));
    }

    /// The interrupt handler is a precondition, not a nicety: when it cannot
    /// be installed the CLI has to say so rather than render without the
    /// cleanup it advertises. `ctrlc` refuses a second handler in a process,
    /// which is how the failure is reachable deterministically.
    ///
    /// The only test in this crate that touches the process-wide handler.
    #[test]
    fn a_handler_that_cannot_be_installed_is_a_failure() {
        let cancel = CancelFlag::new();
        install_interrupt_handler(&cancel).expect("the first handler installs");
        let error = install_interrupt_handler(&cancel).expect_err("the second cannot");
        assert_eq!(error.code(), crate::error::EXIT_INTERNAL);
        assert_eq!(error.id(), "interrupt-handler");
    }
}
