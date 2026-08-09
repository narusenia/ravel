// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Persisted UI state (`ui_state.json`) — REQ-UI-013.
//!
//! What the user was *looking at* is not part of the document: the active
//! composition must not land in the undo history (the `Document` snapshot is
//! the undo unit, so a composition switch would roll back with an edit) nor
//! in the saved document's diff. It lives in its own archive entry instead.
//!
//! The entry is **optional in both directions**: an archive without it loads
//! with defaults (the active composition falls back to `Document::root_comp`),
//! and unknown fields written by a newer Ravel are ignored rather than
//! rejected. Adding this entry kept `manifest.json`'s `format_version` at 3;
//! the current project format is v4 for unrelated asset-reference changes.
//! The UI-state entry does not change how any existing archive reads. An
//! entry that cannot be parsed at all degrades to the default too (with a
//! warning): it carries no user data, so it must never cost someone an
//! otherwise intact project.
//!
//! This is the container for future UI state as well (Outliner expansion,
//! node editor viewport, …); add fields with `#[serde(default)]` so old
//! archives keep loading.

use ravel_core::composition::Document;
use ravel_core::id::CompId;
use ravel_core::runtime::playback::LoopRange;
use ravel_ui::panels::timeline::BpmGrid;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// UI state persisted alongside a project.
///
/// Not `Eq`: [`BpmGrid`] carries a tempo as `f64`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// The composition that was active when the project was saved. `None`
    /// (or a missing entry) means "start on the document root".
    ///
    /// A load drops an id the document does not have
    /// (`ProjectFile::from_archive`), so a value read from a loaded project
    /// always resolves; [`Self::initial_active_comp`] adds the root
    /// fallback on top.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_comp: Option<CompId>,

    /// The Timeline's musical beat grid — on/off, tempo, and the frame that
    /// carries beat 1. It steers nothing in the rendered picture, which is
    /// why it lives here rather than on the composition and why adding it
    /// left `format_version` alone.
    ///
    /// `None` (or a missing entry) is the ordinary first-run state and reads
    /// as [`BpmGrid::default`]; use [`Self::bpm_grid`], which also sanitizes
    /// a hand-edited tempo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bpm_grid: Option<BpmGrid>,

    /// The loop range of every composition that has one, in id order.
    ///
    /// A list rather than a map because a JSON object cannot key on an
    /// integer, and per composition because that is the granularity of the
    /// feature — the range belongs to the composition you set it in.
    ///
    /// It lives here rather than on the `Composition` for the same reason as
    /// the beat grid: it is where you are working, not what the frame looks
    /// like. Handing a project to someone else therefore does not hand them
    /// your loop range (2026-08-09).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loop_ranges: Vec<(CompId, LoopRange)>,
}

impl UiState {
    /// The UI state of a session whose active composition is `active_comp`.
    pub fn with_active_comp(active_comp: Option<CompId>) -> Self {
        Self {
            active_comp,
            ..Self::default()
        }
    }

    /// The beat grid the Timeline should open with: the persisted one pulled
    /// back into range, or the default when the entry is absent.
    pub fn bpm_grid(&self) -> BpmGrid {
        self.bpm_grid.unwrap_or_default().sanitized()
    }

    /// The loop ranges that still apply to `document`: compositions it no
    /// longer has are dropped, and a range that outlived a shortened
    /// composition is pulled back inside it (or dropped when nothing of it is
    /// left). A hand-edited or stale entry can therefore never install a
    /// range that points past the end of a composition.
    pub fn loop_ranges(&self, document: &Document) -> BTreeMap<CompId, LoopRange> {
        self.loop_ranges
            .iter()
            .filter_map(|(id, range)| {
                let comp = document.get_composition(*id)?;
                Some((*id, range.clamped_to(comp.duration_frames)?))
            })
            .collect()
    }

    /// The composition the UI should open `document` on: the persisted one
    /// while it still exists, otherwise the document root. A composition
    /// deleted since the last save (or an entry from an unrelated project)
    /// therefore degrades to the root instead of leaving the UI on nothing.
    pub fn initial_active_comp(&self, document: &Document) -> Option<CompId> {
        self.active_comp
            .filter(|id| document.get_composition(*id).is_some())
            .or(document.root_comp)
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::Composition;
    use ravel_core::types::FrameRate;

    fn document_with(comps: &[CompId]) -> Document {
        let mut document = Document::default();
        for (index, id) in comps.iter().enumerate() {
            document = document.with_composition(Composition::new(
                *id,
                format!("Comp {index}"),
                (1920, 1080),
                FrameRate::new(30, 1),
                300,
            ));
        }
        document
    }

    #[test]
    fn json_roundtrip_keeps_the_active_composition() {
        let state = UiState::with_active_comp(Some(CompId::new(7)));
        let json = state.to_json().unwrap();
        assert!(
            json.contains("\"active_comp\": 7"),
            "unexpected json: {json}"
        );
        assert_eq!(UiState::from_json(&json).unwrap(), state);
    }

    #[test]
    fn an_empty_object_loads_as_the_default() {
        assert_eq!(UiState::from_json("{}").unwrap(), UiState::default());
    }

    /// Forward compatibility: a newer Ravel's extra fields must not make the
    /// entry unreadable for this build.
    #[test]
    fn unknown_fields_are_ignored() {
        let state = UiState::from_json(r#"{"active_comp": 3, "outliner_expanded": [1, 2]}"#)
            .expect("unknown fields must not fail the parse");
        assert_eq!(state.active_comp, Some(CompId::new(3)));
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(UiState::from_json("{ not json").is_err());
    }

    #[test]
    fn the_persisted_composition_wins_while_it_exists() {
        let (root, other) = (CompId::new(1), CompId::new(2));
        let mut document = document_with(&[root, other]);
        document.root_comp = Some(root);

        let state = UiState::with_active_comp(Some(other));
        assert_eq!(state.initial_active_comp(&document), Some(other));
    }

    #[test]
    fn a_missing_or_stale_composition_falls_back_to_the_root() {
        let root = CompId::new(1);
        let mut document = document_with(&[root]);
        document.root_comp = Some(root);

        // No entry at all (an archive written before ui_state.json existed).
        assert_eq!(
            UiState::default().initial_active_comp(&document),
            Some(root)
        );
        // An id that no longer resolves (composition deleted since the save).
        let stale = UiState::with_active_comp(Some(CompId::new(99)));
        assert_eq!(stale.initial_active_comp(&document), Some(root));
    }

    #[test]
    fn the_bpm_grid_round_trips_and_is_absent_by_default() {
        // The default state writes no entry at all, so an untouched project
        // keeps the same `ui_state.json` it always had.
        let json = UiState::default().to_json().unwrap();
        assert!(!json.contains("bpm_grid"), "unexpected json: {json}");
        assert_eq!(
            UiState::from_json(&json).unwrap().bpm_grid(),
            BpmGrid::default()
        );

        let state = UiState {
            bpm_grid: Some(BpmGrid {
                enabled: true,
                bpm: 174.0,
                offset_frames: 12.5,
            }),
            ..UiState::default()
        };
        let json = state.to_json().unwrap();
        let back = UiState::from_json(&json).unwrap();
        assert_eq!(back, state);
        assert_eq!(back.bpm_grid().bpm, 174.0);
    }

    #[test]
    fn loop_ranges_round_trip_and_are_absent_by_default() {
        // A project nobody looped in writes no entry at all, so its
        // `ui_state.json` stays byte-identical to what earlier builds wrote.
        let json = UiState::default().to_json().unwrap();
        assert!(!json.contains("loop_ranges"), "unexpected json: {json}");

        let state = UiState {
            loop_ranges: vec![(CompId::new(2), LoopRange::new(10, 40))],
            ..UiState::default()
        };
        let json = state.to_json().unwrap();
        assert_eq!(UiState::from_json(&json).unwrap(), state);
    }

    /// Ranges are per composition, and a load must not install one that
    /// points outside the composition it belongs to.
    #[test]
    fn loop_ranges_drop_stale_compositions_and_clamp_to_the_duration() {
        let (first, second) = (CompId::new(1), CompId::new(2));
        let document = document_with(&[first, second]); // 300 frames each
        let state = UiState {
            loop_ranges: vec![
                (first, LoopRange::new(10, 40)),
                (second, LoopRange::new(280, 900)),
                // A composition this document no longer has.
                (CompId::new(99), LoopRange::new(0, 10)),
            ],
            ..UiState::default()
        };

        let ranges = state.loop_ranges(&document);
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[&first], LoopRange::new(10, 40));
        assert_eq!(ranges[&second], LoopRange::new(280, 299));

        // Nothing of the range is left inside the composition.
        let state = UiState {
            loop_ranges: vec![(first, LoopRange::new(400, 500))],
            ..UiState::default()
        };
        assert!(state.loop_ranges(&document).is_empty());
    }

    /// A partial or hand-edited entry must not reach the painter as a
    /// degenerate grid.
    #[test]
    fn a_hand_edited_bpm_grid_is_sanitized_on_read() {
        let state = UiState::from_json(r#"{"bpm_grid": {"enabled": true}}"#).unwrap();
        assert_eq!(state.bpm_grid().bpm, BpmGrid::default().bpm);
        assert!(state.bpm_grid().enabled);

        let state = UiState::from_json(r#"{"bpm_grid": {"enabled": true, "bpm": 0}}"#).unwrap();
        assert_eq!(state.bpm_grid().bpm, ravel_ui::panels::timeline::MIN_BPM);
    }

    /// Composition 0: nothing to fall back to, and that is a valid state.
    #[test]
    fn a_document_without_compositions_has_no_active_composition() {
        let document = Document::default();
        assert_eq!(UiState::default().initial_active_comp(&document), None);
        assert_eq!(
            UiState::with_active_comp(Some(CompId::new(5))).initial_active_comp(&document),
            None
        );
    }
}
