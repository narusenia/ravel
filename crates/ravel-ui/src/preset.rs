// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Workspace layouts and presets.
//!
//! A workspace layout is a split/area tree of panel instances
//! ([`LayoutNode`], see [`crate::layout`]). Ravel ships four built-in presets
//! (Edit / Node / Color / Motion); users can save additional named presets.
//! Layouts serialize to and from TOML and JSON so they can live in
//! `assets/workspaces/` or in a project file.

use crate::layout::{LayoutNode, Orientation, PanelInstance, PanelInstanceId};
use crate::panel::PanelKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The pre-v2 layout tree: a binary split tree whose leaves each host a
/// single panel.
///
/// Retained so old preset definitions can be mapped onto layout model v2:
/// [`LegacyLayoutNode::into_layout`] turns every leaf into a one-tab area,
/// assigning instance ids sequentially in traversal order.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LegacyLayoutNode {
    /// A single panel occupying its area.
    Leaf {
        /// The panel hosted by this leaf.
        panel: PanelKind,
    },
    /// A split between two child subtrees.
    Split {
        /// Whether the split is horizontal or vertical.
        orientation: Orientation,
        /// Fraction `(0.0, 1.0)` of the area given to `first`.
        ratio: f32,
        /// The leading child (left or top).
        first: Box<LegacyLayoutNode>,
        /// The trailing child (right or bottom).
        second: Box<LegacyLayoutNode>,
    },
}

impl LegacyLayoutNode {
    /// Convenience constructor for a leaf.
    fn leaf(panel: PanelKind) -> Self {
        LegacyLayoutNode::Leaf { panel }
    }

    /// Convenience constructor for a split.
    fn split(
        orientation: Orientation,
        ratio: f32,
        first: LegacyLayoutNode,
        second: LegacyLayoutNode,
    ) -> Self {
        LegacyLayoutNode::Split {
            orientation,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// Maps this tree onto layout model v2: every leaf becomes a one-tab
    /// area. Instance ids are assigned sequentially from 0 in left-to-right,
    /// top-to-bottom traversal order.
    pub fn into_layout(self) -> LayoutNode {
        let mut next = 0;
        self.into_layout_with(&mut next)
    }

    fn into_layout_with(self, next: &mut u64) -> LayoutNode {
        match self {
            LegacyLayoutNode::Leaf { panel } => {
                let id = PanelInstanceId(*next);
                *next += 1;
                LayoutNode::area(vec![PanelInstance::new(id, panel)])
            }
            LegacyLayoutNode::Split {
                orientation,
                ratio,
                first,
                second,
            } => LayoutNode::split(
                orientation,
                ratio,
                first.into_layout_with(next),
                second.into_layout_with(next),
            ),
        }
    }
}

/// A named workspace layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspacePreset {
    /// i18n label key (built-in) or user-provided display name (custom).
    pub name: String,
    /// The root of the layout tree.
    pub layout: LayoutNode,
}

impl WorkspacePreset {
    /// Returns the panels laid out by this preset.
    pub fn panels(&self) -> Vec<PanelKind> {
        self.layout.panels()
    }

    /// Serializes the preset to a TOML document.
    pub fn to_toml(&self) -> Result<String, PresetError> {
        toml::to_string_pretty(self).map_err(|e| PresetError::Serialize(e.to_string()))
    }

    /// Parses a preset from a TOML document.
    pub fn from_toml(input: &str) -> Result<Self, PresetError> {
        let preset: WorkspacePreset =
            toml::from_str(input).map_err(|e| PresetError::Parse(e.to_string()))?;
        preset.validated()
    }

    /// Serializes the preset to a JSON document.
    pub fn to_json(&self) -> Result<String, PresetError> {
        serde_json::to_string_pretty(self).map_err(|e| PresetError::Serialize(e.to_string()))
    }

    /// Parses a preset from a JSON document.
    pub fn from_json(input: &str) -> Result<Self, PresetError> {
        let preset: WorkspacePreset =
            serde_json::from_str(input).map_err(|e| PresetError::Parse(e.to_string()))?;
        preset.validated()
    }

    fn validated(self) -> Result<Self, PresetError> {
        if self.layout.is_valid() {
            Ok(self)
        } else {
            Err(PresetError::InvalidLayout(self.name))
        }
    }
}

/// Errors produced while (de)serializing or managing presets.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PresetError {
    /// Failed to parse a preset definition.
    #[error("failed to parse workspace preset: {0}")]
    Parse(String),
    /// Failed to serialize a preset definition.
    #[error("failed to serialize workspace preset: {0}")]
    Serialize(String),
    /// A layout contained an out-of-range split ratio.
    #[error("workspace preset '{0}' has an invalid layout (split ratio out of range)")]
    InvalidLayout(String),
    /// No preset is registered under the given name.
    #[error("unknown workspace preset: {0}")]
    Unknown(String),
}

/// Identifies the four built-in workspace presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinPreset {
    /// Timeline-centric editing workspace.
    Edit,
    /// Node-graph-centric procedural workspace.
    Node,
    /// Color grading workspace with scopes.
    Color,
    /// Motion graphics workspace.
    Motion,
}

impl BuiltinPreset {
    /// All built-in presets in display order.
    pub const ALL: [BuiltinPreset; 4] = [
        BuiltinPreset::Edit,
        BuiltinPreset::Node,
        BuiltinPreset::Color,
        BuiltinPreset::Motion,
    ];

    /// The i18n label key used as the preset name.
    pub fn label_key(self) -> &'static str {
        match self {
            BuiltinPreset::Edit => "workspace.preset.edit",
            BuiltinPreset::Node => "workspace.preset.node",
            BuiltinPreset::Color => "workspace.preset.color",
            BuiltinPreset::Motion => "workspace.preset.motion",
        }
    }

    /// Builds the concrete layout for this preset.
    ///
    /// Layouts follow `docs/specifications/ui-spec.md`. They are written in
    /// the pre-v2 leaf/split form and mapped onto layout model v2 (one tab
    /// per area) by [`LegacyLayoutNode::into_layout`].
    pub fn preset(self) -> WorkspacePreset {
        use Orientation::{Horizontal, Vertical};
        use PanelKind::*;

        let layout = match self {
            // Edit: [Outliner | Viewer          | Properties]
            //       [MediaBin |                 |           ]
            //       [NodeGraph| Timeline        |           ]
            // Properties 18%, left column 20%, upper row 65%,
            // Outliner 55% of the left column. The media bin shares that
            // column because both answer "what is in this project".
            BuiltinPreset::Edit => LegacyLayoutNode::split(
                Horizontal,
                0.82,
                LegacyLayoutNode::split(
                    Vertical,
                    0.65,
                    LegacyLayoutNode::split(
                        Horizontal,
                        0.2,
                        LegacyLayoutNode::split(
                            Vertical,
                            0.55,
                            LegacyLayoutNode::leaf(Outliner),
                            LegacyLayoutNode::leaf(MediaBin),
                        ),
                        LegacyLayoutNode::leaf(Viewer),
                    ),
                    LegacyLayoutNode::split(
                        Horizontal,
                        0.35,
                        LegacyLayoutNode::leaf(NodeGraph),
                        LegacyLayoutNode::leaf(Timeline),
                    ),
                ),
                LegacyLayoutNode::leaf(Properties),
            ),
            // Node: [Outliner | Viewer          | Properties]
            //       [    Node Graph             |           ]
            //       [    Dopesheet              |           ]
            // Properties 18%, Outliner 20%
            BuiltinPreset::Node => LegacyLayoutNode::split(
                Horizontal,
                0.82,
                LegacyLayoutNode::split(
                    Vertical,
                    0.35,
                    LegacyLayoutNode::split(
                        Horizontal,
                        0.2,
                        LegacyLayoutNode::leaf(Outliner),
                        LegacyLayoutNode::leaf(Viewer),
                    ),
                    LegacyLayoutNode::split(
                        Vertical,
                        0.82,
                        LegacyLayoutNode::leaf(NodeGraph),
                        LegacyLayoutNode::leaf(Dopesheet),
                    ),
                ),
                LegacyLayoutNode::leaf(Properties),
            ),
            // Color: [Viewer    | Waveform   ]
            //        [          | Vectorscope]
            //        [NodeGraph | Histogram  ]
            //        [          | Parade     ]
            //        [Dopesheet              ]
            // Scopes 30%, Dopesheet 12%
            BuiltinPreset::Color => LegacyLayoutNode::split(
                Vertical,
                0.88,
                LegacyLayoutNode::split(
                    Horizontal,
                    0.7,
                    LegacyLayoutNode::split(
                        Vertical,
                        0.5,
                        LegacyLayoutNode::leaf(Viewer),
                        LegacyLayoutNode::leaf(NodeGraph),
                    ),
                    LegacyLayoutNode::split(
                        Vertical,
                        0.5,
                        LegacyLayoutNode::split(
                            Vertical,
                            0.5,
                            LegacyLayoutNode::leaf(Waveform),
                            LegacyLayoutNode::leaf(Vectorscope),
                        ),
                        LegacyLayoutNode::split(
                            Vertical,
                            0.5,
                            LegacyLayoutNode::leaf(Histogram),
                            LegacyLayoutNode::leaf(Parade),
                        ),
                    ),
                ),
                LegacyLayoutNode::leaf(Dopesheet),
            ),
            // Motion: [Outliner | Viewer     | TextEditor]
            //         [    Node Graph        | Properties]
            //         [    Dopesheet                     ]
            // Right column 30%, Outliner 20%, Dopesheet 12%
            BuiltinPreset::Motion => LegacyLayoutNode::split(
                Vertical,
                0.88,
                LegacyLayoutNode::split(
                    Horizontal,
                    0.7,
                    LegacyLayoutNode::split(
                        Vertical,
                        0.45,
                        LegacyLayoutNode::split(
                            Horizontal,
                            0.2,
                            LegacyLayoutNode::leaf(Outliner),
                            LegacyLayoutNode::leaf(Viewer),
                        ),
                        LegacyLayoutNode::leaf(NodeGraph),
                    ),
                    LegacyLayoutNode::split(
                        Vertical,
                        0.5,
                        LegacyLayoutNode::leaf(TextEditor),
                        LegacyLayoutNode::leaf(Properties),
                    ),
                ),
                LegacyLayoutNode::leaf(Dopesheet),
            ),
        };

        WorkspacePreset {
            name: self.label_key().to_owned(),
            layout: layout.into_layout(),
        }
    }
}

/// Holds the built-in and user-defined presets and tracks the active layout.
#[derive(Debug, Clone)]
pub struct PresetLibrary {
    custom: BTreeMap<String, WorkspacePreset>,
    active: WorkspacePreset,
    active_builtin: Option<BuiltinPreset>,
    visibility: crate::panel::PanelVisibility,
}

impl PresetLibrary {
    /// Creates a library with the given built-in preset active.
    pub fn new(initial: BuiltinPreset) -> Self {
        let active = initial.preset();
        let visibility = crate::panel::PanelVisibility::with_visible(active.panels());
        Self {
            custom: BTreeMap::new(),
            active,
            active_builtin: Some(initial),
            visibility,
        }
    }

    /// Returns the currently active preset.
    pub fn active(&self) -> &WorkspacePreset {
        &self.active
    }

    /// Returns the active built-in preset, if the active layout is built-in.
    pub fn active_builtin(&self) -> Option<BuiltinPreset> {
        self.active_builtin
    }

    /// Read-only access to the current panel visibility state.
    pub fn visibility(&self) -> &crate::panel::PanelVisibility {
        &self.visibility
    }

    /// Mutable access to panel visibility (driven by the View menu).
    pub fn visibility_mut(&mut self) -> &mut crate::panel::PanelVisibility {
        &mut self.visibility
    }

    /// Switches to a built-in preset, resetting panel visibility to match.
    pub fn switch_builtin(&mut self, preset: BuiltinPreset) {
        self.active = preset.preset();
        self.active_builtin = Some(preset);
        self.visibility = crate::panel::PanelVisibility::with_visible(self.active.panels());
    }

    /// Saves a custom preset under its name (overwriting any previous one).
    pub fn save_custom(&mut self, preset: WorkspacePreset) {
        self.custom.insert(preset.name.clone(), preset);
    }

    /// Switches to a previously saved custom preset.
    pub fn switch_custom(&mut self, name: &str) -> Result<(), PresetError> {
        let preset = self
            .custom
            .get(name)
            .cloned()
            .ok_or_else(|| PresetError::Unknown(name.to_owned()))?;
        self.visibility = crate::panel::PanelVisibility::with_visible(preset.panels());
        self.active = preset;
        self.active_builtin = None;
        Ok(())
    }

    /// Iterates over the names of saved custom presets.
    pub fn custom_names(&self) -> impl Iterator<Item = &str> {
        self.custom.keys().map(String::as_str)
    }
}

impl Default for PresetLibrary {
    fn default() -> Self {
        Self::new(BuiltinPreset::Edit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_presets_contain_expected_panels() {
        let edit = BuiltinPreset::Edit.preset();
        let panels = edit.panels();
        assert!(panels.contains(&PanelKind::Outliner));
        assert!(panels.contains(&PanelKind::NodeGraph));
        assert!(panels.contains(&PanelKind::Timeline));
        assert!(panels.contains(&PanelKind::Viewer));
        assert!(panels.contains(&PanelKind::Properties));

        let node = BuiltinPreset::Node.preset();
        let panels = node.panels();
        assert!(panels.contains(&PanelKind::Outliner));
        assert!(panels.contains(&PanelKind::NodeGraph));
        assert!(panels.contains(&PanelKind::Dopesheet));
        assert!(panels.contains(&PanelKind::Properties));

        let color = BuiltinPreset::Color.preset();
        for scope in [
            PanelKind::Waveform,
            PanelKind::Vectorscope,
            PanelKind::Histogram,
            PanelKind::Parade,
        ] {
            assert!(color.panels().contains(&scope), "color missing {scope:?}");
        }
        assert!(color.panels().contains(&PanelKind::NodeGraph));

        let motion = BuiltinPreset::Motion.preset();
        assert!(motion.panels().contains(&PanelKind::TextEditor));
        assert!(motion.panels().contains(&PanelKind::Outliner));
        assert!(motion.panels().contains(&PanelKind::NodeGraph));
    }

    #[test]
    fn all_builtin_layouts_are_valid() {
        for preset in BuiltinPreset::ALL {
            assert!(preset.preset().layout.is_valid(), "{preset:?} invalid");
        }
    }

    #[test]
    fn toml_roundtrip_preserves_layout() {
        let original = BuiltinPreset::Node.preset();
        let toml = original.to_toml().unwrap();
        let parsed = WorkspacePreset::from_toml(&toml).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn json_roundtrip_preserves_layout() {
        let original = BuiltinPreset::Color.preset();
        let json = original.to_json().unwrap();
        let parsed = WorkspacePreset::from_json(&json).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn invalid_ratio_is_rejected_on_parse() {
        let bad = r#"{
            "name": "bad",
            "layout": {
                "type": "split",
                "orientation": "horizontal",
                "ratio": 1.5,
                "first": { "type": "area", "tabs": [{ "id": 0, "kind": "viewer" }], "active": 0 },
                "second": { "type": "area", "tabs": [{ "id": 1, "kind": "timeline" }], "active": 0 }
            }
        }"#;
        let err = WorkspacePreset::from_json(bad).unwrap_err();
        assert!(matches!(err, PresetError::InvalidLayout(_)));
    }

    #[test]
    fn legacy_leaf_split_tree_maps_to_one_tab_areas() {
        // The pre-v2 preset format: binary splits with single-panel leaves.
        let legacy = r#"{
            "type": "split",
            "orientation": "horizontal",
            "ratio": 0.8,
            "first": {
                "type": "split",
                "orientation": "vertical",
                "ratio": 0.6,
                "first": { "type": "leaf", "panel": "outliner" },
                "second": { "type": "leaf", "panel": "viewer" }
            },
            "second": { "type": "leaf", "panel": "properties" }
        }"#;
        let legacy: LegacyLayoutNode = serde_json::from_str(legacy).unwrap();
        let layout = legacy.into_layout();

        let expected = LayoutNode::split(
            Orientation::Horizontal,
            0.8,
            LayoutNode::split(
                Orientation::Vertical,
                0.6,
                LayoutNode::area(vec![PanelInstance::new(
                    PanelInstanceId(0),
                    PanelKind::Outliner,
                )]),
                LayoutNode::area(vec![PanelInstance::new(
                    PanelInstanceId(1),
                    PanelKind::Viewer,
                )]),
            ),
            LayoutNode::area(vec![PanelInstance::new(
                PanelInstanceId(2),
                PanelKind::Properties,
            )]),
        );
        assert_eq!(layout, expected);
        assert!(layout.is_valid());
    }

    #[test]
    fn builtin_presets_have_unique_sequential_instance_ids() {
        for preset in BuiltinPreset::ALL {
            let layout = preset.preset().layout;
            let ids: Vec<_> = layout.instances().iter().map(|t| t.id.0).collect();
            let expected: Vec<_> = (0..ids.len() as u64).collect();
            assert_eq!(ids, expected, "{preset:?} instance ids not sequential");
        }
    }

    #[test]
    fn switching_builtin_resets_visibility() {
        let mut lib = PresetLibrary::new(BuiltinPreset::Edit);
        assert_eq!(lib.active_builtin(), Some(BuiltinPreset::Edit));
        assert!(lib.visibility().is_visible(PanelKind::Timeline));

        lib.switch_builtin(BuiltinPreset::Color);
        assert_eq!(lib.active_builtin(), Some(BuiltinPreset::Color));
        assert!(lib.visibility().is_visible(PanelKind::Waveform));
        // Timeline is not part of the Color preset.
        assert!(!lib.visibility().is_visible(PanelKind::Timeline));
    }

    #[test]
    fn custom_preset_save_and_switch() {
        let mut lib = PresetLibrary::new(BuiltinPreset::Edit);
        let custom = WorkspacePreset {
            name: "My Layout".to_owned(),
            layout: LayoutNode::split(
                Orientation::Horizontal,
                0.5,
                LayoutNode::area(vec![PanelInstance::new(
                    PanelInstanceId(0),
                    PanelKind::NodeGraph,
                )]),
                LayoutNode::area(vec![PanelInstance::new(
                    PanelInstanceId(1),
                    PanelKind::Viewer,
                )]),
            ),
        };
        lib.save_custom(custom);
        lib.switch_custom("My Layout").unwrap();
        assert_eq!(lib.active_builtin(), None);
        assert!(lib.visibility().is_visible(PanelKind::NodeGraph));
        assert_eq!(lib.custom_names().count(), 1);
    }

    #[test]
    fn switching_unknown_custom_errors() {
        let mut lib = PresetLibrary::new(BuiltinPreset::Edit);
        let err = lib.switch_custom("nope").unwrap_err();
        assert!(matches!(err, PresetError::Unknown(_)));
    }
}

#[cfg(test)]
mod export_assets {
    use super::*;
    use std::fs;

    const PRESET_FILES: [(BuiltinPreset, &str); 4] = [
        (BuiltinPreset::Edit, "edit.toml"),
        (BuiltinPreset::Node, "node.toml"),
        (BuiltinPreset::Color, "color.toml"),
        (BuiltinPreset::Motion, "motion.toml"),
    ];

    /// Helper (run manually) that writes the built-in presets to
    /// `assets/workspaces/`. Ignored in normal runs.
    #[test]
    #[ignore = "asset generator; run with --ignored to regenerate"]
    fn write_builtin_preset_assets() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/workspaces");
        fs::create_dir_all(dir).unwrap();
        for (preset, file) in PRESET_FILES {
            let toml = preset.preset().to_toml().unwrap();
            fs::write(format!("{dir}/{file}"), toml).unwrap();
        }
    }

    #[test]
    fn asset_files_match_builtin_presets() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/workspaces");
        for (preset, file) in PRESET_FILES {
            let path = format!("{dir}/{file}");
            let contents =
                fs::read_to_string(&path).unwrap_or_else(|e| panic!("{file} not readable: {e}"));
            let from_file = WorkspacePreset::from_toml(&contents)
                .unwrap_or_else(|e| panic!("{file} parse failed: {e}"));
            let from_code = preset.preset();
            assert_eq!(
                from_file, from_code,
                "asset {file} has drifted from BuiltinPreset::{preset:?} — \
                 regenerate with: cargo test -p ravel-ui -- --ignored write_builtin"
            );
        }
    }
}
