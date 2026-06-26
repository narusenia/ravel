// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Track and clip data model for the timeline.

use super::id::{ClipId, TrackId};
use crate::id::NodeId;
use serde::{Deserialize, Serialize};

/// The kind of content a track carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
    Effect,
}

/// Reference to the source media for a clip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ClipSource {
    Placeholder { label: String },
    Media { asset_id: String },
    Sequence { node_id: NodeId },
    Generator { node_id: NodeId },
}

/// A clip placed on a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub name: String,
    pub source: ClipSource,
    pub start_frame: u64,
    pub duration_frames: u64,
    pub source_in: u64,
    pub source_out: u64,
    pub color: Option<[f32; 4]>,
}

impl Clip {
    pub fn end_frame(&self) -> u64 {
        self.start_frame + self.duration_frames
    }
}

const DEFAULT_TRACK_HEIGHT: f32 = 60.0;

/// A track containing an ordered list of clips.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub clips: im::Vector<Clip>,
    pub muted: bool,
    pub locked: bool,
    pub height: f32,
}

impl Track {
    pub fn new(id: TrackId, name: impl Into<String>, kind: TrackKind) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            clips: im::Vector::new(),
            muted: false,
            locked: false,
            height: DEFAULT_TRACK_HEIGHT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_end_frame() {
        let clip = Clip {
            id: ClipId::new(1),
            name: "test".into(),
            source: ClipSource::Placeholder {
                label: "src".into(),
            },
            start_frame: 10,
            duration_frames: 30,
            source_in: 0,
            source_out: 30,
            color: None,
        };
        assert_eq!(clip.end_frame(), 40);
    }

    #[test]
    fn track_defaults() {
        let track = Track::new(TrackId::new(1), "Video 1", TrackKind::Video);
        assert!(!track.muted);
        assert!(!track.locked);
        assert!(track.clips.is_empty());
        assert_eq!(track.height, 60.0);
    }

    #[test]
    fn serde_roundtrip() {
        let mut track = Track::new(TrackId::new(1), "Audio 1", TrackKind::Audio);
        track.clips.push_back(Clip {
            id: ClipId::new(1),
            name: "clip".into(),
            source: ClipSource::Placeholder {
                label: "file.wav".into(),
            },
            start_frame: 0,
            duration_frames: 100,
            source_in: 0,
            source_out: 100,
            color: Some([0.2, 0.4, 0.8, 1.0]),
        });

        let ron_str = ron::to_string(&track).unwrap();
        let back: Track = ron::from_str(&ron_str).unwrap();
        assert_eq!(track, back);
    }
}
