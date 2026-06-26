// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Timeline aggregate holding tracks and clips with immutable mutation API.

use super::id::{ClipId, TrackId};
use super::track::{Clip, Track};
use crate::types::FrameRate;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TimelineError {
    #[error("track {0:?} not found")]
    TrackNotFound(TrackId),
    #[error("clip {0:?} not found in track {1:?}")]
    ClipNotFound(ClipId, TrackId),
    #[error("duplicate track id {0:?}")]
    DuplicateTrack(TrackId),
    #[error("track {0:?} is locked")]
    TrackLocked(TrackId),
}

pub type TimelineResult<T> = Result<T, TimelineError>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    tracks: im::Vector<Track>,
    frame_rate: FrameRate,
    duration_frames: u64,
}

impl Timeline {
    pub fn new(frame_rate: FrameRate) -> Self {
        Self {
            tracks: im::Vector::new(),
            frame_rate,
            duration_frames: 0,
        }
    }

    pub fn tracks(&self) -> &im::Vector<Track> {
        &self.tracks
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    pub fn frame_rate(&self) -> FrameRate {
        self.frame_rate
    }

    pub fn duration_frames(&self) -> u64 {
        self.duration_frames
    }

    pub fn add_track(mut self, track: Track) -> TimelineResult<Self> {
        if self.tracks.iter().any(|t| t.id == track.id) {
            return Err(TimelineError::DuplicateTrack(track.id));
        }
        self.tracks.push_back(track);
        Ok(self)
    }

    pub fn remove_track(mut self, id: TrackId) -> TimelineResult<Self> {
        let idx = self
            .track_index(id)
            .ok_or(TimelineError::TrackNotFound(id))?;
        self.tracks.remove(idx);
        self.recompute_duration();
        Ok(self)
    }

    pub fn add_clip(mut self, track_id: TrackId, clip: Clip) -> TimelineResult<Self> {
        let idx = self
            .track_index(track_id)
            .ok_or(TimelineError::TrackNotFound(track_id))?;
        let track = &self.tracks[idx];
        if track.locked {
            return Err(TimelineError::TrackLocked(track_id));
        }
        let mut track = track.clone();
        track.clips.push_back(clip);
        self.tracks.set(idx, track);
        self.recompute_duration();
        Ok(self)
    }

    pub fn remove_clip(mut self, track_id: TrackId, clip_id: ClipId) -> TimelineResult<Self> {
        let idx = self
            .track_index(track_id)
            .ok_or(TimelineError::TrackNotFound(track_id))?;
        let track = &self.tracks[idx];
        if track.locked {
            return Err(TimelineError::TrackLocked(track_id));
        }
        let clip_idx = track
            .clips
            .iter()
            .position(|c| c.id == clip_id)
            .ok_or(TimelineError::ClipNotFound(clip_id, track_id))?;
        let mut track = track.clone();
        track.clips.remove(clip_idx);
        self.tracks.set(idx, track);
        self.recompute_duration();
        Ok(self)
    }

    fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    fn recompute_duration(&mut self) {
        self.duration_frames = self
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.end_frame())
            .max()
            .unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::track::{ClipSource, TrackKind};

    fn make_clip(id: u64, start: u64, dur: u64) -> Clip {
        Clip {
            id: ClipId::new(id),
            name: format!("clip-{id}"),
            source: ClipSource::Placeholder { label: "test".into() },
            start_frame: start,
            duration_frames: dur,
            source_in: 0,
            source_out: dur,
            color: None,
        }
    }

    #[test]
    fn empty_timeline() {
        let tl = Timeline::new(FrameRate::new(30, 1));
        assert_eq!(tl.track_count(), 0);
        assert_eq!(tl.duration_frames(), 0);
    }

    #[test]
    fn add_and_find_track() {
        let tl = Timeline::new(FrameRate::new(24, 1));
        let tid = TrackId::new(100);
        let tl = tl
            .add_track(Track::new(tid, "Video 1", TrackKind::Video))
            .unwrap();
        assert_eq!(tl.track_count(), 1);
        assert!(tl.track(tid).is_some());
    }

    #[test]
    fn duplicate_track_errors() {
        let tid = TrackId::new(200);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap();
        assert!(
            tl.add_track(Track::new(tid, "V2", TrackKind::Video))
                .is_err()
        );
    }

    #[test]
    fn remove_track() {
        let tid = TrackId::new(300);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap();
        let tl = tl.remove_track(tid).unwrap();
        assert_eq!(tl.track_count(), 0);
    }

    #[test]
    fn remove_nonexistent_track_errors() {
        let tl = Timeline::new(FrameRate::new(30, 1));
        assert!(tl.remove_track(TrackId::new(999)).is_err());
    }

    #[test]
    fn add_clip_updates_duration() {
        let tid = TrackId::new(400);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap()
            .add_clip(tid, make_clip(1, 0, 90))
            .unwrap()
            .add_clip(tid, make_clip(2, 100, 50))
            .unwrap();
        assert_eq!(tl.duration_frames(), 150);
    }

    #[test]
    fn add_clip_to_locked_track_errors() {
        let tid = TrackId::new(500);
        let mut track = Track::new(tid, "V1", TrackKind::Video);
        track.locked = true;
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(track)
            .unwrap();
        assert!(tl.add_clip(tid, make_clip(1, 0, 30)).is_err());
    }

    #[test]
    fn remove_clip() {
        let tid = TrackId::new(600);
        let cid = ClipId::new(10);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap()
            .add_clip(tid, make_clip(10, 0, 60))
            .unwrap();
        assert_eq!(tl.duration_frames(), 60);
        let tl = tl.remove_clip(tid, cid).unwrap();
        assert_eq!(tl.track(tid).unwrap().clips.len(), 0);
        assert_eq!(tl.duration_frames(), 0);
    }

    #[test]
    fn remove_nonexistent_clip_errors() {
        let tid = TrackId::new(700);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap();
        assert!(tl.remove_clip(tid, ClipId::new(999)).is_err());
    }

    #[test]
    fn structural_sharing() {
        let tid = TrackId::new(800);
        let tl1 = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap()
            .add_clip(tid, make_clip(1, 0, 30))
            .unwrap();
        let tl2 = tl1.clone().add_clip(tid, make_clip(2, 30, 30)).unwrap();
        assert_eq!(tl1.track(tid).unwrap().clips.len(), 1);
        assert_eq!(tl2.track(tid).unwrap().clips.len(), 2);
    }

    #[test]
    fn serde_roundtrip() {
        let tid = TrackId::new(900);
        let tl = Timeline::new(FrameRate::new(24, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap()
            .add_clip(tid, make_clip(1, 10, 50))
            .unwrap();
        let s = ron::to_string(&tl).unwrap();
        let back: Timeline = ron::from_str(&s).unwrap();
        assert_eq!(tl, back);
    }
}
