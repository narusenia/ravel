// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Rendering a frame range's soundtrack in one pass
//! (`docs/implementation/render-export-plan.md`, unit 4).
//!
//! Playback asks the [`Mixer`] for the next few milliseconds, over and over,
//! from a device callback. A render asks it once, for a whole range, from
//! whatever thread it likes — [`Mixer::mix`] takes `&self` and knows nothing
//! about clocks, so the offline path is the same mixer with a different
//! caller, not a second implementation.
//!
//! ```text
//! Document ─▶ AudioMixdown::desired_tracks ─▶ decode ─▶ Mixer ─▶ AudioBuffer
//!             (mixdown.rs, shared with playback)                     │
//!                                       ┌───────────────────────────┘
//!                            WAV beside an image sequence,
//!                            or MediaWriter::write_audio_chunk
//!                            once a container writer exists
//! ```
//!
//! # Why the result is an `AudioBuffer`
//!
//! [`AudioBuffer`] is the argument type of
//! [`MediaWriter::write_audio_chunk`](ravel_core::media::MediaWriter::write_audio_chunk),
//! which is how a container gets its audio stream. Handing back that type
//! rather than a bare `Vec<f32>` **is** the muxing wiring unit 4 asks for:
//! when a video `Encoder` appears, the soundtrack it needs is already in the
//! currency it takes, and nothing here changes. Today the only consumer
//! writes it to a WAV instead, because an image sequence has nowhere to put
//! sound.
//!
//! # Why the range is converted at its boundaries
//!
//! The sample range comes from converting `range.start` and `range.end`
//! independently, never from converting the length. Frame *n* therefore lands
//! on the same output sample whichever render produced it, which is what
//! makes the concatenation of `--range 0-49` and `--range 50-99` identical to
//! `--range 0-99` — the split guarantee REQ-RENDER-005 rests on, for sound as
//! well as picture.
//!
//! # What is not decided here
//!
//! The output rate and channel count arrive as a [`MixerConfig`]. This module
//! has no device to ask and does not invent a default: the front end that
//! knows what it is delivering says.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use ravel_core::composition::Document;
use ravel_core::id::{AssetId, CompId, LayerId};
use ravel_core::types::AudioBuffer;

use crate::mixdown::{self, AudioMixdown, CacheKey, DecodedAudio};
use crate::mixer::{Mixer, MixerConfig};

/// One audio-carrying layer that contributed nothing, and why.
///
/// Never a hard failure: a project whose voice-over went missing should still
/// render, and the caller is expected to say so out loud rather than hand
/// back a quietly incomplete mix. An asset over
/// [`MAX_DECODE_BYTES`](mixdown::MAX_DECODE_BYTES) arrives here too, which is
/// what makes "the file was too big to load" visible instead of silent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedSource {
    /// The layer whose sound is missing from the mix.
    pub layer_id: LayerId,
    /// The asset it names, whether or not the document still has it.
    ///
    /// An identity, not a name: the caller resolves it against the document
    /// when it wants something to show the user, because an id the document
    /// has lost has no name left to resolve.
    pub asset_id: AssetId,
    /// Diagnostic English, for the caller to wrap in its own sentence.
    pub reason: String,
}

/// A rendered range of a composition's soundtrack.
#[derive(Clone, Debug)]
pub struct RangeMixdown {
    /// Interleaved samples at the requested rate and channel count, covering
    /// exactly the frame range asked for.
    pub buffer: AudioBuffer,
    /// The absolute output-timeline sample frame `buffer` begins at — the
    /// composition's frame 0 is sample 0, so a `--range 100-199` render
    /// starts well past it. Recorded so a caller placing this in a container
    /// (or checking that two split renders abut) does not have to redo the
    /// conversion.
    pub start_sample: u64,
    /// Layers whose sound could not be loaded. Empty on a clean render.
    pub skipped: Vec<SkippedSource>,
}

impl RangeMixdown {
    /// Number of per-channel sample frames in [`buffer`](Self::buffer).
    pub fn frame_count(&self) -> u64 {
        if self.buffer.channels == 0 {
            return 0;
        }
        (self.buffer.data.len() / self.buffer.channels as usize) as u64
    }
}

/// Render the audio of `comp`'s half-open frame `range` at `config`'s rate.
///
/// Returns `None` when the composition has no audio-carrying layers at all —
/// there is no soundtrack to deliver, as distinct from a soundtrack that came
/// out silent, which returns `Some` with the reasons in
/// [`skipped`](RangeMixdown::skipped).
///
/// Decoding is full-length and memory-resident (decision 8 of
/// `audio-plan.md`), and every asset is decoded at most once however many
/// layers use it.
pub fn mix_range(
    document: &Document,
    comp: CompId,
    range: Range<u64>,
    config: &MixerConfig,
) -> Option<RangeMixdown> {
    let composition = document.get_composition(comp)?;
    let rate = config.output_sample_rate;
    let specs = AudioMixdown::desired_tracks(composition, rate);
    if specs.is_empty() {
        return None;
    }

    let fps = composition.frame_rate;
    // Both ends converted independently: see the module docs.
    let start_sample = mixdown::comp_frames_to_rate(range.start, fps, rate);
    let end_sample = mixdown::comp_frames_to_rate(range.end, fps, rate);
    let frame_count = end_sample.saturating_sub(start_sample);

    let mut mixer = Mixer::new(config.clone());
    let mut decoded: HashMap<CacheKey, Arc<DecodedAudio>> = HashMap::new();
    let mut skipped = Vec::new();

    for spec in &specs {
        let key = spec.cache_key();
        let audio = match decoded.get(&key) {
            Some(audio) => audio.clone(),
            None => match prepare(document, spec.asset_id, spec.stream_index, rate) {
                Ok(audio) => {
                    let audio = Arc::new(audio);
                    decoded.insert(key, audio.clone());
                    audio
                }
                Err(reason) => {
                    skipped.push(SkippedSource {
                        layer_id: spec.layer_id,
                        asset_id: spec.asset_id,
                        reason,
                    });
                    continue;
                }
            },
        };
        if let Some(track) = AudioMixdown::build_track(spec, &audio, fps, rate) {
            mixer.set_track(track);
        }
    }

    // `usize` on a 32-bit host cannot address a range this long, and neither
    // could the buffer it would produce; an empty mix is the honest answer.
    let (offset, count) = match (usize::try_from(start_sample), usize::try_from(frame_count)) {
        (Ok(offset), Ok(count)) => (offset, count),
        _ => (0, 0),
    };
    let samples = mixer.mix(offset, count);

    Some(RangeMixdown {
        buffer: AudioBuffer::new(rate, config.output_channels, samples),
        start_sample,
        skipped,
    })
}

/// Decode one asset's stream and bring it to the output rate.
///
/// The `Err` is the sentence the caller reports; every path that cannot
/// produce audio — an asset the document lost, one whose file is offline, one
/// past the decode cap, a build with no decoder — comes back through it, so
/// none of them can turn into silence without a word.
///
/// The sentence names the asset the way the user does, by its display name,
/// which is why this takes the whole `document` and not just the entry: an id
/// the document has lost has no name to show, and only there does the
/// sentence fall back to the id — as a reference that resolves to nothing,
/// not as a number the reader is expected to recognise.
fn prepare(
    document: &Document,
    asset_id: AssetId,
    stream_index: usize,
    output_rate: u32,
) -> Result<DecodedAudio, String> {
    let entry = document.get_media_asset(asset_id).ok_or_else(|| {
        format!("the project contains no media asset for this reference ({asset_id})")
    })?;
    let path = entry
        .resolved
        .as_ref()
        .ok_or_else(|| format!("the media asset {:?} is offline", entry.name))?;
    let audio =
        mixdown::decode_full_audio(path, stream_index).map_err(|error| format!("{error:#}"))?;
    mixdown::prepare_audio_at_rate(audio, output_rate).map_err(|error| format!("{error:#}"))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::{AudioSource, Composition, Layer, MediaAssetEntry};
    use ravel_core::graph::Graph;
    use ravel_core::types::FrameRate;

    const FPS_30: FrameRate = FrameRate { num: 30, den: 1 };

    fn config() -> MixerConfig {
        MixerConfig {
            output_sample_rate: 48_000,
            output_channels: 2,
        }
    }

    fn comp_with_audio(layers: Vec<Layer>) -> Document {
        let mut comp = Composition::new(CompId::new(1), "Main", (16, 16), FPS_30, 300);
        comp.layers = layers.into_iter().collect();
        Document::default().with_composition(comp)
    }

    /// A layer whose sound comes from `asset`. The reference is written the
    /// way the document writes it — the id's decimal spelling — so the test
    /// exercises the same parse the loader does.
    fn audio_layer(id: u64, asset: AssetId) -> Layer {
        let mut layer =
            Layer::new(LayerId::new(id), format!("layer {id}"), Graph::new()).with_time(0, 0, 300);
        layer.audio = Some(AudioSource::new(asset.to_param_value(), 0));
        layer
    }

    /// A composition with nothing to hear produces no mixdown at all, which
    /// is what keeps a picture-only render from writing a silent WAV.
    #[test]
    fn a_composition_without_audio_layers_has_no_soundtrack() {
        let document = comp_with_audio(vec![Layer::new(LayerId::new(1), "solid", Graph::new())]);
        assert!(mix_range(&document, CompId::new(1), 0..30, &config()).is_none());
    }

    #[test]
    fn an_unknown_composition_has_no_soundtrack() {
        let voice = AssetId::next();
        let document = comp_with_audio(vec![audio_layer(1, voice)]);
        assert!(mix_range(&document, CompId::new(9), 0..30, &config()).is_none());
    }

    /// The length of the buffer is the length of the picture: one second of
    /// 30 fps frames is one second of 48 kHz samples.
    #[test]
    fn the_buffer_covers_exactly_the_requested_frame_range() {
        let voice = AssetId::next();
        let document = comp_with_audio(vec![audio_layer(1, voice)]);
        let mix = mix_range(&document, CompId::new(1), 0..30, &config()).expect("has audio");
        assert_eq!(mix.frame_count(), 48_000);
        assert_eq!(mix.start_sample, 0);
        assert_eq!(mix.buffer.sample_rate, 48_000);
        assert_eq!(mix.buffer.channels, 2);
    }

    /// A range that does not start at zero starts at the sample its first
    /// frame does, not at zero with a shorter buffer.
    #[test]
    fn a_later_range_starts_at_its_own_sample() {
        let voice = AssetId::next();
        let document = comp_with_audio(vec![audio_layer(1, voice)]);
        let mix = mix_range(&document, CompId::new(1), 100..200, &config()).expect("has audio");
        assert_eq!(mix.start_sample, 100 * 48_000 / 30);
        assert_eq!(mix.frame_count(), 100 * 48_000 / 30);
    }

    /// Splitting a range must not lose or duplicate a sample: the two halves
    /// have to abut exactly where the whole one has its middle.
    #[test]
    fn split_ranges_abut_at_the_sample_the_whole_range_has_there() {
        let voice = AssetId::next();
        let document = comp_with_audio(vec![audio_layer(1, voice)]);
        let whole = mix_range(&document, CompId::new(1), 0..100, &config()).expect("audio");
        let first = mix_range(&document, CompId::new(1), 0..37, &config()).expect("audio");
        let second = mix_range(&document, CompId::new(1), 37..100, &config()).expect("audio");

        assert_eq!(first.start_sample, whole.start_sample);
        assert_eq!(
            first.start_sample + first.frame_count(),
            second.start_sample,
            "the halves abut with no gap and no overlap"
        );
        assert_eq!(
            first.frame_count() + second.frame_count(),
            whole.frame_count(),
            "and together they are exactly the whole"
        );
    }

    /// An asset the document no longer has is reported, not silently dropped.
    #[test]
    fn a_missing_asset_is_reported_rather_than_silently_dropped() {
        let voice = AssetId::next();
        let document = comp_with_audio(vec![audio_layer(1, voice)]);
        let mix = mix_range(&document, CompId::new(1), 0..30, &config()).expect("has audio");
        assert_eq!(mix.skipped.len(), 1);
        assert_eq!(mix.skipped[0].layer_id, LayerId::new(1));
        assert_eq!(mix.skipped[0].asset_id, voice);
        assert!(mix.skipped[0].reason.contains("no media asset"));
        // Still the right length, so the picture it accompanies still lines up.
        assert_eq!(mix.frame_count(), 48_000);
        assert!(mix.buffer.data.iter().all(|s| *s == 0.0), "and is silent");
    }

    /// An asset that is in the document but not on disk says *that*, because
    /// it is a different thing for the user to fix.
    #[test]
    fn an_offline_asset_says_so() {
        let voice = AssetId::next();
        let mut document = comp_with_audio(vec![audio_layer(1, voice)]);
        let mut entry = MediaAssetEntry::from_absolute("/nowhere/voice.wav");
        // `resolved` is what evaluation reads, and `None` is exactly what
        // "the file this names is not there" looks like after a load.
        entry.resolved = None;
        document.media_assets.insert(voice, entry);
        let mix = mix_range(&document, CompId::new(1), 0..30, &config()).expect("has audio");
        assert_eq!(mix.skipped.len(), 1);
        assert!(
            mix.skipped[0].reason.contains("offline"),
            "{:?}",
            mix.skipped
        );
    }

    /// Two layers on one asset must not decode it twice, and both have to be
    /// reported when it cannot be decoded at all.
    #[test]
    fn one_asset_shared_by_two_layers_is_reported_once_per_layer() {
        let voice = AssetId::next();
        let document = comp_with_audio(vec![audio_layer(1, voice), audio_layer(2, voice)]);
        let mix = mix_range(&document, CompId::new(1), 0..30, &config()).expect("has audio");
        assert_eq!(
            mix.skipped.len(),
            2,
            "each layer is missing its sound, and each is said"
        );
    }
}
