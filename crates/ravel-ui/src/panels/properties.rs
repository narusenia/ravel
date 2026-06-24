// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Properties inspector — shell only.
//!
//! This task delivers the panel frame and empty/placeholder state. Rendering
//! the parameters of a selected node or clip is a follow-up task; for now the
//! panel exposes its selection state and the i18n key for the placeholder text
//! shown when nothing is selected.

use crate::panel::PanelKind;

/// What the Properties panel is currently inspecting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Selection {
    /// Nothing is selected; the panel shows its placeholder.
    #[default]
    Empty,
    /// A node is selected, identified by its graph node id.
    ///
    /// Parameter editing is implemented in a follow-up task; the id is carried
    /// so the shell can already react to selection changes.
    Node(u64),
    /// A timeline clip is selected, identified by its clip id.
    Clip(u64),
}

/// State for the Properties inspector shell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PropertiesPanel {
    selection: Selection,
    /// Whether the panel is collapsed (header only).
    collapsed: bool,
}

impl PropertiesPanel {
    /// The panel kind this shell renders.
    pub const KIND: PanelKind = PanelKind::Properties;

    /// Creates an empty (nothing-selected) panel.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current selection.
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Updates the current selection.
    pub fn set_selection(&mut self, selection: Selection) {
        self.selection = selection;
    }

    /// Returns `true` when nothing is selected (placeholder should show).
    pub fn is_empty(&self) -> bool {
        self.selection == Selection::Empty
    }

    /// i18n key for the placeholder shown when nothing is selected.
    pub fn placeholder_key(&self) -> &'static str {
        "panel.properties.empty"
    }

    /// i18n key for the panel header title.
    pub fn title_key(&self) -> &'static str {
        PanelKind::Properties.label_key()
    }

    /// Whether the panel is collapsed to its header.
    pub fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    /// Toggles the collapsed state and returns the new value.
    pub fn toggle_collapsed(&mut self) -> bool {
        self.collapsed = !self.collapsed;
        self.collapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_empty_with_placeholder() {
        let panel = PropertiesPanel::new();
        assert!(panel.is_empty());
        assert_eq!(panel.placeholder_key(), "panel.properties.empty");
        assert_eq!(panel.title_key(), "panel.properties");
    }

    #[test]
    fn selection_clears_empty_state() {
        let mut panel = PropertiesPanel::new();
        panel.set_selection(Selection::Node(7));
        assert!(!panel.is_empty());
        assert_eq!(panel.selection(), &Selection::Node(7));
        panel.set_selection(Selection::Empty);
        assert!(panel.is_empty());
    }

    #[test]
    fn collapse_toggles() {
        let mut panel = PropertiesPanel::new();
        assert!(!panel.is_collapsed());
        assert!(panel.toggle_collapsed());
        assert!(panel.is_collapsed());
    }
}
