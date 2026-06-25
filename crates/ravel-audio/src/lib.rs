// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Audio engine for Ravel.
//!
//! Provides CPAL-based cross-platform audio output, a multi-track mixer
//! (using dasp sample primitives), rubato-based sample rate conversion,
//! basic effects (gain, fade), video-audio synchronisation, and waveform
//! data generation for the timeline UI.
//!
//! # Architecture
//!
//! Audio processing runs on a dedicated prep thread; the CPAL callback runs
//! on a high-priority OS thread and only copies pre-mixed audio from a
//! lock-free channel.  Neither thread allocates on the heap in the hot path.
//!
//! ```text
//! UI Thread ── AudioCommand ──► Prep Thread ── chunks ──► CPAL Callback
//!                                    │                        │
//!                                    │                   SyncClock::advance()
//!                                    └── SyncClock ◄──────────┘
//! ```
//!
//! The [`SyncClock`] is the single source of truth for playback position —
//! the video renderer reads it to determine which frame to display.

pub mod device;
pub mod effects;
pub mod engine;
pub mod error;
pub mod mixer;
pub mod resampler;
pub mod sync;
pub mod waveform;

// Re-export key types at crate root for convenience.
pub use engine::{AudioCommand, AudioEngine, AudioEngineConfig};
pub use error::AudioError;
pub use mixer::{Mixer, MixerConfig, Track, TrackId};
pub use sync::SyncClock;
pub use waveform::WaveformData;
