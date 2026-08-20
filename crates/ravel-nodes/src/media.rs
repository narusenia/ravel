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
//! Decoded frames are not kept here. They go into the
//! [`MediaFrameCache`](ravel_media::frame_cache::MediaFrameCache) the
//! processor was built with (`CACHE-8`), which is shared by every `media`
//! node of an evaluation worker and keyed by the footage rather than by the
//! node — so two layers on one clip decode it once, and scrubbing backwards
//! re-reads instead of re-decoding a GOP.
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

use ravel_core::color::ColorSpace;
use ravel_core::composition::{AssetKind, ColorSpaceSource, MediaAssetEntry};
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::id::AssetId;
use ravel_core::media::{MediaReader, MediaResult, VideoStreamInfo};
use ravel_core::types::{FrameBuffer, NodeData};
use ravel_media::frame_cache::{FrameKey, MediaFrameCache};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Opens a [`MediaReader`] for a path, told which colour space the file's
/// samples are in so the decoder can hand back working-space values.
/// Injectable for tests and alternate backends.
pub type ReaderFactory =
    Arc<dyn Fn(&Path, ColorSpace) -> MediaResult<Box<dyn MediaReader>> + Send + Sync>;

/// Reads one still image as an RGBA f32 frame in the working space.
/// Injectable for tests; the default backend is `ravel-media`'s single-image
/// decoder (a one-frame "video" to FFmpeg).
pub type ImageReaderFactory =
    Arc<dyn Fn(&Path, ColorSpace) -> MediaResult<FrameBuffer> + Send + Sync>;

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
    color_space: ColorSpace,
    reader: Box<dyn MediaReader>,
}

/// Decodes one media frame per evaluation, branching on the asset's
/// [`AssetKind`]. The open decoder is kept here and keyed by the resolved
/// path — never by parameter values — so `asset_id` edits only require dirty
/// marking; it stays a single entry ("one open at a time"). The frames that
/// decoder produces live in the shared [`MediaFrameCache`] instead, so the
/// number of frames retained is a budget decision rather than a property of
/// how many `media` nodes exist.
pub struct MediaProcessor {
    factory: ReaderFactory,
    image_factory: ImageReaderFactory,
    open: Mutex<Option<OpenReader>>,
    frames: MediaFrameCache,
    /// Asset ids already warned about. Offline assets and decode failures
    /// degrade to a transparent frame instead of failing, so without this
    /// set every frame of a broken clip would re-log the same warning.
    warned: Mutex<HashSet<String>>,
}

impl MediaProcessor {
    /// The production constructor: every `media` node of one evaluation
    /// worker is handed the same `frames`, which is what makes a decode
    /// shared rather than per node.
    pub fn from_node(_node: &Node, frames: &MediaFrameCache) -> Self {
        Self::with_factories_and_cache(
            default_reader_factory(),
            default_image_reader_factory(),
            frames.clone(),
        )
    }

    /// Inject only the container backend; stills and sequences keep the
    /// default single-image reader. The decode cache is this processor's
    /// own — for tests and standalone hosts.
    pub fn with_reader_factory(factory: ReaderFactory) -> Self {
        Self::with_factories(factory, default_image_reader_factory())
    }

    /// Inject both backends, with a decode cache of this processor's own.
    pub fn with_factories(factory: ReaderFactory, image_factory: ImageReaderFactory) -> Self {
        Self::with_factories_and_cache(factory, image_factory, MediaFrameCache::standalone())
    }

    /// Inject both backends and the decode cache — what a test that has to
    /// observe sharing between two processors uses.
    pub fn with_factories_and_cache(
        factory: ReaderFactory,
        image_factory: ImageReaderFactory,
        frames: MediaFrameCache,
    ) -> Self {
        Self {
            factory,
            image_factory,
            open: Mutex::new(None),
            frames,
            warned: Mutex::new(HashSet::new()),
        }
    }

    /// Log `detail` once per asset and yield a transparent frame at the
    /// evaluation resolution. An offline or undecodable asset must not fail
    /// the surrounding composition (`docs/implementation/media-import-plan.md`,
    /// decision 7); the warn-once set keeps per-frame evaluations from
    /// flooding the log.
    ///
    /// `label` is [`asset_label`]'s output: it identifies the asset in the log
    /// and is the warn-once key, so it has to be stable for a given asset
    /// across frames.
    fn fallback_frame(
        &self,
        label: &str,
        ctx: &EvalContext,
        detail: impl FnOnce() -> String,
    ) -> Arc<dyn NodeData> {
        let mut warned = self.warned.lock().expect("media warn lock poisoned");
        if warned.insert(label.to_string()) {
            tracing::warn!("media: asset {label}: {}", detail());
        }
        Arc::new(FrameBuffer::new_zeroed(ctx.resolution.0, ctx.resolution.1))
    }

    /// Log an inferred (not user-set) input colour space once per asset.
    /// Shares the warn-once set with [`Self::fallback_frame`]: both are
    /// per-asset notices, and per-frame evaluation would otherwise repeat
    /// them for every frame of the clip.
    fn note_inferred_color_space(&self, label: &str, space: ColorSpace, source: ColorSpaceSource) {
        let mut warned = self.warned.lock().expect("media warn lock poisoned");
        if warned.insert(format!("colour-space:{label}")) {
            tracing::info!(
                "media: asset {label}: input colour space {} inferred from {}",
                space.name().unwrap_or("custom"),
                match source {
                    ColorSpaceSource::Metadata => "file metadata",
                    ColorSpaceSource::ExtensionDefault => "the file extension",
                    ColorSpaceSource::Explicit => "the asset setting",
                }
            );
        }
    }

    /// Decode one frame from a container, reusing the already-open reader
    /// while the resolved path is unchanged.
    ///
    /// The shared cache is consulted **after** the stream is known, because
    /// the frame number the key names is `floor(t · media_fps)` and only the
    /// open reader knows that rate. Opening is the cheap half — the single
    /// `OpenReader` entry already amortizes it across frames — while the
    /// decode a hit skips is the seek-and-replay of a whole GOP.
    fn decode_container_frame(
        &self,
        path: &Path,
        color_space: ColorSpace,
        ctx: &EvalContext,
    ) -> anyhow::Result<Arc<FrameBuffer>> {
        let mut open = self.open.lock().expect("media reader lock poisoned");
        if open
            .as_ref()
            .is_none_or(|o| o.path != path || o.color_space != color_space)
        {
            let reader = (self.factory)(path, color_space)
                .map_err(|e| anyhow::anyhow!("media: failed to open {path:?}: {e}"))?;
            *open = Some(OpenReader {
                path: path.to_path_buf(),
                color_space,
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
        let key = FrameKey::video(path, color_space, stream.stream_index, frame);
        if let Some(hit) = self.frames.get(&key) {
            return Ok(hit);
        }
        let decoded = Arc::new(
            open.reader
                .decode_video_frame(stream.stream_index, frame)
                .map_err(|e| anyhow::anyhow!("media: decoding frame {frame} failed: {e}"))?,
        );
        self.frames.insert(key, Arc::clone(&decoded));
        Ok(decoded)
    }

    /// Read a single image, serving the shared cache when this exact file in
    /// this exact input colour space has already been decoded. A still or a
    /// sequence frame is one picture per file, so the path is the position:
    /// scrubbing back over a sequence hits every frame the budget still
    /// holds, not merely the previous one.
    fn decode_image(&self, path: &Path, color_space: ColorSpace) -> MediaResult<Arc<FrameBuffer>> {
        let key = FrameKey::image(path, color_space);
        if let Some(hit) = self.frames.get(&key) {
            return Ok(hit);
        }
        let frame = Arc::new((self.image_factory)(path, color_space)?);
        self.frames.insert(key, Arc::clone(&frame));
        Ok(frame)
    }
}

/// How an asset is named in this node's log lines: the display name a user
/// would recognise, plus the reference the document stores.
///
/// Both halves earn their place. A support question is asked in terms of the
/// name ("why is the plate blank?"), while the reference is what has to be
/// found in `document/main.ron` — and since v9 the two are independent, so
/// neither implies the other. It doubles as the warn-once key, which is why it
/// is derived from the reference rather than from the name alone: two assets
/// may share a name.
fn asset_label(reference: &str, entry: Option<&MediaAssetEntry>) -> String {
    match entry {
        Some(entry) if !entry.name.is_empty() => format!("{:?} ({reference})", entry.name),
        _ => reference.to_string(),
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
        // The reference is the decimal spelling of an `AssetId`
        // (`AssetId::to_param_value`). Empty is the template default: a node
        // nobody has pointed at an asset, which is a mistake to report rather
        // than a picture to degrade.
        let reference = params.str_or("asset_id", "");
        anyhow::ensure!(!reference.is_empty(), "media: asset_id is not set");

        let document = _scope
            .document()
            .ok_or_else(|| anyhow::anyhow!("media: no document set on the evaluator"))?;
        // A reference the asset table does not answer is **offline**, not an
        // error. Since `.ravprj` v9 an `AssetId` is never reused, so deleting
        // an asset — or copying a layer out of another project — leaves
        // exactly this state behind by design, and the composition around it
        // still has to render (`docs/implementation/asset-identity-plan.md`).
        // A reference that is not a decimal id at all lands here too: it names
        // no asset either way, and refusing to draw would turn a hand edit
        // into a failed render.
        let asset =
            AssetId::from_param_value(reference).and_then(|id| document.get_media_asset(id));
        let label = asset_label(reference, asset);
        let Some(asset) = asset else {
            return Ok(self.fallback_frame(&label, ctx, || {
                "the project has no such asset (offline), transparent frame".to_string()
            }));
        };
        // `resolved` is the only path evaluation may use: the persisted
        // `path` can be project-relative or variable-prefixed, and only the
        // host knows the project root that anchors it. `None` means the
        // asset is offline — degrade to transparent, never fail.
        let Some(path) = asset.resolved.as_ref() else {
            return Ok(self.fallback_frame(&label, ctx, || {
                format!(
                    "offline (unresolved path {}), transparent frame",
                    asset.path
                )
            }));
        };

        // CM-2: the values this node yields are working-space, so the file's
        // own colour space has to be resolved before anything is decoded.
        // Tiers 2 and 3 are guesses; log which one was taken, once per asset,
        // so a clip that looks wrong can be traced back to the guess.
        let (color_space, source) = asset.input_color_space();
        if source != ColorSpaceSource::Explicit {
            self.note_inferred_color_space(&label, color_space, source);
        }

        let decoded: anyhow::Result<Arc<FrameBuffer>> = match &asset.kind {
            AssetKind::Container => self.decode_container_frame(path, color_space, ctx),
            AssetKind::Still => self
                .decode_image(path, color_space)
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
                self.decode_image(&dir.join(name), color_space)
                    .map_err(|e| {
                        anyhow::anyhow!("media: decoding sequence frame {index} failed: {e}")
                    })
            }
        };
        match decoded {
            Ok(frame) => Ok(frame),
            Err(err) => {
                Ok(self.fallback_frame(&label, ctx, || format!("{err:#}, transparent frame")))
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
    Arc::new(|path, color_space| {
        ravel_media::decoder::FfmpegDecoder::open(path)
            .map(|r| Box::new(r.with_input_color_space(color_space)) as Box<dyn MediaReader>)
    })
}

/// Without the `ffmpeg` feature there is no decoding backend.
#[cfg(not(feature = "ffmpeg"))]
fn default_reader_factory() -> ReaderFactory {
    Arc::new(|_path, _color_space| {
        Err(ravel_core::media::MediaError::Other(
            "media decoding requires the `ffmpeg` feature of ravel-nodes".into(),
        ))
    })
}

/// FFmpeg-backed single-image reader (requires the `ffmpeg` feature).
#[cfg(feature = "ffmpeg")]
fn default_image_reader_factory() -> ImageReaderFactory {
    Arc::new(ravel_media::image_seq::read_image_frame_in)
}

/// Without the `ffmpeg` feature there is no image decoding backend.
#[cfg(not(feature = "ffmpeg"))]
fn default_image_reader_factory() -> ImageReaderFactory {
    Arc::new(|_path, _color_space| {
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
    /// index (`frame / 1000`), counting the decodes it was asked for.
    struct FakeReader {
        info: MediaInfo,
        decodes: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FakeReader {
        fn new(fps: FrameRate, frame_count: Option<u64>) -> Self {
            Self::counting(fps, frame_count, Arc::default())
        }

        fn counting(
            fps: FrameRate,
            frame_count: Option<u64>,
            decodes: Arc<std::sync::atomic::AtomicUsize>,
        ) -> Self {
            Self {
                decodes,
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
                        color_primaries: None,
                        color_transfer: None,
                        color_matrix: None,
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
            self.decodes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
        Arc::new(move |_path, _color_space| {
            Ok(Box::new(FakeReader::new(fps, frame_count)) as Box<_>)
        })
    }

    /// The asset every fixture in this module registers and every
    /// [`media_node`] references. Fixed so both halves of the reference can be
    /// written without threading a value through each helper.
    fn test_asset() -> AssetId {
        AssetId::new(1)
    }

    fn media_node(id: u64) -> Node {
        Node::new(NodeId::new(id), "media")
            .with_output("frame", DataTypeId::FRAME_BUFFER)
            .with_param(
                "asset_id",
                ParameterValue::String(test_asset().to_param_value()),
            )
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
            Document::default().with_media_asset(test_asset(), "/fake/clip.mov"),
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

    /// A reference the asset table cannot answer yields a transparent frame,
    /// not an error.
    ///
    /// This **reverses** the pre-v9 behaviour, where an unknown asset id
    /// failed the evaluation. Since v9 an `AssetId` is never reused, so a
    /// deleted asset — or a layer pasted from another project — leaves a
    /// reference that resolves to nothing *by design*
    /// (`docs/implementation/asset-identity-plan.md`). Failing the render for
    /// the designed outcome would make deleting one clip break the whole
    /// composite, so it degrades exactly like an offline asset.
    #[test]
    fn a_reference_to_a_missing_asset_yields_a_transparent_frame() {
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
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx).unwrap();
        let frame = out.downcast_ref::<FrameBuffer>().expect("a frame");
        assert_eq!((frame.width, frame.height), (4, 4));
        assert!(
            frame.as_f32().iter().all(|value| *value == 0.0),
            "an unresolvable reference is offline, so the frame is transparent"
        );
    }

    /// A node nobody has pointed at an asset yet is a mistake to report, not a
    /// picture to degrade: it is the template default, not a broken reference.
    #[test]
    fn an_unset_asset_reference_is_an_error() {
        let node = Node::new(NodeId::new(1), "media")
            .with_output("frame", DataTypeId::FRAME_BUFFER)
            .with_param("asset_id", ParameterValue::String(String::new()));
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
            Arc::new(move |_path, _color_space| {
                opens.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(FakeReader::new(FrameRate::new(24, 1), None)) as Box<_>)
            })
        };

        let graph = Graph::new().add_node(media_node(1)).unwrap();
        let mut ev = Evaluator::new();
        ev.set_document(Arc::new(Document::default().with_media_asset_entry(
            test_asset(),
            MediaAssetEntry {
                name: String::new(),
                color_space: None,
                path: AssetPath::Relative("./footage/clip.mov".into()),
                kind: AssetKind::Container,
                metadata: AssetMetadata::default(),
                exposed_owner: None,
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

        let factory: ReaderFactory =
            Arc::new(|_path, _color_space| Err(MediaError::Other("cannot open".into())));
        let entry = MediaAssetEntry {
            name: String::new(),
            color_space: None,
            path: AssetPath::Absolute(PathBuf::from("/fake/clip.mov")),
            kind: AssetKind::Container,
            metadata: AssetMetadata::default(),
            exposed_owner: None,
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
            Arc::new(move |path, _color_space| {
                seen.lock().unwrap().push(path.to_path_buf());
                Ok(Box::new(FakeReader::new(FrameRate::new(24, 1), None)) as Box<_>)
            })
        };

        let graph = Graph::new().add_node(media_node(1)).unwrap();
        let mut ev = Evaluator::new();
        ev.set_document(Arc::new(Document::default().with_media_asset_entry(
            test_asset(),
            MediaAssetEntry {
                name: String::new(),
                color_space: None,
                path: AssetPath::Relative("./footage/clip.mov".into()),
                kind: AssetKind::Container,
                metadata: AssetMetadata::default(),
                exposed_owner: None,
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
            Document::default().with_media_asset_entry(test_asset(), entry),
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
            Arc::new(move |_path, _color_space| {
                decodes.fetch_add(1, Ordering::SeqCst);
                Ok(solid_image(0.5))
            })
        };
        let processor = MediaProcessor::with_factories(
            fake_factory(FrameRate::new(24, 1), None),
            image_factory,
        );
        let entry = MediaAssetEntry {
            name: String::new(),
            color_space: None,
            path: AssetPath::Absolute(PathBuf::from("/fake/plate.png")),
            kind: AssetKind::Still,
            metadata: AssetMetadata::default(),
            exposed_owner: None,
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

    /// CM-2: the resolved input colour space reaches the decoder, and the
    /// resolution order decides which one. The decoder is what removes the
    /// transfer function, so handing it the wrong space is the whole failure
    /// mode this unit exists to prevent.
    #[test]
    fn the_resolved_input_colour_space_reaches_the_decoder() {
        use ravel_core::color::ColorSpace;
        use ravel_core::composition::{AssetMetadata, AssetPath, MediaAssetEntry};
        use std::sync::Mutex as StdMutex;

        fn seen(entry: MediaAssetEntry) -> ColorSpace {
            let seen: Arc<StdMutex<Vec<ColorSpace>>> = Arc::new(StdMutex::new(Vec::new()));
            let image_factory: ImageReaderFactory = {
                let seen = Arc::clone(&seen);
                Arc::new(move |_path, color_space| {
                    seen.lock().unwrap().push(color_space);
                    Ok(solid_image(0.5))
                })
            };
            let processor = MediaProcessor::with_factories(
                fake_factory(FrameRate::new(24, 1), None),
                image_factory,
            );
            let (mut ev, graph) = media_evaluator(processor, entry);
            ev.evaluate(
                &graph,
                NodeId::new(1),
                &EvalContext::new(0, FrameRate::new(30, 1), (4, 4)),
            )
            .unwrap();
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1, "the still should be decoded exactly once");
            seen[0]
        }

        fn still(name: &str) -> MediaAssetEntry {
            MediaAssetEntry {
                name: String::new(),
                color_space: None,
                path: AssetPath::Absolute(PathBuf::from(format!("/fake/{name}"))),
                kind: AssetKind::Still,
                metadata: AssetMetadata::default(),
                exposed_owner: None,
                resolved: Some(PathBuf::from(format!("/fake/{name}"))),
            }
        }

        // Extension default: integer format → sRGB, float format → linear
        // (so a linear EXR is not decoded a second time).
        assert_eq!(seen(still("plate.png")), ColorSpace::SRGB);
        assert_eq!(seen(still("plate.exr")), ColorSpace::LINEAR_REC709);

        // Metadata beats the extension.
        let mut tagged = still("plate.exr");
        tagged.metadata.color_space = Some("srgb".into());
        assert_eq!(seen(tagged.clone()), ColorSpace::SRGB);

        // An explicit setting beats both.
        let mut explicit = tagged;
        explicit.color_space = Some(ColorSpace::LINEAR_REC709);
        assert_eq!(seen(explicit), ColorSpace::LINEAR_REC709);
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
            Arc::new(move |path, _color_space| {
                requested.lock().unwrap().push(path.to_path_buf());
                Ok(solid_image(0.25))
            })
        };
        let processor = MediaProcessor::with_factories(
            fake_factory(FrameRate::new(24, 1), None),
            image_factory,
        );
        let entry = MediaAssetEntry {
            name: String::new(),
            color_space: None,
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
            exposed_owner: None,
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
            Arc::new(move |path, _color_space| {
                requested.lock().unwrap().push(path.to_path_buf());
                Ok(solid_image(0.25))
            })
        };
        let processor = MediaProcessor::with_factories(
            fake_factory(FrameRate::new(24, 1), None),
            image_factory,
        );
        let entry = MediaAssetEntry {
            name: String::new(),
            color_space: None,
            path: AssetPath::Absolute(PathBuf::from("/fake/seq/f_0100.png")),
            kind: AssetKind::Sequence {
                prefix: "f_".into(),
                suffix: ".png".into(),
                padding: 4,
                start: 100,
                end: 200,
            },
            metadata: AssetMetadata::default(),
            exposed_owner: None,
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

    // =======================================================================
    // CACHE-8: the shared decode cache
    // =======================================================================

    use ravel_core::composition::{AssetMetadata, AssetPath};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A container factory whose readers all report to one decode counter.
    fn counting_factory(
        fps: FrameRate,
        frame_count: Option<u64>,
    ) -> (ReaderFactory, Arc<AtomicUsize>) {
        let decodes: Arc<AtomicUsize> = Arc::default();
        let factory: ReaderFactory = {
            let decodes = Arc::clone(&decodes);
            Arc::new(move |_path, _color_space| {
                Ok(
                    Box::new(FakeReader::counting(fps, frame_count, Arc::clone(&decodes)))
                        as Box<_>,
                )
            })
        };
        (factory, decodes)
    }

    /// A single-image factory counting reads and colouring each frame by the
    /// file it came from.
    fn counting_image_factory() -> (ImageReaderFactory, Arc<AtomicUsize>) {
        let reads: Arc<AtomicUsize> = Arc::default();
        let factory: ImageReaderFactory = {
            let reads = Arc::clone(&reads);
            Arc::new(move |path: &Path, _color_space| {
                reads.fetch_add(1, Ordering::SeqCst);
                // The last digits of the file name become the pixel value, so
                // a wrong hit is visible in the picture and not only in the
                // counter.
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let digits: String = stem.chars().filter(|c| c.is_ascii_digit()).collect();
                Ok(solid_image(digits.parse::<f32>().unwrap_or(0.0) / 1000.0))
            })
        };
        (factory, reads)
    }

    fn container(path: &str) -> MediaAssetEntry {
        MediaAssetEntry {
            name: String::new(),
            color_space: None,
            path: AssetPath::Absolute(PathBuf::from(path)),
            kind: AssetKind::Container,
            metadata: AssetMetadata::default(),
            exposed_owner: None,
            resolved: Some(PathBuf::from(path)),
        }
    }

    fn still(path: &str) -> MediaAssetEntry {
        MediaAssetEntry {
            kind: AssetKind::Still,
            ..container(path)
        }
    }

    /// The media frame a comp frame decodes, as the fake reader encodes it.
    fn decoded_frame_index(value: &Arc<dyn NodeData>) -> f32 {
        value.downcast_ref::<FrameBuffer>().unwrap().as_f32()[0] * 1000.0
    }

    /// A `MediaFrameCache` whose budget holds `frames` of the fake reader's
    /// 4×4 output and no more.
    fn cache_for(frames: u64) -> MediaFrameCache {
        use ravel_core::cache_budget::{CacheBudgetConfig, SharedCacheBudget};
        let one = solid_image(0.0).byte_size();
        MediaFrameCache::new(SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: 0,
            ram_bytes: one * frames,
            disk_bytes: 0,
            sim_reserve_ratio: 0.0,
        }))
    }

    /// HIGH-16: scrubbing back over frames already decoded must not decode
    /// them again. Before the shared cache every backward step flushed the
    /// decoder, sought the preceding keyframe and replayed the GOP.
    #[test]
    fn scrubbing_backwards_does_not_decode_again() {
        let (factory, decodes) = counting_factory(FrameRate::new(30, 1), None);
        let processor = MediaProcessor::with_factories_and_cache(
            factory,
            default_image_reader_factory(),
            MediaFrameCache::standalone(),
        );
        let (mut ev, graph) = media_evaluator(processor, container("/fake/clip.mov"));

        let fps = FrameRate::new(30, 1);
        let at = |ev: &mut Evaluator, frame: u64| {
            decoded_frame_index(
                &ev.evaluate(
                    &graph,
                    NodeId::new(1),
                    &EvalContext::new(frame, fps, (4, 4)),
                )
                .unwrap(),
            )
        };

        for frame in 0..6 {
            assert_eq!(at(&mut ev, frame), frame as f32);
        }
        let forward = decodes.load(Ordering::SeqCst);
        assert_eq!(forward, 6, "each frame is decoded once on the way out");

        // Back over the same frames: the pictures must still be right, and
        // nothing may reach the decoder.
        for frame in (0..6).rev() {
            assert_eq!(at(&mut ev, frame), frame as f32, "backward frame {frame}");
        }
        assert_eq!(
            decodes.load(Ordering::SeqCst),
            forward,
            "a backward scrub re-decoded frames"
        );
    }

    /// The cache is keyed by the footage, not by the node, so two layers
    /// pointing at one clip decode it once between them.
    #[test]
    fn two_layers_on_one_clip_share_the_decode() {
        let (factory, decodes) = counting_factory(FrameRate::new(30, 1), None);
        let frames = MediaFrameCache::standalone();
        let processor = |factory: ReaderFactory| {
            MediaProcessor::with_factories_and_cache(
                factory,
                default_image_reader_factory(),
                frames.clone(),
            )
        };

        // Two `media` nodes, each with its own processor — what two layers on
        // one asset compile to.
        let graph = Graph::new()
            .add_node(media_node(1))
            .unwrap()
            .add_node(media_node(2))
            .unwrap();
        let mut ev = Evaluator::new();
        ev.set_document(Arc::new(
            Document::default().with_media_asset_entry(test_asset(), container("/fake/clip.mov")),
        ));
        ev.register(NodeId::new(1), Arc::new(processor(Arc::clone(&factory))));
        ev.register(NodeId::new(2), Arc::new(processor(factory)));

        let ctx = EvalContext::new(4, FrameRate::new(30, 1), (4, 4));
        let first = decoded_frame_index(&ev.evaluate(&graph, NodeId::new(1), &ctx).unwrap());
        let second = decoded_frame_index(&ev.evaluate(&graph, NodeId::new(2), &ctx).unwrap());

        assert_eq!(first, 4.0);
        assert_eq!(second, 4.0);
        assert_eq!(
            decodes.load(Ordering::SeqCst),
            1,
            "the second layer decoded the clip a second time"
        );
    }

    /// The budget is the only limit: past it the least recently used frame
    /// goes, and revisiting it costs a decode again. Without this the
    /// eviction path could be dead and every other test would still pass.
    #[test]
    fn a_frame_past_the_budget_is_dropped_and_decoded_again() {
        let (factory, decodes) = counting_factory(FrameRate::new(30, 1), None);
        let processor = MediaProcessor::with_factories_and_cache(
            factory,
            default_image_reader_factory(),
            // Room for two frames of this clip.
            cache_for(2),
        );
        let (mut ev, graph) = media_evaluator(processor, container("/fake/clip.mov"));

        let fps = FrameRate::new(30, 1);
        let at = |ev: &mut Evaluator, frame: u64| {
            decoded_frame_index(
                &ev.evaluate(
                    &graph,
                    NodeId::new(1),
                    &EvalContext::new(frame, fps, (4, 4)),
                )
                .unwrap(),
            )
        };

        for frame in 0..3 {
            at(&mut ev, frame);
        }
        assert_eq!(decodes.load(Ordering::SeqCst), 3);

        // Frames 1 and 2 are still resident.
        at(&mut ev, 2);
        at(&mut ev, 1);
        assert_eq!(
            decodes.load(Ordering::SeqCst),
            3,
            "a resident frame decoded"
        );

        // Frame 0 fell out when frame 2 arrived, and comes back correct.
        assert_eq!(at(&mut ev, 0), 0.0);
        assert_eq!(
            decodes.load(Ordering::SeqCst),
            4,
            "the evicted frame was served from a cache that should not have it"
        );
    }

    /// MED-MED-02 (second half): a sequence used to keep exactly one frame,
    /// so playback paid a decoder construction per frame and a step back paid
    /// another. Now the recent frames stay resident up to the budget.
    #[test]
    fn a_sequence_keeps_the_recent_frames() {
        let (image_factory, reads) = counting_image_factory();
        let processor = MediaProcessor::with_factories_and_cache(
            fake_factory(FrameRate::new(24, 1), None),
            image_factory,
            MediaFrameCache::standalone(),
        );
        let entry = MediaAssetEntry {
            kind: AssetKind::Sequence {
                prefix: "f_".into(),
                suffix: ".png".into(),
                padding: 4,
                start: 100,
                end: 200,
            },
            metadata: AssetMetadata {
                frame_rate: Some(FrameRate::new(30, 1)),
                ..AssetMetadata::default()
            },
            ..still("/fake/seq/f_0100.png")
        };
        let (mut ev, graph) = media_evaluator(processor, entry);

        let fps = FrameRate::new(30, 1);
        let at = |ev: &mut Evaluator, frame: u64| {
            decoded_frame_index(
                &ev.evaluate(
                    &graph,
                    NodeId::new(1),
                    &EvalContext::new(frame, fps, (4, 4)),
                )
                .unwrap(),
            )
        };

        // Play frames 100..=104, then walk back over all of them.
        for frame in 0..5 {
            assert_eq!(at(&mut ev, frame), 100.0 + frame as f32);
        }
        assert_eq!(reads.load(Ordering::SeqCst), 5);
        for frame in (0..5).rev() {
            assert_eq!(at(&mut ev, frame), 100.0 + frame as f32);
        }
        assert_eq!(
            reads.load(Ordering::SeqCst),
            5,
            "the sequence still kept only one frame"
        );
    }

    /// The key names the resolved path, so a relinked asset cannot be served
    /// the frame decoded from the file it used to point at. Getting this
    /// wrong shows the old footage with no way to tell.
    #[test]
    fn a_relinked_asset_never_hits_the_old_paths_frame() {
        let (image_factory, reads) = counting_image_factory();
        let processor = MediaProcessor::with_factories_and_cache(
            fake_factory(FrameRate::new(24, 1), None),
            image_factory,
            MediaFrameCache::standalone(),
        );
        let (mut ev, graph) = media_evaluator(processor, still("/fake/old/plate_0001.png"));

        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (4, 4));
        assert_eq!(
            decoded_frame_index(&ev.evaluate(&graph, NodeId::new(1), &ctx).unwrap()),
            1.0
        );

        // Relink: the same asset id now resolves elsewhere.
        ev.set_document(Arc::new(Document::default().with_media_asset_entry(
            test_asset(),
            still("/fake/new/plate_0002.png"),
        )));
        ev.invalidate_all();

        assert_eq!(
            decoded_frame_index(&ev.evaluate(&graph, NodeId::new(1), &ctx).unwrap()),
            2.0,
            "the relinked asset was served the old file's frame"
        );
        assert_eq!(reads.load(Ordering::SeqCst), 2, "the new file was read");
    }
}
