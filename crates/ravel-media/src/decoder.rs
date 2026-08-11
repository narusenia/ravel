// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! FFmpeg-based media decoder implementing [`MediaReader`].
//!
//! Opens a media file via FFmpeg's `avformat` layer, probes stream metadata,
//! and decodes video frames (to RGBA f32) and audio chunks (to interleaved
//! f32 PCM).  All FFmpeg access is dynamic-linked (LGPL compliant).
//!
//! Audio arrives in whatever sample format the codec uses natively; the
//! decoder only recognizes the format and hands the raw planes to
//! [`crate::audio_sample`], which owns the conversion to packed f32.
//!
//! When available, hardware-accelerated decoding is used via VideoToolbox
//! (macOS) or NVDEC/D3D11VA (Windows), falling back to software decode
//! transparently.

use std::path::Path;

use ravel_core::color::{
    ColorSpace, Primaries, Transfer, ingest_rgba8, ingest_rgba16, ingest_rgbaf32,
};

use ffmpeg_the_third as ffmpeg;
use ffmpeg_the_third::ffi;
use ffmpeg_the_third::ffi::AV_TIME_BASE;
use ffmpeg_the_third::format::context::Input;
use ffmpeg_the_third::media::Type as MediaType;
use ffmpeg_the_third::software::scaling as sws;
use ffmpeg_the_third::util::color;
use ffmpeg_the_third::util::format::pixel::Pixel as PixelFormat;
use ffmpeg_the_third::util::format::sample::Sample as SampleFormat;
use ffmpeg_the_third::util::frame;
use rayon::prelude::*;
use tracing::{debug, warn};

use crate::audio_sample::{self, SampleEncoding};
use crate::hwaccel::HwAccelConfig;
use crate::hwaccel::device::HwDeviceContext;
use crate::hwaccel::transfer::ensure_sw_frame;
use ravel_core::media::{
    AudioCodec, AudioStreamInfo, ContainerFormat, MediaError, MediaInfo, MediaReader, MediaResult,
    StreamInfo, VideoCodec, VideoStreamInfo,
};
use ravel_core::types::{AudioBuffer, FrameBuffer, FrameRate};

/// Ensure FFmpeg is initialized (safe to call multiple times).
pub(crate) fn init_ffmpeg() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        ffmpeg::init().expect("FFmpeg initialization failed");
    });
}

/// Cached video decoder context, persisted across `decode_video_frame` calls.
struct CachedVideoDecoder {
    decoder: ffmpeg::codec::decoder::Video,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    frame_rate: ffmpeg::Rational,
    /// Whether this decoder is using hardware acceleration.
    hw_active: bool,
    /// Presentation timestamp of the frame most recently returned, in
    /// `time_base` ticks. Lets a forward request continue from where the
    /// last one stopped instead of seeking again (see
    /// [`FfmpegDecoder::decode_video_frame`]).
    last_returned_pts: Option<i64>,
    /// Cached software scaler and its output frame. The scaler key includes
    /// every input and output property that changes the filter tables.
    scaler: ScalerCache,
    /// Test-only switch used by the old/new performance harness.
    #[cfg(test)]
    legacy_conversion: bool,
}

/// The complete configuration that determines an sws scaler's filter tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScalerKey {
    input_format: PixelFormat,
    input_width: u32,
    input_height: u32,
    output_format: PixelFormat,
    output_width: u32,
    output_height: u32,
}

impl ScalerKey {
    fn new(frame: &frame::Video, output: PixelFormat) -> Self {
        Self {
            input_format: frame.format(),
            input_width: frame.width(),
            input_height: frame.height(),
            output_format: output,
            output_width: frame.width(),
            output_height: frame.height(),
        }
    }
}

/// Cache one scaler and its output frame for the currently active format and
/// dimensions. A stream can change its decoded pixel format or dimensions,
/// so the full [`ScalerKey`] is checked before every run.
#[derive(Default)]
struct ScalerCache {
    key: Option<ScalerKey>,
    scaler: Option<sws::Context>,
    output_frame: Option<frame::Video>,
    /// Input formats sws has already refused to scale to [`DEEP_OUTPUT`].
    /// A linear scan over one or two entries in practice — a stream keeps its
    /// decoded format — so `PixelFormat` not being `Hash` costs nothing here.
    deep_scale_broken: Vec<PixelFormat>,
    #[cfg(test)]
    creations: usize,
}

// `FfmpegDecoder` is moved to a worker through `MediaReader: Send`. The sws
// context is owned exclusively by that decoder and is only ever accessed
// through `&mut self`; it is never shared between threads or used
// concurrently. Moving the FFmpeg-owned pointer with its decoder therefore
// preserves the same ownership invariant as the decoder's other FFmpeg
// contexts.
unsafe impl Send for ScalerCache {}

impl ScalerCache {
    fn ensure(&mut self, frame: &frame::Video, output: PixelFormat) -> MediaResult<()> {
        let key = ScalerKey::new(frame, output);
        if self.key == Some(key) {
            return Ok(());
        }

        let output_changed = self.key.is_some_and(|previous| {
            previous.output_format != key.output_format
                || previous.output_width != key.output_width
                || previous.output_height != key.output_height
        });
        let scaler = sws::Context::get(
            key.input_format,
            key.input_width,
            key.input_height,
            key.output_format,
            key.output_width,
            key.output_height,
            sws::Flags::BILINEAR,
        )
        .map_err(|e| MediaError::DecodeError(format!("create scaler: {e}")))?;

        self.key = Some(key);
        self.scaler = Some(scaler);
        if output_changed {
            self.output_frame = None;
        }
        #[cfg(test)]
        {
            self.creations += 1;
        }
        Ok(())
    }

    fn run(&mut self, frame: &frame::Video, output: PixelFormat) -> MediaResult<&frame::Video> {
        self.ensure(frame, output)?;
        let output_frame = self.output_frame.get_or_insert_with(frame::Video::empty);
        self.scaler
            .as_mut()
            .expect("scaler is initialized by ensure")
            .run(frame, output_frame)
            .map_err(|e| MediaError::DecodeError(format!("scale frame: {e}")))?;
        Ok(output_frame)
    }

    fn deep_scale_known_broken(&self, format: PixelFormat) -> bool {
        self.deep_scale_broken.contains(&format)
    }

    fn mark_deep_scale_broken(&mut self, format: PixelFormat) {
        if !self.deep_scale_known_broken(format) {
            self.deep_scale_broken.push(format);
        }
    }

    #[cfg(test)]
    fn creations(&self) -> usize {
        self.creations
    }
}

/// Cached audio decoder context, persisted across `decode_audio_chunk` calls.
struct CachedAudioDecoder {
    decoder: ffmpeg::codec::decoder::Audio,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    /// First stream timestamp in `time_base` ticks, normalized to zero when
    /// the container does not declare one.
    start_pts: i64,
    sample_rate: u32,
    channels: u32,
}

/// FFmpeg-based decoder for video and audio files.
///
/// Supports H.264, H.265, AV1, ProRes, DNxHR video codecs and
/// AAC, PCM, FLAC, Opus audio codecs in MP4, MOV, MKV, WebM containers.
///
/// Hardware-accelerated decoding is attempted automatically via
/// VideoToolbox (macOS) or NVDEC/D3D11VA (Windows).
pub struct FfmpegDecoder {
    input_ctx: Input,
    info: MediaInfo,
    /// Index of the best video stream, if any.
    #[allow(dead_code)]
    video_stream_index: Option<usize>,
    /// Index of the best audio stream, if any.
    #[allow(dead_code)]
    audio_stream_index: Option<usize>,
    /// Cached video decoder, created on first decode call.
    video_decoder: Option<CachedVideoDecoder>,
    /// Cached audio decoder, created on first decode call.
    audio_decoder: Option<CachedAudioDecoder>,
    /// Hardware device context, shared across all video decoders.
    hw_device_ctx: Option<HwDeviceContext>,
    /// The colour space the file's samples are in.
    ///
    /// Defaults to [`ColorSpace::LINEAR_REC709`], which makes the decode
    /// path a no-op — the shape every caller that only wants the file's own
    /// values (thumbnails, probes) keeps. The `media` node resolves the real
    /// space from the asset and sets it through
    /// [`FfmpegDecoder::with_input_color_space`], and only then are decoded
    /// samples converted into the working space.
    input_color_space: ColorSpace,
}

/// C-callable `get_format` callback for FFmpeg codec context.
///
/// Selects the hardware pixel format matching the target stored in `opaque`.
/// Falls back to the first offered software format if the target is not
/// in the list.
unsafe extern "C" fn hw_get_format(
    ctx: *mut ffi::AVCodecContext,
    pix_fmts: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    let target_raw = unsafe { (*ctx).opaque as i32 };
    let target = ffi::AVPixelFormat(target_raw);

    let mut p = pix_fmts;
    unsafe {
        while *p != ffi::AVPixelFormat::NONE {
            if *p == target {
                return *p;
            }
            p = p.add(1);
        }
    }

    // HW format not offered — return first SW format.
    unsafe { *pix_fmts }
}

/// Try to find a matching HW config for the codec that is compatible
/// with our `HwDeviceContext`.
///
/// Returns the hardware pixel format if a match is found.
fn find_hw_config(codec: &ffmpeg::Codec, hw_ctx: &HwDeviceContext) -> Option<ffi::AVPixelFormat> {
    let target_device_type = hw_ctx.backend().to_av_device_type();
    let codec_ptr = codec.as_ptr();

    for i in 0.. {
        let config = unsafe { ffi::avcodec_get_hw_config(codec_ptr, i) };
        if config.is_null() {
            break;
        }

        let config = unsafe { &*config };
        let has_device_method =
            (config.methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX.0 as i32) != 0;

        if has_device_method && config.device_type == target_device_type {
            return Some(config.pix_fmt);
        }
    }

    None
}

/// A decode target expressed in the two units FFmpeg needs at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SeekTarget {
    /// The stream's own time base — what packet timestamps carry.
    pts: i64,
    /// `AV_TIME_BASE` units (microseconds) — what `avformat_seek_file` reads
    /// when it is given `stream_index = -1`.
    micros: i64,
}

/// Ticks of `time_base` in one second, at least 1.
fn ticks_per_second(time_base: ffmpeg::Rational) -> i64 {
    (i64::from(time_base.denominator()) / i64::from(time_base.numerator()).max(1)).max(1)
}

/// The position of `frame_number` in both units.
///
/// Mixing these up is easy and expensive: seeking with stream-time-base ticks
/// makes FFmpeg read them as microseconds, which for a typical `1/12800` base
/// lands two orders of magnitude early — near the start of the file for any
/// frame. The decoder then walks forward to the target, so the cost of
/// decoding one frame grows with its index.
fn seek_target(
    frame_number: u64,
    frame_rate: ffmpeg::Rational,
    time_base: ffmpeg::Rational,
) -> SeekTarget {
    if frame_rate.numerator() <= 0 || frame_rate.denominator() <= 0 {
        // No usable rate: fall back to treating the index as a raw timestamp,
        // which at least keeps ordering monotonic.
        return SeekTarget {
            pts: frame_number as i64,
            micros: 0,
        };
    }

    let sec_per_frame = f64::from(frame_rate.denominator()) / f64::from(frame_rate.numerator());
    let target_sec = frame_number as f64 * sec_per_frame;
    SeekTarget {
        pts: (target_sec * f64::from(time_base.denominator()) / f64::from(time_base.numerator()))
            as i64,
        micros: (target_sec * f64::from(AV_TIME_BASE)) as i64,
    }
}

/// Create a video decoder for the given stream, optionally with HW accel.
fn create_video_decoder(
    input_ctx: &Input,
    stream_index: usize,
    hw_device_ctx: &Option<HwDeviceContext>,
) -> MediaResult<CachedVideoDecoder> {
    let stream_ref = input_ctx
        .stream(stream_index)
        .ok_or(MediaError::NoStreamFound)?;
    let time_base = stream_ref.time_base();
    let frame_rate = stream_ref.rate();
    let codec_params = stream_ref.parameters();

    let mut decoder_ctx = ffmpeg::codec::Context::from_parameters(codec_params)
        .map_err(|e| MediaError::DecodeError(format!("create decoder context: {e}")))?;

    // Try to configure hardware acceleration.
    let mut hw_active = false;
    if let Some(hw_ctx) = hw_device_ctx
        && let Some(codec) = decoder_ctx.codec()
        && let Some(hw_pix_fmt) = find_hw_config(&codec, hw_ctx)
    {
        let buf_ref = unsafe { hw_ctx.new_ref() };
        if !buf_ref.is_null() {
            unsafe {
                let raw = decoder_ctx.as_mut_ptr();
                (*raw).hw_device_ctx = buf_ref;
                (*raw).get_format = Some(hw_get_format);
                (*raw).opaque = hw_pix_fmt.0 as *mut std::ffi::c_void;
            }
            hw_active = true;
            debug!(
                backend = hw_ctx.backend().name(),
                "configured HW accel for stream {stream_index}"
            );
        } else {
            warn!("av_buffer_ref failed, skipping HW accel");
        }
    }

    let decoder_result = decoder_ctx.decoder().video();

    match decoder_result {
        Ok(decoder) => Ok(CachedVideoDecoder {
            decoder,
            stream_index,
            time_base,
            frame_rate,
            hw_active,
            last_returned_pts: None,
            scaler: ScalerCache::default(),
            #[cfg(test)]
            legacy_conversion: false,
        }),
        Err(e) if hw_active => {
            // HW accel failed to open — retry without it.
            warn!("HW decoder open failed ({e}), falling back to software");
            let fallback_stream = input_ctx
                .stream(stream_index)
                .ok_or(MediaError::NoStreamFound)?;
            let fallback_params = fallback_stream.parameters();
            let decoder_ctx = ffmpeg::codec::Context::from_parameters(fallback_params)
                .map_err(|e| MediaError::DecodeError(format!("create decoder context: {e}")))?;
            let decoder = decoder_ctx
                .decoder()
                .video()
                .map_err(|e| MediaError::DecodeError(format!("open video decoder: {e}")))?;
            Ok(CachedVideoDecoder {
                decoder,
                stream_index,
                time_base,
                frame_rate,
                hw_active: false,
                last_returned_pts: None,
                scaler: ScalerCache::default(),
                #[cfg(test)]
                legacy_conversion: false,
            })
        }
        Err(e) => Err(MediaError::DecodeError(format!("open video decoder: {e}"))),
    }
}

/// Create an audio decoder for the given stream.
fn create_audio_decoder(input_ctx: &Input, stream_index: usize) -> MediaResult<CachedAudioDecoder> {
    let stream = input_ctx
        .stream(stream_index)
        .ok_or(MediaError::NoStreamFound)?;
    let time_base = stream.time_base();
    let start_pts = match stream.start_time() {
        ffmpeg::ffi::AV_NOPTS_VALUE => 0,
        start_pts => start_pts,
    };
    let codec_params = stream.parameters();

    let decoder_ctx = ffmpeg::codec::Context::from_parameters(codec_params)
        .map_err(|e| MediaError::DecodeError(format!("create decoder context: {e}")))?;
    let decoder = decoder_ctx
        .decoder()
        .audio()
        .map_err(|e| MediaError::DecodeError(format!("open audio decoder: {e}")))?;

    let sample_rate = decoder.rate();
    let channels = decoder.ch_layout().channels();

    Ok(CachedAudioDecoder {
        decoder,
        stream_index,
        time_base,
        start_pts,
        sample_rate,
        channels,
    })
}

impl FfmpegDecoder {
    /// Probe a media file and build [`MediaInfo`] without fully opening
    /// a decoder context.  Useful for asset metadata collection.
    pub fn probe(path: &Path) -> MediaResult<MediaInfo> {
        init_ffmpeg();
        let ctx = ffmpeg::format::input(path)
            .map_err(|e| MediaError::Other(format!("cannot open {}: {e}", path.display())))?;
        Ok(build_media_info(&ctx))
    }

    /// Declare the colour space this file's samples are in, so decoded
    /// frames come out in the working space (`CM-2`).
    pub fn with_input_color_space(mut self, space: ColorSpace) -> Self {
        self.input_color_space = space;
        self
    }

    /// Whether hardware-accelerated decoding is active for video.
    pub fn hw_accel_active(&self) -> bool {
        self.video_decoder.as_ref().is_some_and(|d| d.hw_active)
    }

    /// The name of the active HW backend, if any.
    pub fn hw_backend_name(&self) -> Option<&'static str> {
        self.hw_device_ctx.as_ref().map(|ctx| ctx.backend().name())
    }

    /// Ensure a video decoder is cached for the given stream index.
    fn ensure_video_decoder(&mut self, stream_index: usize) -> MediaResult<()> {
        let needs_create =
            !matches!(&self.video_decoder, Some(cached) if cached.stream_index == stream_index);
        if needs_create {
            let cached = create_video_decoder(&self.input_ctx, stream_index, &self.hw_device_ctx)?;
            self.video_decoder = Some(cached);
        }
        Ok(())
    }

    /// Ensure an audio decoder is cached for the given stream index.
    fn ensure_audio_decoder(&mut self, stream_index: usize) -> MediaResult<()> {
        let needs_create =
            !matches!(&self.audio_decoder, Some(cached) if cached.stream_index == stream_index);
        if needs_create {
            let cached = create_audio_decoder(&self.input_ctx, stream_index)?;
            self.audio_decoder = Some(cached);
        }
        Ok(())
    }
}

impl MediaReader for FfmpegDecoder {
    fn open(path: &Path) -> MediaResult<Self> {
        init_ffmpeg();

        let input_ctx = ffmpeg::format::input(path)
            .map_err(|e| MediaError::Other(format!("cannot open {}: {e}", path.display())))?;

        let info = build_media_info(&input_ctx);

        let video_stream_index = input_ctx
            .streams()
            .best(MediaType::Video)
            .map(|s| s.index());

        let audio_stream_index = input_ctx
            .streams()
            .best(MediaType::Audio)
            .map(|s| s.index());

        // Try to initialize hardware acceleration.
        let config = HwAccelConfig::default();
        let hw_device_ctx = HwDeviceContext::try_create(&config).unwrap_or_else(|e| {
            warn!("HW device context creation failed: {e}");
            None
        });

        Ok(Self {
            input_ctx,
            info,
            video_stream_index,
            audio_stream_index,
            video_decoder: None,
            audio_decoder: None,
            hw_device_ctx,
            input_color_space: ColorSpace::WORKING,
        })
    }

    fn info(&self) -> &MediaInfo {
        &self.info
    }

    fn decode_video_frame(
        &mut self,
        stream_index: usize,
        frame_number: u64,
    ) -> MediaResult<FrameBuffer> {
        // Copied out before the mutable borrow of the cached decoder below.
        let input_color_space = self.input_color_space;
        self.ensure_video_decoder(stream_index)?;
        let cached = self.video_decoder.as_mut().unwrap();

        let time_base = cached.time_base;
        let frame_rate = cached.frame_rate;

        // The target position, in two different units.
        //
        // `target_pts` is in the stream's own time base and is what packet
        // timestamps are compared against below. `target_micros` is the same
        // instant in `AV_TIME_BASE` units, which is what seeking needs:
        // `Input::seek` calls `avformat_seek_file` with `stream_index = -1`,
        // and FFmpeg documents that timestamps are then in `AV_TIME_BASE`
        // (microseconds) rather than any stream's time base.
        //
        // Passing stream-time-base ticks here used to make every seek land
        // near the start of the file — for a typical `1/12800` time base,
        // frame 100 at 24 fps asks for tick 53333, which reads back as
        // 53 ms. The decode loop then walked forward from there to the
        // target, so the cost of one frame grew with its index.
        let SeekTarget {
            pts: target_pts,
            micros: target_micros,
        } = seek_target(frame_number, frame_rate, time_base);

        // How far ahead the sequential path may read before it gives up and
        // seeks, expressed in `time_base` ticks. One second is a couple of
        // GOPs for typical footage, so a scrub of more than that is faster
        // served by a keyframe seek.
        let forward_scan_limit_pts = ticks_per_second(time_base);

        // Playback asks for frames in order, so the common case is "the next
        // frame after the one just returned". Seeking for that would throw
        // away the decoder state and re-decode from the preceding keyframe
        // every time — on a 60-frame GOP, ~30 wasted frames per displayed
        // frame. Continue reading instead whenever the target lies ahead of
        // the last frame returned; the loop below already stops at the first
        // frame whose pts reaches the target.
        //
        // The window is capped so a long forward jump still seeks: walking
        // forward only wins while it stays cheaper than landing on a
        // keyframe.
        let can_continue = cached
            .last_returned_pts
            .is_some_and(|last| target_pts > last && target_pts - last <= forward_scan_limit_pts);

        if !can_continue {
            // Flush the decoder to discard buffered frames from the previous
            // decode position.
            cached.decoder.flush();

            // Seek to the nearest keyframe at or before the target. The range
            // is open at the start so FFmpeg may rewind as far as it needs to
            // reach one; capping it at the target keeps it from overshooting.
            if frame_number == 0 {
                let _ = self.input_ctx.seek(0, ..=0);
            } else {
                self.input_ctx
                    .seek(target_micros, ..=target_micros)
                    .map_err(|_| MediaError::SeekFailed(frame_number))?;
            }
        }

        let mut decoded_frame = frame::Video::empty();
        let mut best_frame: Option<frame::Video> = None;

        for result in self.input_ctx.packets() {
            let (stream, packet) =
                result.map_err(|e| MediaError::DecodeError(format!("read packet: {e}")))?;

            if stream.index() != stream_index {
                continue;
            }

            let decoder = &mut cached.decoder;

            decoder
                .send_packet(&packet)
                .map_err(|e| MediaError::DecodeError(format!("send packet: {e}")))?;

            while decoder.receive_frame(&mut decoded_frame).is_ok() {
                let pts = decoded_frame.pts().unwrap_or(0);

                if pts >= target_pts {
                    // Remember where playback stopped so the next forward
                    // request can continue instead of seeking.
                    cached.last_returned_pts = Some(pts);
                    let sw_frame = ensure_sw_frame(&decoded_frame)?;
                    return convert_decoded_video_frame(
                        cached,
                        sw_frame.as_ref().unwrap_or(&decoded_frame),
                        input_color_space,
                    );
                }

                let mut stash = frame::Video::empty();
                std::mem::swap(&mut stash, &mut decoded_frame);
                best_frame = Some(stash);
            }
        }

        // Flush decoder.
        let decoder = &mut cached.decoder;
        decoder
            .send_eof()
            .map_err(|e| MediaError::DecodeError(format!("flush: {e}")))?;
        while decoder.receive_frame(&mut decoded_frame).is_ok() {
            let pts = decoded_frame.pts().unwrap_or(0);
            if pts >= target_pts {
                // Drained at EOF: the decoder holds no more packets, so the
                // next request has to seek regardless.
                cached.last_returned_pts = None;
                let sw_frame = ensure_sw_frame(&decoded_frame)?;
                return convert_decoded_video_frame(
                    cached,
                    sw_frame.as_ref().unwrap_or(&decoded_frame),
                    input_color_space,
                );
            }
            let mut stash = frame::Video::empty();
            std::mem::swap(&mut stash, &mut decoded_frame);
            best_frame = Some(stash);
        }

        if let Some(ref frame) = best_frame {
            cached.last_returned_pts = None;
            let sw_frame = ensure_sw_frame(frame)?;
            return convert_decoded_video_frame(
                cached,
                sw_frame.as_ref().unwrap_or(frame),
                input_color_space,
            );
        }

        Err(MediaError::SeekFailed(frame_number))
    }

    fn decode_audio_chunk(
        &mut self,
        stream_index: usize,
        start_sample: u64,
        sample_count: usize,
    ) -> MediaResult<AudioBuffer> {
        self.ensure_audio_decoder(stream_index)?;
        let cached = self.audio_decoder.as_ref().unwrap();

        let sample_rate = cached.sample_rate;
        let channels = cached.channels;
        let time_base = cached.time_base;
        let start_pts = cached.start_pts;

        if sample_count == 0 {
            return Ok(AudioBuffer::new(sample_rate, channels, Vec::new()));
        }

        // Flush the decoder before seeking.
        self.audio_decoder.as_mut().unwrap().decoder.flush();

        // Container-wide seek uses AV_TIME_BASE microseconds, while decoded
        // frame timestamps remain in the stream's own time base.
        let target = seek_target(
            start_sample,
            ffmpeg::Rational::new(sample_rate as i32, 1),
            time_base,
        );

        let stream_start_micros = pts_to_micros(start_pts, time_base);
        let absolute_target_micros = stream_start_micros.saturating_add(target.micros);
        self.input_ctx
            .seek(absolute_target_micros, ..=absolute_target_micros)
            .map_err(|_| MediaError::SeekFailed(start_sample))?;

        let mut collector = AudioChunkCollector::new(
            channels,
            sample_rate,
            time_base,
            start_pts,
            start_sample,
            sample_count,
        );
        let mut decoded_frame = frame::Audio::empty();

        for result in self.input_ctx.packets() {
            let (stream, packet) =
                result.map_err(|e| MediaError::DecodeError(format!("read packet: {e}")))?;

            if stream.index() != stream_index {
                continue;
            }

            let decoder = &mut self.audio_decoder.as_mut().unwrap().decoder;

            decoder
                .send_packet(&packet)
                .map_err(|e| MediaError::DecodeError(format!("send packet: {e}")))?;

            while decoder.receive_frame(&mut decoded_frame).is_ok() {
                if collector.push(&decoded_frame)? {
                    return Ok(AudioBuffer::new(sample_rate, channels, collector.finish()));
                }
            }
        }

        // Flush decoder.
        let decoder = &mut self.audio_decoder.as_mut().unwrap().decoder;
        decoder
            .send_eof()
            .map_err(|e| MediaError::DecodeError(format!("flush: {e}")))?;
        while decoder.receive_frame(&mut decoded_frame).is_ok() {
            if collector.push(&decoded_frame)? {
                break;
            }
        }

        Ok(AudioBuffer::new(sample_rate, channels, collector.finish()))
    }
}

fn audio_pts_to_sample(pts: i64, time_base: ffmpeg::Rational, sample_rate: u32) -> i64 {
    let numerator = i128::from(pts)
        .saturating_mul(i128::from(time_base.numerator()))
        .saturating_mul(i128::from(sample_rate));
    let denominator = i128::from(time_base.denominator()).max(1);
    i64::try_from(numerator.div_euclid(denominator)).unwrap_or_else(|_| {
        if numerator.is_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}

fn pts_to_micros(pts: i64, time_base: ffmpeg::Rational) -> i64 {
    audio_pts_to_sample(pts, time_base, AV_TIME_BASE as u32)
}

struct AudioChunkCollector {
    channels: u32,
    sample_rate: u32,
    time_base: ffmpeg::Rational,
    stream_start_pts: i64,
    target_sample: u64,
    sample_count: usize,
    next_frame_sample: Option<i64>,
    collected: Vec<f32>,
}

impl AudioChunkCollector {
    fn new(
        channels: u32,
        sample_rate: u32,
        time_base: ffmpeg::Rational,
        stream_start_pts: i64,
        target_sample: u64,
        sample_count: usize,
    ) -> Self {
        Self {
            channels,
            sample_rate,
            time_base,
            stream_start_pts,
            target_sample,
            sample_count,
            next_frame_sample: None,
            collected: Vec::with_capacity(sample_count.saturating_mul(channels as usize)),
        }
    }

    fn push(&mut self, frame: &frame::Audio) -> MediaResult<bool> {
        let frame_start = self.frame_start_sample(frame.pts());
        let frame_samples = frame.samples() as i64;
        self.next_frame_sample = Some(frame_start.saturating_add(frame_samples));

        let samples = extract_audio_samples(frame, self.channels)?;
        Ok(self.push_positioned_samples(frame_start, &samples))
    }

    fn frame_start_sample(&self, pts: Option<i64>) -> i64 {
        let fallback = self.target_sample.min(i64::MAX as u64) as i64;
        let Some(pts) = pts else {
            return self.next_frame_sample.unwrap_or(fallback);
        };
        let timestamp_position = audio_pts_to_sample(
            pts.saturating_sub(self.stream_start_pts),
            self.time_base,
            self.sample_rate,
        );
        let Some(contiguous_position) = self.next_frame_sample else {
            return timestamp_position;
        };

        // A coarse stream time base cannot represent every audio sample.
        // Treat sub-tick discrepancies as timestamp quantization, while
        // preserving larger discontinuities as real gaps or overlaps.
        let tick_samples = timestamp_tick_samples(self.time_base, self.sample_rate);
        if timestamp_position.abs_diff(contiguous_position) <= tick_samples {
            contiguous_position
        } else {
            timestamp_position
        }
    }

    fn push_positioned_samples(&mut self, frame_start: i64, samples: &[f32]) -> bool {
        let channels = self.channels as usize;
        let target_len = self.sample_count.saturating_mul(channels);
        let frame_samples = (samples.len() / channels.max(1)) as i64;
        let frame_end = frame_start.saturating_add(frame_samples);
        let target = self.target_sample.min(i64::MAX as u64) as i64;
        let collected_frames = self.collected.len() / channels.max(1);
        let output_position = target.saturating_add(collected_frames as i64);

        if frame_end <= output_position {
            return false;
        }

        if frame_start > output_position {
            let gap_frames = usize::try_from(frame_start - output_position).unwrap_or(usize::MAX);
            let gap_samples = gap_frames
                .saturating_mul(channels)
                .min(target_len.saturating_sub(self.collected.len()));
            self.collected
                .resize(self.collected.len() + gap_samples, 0.0);
            if self.collected.len() >= target_len {
                return true;
            }
        }

        let collected_frames = self.collected.len() / channels.max(1);
        let output_position = target.saturating_add(collected_frames as i64);
        let trim_frames = output_position.saturating_sub(frame_start).max(0) as usize;
        let trim_samples = trim_frames.saturating_mul(channels).min(samples.len());
        let needed = target_len.saturating_sub(self.collected.len());
        let available = &samples[trim_samples..];
        self.collected
            .extend_from_slice(&available[..available.len().min(needed)]);
        self.collected.len() >= target_len
    }

    fn finish(mut self) -> Vec<f32> {
        self.collected
            .truncate(self.sample_count.saturating_mul(self.channels as usize));
        self.collected
    }
}

fn timestamp_tick_samples(time_base: ffmpeg::Rational, sample_rate: u32) -> u64 {
    let numerator = i128::from(time_base.numerator())
        .abs()
        .saturating_mul(i128::from(sample_rate));
    let denominator = i128::from(time_base.denominator()).abs().max(1);
    let ceiling = numerator
        .saturating_add(denominator.saturating_sub(1))
        .div_euclid(denominator)
        .max(1);
    u64::try_from(ceiling).unwrap_or(u64::MAX)
}

// ===========================================================================
// Internal helpers
// ===========================================================================

/// Build [`MediaInfo`] from an opened FFmpeg input context.
fn build_media_info(ctx: &Input) -> MediaInfo {
    let format_name = ctx.format().name().to_string();
    // SAFETY: `ctx.as_ptr()` returns a valid `*const AVFormatContext`
    // that is alive for the duration of `ctx`.
    let url = unsafe { std::ffi::CStr::from_ptr((*ctx.as_ptr()).url) }
        .to_str()
        .unwrap_or("");
    let container = detect_container_from_url(url).or_else(|| detect_container(&format_name));

    let duration_secs = if ctx.duration() >= 0 {
        Some(ctx.duration() as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE))
    } else {
        None
    };

    let streams = ctx
        .streams()
        .filter_map(|stream| {
            let codec_params = stream.parameters();
            match codec_params.medium() {
                MediaType::Video => {
                    let codec_name = codec_params.id().name().to_string();
                    let codec = VideoCodec::from_ffmpeg_name(&codec_name);
                    let rate = stream.rate();
                    let time_base = stream.time_base();
                    let frame_rate = if rate.numerator() > 0 && rate.denominator() > 0 {
                        FrameRate::new(rate.numerator() as u32, rate.denominator() as u32)
                    } else {
                        FrameRate::new(30, 1)
                    };

                    let frame_count = if stream.frames() > 0 {
                        Some(stream.frames() as u64)
                    } else {
                        None
                    };

                    let duration_secs = if stream.duration() > 0 && time_base.numerator() > 0 {
                        Some(
                            stream.duration() as f64 * time_base.numerator() as f64
                                / time_base.denominator() as f64,
                        )
                    } else {
                        None
                    };

                    let (width, height) = extract_video_dimensions(&codec_params);

                    Some(StreamInfo::Video(VideoStreamInfo {
                        stream_index: stream.index(),
                        codec,
                        codec_name,
                        width,
                        height,
                        frame_rate,
                        frame_count,
                        duration_secs,
                        pixel_format: String::new(),
                        color_primaries: probe_primaries(codec_params.color_primaries()),
                        color_transfer: probe_transfer(
                            codec_params.color_transfer_characteristic(),
                        ),
                        color_matrix: codec_params.color_space().name().map(str::to_owned),
                    }))
                }
                MediaType::Audio => {
                    let codec_name = codec_params.id().name().to_string();
                    let codec = AudioCodec::from_ffmpeg_name(&codec_name);
                    let time_base = stream.time_base();

                    let (sample_rate, channels) = extract_audio_params(&codec_params);

                    let duration_secs = if stream.duration() > 0 && time_base.numerator() > 0 {
                        Some(
                            stream.duration() as f64 * time_base.numerator() as f64
                                / time_base.denominator() as f64,
                        )
                    } else {
                        None
                    };

                    Some(StreamInfo::Audio(AudioStreamInfo {
                        stream_index: stream.index(),
                        codec,
                        codec_name,
                        sample_rate,
                        channels,
                        sample_count: None,
                        duration_secs,
                    }))
                }
                _ => None,
            }
        })
        .collect();

    MediaInfo {
        container,
        container_name: format_name,
        streams,
        duration_secs,
    }
}

/// Detect container from the file URL/path extension.
fn detect_container_from_url(url: &str) -> Option<ContainerFormat> {
    let path = std::path::Path::new(url);
    let ext = path.extension()?.to_str()?;
    ContainerFormat::from_extension(ext)
}

/// Map FFmpeg format name to our [`ContainerFormat`].
fn detect_container(name: &str) -> Option<ContainerFormat> {
    for part in name.split(',') {
        match part.trim() {
            "mp4" | "m4a" | "m4v" => return Some(ContainerFormat::Mp4),
            "mov" => return Some(ContainerFormat::Mov),
            "matroska" | "mkv" => return Some(ContainerFormat::Mkv),
            "webm" => return Some(ContainerFormat::WebM),
            _ => {}
        }
    }
    None
}

/// Extract video width and height from codec parameters.
fn extract_video_dimensions(params: &ffmpeg::codec::ParametersRef<'_>) -> (u32, u32) {
    unsafe {
        let ptr = params.as_ptr();
        ((*ptr).width as u32, (*ptr).height as u32)
    }
}

/// Map FFmpeg's declared colour primaries onto Ravel's vocabulary. Anything
/// Ravel cannot name stays `None` — the input-colour-space resolution then
/// falls through to the extension default instead of guessing
/// (`docs/specifications/color-management.md`).
fn probe_primaries(primaries: color::Primaries) -> Option<Primaries> {
    match primaries {
        color::Primaries::BT709 => Some(Primaries::Rec709),
        color::Primaries::BT2020 => Some(Primaries::Rec2020),
        _ => None,
    }
}

/// Map FFmpeg's declared transfer characteristic onto Ravel's vocabulary,
/// with the same `None`-means-unknown rule as [`probe_primaries`].
fn probe_transfer(trc: color::TransferCharacteristic) -> Option<Transfer> {
    match trc {
        color::TransferCharacteristic::Linear => Some(Transfer::Linear),
        color::TransferCharacteristic::IEC61966_2_1 => Some(Transfer::Srgb),
        // BT.2020 encodes with the BT.709 OETF; FFmpeg spells it per depth.
        color::TransferCharacteristic::BT709
        | color::TransferCharacteristic::BT2020_10
        | color::TransferCharacteristic::BT2020_12 => Some(Transfer::Rec709),
        color::TransferCharacteristic::SMPTE2084 => Some(Transfer::Pq),
        _ => None,
    }
}

/// Extract audio sample rate and channel count from codec parameters.
fn extract_audio_params(params: &ffmpeg::codec::ParametersRef<'_>) -> (u32, u32) {
    unsafe {
        let ptr = params.as_ptr();
        let sample_rate = (*ptr).sample_rate as u32;
        let channels = (*ptr).ch_layout.nb_channels as u32;
        (sample_rate, channels)
    }
}

/// Convert one decoded frame, switching to the pre-cache seam the performance
/// harness measures against.
fn convert_decoded_video_frame(
    cached: &mut CachedVideoDecoder,
    frame: &frame::Video,
    input_color_space: ColorSpace,
) -> MediaResult<FrameBuffer> {
    #[cfg(test)]
    if cached.legacy_conversion {
        return convert_video_frame_to_rgba_legacy(frame, input_color_space);
    }

    convert_video_frame_to_rgba(frame, input_color_space, &mut cached.scaler)
}

/// Convert an FFmpeg video frame to RGBA f32 [`FrameBuffer`], decoding
/// `input_color_space` into the working space on the way.
///
/// This is the ingest end of the linear pipeline (`CM-2`): the transfer
/// function is removed **immediately** after the samples are normalised, so
/// nothing downstream ever sees an encoded value. Alpha carries no transfer
/// function and is copied through.
///
/// The road the samples take depends on how much precision the decoded
/// format carries — never on the file extension:
///
/// - **Float RGB formats** (EXR and other scene-referred sources) are read
///   plane by plane, without the scaler: no quantisation and **no clamp**,
///   so values above 1.0 reach the working space.
/// - **Formats deeper than 8 bits** (ProRes 422 10-bit, DNxHR, 16-bit
///   stills) scale to RGBA64 and ingest at 16 bits.
/// - **Everything else** keeps the 8-bit RGBA path it has always had; its
///   output is unchanged bit for bit.
fn convert_video_frame_to_rgba(
    frame: &frame::Video,
    input_color_space: ColorSpace,
    scaler: &mut ScalerCache,
) -> MediaResult<FrameBuffer> {
    let width = frame.width();
    let height = frame.height();

    if width == 0 || height == 0 {
        return Err(MediaError::DecodeError(
            "decoded frame has zero dimensions".into(),
        ));
    }

    if let Some(layout) = FloatRgbLayout::of(frame.format()) {
        return read_float_rgb_frame(frame, layout, input_color_space);
    }

    if source_depth(frame.format()) > 8 && !scaler.deep_scale_known_broken(frame.format()) {
        match scaler.run(frame, DEEP_OUTPUT) {
            Ok(scaled) => return Ok(framebuffer_from_rgba64(scaled, input_color_space)),
            Err(error) => {
                // sws support is a property of the format pair, not of this
                // frame, so record the refusal: without it the next frame
                // retries the deep scale, fails again, and rebuilds the RGBA
                // scaler the fallback just replaced.
                scaler.mark_deep_scale_broken(frame.format());
                warn!(
                    format = ?frame.format(),
                    %error,
                    "16-bit scaling unavailable; decoding through 8-bit RGBA"
                );
            }
        }
    }

    let scaled = scaler.run(frame, PixelFormat::RGBA)?;
    Ok(framebuffer_from_rgba8(scaled, input_color_space))
}

/// The scaler output format for sources deeper than 8 bits, in host byte
/// order so the u16 lanes read back without a swap.
#[cfg(target_endian = "little")]
const DEEP_OUTPUT: PixelFormat = PixelFormat::RGBA64LE;
#[cfg(target_endian = "big")]
const DEEP_OUTPUT: PixelFormat = PixelFormat::RGBA64BE;

/// Bits per sample of a pixel format — the deepest component. Formats
/// FFmpeg cannot describe are treated as 8-bit, which is the path they took
/// before the deep paths existed.
fn source_depth(format: PixelFormat) -> u32 {
    // SAFETY: the returned descriptor is a static table entry owned by
    // libavutil; it outlives this read and is never mutated.
    let desc = unsafe { ffi::av_pix_fmt_desc_get(format.into()) };
    if desc.is_null() {
        return 8;
    }
    let desc = unsafe { &*desc };
    (0..desc.nb_components as usize)
        .map(|index| desc.comp[index].depth as u32)
        .max()
        .unwrap_or(8)
}

/// Ingest an 8-bit RGBA frame into the working space.
fn framebuffer_from_rgba8(frame: &frame::Video, input_color_space: ColorSpace) -> FrameBuffer {
    let (width, height) = (frame.width(), frame.height());
    let stride = frame.stride(0);
    let data = frame.data(0);
    let pixel_count = (width * height) as usize;
    let mut f32_data = Vec::with_capacity(pixel_count * 4);

    for y in 0..height as usize {
        let row_start = y * stride;
        for x in 0..width as usize {
            let offset = row_start + x * 4;
            f32_data.extend_from_slice(&ingest_rgba8(
                [
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ],
                input_color_space,
            ));
        }
    }

    FrameBuffer::from_f32(width, height, f32_data)
}

/// Ingest a 16-bit RGBA frame (native-endian, see [`DEEP_OUTPUT`]) into the
/// working space.
fn framebuffer_from_rgba64(frame: &frame::Video, input_color_space: ColorSpace) -> FrameBuffer {
    let (width, height) = (frame.width(), frame.height());
    let stride = frame.stride(0);
    let data = frame.data(0);
    let pixel_count = (width * height) as usize;
    let mut f32_data = Vec::with_capacity(pixel_count * 4);

    let lane = |offset: usize| u16::from_ne_bytes([data[offset], data[offset + 1]]);
    for y in 0..height as usize {
        let row_start = y * stride;
        for x in 0..width as usize {
            let offset = row_start + x * 8;
            f32_data.extend_from_slice(&ingest_rgba16(
                [
                    lane(offset),
                    lane(offset + 2),
                    lane(offset + 4),
                    lane(offset + 6),
                ],
                input_color_space,
            ));
        }
    }

    FrameBuffer::from_f32(width, height, f32_data)
}

/// How a float RGB frame lays its samples out. These formats are read
/// directly rather than scaled: the scaler's float support varies by FFmpeg
/// version, and an integer intermediate would clamp the highlights these
/// formats exist to carry.
#[derive(Clone, Copy, Debug)]
enum FloatRgbLayout {
    /// `GBRPF32`: one f32 plane per channel, in G, B, R order; opaque.
    Planar { big_endian: bool },
    /// `GBRAPF32`: planar, with an alpha plane after the colour planes.
    PlanarAlpha { big_endian: bool },
    /// `RGBAF32`: interleaved.
    Packed { big_endian: bool },
}

impl FloatRgbLayout {
    fn of(format: PixelFormat) -> Option<Self> {
        Some(match format {
            PixelFormat::GBRPF32LE => Self::Planar { big_endian: false },
            PixelFormat::GBRPF32BE => Self::Planar { big_endian: true },
            PixelFormat::GBRAPF32LE => Self::PlanarAlpha { big_endian: false },
            PixelFormat::GBRAPF32BE => Self::PlanarAlpha { big_endian: true },
            PixelFormat::RGBAF32LE => Self::Packed { big_endian: false },
            PixelFormat::RGBAF32BE => Self::Packed { big_endian: true },
            _ => return None,
        })
    }

    fn sample(self, data: &[u8], offset: usize) -> f32 {
        let bytes = [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ];
        match self {
            Self::Planar { big_endian }
            | Self::PlanarAlpha { big_endian }
            | Self::Packed { big_endian }
                if big_endian =>
            {
                f32::from_be_bytes(bytes)
            }
            _ => f32::from_le_bytes(bytes),
        }
    }
}

/// Read a float RGB frame straight into the working space. No scaling, no
/// quantisation, no clamp — 1.0 is not a ceiling here.
fn read_float_rgb_frame(
    frame: &frame::Video,
    layout: FloatRgbLayout,
    input_color_space: ColorSpace,
) -> MediaResult<FrameBuffer> {
    let (width, height) = (frame.width(), frame.height());
    let pixel_count = (width * height) as usize;
    let row_len = width as usize * 4;
    debug_assert!(row_len > 0, "float ingest requires a non-zero row width");
    let mut f32_data = vec![0.0f32; pixel_count * 4];

    match layout {
        FloatRgbLayout::Planar { .. } | FloatRgbLayout::PlanarAlpha { .. } => {
            // GBRP order: plane 0 is green, 1 is blue, 2 is red.
            let planes = [frame.data(2), frame.data(0), frame.data(1)];
            let strides = [frame.stride(2), frame.stride(0), frame.stride(1)];
            let alpha = match layout {
                FloatRgbLayout::PlanarAlpha { .. } => Some((frame.data(3), frame.stride(3))),
                _ => None,
            };
            f32_data
                .par_chunks_exact_mut(row_len)
                .enumerate()
                .for_each(|(y, out_row)| {
                    for x in 0..width as usize {
                        let rgb =
                            [0, 1, 2].map(|c| layout.sample(planes[c], y * strides[c] + x * 4));
                        let a = alpha.map_or(1.0, |(data, stride)| {
                            layout.sample(data, y * stride + x * 4)
                        });
                        let pixel = ingest_rgbaf32([rgb[0], rgb[1], rgb[2], a], input_color_space);
                        out_row[x * 4..x * 4 + 4].copy_from_slice(&pixel);
                    }
                });
        }
        FloatRgbLayout::Packed { .. } => {
            let data = frame.data(0);
            let stride = frame.stride(0);
            f32_data
                .par_chunks_exact_mut(row_len)
                .enumerate()
                .for_each(|(y, out_row)| {
                    for x in 0..width as usize {
                        let offset = y * stride + x * 16;
                        let pixel = ingest_rgbaf32(
                            [
                                layout.sample(data, offset),
                                layout.sample(data, offset + 4),
                                layout.sample(data, offset + 8),
                                layout.sample(data, offset + 12),
                            ],
                            input_color_space,
                        );
                        out_row[x * 4..x * 4 + 4].copy_from_slice(&pixel);
                    }
                });
        }
    }

    Ok(FrameBuffer::from_f32(width, height, f32_data))
}

/// Reconstruct the pre-cache conversion path for the performance harness.
/// It differs from [`convert_video_frame_to_rgba`] in exactly one respect —
/// the scaler is built per frame instead of reused — so the harness isolates
/// scaler reuse from every other difference, including the already-landed
/// transfer-function optimisations, which both paths share.
#[cfg(test)]
fn convert_video_frame_to_rgba_legacy(
    frame: &frame::Video,
    input_color_space: ColorSpace,
) -> MediaResult<FrameBuffer> {
    if let Some(layout) = FloatRgbLayout::of(frame.format()) {
        return read_float_rgb_frame(frame, layout, input_color_space);
    }

    if source_depth(frame.format()) > 8
        && let Ok(scaled) = scale_frame_legacy(frame, DEEP_OUTPUT)
    {
        return Ok(framebuffer_from_rgba64(&scaled, input_color_space));
    }

    let scaled = scale_frame_legacy(frame, PixelFormat::RGBA)?;
    Ok(framebuffer_from_rgba8(&scaled, input_color_space))
}

#[cfg(test)]
fn scale_frame_legacy(frame: &frame::Video, output: PixelFormat) -> MediaResult<frame::Video> {
    let mut scaler = sws::Context::get(
        frame.format(),
        frame.width(),
        frame.height(),
        output,
        frame.width(),
        frame.height(),
        sws::Flags::BILINEAR,
    )
    .map_err(|e| MediaError::DecodeError(format!("create scaler: {e}")))?;

    let mut scaled = frame::Video::empty();
    scaler
        .run(frame, &mut scaled)
        .map_err(|e| MediaError::DecodeError(format!("scale frame: {e}")))?;
    Ok(scaled)
}

/// Map an FFmpeg sample format onto its numeric encoding.
///
/// Returns `None` for `AV_SAMPLE_FMT_NONE`, which a frame only carries when
/// the decoder produced nothing usable.
fn sample_encoding(format: SampleFormat) -> Option<SampleEncoding> {
    Some(match format {
        SampleFormat::U8(_) => SampleEncoding::U8,
        SampleFormat::I16(_) => SampleEncoding::S16,
        SampleFormat::I32(_) => SampleEncoding::S32,
        SampleFormat::I64(_) => SampleEncoding::S64,
        SampleFormat::F32(_) => SampleEncoding::F32,
        SampleFormat::F64(_) => SampleEncoding::F64,
        SampleFormat::None => return None,
    })
}

/// Borrow an audio frame's raw plane bytes.
///
/// This deliberately bypasses `frame::Audio::data()`. FFmpeg only fills
/// `AVFrame::linesize[0]` for audio — the remaining entries stay zero — while
/// `data()` sizes plane `i` from `linesize[i]`. For a planar frame that makes
/// every channel past the first look like an empty plane, which is how
/// planar sources (AAC, and therefore the audio of most video files) used to
/// decode with only channel 0 carrying signal.
///
/// The plane sizes are derived from the frame's own geometry instead:
/// `nb_samples` samples per planar plane, `nb_samples × channels` for a
/// packed one.
fn audio_planes(frame: &frame::Audio, planar: bool, channels: usize) -> Vec<&[u8]> {
    let width = frame.format().bytes();
    let (plane_count, plane_len) = if planar {
        (channels, frame.samples() * width)
    } else {
        (1, frame.samples() * channels * width)
    };

    let mut planes = Vec::with_capacity(plane_count);
    // SAFETY: `extended_data` points to at least `plane_count` plane pointers
    // for the lifetime of the frame; it aliases `data` for the first eight
    // planes and is separately allocated beyond that, so `data` is only read
    // for indices inside its fixed-size array. Each plane FFmpeg allocated
    // holds at least `plane_len` bytes — `av_samples_get_buffer_size` rounds
    // the sample count up to a multiple of 32, never down. The returned
    // slices borrow `frame`, so they cannot outlive the buffer.
    unsafe {
        let raw = frame.as_ptr();
        let extended = (*raw).extended_data;
        for index in 0..plane_count {
            let data = if !extended.is_null() {
                *extended.add(index)
            } else if index < (*raw).data.len() {
                (*raw).data[index]
            } else {
                break;
            };
            if data.is_null() {
                break;
            }
            planes.push(std::slice::from_raw_parts(data, plane_len));
        }
    }
    planes
}

/// Extract interleaved f32 samples from an FFmpeg audio frame.
///
/// `channels` is the interleave stride the caller's [`AudioBuffer`] promises,
/// taken from the stream header. A frame that disagrees with it is mapped
/// onto that stride rather than allowed to shift the interleave — see
/// [`audio_sample::to_packed_f32`].
fn extract_audio_samples(frame: &frame::Audio, channels: u32) -> MediaResult<Vec<f32>> {
    let samples = frame.samples();
    let out_channels = channels as usize;
    if samples == 0 || out_channels == 0 {
        return Ok(Vec::new());
    }

    let format = frame.format();
    let encoding = sample_encoding(format)
        .ok_or_else(|| MediaError::UnsupportedSampleFormat(format!("{format:?}")))?;
    let planar = format.is_planar();
    let frame_channels = frame.ch_layout().channels() as usize;
    if frame_channels != out_channels {
        debug!(
            frame_channels,
            declared = out_channels,
            "audio frame channel count differs from the stream header"
        );
    }

    let planes = audio_planes(frame, planar, frame_channels);
    Ok(audio_sample::to_packed_f32(
        &planes,
        audio_sample::FrameSpec {
            encoding,
            planar,
            channels: frame_channels,
            samples,
        },
        out_channels,
    ))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The seek timestamp must be in `AV_TIME_BASE` units, not the stream's.
    /// Conflating them made every seek land near the start of the file, so
    /// decoding frame N cost O(N) (audio-plan/media perf regression).
    #[test]
    fn seek_target_reports_microseconds_independently_of_the_time_base() {
        let rate = ffmpeg::Rational::new(24, 1);
        // A 1/12800 time base is what a typical H.264 mp4 carries.
        let fine = ffmpeg::Rational::new(1, 12800);
        let target = seek_target(240, rate, fine);

        // 240 frames at 24 fps is exactly 10 s.
        assert_eq!(target.micros, 10_000_000);
        assert_eq!(target.pts, 128_000);

        // The same instant, described by a coarser stream clock: the pts
        // changes, the microsecond position must not.
        let coarse = ffmpeg::Rational::new(1, 600);
        let target = seek_target(240, rate, coarse);
        assert_eq!(target.micros, 10_000_000);
        assert_eq!(target.pts, 6_000);
    }

    #[test]
    fn seek_target_handles_frame_zero_and_unusable_rates() {
        let fine = ffmpeg::Rational::new(1, 12800);
        let zero = seek_target(0, ffmpeg::Rational::new(24, 1), fine);
        assert_eq!(zero.pts, 0);
        assert_eq!(zero.micros, 0);

        // A stream with no declared rate must not divide by zero.
        let unusable = seek_target(7, ffmpeg::Rational::new(0, 1), fine);
        assert_eq!(unusable.pts, 7);
        assert_eq!(unusable.micros, 0);
    }

    #[test]
    fn audio_seek_target_uses_microseconds_and_stream_pts_for_the_same_sample() {
        let target = seek_target(
            220_500,
            ffmpeg::Rational::new(44_100, 1),
            ffmpeg::Rational::new(1, 44_100),
        );
        assert_eq!(target.micros, 5_000_000);
        assert_eq!(target.pts, 220_500);
    }

    #[test]
    fn audio_pts_convert_to_sample_positions_without_float_rounding() {
        assert_eq!(
            audio_pts_to_sample(22_050, ffmpeg::Rational::new(1, 44_100), 44_100),
            22_050
        );
        assert_eq!(
            audio_pts_to_sample(500, ffmpeg::Rational::new(1, 1_000), 48_000),
            24_000
        );
    }

    #[test]
    fn audio_collector_normalizes_start_pts_and_places_gaps_and_overlaps() {
        let mut collector =
            AudioChunkCollector::new(1, 48_000, ffmpeg::Rational::new(1, 48_000), 96_000, 2, 8);

        assert!(!collector.push_positioned_samples(0, &[0.0, 1.0, 2.0, 3.0]));
        // Position 4 is absent, so it becomes silence. The frame then starts
        // at 5 and contributes two samples.
        assert!(!collector.push_positioned_samples(5, &[5.0, 6.0]));
        // This frame overlaps position 6; only positions 7 and 8 are new.
        assert!(collector.push_positioned_samples(6, &[60.0, 7.0, 8.0, 9.0]));

        assert_eq!(
            collector.finish(),
            vec![2.0, 3.0, 0.0, 5.0, 6.0, 7.0, 8.0, 9.0]
        );
        assert_eq!(
            audio_pts_to_sample(
                120_000_i64.saturating_sub(96_000),
                ffmpeg::Rational::new(1, 48_000),
                48_000,
            ),
            24_000
        );
    }

    #[test]
    fn audio_collector_snaps_coarse_timestamp_quantization_to_contiguous_frames() {
        let mut collector =
            AudioChunkCollector::new(1, 44_100, ffmpeg::Rational::new(1, 1_000), 2_000, 0, 20_000);

        assert_eq!(collector.frame_start_sample(Some(2_000)), 0);
        collector.next_frame_sample = Some(4_608);
        // 104 ms floors to sample 4,586, but the decoder's preceding frame
        // ends at 4,608. The 22-sample difference is below one 44.1-sample
        // timestamp tick and must not become an overlap.
        assert_eq!(collector.frame_start_sample(Some(2_104)), 4_608);

        collector.next_frame_sample = Some(9_216);
        assert_eq!(collector.frame_start_sample(Some(2_209)), 9_216);

        collector.next_frame_sample = Some(13_824);
        // A multi-tick jump remains a real discontinuity.
        assert_eq!(collector.frame_start_sample(Some(2_400)), 17_640);
    }

    #[test]
    fn forward_scan_limit_is_one_second_of_ticks() {
        assert_eq!(ticks_per_second(ffmpeg::Rational::new(1, 12800)), 12_800);
        assert_eq!(ticks_per_second(ffmpeg::Rational::new(1, 600)), 600);
        // Degenerate numerators must not panic or yield a zero-width window.
        assert_eq!(ticks_per_second(ffmpeg::Rational::new(0, 600)), 600);
        assert!(ticks_per_second(ffmpeg::Rational::new(1, 0)) >= 1);
    }

    #[test]
    fn detect_container_from_format_name() {
        assert_eq!(
            detect_container("mov,mp4,m4a,3gp,3g2,mj2"),
            Some(ContainerFormat::Mov)
        );
        assert_eq!(
            detect_container("matroska,webm"),
            Some(ContainerFormat::Mkv)
        );
        assert_eq!(detect_container("webm"), Some(ContainerFormat::WebM));
        assert_eq!(detect_container("mp4"), Some(ContainerFormat::Mp4));
        assert_eq!(detect_container("avi"), None);
    }

    #[test]
    fn init_ffmpeg_is_idempotent() {
        init_ffmpeg();
        init_ffmpeg();
    }

    #[test]
    fn scaler_cache_recreates_only_when_its_key_changes() {
        init_ffmpeg();
        let mut cache = ScalerCache::default();
        let rgba = frame::Video::new(PixelFormat::RGBA, 4, 4);

        cache.ensure(&rgba, PixelFormat::RGBA).unwrap();
        cache.ensure(&rgba, PixelFormat::RGBA).unwrap();
        assert_eq!(cache.creations(), 1);

        let resized = frame::Video::new(PixelFormat::RGBA, 8, 4);
        cache.ensure(&resized, PixelFormat::RGBA).unwrap();
        assert_eq!(cache.creations(), 2);

        let reformatted = frame::Video::new(PixelFormat::RGB24, 8, 4);
        cache.ensure(&reformatted, PixelFormat::RGBA).unwrap();
        assert_eq!(cache.creations(), 3);

        cache.ensure(&reformatted, PixelFormat::BGRA).unwrap();
        assert_eq!(cache.creations(), 4);
    }

    /// Reconstruct the pre-LUT u8 ingest loop for the measurement harness.
    fn serial_framebuffer_from_rgba8(
        frame: &frame::Video,
        input_color_space: ColorSpace,
    ) -> FrameBuffer {
        let (width, height) = (frame.width(), frame.height());
        let stride = frame.stride(0);
        let data = frame.data(0);
        let mut f32_data = Vec::with_capacity((width * height * 4) as usize);

        for y in 0..height as usize {
            let row_start = y * stride;
            for x in 0..width as usize {
                let offset = row_start + x * 4;
                let norm = |value: u8| f32::from(value) / 255.0;
                let rgb = ravel_core::color::convert(
                    [
                        norm(data[offset]),
                        norm(data[offset + 1]),
                        norm(data[offset + 2]),
                    ],
                    input_color_space,
                    ColorSpace::WORKING,
                );
                f32_data.extend_from_slice(&[rgb[0], rgb[1], rgb[2], norm(data[offset + 3])]);
            }
        }

        FrameBuffer::from_f32(width, height, f32_data)
    }

    /// Reconstruct the pre-LUT u16 ingest loop for the measurement harness.
    fn serial_framebuffer_from_rgba64(
        frame: &frame::Video,
        input_color_space: ColorSpace,
    ) -> FrameBuffer {
        let (width, height) = (frame.width(), frame.height());
        let stride = frame.stride(0);
        let data = frame.data(0);
        let mut f32_data = Vec::with_capacity((width * height * 4) as usize);
        let lane = |offset: usize| u16::from_ne_bytes([data[offset], data[offset + 1]]);

        for y in 0..height as usize {
            let row_start = y * stride;
            for x in 0..width as usize {
                let offset = row_start + x * 8;
                let norm = |value: u16| f32::from(value) / 65_535.0;
                let rgb = ravel_core::color::convert(
                    [
                        norm(lane(offset)),
                        norm(lane(offset + 2)),
                        norm(lane(offset + 4)),
                    ],
                    input_color_space,
                    ColorSpace::WORKING,
                );
                f32_data.extend_from_slice(&[rgb[0], rgb[1], rgb[2], norm(lane(offset + 6))]);
            }
        }

        FrameBuffer::from_f32(width, height, f32_data)
    }

    /// Reconstruct the pre-rayon float ingest loop for the measurement
    /// harness and for an exact output comparison with the parallel path.
    fn serial_read_float_rgb_frame(
        frame: &frame::Video,
        layout: FloatRgbLayout,
        input_color_space: ColorSpace,
    ) -> FrameBuffer {
        let (width, height) = (frame.width(), frame.height());
        let mut f32_data = Vec::with_capacity((width * height * 4) as usize);

        match layout {
            FloatRgbLayout::Planar { .. } | FloatRgbLayout::PlanarAlpha { .. } => {
                let planes = [frame.data(2), frame.data(0), frame.data(1)];
                let strides = [frame.stride(2), frame.stride(0), frame.stride(1)];
                let alpha = match layout {
                    FloatRgbLayout::PlanarAlpha { .. } => Some((frame.data(3), frame.stride(3))),
                    _ => None,
                };
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let rgb =
                            [0, 1, 2].map(|c| layout.sample(planes[c], y * strides[c] + x * 4));
                        let a = alpha.map_or(1.0, |(data, stride)| {
                            layout.sample(data, y * stride + x * 4)
                        });
                        let converted =
                            ravel_core::color::convert(rgb, input_color_space, ColorSpace::WORKING);
                        f32_data.extend_from_slice(&[converted[0], converted[1], converted[2], a]);
                    }
                }
            }
            FloatRgbLayout::Packed { .. } => {
                let data = frame.data(0);
                let stride = frame.stride(0);
                for y in 0..height as usize {
                    for x in 0..width as usize {
                        let offset = y * stride + x * 16;
                        let rgb = [
                            layout.sample(data, offset),
                            layout.sample(data, offset + 4),
                            layout.sample(data, offset + 8),
                        ];
                        let converted =
                            ravel_core::color::convert(rgb, input_color_space, ColorSpace::WORKING);
                        f32_data.extend_from_slice(&[
                            converted[0],
                            converted[1],
                            converted[2],
                            layout.sample(data, offset + 12),
                        ]);
                    }
                }
            }
        }

        FrameBuffer::from_f32(width, height, f32_data)
    }

    #[test]
    fn integer_decoder_ingest_paths_use_the_core_ingest_results() {
        let mut rgba8 = frame::Video::new(PixelFormat::RGBA, 1, 1);
        rgba8.data_mut(0)[..4].copy_from_slice(&[128, 64, 32, 255]);
        let actual = framebuffer_from_rgba8(&rgba8, ColorSpace::SRGB);
        let expected = ravel_core::color::ingest_rgba8([128, 64, 32, 255], ColorSpace::SRGB);
        assert_eq!(actual.as_f32().as_ref(), expected.as_slice());

        let mut rgba64 = frame::Video::new(DEEP_OUTPUT, 1, 1);
        let data = rgba64.data_mut(0);
        for (offset, value) in [32_768u16, 16_384, 8_192, 65_535].into_iter().enumerate() {
            data[offset * 2..offset * 2 + 2].copy_from_slice(&value.to_ne_bytes());
        }
        let actual = framebuffer_from_rgba64(&rgba64, ColorSpace::SRGB);
        let expected =
            ravel_core::color::ingest_rgba16([32_768, 16_384, 8_192, 65_535], ColorSpace::SRGB);
        assert_eq!(actual.as_f32().as_ref(), expected.as_slice());
    }

    #[test]
    fn float_decoder_ingest_matches_the_serial_reference() {
        let mut packed = frame::Video::new(PixelFormat::RGBAF32LE, 2, 3);
        let stride = packed.stride(0);
        let data = packed.data_mut(0);
        for y in 0..3usize {
            for x in 0..2usize {
                let offset = y * stride + x * 16;
                for (channel, value) in [0.1, 0.2, 0.3, 1.0].into_iter().enumerate() {
                    data[offset + channel * 4..offset + channel * 4 + 4]
                        .copy_from_slice(&(value + (x + y) as f32 * 0.01).to_le_bytes());
                }
            }
        }
        let expected = serial_read_float_rgb_frame(
            &packed,
            FloatRgbLayout::Packed { big_endian: false },
            ColorSpace::SRGB,
        );
        let actual = read_float_rgb_frame(
            &packed,
            FloatRgbLayout::Packed { big_endian: false },
            ColorSpace::SRGB,
        )
        .expect("float frame");
        assert_eq!(actual.as_f32(), expected.as_f32());

        let mut planar = frame::Video::new(PixelFormat::GBRAPF32LE, 2, 3);
        for plane in 0..4 {
            let stride = planar.stride(plane);
            let data = planar.data_mut(plane);
            for y in 0..3usize {
                for x in 0..2usize {
                    let offset = y * stride + x * 4;
                    let value = (plane as f32 + 1.0) * 0.1 + (x + y) as f32 * 0.01;
                    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
                }
            }
        }
        let expected = serial_read_float_rgb_frame(
            &planar,
            FloatRgbLayout::PlanarAlpha { big_endian: false },
            ColorSpace::SRGB,
        );
        let actual = read_float_rgb_frame(
            &planar,
            FloatRgbLayout::PlanarAlpha { big_endian: false },
            ColorSpace::SRGB,
        )
        .expect("planar alpha frame");
        assert_eq!(actual.as_f32(), expected.as_f32());
    }

    /// Alternating old/new 1080p ingest measurements.
    ///
    /// ```text
    /// uptime
    /// cargo test -p ravel-media --features ffmpeg --release \
    ///     measure_ingest_transfer_cost -- --ignored --nocapture
    /// ```
    ///
    /// The old loop is reconstructed locally so the comparison remains
    /// available after the production path changes. Each colour-space/path
    /// pair runs three old and three new executions, alternating within each
    /// round. LUTs and the nine primary-matrix pairs are warmed before
    /// timing.
    #[test]
    #[ignore = "measurement harness; run with --ignored --nocapture"]
    fn measure_ingest_transfer_cost() {
        use std::hint::black_box;
        use std::time::Instant;

        const WIDTH: u32 = 1920;
        const HEIGHT: u32 = 1080;
        const ROUNDS: usize = 3;
        const SPACES: [(&str, ColorSpace); 4] = [
            ("sRGB", ColorSpace::SRGB),
            ("Rec709", ColorSpace::REC709),
            ("PQ", ColorSpace::REC2020_PQ),
            ("Linear", ColorSpace::WORKING),
        ];

        let mut rgba8 = frame::Video::new(PixelFormat::RGBA, WIDTH, HEIGHT);
        let stride = rgba8.stride(0);
        let data = rgba8.data_mut(0);
        for y in 0..HEIGHT as usize {
            for x in 0..WIDTH as usize {
                let i = y * WIDTH as usize + x;
                let offset = y * stride + x * 4;
                data[offset..offset + 4].copy_from_slice(&[
                    (i.wrapping_mul(3) % 256) as u8,
                    (i.wrapping_mul(5).wrapping_add(17) % 256) as u8,
                    (i.wrapping_mul(7).wrapping_add(31) % 256) as u8,
                    255,
                ]);
            }
        }

        let mut rgba64 = frame::Video::new(DEEP_OUTPUT, WIDTH, HEIGHT);
        let stride = rgba64.stride(0);
        let data = rgba64.data_mut(0);
        for y in 0..HEIGHT as usize {
            for x in 0..WIDTH as usize {
                let i = y * WIDTH as usize + x;
                let offset = y * stride + x * 8;
                for (lane, value) in [
                    i.wrapping_mul(3) % 65_536,
                    i.wrapping_mul(5).wrapping_add(4_321) % 65_536,
                    i.wrapping_mul(7).wrapping_add(8_765) % 65_536,
                    65_535,
                ]
                .into_iter()
                .enumerate()
                {
                    data[offset + lane * 2..offset + lane * 2 + 2]
                        .copy_from_slice(&(value as u16).to_ne_bytes());
                }
            }
        }

        let mut float_frame = frame::Video::new(PixelFormat::RGBAF32LE, WIDTH, HEIGHT);
        let stride = float_frame.stride(0);
        let data = float_frame.data_mut(0);
        for y in 0..HEIGHT as usize {
            for x in 0..WIDTH as usize {
                let i = y * WIDTH as usize + x;
                let offset = y * stride + x * 16;
                let base = (i % 1024) as f32 / 1023.0;
                for (channel, value) in [base, base * 0.75, base * 0.5, 1.0].into_iter().enumerate()
                {
                    data[offset + channel * 4..offset + channel * 4 + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
            }
        }

        // Do not include one-time table construction in the new-path timing.
        for (_, space) in SPACES {
            black_box(ravel_core::color::ingest_rgba8([1, 2, 3, 4], space));
            black_box(ravel_core::color::ingest_rgba16([1, 2, 3, 4], space));
        }
        for from in Primaries::ALL {
            for to in Primaries::ALL {
                black_box(ravel_core::color::primaries_matrix(from, to));
            }
        }

        let time = |f: &mut dyn FnMut() -> FrameBuffer| {
            let start = Instant::now();
            let output = black_box(f());
            black_box(output.as_f32()[0]);
            start.elapsed().as_nanos()
        };

        let mut old_u8 = [0u128; SPACES.len()];
        let mut new_u8 = [0u128; SPACES.len()];
        let mut old_u16 = [0u128; SPACES.len()];
        let mut new_u16 = [0u128; SPACES.len()];
        let mut old_float = [0u128; SPACES.len()];
        let mut new_float = [0u128; SPACES.len()];

        for _ in 0..ROUNDS {
            for (index, (_, space)) in SPACES.into_iter().enumerate() {
                old_u8[index] += time(&mut || serial_framebuffer_from_rgba8(&rgba8, space));
                new_u8[index] += time(&mut || framebuffer_from_rgba8(&rgba8, space));
                old_u16[index] += time(&mut || serial_framebuffer_from_rgba64(&rgba64, space));
                new_u16[index] += time(&mut || framebuffer_from_rgba64(&rgba64, space));
                old_float[index] += time(&mut || {
                    serial_read_float_rgb_frame(
                        &float_frame,
                        FloatRgbLayout::Packed { big_endian: false },
                        space,
                    )
                });
                new_float[index] += time(&mut || {
                    read_float_rgb_frame(
                        &float_frame,
                        FloatRgbLayout::Packed { big_endian: false },
                        space,
                    )
                    .expect("float frame")
                });
            }
        }

        eprintln!("1080p ingest measurement: rounds={ROUNDS}, executions=3 per path/space");
        for (index, (name, _)) in SPACES.into_iter().enumerate() {
            eprintln!(
                "u8 {name}: old={:.3} ms new={:.3} ms",
                old_u8[index] as f64 / ROUNDS as f64 / 1_000_000.0,
                new_u8[index] as f64 / ROUNDS as f64 / 1_000_000.0
            );
            eprintln!(
                "u16 {name}: old={:.3} ms new={:.3} ms",
                old_u16[index] as f64 / ROUNDS as f64 / 1_000_000.0,
                new_u16[index] as f64 / ROUNDS as f64 / 1_000_000.0
            );
            eprintln!(
                "float {name}: old={:.3} ms new={:.3} ms",
                old_float[index] as f64 / ROUNDS as f64 / 1_000_000.0,
                new_float[index] as f64 / ROUNDS as f64 / 1_000_000.0
            );
        }
    }

    /// Alternate the pre-HIGH-17 and cached decoder paths on the same 1080p
    /// input. The first frame warms the decoder and is not timed; the three
    /// measured frames exercise a reused scaler in the new path.
    ///
    /// ```text
    /// uptime
    /// cargo test -p ravel-media --features ffmpeg --release \
    ///     measure_decoder_frame_cost -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "measurement harness; run with --ignored --nocapture"]
    fn measure_decoder_frame_cost() {
        use std::hint::black_box;
        use std::process::Command;
        use std::time::Instant;

        const WIDTH: usize = 1920;
        const HEIGHT: usize = 1080;
        const ROUNDS: usize = 3;

        init_ffmpeg();
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("high17-1080p.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc=duration=2:size={WIDTH}x{HEIGHT}:rate=30"),
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-preset",
                "ultrafast",
                "-an",
                path.to_str().expect("UTF-8 temporary path"),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg CLI not found");
        assert!(status.success(), "ffmpeg failed to generate 1080p input");

        let mut old = FfmpegDecoder::open(&path)
            .expect("open old decoder")
            .with_input_color_space(ColorSpace::REC2020_PQ);
        let old_stream = old
            .info()
            .first_video()
            .expect("old video stream")
            .stream_index;
        old.ensure_video_decoder(old_stream)
            .expect("old decoder context");
        old.video_decoder
            .as_mut()
            .expect("old cached decoder")
            .legacy_conversion = true;

        let mut new = FfmpegDecoder::open(&path)
            .expect("open new decoder")
            .with_input_color_space(ColorSpace::REC2020_PQ);
        let new_stream = new
            .info()
            .first_video()
            .expect("new video stream")
            .stream_index;

        black_box(
            old.decode_video_frame(old_stream, 0)
                .expect("warm old decoder"),
        );
        black_box(
            new.decode_video_frame(new_stream, 0)
                .expect("warm new decoder"),
        );

        let mut old_total = 0u128;
        let mut new_total = 0u128;
        for frame_number in 1..=ROUNDS as u64 {
            let start = Instant::now();
            let old_frame = old
                .decode_video_frame(old_stream, frame_number)
                .expect("old frame");
            black_box(old_frame.as_f32()[0]);
            old_total += start.elapsed().as_nanos();

            let start = Instant::now();
            let new_frame = new
                .decode_video_frame(new_stream, frame_number)
                .expect("new frame");
            black_box(new_frame.as_f32()[0]);
            new_total += start.elapsed().as_nanos();
        }

        eprintln!(
            "1080p decoder measurement: rounds={ROUNDS}, executions=3 per path, old={:.3} ms, new={:.3} ms",
            old_total as f64 / ROUNDS as f64 / 1_000_000.0,
            new_total as f64 / ROUNDS as f64 / 1_000_000.0,
        );
    }
}
