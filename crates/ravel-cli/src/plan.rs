// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Arguments plus a loaded project into a render the worker can be handed.
//!
//! # Why this is a function and not a `main`
//!
//! Everything that can refuse a render is decided here, **before** a job is
//! submitted: the format, the composition, the range, the parameters, the
//! output names. That ordering is what the plan's "fails before a frame is
//! evaluated" conditions come down to, and it is what lets the interactive
//! mode (`EXPORT-7`) reuse the whole of it — that mode builds the same
//! [`RenderArgs`](crate::args::RenderArgs) from answers instead of from
//! `argv` and calls [`plan_render`] after each one to say whether the answer
//! so far is renderable.
//!
//! The one thing not decided here is whether the output already exists,
//! because that is a question about the filesystem rather than about the
//! arguments and the document — and keeping this function free of the
//! filesystem is what lets `EXPORT-7` call it after every answer. The
//! authoritative check belongs to the worker ([`OverwritePolicy`]), which
//! performs it at the instant the job starts, under the same lockless
//! assumptions as the encoder that writes. [`crate::render_with_hooks`] scans
//! for conflicts once more before that, early enough that the refusal does
//! not queue behind building a GPU context; both go through
//! [`RenderOutput::conflicts`], so it is one answer asked twice rather than
//! two answers.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use ravel_core::composition::Document;
use ravel_core::exposed::apply::{self, AssetContext};
use ravel_core::exposed::listing::ExposedListing;
use ravel_core::id::CompId;
use ravel_core::media::encode::{
    Availability, EncoderAvailability, ImageSequenceOutput, SequenceCodec,
};
use ravel_core::runtime::{OverwritePolicy, RenderOutput};

use crate::args::{OutputFormat, RenderArgs};
use crate::error::CliError;

/// A render that has been fully decided.
pub struct RenderPlan {
    /// The document the job renders, with every `--param` already applied.
    pub document: Arc<Document>,
    pub comp: CompId,
    /// Name of `comp`, for the progress line.
    pub comp_name: String,
    /// Half-open absolute frame range.
    pub range: std::ops::Range<u64>,
    pub codec: SequenceCodec,
    pub output: ImageSequenceOutput,
    pub overwrite: OverwritePolicy,
    /// Everything the user should know but that does not stop the render.
    pub warnings: Vec<Warning>,
}

impl RenderPlan {
    /// What the job would occupy on disk.
    pub fn render_output(&self) -> RenderOutput {
        RenderOutput::Sequence(self.output.clone())
    }

    pub fn frame_count(&self) -> u64 {
        self.range.end.saturating_sub(self.range.start)
    }
}

/// Something worth saying that is not a reason to stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    /// The project has layers with audio and this build renders picture
    /// only. Said out loud so a silent deliverable is never a surprise;
    /// `EXPORT-4` is what removes this.
    AudioNotRendered { layers: usize },
    /// A supplied value did not (fully) reach the parameter it names — the
    /// node was deleted, or the parameter is animated where the value would
    /// have gone. `apply` reports these rather than failing, because the
    /// project is what is broken, not the call.
    BindingIssue { detail: String },
}

/// Decide everything about the render, or refuse it.
///
/// `encoders` is passed in rather than probed so the awkward environments —
/// no FFmpeg, no VideoToolbox — are testable on a machine that has both,
/// exactly as `ravel-core` does with
/// [`EncoderProbe`](ravel_core::media::encode::EncoderProbe).
pub fn plan_render(
    args: &RenderArgs,
    document: &Document,
    project_root: Option<&Path>,
    encoders: &[EncoderAvailability],
) -> Result<RenderPlan, CliError> {
    // The format first: a request nothing here can write should not spend
    // time loading parameters or scanning layers to find that out.
    let codec = resolve_codec(args.format, args.png_depth.into(), encoders)?;

    let comp = resolve_comp(document, args.comp.as_deref())?;
    let comp_name = document
        .get_composition(comp)
        .expect("resolve_comp returned a composition of this document")
        .name
        .clone();

    let range = match args.range {
        Some(range) => range.to_range()?,
        None => {
            let duration = document
                .get_composition(comp)
                .expect("resolved above")
                .duration_frames;
            0..duration
        }
    };
    if range.end <= range.start {
        return Err(CliError::EmptyRange {
            start: range.start,
            // Reported the way the user writes it: inclusive.
            end: range.end.saturating_sub(1),
        });
    }

    let output = ImageSequenceOutput::new(
        &args.output,
        args.prefix.clone(),
        args.suffix.clone(),
        codec,
        args.padding,
    )?;

    let listing = ExposedListing::of(document);
    let values = crate::params::parse(&args.params, &listing)?;
    let (document, mut warnings) = apply_values(document.clone(), &values, project_root)?;

    if let Some(layers) = audio_layers(&document, comp) {
        warnings.push(Warning::AudioNotRendered { layers });
    }

    Ok(RenderPlan {
        document: Arc::new(document),
        comp,
        comp_name,
        range,
        codec,
        output,
        overwrite: if args.overwrite {
            OverwritePolicy::Replace
        } else {
            OverwritePolicy::Refuse
        },
        warnings,
    })
}

/// Apply the supplied values, turning the bindings that did not land into
/// warnings.
fn apply_values(
    document: Document,
    values: &HashMap<String, ravel_core::exposed::ExposedValue>,
    project_root: Option<&Path>,
) -> Result<(Document, Vec<Warning>), CliError> {
    if values.is_empty() {
        return Ok((document, Vec::new()));
    }
    let assets = match project_root {
        Some(root) => AssetContext::rooted(root),
        None => AssetContext::default(),
    };
    let applied = apply::apply(document, values, assets)?;
    let warnings = applied
        .issues
        .iter()
        .map(|issue| Warning::BindingIssue {
            detail: issue.to_string(),
        })
        .collect();
    Ok((applied.document, warnings))
}

/// Which composition to render: the one named, or the project's root.
fn resolve_comp(document: &Document, requested: Option<&str>) -> Result<CompId, CliError> {
    let Some(requested) = requested else {
        return document
            .root_comp
            .filter(|id| document.get_composition(*id).is_some())
            // A project with exactly one composition and no root recorded is
            // unambiguous, and refusing it would make `--comp` mandatory for
            // documents the GUI opens without complaint.
            .or_else(|| match document.compositions.len() {
                1 => document.compositions.keys().copied().next(),
                _ => None,
            })
            .ok_or(CliError::NoComposition);
    };

    // By name first: a name is what the user sees, and a composition called
    // "2" must not be shadowed by the composition whose id is 2.
    let mut by_name = document
        .compositions
        .iter()
        .filter(|(_, comp)| comp.name == requested)
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    by_name.sort();
    match by_name.as_slice() {
        [id] => return Ok(*id),
        // Names are not unique in a document, and picking one — the lowest
        // id, say — is deterministic without being *predictable*: a script
        // would render a different composition than its author meant and
        // never learn. Everything else this unit refuses, it refuses out
        // loud (an existing output, an unwritable codec), so this does too,
        // and it names the ids so the retry can be typed from the message.
        [_, _, ..] => {
            return Err(CliError::AmbiguousComposition {
                name: requested.to_string(),
                ids: by_name.iter().map(|id| id.raw()).collect(),
            });
        }
        [] => {}
    }
    if let Ok(raw) = requested.parse::<u64>() {
        let id = CompId::new(raw);
        if document.get_composition(id).is_some() {
            return Ok(id);
        }
    }
    Err(CliError::UnknownComposition(requested.to_string()))
}

/// How many layers of `comp` carry audio, or `None` when none do.
///
/// Scoped to the composition being rendered rather than to the whole
/// document: a warning about audio in a composition nobody asked for would
/// train the reader to ignore it.
fn audio_layers(document: &Document, comp: CompId) -> Option<usize> {
    let count = document
        .get_composition(comp)?
        .layers
        .iter()
        .filter(|layer| layer.audio.is_some())
        .count();
    (count > 0).then_some(count)
}

/// The writer for `format`, or why there is none.
fn resolve_codec(
    format: OutputFormat,
    depth: ravel_core::media::encode::PngDepth,
    encoders: &[EncoderAvailability],
) -> Result<SequenceCodec, CliError> {
    let target = format.target();
    let row = encoders
        .iter()
        .find(|row| row.target == target)
        // Every `OutputFormat` names a row of the table `enumerate_encoders`
        // always returns whole; a caller that passed a filtered list gets the
        // same answer as an unavailable entry rather than a panic.
        .ok_or(CliError::CodecNoWriter {
            format: format.id(),
        })?;

    if let Availability::Unavailable(reason) = &row.availability {
        return Err(CliError::CodecUnavailable {
            format: format.id(),
            reason: reason.clone(),
        });
    }
    // Available and still unwritable: the enumeration answers "can this
    // machine encode it", and for video the missing half is Ravel's own
    // container writer (`EXPORT-4`). Saying so beats pretending the
    // environment is at fault.
    format.sequence_codec(depth).ok_or(CliError::CodecNoWriter {
        format: format.id(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::{Composition, Layer};
    use ravel_core::graph::Graph;
    use ravel_core::id::LayerId;
    use ravel_core::media::encode::{EncodeRoute, EncodeTarget, PngDepth, UnavailableReason};
    use ravel_core::media::{ImageFormat, VideoCodec};
    use ravel_core::types::FrameRate;
    use ravel_media::encode::available_encoders;

    fn comp(id: u64, name: &str) -> Composition {
        Composition::new(CompId::new(id), name, (16, 16), FrameRate::new(30, 1), 50)
    }

    fn document() -> Document {
        Document::default()
            .with_composition(comp(1, "Main"))
            .with_composition(comp(2, "Insert"))
    }

    fn args(output: &Path) -> RenderArgs {
        RenderArgs {
            project: "project.ravprj".into(),
            comp: None,
            range: None,
            format: OutputFormat::Png,
            png_depth: crate::args::PngBits::Eight,
            output: output.to_path_buf(),
            prefix: "frame_".into(),
            suffix: String::new(),
            padding: 4,
            params: Vec::new(),
            overwrite: false,
            progress: crate::args::ProgressMode::Quiet,
        }
    }

    #[test]
    fn no_composition_argument_renders_the_root_over_its_whole_duration() {
        let plan = plan_render(
            &args(Path::new("/tmp/out")),
            &document(),
            None,
            &available_encoders(),
        )
        .expect("plans");
        assert_eq!(plan.comp, CompId::new(1));
        assert_eq!(plan.comp_name, "Main");
        assert_eq!(plan.range, 0..50);
        assert_eq!(plan.overwrite, OverwritePolicy::Refuse);
    }

    #[test]
    fn a_composition_can_be_named_or_numbered() {
        let mut by_name = args(Path::new("/tmp/out"));
        by_name.comp = Some("Insert".into());
        assert_eq!(
            plan_render(&by_name, &document(), None, &available_encoders())
                .expect("plans")
                .comp,
            CompId::new(2)
        );

        let mut by_id = args(Path::new("/tmp/out"));
        by_id.comp = Some("2".into());
        assert_eq!(
            plan_render(&by_id, &document(), None, &available_encoders())
                .expect("plans")
                .comp,
            CompId::new(2)
        );

        let mut unknown = args(Path::new("/tmp/out"));
        unknown.comp = Some("Nope".into());
        assert!(matches!(
            plan_render(&unknown, &document(), None, &available_encoders()),
            Err(CliError::UnknownComposition(_))
        ));
    }

    /// Names are not unique in a document. Picking the lowest id would be
    /// deterministic and still wrong: the script renders something its author
    /// did not name and never finds out.
    #[test]
    fn a_name_two_compositions_share_is_refused_and_offers_the_ids() {
        let document = Document::default()
            .with_composition(comp(1, "Main"))
            .with_composition(comp(4, "Main"));
        let mut ambiguous = args(Path::new("/tmp/out"));
        ambiguous.comp = Some("Main".into());

        let error = match plan_render(&ambiguous, &document, None, &available_encoders()) {
            Err(error) => error,
            Ok(plan) => panic!(
                "two compositions answer to that name, yet {:?} was chosen",
                plan.comp
            ),
        };
        match &error {
            CliError::AmbiguousComposition { name, ids } => {
                assert_eq!(name, "Main");
                assert_eq!(ids, &vec![1, 4], "the ids are offered, in order");
            }
            other => panic!("expected an ambiguity refusal, got {other:?}"),
        }
        assert_eq!(error.code(), crate::error::EXIT_USAGE);

        // The way out the message names has to actually work.
        let mut by_id = args(Path::new("/tmp/out"));
        by_id.comp = Some("4".into());
        assert_eq!(
            plan_render(&by_id, &document, None, &available_encoders())
                .expect("an id is never ambiguous")
                .comp,
            CompId::new(4)
        );
    }

    /// A composition whose *name* is a number must win over the id that
    /// happens to match it.
    #[test]
    fn a_numeric_name_is_still_a_name() {
        let document = Document::default()
            .with_composition(comp(1, "2"))
            .with_composition(comp(2, "Other"));
        let mut args = args(Path::new("/tmp/out"));
        args.comp = Some("2".into());
        assert_eq!(
            plan_render(&args, &document, None, &available_encoders())
                .expect("plans")
                .comp,
            CompId::new(1)
        );
    }

    #[test]
    fn an_inclusive_range_becomes_the_half_open_one_the_worker_takes() {
        let mut args = args(Path::new("/tmp/out"));
        args.range = Some("100-199".parse().expect("range"));
        let plan = plan_render(&args, &document(), None, &available_encoders()).expect("plans");
        assert_eq!(plan.range, 100..200);
        assert_eq!(plan.frame_count(), 100);
        assert_eq!(
            plan.output.frame_path(100).file_name().unwrap(),
            "frame_0100.png",
            "the file name is the absolute frame number"
        );
    }

    /// The output name components are checked by `ravel-core`, which refuses
    /// anything that could leave the output directory.
    #[test]
    fn an_escaping_prefix_is_refused() {
        let mut args = args(Path::new("/tmp/out"));
        args.prefix = "../escape_".into();
        assert!(matches!(
            plan_render(&args, &document(), None, &available_encoders()),
            Err(CliError::OutputName(_))
        ));
    }

    /// In a build with no FFmpeg every video target is unavailable, and the
    /// refusal has to carry the reason rather than a bare "no".
    #[test]
    fn an_unavailable_codec_is_refused_with_its_reason() {
        let encoders = vec![EncoderAvailability {
            target: EncodeTarget::Video(VideoCodec::Vp9),
            availability: Availability::Unavailable(UnavailableReason::FfmpegNotLinked),
        }];
        let error = resolve_codec(OutputFormat::Vp9, PngDepth::Eight, &encoders)
            .expect_err("no FFmpeg, no VP9");
        assert!(matches!(
            error,
            CliError::CodecUnavailable {
                format: "vp9",
                reason: UnavailableReason::FfmpegNotLinked
            }
        ));
        assert_eq!(error.code(), crate::error::EXIT_CODEC);
    }

    /// A codec this machine *can* encode but Ravel cannot yet write must say
    /// which of the two is missing.
    #[test]
    fn an_available_video_codec_still_has_no_writer_yet() {
        let encoders = vec![EncoderAvailability {
            target: EncodeTarget::Video(VideoCodec::Vp9),
            availability: Availability::Available(EncodeRoute::FfmpegSoftware {
                encoder: "libvpx-vp9",
            }),
        }];
        assert!(matches!(
            resolve_codec(OutputFormat::Vp9, PngDepth::Eight, &encoders),
            Err(CliError::CodecNoWriter { format: "vp9" })
        ));
    }

    #[test]
    fn image_sequences_are_writable_in_every_build() {
        let encoders = available_encoders();
        assert_eq!(
            resolve_codec(OutputFormat::Png, PngDepth::Sixteen, &encoders).expect("png"),
            SequenceCodec::Png(PngDepth::Sixteen)
        );
        assert_eq!(
            resolve_codec(OutputFormat::Exr, PngDepth::Eight, &encoders).expect("exr"),
            SequenceCodec::Exr
        );
        assert!(encoders.iter().any(|row| row.target
            == EncodeTarget::ImageSequence(ImageFormat::Png)
            && row.is_available()));
    }

    /// A project with audio renders picture only until `EXPORT-4`, and has
    /// to say so rather than hand back a silent file without comment.
    #[test]
    fn a_composition_with_audio_layers_warns() {
        use ravel_core::composition::AudioSource;

        let mut composition = comp(1, "Main");
        let mut layer = Layer::new(LayerId::new(1), "voice", Graph::new());
        layer.audio = Some(AudioSource {
            asset_id: "a".into(),
            stream_index: 0,
            ..Default::default()
        });
        composition = composition.add_layer(layer);
        composition = composition.add_layer(Layer::new(LayerId::new(2), "silent", Graph::new()));

        let document = Document::default().with_composition(composition);
        let plan = plan_render(
            &args(Path::new("/tmp/out")),
            &document,
            None,
            &available_encoders(),
        )
        .expect("plans");
        assert_eq!(plan.warnings, vec![Warning::AudioNotRendered { layers: 1 }]);
    }

    #[test]
    fn a_composition_without_audio_says_nothing() {
        let plan = plan_render(
            &args(Path::new("/tmp/out")),
            &document(),
            None,
            &available_encoders(),
        )
        .expect("plans");
        assert!(plan.warnings.is_empty());
    }
}
