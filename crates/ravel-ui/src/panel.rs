// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Panel taxonomy and per-panel visibility state.
//!
//! The shell is built out of a fixed set of [`PanelKind`]s. The actual GPUI
//! views are attached on the host side; this module owns the kind enumeration,
//! the human-facing i18n label keys, and the [`PanelVisibility`] toggle map
//! driven by the View menu.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Every panel the shell knows how to host.
///
/// Variants are serialized in `snake_case` so they can be referenced from
/// workspace preset definition files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelKind {
    /// Generator-based object list + subgraph tree.
    Outliner,
    /// Procedural node graph editor.
    NodeGraph,
    /// Timeline / sequence editor.
    Timeline,
    /// Output preview.
    Viewer,
    /// Keyframe dopesheet (tab-shared with CurveEditor).
    Dopesheet,
    /// Selected-node / clip parameter inspector.
    Properties,
    /// Project media management (thumbnail browser).
    MediaBin,
    /// Animation curve editor (tab-shared with Dopesheet).
    CurveEditor,
    /// Waveform monitor scope.
    Waveform,
    /// Vectorscope.
    Vectorscope,
    /// Histogram scope.
    Histogram,
    /// RGB parade scope.
    Parade,
    /// Typography editor.
    TextEditor,
    /// WGSL custom shader editor.
    ShaderEditor,
    /// Lua scripting console.
    LuaConsole,
    /// Render job queue.
    RenderQueue,
}

impl PanelKind {
    /// All panel kinds in declaration order.
    pub const ALL: [PanelKind; 16] = [
        PanelKind::Outliner,
        PanelKind::NodeGraph,
        PanelKind::Timeline,
        PanelKind::Viewer,
        PanelKind::Dopesheet,
        PanelKind::Properties,
        PanelKind::MediaBin,
        PanelKind::CurveEditor,
        PanelKind::Waveform,
        PanelKind::Vectorscope,
        PanelKind::Histogram,
        PanelKind::Parade,
        PanelKind::TextEditor,
        PanelKind::ShaderEditor,
        PanelKind::LuaConsole,
        PanelKind::RenderQueue,
    ];

    /// Returns a stable snake_case identifier for serialization and dock identity.
    pub fn panel_id(self) -> &'static str {
        match self {
            PanelKind::Outliner => "outliner",
            PanelKind::NodeGraph => "node_graph",
            PanelKind::Timeline => "timeline",
            PanelKind::Viewer => "viewer",
            PanelKind::Dopesheet => "dopesheet",
            PanelKind::Properties => "properties",
            PanelKind::MediaBin => "media_bin",
            PanelKind::CurveEditor => "curve_editor",
            PanelKind::Waveform => "waveform",
            PanelKind::Vectorscope => "vectorscope",
            PanelKind::Histogram => "histogram",
            PanelKind::Parade => "parade",
            PanelKind::TextEditor => "text_editor",
            PanelKind::ShaderEditor => "shader_editor",
            PanelKind::LuaConsole => "lua_console",
            PanelKind::RenderQueue => "render_queue",
        }
    }

    /// Returns the i18n label key for the panel title.
    pub fn label_key(self) -> &'static str {
        match self {
            PanelKind::Outliner => "panel.outliner",
            PanelKind::NodeGraph => "panel.node_graph",
            PanelKind::Timeline => "panel.timeline",
            PanelKind::Viewer => "panel.viewer",
            PanelKind::Dopesheet => "panel.dopesheet",
            PanelKind::Properties => "panel.properties",
            PanelKind::MediaBin => "panel.media_bin",
            PanelKind::CurveEditor => "panel.curve_editor",
            PanelKind::Waveform => "panel.waveform",
            PanelKind::Vectorscope => "panel.vectorscope",
            PanelKind::Histogram => "panel.histogram",
            PanelKind::Parade => "panel.parade",
            PanelKind::TextEditor => "panel.text_editor",
            PanelKind::ShaderEditor => "panel.shader_editor",
            PanelKind::LuaConsole => "panel.lua_console",
            PanelKind::RenderQueue => "panel.render_queue",
        }
    }
}

/// Tracks which panels are currently shown, independently of the active
/// workspace layout.
///
/// A preset declares the panels it lays out (all visible on switch); the View
/// menu then toggles individual panels on and off. Panels not present in the
/// map are treated as hidden.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PanelVisibility {
    shown: BTreeMap<PanelKind, bool>,
}

impl PanelVisibility {
    /// Creates an empty visibility map (everything hidden).
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a map with the given panels visible and all others hidden.
    pub fn with_visible<I: IntoIterator<Item = PanelKind>>(panels: I) -> Self {
        let mut v = Self::new();
        for p in panels {
            v.set(p, true);
        }
        v
    }

    /// Returns whether `panel` is currently visible.
    pub fn is_visible(&self, panel: PanelKind) -> bool {
        self.shown.get(&panel).copied().unwrap_or(false)
    }

    /// Sets the visibility of `panel`.
    pub fn set(&mut self, panel: PanelKind, visible: bool) {
        self.shown.insert(panel, visible);
    }

    /// Flips the visibility of `panel` and returns the new state.
    pub fn toggle(&mut self, panel: PanelKind) -> bool {
        let next = !self.is_visible(panel);
        self.set(panel, next);
        next
    }

    /// Iterates over the currently visible panels in a stable order.
    pub fn visible_panels(&self) -> impl Iterator<Item = PanelKind> + '_ {
        self.shown.iter().filter_map(|(k, v)| v.then_some(*k))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_kinds_have_unique_label_keys() {
        let mut seen = std::collections::HashSet::new();
        for kind in PanelKind::ALL {
            assert!(seen.insert(kind.label_key()), "dup label for {kind:?}");
        }
        assert_eq!(seen.len(), PanelKind::ALL.len());
    }

    #[test]
    fn unset_panel_is_hidden() {
        let v = PanelVisibility::new();
        assert!(!v.is_visible(PanelKind::Viewer));
    }

    #[test]
    fn toggle_flips_state() {
        let mut v = PanelVisibility::new();
        assert!(v.toggle(PanelKind::Timeline));
        assert!(v.is_visible(PanelKind::Timeline));
        assert!(!v.toggle(PanelKind::Timeline));
        assert!(!v.is_visible(PanelKind::Timeline));
    }

    #[test]
    fn with_visible_marks_only_given_panels() {
        let v = PanelVisibility::with_visible([PanelKind::Viewer, PanelKind::NodeGraph]);
        assert!(v.is_visible(PanelKind::Viewer));
        assert!(v.is_visible(PanelKind::NodeGraph));
        assert!(!v.is_visible(PanelKind::Timeline));
        let shown: Vec<_> = v.visible_panels().collect();
        assert_eq!(shown.len(), 2);
    }

    #[test]
    fn panel_kind_serializes_snake_case() {
        let json = serde_json::to_string(&PanelKind::NodeGraph).unwrap();
        assert_eq!(json, "\"node_graph\"");
        let back: PanelKind = serde_json::from_str("\"curve_editor\"").unwrap();
        assert_eq!(back, PanelKind::CurveEditor);
    }
}
