// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The persisted layout document, and the rule that decides which layout a
//! session runs on.
//!
//! Layouts are persisted in two places, both holding the same
//! [`LayoutDocument`] shape:
//!
//! | Where | What it means |
//! |---|---|
//! | `<config>/ravel/layout.toml` | the **application default** — the arrangement the user last worked in, restored at launch |
//! | `workspace_layout.toml` inside a `.ravprj` | an **opt-in** layout the project author chose to ship with it |
//!
//! Both carry a [`LAYOUT_VERSION`] stamp. Ravel has never persisted a layout
//! before, so there is nothing to migrate *from*: an unreadable document, or
//! one stamped with a version this build does not know, degrades to the
//! default layout. Losing a remembered arrangement is a small cost; refusing
//! to launch over it is not acceptable, so [`LayoutDocument::from_toml`]
//! reports the reason and every caller is expected to fall back.
//!
//! [`LayoutStore`] holds the application default alongside one bit of session
//! state: whether a project's embedded layout currently owns the session. That
//! bit is what keeps opening someone else's project from redecorating the
//! user's own workspace — while it is set, the live layout is *not* written
//! back as the application default.

use crate::layout::WorkspaceLayout;
use crate::preset::{BuiltinPreset, WorkspacePreset};
use serde::{Deserialize, Serialize};

/// On-disk version of the layout document.
///
/// Bump this only when an existing field changes meaning or shape. Purely
/// additive fields are read with `#[serde(default)]` and need no bump, exactly
/// as in `.ravprj` (`docs/dev/persistence.md`).
pub const LAYOUT_VERSION: u32 = 1;

/// A persisted workspace arrangement.
///
/// The extra fields beyond `layout` are only ever written to the application's
/// own `layout.toml`; the copy embedded in a project leaves them at their
/// defaults, so an embedded document never carries the user's preferences or
/// their private preset library into someone else's hands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutDocument {
    /// Version stamp; see [`LAYOUT_VERSION`].
    pub layout_version: u32,
    /// Whether saving a project embeds the current layout into it. Opt-in, so
    /// the default is `false` — a project is not a place to store someone
    /// else's screen arrangement unless its author asked for that.
    #[serde(default, skip_serializing_if = "is_not_set")]
    pub embed_in_projects: bool,
    /// The user's named layouts (REQ-UI-005).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_presets: Vec<WorkspacePreset>,
    /// Every window and its layout tree.
    pub layout: WorkspaceLayout,
}

fn is_not_set(value: &bool) -> bool {
    !*value
}

impl LayoutDocument {
    /// A document holding `layout`, stamped with the current version.
    pub fn new(layout: WorkspaceLayout) -> Self {
        Self {
            layout_version: LAYOUT_VERSION,
            embed_in_projects: false,
            custom_presets: Vec::new(),
            layout,
        }
    }

    /// Serializes to the TOML written to disk.
    pub fn to_toml(&self) -> Result<String, LayoutDocError> {
        toml::to_string_pretty(self).map_err(|e| LayoutDocError::Serialize(e.to_string()))
    }

    /// Parses a document, rejecting anything this build cannot read.
    ///
    /// The version is read from the raw table *before* the document is typed,
    /// so a future layout shape is reported as
    /// [`LayoutDocError::UnsupportedVersion`] rather than as a confusing parse
    /// error about fields this build happens not to have.
    pub fn from_toml(input: &str) -> Result<Self, LayoutDocError> {
        let table: toml::Table = input
            .parse()
            .map_err(|e: toml::de::Error| LayoutDocError::Parse(e.to_string()))?;
        let version = table
            .get("layout_version")
            .and_then(toml::Value::as_integer)
            .ok_or(LayoutDocError::MissingVersion)?;
        if version != i64::from(LAYOUT_VERSION) {
            return Err(LayoutDocError::UnsupportedVersion(version));
        }
        toml::from_str(input).map_err(|e| LayoutDocError::Parse(e.to_string()))
    }
}

impl Default for LayoutDocument {
    /// The layout a fresh installation starts on: the Edit preset in one
    /// window. This is also what a corrupt document degrades to.
    fn default() -> Self {
        Self::new(
            WorkspaceLayout::new(BuiltinPreset::Edit.preset().layout)
                .expect("built-in preset layouts are valid"),
        )
    }
}

/// Reasons a persisted layout document cannot be used. Every one of them means
/// "fall back to the default layout", never "fail to start".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutDocError {
    /// The document is not readable TOML, or its layout is structurally
    /// invalid (`WorkspaceLayout` validates on deserialization).
    #[error("failed to parse the layout document: {0}")]
    Parse(String),
    /// The document could not be serialized.
    #[error("failed to serialize the layout document: {0}")]
    Serialize(String),
    /// No version stamp: not a layout document at all.
    #[error("the layout document has no layout_version field")]
    MissingVersion,
    /// Written by a build that reads a different layout version.
    #[error("layout_version {0} is not readable by this build (which reads {LAYOUT_VERSION})")]
    UnsupportedVersion(i64),
}

/// The application-level layout preference, plus whether a project's embedded
/// layout currently owns the session.
///
/// The store is the only thing that decides what `layout.toml` will contain.
/// [`Self::capture`] folds the live session back into it, and refuses to do so
/// for the layout part while a project's embedded layout is in effect — that
/// refusal *is* the guarantee that opening a project with an embedded layout
/// leaves the user's own default alone.
#[derive(Debug, Clone, Default)]
pub struct LayoutStore {
    document: LayoutDocument,
    session_layout_active: bool,
}

impl LayoutStore {
    /// Builds a store around a restored document.
    pub fn new(document: LayoutDocument) -> Self {
        Self {
            document,
            session_layout_active: false,
        }
    }

    /// The document as it would be written to `layout.toml` right now.
    pub fn document(&self) -> &LayoutDocument {
        &self.document
    }

    /// The application default layout — what a session starts on, and what a
    /// project without an embedded layout falls back to.
    pub fn app_layout(&self) -> &WorkspaceLayout {
        &self.document.layout
    }

    /// The user's named layouts.
    pub fn custom_presets(&self) -> &[WorkspacePreset] {
        &self.document.custom_presets
    }

    /// Whether saving a project embeds the session layout into it.
    pub fn embed_in_projects(&self) -> bool {
        self.document.embed_in_projects
    }

    /// Sets the embed preference (the opt-in toggle).
    pub fn set_embed_in_projects(&mut self, embed: bool) {
        self.document.embed_in_projects = embed;
    }

    /// Whether the live layout came from a project rather than from the
    /// application default.
    pub fn session_layout_active(&self) -> bool {
        self.session_layout_active
    }

    /// Folds the live session state into the document that will be persisted.
    ///
    /// Named layouts are always taken — they are the user's, no matter which
    /// project is open. The *layout* is taken only while the session is still
    /// running on the application default: once a project's embedded layout
    /// has been applied, the live arrangement describes that project, and
    /// writing it back would replace the user's default with it.
    pub fn capture(&mut self, live: &WorkspaceLayout, custom_presets: Vec<WorkspacePreset>) {
        self.document.custom_presets = custom_presets;
        if !self.session_layout_active {
            self.document.layout = live.clone();
        }
    }

    /// The layout a project load installs: the project's own when it embedded
    /// one, otherwise the application default.
    ///
    /// Also records which of the two happened, so [`Self::capture`] knows
    /// whether the live layout still belongs to the user. Opening a project
    /// without an embedded layout therefore *returns* to the application
    /// default rather than inheriting the previous project's arrangement.
    pub fn layout_for_project(&mut self, embedded: Option<&WorkspaceLayout>) -> WorkspaceLayout {
        match embedded {
            Some(layout) => {
                self.session_layout_active = true;
                layout.clone()
            }
            None => {
                self.session_layout_active = false;
                self.document.layout.clone()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{LayoutNode, PanelInstance, PanelInstanceId};
    use crate::panel::PanelKind;

    fn layout_of(kind: PanelKind) -> WorkspaceLayout {
        WorkspaceLayout::new(LayoutNode::area(vec![PanelInstance::new(
            PanelInstanceId(0),
            kind,
        )]))
        .unwrap()
    }

    fn named(name: &str) -> WorkspacePreset {
        WorkspacePreset {
            name: name.to_owned(),
            layout: LayoutNode::area(vec![PanelInstance::new(
                PanelInstanceId(0),
                PanelKind::NodeGraph,
            )]),
        }
    }

    // -- wire format ---------------------------------------------------------

    #[test]
    fn toml_roundtrip_preserves_every_field() {
        let mut document = LayoutDocument::new(layout_of(PanelKind::Viewer));
        document.embed_in_projects = true;
        document.custom_presets = vec![named("Grading"), named("Review")];
        let toml = document.to_toml().unwrap();
        assert_eq!(LayoutDocument::from_toml(&toml).unwrap(), document);
    }

    /// The window placement and the always-on-top pin are what
    /// "restore my windows" means, so they have to survive the round trip.
    #[test]
    fn toml_roundtrip_preserves_placement_and_always_on_top() {
        let mut layout = layout_of(PanelKind::Viewer);
        layout
            .window_mut(crate::window::WindowId(0))
            .unwrap()
            .placement = Some(crate::window::WindowPlacement {
            x: -40.5,
            y: 96.0,
            width: 1440.0,
            height: 900.0,
        });
        layout
            .window_mut(crate::window::WindowId(0))
            .unwrap()
            .always_on_top = true;

        let document = LayoutDocument::new(layout.clone());
        let back = LayoutDocument::from_toml(&document.to_toml().unwrap()).unwrap();
        let window = back.layout.main_window();
        assert_eq!(window.placement, layout.main_window().placement);
        assert!(window.always_on_top);
    }

    /// A document written by this build stamps the current version, and the
    /// opt-in default is off.
    #[test]
    fn a_new_document_is_stamped_and_opts_out_of_embedding() {
        let document = LayoutDocument::new(layout_of(PanelKind::Viewer));
        assert_eq!(document.layout_version, LAYOUT_VERSION);
        assert!(!document.embed_in_projects);
        let toml = document.to_toml().unwrap();
        assert!(
            toml.contains(&format!("layout_version = {LAYOUT_VERSION}")),
            "unexpected toml: {toml}"
        );
        // Defaults are not written, so an embedded copy stays minimal.
        assert!(!toml.contains("embed_in_projects"), "{toml}");
        assert!(!toml.contains("custom_presets"), "{toml}");
    }

    #[test]
    fn a_document_without_a_version_is_rejected() {
        let document = LayoutDocument::new(layout_of(PanelKind::Viewer));
        let toml = document
            .to_toml()
            .unwrap()
            .replace(&format!("layout_version = {LAYOUT_VERSION}"), "");
        assert_eq!(
            LayoutDocument::from_toml(&toml),
            Err(LayoutDocError::MissingVersion)
        );
    }

    #[test]
    fn a_newer_version_is_rejected_by_version_not_by_shape() {
        let document = LayoutDocument::new(layout_of(PanelKind::Viewer));
        let toml = document.to_toml().unwrap().replace(
            &format!("layout_version = {LAYOUT_VERSION}"),
            &format!("layout_version = {}", LAYOUT_VERSION + 1),
        );
        assert_eq!(
            LayoutDocument::from_toml(&toml),
            Err(LayoutDocError::UnsupportedVersion(i64::from(
                LAYOUT_VERSION + 1
            )))
        );
    }

    /// Anything a corrupt file can be — truncated, not TOML at all, or a
    /// structurally invalid layout — has to come back as an error the caller
    /// can fall back from, never a panic.
    #[test]
    fn corrupt_documents_are_errors_not_panics() {
        let broken = [
            "",
            "not toml at all {{{",
            "layout_version = 1\n",
            "layout_version = \"one\"\n",
            // A valid version with a layout that breaks the model's invariants
            // (`next_instance_id` behind an id in use).
            "layout_version = 1\n[layout]\nnext_window_id = 1\nnext_instance_id = 0\n\
             [[layout.windows]]\nid = 0\nalways_on_top = false\n\
             [layout.windows.root]\ntype = \"area\"\nactive = 0\n\
             [[layout.windows.root.tabs]]\nid = 0\nkind = \"viewer\"\n",
        ];
        for input in broken {
            assert!(
                LayoutDocument::from_toml(input).is_err(),
                "must not accept: {input:?}"
            );
        }
    }

    /// Truncating a good document at every byte must never produce anything
    /// but an error — this is the shape a crash mid-write leaves behind.
    #[test]
    fn every_truncation_of_a_good_document_is_rejected_or_valid() {
        let toml = LayoutDocument::new(layout_of(PanelKind::Viewer))
            .to_toml()
            .unwrap();
        for cut in 0..toml.len() {
            // Only assert "no panic, and a valid document if it parses".
            if let Ok(document) = LayoutDocument::from_toml(&toml[..cut]) {
                assert!(document.layout.is_valid());
            }
        }
    }

    // -- store ---------------------------------------------------------------

    #[test]
    fn capture_records_the_live_layout_as_the_default() {
        let mut store = LayoutStore::new(LayoutDocument::new(layout_of(PanelKind::Viewer)));
        let live = layout_of(PanelKind::Timeline);
        store.capture(&live, vec![named("Mine")]);
        assert_eq!(store.app_layout(), &live);
        assert_eq!(store.custom_presets().len(), 1);
    }

    /// The completion criterion: alternating between a project that embeds a
    /// layout and one that does not must leave the application default exactly
    /// as it was.
    #[test]
    fn alternating_embedded_and_plain_projects_never_dirty_the_app_default() {
        let app_default = layout_of(PanelKind::Viewer);
        let mut store = LayoutStore::new(LayoutDocument::new(app_default.clone()));
        let embedded = layout_of(PanelKind::NodeGraph);

        for round in 0..3 {
            // Open the project that ships a layout: the session runs on it.
            let session = store.layout_for_project(Some(&embedded));
            assert_eq!(session, embedded, "round {round}");
            assert!(store.session_layout_active());
            // The user rearranges inside that session; nothing is written back.
            store.capture(&layout_of(PanelKind::Outliner), Vec::new());
            assert_eq!(store.app_layout(), &app_default, "round {round}");

            // Open a project without one: back to the application default.
            let session = store.layout_for_project(None);
            assert_eq!(session, app_default, "round {round}");
            assert!(!store.session_layout_active());
            // …and from here the session is the user's again.
            store.capture(&app_default, Vec::new());
            assert_eq!(store.app_layout(), &app_default, "round {round}");
        }
    }

    /// Named layouts are the user's regardless of which project is open, so
    /// they are captured even while an embedded layout owns the session.
    #[test]
    fn named_layouts_are_captured_during_an_embedded_session() {
        let app_default = layout_of(PanelKind::Viewer);
        let mut store = LayoutStore::new(LayoutDocument::new(app_default.clone()));
        store.layout_for_project(Some(&layout_of(PanelKind::NodeGraph)));

        store.capture(&layout_of(PanelKind::Outliner), vec![named("Grading")]);
        assert_eq!(store.custom_presets(), &[named("Grading")]);
        assert_eq!(store.app_layout(), &app_default);
    }

    #[test]
    fn the_embed_preference_survives_a_capture() {
        let mut store = LayoutStore::new(LayoutDocument::new(layout_of(PanelKind::Viewer)));
        assert!(!store.embed_in_projects());
        store.set_embed_in_projects(true);
        store.capture(&layout_of(PanelKind::Timeline), Vec::new());
        assert!(store.embed_in_projects());
        assert!(store.document().to_toml().unwrap().contains("embed"));
    }
}
