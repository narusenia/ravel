// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless state of the export dialog (`render-export-plan.md`, unit 5).
//!
//! The dialog is a form over strings and a few choices; this is the part that
//! turns one into a **resolved job description** — the composition, the
//! half-open absolute frame range, the [`ImageSequenceOutput`] the frames are
//! named from, and the [`OverwritePolicy`]. The GPUI view holds the widgets,
//! reads them out on OK, and hands the result here.
//!
//! # Who is authoritative
//!
//! Nothing checked here is a *decision*: the render worker checks the same
//! things again at the instant the job starts
//! (`ravel_core::runtime::render::check_preconditions` — composition present,
//! range non-empty, output free) and its answer is the one that governs, for
//! the reason its own documentation gives: the filesystem's state at submit
//! time is already stale by the time a frame is written. What this adds is a
//! sentence in the dialog instead of a failed row in the queue panel, which
//! is a presentation concern.
//!
//! The same holds for `ravel-cli`: `plan::plan_render` resolves the flag form
//! of the same request, and both front ends read the one authority for what
//! can be written (`ravel_media::encode::available_encoders`) and build the
//! one output description (`ImageSequenceOutput`). The two resolvers are not
//! shared code today because `plan_render` takes `clap` argument types; if
//! they are ever unified, `plan_render` is the side to keep.

use ravel_core::id::CompId;
use ravel_core::media::ImageFormat;
use ravel_core::media::encode::{ImageSequenceOutput, PngDepth, SequenceCodec};
use ravel_core::runtime::{OverwritePolicy, RenderOutput};
use std::ops::Range;
use std::path::PathBuf;

/// Zero padding of the frame number in a sequence file name.
///
/// Four digits reaches frame 9999 before the names widen, which is past the
/// length of anything the timeline model can hold at a sane frame rate, and
/// is what `ravel-cli` defaults to — two front ends that pad differently
/// produce two sets of names for one project.
pub const DEFAULT_PADDING: usize = 4;

/// Largest padding the form accepts. Beyond this a name is all zeros and
/// nothing is gained; the limit exists so a mistyped field cannot build a
/// path of unbounded length.
pub const MAX_PADDING: usize = 12;

/// Everything the export dialog collects, as the widgets hold it.
///
/// Strings rather than numbers for the typed fields, because a half-typed
/// value has to survive between keystrokes — the parse happens once, in
/// [`resolve`](Self::resolve).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportSettings {
    /// Composition to render. `None` when the project has none, which
    /// [`resolve`](Self::resolve) refuses rather than guessing at.
    pub comp: Option<CompId>,
    /// First frame, absolute and **inclusive**.
    pub start: String,
    /// Last frame, absolute and **inclusive** — as the user reads a range,
    /// and as `ravel-cli`'s `--range start:end` takes it.
    pub end: String,
    /// Still-image format of the sequence.
    pub format: ImageFormat,
    /// Bit depth, used only when `format` is [`ImageFormat::Png`].
    pub png_depth: PngDepth,
    /// Directory the frames are written into.
    pub directory: String,
    /// Text before the frame number.
    pub prefix: String,
    /// Text after the frame number, before the extension.
    pub suffix: String,
    /// Digits the frame number is padded to.
    pub padding: String,
    /// Whether existing output may be replaced.
    pub overwrite: bool,
    /// Whether a soundtrack is written beside the frames.
    pub audio: bool,
}

impl ExportSettings {
    /// The form as it opens for `comp`: the composition's whole duration,
    /// PNG at eight bits, and the composition's name as the file-name prefix.
    ///
    /// `duration` is the composition's frame count, so the inclusive last
    /// frame is one before it; a zero-length composition leaves both ends at
    /// frame 0 and is refused on OK rather than at construction, because the
    /// dialog has to be able to open on it.
    pub fn for_composition(comp: CompId, name: &str, duration: u64, directory: PathBuf) -> Self {
        Self {
            comp: Some(comp),
            start: "0".to_owned(),
            end: duration.saturating_sub(1).to_string(),
            format: ImageFormat::Png,
            png_depth: PngDepth::Eight,
            directory: directory.to_string_lossy().into_owned(),
            prefix: sanitized_prefix(name),
            suffix: String::new(),
            padding: DEFAULT_PADDING.to_string(),
            overwrite: false,
            audio: true,
        }
    }

    /// The codec the current format and depth name, or `None` for a format
    /// with no sequence writer ([`ImageFormat::Tiff`], [`ImageFormat::Dpx`]).
    pub fn codec(&self) -> Option<SequenceCodec> {
        SequenceCodec::from_image_format(self.format, self.png_depth)
    }

    /// Turn the form into the job description, or say which field is wrong.
    ///
    /// Field order is the order they are read on screen, so the first
    /// complaint is the topmost mistake.
    pub fn resolve(&self) -> Result<ExportRequest, ExportError> {
        let comp = self.comp.ok_or(ExportError::NoComposition)?;

        let start: u64 = self
            .start
            .trim()
            .parse()
            .map_err(|_| ExportError::InvalidStart)?;
        let end: u64 = self
            .end
            .trim()
            .parse()
            .map_err(|_| ExportError::InvalidEnd)?;
        // Inclusive on screen, half-open in the worker. `end < start` is the
        // "out before in" the unit's completion criteria name; `end == start`
        // is one frame, not none.
        if end < start {
            return Err(ExportError::EmptyRange);
        }
        let range = start..end + 1;

        let codec = self.codec().ok_or(ExportError::NoWriter)?;

        let directory = self.directory.trim();
        if directory.is_empty() {
            return Err(ExportError::MissingDirectory);
        }

        let padding: usize = self
            .padding
            .trim()
            .parse()
            .map_err(|_| ExportError::InvalidPadding)?;
        if padding == 0 || padding > MAX_PADDING {
            return Err(ExportError::InvalidPadding);
        }

        // `ImageSequenceOutput::new` is the one place a sequence name is
        // checked for components that could leave the directory; the dialog
        // reports its refusal rather than repeating the rule.
        let output = ImageSequenceOutput::new(
            PathBuf::from(directory),
            self.prefix.trim(),
            self.suffix.trim(),
            codec,
            padding,
        )
        .map_err(|_| ExportError::OutputName)?;

        Ok(ExportRequest {
            comp,
            range,
            output,
            overwrite: if self.overwrite {
                OverwritePolicy::Replace
            } else {
                OverwritePolicy::Refuse
            },
            audio: self.audio,
        })
    }
}

/// A resolved export: everything the host needs to build a
/// [`RenderJob`](ravel_core::runtime::RenderJob) and its encoder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportRequest {
    /// Composition to render.
    pub comp: CompId,
    /// Half-open range of absolute frame numbers, as the worker takes it.
    pub range: Range<u64>,
    /// The frames' names. Cloned into both the encoder and the job's
    /// [`RenderOutput`], which [`RenderJob`](ravel_core::runtime::RenderJob)
    /// requires to describe the same files.
    pub output: ImageSequenceOutput,
    /// Whether existing output is an error.
    pub overwrite: OverwritePolicy,
    /// Whether a soundtrack is written beside the frames.
    pub audio: bool,
}

impl ExportRequest {
    /// What the job occupies on disk, for the conflict check and the job.
    pub fn render_output(&self) -> RenderOutput {
        RenderOutput::Sequence(self.output.clone())
    }

    /// Frames the range covers.
    pub fn frame_count(&self) -> u64 {
        self.range.end.saturating_sub(self.range.start)
    }

    /// Where the soundtrack goes, named from the same components as the
    /// frames (`ImageSequenceOutput::audio_path`).
    pub fn audio_path(&self) -> PathBuf {
        self.output.audio_path(self.range.clone())
    }
}

/// Why a filled-in form cannot become a job.
///
/// Carries no prose: the host turns each variant into a sentence through
/// `t!`, the same split every headless refusal in this crate uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportError {
    /// The project has no composition to render.
    NoComposition,
    /// The first frame is not a number.
    InvalidStart,
    /// The last frame is not a number.
    InvalidEnd,
    /// The last frame is before the first.
    EmptyRange,
    /// The chosen format has no sequence writer.
    NoWriter,
    /// No output directory was given.
    MissingDirectory,
    /// The padding is not a usable digit count.
    InvalidPadding,
    /// The prefix or suffix would put frames outside the directory.
    OutputName,
}

impl ExportError {
    /// The locale key of the sentence shown under the form.
    pub fn message_key(self) -> &'static str {
        match self {
            Self::NoComposition => "export.error.no_composition",
            Self::InvalidStart => "export.error.invalid_start",
            Self::InvalidEnd => "export.error.invalid_end",
            Self::EmptyRange => "export.error.empty_range",
            Self::NoWriter => "export.error.no_writer",
            Self::MissingDirectory => "export.error.missing_directory",
            Self::InvalidPadding => "export.error.invalid_padding",
            Self::OutputName => "export.error.output_name",
        }
    }
}

/// Turn a composition name into something usable as a file-name prefix.
///
/// Only the characters `ImageSequenceOutput::new` refuses are replaced —
/// separators, the drive colon, and the null byte — so a name that is already
/// a fine prefix survives untouched. Names are user text, so this cannot
/// assume anything about them; what it must not do is hand the constructor a
/// value it will reject and turn the dialog's opening state into an error.
fn sanitized_prefix(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' | '.' => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "frame_".to_owned()
    } else {
        format!("{trimmed}_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comp() -> CompId {
        CompId::new(7)
    }

    fn settings() -> ExportSettings {
        ExportSettings::for_composition(comp(), "shot 010", 120, PathBuf::from("/tmp/out"))
    }

    #[test]
    fn a_filled_form_resolves_into_a_job_description() {
        let request = settings().resolve().expect("the default form is valid");
        assert_eq!(request.comp, comp());
        // 120 frames, entered as the inclusive 0..=119 and handed on as the
        // half-open range the worker takes.
        assert_eq!(request.range, 0..120);
        assert_eq!(request.frame_count(), 120);
        assert_eq!(request.overwrite, OverwritePolicy::Refuse);
        assert_eq!(
            request.output.codec(),
            SequenceCodec::Png(PngDepth::Eight),
            "the dialog opens on the format the CLI defaults to",
        );
        assert_eq!(request.output.padding(), DEFAULT_PADDING);
        assert_eq!(
            request.output.frame_path(9),
            PathBuf::from("/tmp/out/shot 010_0009.png"),
        );
        assert_eq!(
            request.render_output(),
            RenderOutput::Sequence(request.output.clone()),
            "the job's output and its encoder must describe the same files",
        );
    }

    /// The unit's second completion criterion.
    #[test]
    fn a_range_that_ends_before_it_starts_is_refused() {
        let mut form = settings();
        form.start = "100".into();
        form.end = "99".into();
        assert_eq!(form.resolve(), Err(ExportError::EmptyRange));

        // The boundary is not off by one: one frame is a range.
        form.end = "100".into();
        let request = form.resolve().expect("a single frame is a range");
        assert_eq!(request.range, 100..101);
        assert_eq!(request.frame_count(), 1);
    }

    #[test]
    fn unparsable_frame_numbers_name_the_field_they_came_from() {
        let mut form = settings();
        form.start = "one".into();
        assert_eq!(form.resolve(), Err(ExportError::InvalidStart));

        let mut form = settings();
        form.end = "-1".into();
        assert_eq!(form.resolve(), Err(ExportError::InvalidEnd));

        let mut form = settings();
        form.start = " 12 ".into();
        form.end = " 20 ".into();
        assert_eq!(
            form.resolve()
                .expect("surrounding space is not a mistake")
                .range,
            12..21,
        );
    }

    #[test]
    fn an_output_that_could_escape_its_directory_is_refused() {
        let mut form = settings();
        form.prefix = "../frame_".into();
        assert_eq!(form.resolve(), Err(ExportError::OutputName));

        let mut form = settings();
        form.suffix = "/beauty".into();
        assert_eq!(form.resolve(), Err(ExportError::OutputName));
    }

    #[test]
    fn a_missing_directory_is_refused() {
        let mut form = settings();
        form.directory = "   ".into();
        assert_eq!(form.resolve(), Err(ExportError::MissingDirectory));
    }

    #[test]
    fn padding_must_be_a_usable_digit_count() {
        for bad in ["0", "", "x", "13"] {
            let mut form = settings();
            form.padding = bad.into();
            assert_eq!(
                form.resolve(),
                Err(ExportError::InvalidPadding),
                "padding {bad:?} should be refused",
            );
        }
    }

    #[test]
    fn a_format_with_no_sequence_writer_is_refused() {
        let mut form = settings();
        form.format = ImageFormat::Tiff;
        assert_eq!(form.resolve(), Err(ExportError::NoWriter));
        assert_eq!(form.codec(), None);

        form.format = ImageFormat::Exr;
        assert_eq!(form.codec(), Some(SequenceCodec::Exr));
    }

    #[test]
    fn a_project_with_no_composition_cannot_be_exported() {
        let mut form = settings();
        form.comp = None;
        assert_eq!(form.resolve(), Err(ExportError::NoComposition));
    }

    #[test]
    fn overwrite_maps_to_the_workers_policy() {
        let mut form = settings();
        form.overwrite = true;
        assert_eq!(
            form.resolve().expect("valid").overwrite,
            OverwritePolicy::Replace,
        );
    }

    /// The soundtrack goes beside the frames, named from the same components
    /// — the rule `ImageSequenceOutput::audio_path` states, applied to the
    /// range the user typed.
    #[test]
    fn the_soundtrack_is_named_after_the_range() {
        let mut form = settings();
        form.start = "100".into();
        form.end = "199".into();
        let request = form.resolve().expect("valid");
        assert_eq!(
            request.audio_path(),
            PathBuf::from("/tmp/out/shot 010_0100-0199.wav"),
        );
    }

    /// A composition name is user text and may hold anything; the opening
    /// form must still be one `ImageSequenceOutput::new` accepts.
    #[test]
    fn the_opening_prefix_is_always_a_usable_name() {
        for name in ["a/b", "c:\\d", "..", "  ", "plain"] {
            let form = ExportSettings::for_composition(comp(), name, 10, PathBuf::from("/tmp/out"));
            assert!(
                form.resolve().is_ok(),
                "the dialog must open on a valid form for the name {name:?}",
            );
        }
    }

    #[test]
    fn every_error_has_its_own_message_key() {
        let mut seen = std::collections::HashSet::new();
        for error in [
            ExportError::NoComposition,
            ExportError::InvalidStart,
            ExportError::InvalidEnd,
            ExportError::EmptyRange,
            ExportError::NoWriter,
            ExportError::MissingDirectory,
            ExportError::InvalidPadding,
            ExportError::OutputName,
        ] {
            assert!(
                seen.insert(error.message_key()),
                "duplicate message key for {error:?}",
            );
        }
    }
}
