// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `video` — decoded media frames as a network source (REQ-LAYER-008).
//!
//! The node's `asset_id` parameter indexes the document's media asset table
//! ([`ravel_core::composition::MediaAssetEntry`]); the frame to decode is
//! derived from the layer-local time in **seconds** so media whose frame
//! rate differs from the composition maps correctly (REQ-LAYER-006):
//! `media_frame = floor(t · media_fps)`, clamped to the stream's last frame.
//!
//! Decoding goes through the [`MediaReader`] abstraction. The default
//! backend is `ravel-media`'s FFmpeg decoder (enable the `ffmpeg` feature);
//! tests inject synthetic readers through
//! [`VideoProcessor::with_reader_factory`].

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::media::{MediaReader, MediaResult, VideoStreamInfo};
use ravel_core::types::NodeData;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Opens a [`MediaReader`] for a path. Injectable for tests and alternate
/// backends.
pub type ReaderFactory = Arc<dyn Fn(&Path) -> MediaResult<Box<dyn MediaReader>> + Send + Sync>;

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

/// Decodes one video frame per evaluation. The opened decoder is cached and
/// keyed by the resolved path — never by parameter values — so `asset_id`
/// edits only require dirty marking.
pub struct VideoProcessor {
    factory: ReaderFactory,
    open: Mutex<Option<OpenReader>>,
}

impl VideoProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self::with_reader_factory(default_reader_factory())
    }

    pub fn with_reader_factory(factory: ReaderFactory) -> Self {
        Self {
            factory,
            open: Mutex::new(None),
        }
    }
}

impl NodeProcessor for VideoProcessor {
    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let asset_id = params.str_or("asset_id", "");
        anyhow::ensure!(!asset_id.is_empty(), "video: asset_id is not set");

        let document = _scope
            .document()
            .ok_or_else(|| anyhow::anyhow!("video: no document set on the evaluator"))?;
        let asset = document
            .get_media_asset(asset_id)
            .ok_or_else(|| anyhow::anyhow!("video: unknown asset id {asset_id:?}"))?;

        let mut open = self.open.lock().expect("video reader lock poisoned");
        if open.as_ref().is_none_or(|o| o.path != asset.path) {
            let reader = (self.factory)(&asset.path)
                .map_err(|e| anyhow::anyhow!("video: failed to open {:?}: {e}", asset.path))?;
            *open = Some(OpenReader {
                path: asset.path.clone(),
                reader,
            });
        }
        // SAFETY of unwrap: populated just above.
        let open = open.as_mut().unwrap();

        let stream = open
            .reader
            .info()
            .first_video()
            .ok_or_else(|| anyhow::anyhow!("video: {:?} has no video stream", open.path))?
            .clone();
        let frame = media_frame_for(ctx.time, &stream);
        let buffer = open
            .reader
            .decode_video_frame(stream.stream_index, frame)
            .map_err(|e| anyhow::anyhow!("video: decoding frame {frame} failed: {e}"))?;
        Ok(Arc::new(buffer))
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
            "video decoding requires the `ffmpeg` feature of ravel-nodes".into(),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::Document;
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
            Ok(FrameBuffer {
                width: 4,
                height: 4,
                data: data.into(),
            })
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

    fn video_node(id: u64) -> Node {
        Node::new(NodeId::new(id), "video")
            .with_output("frame", DataTypeId::FRAME_BUFFER)
            .with_param("asset_id", ParameterValue::String("clip".into()))
    }

    fn decode_at(
        comp_fps: FrameRate,
        media_fps: FrameRate,
        frame_count: Option<u64>,
        comp_frame: u64,
    ) -> f32 {
        let node = video_node(1);
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.set_document(Arc::new(
            Document::default().with_media_asset("clip", "/fake/clip.mov"),
        ));
        ev.register(
            NodeId::new(1),
            Arc::new(VideoProcessor::with_reader_factory(fake_factory(
                media_fps,
                frame_count,
            ))),
        );
        let ctx = EvalContext::new(comp_frame, comp_fps, (4, 4));
        let out = ev.evaluate(&graph, NodeId::new(1), &ctx).unwrap();
        out.downcast_ref::<FrameBuffer>().unwrap().data[0] * 1000.0
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
        let node = video_node(1);
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.set_document(Arc::new(Document::default()));
        ev.register(
            NodeId::new(1),
            Arc::new(VideoProcessor::with_reader_factory(fake_factory(
                FrameRate::new(24, 1),
                None,
            ))),
        );
        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (4, 4));
        assert!(ev.evaluate(&graph, NodeId::new(1), &ctx).is_err());
    }
}
