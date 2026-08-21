// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Media import path (REQ-UI-010, media-import plan unit 3).
//!
//! [`import_paths`] is the single entry point shared by File ▸ Import and OS
//! file drag-and-drop. Probing (FFmpeg `probe` and image-sequence detection)
//! runs on the background executor so the UI thread never blocks; the
//! resulting [`ProbedAsset`]s are applied to the document by
//! [`crate::project_state::ProjectState::import_media`] as **one**
//! `commit_document`, so a multi-file import is exactly one undo step.
//!
//! An import registers assets and nothing else: the file lands in the
//! MediaBin, not on a composition. Placing it is a separate action
//! ([`crate::project_state::ProjectState::add_asset_layers`]).
//!
//! The probe backends are injectable through [`MediaProber`] (the same idea
//! as the `media` node's `ReaderFactory`), which keeps the tests free of
//! real FFmpeg and real media files.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{App, PathPromptOptions};
use ravel_core::color::ColorSpace;
use ravel_core::composition::{AssetKind, AssetMetadata, AudioStreamMetadata};
use ravel_core::id::AssetId;
use ravel_core::media::{ImageSequenceInfo, MediaInfo, MediaResult, StreamInfo, VideoStreamInfo};
// Only the no-FFmpeg stub and the tests construct an error of their own; with
// the feature on, the real backend is the one that fails.
#[cfg(any(test, not(feature = "ffmpeg")))]
use ravel_core::media::MediaError;
use ravel_core::types::FrameRate;

use crate::project_state::ProjectStateHandle;

/// FFmpeg container probe, injectable for tests.
pub type ProbeFn = Arc<dyn Fn(&Path) -> MediaResult<MediaInfo> + Send + Sync>;

/// Image-sequence detection, injectable for tests.
pub type SequenceDetectFn = Arc<dyn Fn(&Path) -> MediaResult<ImageSequenceInfo> + Send + Sync>;

/// The two probe backends an import needs. `Clone` so it can travel into the
/// background task.
#[derive(Clone)]
pub struct MediaProber {
    probe: ProbeFn,
    detect_sequence: SequenceDetectFn,
}

impl MediaProber {
    pub fn new(probe: ProbeFn, detect_sequence: SequenceDetectFn) -> Self {
        Self {
            probe,
            detect_sequence,
        }
    }

    /// The production backends: FFmpeg probing plus on-disk sequence
    /// detection. Without the `ffmpeg` feature every container probe fails,
    /// so only stills and sequences import.
    #[cfg(feature = "ffmpeg")]
    pub fn ffmpeg() -> Self {
        Self::new(
            Arc::new(ravel_media::format::probe),
            Arc::new(ravel_media::image_seq::detect_sequence),
        )
    }

    /// Without FFmpeg there is no container backend; sequence detection is
    /// pure filesystem work and stays available.
    #[cfg(not(feature = "ffmpeg"))]
    pub fn ffmpeg() -> Self {
        Self::new(
            Arc::new(|_path| {
                Err(MediaError::Other(
                    "media import requires the `ffmpeg` feature of ravel-app".into(),
                ))
            }),
            Arc::new(ravel_media::image_seq::detect_sequence),
        )
    }
}

/// One successfully probed file, ready to become a
/// [`ravel_core::composition::MediaAssetEntry`].
///
/// `path` is the absolute on-disk location evaluation will use — for a
/// sequence it is the representative (first) frame, not necessarily the file
/// the user picked.
#[derive(Clone, Debug)]
pub struct ProbedAsset {
    pub path: PathBuf,
    pub kind: AssetKind,
    pub metadata: AssetMetadata,
}

/// One file that could not be imported, with the reason for the summary log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportFailure {
    pub path: PathBuf,
    pub reason: String,
}

/// What one import run did: the asset ids now in the document (new or
/// reused) and the files that were skipped. Nothing is placed on a
/// composition — see [`crate::project_state::ProjectState::import_media`].
#[derive(Clone, Debug, Default)]
pub struct ImportSummary {
    pub imported: Vec<(ravel_core::id::AssetId, PathBuf)>,
    pub skipped: Vec<ImportFailure>,
}

/// Import `paths` into the open project: probe off the UI thread, then apply
/// one document commit. This is the whole File ▸ Import / file-drop path;
/// cancelling the file dialog simply never calls it.
pub fn import_paths(paths: Vec<PathBuf>, cx: &mut App) {
    import_paths_with(paths, MediaProber::ffmpeg(), cx);
}

/// [`import_paths`] with explicit probe backends (tests and alternate
/// backends).
pub fn import_paths_with(paths: Vec<PathBuf>, prober: MediaProber, cx: &mut App) {
    if paths.is_empty() {
        return;
    }
    let Some(project) = cx
        .try_global::<ProjectStateHandle>()
        .and_then(|handle| handle.0.upgrade())
    else {
        tracing::warn!("media import: project state unavailable");
        return;
    };
    // A sequence carries no rate of its own: the active composition's frame
    // rate becomes its metadata default (unit 2 reads it during evaluation).
    let sequence_fps = project
        .read_with(cx, |project, cx| {
            project.active_composition(cx).map(|comp| comp.frame_rate)
        })
        .unwrap_or(FrameRate::new(30, 1));

    let probe = cx.background_executor().spawn(async move {
        paths
            .iter()
            .map(|path| probe_path(path, &prober, sequence_fps))
            .collect::<Vec<_>>()
    });
    cx.spawn(async move |cx| {
        let results = probe.await;
        let (assets, skipped): (Vec<_>, Vec<_>) = results.into_iter().partition(Result::is_ok);
        let assets: Vec<ProbedAsset> = assets.into_iter().map(Result::unwrap).collect();
        let skipped: Vec<ImportFailure> = skipped.into_iter().map(Result::unwrap_err).collect();
        for failure in &skipped {
            tracing::warn!(
                path = %failure.path.display(),
                reason = %failure.reason,
                "media import skipped"
            );
        }
        let total = assets.len() + skipped.len();
        project.update(cx, |project, cx| {
            let summary = project.import_media(assets, skipped, cx);
            tracing::info!(
                imported = summary.imported.len(),
                total,
                "media import: {} of {total} files imported",
                summary.imported.len(),
            );
        });
    })
    .detach();
}

/// Point an existing asset at another file (media-import plan unit 6): ask
/// for one, probe it, and replace the asset's reference — **one** undo step.
/// Cancelling the dialog is a no-op.
///
/// The dialog and the probe are the reason this is not a plain document edit:
/// neither may happen on the UI thread, so the edit lands in a task, exactly
/// as an import's does.
pub fn prompt_relink(asset_id: AssetId, cx: &mut App) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: None,
    });
    cx.spawn(async move |cx| match receiver.await {
        Ok(Ok(Some(paths))) => {
            if let Some(path) = paths.into_iter().next() {
                cx.update(|cx| relink_asset(asset_id, path, cx));
            }
        }
        // The dialog was cancelled (or the app is shutting down).
        Ok(Ok(None)) | Err(_) => {}
        Ok(Err(err)) => tracing::error!(%err, "relink dialog failed"),
    })
    .detach();
}

/// [`prompt_relink`] without the dialog: the file is already chosen.
pub fn relink_asset(asset_id: AssetId, path: PathBuf, cx: &mut App) {
    relink_asset_with(asset_id, path, MediaProber::ffmpeg(), cx);
}

/// [`relink_asset`] with explicit probe backends (tests and alternate
/// backends).
///
/// The new file is probed exactly as an imported one is, so the relinked asset
/// carries *its* kind and metadata rather than the old file's numbers. A file
/// the probe refuses relinks nothing: the same refusal that keeps it out of an
/// import keeps it from replacing a working reference.
pub fn relink_asset_with(asset_id: AssetId, path: PathBuf, prober: MediaProber, cx: &mut App) {
    let Some(project) = cx
        .try_global::<ProjectStateHandle>()
        .and_then(|handle| handle.0.upgrade())
    else {
        tracing::warn!("media relink: project state unavailable");
        return;
    };
    let sequence_fps = project
        .read_with(cx, |project, cx| {
            project.active_composition(cx).map(|comp| comp.frame_rate)
        })
        .unwrap_or(FrameRate::new(30, 1));

    let probe = cx
        .background_executor()
        .spawn(async move { probe_path(&path, &prober, sequence_fps) });
    cx.spawn(async move |cx| match probe.await {
        Ok(asset) => {
            project.update(cx, |project, cx| {
                project.relink_media_asset(asset_id, asset, cx);
            });
        }
        Err(failure) => {
            tracing::warn!(
                path = %failure.path.display(),
                reason = %failure.reason,
                "media relink refused"
            );
            project.update(cx, |project, cx| {
                project.report_media_failures(vec![failure], cx);
            });
        }
    })
    .detach();
}

/// Classify and probe one file. Pure apart from the injected backends and
/// `stat`, so it runs on the background executor.
///
/// Kind decision order: a detected multi-frame sequence wins; a still
/// extension is a single image; anything else must probe as a container or
/// the file is not imported.
pub fn probe_path(
    path: &Path,
    prober: &MediaProber,
    sequence_fps: FrameRate,
) -> Result<ProbedAsset, ImportFailure> {
    let fail = |reason: String| ImportFailure {
        path: path.to_path_buf(),
        reason,
    };

    if let Ok(info) = (prober.detect_sequence)(path)
        && info.frame_count() > 1
    {
        // The asset points at the representative (first) frame; dedupe then
        // works no matter which frame of the sequence was dropped.
        let representative = info.frame_path(info.start_frame);
        let mut metadata = probe_metadata(&representative, prober);
        metadata.frame_rate = Some(sequence_fps);
        metadata.duration_secs = Some(info.frame_count() as f64 / sequence_fps.as_f64());
        metadata.audio_stream_count = 0;
        metadata.audio_streams.clear();
        metadata.file_size = (info.start_frame..=info.end_frame)
            .map(|frame| file_size(&info.frame_path(frame)))
            .sum();
        return Ok(ProbedAsset {
            path: representative,
            kind: AssetKind::Sequence {
                prefix: info.prefix,
                // `AssetKind::Sequence` carries the extension inside the
                // suffix (`sequence_frame_name` builds the whole file name).
                suffix: format!("{}.{}", info.suffix, info.format.extension()),
                padding: info.padding,
                start: info.start_frame,
                end: info.end_frame,
            },
            metadata,
        });
    }

    if AssetKind::infer_from_path(path) == AssetKind::Still {
        // Metadata is best-effort for stills: a decoder that
        // `read_image_frame` handles may still not probe cleanly, and the
        // media node does not need the metadata to decode.
        let mut metadata = probe_metadata(path, prober);
        metadata.file_size = file_size(path);
        return Ok(ProbedAsset {
            path: path.to_path_buf(),
            kind: AssetKind::Still,
            metadata,
        });
    }

    match (prober.probe)(path) {
        Ok(info) => {
            let mut metadata = metadata_from_info(&info);
            metadata.file_size = file_size(path);
            Ok(ProbedAsset {
                path: path.to_path_buf(),
                kind: AssetKind::Container,
                metadata,
            })
        }
        Err(err) => Err(fail(format!("probe failed: {err}"))),
    }
}

/// Metadata from probing `path`, with every field optional on failure.
fn probe_metadata(path: &Path, prober: &MediaProber) -> AssetMetadata {
    match (prober.probe)(path) {
        Ok(info) => metadata_from_info(&info),
        Err(err) => {
            tracing::debug!(path = %path.display(), %err, "metadata probe failed; importing without it");
            AssetMetadata::default()
        }
    }
}

/// Map a probed [`MediaInfo`] onto the persisted [`AssetMetadata`].
fn metadata_from_info(info: &MediaInfo) -> AssetMetadata {
    let video = info.first_video();
    // Container order, with each stream's own container index: that is the
    // number the decoder seeks by and the one `AudioSource::stream_index`
    // stores, so the stream picker can offer them without reopening the file.
    let audio_streams: Vec<AudioStreamMetadata> = info
        .streams
        .iter()
        .filter_map(|stream| match stream {
            StreamInfo::Audio(audio) => Some(AudioStreamMetadata {
                stream_index: audio.stream_index,
                codec: Some(audio.codec_name.clone()),
                sample_rate: audio.sample_rate,
                channels: audio.channels,
            }),
            _ => None,
        })
        .collect();
    AssetMetadata {
        width: video.map(|v| v.width),
        height: video.map(|v| v.height),
        frame_rate: video.map(|v| v.frame_rate),
        duration_secs: info
            .duration_secs
            .or_else(|| video.and_then(|v| v.duration_secs)),
        codec: video
            .map(|v| v.codec_name.clone())
            .or_else(|| info.first_audio().map(|a| a.codec_name.clone())),
        color_space: video.and_then(probed_color_space),
        audio_stream_count: audio_streams.len(),
        audio_streams,
        file_size: 0,
    }
}

/// The colour space the container declares, as a [`ColorSpace`] name.
///
/// Only combinations that have a name in `ColorSpace`'s vocabulary are
/// written: anything else — an undeclared field, or a pair Ravel has no
/// name for — stays `None` so the resolution order falls through to the
/// extension default instead of persisting a string
/// [`ColorSpace::from_name`] cannot read back
/// (`docs/specifications/color-management.md`, tier 2 of the input colour
/// space resolution).
fn probed_color_space(video: &VideoStreamInfo) -> Option<String> {
    let space = ColorSpace::new(video.color_primaries?, video.color_transfer?);
    space.name().map(str::to_owned)
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::media::{ImageFormat, VideoStreamInfo};

    fn failing_probe(message: &str) -> ProbeFn {
        let message = message.to_string();
        Arc::new(move |_path| Err(MediaError::Other(message.clone())))
    }

    fn container_info(width: u32, height: u32, fps: FrameRate, duration: Option<f64>) -> MediaInfo {
        MediaInfo {
            container: None,
            container_name: "fake".into(),
            streams: vec![
                StreamInfo::Video(VideoStreamInfo {
                    stream_index: 0,
                    codec: None,
                    codec_name: "fakecodec".into(),
                    width,
                    height,
                    frame_rate: fps,
                    frame_count: None,
                    duration_secs: duration,
                    pixel_format: "rgba".into(),
                    color_primaries: None,
                    color_transfer: None,
                    color_matrix: None,
                }),
                StreamInfo::Audio(ravel_core::media::AudioStreamInfo {
                    stream_index: 1,
                    codec: None,
                    codec_name: "aac".into(),
                    sample_rate: 48_000,
                    channels: 2,
                    sample_count: None,
                    duration_secs: duration,
                }),
            ],
            duration_secs: duration,
        }
    }

    fn no_sequence() -> SequenceDetectFn {
        Arc::new(|_path| Err(MediaError::Other("no numeric portion".into())))
    }

    /// A container that probes cleanly imports with the probe's metadata.
    #[test]
    fn container_carries_probed_metadata() {
        let prober = MediaProber::new(
            Arc::new(|_path| Ok(container_info(1920, 1080, FrameRate::new(24, 1), Some(2.5)))),
            no_sequence(),
        );
        let asset = probe_path(Path::new("/fake/clip.mov"), &prober, FrameRate::new(30, 1))
            .expect("container should import");
        assert_eq!(asset.kind, AssetKind::Container);
        assert_eq!(asset.metadata.width, Some(1920));
        assert_eq!(asset.metadata.height, Some(1080));
        assert_eq!(asset.metadata.frame_rate, Some(FrameRate::new(24, 1)));
        assert_eq!(asset.metadata.duration_secs, Some(2.5));
        assert_eq!(asset.metadata.codec.as_deref(), Some("fakecodec"));
        assert_eq!(asset.metadata.audio_stream_count, 1);
    }

    /// The probed audio streams are recorded with their **container** stream
    /// index, so the first audio stream of a muxed clip is stream 1 — the
    /// value an `AudioSource` has to carry to decode anything.
    #[test]
    fn container_records_its_audio_streams() {
        let prober = MediaProber::new(
            Arc::new(|_path| {
                let mut info = container_info(640, 480, FrameRate::new(30, 1), Some(1.0));
                info.streams
                    .push(StreamInfo::Audio(ravel_core::media::AudioStreamInfo {
                        stream_index: 2,
                        codec: None,
                        codec_name: "pcm_s16le".into(),
                        sample_rate: 44_100,
                        channels: 1,
                        sample_count: None,
                        duration_secs: Some(1.0),
                    }));
                Ok(info)
            }),
            no_sequence(),
        );
        let asset = probe_path(Path::new("/fake/clip.mov"), &prober, FrameRate::new(30, 1))
            .expect("container should import");
        assert_eq!(asset.metadata.audio_stream_count, 2);
        assert_eq!(
            asset.metadata.audio_streams,
            vec![
                AudioStreamMetadata {
                    stream_index: 1,
                    codec: Some("aac".into()),
                    sample_rate: 48_000,
                    channels: 2,
                },
                AudioStreamMetadata {
                    stream_index: 2,
                    codec: Some("pcm_s16le".into()),
                    sample_rate: 44_100,
                    channels: 1,
                },
            ]
        );
        assert_eq!(asset.metadata.first_audio_stream_index(), Some(1));
    }

    /// A silent container records no streams, so nothing binds an audio
    /// source to it.
    #[test]
    fn a_silent_container_has_no_audio_streams() {
        let prober = MediaProber::new(
            Arc::new(|_path| {
                Ok(MediaInfo {
                    container: None,
                    container_name: "fake".into(),
                    streams: vec![StreamInfo::Video(VideoStreamInfo {
                        stream_index: 0,
                        codec: None,
                        codec_name: "fakecodec".into(),
                        width: 640,
                        height: 480,
                        frame_rate: FrameRate::new(30, 1),
                        frame_count: None,
                        duration_secs: Some(1.0),
                        pixel_format: "rgba".into(),
                        color_primaries: None,
                        color_transfer: None,
                        color_matrix: None,
                    })],
                    duration_secs: Some(1.0),
                })
            }),
            no_sequence(),
        );
        let asset = probe_path(
            Path::new("/fake/silent.mov"),
            &prober,
            FrameRate::new(30, 1),
        )
        .expect("container should import");
        assert_eq!(asset.metadata.audio_stream_count, 0);
        assert!(asset.metadata.audio_streams.is_empty());
        assert!(!asset.metadata.has_audio());
    }

    /// The container's declared colour metadata lands in the persisted
    /// metadata as a `ColorSpace` name — tier 2 of the input colour space
    /// resolution (`MED-MED-07`).
    #[test]
    fn container_carries_probed_colour_space() {
        let cases: [(
            Option<ravel_core::color::Primaries>,
            Option<ravel_core::color::Transfer>,
            Option<&str>,
        ); 4] = [
            (
                Some(ravel_core::color::Primaries::Rec709),
                Some(ravel_core::color::Transfer::Rec709),
                Some("rec709"),
            ),
            (
                Some(ravel_core::color::Primaries::Rec709),
                Some(ravel_core::color::Transfer::Srgb),
                Some("srgb"),
            ),
            (
                Some(ravel_core::color::Primaries::Rec2020),
                Some(ravel_core::color::Transfer::Pq),
                Some("rec2020_pq"),
            ),
            // Undeclared on either axis: nothing is written, so the
            // resolution falls through to the extension default.
            (Some(ravel_core::color::Primaries::Rec709), None, None),
        ];
        for (primaries, transfer, expected) in cases {
            let prober = MediaProber::new(
                Arc::new(move |_path| {
                    let mut info = container_info(640, 480, FrameRate::new(30, 1), Some(1.0));
                    if let Some(StreamInfo::Video(video)) = info.streams.first_mut() {
                        video.color_primaries = primaries;
                        video.color_transfer = transfer;
                    }
                    Ok(info)
                }),
                no_sequence(),
            );
            let asset = probe_path(Path::new("/fake/clip.mov"), &prober, FrameRate::new(30, 1))
                .expect("container should import");
            assert_eq!(
                asset.metadata.color_space.as_deref(),
                expected,
                "primaries {primaries:?} / transfer {transfer:?}"
            );
        }
    }

    /// A probed pair Ravel has no name for (Rec.2020 primaries with the
    /// BT.709 OETF) is not persisted — guessing would be worse than the
    /// extension default it falls through to.
    #[test]
    fn unnameable_probed_colour_space_is_dropped() {
        let prober = MediaProber::new(
            Arc::new(move |_path| {
                let mut info = container_info(640, 480, FrameRate::new(30, 1), Some(1.0));
                if let Some(StreamInfo::Video(video)) = info.streams.first_mut() {
                    video.color_primaries = Some(ravel_core::color::Primaries::Rec2020);
                    video.color_transfer = Some(ravel_core::color::Transfer::Rec709);
                }
                Ok(info)
            }),
            no_sequence(),
        );
        let asset = probe_path(Path::new("/fake/clip.mov"), &prober, FrameRate::new(30, 1))
            .expect("container should import");
        assert_eq!(asset.metadata.color_space, None);
    }

    /// An image sequence never carries audio, whatever the representative
    /// frame's probe reported.
    #[test]
    fn a_sequence_clears_any_probed_audio() {
        let detect: SequenceDetectFn = Arc::new(|path| {
            Ok(ImageSequenceInfo {
                directory: path.parent().unwrap().to_path_buf(),
                prefix: "f_".into(),
                suffix: "".into(),
                format: ImageFormat::Png,
                start_frame: 1,
                end_frame: 10,
                padding: 4,
            })
        });
        // A probe that (nonsensically) reports audio must not leak into the
        // sequence's metadata.
        let prober = MediaProber::new(
            Arc::new(|_path| Ok(container_info(64, 64, FrameRate::new(24, 1), Some(1.0)))),
            detect,
        );
        let asset = probe_path(
            Path::new("/fake/seq/f_0001.png"),
            &prober,
            FrameRate::new(24, 1),
        )
        .expect("sequences import");
        assert!(asset.metadata.audio_streams.is_empty());
        assert!(!asset.metadata.has_audio());
    }

    /// A file that neither sequences nor is a still and fails to probe is
    /// not imported — the failure carries the reason for the summary.
    #[test]
    fn failed_probe_skips_the_file() {
        let prober = MediaProber::new(failing_probe("cannot open"), no_sequence());
        let failure = probe_path(
            Path::new("/fake/broken.mov"),
            &prober,
            FrameRate::new(30, 1),
        )
        .expect_err("unprobed containers are skipped");
        assert_eq!(failure.path, PathBuf::from("/fake/broken.mov"));
        assert!(failure.reason.contains("cannot open"));
    }

    /// A still extension imports as `Still` even when the metadata probe
    /// fails — the media node decodes stills without probed metadata.
    #[test]
    fn still_imports_without_probed_metadata() {
        let prober = MediaProber::new(failing_probe("no ffmpeg"), no_sequence());
        let asset = probe_path(Path::new("/fake/plate.png"), &prober, FrameRate::new(30, 1))
            .expect("stills import best-effort");
        assert_eq!(asset.kind, AssetKind::Still);
        assert_eq!(asset.metadata.width, None);
    }

    /// A detected sequence maps its range onto `AssetKind::Sequence`, takes
    /// the composition frame rate as its own, and derives a duration from
    /// the frame count. The asset path moves to the first frame.
    #[test]
    fn sequence_maps_detection_result_onto_the_asset_kind() {
        let detect: SequenceDetectFn = Arc::new(|path| {
            Ok(ImageSequenceInfo {
                directory: path.parent().unwrap().to_path_buf(),
                prefix: "f_".into(),
                suffix: "".into(),
                format: ImageFormat::Png,
                start_frame: 1,
                end_frame: 48,
                padding: 4,
            })
        });
        let prober = MediaProber::new(failing_probe("no ffmpeg"), detect);
        let asset = probe_path(
            Path::new("/fake/seq/f_0024.png"),
            &prober,
            FrameRate::new(24, 1),
        )
        .expect("sequences import");
        assert_eq!(
            asset.kind,
            AssetKind::Sequence {
                prefix: "f_".into(),
                suffix: ".png".into(),
                padding: 4,
                start: 1,
                end: 48,
            }
        );
        assert_eq!(asset.path, PathBuf::from("/fake/seq/f_0001.png"));
        assert_eq!(asset.metadata.frame_rate, Some(FrameRate::new(24, 1)));
        assert_eq!(asset.metadata.duration_secs, Some(2.0));
        assert_eq!(asset.metadata.audio_stream_count, 0);
    }

    /// A lone numbered image is not a sequence — it imports as a still.
    #[test]
    fn single_frame_detection_is_a_still() {
        let detect: SequenceDetectFn = Arc::new(|path| {
            Ok(ImageSequenceInfo {
                directory: path.parent().unwrap().to_path_buf(),
                prefix: "f_".into(),
                suffix: "".into(),
                format: ImageFormat::Png,
                start_frame: 1,
                end_frame: 1,
                padding: 4,
            })
        });
        let prober = MediaProber::new(failing_probe("no ffmpeg"), detect);
        let asset = probe_path(
            Path::new("/fake/f_0001.png"),
            &prober,
            FrameRate::new(30, 1),
        )
        .expect("a single numbered frame imports as a still");
        assert_eq!(asset.kind, AssetKind::Still);
    }
}
