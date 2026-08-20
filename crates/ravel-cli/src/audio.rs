// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The soundtrack half of a render.
//!
//! An image sequence has nowhere to put sound, so a render whose composition
//! has audio layers writes a WAV beside the frames, covering exactly the same
//! frame range (`docs/implementation/render-export-plan.md`, unit 4). The mix
//! itself is [`ravel_audio::offline::mix_range`] — the same mixer, the same
//! document → track mapping, and the same fade and gain evaluation the
//! application plays through. Nothing about the sound is decided here except
//! where it goes and what the user is told.
//!
//! # Why the sound is written before the picture
//!
//! Two reasons, both about failing well:
//!
//! * **A warning has to arrive in time to act on.** "This project's
//!   voice-over is offline" is worth knowing before a render, not after an
//!   hour of it. Decoding first is what puts those warnings next to the ones
//!   planning already produced.
//! * **Cleanup is one known path.** If the render is then cancelled or fails,
//!   the soundtrack is one temporary file to remove, at a name computed before
//!   anything started. The reverse order would mean deleting however many
//!   frames the worker had written because the *audio* failed, which the
//!   encoder's own abort is not there to do.
//!
//! # Why it is not written where it belongs
//!
//! The mix goes to a temporary name **in the frames' own directory** and is
//! renamed into place only once the render has produced its pictures
//! ([`PendingAudio`]). Writing straight to `frame_0000-0047.wav` would mean
//! three different ways to leave a deliverable nobody can use:
//!
//! * a render that fails **before** a frame — no GPU adapter is the ordinary
//!   case — leaving a soundtrack for pictures that do not exist;
//! * `--overwrite` truncating the previous render's audio and then being
//!   interrupted, so neither the old sound nor the new one survives;
//! * `WavWriter::finish` dying partway through its length fields, leaving a
//!   WAV that claims to be complete under the name a finished one would have.
//!
//! One `rename` after the frames answers all three: nothing appears at the
//! real name until there is a render to go with it. The temporary file has to
//! share the directory for that — `rename` is atomic within a filesystem and
//! a copy across two.
//!
//! # The muxing that is not here
//!
//! When a container writer exists, its audio comes from the same
//! [`mix_range`](ravel_audio::offline::mix_range) call: the
//! [`AudioBuffer`](ravel_core::types::AudioBuffer) it returns is already what
//! [`MediaWriter::write_audio_chunk`](ravel_core::media::MediaWriter::write_audio_chunk)
//! takes. What changes is this module's last step — `WavWriter` instead of
//! the container — and nothing before it. Video containers are refused
//! earlier (`CliError::CodecNoWriter`), so that branch has no code yet rather
//! than dead code.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ravel_audio::MixerConfig;
use ravel_core::composition::Document;
use ravel_core::id::CompId;
use ravel_media::encode::WavWriter;

use crate::error::CliError;
use crate::plan::{RenderPlan, Warning};
use crate::report::Reporter;

/// Whether this build can decode an audio asset at all.
///
/// Decoding needs FFmpeg, which is an optional feature. A build without it
/// cannot produce a soundtrack, and the honest response is to say so once —
/// not to write a silent WAV, and not to report every layer separately for a
/// reason that has nothing to do with the project.
pub const DECODE_AVAILABLE: bool = cfg!(feature = "ffmpeg");

/// What the render's sound is, and where it goes.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioPlan {
    /// The WAV to write, named after the absolute frame range.
    pub path: PathBuf,
    /// Rate and channel count of the mix.
    pub config: MixerConfig,
}

/// The delivery format for a render's audio.
///
/// Fixed rather than exposed as flags: there is no device to ask what the
/// host prefers, and 48 kHz stereo is what every container and every editing
/// tool takes without comment. Assets at another rate are converted on the
/// way in, which is the conversion `MED-MED-03`'s delay compensation exists
/// to keep aligned.
pub const OUTPUT_SAMPLE_RATE: u32 = 48_000;
/// Channels in the delivered mix. See [`OUTPUT_SAMPLE_RATE`].
pub const OUTPUT_CHANNELS: u32 = 2;

/// Decide whether this render has a soundtrack, and what to say if it does
/// not.
///
/// Called from [`plan_render`](crate::plan::plan_render), so a render that
/// will not carry the project's sound says so **before** it starts — and,
/// because the answer is a function of the arguments and the document alone,
/// so does `EXPORT-7`'s interactive mode after every answer.
pub fn plan_audio(
    document: &Document,
    comp: CompId,
    output: &ravel_core::media::encode::ImageSequenceOutput,
    range: std::ops::Range<u64>,
    requested: bool,
) -> (Option<AudioPlan>, Vec<Warning>) {
    let Some(layers) = audio_layers(document, comp) else {
        // Nothing to render and nothing to say: a picture-only project gets a
        // picture-only deliverable, which is what it is.
        return (None, Vec::new());
    };
    if !requested {
        return (
            None,
            vec![Warning::AudioNotRendered {
                layers,
                reason: NoAudio::NotAsked,
            }],
        );
    }
    if !DECODE_AVAILABLE {
        return (
            None,
            vec![Warning::AudioNotRendered {
                layers,
                reason: NoAudio::NoDecoder,
            }],
        );
    }
    (
        Some(AudioPlan {
            path: output.audio_path(range),
            config: MixerConfig {
                output_sample_rate: OUTPUT_SAMPLE_RATE,
                output_channels: OUTPUT_CHANNELS,
            },
        }),
        Vec::new(),
    )
}

/// Why a project with sound is being rendered without it.
///
/// Two different situations for the reader: one they chose, one their build
/// imposed. Both keep the `audio-not-rendered` identifier, because what a
/// script needs to know — this deliverable is silent — is the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoAudio {
    /// `--no-audio`, or the absence of `--audio`.
    NotAsked,
    /// This build has no FFmpeg, so nothing can be decoded.
    NoDecoder,
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

/// Mix the plan's range and write it, reporting every source that could not be
/// loaded.
///
/// Returns the finished mix as a [`PendingAudio`] — written, but not yet at
/// the name it is for — or `None` when the plan has no audio. A source that
/// cannot be decoded — offline, relinked away, past
/// [`MAX_DECODE_BYTES`](ravel_audio::mixdown::MAX_DECODE_BYTES) — is a
/// **warning**, not a failure: the picture is still worth having and the mix
/// is still the right length, so the deliverable stays in sync. What must not
/// happen is that it becomes silence without a word, which is what these
/// notes prevent.
pub fn render_audio(
    plan: &RenderPlan,
    reporter: &mut dyn Reporter,
) -> Result<Option<PendingAudio>, CliError> {
    let Some(audio) = &plan.audio else {
        return Ok(None);
    };

    let Some(mix) = ravel_audio::offline::mix_range(
        &plan.document,
        plan.comp,
        plan.range.clone(),
        &audio.config,
    ) else {
        // `plan_audio` only produces a plan for a composition with audio
        // layers, and both read the same document; this is unreachable rather
        // than a case with a behaviour.
        return Ok(None);
    };

    for skipped in &mix.skipped {
        let warning = Warning::AudioSourceSkipped {
            layer: plan
                .document
                .get_composition(plan.comp)
                .and_then(|comp| comp.layers.iter().find(|l| l.id == skipped.layer_id))
                .map(|layer| layer.name.clone())
                .unwrap_or_else(|| skipped.layer_id.raw().to_string()),
            // The name the user knows the asset by. An id the document has
            // lost has none, and the same rule as the layer above applies:
            // fall back to the reference itself rather than say nothing.
            asset: plan
                .document
                .get_media_asset(skipped.asset_id)
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| skipped.asset_id.to_string()),
            detail: skipped.reason.clone(),
        };
        let (id, message) = crate::report::warning_text(&warning);
        reporter.note(id, &message);
    }

    // The frames' own directory, which the image encoder creates in `begin` —
    // and this runs first, so it may not exist yet.
    if let Some(parent) = audio.path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            CliError::Encode(format!("cannot create {}: {error}", parent.display()))
        })?;
    }

    // Everything from here writes the *temporary* name. The real one is not
    // opened, truncated or created at any point before the rename, which is
    // what leaves an existing soundtrack intact until there is a new render to
    // replace it with. A `WavWriter` that fails or is dropped unfinished
    // removes its own file, so this stretch needs no cleanup of its own.
    let temporary = temporary_path(&audio.path);
    let mut writer = WavWriter::create(&temporary, mix.buffer.sample_rate, mix.buffer.channels)
        .map_err(|error| CliError::Encode(error.to_string()))?;
    writer
        .write_samples(&mix.buffer.data)
        .map_err(|error| CliError::Encode(error.to_string()))?;
    writer
        .finish()
        .map_err(|error| CliError::Encode(error.to_string()))?;

    Ok(Some(PendingAudio {
        destination: audio.path.clone(),
        temporary,
    }))
}

/// A finished mix, written beside where it belongs and waiting for the render
/// to earn it.
///
/// **Dropping this removes the file.** That is the whole cleanup path for a
/// render that fails, is cancelled, or never starts: the sound is written
/// first, so every one of those exits passes through this drop, and none of
/// them can reach the name the deliverable is under.
/// [`publish`](Self::publish) is the only thing that does.
pub struct PendingAudio {
    destination: PathBuf,
    /// The file actually written. Emptied by [`publish`](Self::publish), so
    /// [`Drop`] knows whether there is still anything to take back.
    temporary: PathBuf,
}

impl PendingAudio {
    /// Where the soundtrack will go once the frames are there.
    pub fn destination(&self) -> &Path {
        &self.destination
    }

    /// Put the mix at its real name, and return that name.
    ///
    /// Called after the frames, so the rename is the moment the deliverable
    /// becomes complete — atomically, because the temporary file shares the
    /// destination's directory. A destination that already exists is replaced
    /// (the render asked for `--overwrite`, or nothing would have got this
    /// far); a destination that is a symlink is replaced by the file, which is
    /// the same refusal-or-replacement the frames get rather than a write
    /// through to wherever the link pointed.
    pub fn publish(mut self) -> Result<PathBuf, CliError> {
        std::fs::rename(&self.temporary, &self.destination).map_err(|error| {
            CliError::Encode(format!(
                "cannot put the soundtrack at {}: {error}",
                self.destination.display()
            ))
        })?;
        // Published: there is no temporary file left to take back, and the
        // drop that follows must leave the deliverable alone.
        self.temporary = PathBuf::new();
        Ok(self.destination.clone())
    }
}

impl Drop for PendingAudio {
    fn drop(&mut self) {
        if self.temporary.as_os_str().is_empty() {
            return;
        }
        if let Err(error) = std::fs::remove_file(&self.temporary)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.temporary.display(),
                %error,
                "could not remove the unfinished render's audio"
            );
        }
    }
}

/// The name a mix is written under until the render has earned the real one.
///
/// **Beside the destination, never in a system temporary directory.**
/// Publication is a `rename`, which is atomic within one filesystem and a
/// copy-then-delete across two — and a copy that fails halfway is exactly the
/// half-written deliverable this design exists to rule out.
///
/// The leading dot keeps it out of a listing; the process id and the serial
/// keep two renders — the split `--range` slices a render farm runs into one
/// directory — from writing to one another's file.
fn temporary_path(destination: &Path) -> PathBuf {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let name = destination
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
    destination.with_file_name(format!(".{name}.{}-{serial}.part", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::{AudioSource, Composition, Layer};
    use ravel_core::graph::Graph;
    use ravel_core::id::{AssetId, LayerId};
    use ravel_core::media::encode::{ImageSequenceOutput, PngDepth, SequenceCodec};
    use ravel_core::types::FrameRate;

    fn output() -> ImageSequenceOutput {
        ImageSequenceOutput::new("/out", "frame_", "", SequenceCodec::Png(PngDepth::Eight), 4)
            .expect("valid name")
    }

    fn document(with_audio: bool) -> Document {
        let mut comp =
            Composition::new(CompId::new(1), "Main", (16, 16), FrameRate::new(30, 1), 90);
        let mut layer = Layer::new(LayerId::new(1), "voice", Graph::new());
        if with_audio {
            // The reference a document holds: the asset id's decimal spelling.
            layer.audio = Some(AudioSource::new(AssetId::next(), 0));
        }
        comp = comp.add_layer(layer);
        Document::default().with_composition(comp)
    }

    #[test]
    fn a_project_without_audio_neither_plans_nor_warns() {
        let (plan, warnings) = plan_audio(&document(false), CompId::new(1), &output(), 0..30, true);
        assert!(plan.is_none());
        assert!(
            warnings.is_empty(),
            "silence nobody asked about is not news"
        );
    }

    /// The completion condition: asking for a silent deliverable from a
    /// project that has sound must say so.
    #[test]
    fn declining_the_audio_of_a_project_that_has_some_warns() {
        let (plan, warnings) = plan_audio(&document(true), CompId::new(1), &output(), 0..30, false);
        assert!(plan.is_none());
        assert_eq!(
            warnings,
            vec![Warning::AudioNotRendered {
                layers: 1,
                reason: NoAudio::NotAsked
            }]
        );
    }

    /// The same warning, for the other reason a render comes out silent.
    #[test]
    fn a_build_that_cannot_decode_warns_rather_than_writing_silence() {
        let (plan, warnings) = plan_audio(&document(true), CompId::new(1), &output(), 0..30, true);
        if DECODE_AVAILABLE {
            let plan = plan.expect("an ffmpeg build renders the audio");
            assert_eq!(plan.path, PathBuf::from("/out/frame_0000-0029.wav"));
            assert_eq!(plan.config.output_sample_rate, OUTPUT_SAMPLE_RATE);
            assert!(warnings.is_empty());
        } else {
            assert!(plan.is_none());
            assert_eq!(
                warnings,
                vec![Warning::AudioNotRendered {
                    layers: 1,
                    reason: NoAudio::NoDecoder
                }]
            );
        }
    }
}
