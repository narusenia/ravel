// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless state for the timeline panel.

use crate::panel::PanelKind;
use ravel_core::timeline::{ClipId, Timeline, TrackId};
use ravel_core::types::FrameRate;

const DEFAULT_PPF: f64 = 4.0;
const MIN_PPF: f64 = 0.1;
const MAX_PPF: f64 = 50.0;
const ZOOM_FACTOR: f64 = 1.2;

#[derive(Debug, Clone)]
pub struct TimelinePanel {
    timeline: Timeline,
    playhead: u64,
    scroll_offset: f64,
    pixels_per_frame: f64,
    selected_track: Option<TrackId>,
    selected_clip: Option<(TrackId, ClipId)>,
}

impl TimelinePanel {
    pub const KIND: PanelKind = PanelKind::Timeline;

    pub fn new(frame_rate: FrameRate) -> Self {
        Self {
            timeline: Timeline::new(frame_rate),
            playhead: 0,
            scroll_offset: 0.0,
            pixels_per_frame: DEFAULT_PPF,
            selected_track: None,
            selected_clip: None,
        }
    }

    pub fn with_timeline(timeline: Timeline) -> Self {
        Self {
            timeline,
            playhead: 0,
            scroll_offset: 0.0,
            pixels_per_frame: DEFAULT_PPF,
            selected_track: None,
            selected_clip: None,
        }
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    pub fn set_timeline(&mut self, timeline: Timeline) {
        self.timeline = timeline;
    }

    pub fn playhead(&self) -> u64 {
        self.playhead
    }

    pub fn set_playhead(&mut self, frame: u64) {
        self.playhead = frame;
    }

    pub fn scroll_offset(&self) -> f64 {
        self.scroll_offset
    }

    pub fn set_scroll_offset(&mut self, offset: f64) {
        self.scroll_offset = offset.max(0.0);
    }

    pub fn pixels_per_frame(&self) -> f64 {
        self.pixels_per_frame
    }

    pub fn set_pixels_per_frame(&mut self, ppf: f64) {
        self.pixels_per_frame = ppf.clamp(MIN_PPF, MAX_PPF);
    }

    pub fn zoom_in(&mut self) {
        self.set_pixels_per_frame(self.pixels_per_frame * ZOOM_FACTOR);
    }

    pub fn zoom_out(&mut self) {
        self.set_pixels_per_frame(self.pixels_per_frame / ZOOM_FACTOR);
    }

    pub fn zoom_at(&mut self, cursor_x: f64, factor: f64) {
        let frame_under_cursor = self.x_to_frame_f64(cursor_x);
        self.set_pixels_per_frame(self.pixels_per_frame * factor);
        self.scroll_offset = (frame_under_cursor - cursor_x / self.pixels_per_frame).max(0.0);
    }

    pub fn selected_track(&self) -> Option<TrackId> {
        self.selected_track
    }

    pub fn select_track(&mut self, id: Option<TrackId>) {
        self.selected_track = id;
    }

    pub fn selected_clip(&self) -> Option<(TrackId, ClipId)> {
        self.selected_clip
    }

    pub fn select_clip(&mut self, selection: Option<(TrackId, ClipId)>) {
        self.selected_clip = selection;
    }

    pub fn frame_to_x(&self, frame: u64) -> f64 {
        (frame as f64 - self.scroll_offset) * self.pixels_per_frame
    }

    pub fn x_to_frame(&self, x: f64) -> u64 {
        self.x_to_frame_f64(x).round().max(0.0) as u64
    }

    fn x_to_frame_f64(&self, x: f64) -> f64 {
        x / self.pixels_per_frame + self.scroll_offset
    }

    pub fn title_key(&self) -> &'static str {
        PanelKind::Timeline.label_key()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel() -> TimelinePanel {
        TimelinePanel::new(FrameRate::new(30, 1))
    }

    #[test]
    fn default_values() {
        let p = panel();
        assert_eq!(p.playhead(), 0);
        assert_eq!(p.scroll_offset(), 0.0);
        assert_eq!(p.pixels_per_frame(), DEFAULT_PPF);
        assert!(p.selected_track().is_none());
        assert!(p.selected_clip().is_none());
    }

    #[test]
    fn playhead_get_set() {
        let mut p = panel();
        p.set_playhead(42);
        assert_eq!(p.playhead(), 42);
    }

    #[test]
    fn scroll_clamps_negative() {
        let mut p = panel();
        p.set_scroll_offset(-10.0);
        assert_eq!(p.scroll_offset(), 0.0);
    }

    #[test]
    fn zoom_clamps_range() {
        let mut p = panel();
        p.set_pixels_per_frame(0.01);
        assert!((p.pixels_per_frame() - MIN_PPF).abs() < f64::EPSILON);

        p.set_pixels_per_frame(100.0);
        assert!((p.pixels_per_frame() - MAX_PPF).abs() < f64::EPSILON);
    }

    #[test]
    fn frame_to_x_roundtrip() {
        let p = panel();
        let frame = 100u64;
        let x = p.frame_to_x(frame);
        assert_eq!(p.x_to_frame(x), frame);
    }

    #[test]
    fn frame_to_x_with_scroll() {
        let mut p = panel();
        p.set_scroll_offset(50.0);
        let x = p.frame_to_x(50);
        assert!((x - 0.0).abs() < f64::EPSILON);
        let x = p.frame_to_x(60);
        assert!((x - 40.0).abs() < f64::EPSILON);
    }

    #[test]
    fn zoom_at_cursor_anchor() {
        let mut p = panel();
        p.set_scroll_offset(0.0);
        p.set_pixels_per_frame(4.0);
        let cursor_x = 200.0;
        let frame_before = p.x_to_frame_f64(cursor_x);
        p.zoom_at(cursor_x, 2.0);
        let frame_after = p.x_to_frame_f64(cursor_x);
        assert!((frame_before - frame_after).abs() < 0.01);
    }

    #[test]
    fn selection_state() {
        let mut p = panel();
        let tid = TrackId::new(1);
        let cid = ClipId::new(2);
        p.select_track(Some(tid));
        assert_eq!(p.selected_track(), Some(tid));
        p.select_clip(Some((tid, cid)));
        assert_eq!(p.selected_clip(), Some((tid, cid)));
        p.select_clip(None);
        assert!(p.selected_clip().is_none());
    }

    #[test]
    fn title_key_is_valid() {
        let p = panel();
        assert_eq!(p.title_key(), "panel.timeline");
    }
}
