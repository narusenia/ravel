// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `ravel-cli interactive` — the same render, asked for instead of typed.
//!
//! # A layer, not a second front end
//!
//! This module owns no knowledge of what a project contains and no opinion
//! about what is renderable. It asks [`listing`](crate::listing) what the
//! choices are — the very functions `ravel-cli list comps | codecs` prints —
//! and it decides nothing about an answer: after every one it builds a
//! [`RenderArgs`] and hands it to [`plan_render`], the function `argv` goes
//! through. A refusal is shown and the question is asked again.
//!
//! That is what makes this unit's claim testable rather than aspirational:
//! **the answers are a command line**. [`equivalent_argv`] writes it out, and
//! a test parses that back with `clap` and plans both.
//!
//! # Where the terminal question is asked
//!
//! [`gate`] takes the answer rather than asking it, exactly as
//! [`report`](crate::report)'s mode resolution does, so a test can walk both
//! branches on a machine with no terminal. Two different questions, though:
//! `report` asks about **stdout**, because that is where machine-readable
//! output goes, and this asks about **stdin**, because that is where an
//! answer would have to come from. A prompt written into a pipe waits for an
//! answer nobody is going to send, and the script that started it hangs
//! rather than fails — so the gate runs before the project is even loaded.
//!
//! # Progress and machine-readable output
//!
//! [`PROGRESS`] is [`ProgressMode::Bar`], not [`ProgressMode::Auto`]. A
//! session that has just been driven by hand is a person watching, and
//! fixing the mode is how "the bar and the JSON are exclusive" becomes
//! structural: this path cannot reach [`ProgressMode::Json`], so no bar is
//! ever drawn across a stream something is parsing, and the JSON a script
//! reads is produced by `ravel-cli render` alone — unchanged by whether a
//! bar was drawn somewhere else.

use std::path::Path;

use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Select};
use ravel_core::composition::Document;
use ravel_core::exposed::ExposedValue;
use ravel_core::exposed::listing::{ExposedListing, ExposedListingEntry};
use ravel_core::media::encode::EncoderAvailability;
use ravel_i18n::t;

use crate::args::{OutputFormat, PngBits, ProgressMode, RenderArgs};
use crate::error::CliError;
use crate::listing;
use crate::plan::{RenderPlan, plan_render};

/// How an interactively started render narrates itself. See the module docs.
pub const PROGRESS: ProgressMode = ProgressMode::Bar;

/// The file name pieces this mode never asks about, and so never changes.
///
/// They duplicate `clap`'s defaults in [`RenderArgs`] so [`equivalent_argv`]
/// can leave them out of the command it writes; `clap_defaults_are_unchanged`
/// is the test that keeps the two copies honest.
const DEFAULT_PREFIX: &str = "frame_";
const DEFAULT_SUFFIX: &str = "";
const DEFAULT_PADDING: usize = 4;

// ===========================================================================
// Asking
// ===========================================================================

/// The three questions a session asks, and the refusal it shows.
///
/// A trait rather than direct `dialoguer` calls so the whole dialogue can be
/// driven from a test by a scripted set of answers — on a machine with no
/// terminal, which is every machine running CI.
pub trait Prompt {
    /// Choose one of `options`. `default` indexes the pre-selected one.
    fn select(
        &mut self,
        question: &str,
        options: &[String],
        default: usize,
    ) -> Result<usize, CliError>;

    /// Type a line. An empty answer means `default` was accepted.
    fn text(&mut self, question: &str, default: &str) -> Result<String, CliError>;

    /// Answer yes or no.
    fn confirm(&mut self, question: &str, default: bool) -> Result<bool, CliError>;

    /// Show why the last answer was refused, before it is asked again.
    fn note(&mut self, message: &str);
}

/// Refuse a session nobody can answer.
///
/// The argument is passed in rather than probed, so both branches are
/// reachable from a test. The caller supplies
/// `std::io::stdin().is_terminal()`.
pub fn gate(stdin_is_terminal: bool) -> Result<(), CliError> {
    if stdin_is_terminal {
        Ok(())
    } else {
        Err(CliError::NotInteractive)
    }
}

/// Ask for a render, and return the arguments it would have been typed as.
///
/// Every answer is validated by [`plan_render`] — the same function `argv`
/// goes through — and a refusal re-asks rather than aborting. The final
/// confirmation is part of the session: it is the last chance to walk away
/// after seeing what the answers add up to, and declining is
/// [`CliError::Cancelled`], the same class as a Ctrl-C.
pub fn collect(
    prompt: &mut dyn Prompt,
    project: &Path,
    document: &Document,
    project_root: Option<&Path>,
    encoders: &[EncoderAvailability],
) -> Result<RenderArgs, CliError> {
    let mut args = RenderArgs {
        project: project.to_path_buf(),
        comp: None,
        range: None,
        format: OutputFormat::Png,
        png_depth: PngBits::Eight,
        output: std::path::PathBuf::new(),
        prefix: DEFAULT_PREFIX.to_string(),
        suffix: DEFAULT_SUFFIX.to_string(),
        padding: DEFAULT_PADDING,
        params: Vec::new(),
        overwrite: false,
        no_audio: false,
        progress: PROGRESS,
    };

    args.comp = Some(ask_composition(prompt, document)?);
    args.format = ask_format(prompt, encoders)?;
    if args.format == OutputFormat::Png {
        args.png_depth = ask_png_depth(prompt)?;
    }

    // The output directory is asked in a loop against the whole plan, so a
    // name `ravel-core` refuses — one that would escape the directory, say —
    // is caught here rather than after every remaining question.
    loop {
        args.output = ask_output(prompt)?;
        match plan_render(&args, document, project_root, encoders) {
            Ok(plan) => match resolve_conflicts(prompt, &plan)? {
                Conflicts::None => break,
                Conflicts::Replace => {
                    args.overwrite = true;
                    break;
                }
                // Chose to keep the files: ask for somewhere else to write.
                Conflicts::Elsewhere => continue,
            },
            Err(error) => prompt.note(&error.localized()),
        }
    }

    ask_parameters(prompt, &mut args, document, project_root, encoders)?;

    let confirm =
        t!("cli.prompt.confirm").replace("{command}", &shell_join(&equivalent_argv(&args)));
    if prompt.confirm(&confirm, true)? {
        Ok(args)
    } else {
        Err(CliError::Cancelled)
    }
}

/// Which composition, spelled the way `--comp` will resolve back to it.
///
/// `--comp` reads a **name first** and only then an id, because a composition
/// called "2" must not be shadowed by the composition whose id is 2
/// (`plan::resolve_comp`). Handing back the id unconditionally would
/// therefore turn a chosen composition into a different one whenever some
/// other composition is named with those digits — silently, since planning
/// would succeed. So a name that only one composition answers to is used as
/// it is, and the id is the fallback for a shared name.
///
/// The fallback is still wrong for one document: a name two compositions
/// share *and* a third composition named with the chosen one's id digits.
/// Nothing spellable resolves to the right composition there — that document
/// is what `CliError::AmbiguousComposition` exists for — and the user can
/// rename either one and start again.
fn ask_composition(prompt: &mut dyn Prompt, document: &Document) -> Result<String, CliError> {
    let entries = listing::compositions(document);
    if entries.is_empty() {
        return Err(CliError::NoComposition);
    }
    let options: Vec<String> = entries
        .iter()
        .map(|entry| {
            let label = t!("cli.prompt.comp_option")
                .replace("{name}", &entry.name)
                .replace("{width}", &entry.width.to_string())
                .replace("{height}", &entry.height.to_string())
                .replace("{frames}", &entry.duration_frames.to_string());
            match entry.root {
                true => format!("{label} {}", t!("cli.prompt.comp_root")),
                false => label,
            }
        })
        .collect();
    let default = entries.iter().position(|entry| entry.root).unwrap_or(0);
    let chosen = &entries[prompt.select(&t!("cli.prompt.comp"), &options, default)?];
    let unique = entries
        .iter()
        .filter(|entry| entry.name == chosen.name)
        .count()
        == 1;
    match unique {
        true => Ok(chosen.name.clone()),
        false => Ok(chosen.id.to_string()),
    }
}

/// Which output format, out of the ones that can actually be written.
///
/// Unavailable rows are not offered at all. `ravel-cli list codecs` still
/// lists them with their reason — that is the place to learn why this
/// machine has no ProRes — but a menu entry that can only be refused is a
/// question with a wrong answer in it.
fn ask_format(
    prompt: &mut dyn Prompt,
    encoders: &[EncoderAvailability],
) -> Result<OutputFormat, CliError> {
    let usable = usable_formats(encoders);
    let (formats, options): (Vec<OutputFormat>, Vec<String>) = usable.into_iter().unzip();
    match formats.first() {
        // Image sequences are writable in every build, so this is a filtered
        // encoder list rather than a reachable environment.
        None => Err(CliError::CodecNoWriter { format: "png" }),
        Some(_) => {
            let chosen = prompt.select(&t!("cli.prompt.format"), &options, 0)?;
            Ok(formats[chosen])
        }
    }
}

/// The formats this build on this machine can write, with their labels.
///
/// Built from [`listing::codecs`], the same enumeration `list codecs`
/// prints: `available` is the machine's verdict and `writable` is Ravel's
/// (a video codec this machine can encode still has no container writer,
/// `EXPORT-4`), and an entry has to pass both to be offered.
pub fn usable_formats(encoders: &[EncoderAvailability]) -> Vec<(OutputFormat, String)> {
    listing::codecs(encoders)
        .into_iter()
        .filter(|entry| entry.available && entry.writable)
        .filter_map(|entry| {
            let format = OutputFormat::ALL
                .iter()
                .copied()
                .find(|format| format.id() == entry.format)?;
            let label = t!("cli.prompt.format_option")
                .replace("{format}", entry.format)
                .replace("{route}", entry.route.as_deref().unwrap_or_default());
            Some((format, label))
        })
        .collect()
}

fn ask_png_depth(prompt: &mut dyn Prompt) -> Result<PngBits, CliError> {
    let options = ["8".to_string(), "16".to_string()];
    match prompt.select(&t!("cli.prompt.png_depth"), &options, 0)? {
        0 => Ok(PngBits::Eight),
        _ => Ok(PngBits::Sixteen),
    }
}

fn ask_output(prompt: &mut dyn Prompt) -> Result<std::path::PathBuf, CliError> {
    loop {
        let answer = prompt.text(&t!("cli.prompt.output"), "")?;
        if answer.trim().is_empty() {
            prompt.note(&t!("cli.prompt.output_required"));
            continue;
        }
        return Ok(std::path::PathBuf::from(answer.trim()));
    }
}

/// What to do about output files that are already there.
enum Conflicts {
    None,
    Replace,
    Elsewhere,
}

/// Ask before a render that would land on existing files.
///
/// The question is asked here rather than left to
/// [`crate::render_with_hooks`]'s refusal, because a session that answered
/// every question and then died with "those files exist" would have wasted
/// the whole dialogue on a fact known before the parameters were asked.
/// Saying yes is `--overwrite`; the refusal itself stays exactly where it
/// was for the non-interactive path.
fn resolve_conflicts(prompt: &mut dyn Prompt, plan: &RenderPlan) -> Result<Conflicts, CliError> {
    let conflicts = plan.conflicts();
    let Some(first) = conflicts.first() else {
        return Ok(Conflicts::None);
    };
    let question = t!("cli.prompt.overwrite")
        .replace("{count}", &conflicts.len().to_string())
        .replace("{first}", &first.display().to_string());
    match prompt.confirm(&question, false)? {
        true => Ok(Conflicts::Replace),
        false => Ok(Conflicts::Elsewhere),
    }
}

/// Offer every declared parameter, keeping only the answers that change one.
///
/// The declarations come from [`ExposedListing::of`] — `ravel-cli list
/// params` prints the same thing — and each answer is validated by planning
/// the render with it, which is how an undeclared name or a value of the
/// wrong type is refused on the spot rather than at submission.
///
/// An answer equal to the declared default records nothing: it renders
/// identically, and the command line stays as short as what the user
/// actually changed.
fn ask_parameters(
    prompt: &mut dyn Prompt,
    args: &mut RenderArgs,
    document: &Document,
    project_root: Option<&Path>,
    encoders: &[EncoderAvailability],
) -> Result<(), CliError> {
    let listing = ExposedListing::of(document);
    for entry in &listing.parameters {
        let default = default_text(&entry.default);
        let question = parameter_question(entry);
        loop {
            let answer = prompt.text(&question, &default)?;
            if answer.is_empty() || answer == default {
                break;
            }
            args.params.push(format!("{}={answer}", entry.name));
            match plan_render(args, document, project_root, encoders) {
                Ok(_) => break,
                Err(error) => {
                    args.params.pop();
                    prompt.note(&error.localized());
                }
            }
        }
    }
    Ok(())
}

fn parameter_question(entry: &ExposedListingEntry) -> String {
    let mut question = t!("cli.prompt.param")
        .replace("{name}", &entry.name)
        .replace("{type}", &entry.value_type.to_string());
    if !entry.description.is_empty() {
        question.push_str(" — ");
        question.push_str(&entry.description);
    }
    if !entry.resolved {
        question.push(' ');
        question.push_str(&t!("cli.prompt.param_unresolved"));
    }
    question
}

/// A declared default as the text `--param` takes for it.
///
/// The spelling is [`crate::params`]' input syntax, not the listing's JSON:
/// what is offered as the value to edit has to be something the parser would
/// accept back. `a_default_is_valid_input_for_its_own_type` pins that.
pub fn default_text(value: &ExposedValue) -> String {
    fn floats(components: &[f32]) -> String {
        components
            .iter()
            .map(f32::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }
    match value {
        ExposedValue::Float(v) => v.to_string(),
        ExposedValue::Int(v) => v.to_string(),
        ExposedValue::Bool(v) => v.to_string(),
        ExposedValue::String(v) => v.clone(),
        ExposedValue::Vec2(v) => floats(&[v.0, v.1]),
        ExposedValue::Vec3(v) => floats(&[v.0, v.1, v.2]),
        ExposedValue::Vec4(v) => floats(&[v.0, v.1, v.2, v.3]),
        ExposedValue::Color(v) => floats(&[v.r, v.g, v.b, v.a]),
        ExposedValue::Media(path) => path.to_string(),
    }
}

// ===========================================================================
// The command the answers add up to
// ===========================================================================

/// The `ravel-cli render …` command line these arguments were collected as.
///
/// Shown before the render so a session teaches the flags it replaces, and —
/// more usefully for this crate — parsed back by
/// `the_answers_are_a_command_line` to prove the interactive mode adds
/// nothing the non-interactive one cannot express.
///
/// Arguments left at their defaults are omitted, which is why the three file
/// name pieces have local constants.
pub fn equivalent_argv(args: &RenderArgs) -> Vec<String> {
    let mut argv = vec!["ravel-cli".to_string(), "render".to_string()];
    argv.push(args.project.display().to_string());
    if let Some(comp) = &args.comp {
        argv.push("--comp".to_string());
        argv.push(comp.clone());
    }
    if let Some(range) = args.range {
        argv.push("--range".to_string());
        argv.push(format!("{}-{}", range.first, range.last));
    }
    argv.push("--format".to_string());
    argv.push(args.format.id().to_string());
    if args.format == OutputFormat::Png && args.png_depth == PngBits::Sixteen {
        argv.push("--png-depth".to_string());
        argv.push("16".to_string());
    }
    argv.push("--output".to_string());
    argv.push(args.output.display().to_string());
    if args.prefix != DEFAULT_PREFIX {
        argv.push("--prefix".to_string());
        argv.push(args.prefix.clone());
    }
    if args.suffix != DEFAULT_SUFFIX {
        argv.push("--suffix".to_string());
        argv.push(args.suffix.clone());
    }
    if args.padding != DEFAULT_PADDING {
        argv.push("--padding".to_string());
        argv.push(args.padding.to_string());
    }
    for param in &args.params {
        argv.push("--param".to_string());
        argv.push(param.clone());
    }
    if args.overwrite {
        argv.push("--overwrite".to_string());
    }
    if args.no_audio {
        argv.push("--no-audio".to_string());
    }
    if args.progress != ProgressMode::Auto {
        argv.push("--progress".to_string());
        argv.push(
            match args.progress {
                ProgressMode::Bar => "bar",
                ProgressMode::Json => "json",
                ProgressMode::Quiet => "quiet",
                ProgressMode::Auto => unreachable!("excluded above"),
            }
            .to_string(),
        );
    }
    argv
}

/// The same command as one line a shell would take back.
///
/// Quoting is deliberately crude — anything outside a conservative set of
/// characters goes inside single quotes — because this line is *shown*, not
/// executed, and a quote too many costs nothing while a quote too few
/// teaches a command that breaks on the first path with a space in it.
pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|token| {
            let safe = !token.is_empty()
                && token
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._/=:,+-".contains(c));
            match safe {
                true => token.clone(),
                false => format!("'{}'", token.replace('\'', r"'\''")),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ===========================================================================
// The terminal
// ===========================================================================

/// The prompts as a person sees them: `dialoguer`, on **stderr**.
///
/// stderr because stdout is where the machine-readable output goes, and a
/// question drawn into it would be parsed as a result. `dialoguer` draws on
/// stderr by default; nothing here overrides that.
#[derive(Default)]
pub struct TerminalPrompt {
    theme: ColorfulTheme,
}

/// Ctrl-C at a prompt is the same answer as Ctrl-C during a render: stop,
/// and leave nothing behind. Anything else the terminal reports is a genuine
/// I/O failure and is not silently read as a cancellation.
fn prompt_error(error: dialoguer::Error) -> CliError {
    match error {
        dialoguer::Error::IO(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            CliError::Cancelled
        }
        dialoguer::Error::IO(error) => CliError::Internal(error.to_string()),
    }
}

impl Prompt for TerminalPrompt {
    fn select(
        &mut self,
        question: &str,
        options: &[String],
        default: usize,
    ) -> Result<usize, CliError> {
        Select::with_theme(&self.theme)
            .with_prompt(question)
            .items(options)
            .default(default)
            .interact()
            .map_err(prompt_error)
    }

    fn text(&mut self, question: &str, default: &str) -> Result<String, CliError> {
        let mut input = Input::<String>::with_theme(&self.theme);
        input = input.with_prompt(question).allow_empty(true);
        if !default.is_empty() {
            input = input.default(default.to_string());
        }
        input.interact_text().map_err(prompt_error)
    }

    fn confirm(&mut self, question: &str, default: bool) -> Result<bool, CliError> {
        Confirm::with_theme(&self.theme)
            .with_prompt(question)
            .default(default)
            .interact()
            .map_err(prompt_error)
    }

    fn note(&mut self, message: &str) {
        eprintln!("{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{Cli, Command};
    use clap::Parser;
    use ravel_core::composition::{Composition, Layer};
    use ravel_core::exposed::{ExposedBinding, ExposedParameter, ExposedParameters};
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::{CompId, LayerId, NodeId};
    use ravel_core::media::encode::{
        Availability, EncodeRoute, EncodeTarget, EncoderAvailability, UnavailableReason,
    };
    use ravel_core::media::{ImageFormat, VideoCodec};
    use ravel_core::types::FrameRate;
    use ravel_media::encode::available_encoders;

    /// An answer, in the order the session asks for one.
    #[derive(Clone, Debug)]
    enum Answer {
        Select(usize),
        Text(String),
        /// Accept whatever the prompt offered as the default.
        Keep,
        Confirm(bool),
    }

    /// A typed line, for a script that is written as literals.
    fn text(answer: &str) -> Answer {
        Answer::Text(answer.to_string())
    }

    /// A session driven from a list instead of a terminal.
    struct Scripted {
        answers: std::collections::VecDeque<Answer>,
        /// Every refusal the session showed, in order.
        notes: Vec<String>,
        /// Every question asked, for the tests that care about the menu.
        options: Vec<Vec<String>>,
    }

    impl Scripted {
        fn new(answers: impl IntoIterator<Item = Answer>) -> Self {
            Self {
                answers: answers.into_iter().collect(),
                notes: Vec::new(),
                options: Vec::new(),
            }
        }

        fn next(&mut self) -> Result<Answer, CliError> {
            self.answers
                .pop_front()
                // The session asked one question more than the script has an
                // answer for. Reported as a cancellation because that is what
                // a person out of patience does, and because it stops the
                // retry loop instead of hanging the test.
                .ok_or(CliError::Cancelled)
        }
    }

    impl Prompt for Scripted {
        fn select(
            &mut self,
            _question: &str,
            options: &[String],
            _default: usize,
        ) -> Result<usize, CliError> {
            self.options.push(options.to_vec());
            match self.next()? {
                Answer::Select(index) => Ok(index),
                other => panic!("a menu was answered with {other:?}"),
            }
        }

        fn text(&mut self, _question: &str, default: &str) -> Result<String, CliError> {
            match self.next()? {
                Answer::Text(text) => Ok(text),
                Answer::Keep => Ok(default.to_string()),
                other => panic!("a text prompt was answered with {other:?}"),
            }
        }

        fn confirm(&mut self, _question: &str, _default: bool) -> Result<bool, CliError> {
            match self.next()? {
                Answer::Confirm(yes) => Ok(yes),
                other => panic!("a confirmation was answered with {other:?}"),
            }
        }

        fn note(&mut self, message: &str) {
            self.notes.push(message.to_string());
        }
    }

    fn document() -> Document {
        Document::default()
            .with_composition(Composition::new(
                CompId::new(1),
                "Main",
                (16, 16),
                FrameRate::new(30, 1),
                50,
            ))
            .with_composition(Composition::new(
                CompId::new(2),
                "Insert",
                (8, 8),
                FrameRate::new(30, 1),
                10,
            ))
    }

    /// A document whose composition declares one float parameter that
    /// actually reaches a node, so `apply` resolves it and a supplied value
    /// lands in the planned document.
    fn document_with_parameter() -> Document {
        let graph = Graph::new()
            .add_node(
                Node::new(NodeId::new(1), "test.frame")
                    .with_param("scale", ParameterValue::Float(1.0)),
            )
            .expect("one node");
        let composition =
            Composition::new(CompId::new(1), "Main", (16, 16), FrameRate::new(30, 1), 50)
                .add_layer(Layer::new(LayerId::new(1), "picture", graph));
        Document::default()
            .with_composition(composition)
            .with_exposed_parameters(
                ExposedParameters::from_declarations([ExposedParameter::inferred(
                    "scale",
                    ExposedValue::Float(1.0),
                    ExposedBinding::new(NodeId::new(1), "scale"),
                )
                .expect("declaration")])
                .expect("unique names"),
            )
    }

    fn collect_with(
        prompt: &mut Scripted,
        document: &Document,
        encoders: &[EncoderAvailability],
    ) -> Result<RenderArgs, CliError> {
        collect(
            prompt,
            Path::new("project.ravprj"),
            document,
            None,
            encoders,
        )
    }

    // -------------------------------------------------------------------
    // The claim: an interactive session is a command line
    // -------------------------------------------------------------------

    /// The unit's whole point. The answers are collected, written out as the
    /// flags they stand for, parsed back by `clap`, and both plans compared:
    /// if the interactive mode could reach a render the non-interactive one
    /// cannot express, this is where it shows.
    #[test]
    fn the_answers_are_a_command_line() {
        let document = document();
        let encoders = available_encoders();
        let mut prompt = Scripted::new([
            Answer::Select(1),     // the composition "Insert"
            Answer::Select(0),     // PNG
            Answer::Select(1),     // 16 bits
            text("/tmp/out-7"),    // the output directory
            Answer::Confirm(true), // render it
        ]);
        let interactive = collect_with(&mut prompt, &document, &encoders).expect("a session");

        // The same render, written by hand as a caller would type it.
        let typed = RenderArgs {
            project: "project.ravprj".into(),
            comp: Some("Insert".into()),
            range: None,
            format: OutputFormat::Png,
            png_depth: PngBits::Sixteen,
            output: "/tmp/out-7".into(),
            prefix: "frame_".into(),
            suffix: String::new(),
            padding: 4,
            params: Vec::new(),
            overwrite: false,
            no_audio: false,
            progress: ProgressMode::Bar,
        };

        let from_answers =
            plan_render(&interactive, &document, None, &encoders).expect("the session plans");
        let from_flags = plan_render(&typed, &document, None, &encoders).expect("the flags plan");
        assert_same_plan(&from_answers, &from_flags);

        // And the command it prints is the command it means: parsed back, it
        // is the same arguments, so a user can rerun the session's render
        // without a session.
        let argv = equivalent_argv(&interactive);
        let parsed = match Cli::try_parse_from(&argv).expect("clap accepts it").command {
            Command::Render(args) => *args,
            other => panic!("not a render command: {other:?}"),
        };
        assert_eq!(parsed, interactive, "argv: {argv:?}");
    }

    /// Everything a job is decided by. `RenderPlan` holds an `Arc<Document>`
    /// and a warning list rather than deriving equality, so the comparison is
    /// spelled out here — and includes the document, because that is where
    /// `--param` lands.
    fn assert_same_plan(left: &RenderPlan, right: &RenderPlan) {
        assert_eq!(left.comp, right.comp);
        assert_eq!(left.comp_name, right.comp_name);
        assert_eq!(left.range, right.range);
        assert_eq!(left.codec, right.codec);
        assert_eq!(left.overwrite, right.overwrite);
        assert_eq!(left.warnings, right.warnings);
        assert_eq!(left.document, right.document);
        assert_eq!(
            left.output.frame_path(left.range.start),
            right.output.frame_path(right.range.start)
        );
        assert_eq!(
            left.output.frame_path(left.range.end - 1),
            right.output.frame_path(right.range.end - 1)
        );
        assert_eq!(
            left.audio.as_ref().map(|audio| audio.path.clone()),
            right.audio.as_ref().map(|audio| audio.path.clone())
        );
    }

    /// A parameter answered interactively has to reach the document the same
    /// way `--param` does, values and all.
    #[test]
    fn a_parameter_answered_in_the_session_is_the_same_as_the_flag() {
        let document = document_with_parameter();
        let encoders = available_encoders();
        let mut prompt = Scripted::new([
            Answer::Select(0),
            Answer::Select(0),
            Answer::Select(0),
            text("/tmp/out-7"),
            text("2"), // scale
            Answer::Confirm(true),
        ]);
        let interactive = collect_with(&mut prompt, &document, &encoders).expect("a session");
        assert_eq!(interactive.params, vec!["scale=2".to_string()]);

        let mut typed = interactive.clone();
        typed.params = vec!["scale=2".to_string()];
        assert_same_plan(
            &plan_render(&interactive, &document, None, &encoders).expect("plans"),
            &plan_render(&typed, &document, None, &encoders).expect("plans"),
        );
    }

    /// Accepting the offered default records nothing: the render is the same
    /// one, and the command line stays as short as what was actually changed.
    #[test]
    fn accepting_a_declared_default_adds_no_argument() {
        let document = document_with_parameter();
        let mut prompt = Scripted::new([
            Answer::Select(0),
            Answer::Select(0),
            Answer::Select(0),
            text("/tmp/out-7"),
            Answer::Keep,
            Answer::Confirm(true),
        ]);
        let args = collect_with(&mut prompt, &document, &available_encoders()).expect("a session");
        assert!(args.params.is_empty(), "{:?}", args.params);
    }

    /// The composition that was pointed at is the composition that renders,
    /// including in the document where a name and an id disagree: `--comp`
    /// reads names before ids, so a composition *named* "1" owns that
    /// spelling and the composition whose *id* is 1 has to be named instead.
    #[test]
    fn the_chosen_composition_is_the_one_that_renders() {
        let document = Document::default()
            .with_composition(Composition::new(
                CompId::new(1),
                "Main",
                (16, 16),
                FrameRate::new(30, 1),
                50,
            ))
            .with_composition(Composition::new(
                CompId::new(7),
                // A name that is another composition's id.
                "1",
                (8, 8),
                FrameRate::new(30, 1),
                10,
            ));
        let encoders = available_encoders();

        for (index, expected) in [(0usize, CompId::new(1)), (1, CompId::new(7))] {
            let mut prompt = Scripted::new([
                Answer::Select(index),
                Answer::Select(0),
                Answer::Select(0),
                text("/tmp/out-7"),
                Answer::Confirm(true),
            ]);
            let args = collect_with(&mut prompt, &document, &encoders).expect("a session");
            assert_eq!(
                plan_render(&args, &document, None, &encoders)
                    .expect("plans")
                    .comp,
                expected,
                "chose entry {index}, and --comp said {:?}",
                args.comp
            );
        }
    }

    /// A name two compositions share cannot be handed to `--comp` — it is
    /// what `AmbiguousComposition` refuses — so the id is used instead.
    #[test]
    fn a_shared_name_falls_back_to_the_id() {
        let document = Document::default()
            .with_composition(Composition::new(
                CompId::new(1),
                "Main",
                (16, 16),
                FrameRate::new(30, 1),
                50,
            ))
            .with_composition(Composition::new(
                CompId::new(4),
                "Main",
                (8, 8),
                FrameRate::new(30, 1),
                10,
            ));
        let encoders = available_encoders();
        let mut prompt = Scripted::new([
            Answer::Select(1),
            Answer::Select(0),
            Answer::Select(0),
            text("/tmp/out-7"),
            Answer::Confirm(true),
        ]);
        let args = collect_with(&mut prompt, &document, &encoders).expect("a session");
        assert_eq!(args.comp.as_deref(), Some("4"));
        assert_eq!(
            plan_render(&args, &document, None, &encoders)
                .expect("an id is never ambiguous")
                .comp,
            CompId::new(4)
        );
    }

    // -------------------------------------------------------------------
    // Refusals
    // -------------------------------------------------------------------

    /// The gate is the reason a prompt can never be written into a pipe. Both
    /// branches are reachable without a terminal because the answer is passed
    /// in, which is also how `report`'s mode resolution is tested.
    #[test]
    fn a_session_without_a_terminal_on_stdin_is_refused_with_a_reason() {
        assert!(gate(true).is_ok());
        let error = gate(false).expect_err("nobody can answer");
        assert!(matches!(error, CliError::NotInteractive));
        assert_eq!(error.code(), crate::error::EXIT_USAGE);
        assert_eq!(error.id(), "not-interactive");
        assert!(
            !error.localized().is_empty(),
            "the refusal says why, it does not just fail"
        );
    }

    /// A codec this build or this machine cannot write is not in the menu at
    /// all, so it cannot be chosen and then refused.
    #[test]
    fn unavailable_codecs_are_not_offered() {
        let encoders = vec![
            EncoderAvailability {
                target: EncodeTarget::ImageSequence(ImageFormat::Png),
                availability: Availability::Available(EncodeRoute::Native),
            },
            EncoderAvailability {
                target: EncodeTarget::Video(VideoCodec::Vp9),
                availability: Availability::Unavailable(UnavailableReason::FfmpegNotLinked),
            },
            // Available on this machine, but Ravel has no container writer
            // yet — equally unchoosable, for a different reason.
            EncoderAvailability {
                target: EncodeTarget::Video(VideoCodec::ProRes),
                availability: Availability::Available(EncodeRoute::FfmpegSoftware {
                    encoder: "prores_ks",
                }),
            },
        ];
        let offered = usable_formats(&encoders);
        assert_eq!(
            offered
                .iter()
                .map(|(format, _)| *format)
                .collect::<Vec<_>>(),
            vec![OutputFormat::Png]
        );

        // And the menu the session shows carries exactly those entries.
        let mut prompt = Scripted::new([
            Answer::Select(0),
            Answer::Select(0),
            Answer::Select(0),
            text("/tmp/out-7"),
            Answer::Confirm(true),
        ]);
        collect_with(&mut prompt, &document(), &encoders).expect("a session");
        // One entry, the one `usable_formats` allowed. The label itself is a
        // locale key here — these tests load no catalogs — so what is checked
        // is the shape of the menu, not its words.
        let format_menu = &prompt.options[1];
        assert_eq!(format_menu.len(), 1, "{format_menu:?}");
    }

    /// A value of the wrong shape is refused where it is typed — by the same
    /// planning the command line goes through — and the question comes back
    /// rather than the session dying.
    #[test]
    fn a_value_of_the_wrong_type_is_refused_on_the_spot() {
        let document = document_with_parameter();
        let mut prompt = Scripted::new([
            Answer::Select(0),
            Answer::Select(0),
            Answer::Select(0),
            text("/tmp/out-7"),
            text("wide"), // not a float
            text("2"),    // and this is
            Answer::Confirm(true),
        ]);
        let args = collect_with(&mut prompt, &document, &available_encoders()).expect("a session");
        assert_eq!(args.params, vec!["scale=2".to_string()]);
        assert_eq!(prompt.notes.len(), 1, "{:?}", prompt.notes);
        assert!(!prompt.notes[0].is_empty(), "the refusal was shown");
    }

    /// The session validates through `plan_render`, so a name the project
    /// does not declare is refused by `ravel-core`'s own contract rather than
    /// by a second opinion kept here. The menu offers only declared names, so
    /// this asks the validator directly.
    #[test]
    fn an_undeclared_parameter_name_is_refused_by_the_same_planning() {
        let document = document_with_parameter();
        let mut args = RenderArgs {
            project: "project.ravprj".into(),
            comp: Some("1".into()),
            range: None,
            format: OutputFormat::Png,
            png_depth: PngBits::Eight,
            output: "/tmp/out-7".into(),
            prefix: DEFAULT_PREFIX.into(),
            suffix: DEFAULT_SUFFIX.into(),
            padding: DEFAULT_PADDING,
            params: vec!["nosuch=1".to_string()],
            overwrite: false,
            no_audio: false,
            progress: PROGRESS,
        };
        // `RenderPlan` carries a document rather than deriving `Debug`, so the
        // refusal is matched instead of unwrapped.
        let error = match plan_render(&args, &document, None, &available_encoders()) {
            Err(error) => error,
            Ok(_) => panic!("the project declares no `nosuch`, yet it planned"),
        };
        assert_eq!(error.code(), crate::error::EXIT_PARAM);
        assert_eq!(error.id(), "param-rejected");

        args.params = vec!["scale=1.5".to_string()];
        assert!(plan_render(&args, &document, None, &available_encoders()).is_ok());
    }

    /// An output name `ravel-core` refuses is caught while the directory is
    /// being asked for, not after every remaining question.
    #[test]
    fn an_unusable_output_directory_is_asked_again() {
        let mut prompt = Scripted::new([
            Answer::Select(0),
            Answer::Select(0),
            Answer::Select(0),
            text(""), // nothing at all
            text("/tmp/out-7"),
            Answer::Confirm(true),
        ]);
        let args =
            collect_with(&mut prompt, &document(), &available_encoders()).expect("a session");
        assert_eq!(args.output, Path::new("/tmp/out-7"));
        assert_eq!(prompt.notes.len(), 1, "{:?}", prompt.notes);
    }

    /// Declining the last question is a cancellation, and nothing is
    /// rendered. The same class as a Ctrl-C, so a script driving `ravel-cli`
    /// sees one exit code for "the user stopped it".
    #[test]
    fn declining_the_confirmation_cancels() {
        let mut prompt = Scripted::new([
            Answer::Select(0),
            Answer::Select(0),
            Answer::Select(0),
            text("/tmp/out-7"),
            Answer::Confirm(false),
        ]);
        let error = collect_with(&mut prompt, &document(), &available_encoders())
            .expect_err("the answer was no");
        assert!(matches!(error, CliError::Cancelled));
        assert_eq!(error.code(), crate::error::EXIT_CANCELLED);
    }

    /// Files already in the way are raised while the directory is being
    /// chosen, and saying yes is exactly `--overwrite`.
    #[test]
    fn existing_output_is_raised_before_the_render_and_becomes_overwrite() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("frame_0000.png"), b"old").expect("a file in the way");
        let path = dir.path().display().to_string();
        let path: &'static str = Box::leak(path.into_boxed_str());

        let mut replace = Scripted::new([
            Answer::Select(0),
            Answer::Select(0),
            Answer::Select(0),
            text(path),
            Answer::Confirm(true), // replace them
            Answer::Confirm(true), // render
        ]);
        let args =
            collect_with(&mut replace, &document(), &available_encoders()).expect("a session");
        assert!(args.overwrite, "answering yes is --overwrite");

        // Answering no asks for somewhere else, and the render that follows
        // does not carry `--overwrite`.
        let mut elsewhere = Scripted::new([
            Answer::Select(0),
            Answer::Select(0),
            Answer::Select(0),
            text(path),
            Answer::Confirm(false), // keep them
            text("/tmp/out-7"),     // somewhere else
            Answer::Confirm(true),
        ]);
        let args =
            collect_with(&mut elsewhere, &document(), &available_encoders()).expect("a session");
        assert!(!args.overwrite);
        assert_eq!(args.output, Path::new("/tmp/out-7"));
    }

    // -------------------------------------------------------------------
    // Progress
    // -------------------------------------------------------------------

    /// A session renders with a bar and can never select the machine-readable
    /// mode, which is what keeps the two exclusive: no bar is drawn across a
    /// stream something is parsing, and the JSON `ravel-cli render` produces
    /// is the same bytes whether or not a bar was drawn elsewhere.
    #[test]
    fn an_interactive_render_never_produces_machine_readable_output() {
        assert_eq!(PROGRESS, ProgressMode::Bar);
        let mut prompt = Scripted::new([
            Answer::Select(0),
            Answer::Select(0),
            Answer::Select(0),
            text("/tmp/out-7"),
            Answer::Confirm(true),
        ]);
        let args =
            collect_with(&mut prompt, &document(), &available_encoders()).expect("a session");
        assert_eq!(args.progress, ProgressMode::Bar);
        assert_ne!(args.progress, ProgressMode::Json);
    }

    // -------------------------------------------------------------------
    // The pieces
    // -------------------------------------------------------------------

    /// The defaults this module leaves out of the command it writes have to
    /// be the defaults `clap` fills in, or the printed command renders
    /// something else.
    #[test]
    fn clap_defaults_are_unchanged() {
        let parsed = match Cli::try_parse_from([
            "ravel-cli",
            "render",
            "project.ravprj",
            "--output",
            "/tmp/out",
        ])
        .expect("a minimal command line")
        .command
        {
            Command::Render(args) => *args,
            other => panic!("not a render command: {other:?}"),
        };
        assert_eq!(parsed.prefix, DEFAULT_PREFIX);
        assert_eq!(parsed.suffix, DEFAULT_SUFFIX);
        assert_eq!(parsed.padding, DEFAULT_PADDING);
    }

    /// The text offered as a parameter's current value has to be text the
    /// parser accepts for that type — otherwise pressing Enter on the offered
    /// default would be refused.
    #[test]
    fn a_default_is_valid_input_for_its_own_type() {
        use ravel_core::composition::AssetPath;
        use ravel_core::exposed::ExposedType;
        use ravel_core::exposed::listing::ExposedListingEntry;
        use ravel_core::types::{Color, Vec2, Vec3, Vec4};

        let values = [
            ExposedValue::Float(1.5),
            ExposedValue::Int(-3),
            ExposedValue::Bool(true),
            ExposedValue::String("a caption".into()),
            ExposedValue::Vec2(Vec2(1.0, 2.0)),
            ExposedValue::Vec3(Vec3(1.0, 2.0, 3.0)),
            ExposedValue::Vec4(Vec4(1.0, 2.0, 3.0, 4.0)),
            ExposedValue::Color(Color::new(0.25, 0.5, 0.75, 1.0)),
            ExposedValue::Media(AssetPath::parse("./clip.mov")),
        ];
        for value in values {
            let value_type = match &value {
                ExposedValue::Float(_) => ExposedType::Float,
                ExposedValue::Int(_) => ExposedType::Int,
                ExposedValue::Bool(_) => ExposedType::Bool,
                ExposedValue::String(_) => ExposedType::String,
                ExposedValue::Vec2(_) => ExposedType::Vec2,
                ExposedValue::Vec3(_) => ExposedType::Vec3,
                ExposedValue::Vec4(_) => ExposedType::Vec4,
                ExposedValue::Color(_) => ExposedType::Color,
                ExposedValue::Media(_) => ExposedType::Media,
            };
            let listing = ExposedListing {
                parameters: vec![ExposedListingEntry {
                    name: "p".into(),
                    value_type,
                    default: value.clone(),
                    description: String::new(),
                    resolved: true,
                }],
            };
            let text = default_text(&value);
            let parsed = crate::params::parse(&[format!("p={text}")], &listing)
                .unwrap_or_else(|error| panic!("{value:?} offered as {text:?}: {error}"));
            assert_eq!(parsed["p"], value, "offered as {text:?}");
        }
    }

    /// The command is shown to be retyped, so a path with a space in it has
    /// to come back quoted.
    #[test]
    fn the_shown_command_quotes_what_a_shell_would_split() {
        let joined = shell_join(&[
            "ravel-cli".to_string(),
            "--output".to_string(),
            "/tmp/my renders".to_string(),
            "--param".to_string(),
            "caption=hello world".to_string(),
        ]);
        assert_eq!(
            joined,
            "ravel-cli --output '/tmp/my renders' --param 'caption=hello world'"
        );
    }
}
