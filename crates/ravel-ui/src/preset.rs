// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Workspace layouts and presets.
//!
//! A workspace is described by a binary split tree of panels ([`LayoutNode`]).
//! Ravel ships four built-in presets (Edit / Node / Color / Motion); users can
//! save additional named presets. Layouts serialize to and from TOML and JSON
//! so they can live in `assets/workspaces/` or in a project file.

use crate::panel::PanelKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Split orientation of a layout node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    /// Children are placed side by side (left / right).
    Horizontal,
    /// Children are stacked (top / bottom).
    Vertical,
}

/// A node in the workspace layout tree.
///
/// Leaves host a single panel; splits divide the available area between two
/// child subtrees by `ratio` (the fraction given to the first child).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutNode {
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
        first: Box<LayoutNode>,
        /// The trailing child (right or bottom).
        second: Box<LayoutNode>,
    },
}

impl LayoutNode {
    /// Convenience constructor for a leaf.
    pub fn leaf(panel: PanelKind) -> Self {
        LayoutNode::Leaf { panel }
    }

    /// Convenience constructor for a split.
    pub fn split(
        orientation: Orientation,
        ratio: f32,
        first: LayoutNode,
        second: LayoutNode,
    ) -> Self {
        LayoutNode::Split {
            orientation,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// Collects every panel hosted in this subtree, in left-to-right,
    /// top-to-bottom traversal order.
    pub fn panels(&self) -> Vec<PanelKind> {
        let mut out = Vec::new();
        self.collect_panels(&mut out);
        out
    }

    fn collect_panels(&self, out: &mut Vec<PanelKind>) {
        match self {
            LayoutNode::Leaf { panel } => out.push(*panel),
            LayoutNode::Split { first, second, .. } => {
                first.collect_panels(out);
                second.collect_panels(out);
            }
        }
    }

    /// Returns `true` if every split ratio is strictly within `(0.0, 1.0)` and
    /// finite. Invalid ratios would collapse a pane to zero size.
    pub fn is_valid(&self) -> bool {
        match self {
            LayoutNode::Leaf { .. } => true,
            LayoutNode::Split {
                ratio,
                first,
                second,
                ..
            } => {
                ratio.is_finite()
                    && *ratio > 0.0
                    && *ratio < 1.0
                    && first.is_valid()
                    && second.is_valid()
            }
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
    /// Layouts follow `docs/specifications/ui-spec.md`.
    pub fn preset(self) -> WorkspacePreset {
        use Orientation::{Horizontal, Vertical};
        use PanelKind::*;

        let layout = match self {
            // Edit: [Outliner+MediaBin(tab) | Viewer | Properties]
            //       [NodeGraph             | Timeline            ]
            BuiltinPreset::Edit => LayoutNode::split(
                Horizontal,
                0.8,
                LayoutNode::split(
                    Vertical,
                    0.5,
                    LayoutNode::split(
                        Horizontal,
                        0.25,
                        LayoutNode::leaf(Outliner), // tab with MediaBin
                        LayoutNode::leaf(Viewer),
                    ),
                    LayoutNode::split(
                        Horizontal,
                        0.35,
                        LayoutNode::leaf(NodeGraph),
                        LayoutNode::leaf(Timeline),
                    ),
                ),
                LayoutNode::leaf(Properties),
            ),
            // Node: [Outliner | Viewer    | Properties]
            //       [    Node Graph                   ]
            //       [    Dopesheet / Curve Editor      ]
            BuiltinPreset::Node => LayoutNode::split(
                Horizontal,
                0.8,
                LayoutNode::split(
                    Vertical,
                    0.35,
                    LayoutNode::split(
                        Horizontal,
                        0.2,
                        LayoutNode::leaf(Outliner),
                        LayoutNode::leaf(Viewer),
                    ),
                    LayoutNode::split(
                        Vertical,
                        0.75,
                        LayoutNode::leaf(NodeGraph),
                        LayoutNode::leaf(Dopesheet), // tab with CurveEditor
                    ),
                ),
                LayoutNode::leaf(Properties),
            ),
            // Color: [Viewer    | Waveform   ]
            //        [          | Vectorscope]
            //        [NodeGraph | Histogram  ]
            //        [          | Parade     ]
            //        [Dopesheet / CurveEditor]
            BuiltinPreset::Color => LayoutNode::split(
                Vertical,
                0.85,
                LayoutNode::split(
                    Horizontal,
                    0.65,
                    LayoutNode::split(
                        Vertical,
                        0.5,
                        LayoutNode::leaf(Viewer),
                        LayoutNode::leaf(NodeGraph),
                    ),
                    LayoutNode::split(
                        Vertical,
                        0.5,
                        LayoutNode::split(
                            Vertical,
                            0.5,
                            LayoutNode::leaf(Waveform),
                            LayoutNode::leaf(Vectorscope),
                        ),
                        LayoutNode::split(
                            Vertical,
                            0.5,
                            LayoutNode::leaf(Histogram),
                            LayoutNode::leaf(Parade),
                        ),
                    ),
                ),
                LayoutNode::leaf(Dopesheet), // tab with CurveEditor
            ),
            // Motion: [Outliner | Viewer    | TextEditor]
            //         [    Node Graph       | Properties]
            //         [    Dopesheet / Curve Editor      ]
            BuiltinPreset::Motion => LayoutNode::split(
                Vertical,
                0.85,
                LayoutNode::split(
                    Horizontal,
                    0.65,
                    LayoutNode::split(
                        Vertical,
                        0.4,
                        LayoutNode::split(
                            Horizontal,
                            0.2,
                            LayoutNode::leaf(Outliner),
                            LayoutNode::leaf(Viewer),
                        ),
                        LayoutNode::leaf(NodeGraph),
                    ),
                    LayoutNode::split(
                        Vertical,
                        0.5,
                        LayoutNode::leaf(TextEditor),
                        LayoutNode::leaf(Properties),
                    ),
                ),
                LayoutNode::leaf(Dopesheet), // tab with CurveEditor
            ),
        };

        WorkspacePreset {
            name: self.label_key().to_owned(),
            layout,
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
                "first": { "type": "leaf", "panel": "viewer" },
                "second": { "type": "leaf", "panel": "timeline" }
            }
        }"#;
        let err = WorkspacePreset::from_json(bad).unwrap_err();
        assert!(matches!(err, PresetError::InvalidLayout(_)));
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
                LayoutNode::leaf(PanelKind::NodeGraph),
                LayoutNode::leaf(PanelKind::Viewer),
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

    /// Helper (run manually) that writes the built-in presets to
    /// `assets/workspaces/`. Ignored in normal runs.
    #[test]
    #[ignore = "asset generator; run with --ignored to regenerate"]
    fn write_builtin_preset_assets() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/workspaces");
        fs::create_dir_all(dir).unwrap();
        for (preset, file) in [
            (BuiltinPreset::Edit, "edit.toml"),
            (BuiltinPreset::Node, "node.toml"),
            (BuiltinPreset::Color, "color.toml"),
            (BuiltinPreset::Motion, "motion.toml"),
        ] {
            let toml = preset.preset().to_toml().unwrap();
            fs::write(format!("{dir}/{file}"), toml).unwrap();
        }
    }
}
