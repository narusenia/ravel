// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `media` — decoded media frames as a network source (REQ-LAYER-008).
//!
//! The node's `asset_id` parameter indexes the document's media asset table
//! ([`ravel_core::composition::MediaAssetEntry`]); the frame to decode is
//! derived from the layer-local time in **seconds** so media whose frame
//! rate differs from the composition maps correctly (REQ-LAYER-006):
//! `media_frame = floor(t · media_fps)`, clamped to the stream's last frame.
//!
//! One node type covers every [`AssetKind`]: containers decode through the
//! [`MediaReader`] abstraction, stills and image-sequence frames through an
//! injectable single-image reader. The default backends live in
//! `ravel-media` (enable the `ffmpeg` feature); tests inject synthetic
//! readers through [`MediaProcessor::with_reader_factory`] and
//! [`MediaProcessor::with_factories`].
//!
//! The type key was renamed from `video` to `media` when the kinds were
//! unified (`docs/implementation/media-import-plan.md`, decision 2).
//! Documents persisted with `type_key: "video"` are rewritten on load by
//! [`ravel_core::composition::Document::normalize_node_type_aliases`], and
//! the processor dispatch accepts both keys.
//!
//! An offline asset (`resolved == None`) or a failed decode never fails the
//! evaluation: the node yields a transparent frame at the evaluation
//! resolution and logs one warning per asset (decision 7). An unset or
//! unknown `asset_id` remains a hard error — not pointing at an asset is a
//! graph bug, not missing footage.

use ravel_core::composition::AssetKind;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::media::{MediaReader, MediaResult, VideoStreamInfo};
use ravel_core::types::{FrameBuffer, NodeData};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Opens a [`MediaReader`] for a path. Injectable for tests and alternate
/// backends.
pub type ReaderFactory = Arc<dyn Fn(&Path) -> MediaResult<Box<dyn MediaReader>> + Send + Sync>;

/// Reads one still image as an RGBA f32 frame. Injectable for tests; the
/// default backend is `ravel-media`'s single-image decoder (a one-frame
/// "video" to FFmpeg).
pub type ImageReaderFactory = Arc<dyn Fn(&Path) -> MediaResult<FrameBuffer> + Send + Sync>;

/// The frame to request from a media stream for layer-local time `t`
/// (seconds). Seconds-based mapping keeps differing frame rates aligned
/// (REQ-LAYER-006); a small epsilon absorbs the float error of `frame / fps`
/// round trips, and the result is clamped to the stream's last frame.
pub fn media_frame_for(t_seconds: f64, stream: &VideoStreamInfo) -> u64 {
    let fps = stream.frame_rate.as_f64();
    let frame = (t_seconds * fps + 1e-6).floor().max(0.0) as u64;
    match stream.frame_count {
        Some(count) if count > 0 => frame.min(count - 1),
        _ => frame,
    }
}

struct OpenReader {
    path: PathBuf,
    reader: Box<dyn MediaReader>,
}

struct CachedImage {
    path: PathBuf,
    frame: Arc<FrameBuffer>,
}

/// Decodes one media frame per evaluation, branching on the asset's
/// [`AssetKind`]. Open decoders and decoded images are cached and keyed by
/// the resolved path — never by parameter values — so `asset_id` edits only
/// require dirty marking. Both caches hold a single entry (`OpenReader`'s
/// "one open at a time" policy): enough for a still to decode once and for
/// a sequence to revisit the previous frame, without letting a whole
/// sequence accumulate in memory.
pub struct MediaProcessor {
    factory: ReaderFactory,
    image_factory: ImageReaderFactory,
    open: Mutex<Option<OpenReader>>,
    image: Mutex<Option<CachedImage>>,
    /// Asset ids already warned about. Offline assets and decode failures
    /// degrade to a transparent frame instead of failing, so without this
    /// set every frame of a broken clip would re-log the same warning.
    warned: Mutex<HashSet<String>>,
}

impl MediaProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self::with_factories(default_reader_factory(), default_image_reader_factory())
    }

    /// Inject only the container backend; stills and sequences keep the
    /// default single-image reader.
    pub fn with_reader_factory(factory: ReaderFactory) -> Self {
        Self::with_factories(factory, default_image_reader_factory())
    }

    pub fn with_factories(factory: ReaderFactory, image_factory: ImageReaderFactory) -> Self {
        Self {
            factory,
            image_factory,
            open: Mutex::new(None),
            image: Mutex::new(None),
            warned: Mutex::new(HashSet::new()),
        }
    }

    /// Log `detail` once per asset and yield a transparent frame at the
    /// evaluation resolution. An offline or undecodable asset must not fail
    /// the surrounding composition (`docs/implementation/media-import-plan.md`,
    /// decision 7); the warn-once set keeps per-frame evaluations from
    /// flooding the log.
    fn fallback_frame(
        &self,
        asset_id: &str,
        ctx: &EvalContext,
        detail: impl FnOnce() -> String,
    ) -> Arc<dyn NodeData> {
        let mut warned = self.warned.lock().expect("media warn lock poisoned");
        if warned.insert(asset_id.to_string()) {
            tracing::warn!("media: asset {asset_id:?}: {}", detail());
        }
        Arc::new(FrameBuffer::new_zeroed(ctx.resolution.0, ctx.resolution.1))
    }

    /// Decode one frame from a container, reusing the already-open reader
    /// while the resolved path is unchanged.
    fn decode_container_frame(
        &self,
        path: &Path,
        ctx: &EvalContext,
    ) -> anyhow::Result<FrameBuffer> {
        let mut open = self.open.lock().expect("media reader lock poisoned");
        if open.as_ref().is_none_or(|o| o.path != path) {
            let reader = (self.factory)(path)
                .map_err(|e| anyhow::anyhow!("media: failed to open {path:?}: {e}"))?;
            *open = Some(OpenReader {
                path: path.to_path_buf(),
                reader,
            });
        }
        // SAFETY of unwrap: populated just above.
        let open = open.as_mut().unwrap();

        let stream = open
            .reader
            .info()
            .first_video()
            .ok_or_else(|| anyhow::anyhow!("media: {:?} has no video stream", open.path))?
            .clone();
        let frame = media_frame_for(ctx.time, &stream);
        open.reader
            .decode_video_frame(stream.stream_index, frame)
            .map_err(|e| anyhow::anyhow!("media: decoding frame {frame} failed: {e}"))
    }

    /// Read a single image, returning the cached frame when this exact
    /// resolved path was the last one decoded. The cache key is the path on
    /// disk, so scrubbing back to a sequence frame that is still cached
    /// does not re-decode it either.
    fn decode_image(&self, path: &Path) -> MediaResult<Arc<FrameBuffer>> {
        let mut cached = self.image.lock().expect("media image cache lock poisoned");
        if let Some(hit) = cached.as_ref().filter(|hit| hit.path == path) {
            return Ok(Arc::clone(&hit.frame));
        }
        let frame = Arc::new((self.image_factory)(path)?);
        *cached = Some(CachedImage {
            path: path.to_path_buf(),
            frame: Arc::clone(&frame),
        });
        Ok(frame)
    }
}

impl NodeProcessor for MediaProcessor {
    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let asset_id = params.str_or("asset_id", "");
        anyhow::ensure!(!asset_id.is_empty(), "media: asset_id is not set");

        let document = _scope
            .document()
            .ok_or_else(|| anyhow::anyhow!("media: no document set on the evaluator"))?;
        let asset = document
            .get_media_asset(asset_id)
            .ok_or_else(|| anyhow::anyhow!("media: unknown asset id {asset_id:?}"))?;
        // `resolved` is the only path evaluation may use: the persisted
        // `path` can be project-relative or variable-prefixed, and only the
        // host knows the project root that anchors it. `None` means the
        // asset is offline — degrade to transparent, never fail.
        let Some(path) = asset.resolved.as_ref() else {
            return Ok(self.fallback_frame(asset_id, ctx, || {
                format!(
                    "offline (unresolved path {}), transparent frame",
                    asset.path
                )
            }));
        };

        let decoded: anyhow::Result<Arc<FrameBuffer>> = match &asset.kind {
            AssetKind::Container => self.decode_container_frame(path, ctx).map(Arc::new),
            AssetKind::Still => self
                .decode_image(path)
                .map_err(|e| anyhow::anyhow!("media: decoding still {path:?} failed: {e}")),
            AssetKind::Sequence { start, end, .. } => {
                // A sequence carries no rate of its own: the probed metadata
                // wins, the composition rate is the fallback. The seconds-
                // based mapping mirrors containers (REQ-LAYER-006), clamped
                // into the sequence range like `frame_count` clamps a stream.
                let seq_fps = asset.metadata.frame_rate.unwrap_or(ctx.fps).as_f64();
                let offset = (ctx.time * seq_fps + 1e-6).floor().max(0.0) as u64;
                let index = start.saturating_add(offset).min(*end);
                // Clamped into `start..=end`, so a name always exists.
                let name = asset
                    .kind
                    .sequence_frame_name(index)
                    .expect("index clamped into the sequence range");
                // `resolved` points at the representative (first) frame; its
                // directory holds every frame of the sequence.
                let dir = path.parent().ok_or_else(|| {
                    anyhow::anyhow!("media: sequence frame {path:?} has no directory")
                })?;
                self.decode_image(&dir.join(name)).map_err(|e| {
                    anyhow::anyhow!("media: decoding sequence frame {index} failed: {e}")
                })
            }
        };
        match decoded {
            Ok(frame) => Ok(frame),
            Err(err) => {
                Ok(self.fallback_frame(asset_id, ctx, || format!("{err:#}, transparent frame")))
            }
        }
    }

    fn is_time_dependent(&self) -> bool {
        true
    }
}

/// FFmpeg-backed factory (requires the `ffmpeg` feature).
#[cfg(feature = "ffmpeg")]
fn default_reader_factory() -> ReaderFactory {
    Arc::new(|path| {
        ravel_media::decoder::FfmpegDecoder::open(path).map(|r| Box::new(r) as Box<dyn MediaReader>)
    })
}

/// Without the `ffmpeg` feature there is no decoding backend.
#[cfg(not(feature = "ffmpeg"))]
fn default_reader_factory() -> ReaderFactory {
    Arc::new(|_path| {
        Err(ravel_core::media::MediaError::Other(
            "media decoding requires the `ffmpeg` feature of ravel-nodes".into(),
        ))
    })
}

/// FFmpeg-backed single-image reader (requires the `ffmpeg` feature).
#[cfg(feature = "ffmpeg")]
fn default_image_reader_factory() -> ImageReaderFactory {
    Arc::new(ravel_media::image_seq::read_image_frame)
}

/// Without the `ffmpeg` feature there is no image decoding backend.
#[cfg(not(feature = "ffmpeg"))]
fn default_image_reader_factory() -> ImageReaderFactory {
    Arc::new(|_path| {
        Err(ravel_core::media::MediaError::Other(
            "image decoding requires the `ffmpeg` feature of ravel-nodes".into(),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::{Document, MediaAssetEntry};
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::media::{MediaError, MediaInfo, StreamInfo};
    use ravel_core::types::{AudioBuffer, FrameBuffer, FrameRate};

    /// Emits a solid frame whose red channel encodes the requested frame
    /// index (`frame / 1000`), recording nothing else.
    struct FakeReader {
        info: MediaInfo,
    }

    impl FakeReader {
        fn new(fps: FrameRate, frame_count: Option<u64>) -> Self {
            Self {
                info: MediaInfo {
                    container: None,
                    container_name: "fake".into(),
                    streams: vec![StreamInfo::Video(VideoStreamInfo {
                        stream_index: 0,
                        codec: None,
                        codec_name: "fake".into(),
                        width: 4,
                        height: 4,
                        frame_rate: fps,
                        frame_count,
                        duration_secs: None,
                        pixel_format: "rgba".into(),
                    })],
                    duration_secs: None,
                },
            }
        }
    }

    impl MediaReader for FakeReader {
        fn open(_path: &Path) -> MediaResult<Self> {
            Err(MediaError::Other("not used".into()))
        }

        fn info(&self) -> &MediaInfo {
            &self.info
        }

        fn decode_video_frame(
            &mut self,
            _stream_index: usize,
            frame_number: u64,
        ) -> MediaResult<FrameBuffer> {
            let value = frame_number as f32 / 1000.0;
            let mut data = Vec::with_capacity(4 * 4 * 4);
            for _ in 0..16 {
                data.extend_from_slice(&[value, 0.0, 0.0, 1.0]);
            }
            Ok(FrameBuffer::from_f32(4, 4, data))
        }

        fn decode_audio_chunk(
            &mut self,
            _stream_index: usize,
            _start_sample: u64,
            _sample_count: usize,
        ) -> MediaResult<AudioBuffer> {
            Err(MediaError::Other("no audio".into()))
        }
    }

    fn fake_factory(fps: FrameRate, frame_count: Option<u64>) -> ReaderFactory {
        Arc::new(move |_path| Ok(Box::new(FakeReader::new(fps, frame_count)) as Box<_>))
    }

    fn media_node(id: u64) -> Node {
        Node::new(NodeId::new(id), "media")
            .with_output("frame", DataTypeId::FRAME_BUFFER)
            .with_param("asset_id", ParameterValue::String("clip".into()))
    }

    fn decode_at(
        comp_fps: FrameRate,
        media_fps: FrameRate,
        frame_count: Option<u64>,
        comp_frame: u64,
    ) -> f32 {
        let node = media_node(1);
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.set_document(Arc::new(
            Document::default().with_media_asset("clip", "/fake/clip.mov"),
        ));
        ev.register(
            NodeId::new(1),
            Arc::new(MediaProcessor::with_reader_factory(fake_factory(
                media_fps,
                frame_count,
            ))),
        );
        let ctx = EvalContext::new(comp_frame, comp_fps, (4, 4));
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx).unwrap();
        out.downcast_ref::<FrameBuffer>().unwrap().as_f32()[0] * 1000.0
    }

    #[test]
    fn media_frame_maps_by_seconds_across_frame_rates() {
        // 30 fps comp frame 15 → t = 0.5 s → 24 fps media frame 12.
        let frame = decode_at(FrameRate::new(30, 1), FrameRate::new(24, 1), None, 15);
        assert!((frame - 12.0).abs() < 0.5, "got media frame {frame}");

        // 30 fps comp frame 30 → t = 1.0 s → 60 fps media frame 60.
        let frame = decode_at(FrameRate::new(30, 1), FrameRate::new(60, 1), None, 30);
        assert!((frame - 60.0).abs() < 0.5, "got media frame {frame}");
    }

    #[test]
    fn media_frame_clamps_to_stream_end() {
        // t = 2 s at 24 fps → frame 48, but the stream has 20 frames.
        let frame = decode_at(FrameRate::new(30, 1), FrameRate::new(24, 1), Some(20), 60);
        assert!((frame - 19.0).abs() < 0.5, "got media frame {frame}");
    }

    #[test]
    fn exact_frame_boundaries_do_not_drift() {
        // Same fps: every comp frame maps to the same media frame.
        for f in [0u64, 1, 7, 29, 30, 299] {
            let frame = decode_at(FrameRate::new(30, 1), FrameRate::new(30, 1), None, f);
            assert!((frame - f as f32).abs() < 0.5, "comp {f} → media {frame}");
        }
        // NTSC rates: 30000/1001 comp at frame 30 ≈ 1.001 s → 24000/1001
        // media frame 24 (exact by the shared 1001 denominator).
        let frame = decode_at(
            FrameRate::new(30000, 1001),
            FrameRate::new(24000, 1001),
            None,
            30,
        );
        assert!((frame - 24.0).abs() < 0.5, "got media frame {frame}");
    }

    #[test]
    fn missing_asset_is_an_error() {
        let node = media_node(1);
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.set_document(Arc::new(Document::default()));
        ev.register(
            NodeId::new(1),
            Arc::new(MediaProcessor::with_reader_factory(fake_factory(
                FrameRate::new(24, 1),
                None,
            ))),
        );
        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (4, 4));
        assert!(ev.evaluate(&graph, NodeId::new(1), &ctx).is_err());
    }

    /// An asset the host never resolved (a relative path in a project that
    /// has no root yet) must not reach the reader factory at all — the
    /// persisted path is not a filesystem path. Instead of failing, the
    /// node yields a transparent frame so the composite continues.
    #[test]
    fn unresolved_asset_never_reaches_the_reader() {
        use ravel_core::composition::{AssetKind, AssetMetadata, AssetPath, MediaAssetEntry};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let opens = Arc::new(AtomicUsize::new(0));
        let factory: ReaderFactory = {
            let opens = Arc::clone(&opens);
            Arc::new(move |_path| {
                opens.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(FakeReader::new(FrameRate::new(24, 1), None)) as Box<_>)
            })
        };

        let graph = Graph::new().add_node(media_node(1)).unwrap();
        let mut ev = Evaluator::new();
        ev.set_document(Arc::new(Document::default().with_media_asset_entry(
            "clip",
            MediaAssetEntry {
                path: AssetPath::Relative("./footage/clip.mov".into()),
                kind: AssetKind::Container,
                metadata: AssetMetadata::default(),
                resolved: None,
            },
        )));
        ev.register(
            NodeId::new(1),
            Arc::new(MediaProcessor::with_reader_factory(factory)),
        );

        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (4, 4));
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx).unwrap();
        let fb = out.downcast_ref::<FrameBuffer>().unwrap();
        assert_eq!((fb.width, fb.height), (4, 4));
        assert!(
            fb.as_f32().iter().all(|&c| c == 0.0),
            "offline assets degrade to a transparent frame"
        );
        assert_eq!(opens.load(Ordering::SeqCst), 0);
    }

    /// A decode failure (missing codec, truncated file, …) degrades the
    /// same way: transparent frame, no evaluation error.
    #[test]
    fn failed_decode_yields_a_transparent_frame_instead_of_failing() {
        use ravel_core::composition::{AssetKind, AssetMetadata, AssetPath, MediaAssetEntry};

        let factory: ReaderFactory = Arc::new(|_path| Err(MediaError::Other("cannot open".into())));
        let entry = MediaAssetEntry {
            path: AssetPath::Absolute(PathBuf::from("/fake/clip.mov")),
            kind: AssetKind::Container,
            metadata: AssetMetadata::default(),
            resolved: Some(PathBuf::from("/fake/clip.mov")),
        };
        let (mut ev, graph) = media_evaluator(MediaProcessor::with_reader_factory(factory), entry);

        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (4, 4));
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx).unwrap();
        let fb = out.downcast_ref::<FrameBuffer>().unwrap();
        assert!(
            fb.as_f32().iter().all(|&c| c == 0.0),
            "a failed decode degrades to a transparent frame"
        );
    }

    /// The same asset resolved to an absolute location decodes normally.
    #[test]
    fn resolved_asset_is_opened_at_its_resolved_path() {
        use ravel_core::composition::{AssetKind, AssetMetadata, AssetPath, MediaAssetEntry};
        use std::path::PathBuf;
        use std::sync::Mutex as StdMutex;

        let seen: Arc<StdMutex<Vec<PathBuf>>> = Arc::new(StdMutex::new(Vec::new()));
        let factory: ReaderFactory = {
            let seen = Arc::clone(&seen);
            Arc::new(move |path| {
                seen.lock().unwrap().push(path.to_path_buf());
                Ok(Box::new(FakeReader::new(FrameRate::new(24, 1), None)) as Box<_>)
            })
        };

        let graph = Graph::new().add_node(media_node(1)).unwrap();
        let mut ev = Evaluator::new();
        ev.set_document(Arc::new(Document::default().with_media_asset_entry(
            "clip",
            MediaAssetEntry {
                path: AssetPath::Relative("./footage/clip.mov".into()),
                kind: AssetKind::Container,
                metadata: AssetMetadata::default(),
                resolved: Some(PathBuf::from("/proj/footage/clip.mov")),
            },
        )));
        ev.register(
            NodeId::new(1),
            Arc::new(MediaProcessor::with_reader_factory(factory)),
        );

        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (4, 4));
        assert!(ev.evaluate(&graph, NodeId::new(1), &ctx).is_ok());
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [PathBuf::from("/proj/footage/clip.mov")]
        );
    }

    fn solid_image(value: f32) -> FrameBuffer {
        let mut data = Vec::with_capacity(4 * 4 * 4);
        for _ in 0..16 {
            data.extend_from_slice(&[value, 0.0, 0.0, 1.0]);
        }
        FrameBuffer::from_f32(4, 4, data)
    }

    /// One media node wired to a document holding `entry` as "clip",
    /// evaluated through `processor`.
    fn media_evaluator(processor: MediaProcessor, entry: MediaAssetEntry) -> (Evaluator, Graph) {
        let graph = Graph::new().add_node(media_node(1)).unwrap();
        let mut ev = Evaluator::new();
        ev.set_document(Arc::new(
            Document::default().with_media_asset_entry("clip", entry),
        ));
        ev.register(NodeId::new(1), Arc::new(processor));
        (ev, graph)
    }

    /// A still is decoded once; re-evaluation (here at other comp frames)
    /// serves the cached `Arc` without touching the image reader again.
    #[test]
    fn still_decodes_once_and_reuses_the_cached_frame() {
        use ravel_core::composition::{AssetMetadata, AssetPath, MediaAssetEntry};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let decodes = Arc::new(AtomicUsize::new(0));
        let image_factory: ImageReaderFactory = {
            let decodes = Arc::clone(&decodes);
            Arc::new(move |_path| {
                decodes.fetch_add(1, Ordering::SeqCst);
                Ok(solid_image(0.5))
            })
        };
        let processor = MediaProcessor::with_factories(
            fake_factory(FrameRate::new(24, 1), None),
            image_factory,
        );
        let entry = MediaAssetEntry {
            path: AssetPath::Absolute(PathBuf::from("/fake/plate.png")),
            kind: AssetKind::Still,
            metadata: AssetMetadata::default(),
            resolved: Some(PathBuf::from("/fake/plate.png")),
        };
        let (mut ev, graph) = media_evaluator(processor, entry);

        let fps = FrameRate::new(30, 1);
        for frame in [0, 1, 7] {
            let out = ev
                .evaluate(
                    &graph,
                    NodeId::new(1),
                    &EvalContext::new(frame, fps, (4, 4)),
                )
                .unwrap();
            let fb = out.downcast_ref::<FrameBuffer>().unwrap();
            assert!(
                (fb.as_f32()[0] - 0.5).abs() < 1e-6,
                "still pixel at {frame}"
            );
        }
        assert_eq!(decodes.load(Ordering::SeqCst), 1);
    }

    /// Sequence frame = `start + floor(t · seq_fps)`, clamped to
    /// `start..=end`, read from the representative frame's directory.
    #[test]
    fn sequence_frames_use_metadata_rate_and_clamp_to_the_range() {
        use ravel_core::composition::{AssetMetadata, AssetPath, MediaAssetEntry};
        use std::sync::Mutex as StdMutex;

        let requested: Arc<StdMutex<Vec<PathBuf>>> = Arc::new(StdMutex::new(Vec::new()));
        let image_factory: ImageReaderFactory = {
            let requested = Arc::clone(&requested);
            Arc::new(move |path| {
                requested.lock().unwrap().push(path.to_path_buf());
                Ok(solid_image(0.25))
            })
        };
        let processor = MediaProcessor::with_factories(
            fake_factory(FrameRate::new(24, 1), None),
            image_factory,
        );
        let entry = MediaAssetEntry {
            path: AssetPath::Absolute(PathBuf::from("/fake/seq/f_0100.png")),
            kind: AssetKind::Sequence {
                prefix: "f_".into(),
                suffix: ".png".into(),
                padding: 4,
                start: 100,
                end: 110,
            },
            metadata: AssetMetadata {
                frame_rate: Some(FrameRate::new(24, 1)),
                ..AssetMetadata::default()
            },
            resolved: Some(PathBuf::from("/fake/seq/f_0100.png")),
        };
        let (mut ev, graph) = media_evaluator(processor, entry);

        let comp_fps = FrameRate::new(30, 1);
        // t = 0.5 s → 24 fps sequence frame 12 → index 112, clamped to 110.
        ev.evaluate(
            &graph,
            NodeId::new(1),
            &EvalContext::new(15, comp_fps, (4, 4)),
        )
        .unwrap();
        // t = 0.1 s → frame 2.4 → 2 → index 102.
        ev.evaluate(
            &graph,
            NodeId::new(1),
            &EvalContext::new(3, comp_fps, (4, 4)),
        )
        .unwrap();
        assert_eq!(
            requested.lock().unwrap().as_slice(),
            [
                PathBuf::from("/fake/seq/f_0110.png"),
                PathBuf::from("/fake/seq/f_0102.png"),
            ]
        );
    }

    /// A sequence without probed metadata plays at the composition rate.
    #[test]
    fn sequence_rate_falls_back_to_the_comp_rate() {
        use ravel_core::composition::{AssetMetadata, AssetPath, MediaAssetEntry};
        use std::sync::Mutex as StdMutex;

        let requested: Arc<StdMutex<Vec<PathBuf>>> = Arc::new(StdMutex::new(Vec::new()));
        let image_factory: ImageReaderFactory = {
            let requested = Arc::clone(&requested);
            Arc::new(move |path| {
                requested.lock().unwrap().push(path.to_path_buf());
                Ok(solid_image(0.25))
            })
        };
        let processor = MediaProcessor::with_factories(
            fake_factory(FrameRate::new(24, 1), None),
            image_factory,
        );
        let entry = MediaAssetEntry {
            path: AssetPath::Absolute(PathBuf::from("/fake/seq/f_0100.png")),
            kind: AssetKind::Sequence {
                prefix: "f_".into(),
                suffix: ".png".into(),
                padding: 4,
                start: 100,
                end: 200,
            },
            metadata: AssetMetadata::default(),
            resolved: Some(PathBuf::from("/fake/seq/f_0100.png")),
        };
        let (mut ev, graph) = media_evaluator(processor, entry);

        // Comp rate 30 fps: t = 0.5 s → frame 15 → index 115.
        ev.evaluate(
            &graph,
            NodeId::new(1),
            &EvalContext::new(15, FrameRate::new(30, 1), (4, 4)),
        )
        .unwrap();
        assert_eq!(
            requested.lock().unwrap().as_slice(),
            [PathBuf::from("/fake/seq/f_0115.png")]
        );
    }
}
