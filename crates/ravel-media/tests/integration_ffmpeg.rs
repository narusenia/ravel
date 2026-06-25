// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Integration tests for the FFmpeg decode/encode pipeline.
//!
//! These tests create synthetic media files using the `ffmpeg` CLI,
//! then exercise our decoder and encoder implementations.

#[cfg(feature = "ffmpeg")]
mod ffmpeg_tests {
    use ravel_core::media::{
        AudioCodec, ContainerFormat, EncoderConfig, MediaReader, MediaWriter, VideoCodec,
        VideoEncoderConfig,
    };
    use ravel_core::types::{FrameBuffer, FrameRate};
    use ravel_media::decoder::FfmpegDecoder;
    use ravel_media::encoder::FfmpegEncoder;
    use std::process::Command;
    use std::sync::Arc;

    /// Generate a short test video using the ffmpeg CLI.
    /// Returns the path to the generated file.
    fn generate_test_video(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=64x64:rate=10",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1:sample_rate=44100",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                path.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg CLI not found");
        assert!(status.success(), "ffmpeg failed to generate test video");
        path
    }

    /// Generate a test video with a specific codec.
    fn generate_test_video_codec(
        dir: &std::path::Path,
        name: &str,
        vcodec: &str,
        container: &str,
    ) -> std::path::PathBuf {
        let path = dir.join(name);
        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=0.5:size=32x32:rate=5",
                "-c:v",
                vcodec,
                "-pix_fmt",
                "yuv420p",
                "-f",
                container,
                path.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg CLI not found");
        assert!(
            status.success(),
            "ffmpeg failed to generate test video with codec {vcodec}"
        );
        path
    }

    // ---- Probe / MediaInfo ------------------------------------------------

    #[test]
    fn probe_h264_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let path = generate_test_video(dir.path(), "test.mp4");

        let info = FfmpegDecoder::probe(&path).expect("probe failed");
        // Container should be detected.
        assert!(
            info.container == Some(ContainerFormat::Mp4)
                || info.container == Some(ContainerFormat::Mov),
            "expected mp4 or mov container, got {:?}",
            info.container,
        );
        // Should have at least one video stream.
        let video = info.first_video().expect("no video stream found");
        assert_eq!(video.width, 64);
        assert_eq!(video.height, 64);
        assert_eq!(video.codec, Some(VideoCodec::H264));

        // Should have at least one audio stream.
        let audio = info.first_audio().expect("no audio stream found");
        assert_eq!(audio.codec, Some(AudioCodec::Aac));
    }

    #[test]
    fn probe_h264_mkv() {
        let dir = tempfile::tempdir().unwrap();
        let path = generate_test_video_codec(dir.path(), "test.mkv", "libx264", "matroska");

        let info = FfmpegDecoder::probe(&path).expect("probe failed");
        assert_eq!(info.container, Some(ContainerFormat::Mkv));
        let video = info.first_video().expect("no video stream");
        assert_eq!(video.codec, Some(VideoCodec::H264));
    }

    #[test]
    fn probe_h265_mp4() {
        let dir = tempfile::tempdir().unwrap();
        let path = generate_test_video_codec(dir.path(), "test_h265.mp4", "libx265", "mp4");

        let info = FfmpegDecoder::probe(&path).expect("probe failed");
        let video = info.first_video().expect("no video stream");
        assert_eq!(video.codec, Some(VideoCodec::H265));
    }

    #[test]
    fn probe_vp9_webm() {
        let dir = tempfile::tempdir().unwrap();
        let path = generate_test_video_codec(dir.path(), "test.webm", "libvpx-vp9", "webm");

        let info = FfmpegDecoder::probe(&path).expect("probe failed");
        assert_eq!(info.container, Some(ContainerFormat::WebM));
        let video = info.first_video().expect("no video stream");
        assert_eq!(video.codec, Some(VideoCodec::Vp9));
    }

    // ---- Video decode -----------------------------------------------------

    #[test]
    fn decode_first_video_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = generate_test_video(dir.path(), "decode_test.mp4");

        let mut decoder = FfmpegDecoder::open(&path).expect("open failed");
        let video_info = decoder.info().first_video().expect("no video stream");
        let stream_idx = video_info.stream_index;

        let frame = decoder
            .decode_video_frame(stream_idx, 0)
            .expect("decode failed");

        // Frame should be 64x64 RGBA.
        assert_eq!(frame.width, 64);
        assert_eq!(frame.height, 64);
        assert_eq!(frame.data.len(), 64 * 64 * 4);

        // Pixel values should be in [0.0, 1.0] range.
        for &val in frame.data.iter() {
            assert!((0.0..=1.0).contains(&val), "pixel value {val} out of range");
        }
    }

    #[test]
    fn decode_non_zero_video_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = generate_test_video(dir.path(), "seek_test.mp4");

        let mut decoder = FfmpegDecoder::open(&path).expect("open failed");
        let video_info = decoder.info().first_video().expect("no video stream");
        let stream_idx = video_info.stream_index;

        // Decode frame 5 (not the first frame) — this exercises the
        // PTS-based seek-and-match logic that was previously broken
        // (review finding C1).
        let frame = decoder
            .decode_video_frame(stream_idx, 5)
            .expect("decode frame 5 failed");

        assert_eq!(frame.width, 64);
        assert_eq!(frame.height, 64);
        assert_eq!(frame.data.len(), 64 * 64 * 4);
    }

    // ---- Audio decode -----------------------------------------------------

    #[test]
    fn decode_audio_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let path = generate_test_video(dir.path(), "audio_test.mp4");

        let mut decoder = FfmpegDecoder::open(&path).expect("open failed");
        let audio_info = decoder.info().first_audio().expect("no audio stream");
        let stream_idx = audio_info.stream_index;

        let chunk = decoder
            .decode_audio_chunk(stream_idx, 0, 1024)
            .expect("decode failed");

        assert!(chunk.sample_rate > 0);
        assert!(chunk.channels > 0);
        // We requested 1024 samples; may get fewer if the file is short.
        assert!(!chunk.data.is_empty());
    }

    // ---- Encode roundtrip -------------------------------------------------

    #[test]
    fn encode_video_frames() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("output.mp4");

        let config = EncoderConfig {
            container: ContainerFormat::Mp4,
            video: Some(VideoEncoderConfig {
                codec: VideoCodec::H264,
                width: 32,
                height: 32,
                frame_rate: FrameRate::new(10, 1),
                crf: Some(23),
                bitrate: None,
            }),
            audio: None,
        };

        let mut encoder =
            FfmpegEncoder::create(&output_path, &config).expect("create encoder failed");

        // Write 5 frames of solid red.
        for _ in 0..5 {
            let mut data = vec![0.0f32; 32 * 32 * 4];
            for pixel in data.chunks_exact_mut(4) {
                pixel[0] = 1.0; // R
                pixel[1] = 0.0; // G
                pixel[2] = 0.0; // B
                pixel[3] = 1.0; // A
            }
            let frame = FrameBuffer {
                width: 32,
                height: 32,
                data: Arc::from(data),
            };
            encoder.write_video_frame(&frame).expect("write failed");
        }

        encoder.finalize().expect("finalize failed");

        // Verify the output file exists and can be probed.
        assert!(output_path.exists());
        let info = FfmpegDecoder::probe(&output_path).expect("probe output failed");
        let video = info.first_video().expect("no video stream in output");
        assert_eq!(video.width, 32);
        assert_eq!(video.height, 32);
    }

    // ---- Format detection -------------------------------------------------

    #[test]
    fn format_detection_matches_probe() {
        use ravel_core::media::{DetectedFormat, detect_format};

        let dir = tempfile::tempdir().unwrap();
        let mp4_path = generate_test_video(dir.path(), "detect.mp4");

        // Extension-based detection.
        assert_eq!(
            detect_format(&mp4_path),
            DetectedFormat::Container(ContainerFormat::Mp4),
        );

        // FFmpeg probe.
        let info = ravel_media::format::probe(&mp4_path).expect("probe failed");
        assert!(info.container.is_some());
    }

    // ---- Hardware acceleration ---------------------------------------------

    #[test]
    fn hw_accel_reports_status() {
        let dir = tempfile::tempdir().unwrap();
        let path = generate_test_video(dir.path(), "hw_status.mp4");

        let decoder = FfmpegDecoder::open(&path).expect("open failed");

        // On macOS with VideoToolbox we expect HW to be available.
        // On CI without GPU we expect None.  Either way, the API must
        // not panic.
        if decoder.hw_accel_active() {
            assert!(decoder.hw_backend_name().is_some());
        }
        // hw_backend_name() reports the device context, not per-stream
        // status — it may be Some even if hw_accel_active() is false
        // (active is set when the first video stream decoder opens).
    }

    #[test]
    fn hw_decode_first_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = generate_test_video(dir.path(), "hw_decode.mp4");

        let mut decoder = FfmpegDecoder::open(&path).expect("open failed");
        let video_info = decoder.info().first_video().expect("no video");
        let stream_idx = video_info.stream_index;

        let frame = decoder
            .decode_video_frame(stream_idx, 0)
            .expect("decode failed");

        assert_eq!(frame.width, 64);
        assert_eq!(frame.height, 64);
        assert_eq!(frame.data.len(), 64 * 64 * 4);

        for &val in frame.data.iter() {
            assert!((0.0..=1.0).contains(&val), "pixel value {val} out of range");
        }
    }

    #[test]
    fn hw_decode_sequential_frames() {
        let dir = tempfile::tempdir().unwrap();
        let path = generate_test_video(dir.path(), "hw_seq.mp4");

        let mut decoder = FfmpegDecoder::open(&path).expect("open failed");
        let video_info = decoder.info().first_video().expect("no video");
        let stream_idx = video_info.stream_index;

        // Decode frames 0, 3, 7 to test seek + HW decoder reuse.
        for frame_num in [0, 3, 7] {
            let frame = decoder
                .decode_video_frame(stream_idx, frame_num)
                .unwrap_or_else(|_| panic!("decode frame {frame_num} failed"));

            assert_eq!(frame.width, 64);
            assert_eq!(frame.height, 64);
        }
    }

    #[test]
    fn hw_decode_h265() {
        let dir = tempfile::tempdir().unwrap();
        let path = generate_test_video_codec(dir.path(), "hw_h265.mp4", "libx265", "mp4");

        let mut decoder = FfmpegDecoder::open(&path).expect("open failed");
        let video_info = decoder.info().first_video().expect("no video");
        let stream_idx = video_info.stream_index;

        let frame = decoder
            .decode_video_frame(stream_idx, 0)
            .expect("decode h265 frame 0 failed");

        assert_eq!(frame.width, 32);
        assert_eq!(frame.height, 32);
    }

    // ---- Image sequence ---------------------------------------------------

    #[test]
    fn image_sequence_detection() {
        let dir = tempfile::tempdir().unwrap();

        // Generate a 5-frame PNG sequence using ffmpeg.
        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=0.5:size=16x16:rate=10",
                "-frames:v",
                "5",
                dir.path().join("frame_%04d.png").to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("ffmpeg CLI not found");
        assert!(status.success());

        let sample = dir.path().join("frame_0003.png");
        let info =
            ravel_media::image_seq::detect_sequence(&sample).expect("detect sequence failed");

        assert_eq!(info.prefix, "frame_");
        assert_eq!(info.format, ravel_core::media::ImageFormat::Png);
        assert_eq!(info.start_frame, 1);
        assert_eq!(info.end_frame, 5);
        assert_eq!(info.frame_count(), 5);
    }
}
