// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless state for the timeline panel (Composition/Layer model).

use crate::panel::PanelKind;
use ravel_core::composition::Composition;
use ravel_core::id::{CompId, LayerId};
use ravel_core::types::FrameRate;

const DEFAULT_PPF: f64 = 4.0;
const MIN_PPF: f64 = 0.1;
const MAX_PPF: f64 = 50.0;
const ZOOM_FACTOR: f64 = 1.2;

#[derive(Debug, Clone)]
pub struct TimelinePanel {
    composition: Composition,
    playhead: u64,
    scroll_offset: f64,
    pixels_per_frame: f64,
    selected_layer: Option<LayerId>,
}

impl TimelinePanel {
    pub const KIND: PanelKind = PanelKind::Timeline;

    pub fn new(frame_rate: FrameRate) -> Self {
        Self {
            composition: Composition::new(CompId::new(0), "Main", (1920, 1080), frame_rate, 300),
            playhead: 0,
            scroll_offset: 0.0,
            pixels_per_frame: DEFAULT_PPF,
            selected_layer: None,
        }
    }

    pub fn with_composition(composition: Composition) -> Self {
        Self {
            composition,
            playhead: 0,
            scroll_offset: 0.0,
            pixels_per_frame: DEFAULT_PPF,
            selected_layer: None,
        }
    }

    pub fn composition(&self) -> &Composition {
        &self.composition
    }

    pub fn set_composition(&mut self, comp: Composition) {
        self.composition = comp;
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

    pub fn selected_layer(&self) -> Option<LayerId> {
        self.selected_layer
    }

    pub fn select_layer(&mut self, id: Option<LayerId>) {
        self.selected_layer = id;
    }

    pub fn frame_to_x(&self, frame: i64) -> f64 {
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
    use ravel_core::composition::{Layer, LayerSource};
    use ravel_core::types::Color;

    fn panel() -> TimelinePanel {
        TimelinePanel::new(FrameRate::new(30, 1))
    }

    #[test]
    fn default_values() {
        let p = panel();
        assert_eq!(p.playhead(), 0);
        assert_eq!(p.scroll_offset(), 0.0);
        assert_eq!(p.pixels_per_frame(), DEFAULT_PPF);
        assert!(p.selected_layer().is_none());
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
        let frame = 100i64;
        let x = p.frame_to_x(frame);
        assert_eq!(p.x_to_frame(x), frame as u64);
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
    fn layer_selection() {
        let mut p = panel();
        let lid = LayerId::new(1);
        p.select_layer(Some(lid));
        assert_eq!(p.selected_layer(), Some(lid));
        p.select_layer(None);
        assert!(p.selected_layer().is_none());
    }

    #[test]
    fn composition_set_get() {
        let mut p = panel();
        let comp = Composition::new(
            CompId::new(42),
            "Test",
            (1280, 720),
            FrameRate::new(24, 1),
            240,
        )
        .add_layer(
            Layer::new(
                LayerId::new(1),
                "Solid",
                LayerSource::Solid {
                    color: Color::WHITE,
                    width: 1280,
                    height: 720,
                },
            )
            .with_time(0, 0, 240),
        );
        p.set_composition(comp);
        assert_eq!(p.composition().id, CompId::new(42));
        assert_eq!(p.composition().layer_count(), 1);
    }

    #[test]
    fn title_key_is_valid() {
        let p = panel();
        assert_eq!(p.title_key(), "panel.timeline");
    }
}
