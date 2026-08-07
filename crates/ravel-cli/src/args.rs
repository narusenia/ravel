// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The command line: what `ravel-cli` accepts, and nothing about what it
//! then does.
//!
//! Parsing stops at "these are the strings the user typed, in typed form".
//! Turning them into something renderable is [`crate::plan`]'s job, which is
//! what lets the interactive mode (`EXPORT-7`) build the same
//! [`RenderArgs`] from answers to questions instead of from `argv`.

use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand, ValueEnum};
use ravel_core::media::encode::{EncodeTarget, PngDepth, SequenceCodec};
use ravel_core::media::{ImageFormat, VideoCodec};

use crate::error::CliError;

/// Headless rendering for Ravel projects.
#[derive(Debug, Parser)]
#[command(name = "ravel-cli", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Render a composition to disk.
    Render(Box<RenderArgs>),
    /// Print what a project or this machine offers, as JSON.
    #[command(subcommand)]
    List(ListCommand),
    /// Ask for the render options and then render, for a terminal.
    ///
    /// A separate subcommand rather than a `render --interactive` flag: the
    /// answers *are* a `render` command line, so the two cannot share a set
    /// of arguments without making `--output` optional — that is, without
    /// weakening the non-interactive contract for the sake of the
    /// interactive one.
    Interactive(ProjectArg),
}

#[derive(Debug, Subcommand)]
pub enum ListCommand {
    /// The project's compositions.
    Comps(ProjectArg),
    /// The project's exposed parameter declarations.
    Params(ProjectArg),
    /// The render outputs this build on this machine can write.
    Codecs,
}

/// Expands a leading `~` (bare, or followed by a separator) to the home
/// directory.
///
/// A shell does this before `argv` exists, so it only matters when the tilde
/// reaches the process intact — which happens two ways, both of them ordinary:
/// a quoted argument (`-o '~/renders'`, and every `~` typed into a Windows
/// prompt), and the paths the interactive mode reads from its own prompts,
/// which no shell ever sees.
///
/// `~user` is left alone: resolving another account's home needs a directory
/// lookup this has no business doing, and passing it through unchanged fails
/// with the path the user typed instead of one silently rewritten to the wrong
/// account. A home directory that cannot be determined leaves the path
/// untouched for the same reason.
pub fn expand_tilde(text: &str) -> PathBuf {
    let Some(rest) = text.strip_prefix('~') else {
        return PathBuf::from(text);
    };
    let rest = match rest {
        "" => "",
        // Both separators, because a Windows user types either one.
        _ if rest.starts_with('/') || rest.starts_with('\\') => &rest[1..],
        // `~user/...`, or a file whose name genuinely starts with a tilde.
        _ => return PathBuf::from(text),
    };
    match dirs::home_dir() {
        Some(home) if rest.is_empty() => home,
        Some(home) => home.join(rest),
        None => PathBuf::from(text),
    }
}

/// `expand_tilde` in the shape clap's `value_parser` wants. Infallible: an
/// unexpandable tilde is a path, not a usage error.
fn tilde_path(text: &str) -> Result<PathBuf, std::convert::Infallible> {
    Ok(expand_tilde(text))
}

#[derive(Debug, Args)]
pub struct ProjectArg {
    /// The `.ravprj` to read. Never written.
    #[arg(value_parser = tilde_path)]
    pub project: PathBuf,
}

/// Everything a render needs, before a project has been looked at.
///
/// `PartialEq` because the interactive mode's claim is an equality: the
/// answers it collects are the same arguments a caller would have typed, and
/// a test says so by comparing the two.
#[derive(Clone, Debug, Args, PartialEq, Eq)]
pub struct RenderArgs {
    /// The `.ravprj` to render. Never written — a project that migrates on
    /// load is migrated in memory only.
    #[arg(value_parser = tilde_path)]
    pub project: PathBuf,

    /// Composition to render, by name or by numeric id. Defaults to the
    /// project's root composition.
    #[arg(long, value_name = "NAME_OR_ID")]
    pub comp: Option<String>,

    /// Absolute frame range, **inclusive at both ends**: `100-199` renders
    /// 100 frames named 0100 to 0199. A bare `42` renders one frame.
    /// Defaults to the whole composition.
    #[arg(long, value_name = "START-END")]
    pub range: Option<FrameRange>,

    /// Output format. Anything but an image sequence depends on this build
    /// and this machine; `ravel-cli list codecs` says which.
    #[arg(long, value_enum, default_value_t = OutputFormat::Png)]
    pub format: OutputFormat,

    /// Bits per channel for PNG output.
    #[arg(long, value_enum, default_value_t = PngBits::Eight)]
    pub png_depth: PngBits,

    /// Directory the numbered frames are written into. Created if missing.
    #[arg(long, short = 'o', value_name = "DIR", value_parser = tilde_path)]
    pub output: PathBuf,

    /// File name text before the frame number.
    #[arg(long, default_value = "frame_")]
    pub prefix: String,

    /// File name text between the frame number and the extension.
    #[arg(long, default_value = "")]
    pub suffix: String,

    /// Minimum digits in the frame number, zero-padded.
    #[arg(long, default_value_t = 4)]
    pub padding: usize,

    /// Set an exposed parameter: `--param NAME=VALUE`, repeatable. Vectors
    /// and colours take comma-separated components (`--param tint=1,0,0,1`).
    #[arg(long = "param", value_name = "NAME=VALUE")]
    pub params: Vec<String>,

    /// Write over output files that already exist. Without it a render that
    /// would land on any existing file fails before evaluating a frame.
    #[arg(long)]
    pub overwrite: bool,

    /// Render picture only. An image sequence carries no sound, so a
    /// composition with audio layers otherwise gets a WAV beside its frames,
    /// covering the same range; this leaves it out. Either way a project
    /// whose sound is not in the deliverable says so.
    #[arg(long)]
    pub no_audio: bool,

    /// How progress is reported.
    #[arg(long, value_enum, default_value_t = ProgressMode::Auto)]
    pub progress: ProgressMode,
}

/// How the run narrates itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ProgressMode {
    /// A progress bar when the terminal is interactive, JSON lines otherwise.
    Auto,
    /// A progress bar on stderr.
    Bar,
    /// One JSON object per line on stdout.
    Json,
    /// Nothing but the final failure, if there is one.
    Quiet,
}

/// Bits per channel for PNG output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PngBits {
    #[value(name = "8")]
    Eight,
    #[value(name = "16")]
    Sixteen,
}

impl From<PngBits> for PngDepth {
    fn from(bits: PngBits) -> Self {
        match bits {
            PngBits::Eight => PngDepth::Eight,
            PngBits::Sixteen => PngDepth::Sixteen,
        }
    }
}

/// One selectable render output, spelled the way a caller spells it.
///
/// Deliberately one variant per [`EncodeTarget`] rather than per writable
/// file: PNG's bit depth is `--png-depth`, so `ravel-cli list codecs` and
/// `--format` name the same set and a caller can feed one to the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// PNG sequence. Always writable.
    Png,
    /// EXR sequence. Always writable.
    Exr,
    Vp9,
    Av1,
    Prores,
    H264,
    H265,
}

impl OutputFormat {
    /// Every format, in the order `list codecs` prints them — the order of
    /// [`ravel_core::media::encode::enumerate_encoders`].
    pub const ALL: &'static [Self] = &[
        Self::Png,
        Self::Exr,
        Self::Vp9,
        Self::Av1,
        Self::Prores,
        Self::H264,
        Self::H265,
    ];

    /// The identifier used on the command line and in the JSON output.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Exr => "exr",
            Self::Vp9 => "vp9",
            Self::Av1 => "av1",
            Self::Prores => "prores",
            Self::H264 => "h264",
            Self::H265 => "h265",
        }
    }

    /// The enumeration row this format asks about.
    pub const fn target(self) -> EncodeTarget {
        match self {
            Self::Png => EncodeTarget::ImageSequence(ImageFormat::Png),
            Self::Exr => EncodeTarget::ImageSequence(ImageFormat::Exr),
            Self::Vp9 => EncodeTarget::Video(VideoCodec::Vp9),
            Self::Av1 => EncodeTarget::Video(VideoCodec::Av1),
            Self::Prores => EncodeTarget::Video(VideoCodec::ProRes),
            Self::H264 => EncodeTarget::Video(VideoCodec::H264),
            Self::H265 => EncodeTarget::Video(VideoCodec::H265),
        }
    }

    /// The still-image writer for this format, or `None` for a video target
    /// — which Ravel can enumerate but cannot yet write (`EXPORT-4`).
    pub const fn sequence_codec(self, depth: PngDepth) -> Option<SequenceCodec> {
        match self {
            Self::Png => Some(SequenceCodec::Png(depth)),
            Self::Exr => Some(SequenceCodec::Exr),
            Self::Vp9 | Self::Av1 | Self::Prores | Self::H264 | Self::H265 => None,
        }
    }

    /// The format whose enumeration row is `target`.
    pub fn from_target(target: EncodeTarget) -> Option<Self> {
        Self::ALL.iter().copied().find(|f| f.target() == target)
    }
}

/// An inclusive range of absolute frame numbers.
///
/// Inclusive because that is how a shot is described everywhere else —
/// "frames 100 to 199" ends at 199 — and because splitting a render across
/// processes then reads as `0-4` and `5-9` rather than as two ranges that
/// both mention frame 5. The worker's half-open range is derived in
/// [`FrameRange::to_range`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameRange {
    pub first: u64,
    pub last: u64,
}

impl FrameRange {
    /// The half-open range [`ravel_core::runtime::RenderJob`] takes.
    ///
    /// Fails for a backwards range rather than silently rendering nothing:
    /// `--range 20-10` is a typo every time.
    pub fn to_range(self) -> Result<std::ops::Range<u64>, CliError> {
        if self.last < self.first {
            return Err(CliError::EmptyRange {
                start: self.first,
                end: self.last,
            });
        }
        // `u64::MAX` as the last frame parses and is not backwards, but has
        // no half-open spelling. Classified rather than left to `+ 1`, which
        // panics in a debug build and wraps to an "empty range" — a wrong
        // sentence — in a release one. Reported as a bad range because that
        // is what it is: a range no render can be expressed over.
        let end = self.last.checked_add(1).ok_or_else(|| CliError::BadRange {
            raw: format!("{}-{}", self.first, self.last),
        })?;
        Ok(self.first..end)
    }
}

impl FromStr for FrameRange {
    type Err = CliError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let bad = || CliError::BadRange {
            raw: text.to_string(),
        };
        // Split on the *last* hyphen so a future negative start would still
        // parse; today both ends are unsigned, and an empty half is a typo.
        match text.rsplit_once('-') {
            Some((first, last)) => {
                let first: u64 = first.trim().parse().map_err(|_| bad())?;
                let last: u64 = last.trim().parse().map_err(|_| bad())?;
                Ok(Self { first, last })
            }
            None => {
                let only: u64 = text.trim().parse().map_err(|_| bad())?;
                Ok(Self {
                    first: only,
                    last: only,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_tilde_becomes_the_home_directory() {
        let home = dirs::home_dir().expect("the test host has a home directory");
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/renders"), home.join("renders"));
        assert_eq!(expand_tilde("~/a/b.ravprj"), home.join("a/b.ravprj"));
    }

    /// Expansion must not touch a path that only looks like it starts with a
    /// home reference, or a relative path that happens to contain a tilde.
    #[test]
    fn only_a_leading_bare_tilde_expands() {
        for text in [
            "~other/renders",
            "~backup",
            "/tmp/~/renders",
            "./~",
            "renders",
            "",
        ] {
            assert_eq!(
                expand_tilde(text),
                PathBuf::from(text),
                "{text:?} must pass through unchanged"
            );
        }
    }

    /// The three path arguments all route through the same expansion — a
    /// tilde that works for `--output` and not for the project is worse than
    /// one that works for neither.
    #[test]
    fn every_path_argument_expands_a_tilde() {
        let home = dirs::home_dir().expect("the test host has a home directory");
        let Command::Render(args) =
            Cli::parse_from(["ravel-cli", "render", "~/p.ravprj", "--output", "~/renders"]).command
        else {
            panic!("render parses as render");
        };
        assert_eq!(args.project, home.join("p.ravprj"));
        assert_eq!(args.output, home.join("renders"));

        let Command::Interactive(project) =
            Cli::parse_from(["ravel-cli", "interactive", "~/p.ravprj"]).command
        else {
            panic!("interactive parses as interactive");
        };
        assert_eq!(project.project, home.join("p.ravprj"));

        let Command::List(ListCommand::Comps(project)) =
            Cli::parse_from(["ravel-cli", "list", "comps", "~/p.ravprj"]).command
        else {
            panic!("list comps parses as list comps");
        };
        assert_eq!(project.project, home.join("p.ravprj"));
    }

    #[test]
    fn a_range_is_inclusive_at_both_ends() {
        let range: FrameRange = "100-199".parse().expect("parses");
        assert_eq!(
            range,
            FrameRange {
                first: 100,
                last: 199
            }
        );
        assert_eq!(range.to_range().expect("forward"), 100..200);
    }

    #[test]
    fn a_bare_number_is_one_frame() {
        let range: FrameRange = "42".parse().expect("parses");
        assert_eq!(range.to_range().expect("forward"), 42..43);
    }

    #[test]
    fn a_backwards_range_is_refused() {
        let range: FrameRange = "20-10".parse().expect("parses");
        assert!(matches!(
            range.to_range(),
            Err(CliError::EmptyRange { start: 20, end: 10 })
        ));
    }

    /// The last representable frame has no half-open spelling. It must be a
    /// classified refusal, not a debug panic and not a release wrap into a
    /// message about an empty range.
    #[test]
    fn a_range_that_cannot_be_made_half_open_is_refused() {
        let pair: FrameRange = "0-18446744073709551615".parse().expect("parses");
        let error = pair.to_range().expect_err("u64::MAX + 1 does not exist");
        assert!(matches!(error, CliError::BadRange { .. }), "{error:?}");
        assert_eq!(error.code(), crate::error::EXIT_USAGE);

        let single: FrameRange = "18446744073709551615".parse().expect("parses");
        assert!(matches!(single.to_range(), Err(CliError::BadRange { .. })));

        // One below still works, so the refusal is the boundary and not an
        // off-by-one that lost the last usable frame.
        let usable: FrameRange = "18446744073709551614".parse().expect("parses");
        assert_eq!(
            usable.to_range().expect("representable"),
            u64::MAX - 1..u64::MAX
        );
    }

    #[test]
    fn nonsense_is_refused() {
        for text in ["", "a-b", "1-", "-1", "1-2-x"] {
            assert!(
                text.parse::<FrameRange>().is_err(),
                "{text:?} must not parse"
            );
        }
    }

    /// `--format` and `list codecs` have to name the same set, or a caller
    /// cannot feed one to the other.
    #[test]
    fn every_enumerated_target_has_a_format_name() {
        for row in ravel_media::encode::available_encoders() {
            assert!(
                OutputFormat::from_target(row.target).is_some(),
                "{:?} has no --format spelling",
                row.target
            );
        }
        for format in OutputFormat::ALL {
            assert!(
                ravel_media::encode::available_encoders()
                    .iter()
                    .any(|row| row.target == format.target()),
                "{} names a target nothing enumerates",
                format.id()
            );
        }
    }
}
