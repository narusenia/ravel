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
//! rejected. That is what keeps `manifest.json`'s `format_version` at 3 —
//! adding this entry does not change how any existing archive reads.
//!
//! This is the container for future UI state as well (Outliner expansion,
//! node editor viewport, …); add fields with `#[serde(default)]` so old
//! archives keep loading.

use ravel_core::composition::Document;
use ravel_core::id::CompId;
use serde::{Deserialize, Serialize};

/// UI state persisted alongside a project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// The composition that was active when the project was saved. `None`
    /// (or a missing entry) means "start on the document root"; the id is
    /// only honoured while it still resolves in the loaded document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_comp: Option<CompId>,
}

impl UiState {
    /// The UI state of a session whose active composition is `active_comp`.
    pub fn with_active_comp(active_comp: Option<CompId>) -> Self {
        Self { active_comp }
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
