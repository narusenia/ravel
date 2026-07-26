// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! FFmpeg integration, hardware decode, audio engine, and OCIO for Ravel.
//!
//! # Feature flags
//!
//! - `ffmpeg` (default) — enables the FFmpeg-based [`decoder::FfmpegDecoder`]
//!   and [`encoder::FfmpegEncoder`].  Requires FFmpeg shared libraries at
//!   link time (LGPL dynamic linking).
//!
//! [`audio_sample`] is deliberately outside that flag: it holds the sample
//! format arithmetic the decoder feeds, so the conversion stays testable
//! without FFmpeg.

#[cfg(feature = "ffmpeg")]
pub mod decoder;
#[cfg(feature = "ffmpeg")]
pub mod encoder;
#[cfg(feature = "ffmpeg")]
pub mod hwaccel;

pub mod audio_sample;
pub mod error;
pub mod format;
pub mod image_seq;
