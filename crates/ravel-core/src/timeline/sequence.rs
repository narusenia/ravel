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
    #[error("invalid trim on clip {0:?} in track {1:?}")]
    InvalidTrim(ClipId, TrackId),
    #[error("track index {0} out of range (count: {1})")]
    TrackIndexOutOfRange(usize, usize),
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
        self.require_unlocked(idx, track_id)?;
        let mut track = self.tracks[idx].clone();
        track.clips.push_back(clip);
        self.tracks.set(idx, track);
        self.recompute_duration();
        Ok(self)
    }

    pub fn remove_clip(mut self, track_id: TrackId, clip_id: ClipId) -> TimelineResult<Self> {
        let (tidx, cidx) = self.resolve_clip(track_id, clip_id)?;
        self.require_unlocked(tidx, track_id)?;
        let mut track = self.tracks[tidx].clone();
        track.clips.remove(cidx);
        self.tracks.set(tidx, track);
        self.recompute_duration();
        Ok(self)
    }

    pub fn move_clip(
        mut self,
        track_id: TrackId,
        clip_id: ClipId,
        new_start_frame: u64,
    ) -> TimelineResult<Self> {
        let (tidx, cidx) = self.resolve_clip(track_id, clip_id)?;
        self.require_unlocked(tidx, track_id)?;
        let mut track = self.tracks[tidx].clone();
        let mut clip = track.clips[cidx].clone();
        clip.start_frame = new_start_frame;
        track.clips.set(cidx, clip);
        self.tracks.set(tidx, track);
        self.recompute_duration();
        Ok(self)
    }

    pub fn move_clip_to_track(
        mut self,
        from_track: TrackId,
        clip_id: ClipId,
        to_track: TrackId,
        new_start_frame: u64,
    ) -> TimelineResult<Self> {
        let (from_idx, cidx) = self.resolve_clip(from_track, clip_id)?;
        self.require_unlocked(from_idx, from_track)?;
        let to_idx = self
            .track_index(to_track)
            .ok_or(TimelineError::TrackNotFound(to_track))?;
        self.require_unlocked(to_idx, to_track)?;

        let mut from = self.tracks[from_idx].clone();
        let mut clip = from.clips.remove(cidx);
        clip.start_frame = new_start_frame;
        self.tracks.set(from_idx, from);

        let mut to = self.tracks[to_idx].clone();
        to.clips.push_back(clip);
        self.tracks.set(to_idx, to);
        self.recompute_duration();
        Ok(self)
    }

    pub fn trim_clip_start(
        mut self,
        track_id: TrackId,
        clip_id: ClipId,
        new_start_frame: u64,
    ) -> TimelineResult<Self> {
        let (tidx, cidx) = self.resolve_clip(track_id, clip_id)?;
        self.require_unlocked(tidx, track_id)?;
        let mut track = self.tracks[tidx].clone();
        let mut clip = track.clips[cidx].clone();

        let delta = new_start_frame as i64 - clip.start_frame as i64;
        let new_duration = clip.duration_frames as i64 - delta;
        let new_source_in = clip.source_in as i64 + delta;
        if new_duration <= 0 || new_source_in < 0 {
            return Err(TimelineError::InvalidTrim(clip_id, track_id));
        }
        clip.start_frame = new_start_frame;
        clip.duration_frames = new_duration as u64;
        clip.source_in = new_source_in as u64;
        track.clips.set(cidx, clip);
        self.tracks.set(tidx, track);
        self.recompute_duration();
        Ok(self)
    }

    pub fn trim_clip_end(
        mut self,
        track_id: TrackId,
        clip_id: ClipId,
        new_end_frame: u64,
    ) -> TimelineResult<Self> {
        let (tidx, cidx) = self.resolve_clip(track_id, clip_id)?;
        self.require_unlocked(tidx, track_id)?;
        let mut track = self.tracks[tidx].clone();
        let clip = &track.clips[cidx];

        let new_duration = new_end_frame as i64 - clip.start_frame as i64;
        if new_duration <= 0 {
            return Err(TimelineError::InvalidTrim(clip_id, track_id));
        }
        let new_source_out = clip.source_in + new_duration as u64;
        let mut clip = clip.clone();
        clip.duration_frames = new_duration as u64;
        clip.source_out = new_source_out;
        track.clips.set(cidx, clip);
        self.tracks.set(tidx, track);
        self.recompute_duration();
        Ok(self)
    }

    pub fn set_track_muted(mut self, id: TrackId, muted: bool) -> TimelineResult<Self> {
        let idx = self
            .track_index(id)
            .ok_or(TimelineError::TrackNotFound(id))?;
        let mut track = self.tracks[idx].clone();
        track.muted = muted;
        self.tracks.set(idx, track);
        Ok(self)
    }

    pub fn set_track_locked(mut self, id: TrackId, locked: bool) -> TimelineResult<Self> {
        let idx = self
            .track_index(id)
            .ok_or(TimelineError::TrackNotFound(id))?;
        let mut track = self.tracks[idx].clone();
        track.locked = locked;
        self.tracks.set(idx, track);
        Ok(self)
    }

    pub fn rename_track(
        mut self,
        id: TrackId,
        name: impl Into<String>,
    ) -> TimelineResult<Self> {
        let idx = self
            .track_index(id)
            .ok_or(TimelineError::TrackNotFound(id))?;
        let mut track = self.tracks[idx].clone();
        track.name = name.into();
        self.tracks.set(idx, track);
        Ok(self)
    }

    pub fn reorder_track(mut self, id: TrackId, new_index: usize) -> TimelineResult<Self> {
        let old_idx = self
            .track_index(id)
            .ok_or(TimelineError::TrackNotFound(id))?;
        let count = self.tracks.len();
        if new_index >= count {
            return Err(TimelineError::TrackIndexOutOfRange(new_index, count));
        }
        let track = self.tracks.remove(old_idx);
        let right = self.tracks.split_off(new_index);
        self.tracks.push_back(track);
        self.tracks.append(right);
        Ok(self)
    }

    fn resolve_clip(
        &self,
        track_id: TrackId,
        clip_id: ClipId,
    ) -> TimelineResult<(usize, usize)> {
        let tidx = self
            .track_index(track_id)
            .ok_or(TimelineError::TrackNotFound(track_id))?;
        let cidx = self.tracks[tidx]
            .clips
            .iter()
            .position(|c| c.id == clip_id)
            .ok_or(TimelineError::ClipNotFound(clip_id, track_id))?;
        Ok((tidx, cidx))
    }

    fn require_unlocked(&self, idx: usize, id: TrackId) -> TimelineResult<()> {
        if self.tracks[idx].locked {
            return Err(TimelineError::TrackLocked(id));
        }
        Ok(())
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

    // -- clip operations --

    #[test]
    fn move_clip_changes_start_frame() {
        let tid = TrackId::new(1000);
        let cid = ClipId::new(1);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap()
            .add_clip(tid, make_clip(1, 0, 60))
            .unwrap();
        let tl = tl.move_clip(tid, cid, 30).unwrap();
        let clip = &tl.track(tid).unwrap().clips[0];
        assert_eq!(clip.start_frame, 30);
        assert_eq!(clip.end_frame(), 90);
        assert_eq!(tl.duration_frames(), 90);
    }

    #[test]
    fn move_clip_on_locked_track_errors() {
        let tid = TrackId::new(1002);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap()
            .add_clip(tid, make_clip(1, 0, 30))
            .unwrap()
            .set_track_locked(tid, true)
            .unwrap();
        assert!(tl.move_clip(tid, ClipId::new(1), 10).is_err());
    }

    #[test]
    fn move_clip_to_track() {
        let v1 = TrackId::new(1010);
        let v2 = TrackId::new(1011);
        let cid = ClipId::new(10);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(v1, "V1", TrackKind::Video))
            .unwrap()
            .add_track(Track::new(v2, "V2", TrackKind::Video))
            .unwrap()
            .add_clip(v1, make_clip(10, 0, 60))
            .unwrap();
        assert_eq!(tl.track(v1).unwrap().clips.len(), 1);
        let tl = tl.move_clip_to_track(v1, cid, v2, 20).unwrap();
        assert_eq!(tl.track(v1).unwrap().clips.len(), 0);
        assert_eq!(tl.track(v2).unwrap().clips.len(), 1);
        let clip = &tl.track(v2).unwrap().clips[0];
        assert_eq!(clip.start_frame, 20);
    }

    #[test]
    fn move_clip_to_locked_destination_errors() {
        let v1 = TrackId::new(1012);
        let v2 = TrackId::new(1013);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(v1, "V1", TrackKind::Video))
            .unwrap()
            .add_track(Track::new(v2, "V2", TrackKind::Video))
            .unwrap()
            .add_clip(v1, make_clip(1, 0, 30))
            .unwrap()
            .set_track_locked(v2, true)
            .unwrap();
        assert!(tl.move_clip_to_track(v1, ClipId::new(1), v2, 0).is_err());
    }

    #[test]
    fn move_clip_within_same_track() {
        let tid = TrackId::new(1014);
        let cid = ClipId::new(1);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap()
            .add_clip(tid, make_clip(1, 0, 60))
            .unwrap();
        let tl = tl.move_clip_to_track(tid, cid, tid, 50).unwrap();
        assert_eq!(tl.track(tid).unwrap().clips.len(), 1);
        assert_eq!(tl.track(tid).unwrap().clips[0].start_frame, 50);
    }

    #[test]
    fn trim_clip_start() {
        let tid = TrackId::new(1020);
        let cid = ClipId::new(1);
        // Clip: start=10, dur=60, source_in=0, source_out=60
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap()
            .add_clip(tid, make_clip(1, 10, 60))
            .unwrap();
        // Trim start to frame 30 (remove 20 frames from left)
        let tl = tl.trim_clip_start(tid, cid, 30).unwrap();
        let clip = &tl.track(tid).unwrap().clips[0];
        assert_eq!(clip.start_frame, 30);
        assert_eq!(clip.duration_frames, 40);
        assert_eq!(clip.source_in, 20);
        assert_eq!(clip.end_frame(), 70);
    }

    #[test]
    fn trim_clip_start_past_end_errors() {
        let tid = TrackId::new(1021);
        let cid = ClipId::new(1);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap()
            .add_clip(tid, make_clip(1, 10, 60))
            .unwrap();
        // Trying to trim start past end (frame 80 > end_frame 70)
        assert!(tl.trim_clip_start(tid, cid, 80).is_err());
    }

    #[test]
    fn trim_clip_end() {
        let tid = TrackId::new(1030);
        let cid = ClipId::new(1);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap()
            .add_clip(tid, make_clip(1, 10, 60))
            .unwrap();
        // Trim end to frame 50 (was 70)
        let tl = tl.trim_clip_end(tid, cid, 50).unwrap();
        let clip = &tl.track(tid).unwrap().clips[0];
        assert_eq!(clip.start_frame, 10);
        assert_eq!(clip.duration_frames, 40);
        assert_eq!(clip.source_out, 40);
        assert_eq!(clip.end_frame(), 50);
        assert_eq!(tl.duration_frames(), 50);
    }

    #[test]
    fn trim_clip_end_before_start_errors() {
        let tid = TrackId::new(1031);
        let cid = ClipId::new(1);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap()
            .add_clip(tid, make_clip(1, 10, 60))
            .unwrap();
        assert!(tl.trim_clip_end(tid, cid, 5).is_err());
    }

    // -- track operations --

    #[test]
    fn set_track_muted() {
        let tid = TrackId::new(1100);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap();
        assert!(!tl.track(tid).unwrap().muted);
        let tl = tl.set_track_muted(tid, true).unwrap();
        assert!(tl.track(tid).unwrap().muted);
        let tl = tl.set_track_muted(tid, false).unwrap();
        assert!(!tl.track(tid).unwrap().muted);
    }

    #[test]
    fn set_track_locked() {
        let tid = TrackId::new(1101);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap();
        let tl = tl.set_track_locked(tid, true).unwrap();
        assert!(tl.track(tid).unwrap().locked);
    }

    #[test]
    fn rename_track() {
        let tid = TrackId::new(1102);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap();
        let tl = tl.rename_track(tid, "Main Video").unwrap();
        assert_eq!(tl.track(tid).unwrap().name, "Main Video");
    }

    #[test]
    fn reorder_track() {
        let t1 = TrackId::new(1110);
        let t2 = TrackId::new(1111);
        let t3 = TrackId::new(1112);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(t1, "V1", TrackKind::Video))
            .unwrap()
            .add_track(Track::new(t2, "V2", TrackKind::Video))
            .unwrap()
            .add_track(Track::new(t3, "A1", TrackKind::Audio))
            .unwrap();
        // Move t3 (index 2) to index 0
        let tl = tl.reorder_track(t3, 0).unwrap();
        let ids: Vec<_> = tl.tracks().iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![t3, t1, t2]);
    }

    #[test]
    fn reorder_track_out_of_range_errors() {
        let tid = TrackId::new(1120);
        let tl = Timeline::new(FrameRate::new(30, 1))
            .add_track(Track::new(tid, "V1", TrackKind::Video))
            .unwrap();
        assert!(tl.reorder_track(tid, 5).is_err());
    }

    #[test]
    fn track_operations_on_nonexistent_track_error() {
        let tl = Timeline::new(FrameRate::new(30, 1));
        let bad = TrackId::new(9999);
        assert!(tl.clone().set_track_muted(bad, true).is_err());
        assert!(tl.clone().set_track_locked(bad, true).is_err());
        assert!(tl.clone().rename_track(bad, "x").is_err());
        assert!(tl.reorder_track(bad, 0).is_err());
    }
}
