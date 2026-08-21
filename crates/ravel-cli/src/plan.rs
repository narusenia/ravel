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

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use ravel_core::composition::{Document, MediaAssetEntry, node_asset_reference};
use ravel_core::exposed::apply::{self, AssetContext};
use ravel_core::exposed::listing::ExposedListing;
use ravel_core::id::{AssetId, CompId};
use ravel_core::media::encode::{
    Availability, EncoderAvailability, ImageSequenceOutput, SequenceCodec,
};
use ravel_core::runtime::{OverwritePolicy, RenderOutput, occupied};

use crate::args::{OutputFormat, RenderArgs};
use crate::audio::{AudioPlan, NoAudio};
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
    /// The soundtrack to write beside the frames, or `None` when this render
    /// has none — either because the composition is silent or because
    /// [`warnings`](Self::warnings) says why it will not be rendered.
    pub audio: Option<AudioPlan>,
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

    /// Every path this render will occupy, the soundtrack included.
    ///
    /// The worker only knows about the frames — the WAV is written by the
    /// front end — so the overwrite refusal has to ask both. One list rather
    /// than two questions, **asked the same way**: `occupied` is the frames'
    /// own predicate, and answering it with `Path::exists` for the sound would
    /// have called a dangling symlink free where the frames call it taken. A
    /// weaker question there is not a smaller refusal, it is a render that
    /// starts and then writes through the link, outside its own output
    /// directory.
    pub fn conflicts(&self) -> Vec<std::path::PathBuf> {
        let mut conflicts = self.render_output().conflicts(self.range.clone());
        if let Some(audio) = &self.audio
            && occupied(&audio.path)
        {
            conflicts.push(audio.path.clone());
        }
        conflicts
    }
}

/// Something worth saying that is not a reason to stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    /// The composition has layers with audio and the deliverable will have
    /// none. Said out loud so a silent render is never a surprise; the
    /// reason distinguishes "you asked for that" from "this build cannot".
    AudioNotRendered { layers: usize, reason: NoAudio },
    /// One audio layer's source could not be loaded, so its sound is missing
    /// from an otherwise complete mix. A warning rather than a failure — the
    /// picture is still worth having and the mix is still the right length —
    /// but never silent, which is the point of `MAX_DECODE_BYTES` having a
    /// visible consequence.
    AudioSourceSkipped {
        /// The layer's name, or its id when the layer is gone.
        layer: String,
        /// The media asset's display name, or the reference itself when the
        /// document no longer has the asset to take a name from. Never the
        /// bare id of an asset that does exist: the reader knows it by name.
        asset: String,
        /// Why it could not be loaded.
        detail: String,
    },
    /// A supplied value did not (fully) reach the parameter it names — the
    /// node was deleted, or the parameter is animated where the value would
    /// have gone. `apply` reports these rather than failing, because the
    /// project is what is broken, not the call.
    BindingIssue { detail: String },
    /// A referenced media asset resolves to nothing, so every layer that
    /// names it renders transparent. The picture-side counterpart of
    /// [`AudioSourceSkipped`](Self::AudioSourceSkipped): a render that loses
    /// footage still produces the rest of the composition, but never
    /// silently.
    MediaOffline {
        /// The asset's display name, or the reference itself when the
        /// document no longer holds the asset — the same rule
        /// [`AudioSourceSkipped`](Self::AudioSourceSkipped) follows.
        asset: String,
        /// **Every** layer of the rendered composition that names it, in
        /// layer order. One row per asset with one layer name would hide the
        /// rest.
        layers: Vec<String>,
    },
    /// A referenced media asset resolves to a file that cannot be opened.
    /// Distinct from [`MediaOffline`](Self::MediaOffline) because the fix is
    /// different: the reference is right and the file is not.
    MediaUnreadable {
        asset: String,
        layers: Vec<String>,
        /// Why it could not be opened.
        detail: String,
    },
    /// An identifier parameter (a `layer.ref` target, a `precomp`
    /// composition, a `media` asset) is driven by something that does not
    /// stand still, so it references **nothing**
    /// ([`ParameterValue::identifier`](ravel_core::graph::ParameterValue::identifier)).
    /// One row per parameter, naming the shape that was ignored: a wire and
    /// a step curve are disconnected in different places.
    IdentifierNotStatic {
        /// The layer whose network holds the node, or its id when the layer
        /// is gone.
        layer: String,
        /// The node's raw id — the only name a node has.
        node: u64,
        param: String,
        /// Untranslated shape word from
        /// [`DynamicIdentifier::as_str`](ravel_core::graph::DynamicIdentifier::as_str).
        shape: &'static str,
    },
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

    // The picture's own warnings, decided from the document exactly as the
    // sound's are. `WARN-1` folded every identifier down to a static value,
    // so what a frame will reference is fully determined here — no frame has
    // to be evaluated to know a reference is dead, which is what keeps these
    // out of the evaluator and off the cached-frame problem.
    warnings.extend(media_warnings(&document, comp, &probe_asset));
    warnings.extend(identifier_warnings(&document, comp));

    let (audio, audio_warnings) =
        crate::audio::plan_audio(&document, comp, &output, range.clone(), !args.no_audio);
    warnings.extend(audio_warnings);

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
        audio,
        warnings,
    })
}

/// Why an asset's file cannot be opened, or `None` when there is nothing to
/// report.
///
/// A missing file is the ordinary case and needs no decoder: `resolved` is a
/// *mapping* of the persisted path, not a promise that anything is there, so
/// footage that moved after the project was saved lands here.
///
/// Everything that is there is asked of FFmpeg, whatever its
/// [`AssetKind`](ravel_core::composition::AssetKind): a still and a sequence
/// frame are decoded by `ravel_media::image_seq::read_image_frame`, which
/// opens them with the same `FfmpegDecoder` a container goes through, so the
/// probe and the render agree about what "readable" means. A sequence is
/// probed through its representative frame — the one `resolved` names — which
/// is the frame the render reads first.
///
/// A build without FFmpeg asks nothing and says nothing: **"cannot be
/// checked" is not "cannot be read"**, and the honest way to report a limited
/// build is the way [`NoAudio::NoDecoder`] does it — once, about the build.
fn probe_asset(entry: &MediaAssetEntry) -> Option<String> {
    let path = entry.resolved.as_ref()?;
    if let Err(error) = std::fs::metadata(path) {
        return Some(format!("{}: {error}", path.display()));
    }
    #[cfg(feature = "ffmpeg")]
    if let Err(error) = ravel_media::format::probe(path) {
        return Some(format!("{}: {error}", path.display()));
    }
    None
}

/// Every media asset the rendered composition references, and which of its
/// layers name it — the grouping the warnings are reported in.
///
/// Keyed by [`AssetId`] rather than by name so two assets that share a
/// display name stay two rows, and walked per layer so a layer whose network
/// references one asset from three nodes contributes its name once.
fn asset_references(document: &Document, comp: CompId) -> BTreeMap<AssetId, Vec<String>> {
    fn walk(graph: &ravel_core::graph::Graph, found: &mut BTreeSet<AssetId>) {
        for node in graph.nodes() {
            if let Some(subnet) = &node.subnet {
                walk(subnet, found);
            }
            if let Some(asset) = node_asset_reference(node) {
                found.insert(asset);
            }
        }
    }

    let mut references: BTreeMap<AssetId, Vec<String>> = BTreeMap::new();
    let Some(comp) = document.get_composition(comp) else {
        return references;
    };
    for layer in &comp.layers {
        let mut in_layer = BTreeSet::new();
        walk(&layer.network, &mut in_layer);
        for asset in in_layer {
            references
                .entry(asset)
                .or_default()
                .push(layer.name.clone());
        }
    }
    references
}

/// One warning per referenced asset that will not produce a picture:
/// offline (the reference resolves to nothing) or unreadable (it resolves to
/// a file `probe` cannot open).
///
/// `probe` is injected for the same reason `plan_render` takes its encoders:
/// the interesting cases — a file that vanished, a container FFmpeg refuses —
/// have to be testable on a machine where everything works.
fn media_warnings(
    document: &Document,
    comp: CompId,
    probe: &dyn Fn(&MediaAssetEntry) -> Option<String>,
) -> Vec<Warning> {
    let mut warnings = Vec::new();
    for (id, layers) in asset_references(document, comp) {
        let entry = document.get_media_asset(id);
        // The name the user knows the asset by, falling back to the
        // reference itself — `AudioSourceSkipped`'s rule.
        let asset = entry
            .map(|entry| entry.name.clone())
            .unwrap_or_else(|| id.to_string());
        let Some(entry) = entry.filter(|entry| entry.resolved.is_some()) else {
            warnings.push(Warning::MediaOffline { asset, layers });
            continue;
        };
        if let Some(detail) = probe(entry) {
            warnings.push(Warning::MediaUnreadable {
                asset,
                layers,
                detail,
            });
        }
    }
    warnings
}

/// One warning per identifier parameter of the rendered composition that
/// references nothing because its value does not stand still.
fn identifier_warnings(document: &Document, comp: CompId) -> Vec<Warning> {
    let composition = document.get_composition(comp);
    document
        .dynamic_identifiers()
        .into_iter()
        .filter(|entry| entry.comp == Some(comp))
        .map(|entry| Warning::IdentifierNotStatic {
            layer: composition
                .and_then(|comp| {
                    comp.layers
                        .iter()
                        .find(|layer| Some(layer.id) == entry.layer)
                })
                .map(|layer| layer.name.clone())
                .unwrap_or_else(|| {
                    entry
                        .layer
                        .map(|id| id.raw().to_string())
                        .unwrap_or_default()
                }),
            node: entry.node.raw(),
            param: entry.param,
            shape: entry.reason.as_str(),
        })
        .collect()
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
    use ravel_core::id::{AssetId, LayerId};
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
            no_audio: false,
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

    /// A project whose sound will not be in the deliverable has to say so
    /// rather than hand back a silent file without comment. The planning
    /// stage is where it is said, so the warning arrives before the render.
    #[test]
    fn a_composition_rendered_without_its_audio_warns() {
        use ravel_core::composition::AudioSource;

        let mut composition = comp(1, "Main");
        let mut layer = Layer::new(LayerId::new(1), "voice", Graph::new());
        layer.audio = Some(AudioSource {
            asset_id: AssetId::next(),
            stream_index: 0,
            ..Default::default()
        });
        composition = composition.add_layer(layer);
        composition = composition.add_layer(Layer::new(LayerId::new(2), "silent", Graph::new()));

        let document = Document::default().with_composition(composition);
        let mut args = args(Path::new("/tmp/out"));
        args.no_audio = true;
        let plan = plan_render(&args, &document, None, &available_encoders()).expect("plans");
        assert_eq!(
            plan.warnings,
            vec![Warning::AudioNotRendered {
                layers: 1,
                reason: NoAudio::NotAsked
            }]
        );
        assert!(plan.audio.is_none(), "and nothing is planned to be written");
    }

    /// Asked for, the same project plans a WAV named after the frame range —
    /// or, in a build that cannot decode, says that instead. Both are the
    /// "never silently silent" rule; which one applies is the build's.
    #[test]
    fn a_composition_with_audio_plans_a_companion_wav() {
        use ravel_core::composition::AudioSource;

        let mut composition = comp(1, "Main");
        let mut layer = Layer::new(LayerId::new(1), "voice", Graph::new());
        layer.audio = Some(AudioSource::new(AssetId::next(), 0));
        composition = composition.add_layer(layer);

        let document = Document::default().with_composition(composition);
        let mut args = args(Path::new("/tmp/out"));
        args.range = Some("10-19".parse().expect("range"));
        let plan = plan_render(&args, &document, None, &available_encoders()).expect("plans");

        if crate::audio::DECODE_AVAILABLE {
            let audio = plan.audio.as_ref().expect("an ffmpeg build renders it");
            assert_eq!(
                audio.path,
                Path::new("/tmp/out/frame_0010-0019.wav"),
                "the soundtrack is named after the same absolute frames as the pictures"
            );
            assert!(plan.warnings.is_empty());
        } else {
            assert!(plan.audio.is_none());
            assert_eq!(
                plan.warnings,
                vec![Warning::AudioNotRendered {
                    layers: 1,
                    reason: NoAudio::NoDecoder
                }]
            );
        }
    }

    /// A symlink pointing nowhere answers `false` to `Path::exists` and `Ok`
    /// to `symlink_metadata`, and `WavWriter::create` follows it — so the
    /// weaker question would call the soundtrack's path free and then truncate
    /// whatever the link names, outside the output directory entirely. The
    /// frames have always asked the stronger one.
    ///
    /// The plan is built and then given its soundtrack by hand, because a
    /// build without FFmpeg plans none at all and this refusal is not a
    /// property of the decoder.
    #[cfg(unix)]
    #[test]
    fn a_soundtrack_path_that_is_a_dangling_symlink_conflicts() {
        use crate::audio::{OUTPUT_CHANNELS, OUTPUT_SAMPLE_RATE};
        use ravel_audio::MixerConfig;

        let dir = tempfile::tempdir().expect("tempdir");
        let wav = dir.path().join("frame_0000-0049.wav");
        std::os::unix::fs::symlink(dir.path().join("elsewhere.wav"), &wav).expect("symlink");
        assert!(!wav.exists(), "the fixture points at nothing");

        let mut plan = plan_render(&args(dir.path()), &document(), None, &available_encoders())
            .expect("plans");
        assert!(plan.conflicts().is_empty(), "no frame is in the way");
        plan.audio = Some(AudioPlan {
            path: wav.clone(),
            config: MixerConfig {
                output_sample_rate: OUTPUT_SAMPLE_RATE,
                output_channels: OUTPUT_CHANNELS,
            },
        });
        assert_eq!(
            plan.conflicts(),
            vec![wav],
            "a link to nothing occupies the name a render would write"
        );
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

    // ---- the picture's warnings --------------------------------------------

    /// A `media` node pointing at `asset`.
    fn media_node(id: u64, asset: AssetId) -> ravel_core::graph::Node {
        use ravel_core::composition::{MEDIA_ASSET_PARAM_KEY, MEDIA_TYPE_KEYS};
        use ravel_core::graph::{Node, ParameterValue};
        use ravel_core::id::{DataTypeId, NodeId};

        Node::new(NodeId::new(id), MEDIA_TYPE_KEYS[0])
            .with_param(
                MEDIA_ASSET_PARAM_KEY,
                ParameterValue::String(asset.to_param_value()),
            )
            .with_output("out", DataTypeId::FRAME_BUFFER)
    }

    fn layer_with(id: u64, name: &str, node: ravel_core::graph::Node) -> Layer {
        Layer::new(LayerId::new(id), name, Graph::new().add_node(node).unwrap())
    }

    /// An asset the document has lost, and one whose path resolves nowhere,
    /// are both offline — one row each, whatever the layer count.
    #[test]
    fn an_offline_asset_is_reported_once_per_asset() {
        let comp = comp(1, "Main")
            .add_layer(layer_with(11, "Plate", media_node(100, AssetId::new(7))))
            .add_layer(layer_with(12, "Insert", media_node(101, AssetId::new(7))))
            .add_layer(layer_with(13, "Sky", media_node(102, AssetId::new(7))));
        let document = Document::default().with_composition(comp);

        let warnings = media_warnings(&document, CompId::new(1), &|_| None);
        assert_eq!(
            warnings,
            vec![Warning::MediaOffline {
                // No entry in the table: the reference itself is the name.
                asset: AssetId::new(7).to_string(),
                layers: vec!["Plate".into(), "Insert".into(), "Sky".into()],
            }],
            "one row per asset, carrying every layer that names it"
        );

        // An entry that exists but resolves to nothing is the same state.
        let mut entry = ravel_core::composition::MediaAssetEntry::from_absolute("/gone/plate.mov");
        entry.resolved = None;
        let document = document.with_media_asset_entry(AssetId::new(7), entry);
        assert_eq!(
            media_warnings(&document, CompId::new(1), &|_| None),
            vec![Warning::MediaOffline {
                asset: "plate".into(),
                layers: vec!["Plate".into(), "Insert".into(), "Sky".into()],
            }],
            "an asset the reader knows by name is named"
        );
    }

    /// An asset that resolves but cannot be opened is a different row with a
    /// different fix, and the reason travels with it.
    #[test]
    fn an_unreadable_asset_is_reported_separately() {
        let comp =
            comp(1, "Main").add_layer(layer_with(11, "Plate", media_node(100, AssetId::new(7))));
        let document = Document::default()
            .with_composition(comp)
            .with_media_asset(AssetId::new(7), "/tmp/plate.mov");

        assert_eq!(
            media_warnings(&document, CompId::new(1), &|_| Some("truncated".into())),
            vec![Warning::MediaUnreadable {
                asset: "plate".into(),
                layers: vec!["Plate".into()],
                detail: "truncated".into(),
            }]
        );
        // A build that cannot check says nothing rather than guessing: this
        // is the shape `probe_asset` takes without FFmpeg.
        assert!(media_warnings(&document, CompId::new(1), &|_| None).is_empty());
    }

    /// The real prober reports the ordinary failure — footage that moved
    /// after the project was saved — with no decoder involved.
    #[test]
    fn the_probe_reports_a_file_that_is_not_there() {
        let missing =
            ravel_core::composition::MediaAssetEntry::from_absolute("/nowhere/at/all/plate.mov");
        let detail = probe_asset(&missing).expect("a path with no file is not readable");
        assert!(
            detail.contains("plate.mov"),
            "the reason names the file: {detail}"
        );

        // A still that is there but is not an image. `read_image_frame` opens
        // it with the same decoder a container goes through, so a probe that
        // let this pass would promise a frame the render then falls back to
        // transparent for — the silence `HIGH-34` is about.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plate.png");
        std::fs::write(&path, b"not really a png").unwrap();
        let entry = ravel_core::composition::MediaAssetEntry::from_absolute(path);
        #[cfg(feature = "ffmpeg")]
        {
            let detail = probe_asset(&entry).expect("a file that is not an image is not readable");
            assert!(
                detail.contains("plate.png"),
                "the reason names the file: {detail}"
            );
        }
        // Without a decoder the build cannot tell, and says so by saying
        // nothing: "cannot be checked" is not "cannot be read".
        #[cfg(not(feature = "ffmpeg"))]
        assert_eq!(probe_asset(&entry), None);
    }

    /// Every shape that stops an identifier from standing still produces one
    /// row naming that shape, with the layer it happened in.
    #[test]
    fn a_driven_identifier_is_reported_per_parameter() {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;
        use ravel_core::animation::step::StepCurve;
        use ravel_core::composition::validate::{LAYER_REF_LAYER_PARAM, LAYER_REF_TYPE_KEY};
        use ravel_core::composition::{MEDIA_ASSET_PARAM_KEY, MEDIA_TYPE_KEYS};
        use ravel_core::graph::{Node, ParameterValue};
        use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};

        let mut curve = KeyframeCurve::new();
        curve.insert(0, 11.0, Interpolation::Linear);
        curve.insert(24, 12.0, Interpolation::Linear);
        let mut steps = StepCurve::new("7".to_string());
        steps.insert(0, "7".to_string());
        steps.insert(24, "8".to_string());

        // A wire into the target of one `layer.ref`, keyframes on another's.
        let wired = Graph::new()
            .add_node(Node::new(NodeId::new(100), "constant").with_output("v", DataTypeId::SCALAR))
            .unwrap()
            .add_node(
                Node::new(NodeId::new(101), LAYER_REF_TYPE_KEY)
                    .with_param(LAYER_REF_LAYER_PARAM, ParameterValue::Int(12))
                    .with_output("out", DataTypeId::FRAME_BUFFER),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(102), LAYER_REF_TYPE_KEY)
                    .with_param(
                        LAYER_REF_LAYER_PARAM,
                        ParameterValue::IntChannel(AnimationChannel::keyframes(curve)),
                    )
                    .with_output("out", DataTypeId::FRAME_BUFFER),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(103), MEDIA_TYPE_KEYS[0])
                    .with_param(MEDIA_ASSET_PARAM_KEY, ParameterValue::StringSteps(steps))
                    .with_output("out", DataTypeId::FRAME_BUFFER),
            )
            .unwrap()
            .expose_param_port(NodeId::new(101), LAYER_REF_LAYER_PARAM)
            .unwrap()
            .add_edge(
                EdgeId::new(104),
                NodeId::new(100),
                OutputPortIndex(0),
                NodeId::new(101),
                InputPortIndex(0),
            )
            .unwrap();
        let document = Document::default()
            .with_composition(comp(1, "Main").add_layer(Layer::new(
                LayerId::new(11),
                "Driven",
                wired,
            )))
            // Another composition's identifiers are not this render's problem.
            .with_composition(comp(2, "Other").add_layer(layer_with(
                21,
                "Elsewhere",
                Node::new(NodeId::new(200), LAYER_REF_TYPE_KEY).with_param(
                    LAYER_REF_LAYER_PARAM,
                    ParameterValue::IntChannel(AnimationChannel::keyframes({
                        let mut curve = KeyframeCurve::new();
                        curve.insert(0, 1.0, Interpolation::Linear);
                        curve.insert(9, 2.0, Interpolation::Linear);
                        curve
                    })),
                ),
            )));

        assert_eq!(
            identifier_warnings(&document, CompId::new(1)),
            vec![
                Warning::IdentifierNotStatic {
                    layer: "Driven".into(),
                    node: 101,
                    param: LAYER_REF_LAYER_PARAM.into(),
                    shape: "parameter port",
                },
                Warning::IdentifierNotStatic {
                    layer: "Driven".into(),
                    node: 102,
                    param: LAYER_REF_LAYER_PARAM.into(),
                    shape: "keyframes",
                },
                Warning::IdentifierNotStatic {
                    layer: "Driven".into(),
                    node: 103,
                    param: MEDIA_ASSET_PARAM_KEY.into(),
                    shape: "string steps",
                },
            ],
            "one row per parameter, each naming the shape that was ignored"
        );
    }

    /// The ids are stable and untranslated, and only the sentence is
    /// localized — the `note` event's contract.
    #[test]
    fn the_new_warnings_carry_stable_ids() {
        use crate::report::warning_text;

        // The sentences are only sentences once a catalog is loaded; the ids
        // never were.
        let locales = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/locales");
        let _ = ravel_i18n::init(&locales, "en");

        let ids: Vec<&str> = [
            Warning::MediaOffline {
                asset: "plate".into(),
                layers: vec!["A".into(), "B".into()],
            },
            Warning::MediaUnreadable {
                asset: "plate".into(),
                layers: vec!["A".into()],
                detail: "truncated".into(),
            },
            Warning::IdentifierNotStatic {
                layer: "Driven".into(),
                node: 7,
                param: "layer".into(),
                shape: "parameter port",
            },
        ]
        .iter()
        .map(|warning| warning_text(warning).0)
        .collect();
        assert_eq!(
            ids,
            vec!["media-offline", "media-unreadable", "identifier-not-static"]
        );

        // The sentence carries the values, and the shape word is the id-like
        // half that stays put.
        let (_, message) = warning_text(&Warning::MediaOffline {
            asset: "plate".into(),
            layers: vec!["A".into(), "B".into()],
        });
        assert!(
            message.contains("plate") && message.contains("A, B"),
            "{message}"
        );
        assert!(
            !message.contains("{asset}"),
            "every slot was filled: {message}"
        );
        let (_, message) = warning_text(&Warning::IdentifierNotStatic {
            layer: "Driven".into(),
            node: 7,
            param: "layer".into(),
            shape: "parameter port",
        });
        assert!(
            message.contains("parameter port") && message.contains('7'),
            "{message}"
        );
    }

    /// A document whose every reference resolves gains no rows, and a render
    /// with warnings is still a render — the exit code is untouched.
    #[test]
    fn a_document_that_resolves_says_nothing_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plate.mov");
        std::fs::write(&path, b"contents").unwrap();
        let document = Document::default()
            .with_composition(comp(1, "Main").add_layer(layer_with(
                11,
                "Plate",
                media_node(100, AssetId::new(7)),
            )))
            .with_media_asset(AssetId::new(7), path);
        let plan = plan_render(
            &args(Path::new("/tmp/out")),
            &document,
            None,
            &available_encoders(),
        )
        .expect("plans");
        assert!(
            plan.warnings.is_empty(),
            "a project with nothing wrong is as quiet as before: {:?}",
            plan.warnings
        );
    }

    /// The whole picture-side scan is part of planning, so the warnings are
    /// on the plan the renderer is handed — which is what puts them in the
    /// report and in `--json` alike.
    #[test]
    fn planning_collects_the_picture_warnings() {
        let document = Document::default().with_composition(comp(1, "Main").add_layer(layer_with(
            11,
            "Plate",
            media_node(100, AssetId::new(7)),
        )));
        let plan = plan_render(
            &args(Path::new("/tmp/out")),
            &document,
            None,
            &available_encoders(),
        )
        .expect("an offline reference is not a reason to refuse");
        assert_eq!(
            plan.warnings,
            vec![Warning::MediaOffline {
                asset: AssetId::new(7).to_string(),
                layers: vec!["Plate".into()],
            }]
        );
    }
}
