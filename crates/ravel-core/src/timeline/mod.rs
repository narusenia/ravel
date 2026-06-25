// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Timeline data model: tracks, clips, and the timeline aggregate.

pub mod id;
pub mod sequence;
pub mod track;

pub use id::{ClipId, TrackId};
pub use sequence::{Timeline, TimelineError, TimelineResult};
pub use track::{Clip, ClipSource, Track, TrackKind};
