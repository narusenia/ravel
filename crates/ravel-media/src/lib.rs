// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! FFmpeg integration, hardware decode, audio engine, and OCIO for Ravel.
//!
//! # Feature flags
//!
//! - `ffmpeg` (default) — enables the FFmpeg-based [`decoder::FfmpegDecoder`]
//!   and [`encoder::FfmpegEncoder`].  Requires FFmpeg shared libraries at
//!   link time (LGPL dynamic linking).

#[cfg(feature = "ffmpeg")]
pub mod decoder;
#[cfg(feature = "ffmpeg")]
pub mod encoder;

pub mod error;
pub mod format;
pub mod image_seq;
