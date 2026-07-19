// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless state for the timeline panel (Composition/Layer model).

use crate::panel::PanelKind;
use ravel_core::composition::Composition;
use ravel_core::id::{CompId, LayerId};
use ravel_core::types::FrameRate;
use std::collections::HashSet;

const DEFAULT_PPF: f64 = 4.0;
pub const MIN_PPF: f64 = 0.1;
pub const MAX_PPF: f64 = 50.0;
const ZOOM_FACTOR: f64 = 1.2;

/// Which transform property group is expanded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PropertyGroup {
    Position,
    Scale,
    Rotation,
    Opacity,
    AnchorPoint,
}

#[derive(Debug, Clone)]
pub struct TimelinePanel {
    composition: Composition,
    playhead: u64,
    scroll_offset: f64,
    pixels_per_frame: f64,
    selected_layer: Option<LayerId>,
    /// Layers whose ▼ property tree is expanded.
    expanded_layers: HashSet<LayerId>,
    /// Per-layer expanded property rows (only relevant if layer is expanded).
    expanded_properties: HashSet<(LayerId, crate::keyframes::PropertyRowId)>,
    /// Vertical scroll offset for the layer list (pixels).
    vertical_scroll: f64,
    /// Whether the visible range follows the playhead during playback.
    follow_playhead: bool,
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
            expanded_layers: HashSet::new(),
            expanded_properties: HashSet::new(),
            vertical_scroll: 0.0,
            follow_playhead: true,
        }
    }

    pub fn with_composition(composition: Composition) -> Self {
        Self {
            composition,
            playhead: 0,
            scroll_offset: 0.0,
            pixels_per_frame: DEFAULT_PPF,
            selected_layer: None,
            expanded_layers: HashSet::new(),
            expanded_properties: HashSet::new(),
            vertical_scroll: 0.0,
            follow_playhead: true,
        }
    }

    // ----- Composition access -----------------------------------------------

    pub fn composition(&self) -> &Composition {
        &self.composition
    }

    pub fn set_composition(&mut self, comp: Composition) {
        self.composition = comp;
    }

    // ----- Playhead --------------------------------------------------------

    pub fn playhead(&self) -> u64 {
        self.playhead
    }

    pub fn set_playhead(&mut self, frame: u64) {
        self.playhead = frame;
    }

    /// Whether the visible range follows the playhead during playback.
    pub fn follow_playhead(&self) -> bool {
        self.follow_playhead
    }

    pub fn set_follow_playhead(&mut self, follow: bool) {
        self.follow_playhead = follow;
    }

    pub fn toggle_follow_playhead(&mut self) {
        self.follow_playhead = !self.follow_playhead;
    }

    /// Scrolls so the playhead is inside the visible range (AE-style page
    /// flip: an off-screen playhead jumps to the left edge). No-op while the
    /// playhead is already visible, when following is disabled, or when the
    /// viewport width is unknown.
    pub fn scroll_to_follow_playhead(&mut self, viewport_width_px: f64) {
        if !self.follow_playhead || viewport_width_px <= 0.0 {
            return;
        }
        let visible_frames = viewport_width_px / self.pixels_per_frame;
        let first = self.scroll_offset;
        let playhead = self.playhead as f64;
        if playhead < first || playhead >= first + visible_frames {
            self.scroll_offset = playhead.max(0.0);
        }
    }

    // ----- Horizontal scroll/zoom ------------------------------------------

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

    // ----- Vertical scroll -------------------------------------------------

    pub fn vertical_scroll(&self) -> f64 {
        self.vertical_scroll
    }

    pub fn set_vertical_scroll(&mut self, offset: f64) {
        self.vertical_scroll = offset.max(0.0);
    }

    // ----- Selection -------------------------------------------------------

    pub fn selected_layer(&self) -> Option<LayerId> {
        self.selected_layer
    }

    pub fn select_layer(&mut self, id: Option<LayerId>) {
        self.selected_layer = id;
    }

    // ----- Property expansion ----------------------------------------------

    pub fn is_layer_expanded(&self, layer_id: LayerId) -> bool {
        self.expanded_layers.contains(&layer_id)
    }

    pub fn toggle_layer_expanded(&mut self, layer_id: LayerId) {
        if !self.expanded_layers.remove(&layer_id) {
            self.expanded_layers.insert(layer_id);
        }
    }

    pub fn is_property_expanded(
        &self,
        layer_id: LayerId,
        row: &crate::keyframes::PropertyRowId,
    ) -> bool {
        self.expanded_properties.contains(&(layer_id, row.clone()))
    }

    pub fn toggle_property_expanded(
        &mut self,
        layer_id: LayerId,
        row: crate::keyframes::PropertyRowId,
    ) {
        let key = (layer_id, row);
        if !self.expanded_properties.remove(&key) {
            self.expanded_properties.insert(key);
        }
    }

    // ----- Coordinate helpers ----------------------------------------------

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
    use ravel_core::composition::Layer;
    use ravel_core::graph::Graph;

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
        .add_layer(Layer::new(LayerId::new(1), "Solid", Graph::new()).with_time(0, 0, 240));
        p.set_composition(comp);
        assert_eq!(p.composition().id, CompId::new(42));
        assert_eq!(p.composition().layer_count(), 1);
    }

    #[test]
    fn title_key_is_valid() {
        let p = panel();
        assert_eq!(p.title_key(), "panel.timeline");
    }

    #[test]
    fn layer_expansion_toggle() {
        let mut p = panel();
        let lid = LayerId::new(1);
        assert!(!p.is_layer_expanded(lid));
        p.toggle_layer_expanded(lid);
        assert!(p.is_layer_expanded(lid));
        p.toggle_layer_expanded(lid);
        assert!(!p.is_layer_expanded(lid));
    }

    #[test]
    fn follow_playhead_defaults_on_and_toggles() {
        let mut p = panel();
        assert!(p.follow_playhead());
        p.toggle_follow_playhead();
        assert!(!p.follow_playhead());
    }

    #[test]
    fn scroll_to_follow_playhead_pages_when_out_of_view() {
        let mut p = panel();
        p.set_pixels_per_frame(4.0);
        // 400 px / 4 ppf = 100 visible frames starting at 0.
        p.set_playhead(50);
        p.scroll_to_follow_playhead(400.0);
        assert_eq!(p.scroll_offset(), 0.0, "visible playhead must not scroll");

        p.set_playhead(100);
        p.scroll_to_follow_playhead(400.0);
        assert_eq!(p.scroll_offset(), 100.0, "page flips to the playhead");

        // Jumping backwards behind the view also snaps to the playhead.
        p.set_playhead(10);
        p.scroll_to_follow_playhead(400.0);
        assert_eq!(p.scroll_offset(), 10.0);
    }

    #[test]
    fn scroll_to_follow_playhead_respects_toggle_and_unknown_width() {
        let mut p = panel();
        p.set_pixels_per_frame(4.0);
        p.set_playhead(500);
        p.scroll_to_follow_playhead(0.0);
        assert_eq!(p.scroll_offset(), 0.0, "unknown width must be a no-op");

        p.set_follow_playhead(false);
        p.scroll_to_follow_playhead(400.0);
        assert_eq!(p.scroll_offset(), 0.0, "disabled follow must be a no-op");
    }

    #[test]
    fn property_expansion_toggle() {
        use crate::keyframes::PropertyRowId;
        let mut p = panel();
        let lid = LayerId::new(1);
        let position = PropertyRowId::Shell(PropertyGroup::Position);
        let scale = PropertyRowId::Shell(PropertyGroup::Scale);
        assert!(!p.is_property_expanded(lid, &position));
        p.toggle_property_expanded(lid, position.clone());
        assert!(p.is_property_expanded(lid, &position));
        assert!(!p.is_property_expanded(lid, &scale));
    }
}
