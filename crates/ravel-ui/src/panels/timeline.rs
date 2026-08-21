// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless state for the timeline panel (Composition/Layer model).
//!
//! The panel mirrors the **active composition** (REQ-UI-013): the host keeps
//! [`TimelinePanel::set_composition`] in sync with the `ActiveComposition`
//! global, and `None` — a document with no composition — is a legitimate
//! state the panel renders as empty. The layer selection is *not* held here;
//! it lives in the host's `LayerSelection` global so the Timeline and the
//! Outliner share one selection instead of mirroring each other.

use crate::keyframes::RevealFilter;
use crate::panel::PanelKind;
use crate::panels::media_bin;
use ravel_core::composition::{Composition, Document, Layer};
use ravel_core::id::{CompId, LayerId};
use ravel_core::runtime::playback::LoopRange;
use ravel_core::types::FrameRate;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ops::Range;

const DEFAULT_PPF: f64 = 4.0;
pub const MIN_PPF: f64 = 0.1;
pub const MAX_PPF: f64 = 50.0;
const ZOOM_FACTOR: f64 = 1.2;

/// Accepted tempo range. The lower bound also keeps the beat spacing finite:
/// a `0` (or negative) tempo read from a hand-edited `ui_state.json` would
/// otherwise make the beat spacing infinite or run the grid backwards.
pub const MIN_BPM: f64 = 1.0;
pub const MAX_BPM: f64 = 999.0;

/// Upper bound on the beats one call may return, so a dense grid at a far
/// zoom-out cannot allocate without limit. At the point the cap bites, beats
/// are far below one pixel apart and the host has already stopped drawing
/// them ([`BpmGrid::is_legible_at`]).
const MAX_BEATS_PER_QUERY: usize = 4096;

/// The musical beat grid the Timeline can overlay on the frame grid
/// (they are independent and can be shown together).
///
/// This is **UI state, not a composition attribute**: it steers nothing in
/// the rendered picture, so it is persisted in `ui_state.json` rather than in
/// the document (`docs/implementation/refactor-plan-0808.md`, unit 8).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BpmGrid {
    /// Whether the beat lines are drawn at all.
    pub enabled: bool,
    /// Tempo in beats per minute.
    pub bpm: f64,
    /// Composition frame that carries beat 1. Recorded music almost never
    /// starts on frame 0, so the grid is useless without it.
    pub offset_frames: f64,
}

impl Default for BpmGrid {
    /// Off, at the tempo most sequencers open on. Not `derive`d: a derived
    /// default would be `bpm: 0.0`, and with `#[serde(default)]` a partial
    /// entry such as `{"enabled": true}` would then load as a degenerate grid.
    fn default() -> Self {
        Self {
            enabled: false,
            bpm: 120.0,
            offset_frames: 0.0,
        }
    }
}

impl BpmGrid {
    /// The same grid with out-of-range or non-finite numbers pulled back into
    /// the accepted range. `ui_state.json` is hand-editable and a `NumberInput`
    /// accepts anything the user types, so every entry point sanitizes.
    pub fn sanitized(self) -> Self {
        let default = Self::default();
        Self {
            enabled: self.enabled,
            bpm: if self.bpm.is_finite() {
                self.bpm.clamp(MIN_BPM, MAX_BPM)
            } else {
                default.bpm
            },
            offset_frames: if self.offset_frames.is_finite() {
                self.offset_frames
            } else {
                0.0
            },
        }
    }

    /// Distance between two beats, in frames.
    ///
    /// **Deliberately fractional.** At 24 fps and 140 BPM a beat lands every
    /// 10.2857… frames, so rounding to whole frames would bunch the lines
    /// against the frame grid and drift a full beat over a few bars. Nothing
    /// snaps to this grid — it is a visual guide — so the host paints the
    /// lines at fractional frame positions and lets the pixel rounding happen
    /// once, at paint time.
    pub fn frames_per_beat(&self, frame_rate: FrameRate) -> f64 {
        let fps = frame_rate.as_f64();
        if !fps.is_finite() || fps <= 0.0 || !self.bpm.is_finite() || self.bpm < MIN_BPM {
            return f64::NAN;
        }
        fps * 60.0 / self.bpm
    }

    /// Whether beats are far enough apart at `pixels_per_frame` to read as a
    /// grid rather than as a smear.
    pub fn is_legible_at(&self, frame_rate: FrameRate, pixels_per_frame: f64) -> bool {
        let spacing = self.frames_per_beat(frame_rate) * pixels_per_frame;
        spacing.is_finite() && spacing >= MIN_BEAT_SPACING_PX
    }

    /// Frames of the beats inside `[first_frame, last_frame]`, in order.
    ///
    /// Each frame is computed as `offset + index * frames_per_beat` from the
    /// beat index rather than by accumulating the step, so a long timeline
    /// does not collect floating-point drift. Beats before beat 1 are not
    /// emitted: a negative beat index has no musical meaning here.
    pub fn beat_frames(
        &self,
        frame_rate: FrameRate,
        first_frame: f64,
        last_frame: f64,
    ) -> Vec<f64> {
        let step = self.frames_per_beat(frame_rate);
        // `offset_frames` is checked here too, not only in `sanitized`: this
        // is a pure function anyone may call with a hand-built grid, and a
        // non-finite offset would make every beat NaN right up to the cap.
        if !step.is_finite()
            || step <= 0.0
            || !self.offset_frames.is_finite()
            || !first_frame.is_finite()
            || !last_frame.is_finite()
        {
            return Vec::new();
        }
        let first_index = ((first_frame - self.offset_frames) / step).ceil().max(0.0);
        if !first_index.is_finite() {
            return Vec::new();
        }
        let mut beats = Vec::new();
        let mut index = first_index;
        loop {
            let frame = self.offset_frames + index * step;
            if frame > last_frame || beats.len() >= MAX_BEATS_PER_QUERY {
                break;
            }
            beats.push(frame);
            index += 1.0;
        }
        beats
    }
}

/// Below this beat spacing the grid is drawn as a solid block rather than as
/// lines, so the host stops drawing it instead.
const MIN_BEAT_SPACING_PX: f64 = 4.0;

/// How near a keyframe a `Shift`-held playhead gesture has to come before it
/// snaps to it, in pixels (`TimelinePanel::snap_playhead_x`).
///
/// A pixel radius rather than a frame count so the pull is the same gesture at
/// every zoom. Half a keyframe diamond's width either side: close enough that
/// aiming at the diamond catches, far enough that a deliberate frame beside it
/// does not.
pub const KEYFRAME_SNAP_RADIUS_PX: f64 = 8.0;

/// Which transform property group is expanded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PropertyGroup {
    Position,
    Scale,
    Rotation,
    Opacity,
    AudioGain,
    AnchorPoint,
}

/// The visualization shown in the timeline's time-based right pane.
///
/// Both modes deliberately share the panel's playhead, horizontal scroll,
/// zoom, property expansion, and keyframe selection state. The GPUI host only
/// swaps the right-pane renderer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TimelineViewMode {
    #[default]
    Bars,
    Graph,
}

/// Stable identity of a Timeline property channel selected for graph display.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimelineChannelRef {
    pub layer: LayerId,
    pub row: crate::keyframes::PropertyRowId,
    pub component: usize,
}

#[derive(Debug, Clone)]
pub struct TimelinePanel {
    /// Mirror of the active composition; `None` when none is active.
    composition: Option<Composition>,
    /// Frame rate used for time formatting while no composition is active.
    fallback_frame_rate: FrameRate,
    playhead: u64,
    scroll_offset: f64,
    pixels_per_frame: f64,
    /// Layers whose ▼ property tree is expanded.
    expanded_layers: HashSet<LayerId>,
    /// Per-layer expanded property rows (only relevant if layer is expanded).
    expanded_properties: HashSet<(LayerId, crate::keyframes::PropertyRowId)>,
    /// Vertical scroll offset for the layer list (pixels).
    vertical_scroll: f64,
    /// Whether the visible range follows the playhead during playback.
    follow_playhead: bool,
    /// Whether the right pane renders layer bars or animated value curves.
    view_mode: TimelineViewMode,
    /// Property channels whose curves the graph view should display.
    selected_channels: Vec<TimelineChannelRef>,
    /// Active reveal criteria; empty means every row is shown.
    ///
    /// Panel state rather than a shared global: it is one panel's view of the
    /// tree, and it deliberately outlives a layer selection change, so it
    /// cannot hang off the selection either.
    reveal: HashSet<crate::keyframes::RevealFilter>,
    /// Layers whose row carries the offline media mark, recomputed by
    /// [`TimelinePanel::sync_offline_layers`]. Kept as a set rather than
    /// resolved per row because the answer needs the document's asset table
    /// and a walk of the layer network, and the host paints rows in
    /// `render()`.
    offline_layers: HashSet<LayerId>,
}

impl TimelinePanel {
    pub const KIND: PanelKind = PanelKind::Timeline;

    /// An empty panel with no active composition. `frame_rate` is only used
    /// to format times until one is set.
    pub fn new(frame_rate: FrameRate) -> Self {
        Self {
            composition: None,
            fallback_frame_rate: frame_rate,
            playhead: 0,
            scroll_offset: 0.0,
            pixels_per_frame: DEFAULT_PPF,
            expanded_layers: HashSet::new(),
            expanded_properties: HashSet::new(),
            vertical_scroll: 0.0,
            follow_playhead: true,
            view_mode: TimelineViewMode::default(),
            selected_channels: Vec::new(),
            reveal: HashSet::new(),
            offline_layers: HashSet::new(),
        }
    }

    pub fn with_composition(composition: Composition) -> Self {
        let mut panel = Self::new(composition.frame_rate);
        panel.composition = Some(composition);
        panel
    }

    // ----- Composition access -----------------------------------------------

    /// The mirrored active composition, `None` in the composition-0 state.
    pub fn composition(&self) -> Option<&Composition> {
        self.composition.as_ref()
    }

    /// Id of the mirrored composition — the composition every edit this
    /// panel makes is routed to.
    pub fn comp_id(&self) -> Option<CompId> {
        self.composition.as_ref().map(|comp| comp.id)
    }

    /// A layer of the mirrored composition.
    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.composition.as_ref()?.get_layer(id)
    }

    /// The mirrored composition's layers, bottom-most first (empty when no
    /// composition is active).
    pub fn layers(&self) -> impl DoubleEndedIterator<Item = &Layer> {
        self.composition.iter().flat_map(|comp| comp.layers.iter())
    }

    /// Frame rate of the mirrored composition, or the construction-time
    /// fallback while none is active.
    pub fn frame_rate(&self) -> FrameRate {
        self.composition
            .as_ref()
            .map_or(self.fallback_frame_rate, |comp| comp.frame_rate)
    }

    /// Duration of the mirrored composition; `0` while none is active, so
    /// the ruler and the transport have nothing to move over.
    pub fn duration_frames(&self) -> u64 {
        self.composition
            .as_ref()
            .map_or(0, |comp| comp.duration_frames)
    }

    pub fn set_composition(&mut self, comp: Option<Composition>) {
        let valid_channels = comp.as_ref().map(channel_refs).unwrap_or_default();
        self.composition = comp;
        self.selected_channels
            .retain(|channel| valid_channels.contains(channel));
        // The marks describe the composition that just went away. A switch
        // reuses layer numbers, so keeping them would mark unrelated layers;
        // the host calls `sync_offline_layers` right after this.
        self.offline_layers.clear();
    }

    /// Recompute which layer rows carry the offline media mark
    /// ([`media_bin::layer_is_offline`]) from `document`.
    ///
    /// Separate from [`TimelinePanel::set_composition`] because the mirror is
    /// a `Composition` while the asset table lives on the `Document`: the host
    /// holds both, and doing the walk here is what keeps it out of `render()`.
    /// Call it after every `set_composition`, which clears the set.
    pub fn sync_offline_layers(&mut self, document: &Document) {
        self.offline_layers = self
            .composition
            .as_ref()
            .map(|comp| media_bin::offline_layers(document, comp))
            .unwrap_or_default();
    }

    /// Whether this layer references a media asset that is offline, and so
    /// shows the mark. `false` for a layer of another composition.
    pub fn is_layer_offline(&self, layer: LayerId) -> bool {
        self.offline_layers.contains(&layer)
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

    // ----- View mode ------------------------------------------------------

    pub fn view_mode(&self) -> TimelineViewMode {
        self.view_mode
    }

    pub fn set_view_mode(&mut self, mode: TimelineViewMode) {
        self.view_mode = mode;
    }

    pub fn toggle_view_mode(&mut self) {
        self.view_mode = match self.view_mode {
            TimelineViewMode::Bars => TimelineViewMode::Graph,
            TimelineViewMode::Graph => TimelineViewMode::Bars,
        };
    }

    // ----- Graph channel selection ---------------------------------------

    /// Selected channels in palette assignment order.
    pub fn selected_channels(&self) -> &[TimelineChannelRef] {
        &self.selected_channels
    }

    pub fn is_channel_selected(&self, channel: &TimelineChannelRef) -> bool {
        self.selected_channels.contains(channel)
    }

    /// Selects a graph channel. Additive selection follows the Timeline's
    /// Shift-click convention and toggles the clicked channel.
    pub fn select_channel(&mut self, channel: TimelineChannelRef, additive: bool) {
        if additive {
            if let Some(index) = self
                .selected_channels
                .iter()
                .position(|selected| selected == &channel)
            {
                self.selected_channels.remove(index);
            } else {
                self.selected_channels.push(channel);
            }
        } else {
            self.selected_channels.clear();
            self.selected_channels.push(channel);
        }
    }

    pub fn clear_selected_channels(&mut self) {
        self.selected_channels.clear();
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

    // ----- Reveal filters ---------------------------------------------------

    /// The active reveal criteria; empty means nothing is filtered out.
    pub fn reveal_filters(&self) -> &HashSet<RevealFilter> {
        &self.reveal
    }

    /// Apply one reveal criterion.
    ///
    /// Unmodified (`additive == false`) **replaces** the current filter, and
    /// applying the criterion the panel is already showing alone clears it —
    /// pressing the same key twice returns to the full tree. `Shift`
    /// (`additive == true`) toggles the criterion's membership instead, so a
    /// second `Shift+<key>` takes that group back out.
    pub fn apply_reveal(&mut self, filter: RevealFilter, additive: bool) {
        if additive {
            if !self.reveal.remove(&filter) {
                self.reveal.insert(filter);
            }
        } else if self.reveal.len() == 1 && self.reveal.contains(&filter) {
            self.reveal.clear();
        } else {
            self.reveal.clear();
            self.reveal.insert(filter);
        }
    }

    /// The property rows of `layer` that survive the active reveal filters.
    ///
    /// **The one filtered enumeration.** Painting, hit testing, rubber-band
    /// selection, the content height and the header tree all go through it;
    /// deriving the row list anywhere else makes them disagree below the first
    /// hidden row (`MED-APP-13`).
    pub fn visible_property_rows(&self, layer: &Layer) -> Vec<crate::keyframes::PropertyRow> {
        let rows = crate::keyframes::property_rows(layer);
        if self.reveal.is_empty() {
            return rows;
        }
        rows.into_iter()
            .filter(|row| self.reveal.iter().any(|filter| filter.matches(layer, row)))
            .collect()
    }

    // ----- Keyframe snapping ------------------------------------------------

    /// Composition frames carrying a keyframe the panel is **currently
    /// showing**, sorted and deduplicated.
    ///
    /// "Showing" is the panel's own tree, not the document. Three gates, all
    /// of which the painter applies before it draws a diamond:
    ///
    /// - a collapsed layer contributes nothing;
    /// - an expanded one contributes only the rows that survive the reveal
    ///   filter ([`Self::visible_property_rows`], the one filtered
    ///   enumeration);
    /// - **a collapsed property row contributes nothing either** — the
    ///   keyframe diamonds live on the channel sub-rows, which are painted
    ///   only inside the `is_property_expanded` branch, so a collapsed
    ///   property draws no keys to snap to.
    ///
    /// Frames outside `[0, duration)` are dropped: the playhead cannot be
    /// scrubbed there, so a key past the end would pull the pointer to the
    /// clamped final frame instead of leaving it where the user aimed.
    pub fn visible_keyframe_frames(&self) -> Vec<i64> {
        let mut frames: Vec<i64> = self
            .layers()
            .filter(|layer| self.is_layer_expanded(layer.id))
            .flat_map(|layer| {
                self.visible_property_rows(layer)
                    .into_iter()
                    .filter(|row| self.is_property_expanded(layer.id, &row.id))
                    .flat_map(move |row| {
                        (0..row.channel_names.len())
                            .flat_map(move |component| {
                                crate::keyframes::row_key_frames(layer, &row.id, component)
                            })
                            .collect::<Vec<_>>()
                    })
                    .map(move |frame| crate::keyframes::comp_frame_for_key(layer, frame))
            })
            .filter(|frame| (0..self.duration_frames() as i64).contains(frame))
            .collect();
        frames.sort_unstable();
        frames.dedup();
        frames
    }

    /// The frame a playhead gesture at lane-local pixel `x` lands on while the
    /// user holds `Shift`: the nearest visible keyframe within
    /// [`KEYFRAME_SNAP_RADIUS_PX`], otherwise the frame under the pointer.
    ///
    /// The radius is compared in **pixels, not frames**, so the pull feels the
    /// same at every zoom instead of covering half the screen when zoomed in.
    pub fn snap_playhead_x(&self, x: f64, candidates: &[i64]) -> u64 {
        let snapped = candidates
            .iter()
            .copied()
            .map(|frame| (frame, (self.frame_to_x(frame) - x).abs()))
            .filter(|(_, distance)| *distance <= KEYFRAME_SNAP_RADIUS_PX)
            // The frame breaks a tie, so the earlier key wins: `min_by` keeps
            // the *last* of equal elements, and the candidates are ascending.
            .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
            .map(|(frame, _)| frame);
        match snapped {
            Some(frame) => frame.max(0) as u64,
            None => self.x_to_frame(x),
        }
    }

    // ----- Coordinate helpers ----------------------------------------------

    pub fn frame_to_x(&self, frame: i64) -> f64 {
        (frame as f64 - self.scroll_offset) * self.pixels_per_frame
    }

    pub fn x_to_frame(&self, x: f64) -> u64 {
        self.x_to_frame_f64(x).round().max(0.0) as u64
    }

    /// Pixel span `(x, width)` of the region past the composition's last
    /// frame inside a lane `viewport_width` pixels wide, or `None` when the
    /// whole viewport is inside the composition.
    ///
    /// The end is *shaded*, not clamped away: the zoom deliberately still
    /// reaches beyond the duration, so the timeline says where the
    /// composition stops instead of refusing to show it.
    pub fn out_of_range_span(&self, viewport_width: f64) -> Option<(f64, f64)> {
        if viewport_width <= 0.0 || !viewport_width.is_finite() {
            return None;
        }
        // No composition means no duration to be outside of.
        let duration = self.composition.as_ref()?.duration_frames;
        let start = self.frame_to_x(i64::try_from(duration).unwrap_or(i64::MAX));
        if !start.is_finite() || start >= viewport_width {
            return None;
        }
        let start = start.max(0.0);
        Some((start, viewport_width - start))
    }

    /// Pixel span `(x, width)` of `range` inside a ruler `viewport_width`
    /// pixels wide, clipped to the viewport, or `None` when the range is
    /// scrolled entirely out of sight.
    ///
    /// The out frame is included, so the band covers the frame the loop
    /// actually plays last rather than stopping at its leading edge.
    pub fn loop_range_span(&self, range: LoopRange, viewport_width: f64) -> Option<(f64, f64)> {
        if viewport_width <= 0.0 || !viewport_width.is_finite() {
            return None;
        }
        let start = self.frame_to_x(i64::try_from(range.in_frame).unwrap_or(i64::MAX));
        let end = self.frame_to_x(i64::try_from(range.out_frame + 1).unwrap_or(i64::MAX));
        if !start.is_finite() || !end.is_finite() || end <= 0.0 || start >= viewport_width {
            return None;
        }
        let start = start.max(0.0);
        Some((start, end.min(viewport_width) - start))
    }

    /// Pixel spans `(x, width)` of the cached frame ranges the frame cache
    /// reports, clipped to a ruler `viewport_width` pixels wide (`CACHE-6`).
    ///
    /// `ranges` are the half-open `[start, end)` spans of
    /// `SharedFrameCache::cached_ranges`. Each becomes the band over exactly
    /// those frames, so the picture and the cache agree by construction:
    /// mapping goes through [`Self::loop_range_span`], the one place the
    /// ruler turns an inclusive frame span into pixels. Ranges scrolled out
    /// of sight, and empty ones, contribute nothing.
    pub fn cache_band_spans(&self, ranges: &[Range<u64>], viewport_width: f64) -> Vec<(f64, f64)> {
        ranges
            .iter()
            .filter(|range| range.end > range.start)
            .filter_map(|range| {
                self.loop_range_span(LoopRange::new(range.start, range.end - 1), viewport_width)
            })
            .collect()
    }

    fn x_to_frame_f64(&self, x: f64) -> f64 {
        x / self.pixels_per_frame + self.scroll_offset
    }

    pub fn title_key(&self) -> &'static str {
        PanelKind::Timeline.label_key()
    }
}

fn channel_refs(composition: &Composition) -> HashSet<TimelineChannelRef> {
    composition
        .layers
        .iter()
        .flat_map(|layer| {
            crate::keyframes::property_rows(layer)
                .into_iter()
                .flat_map(move |row| {
                    (0..row.channel_names.len()).map(move |component| TimelineChannelRef {
                        layer: layer.id,
                        row: row.id.clone(),
                        component,
                    })
                })
        })
        .collect()
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
        assert!(p.composition().is_none(), "no composition is active yet");
        assert_eq!(p.duration_frames(), 0);
        assert_eq!(p.layers().count(), 0);
        assert_eq!(p.view_mode(), TimelineViewMode::Bars);
        assert!(p.selected_channels().is_empty());
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
        p.set_composition(Some(comp));
        assert_eq!(p.comp_id(), Some(CompId::new(42)));
        assert_eq!(p.composition().unwrap().layer_count(), 1);
        assert_eq!(p.frame_rate(), FrameRate::new(24, 1));
        assert_eq!(p.duration_frames(), 240);

        // Composition 0: the mirror empties out instead of keeping a stale
        // composition on screen.
        p.set_composition(None);
        assert_eq!(p.comp_id(), None);
        assert_eq!(p.layers().count(), 0);
        assert_eq!(p.duration_frames(), 0);
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
    fn view_mode_can_be_selected_and_toggled() {
        let mut p = panel();
        p.set_view_mode(TimelineViewMode::Graph);
        assert_eq!(p.view_mode(), TimelineViewMode::Graph);
        p.toggle_view_mode();
        assert_eq!(p.view_mode(), TimelineViewMode::Bars);
    }

    #[test]
    fn graph_channel_selection_supports_replace_and_shift_toggle() {
        let mut p = panel();
        let position_x = TimelineChannelRef {
            layer: LayerId::new(1),
            row: crate::keyframes::PropertyRowId::Shell(PropertyGroup::Position),
            component: 0,
        };
        let position_y = TimelineChannelRef {
            component: 1,
            ..position_x.clone()
        };

        p.select_channel(position_x.clone(), false);
        assert_eq!(p.selected_channels(), std::slice::from_ref(&position_x));

        p.select_channel(position_y.clone(), true);
        assert_eq!(p.selected_channels(), &[position_x.clone(), position_y]);

        p.select_channel(position_x, true);
        assert_eq!(p.selected_channels().len(), 1);
    }

    #[test]
    fn composition_sync_drops_only_stale_graph_channels() {
        let layer_id = LayerId::new(7);
        let comp = Composition::new(
            CompId::new(42),
            "Test",
            (1280, 720),
            FrameRate::new(24, 1),
            240,
        )
        .add_layer(Layer::new(layer_id, "Solid", Graph::new()).with_time(0, 0, 240));
        let mut p = TimelinePanel::with_composition(comp.clone());
        let selected = TimelineChannelRef {
            layer: layer_id,
            row: crate::keyframes::PropertyRowId::Shell(PropertyGroup::Opacity),
            component: 0,
        };
        p.select_channel(selected.clone(), false);

        p.set_composition(Some(comp));
        assert!(p.is_channel_selected(&selected));

        p.set_composition(Some(Composition::new(
            CompId::new(99),
            "Empty",
            (1280, 720),
            FrameRate::new(24, 1),
            240,
        )));
        assert!(p.selected_channels().is_empty());
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
        let position = crate::keyframes::PropertyRowId::Shell(PropertyGroup::Position);
        let scale = PropertyRowId::Shell(PropertyGroup::Scale);
        assert!(!p.is_property_expanded(lid, &position));
        p.toggle_property_expanded(lid, position.clone());
        assert!(p.is_property_expanded(lid, &position));
        assert!(!p.is_property_expanded(lid, &scale));
    }

    // ----- Reveal filters --------------------------------------------------

    /// A layer with a keyframed Position X, an expression on Opacity and a
    /// moved (but constant) Scale — one row per reveal criterion.
    fn reveal_layer() -> Layer {
        use ravel_core::animation::channel::{
            AnimationChannel, ChannelSource, ParameterExpression,
        };
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;

        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        let mut layer = Layer::new(LayerId::new(1), "L", Graph::new()).with_time(0, 0, 100);
        layer.transform.position[0] = AnimationChannel::keyframes(curve);
        layer.transform.scale[0] = AnimationChannel::constant(2.0);
        layer.opacity =
            AnimationChannel::new(ChannelSource::Expression(ParameterExpression::new("1.0")));
        layer
    }

    fn revealed(panel: &TimelinePanel, layer: &Layer) -> Vec<crate::keyframes::PropertyRowId> {
        panel
            .visible_property_rows(layer)
            .into_iter()
            .map(|row| row.id)
            .collect()
    }

    #[test]
    fn no_filter_shows_every_row() {
        let p = panel();
        let layer = reveal_layer();
        assert_eq!(
            p.visible_property_rows(&layer).len(),
            crate::keyframes::property_rows(&layer).len()
        );
    }

    #[test]
    fn a_group_filter_keeps_only_that_group() {
        let mut p = panel();
        let layer = reveal_layer();
        p.apply_reveal(RevealFilter::Group(PropertyGroup::Position), false);
        assert_eq!(
            revealed(&p, &layer),
            vec![crate::keyframes::PropertyRowId::Shell(
                PropertyGroup::Position
            )]
        );

        // A layer without the group reveals nothing rather than erroring.
        let bare = Layer::new(LayerId::new(2), "B", Graph::new());
        p.apply_reveal(RevealFilter::Group(PropertyGroup::AudioGain), false);
        assert!(revealed(&p, &bare).is_empty());
    }

    #[test]
    fn animated_modified_and_expression_select_their_own_rows() {
        let layer = reveal_layer();
        let rows = |filter| {
            let mut p = panel();
            p.apply_reveal(filter, false);
            revealed(&p, &layer)
        };
        let shell = crate::keyframes::PropertyRowId::Shell;

        assert_eq!(
            rows(RevealFilter::Animated),
            vec![shell(PropertyGroup::Position)]
        );
        assert_eq!(
            rows(RevealFilter::Expression),
            vec![shell(PropertyGroup::Opacity)]
        );
        // Everything the user touched: the keyframed Position, the scaled
        // Scale, and the Opacity expression. Anchor Point and Rotation are
        // untouched, so they drop out.
        assert_eq!(
            rows(RevealFilter::Modified),
            vec![
                shell(PropertyGroup::Position),
                shell(PropertyGroup::Scale),
                shell(PropertyGroup::Opacity),
            ]
        );
    }

    /// An expression reached through a blend still counts: the Properties
    /// badge calls that channel expression-driven, so the reveal must agree.
    #[test]
    fn a_blended_expression_still_reveals_the_row() {
        use ravel_core::animation::channel::{
            AnimationChannel, ChannelSource, ParameterExpression,
        };

        let mut layer = reveal_layer();
        layer.transform.rotation = AnimationChannel::new(ChannelSource::Blend(
            Box::new(ChannelSource::Constant(0.0)),
            Box::new(ChannelSource::Expression(ParameterExpression::new("2.0"))),
            Default::default(),
            0.5,
        ));

        let mut p = panel();
        p.apply_reveal(RevealFilter::Expression, false);
        assert!(
            revealed(&p, &layer).contains(&crate::keyframes::PropertyRowId::Shell(
                PropertyGroup::Rotation
            )),
            "a blended expression is still an expression"
        );
    }

    #[test]
    fn unmodified_replaces_shift_adds_and_the_same_key_clears() {
        let mut p = panel();
        let position = RevealFilter::Group(PropertyGroup::Position);
        let scale = RevealFilter::Group(PropertyGroup::Scale);

        p.apply_reveal(position, false);
        assert_eq!(p.reveal_filters().len(), 1);

        // Shift adds to the current filter…
        p.apply_reveal(scale, true);
        assert_eq!(
            p.reveal_filters(),
            &HashSet::from([position, scale]),
            "Shift adds instead of replacing"
        );
        // …and takes the same group back out.
        p.apply_reveal(scale, true);
        assert_eq!(p.reveal_filters(), &HashSet::from([position]));

        // Unmodified replaces.
        p.apply_reveal(scale, false);
        assert_eq!(p.reveal_filters(), &HashSet::from([scale]));
        // The same key again shows everything.
        p.apply_reveal(scale, false);
        assert!(p.reveal_filters().is_empty());
    }

    /// Reselecting a layer — which reaches the panel as a composition sync —
    /// must not drop the filter.
    #[test]
    fn the_filter_survives_a_composition_sync() {
        let comp = Composition::new(
            CompId::new(1),
            "Test",
            (1280, 720),
            FrameRate::new(24, 1),
            240,
        )
        .add_layer(reveal_layer());
        let mut p = TimelinePanel::with_composition(comp.clone());
        p.apply_reveal(RevealFilter::Animated, false);
        p.set_composition(Some(comp));
        assert_eq!(p.reveal_filters(), &HashSet::from([RevealFilter::Animated]));
    }

    // ----- Composition end -------------------------------------------------

    fn panel_with_duration(duration_frames: u64) -> TimelinePanel {
        let mut p = panel();
        p.set_composition(Some(Composition::new(
            CompId::new(1),
            "Test",
            (1280, 720),
            FrameRate::new(30, 1),
            duration_frames,
        )));
        p.set_pixels_per_frame(2.0);
        p
    }

    #[test]
    fn the_out_of_range_span_starts_at_the_last_frame() {
        let p = panel_with_duration(100);
        // 100 frames * 2 px = 200 px of composition inside a 400 px lane.
        assert_eq!(p.out_of_range_span(400.0), Some((200.0, 200.0)));
    }

    #[test]
    fn a_viewport_inside_the_composition_has_no_out_of_range_span() {
        let p = panel_with_duration(1000);
        assert_eq!(p.out_of_range_span(400.0), None);
    }

    #[test]
    fn scrolling_past_the_end_shades_the_whole_viewport() {
        let mut p = panel_with_duration(100);
        p.set_scroll_offset(300.0);
        assert_eq!(p.out_of_range_span(400.0), Some((0.0, 400.0)));
    }

    #[test]
    fn no_composition_has_nothing_to_be_outside_of() {
        let p = panel();
        assert_eq!(p.out_of_range_span(400.0), None);
        // A degenerate viewport is never shaded either.
        assert_eq!(panel_with_duration(100).out_of_range_span(0.0), None);
    }

    #[test]
    fn the_loop_range_span_covers_the_out_frame_and_clips_to_the_viewport() {
        let mut p = panel_with_duration(1000);
        // 2 px per frame: frames 10..=39 occupy [20, 80).
        assert_eq!(
            p.loop_range_span(LoopRange::new(10, 39), 400.0),
            Some((20.0, 60.0))
        );
        // A one-frame loop is still one frame wide.
        assert_eq!(
            p.loop_range_span(LoopRange::new(10, 10), 400.0),
            Some((20.0, 2.0))
        );
        // Clipped at both edges rather than drawn outside the ruler.
        assert_eq!(
            p.loop_range_span(LoopRange::new(0, 999), 400.0),
            Some((0.0, 400.0))
        );

        p.set_scroll_offset(300.0);
        assert_eq!(p.loop_range_span(LoopRange::new(10, 39), 400.0), None);
        p.set_scroll_offset(0.0);
        assert_eq!(p.loop_range_span(LoopRange::new(10, 39), 0.0), None);
    }

    /// The band has to show exactly the frames `cached_ranges` reports —
    /// a band drawn one frame wide of the truth is a band that lies about
    /// what a scrub will cost.
    #[test]
    fn the_cache_band_spans_cover_exactly_the_cached_ranges() {
        let mut p = panel_with_duration(1000);
        // 2 px per frame: 0..3 occupies [0, 6), 10..12 occupies [20, 24).
        assert_eq!(
            p.cache_band_spans(&[0..3, 10..12], 400.0),
            vec![(0.0, 6.0), (20.0, 4.0)]
        );
        // One cached frame is one frame wide.
        assert_eq!(
            p.cache_band_spans(&[7..8, 20..21], 400.0),
            vec![(14.0, 2.0), (40.0, 2.0)]
        );
        // Empty ranges contribute nothing rather than a zero-width quad.
        assert_eq!(
            p.cache_band_spans(&[5..5, 9..9], 400.0),
            Vec::<(f64, f64)>::new()
        );
        assert_eq!(p.cache_band_spans(&[], 400.0), Vec::<(f64, f64)>::new());
        // Scrolled out of sight, and a degenerate viewport, draw nothing.
        p.set_scroll_offset(300.0);
        assert_eq!(
            p.cache_band_spans(&[0..3, 4..6], 400.0),
            Vec::<(f64, f64)>::new()
        );
        p.set_scroll_offset(0.0);
        assert_eq!(
            p.cache_band_spans(&[0..3, 4..6], 0.0),
            Vec::<(f64, f64)>::new()
        );
    }

    // ----- BPM grid --------------------------------------------------------

    #[test]
    fn the_default_grid_is_off_at_a_usable_tempo() {
        let grid = BpmGrid::default();
        assert!(!grid.enabled);
        assert_eq!(grid.bpm, 120.0);
        assert_eq!(grid.offset_frames, 0.0);
    }

    #[test]
    fn a_beat_is_one_second_of_frames_at_sixty_bpm() {
        let grid = BpmGrid {
            bpm: 60.0,
            ..BpmGrid::default()
        };
        assert_eq!(grid.frames_per_beat(FrameRate::new(24, 1)), 24.0);
        // 24 fps, 120 BPM: a beat every 12 frames, on whole frames.
        let grid = BpmGrid {
            bpm: 120.0,
            ..BpmGrid::default()
        };
        assert_eq!(
            grid.beat_frames(FrameRate::new(24, 1), 0.0, 36.0),
            vec![0.0, 12.0, 24.0, 36.0]
        );
    }

    /// The interesting case: neither the frame rate nor the tempo divides
    /// A hand-built grid can carry a non-finite offset without passing
    /// through `sanitized`; the beat query has to answer with nothing rather
    /// than with NaN positions.
    #[test]
    fn a_non_finite_offset_yields_no_beats() {
        let fr = FrameRate::new(24, 1);
        for offset in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let grid = BpmGrid {
                enabled: true,
                offset_frames: offset,
                ..BpmGrid::default()
            };
            assert!(
                grid.beat_frames(fr, 0.0, 1000.0).is_empty(),
                "offset {offset} must produce no beats"
            );
        }
    }

    /// evenly, so the beats sit between frames and must stay there.
    #[test]
    fn beats_stay_on_fractional_frames() {
        let grid = BpmGrid {
            bpm: 140.0,
            ..BpmGrid::default()
        };
        let fr = FrameRate::new(30000, 1001); // 29.97
        let step = grid.frames_per_beat(fr);
        assert!((step - 12.8443).abs() < 0.001, "unexpected step: {step}");
        let beats = grid.beat_frames(fr, 0.0, 40.0);
        assert_eq!(beats.len(), 4);
        for (index, beat) in beats.iter().enumerate() {
            // Computed from the index, so beat 100 is as exact as beat 1.
            assert!((beat - index as f64 * step).abs() < 1e-9);
        }
        let far = grid.beat_frames(fr, 12_844.0, 12_857.0);
        assert!((far[0] - 1000.0 * step).abs() < 1e-9, "drifted: {far:?}");
    }

    #[test]
    fn the_offset_moves_beat_one_and_earlier_beats_are_dropped() {
        let grid = BpmGrid {
            bpm: 120.0,
            offset_frames: 7.0,
            ..BpmGrid::default()
        };
        let fr = FrameRate::new(24, 1);
        assert_eq!(grid.beat_frames(fr, 0.0, 31.0), vec![7.0, 19.0, 31.0]);
        // Nothing before beat 1, even when the viewport starts before it.
        assert_eq!(grid.beat_frames(fr, -100.0, 6.0), Vec::<f64>::new());
    }

    #[test]
    fn a_degenerate_tempo_yields_no_beats_instead_of_looping() {
        let fr = FrameRate::new(24, 1);
        for bpm in [0.0, -60.0, f64::NAN, f64::INFINITY] {
            let grid = BpmGrid {
                bpm,
                ..BpmGrid::default()
            };
            assert!(
                grid.beat_frames(fr, 0.0, 1000.0).is_empty(),
                "bpm {bpm} must produce no beats"
            );
            assert!(!grid.is_legible_at(fr, 4.0), "bpm {bpm} must not be drawn");
        }
    }

    #[test]
    fn sanitizing_pulls_hand_edited_values_back_into_range() {
        let sanitized = |bpm, offset| {
            BpmGrid {
                enabled: true,
                bpm,
                offset_frames: offset,
            }
            .sanitized()
        };
        assert_eq!(sanitized(0.0, 0.0).bpm, MIN_BPM);
        assert_eq!(sanitized(100_000.0, 0.0).bpm, MAX_BPM);
        assert_eq!(sanitized(f64::NAN, 0.0).bpm, BpmGrid::default().bpm);
        assert_eq!(sanitized(120.0, f64::INFINITY).offset_frames, 0.0);
        assert!(
            sanitized(120.0, 5.0).enabled,
            "the toggle is never rewritten"
        );
    }

    #[test]
    fn a_grid_too_dense_to_read_is_not_drawn() {
        let grid = BpmGrid {
            bpm: 120.0,
            ..BpmGrid::default()
        };
        let fr = FrameRate::new(24, 1); // 12 frames per beat
        assert!(grid.is_legible_at(fr, 1.0), "12 px apart is readable");
        assert!(!grid.is_legible_at(fr, 0.1), "1.2 px apart is a smear");
    }

    #[test]
    fn the_beat_query_is_capped() {
        let grid = BpmGrid {
            bpm: MAX_BPM,
            ..BpmGrid::default()
        };
        let beats = grid.beat_frames(FrameRate::new(24, 1), 0.0, 1e9);
        assert_eq!(beats.len(), MAX_BEATS_PER_QUERY);
    }
    // ----- Keyframe snapping -----------------------------------------------

    /// A composition holding one layer whose Position X is keyed at
    /// `position_keys` and whose Rotation is keyed at `rotation_keys`
    /// (layer-local frames; the layer starts at composition frame 0).
    fn snap_composition(position_keys: &[u64], rotation_keys: &[u64]) -> (TimelinePanel, LayerId) {
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::animation::curve::KeyframeCurve;
        use ravel_core::animation::interpolation::Interpolation;

        let curve = |frames: &[u64]| {
            let mut curve = KeyframeCurve::new();
            for frame in frames {
                curve.insert(*frame, 0.0, Interpolation::Linear);
            }
            AnimationChannel::keyframes(curve)
        };
        let lid = LayerId::new(1);
        let mut layer = Layer::new(lid, "L", Graph::new()).with_time(0, 0, 200);
        layer.transform.position[0] = curve(position_keys);
        layer.transform.rotation = curve(rotation_keys);
        let comp = Composition::new(CompId::new(1), "C", (16, 16), FrameRate::new(30, 1), 200)
            .add_layer(layer);
        let mut panel = panel();
        panel.set_composition(Some(comp));
        (panel, lid)
    }

    /// Open the two keyed property rows of [`snap_composition`]'s layer.
    /// Keyframe diamonds live on the channel sub-rows, so a property row has
    /// to be expanded before its keys are on screen at all.
    fn expand_snap_rows(panel: &mut TimelinePanel, lid: LayerId) {
        panel.toggle_layer_expanded(lid);
        panel.toggle_property_expanded(
            lid,
            crate::keyframes::PropertyRowId::Shell(PropertyGroup::Position),
        );
        panel.toggle_property_expanded(
            lid,
            crate::keyframes::PropertyRowId::Shell(PropertyGroup::Rotation),
        );
    }

    /// The completion criterion for the snap: it sees exactly what the panel
    /// shows. A collapsed layer offers nothing, and a reveal filter takes the
    /// rows it hides out of the candidate set with them.
    #[test]
    fn snap_candidates_are_the_rows_the_panel_shows() {
        let (mut p, lid) = snap_composition(&[10, 40], &[25]);
        assert!(
            p.visible_keyframe_frames().is_empty(),
            "a collapsed layer has no keyframes on screen"
        );

        p.toggle_layer_expanded(lid);
        assert!(
            p.visible_keyframe_frames().is_empty(),
            "an expanded layer whose property rows are collapsed draws no keys"
        );

        p.toggle_property_expanded(
            lid,
            crate::keyframes::PropertyRowId::Shell(PropertyGroup::Position),
        );
        p.toggle_property_expanded(
            lid,
            crate::keyframes::PropertyRowId::Shell(PropertyGroup::Rotation),
        );
        assert_eq!(p.visible_keyframe_frames(), vec![10, 25, 40]);

        p.apply_reveal(RevealFilter::Group(PropertyGroup::Position), false);
        assert_eq!(
            p.visible_keyframe_frames(),
            vec![10, 40],
            "a row the reveal filter hides takes its keys with it"
        );
    }

    /// A step row's keys are on screen like any other row's, so the playhead
    /// snaps to them too — the point of routing the enumeration through
    /// `row_key_frames` instead of unwrapping an `AnimationChannel`.
    #[test]
    fn snap_candidates_include_step_row_keys() {
        use ravel_core::animation::step::StepCurve;
        use ravel_core::graph::{Node, ParameterValue};
        use ravel_core::id::NodeId;

        let mut steps = StepCurve::new("a".to_string());
        steps.insert(30, "b".to_string());
        let network = Graph::new()
            .add_node(
                Node::new(NodeId::new(7), "text")
                    .with_param("body", ParameterValue::StringSteps(steps)),
            )
            .unwrap();
        let lid = LayerId::new(1);
        let comp = Composition::new(CompId::new(1), "C", (16, 16), FrameRate::new(30, 1), 200)
            .add_layer(Layer::new(lid, "L", network).with_time(0, 0, 200));
        let mut p = panel();
        p.set_composition(Some(comp));
        let row = crate::keyframes::PropertyRowId::Network {
            node: NodeId::new(7),
            key: "body".into(),
        };
        p.toggle_layer_expanded(lid);
        p.toggle_property_expanded(lid, row);
        assert_eq!(p.visible_keyframe_frames(), vec![30]);
    }

    /// Keys pushed before composition frame 0 by a negative `start_frame` are
    /// not positions the (unsigned) playhead can snap to.
    #[test]
    fn snap_candidates_drop_keys_before_the_composition_start() {
        let (mut p, lid) = snap_composition(&[0, 60], &[]);
        expand_snap_rows(&mut p, lid);
        let mut comp = p.composition().unwrap().clone();
        let mut layer = comp.layers[0].clone();
        layer.start_frame = -20;
        comp.layers.set(0, layer);
        p.set_composition(Some(comp));
        assert_eq!(p.visible_keyframe_frames(), vec![40]);
    }

    /// The pull is a pixel radius, so it covers the same gesture at every
    /// zoom rather than a widening band of frames.
    #[test]
    fn snap_pull_is_a_pixel_radius_at_any_zoom() {
        let (mut p, lid) = snap_composition(&[20], &[]);
        expand_snap_rows(&mut p, lid);
        let candidates = p.visible_keyframe_frames();

        for ppf in [1.0, 4.0, 40.0] {
            p.set_pixels_per_frame(ppf);
            let key_x = p.frame_to_x(20);
            assert_eq!(
                p.snap_playhead_x(key_x + KEYFRAME_SNAP_RADIUS_PX - 0.5, &candidates),
                20,
                "inside the radius at {ppf} px/frame"
            );
            let outside = key_x + KEYFRAME_SNAP_RADIUS_PX + 0.5;
            assert_eq!(
                p.snap_playhead_x(outside, &candidates),
                p.x_to_frame(outside),
                "outside the radius the pointer wins at {ppf} px/frame"
            );
        }
    }

    /// With two keys inside the radius the nearer one wins, and an empty
    /// candidate set is the plain pointer frame.
    #[test]
    fn snap_takes_the_nearest_candidate() {
        let (mut p, lid) = snap_composition(&[20, 22], &[]);
        expand_snap_rows(&mut p, lid);
        let candidates = p.visible_keyframe_frames();
        p.set_pixels_per_frame(4.0);
        assert_eq!(p.snap_playhead_x(p.frame_to_x(22) - 1.0, &candidates), 22);
        assert_eq!(p.snap_playhead_x(p.frame_to_x(20) + 1.0, &candidates), 20);
        assert_eq!(
            p.snap_playhead_x(p.frame_to_x(21), &candidates),
            20,
            "an exact tie goes to the earlier frame"
        );
        assert_eq!(
            p.snap_playhead_x(p.frame_to_x(21), &[]),
            21,
            "no candidates is the frame under the pointer"
        );
    }

    /// Media-import plan unit 7: the layer row's offline mark. The set is
    /// filled from the document on the sync (never in `render()`), covers only
    /// the layers whose media resolves to nothing, and is dropped by a
    /// composition switch so a same-numbered layer cannot inherit it.
    #[test]
    fn the_offline_mark_follows_the_document_and_not_the_layer_number() {
        use ravel_core::composition::{Document, MEDIA_ASSET_PARAM_KEY, MediaAssetEntry};
        use ravel_core::graph::{Node, Parameter, ParameterValue};
        use ravel_core::id::{AssetId, DataTypeId, LayerId, NodeId};
        use std::sync::Arc;

        fn media_network(asset: AssetId) -> Graph {
            let mut node =
                Node::new(NodeId::next(), "media").with_output("frame", DataTypeId::FRAME_BUFFER);
            node.parameters.push(Parameter {
                key: MEDIA_ASSET_PARAM_KEY.to_string(),
                value: ParameterValue::String(asset.to_param_value()),
            });
            Graph::new().add_node(node).unwrap()
        }

        let gone = AssetId::next();
        let here = AssetId::next();
        let broken = Layer::new(LayerId::next(), "Broken", media_network(gone));
        let fine = Layer::new(LayerId::next(), "Fine", media_network(here));
        let broken_id = broken.id;
        let fine_id = fine.id;
        let comp = Composition::new(
            CompId::next(),
            "Comp 1",
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        )
        .add_layer(broken)
        .add_layer(fine);
        let comp_id = comp.id;

        let mut doc = Document::default();
        doc = doc.with_media_asset_entry(
            gone,
            MediaAssetEntry {
                resolved: None,
                ..MediaAssetEntry::from_absolute("/media/gone.mov")
            },
        );
        doc = doc.with_media_asset_entry(here, MediaAssetEntry::from_absolute("/media/clip.mov"));
        doc.compositions.insert(comp_id, Arc::new(comp.clone()));

        let mut p = TimelinePanel::new(FrameRate::new(30, 1));
        p.set_composition(Some(comp));
        assert!(
            !p.is_layer_offline(broken_id),
            "nothing is marked before the host has synced the document"
        );

        p.sync_offline_layers(&doc);
        assert!(p.is_layer_offline(broken_id));
        assert!(
            !p.is_layer_offline(fine_id),
            "a layer whose media resolves carries no mark"
        );

        // A switch replaces what the panel shows; the marks describe the
        // composition that is gone.
        p.set_composition(None);
        assert!(!p.is_layer_offline(broken_id));
    }
}
