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
use ravel_ui::panels::viewer::ViewerResolution;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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

    /// The Properties parameter groups the user has folded away, as
    /// `(type_key, group)` pairs.
    ///
    /// Keyed by node **type**, not by node: the groups belong to the type's
    /// declaration (`parameter-groups-plan.md`), so folding "Source" away on
    /// one Grid and having to fold it again on the next one would be busywork.
    /// The group is the short name the template declares (`""` for the
    /// leading section holding whatever no group claims).
    ///
    /// Only the **folded** groups are listed, because the default is
    /// all-expanded: an untouched project writes no entry, and deleting
    /// `ui_state.json` opens everything. Adding the field left
    /// `format_version` alone for the same reason `bpm_grid` did — see the
    /// decision table in `docs/dev/persistence.md`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collapsed_param_groups: Vec<(String, String)>,

    /// Whether the Node Graph editor draws the parameter name/value rows in
    /// the node bodies (`parameter-groups-plan.md`, PGRP-5).
    ///
    /// One flag for the whole canvas rather than one per node: a per-node
    /// setting is more state to carry for a preference nobody sets twice.
    ///
    /// `None` (or a missing entry) is the ordinary first-run state and reads
    /// as "drawn" through [`Self::show_node_param_values`], so an untouched
    /// project writes no entry and `format_version` stays put — the same rule
    /// as `bpm_grid` above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_node_param_values: Option<bool>,

    /// The viewer's preview resolution factor
    /// (`viewer-preview-resolution-plan.md`, `VRES-3`).
    ///
    /// It says how the user is looking at the composition, not what the frame
    /// contains, so it belongs here and never in `.ravprj`'s document — an
    /// export renders at composition resolution whatever this says.
    ///
    /// `None` (or a missing entry) is the ordinary first-run state and reads
    /// as [`ViewerResolution::default`] through [`Self::viewer_resolution`],
    /// so an untouched project writes no entry and `format_version` stays put
    /// — the same rule as `bpm_grid` above.
    ///
    /// An unreadable value reads as absent rather than failing the whole
    /// entry: this file is hand-editable, and one mistyped factor must not
    /// cost the user their active composition, loop ranges and folded groups
    /// as well (`ProjectFile::from_archive` drops a `ui_state.json` it cannot
    /// parse *in full*).
    #[serde(
        default,
        deserialize_with = "deserialize_tolerant_resolution",
        skip_serializing_if = "Option::is_none"
    )]
    pub viewer_resolution: Option<ViewerResolution>,
}

/// Read a preview resolution factor, treating an unknown one as absent.
///
/// `Option<ViewerResolution>`'s own implementation would fail the
/// deserialization of the whole [`UiState`] on a hand-edited typo, and this
/// entry has no field whose loss is worth that.
fn deserialize_tolerant_resolution<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ViewerResolution>, D::Error> {
    // Through `serde_json::Value` rather than `Option<ViewerResolution>`,
    // because a failed enum deserialization is an error either way — the
    // untyped value is what lets this swallow it.
    let raw = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(raw).ok())
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

    /// The preview resolution factor the viewer should open with: the
    /// persisted one, or the default when the entry is absent or unreadable.
    pub fn viewer_resolution(&self) -> ViewerResolution {
        self.viewer_resolution.unwrap_or_default()
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

    /// The folded parameter groups, sanitized: duplicate pairs collapse into
    /// one and an entry with an empty `type_key` is dropped (it could not name
    /// a node type, so nothing would ever read it).
    ///
    /// This entry is hand-editable, so the reader is the tolerant layer: a
    /// `type_key` no build registers, or a group a template no longer
    /// declares, simply never matches a section and folds nothing.
    pub fn collapsed_param_groups(&self) -> BTreeSet<(String, String)> {
        self.collapsed_param_groups
            .iter()
            .filter(|(type_key, _)| !type_key.is_empty())
            .cloned()
            .collect()
    }

    /// Whether the Node Graph editor should draw the parameter rows in the
    /// node bodies: the persisted choice, or "drawn" when the entry is absent.
    pub fn show_node_param_values(&self) -> bool {
        self.show_node_param_values.unwrap_or(true)
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

    /// `ui_state.json` is hand-editable, so a reversed pair must not reach
    /// `LoopRange::wrap` — `frame - in_frame` would underflow there.
    #[test]
    fn a_hand_edited_loop_range_is_ordered_on_read() {
        let state =
            UiState::from_json(r#"{"loop_ranges": [[1, {"in_frame": 40, "out_frame": 10}]]}"#)
                .expect("a reversed pair must load");
        let range = state.loop_ranges[0].1;
        assert_eq!(range, LoopRange::new(10, 40));
        // The invariant the fold relies on: no wrap can underflow.
        assert_eq!(range.wrap(41), 10);

        let (root, document) = (CompId::new(1), document_with(&[CompId::new(1)]));
        assert_eq!(state.loop_ranges(&document)[&root], LoopRange::new(10, 40));
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
    /// The default writes no entry, so an untouched project keeps the
    /// `ui_state.json` earlier builds wrote — and an archive without the
    /// field opens with everything expanded.
    #[test]
    fn folded_parameter_groups_round_trip_and_are_absent_by_default() {
        let json = UiState::default().to_json().unwrap();
        assert!(
            !json.contains("collapsed_param_groups"),
            "unexpected json: {json}"
        );
        assert!(
            UiState::from_json(&json)
                .unwrap()
                .collapsed_param_groups()
                .is_empty()
        );

        let state = UiState {
            collapsed_param_groups: vec![
                ("scatter.grid".to_string(), "source".to_string()),
                ("math.curve".to_string(), String::new()),
            ],
            ..UiState::default()
        };
        let json = state.to_json().unwrap();
        let back = UiState::from_json(&json).unwrap();
        assert_eq!(back, state);
        assert_eq!(
            back.collapsed_param_groups(),
            BTreeSet::from([
                ("math.curve".to_string(), String::new()),
                ("scatter.grid".to_string(), "source".to_string()),
            ])
        );
    }

    /// A hand-edited entry must not cost the project its UI state: a repeated
    /// pair is one folded group and an entry naming no node type is dropped.
    #[test]
    fn a_hand_edited_folded_group_list_is_sanitized_on_read() {
        let state = UiState::from_json(
            r#"{"collapsed_param_groups": [
                ["scatter.grid", "source"],
                ["scatter.grid", "source"],
                ["", "source"],
                ["field.apply", "scope"]
            ]}"#,
        )
        .expect("a hand-edited list must still parse");
        assert_eq!(
            state.collapsed_param_groups(),
            BTreeSet::from([
                ("field.apply".to_string(), "scope".to_string()),
                ("scatter.grid".to_string(), "source".to_string()),
            ])
        );
    }

    /// An archive written before this field existed — the `ui_state.json` of
    /// every earlier build — opens with every group expanded.
    #[test]
    fn an_entry_without_the_field_opens_fully_expanded() {
        let state = UiState::from_json(r#"{"active_comp": 1}"#).unwrap();
        assert!(state.collapsed_param_groups().is_empty());
    }

    /// The node-body parameter rows are drawn by default, so only the hidden
    /// state is written — an untouched project keeps the `ui_state.json`
    /// earlier builds wrote, and an archive without the field opens with the
    /// rows visible.
    #[test]
    fn hidden_node_parameter_values_round_trip_and_are_absent_by_default() {
        let json = UiState::default().to_json().unwrap();
        assert!(
            !json.contains("show_node_param_values"),
            "unexpected json: {json}"
        );
        assert!(UiState::from_json(&json).unwrap().show_node_param_values());
        assert!(
            UiState::from_json(r#"{"active_comp": 1}"#)
                .unwrap()
                .show_node_param_values()
        );

        let state = UiState {
            show_node_param_values: Some(false),
            ..UiState::default()
        };
        let json = state.to_json().unwrap();
        let back = UiState::from_json(&json).unwrap();
        assert_eq!(back, state);
        assert!(!back.show_node_param_values());
    }

    /// The preview resolution factor rides the same optional-entry rule
    /// (`VRES-3`): the default writes nothing, a chosen factor round-trips,
    /// and an entry from a build that had no factor opens at the default.
    #[test]
    fn the_preview_resolution_round_trips_and_is_absent_by_default() {
        let json = UiState::default().to_json().unwrap();
        assert!(
            !json.contains("viewer_resolution"),
            "the default factor must not write an entry: {json}"
        );
        assert_eq!(
            UiState::from_json(r#"{"active_comp": 1}"#)
                .unwrap()
                .viewer_resolution(),
            ViewerResolution::default(),
            "an entry written before the factor existed opens at the default"
        );

        let state = UiState {
            viewer_resolution: Some(ViewerResolution::Full),
            ..UiState::default()
        };
        let json = state.to_json().unwrap();
        let back = UiState::from_json(&json).unwrap();
        assert_eq!(back, state);
        assert_eq!(back.viewer_resolution(), ViewerResolution::Full);
    }

    /// A hand-edited factor nobody can read costs that one field and nothing
    /// else: the rest of the entry — the active composition, the loop ranges,
    /// the folded groups — still loads.
    #[test]
    fn an_unreadable_preview_resolution_falls_back_without_losing_the_entry() {
        let state = UiState::from_json(
            r#"{"active_comp": 7, "viewer_resolution": "eighth", "show_node_param_values": false}"#,
        )
        .expect("one bad factor must not fail the whole entry");

        assert_eq!(state.viewer_resolution(), ViewerResolution::default());
        assert_eq!(state.active_comp, Some(CompId::new(7)));
        assert!(!state.show_node_param_values());
    }
}
