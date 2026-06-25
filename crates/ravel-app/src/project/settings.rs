// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Project settings (`settings.toml`) and the override hierarchy.
//!
//! Settings are resolved by layering four partial sources, lowest priority
//! first:
//!
//! ```text
//! default  →  global  →  project  →  user
//! ```
//!
//! Each layer is a [`SettingsLayer`] in which every field is optional; a layer
//! only states the values it wishes to override. [`SettingsLayer::merge`]
//! folds a higher-priority layer onto a lower one field-by-field, and
//! [`ResolvedSettings::resolve`] collapses the merged layer into concrete
//! values using built-in defaults for anything still unset.

use serde::{Deserialize, Serialize};

// ===========================================================================
// Partial (overridable) settings
// ===========================================================================

/// Colour-management settings (all fields optional for layering).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocio_config: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_space: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_space: Option<String>,
}

impl ColorLayer {
    fn merge(&self, over: &ColorLayer) -> ColorLayer {
        ColorLayer {
            ocio_config: over
                .ocio_config
                .clone()
                .or_else(|| self.ocio_config.clone()),
            working_space: over
                .working_space
                .clone()
                .or_else(|| self.working_space.clone()),
            display_space: over
                .display_space
                .clone()
                .or_else(|| self.display_space.clone()),
        }
    }
}

/// Proxy playback mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    Off,
    Auto,
    Always,
}

/// Playback settings (all fields optional for layering).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaybackLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_mode: Option<ProxyMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_resolution: Option<f32>,
}

impl PlaybackLayer {
    fn merge(&self, over: &PlaybackLayer) -> PlaybackLayer {
        PlaybackLayer {
            frame_rate: over.frame_rate.clone().or_else(|| self.frame_rate.clone()),
            proxy_mode: over.proxy_mode.or(self.proxy_mode),
            proxy_resolution: over.proxy_resolution.or(self.proxy_resolution),
        }
    }
}

/// Auto-save settings (all fields optional for layering).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutoSaveLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_seconds: Option<u32>,
}

impl AutoSaveLayer {
    fn merge(&self, over: &AutoSaveLayer) -> AutoSaveLayer {
        AutoSaveLayer {
            enabled: over.enabled.or(self.enabled),
            interval_seconds: over.interval_seconds.or(self.interval_seconds),
        }
    }
}

/// A single, partial settings layer as read from one `settings.toml`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(default)]
    pub color: ColorLayer,
    #[serde(default)]
    pub playback: PlaybackLayer,
    #[serde(default)]
    pub auto_save: AutoSaveLayer,
}

impl SettingsLayer {
    /// Fold `over` (higher priority) onto `self`, returning the merged layer.
    pub fn merge(&self, over: &SettingsLayer) -> SettingsLayer {
        SettingsLayer {
            locale: over.locale.clone().or_else(|| self.locale.clone()),
            color: self.color.merge(&over.color),
            playback: self.playback.merge(&over.playback),
            auto_save: self.auto_save.merge(&over.auto_save),
        }
    }

    /// Merge an ordered list of layers, lowest priority first.
    ///
    /// `default → global → project → user` becomes
    /// `merge_all([default, global, project, user])`.
    pub fn merge_all(layers: &[SettingsLayer]) -> SettingsLayer {
        layers
            .iter()
            .fold(SettingsLayer::default(), |acc, layer| acc.merge(layer))
    }

    /// Parse a layer from TOML text.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Serialize this layer to TOML text.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }
}

// ===========================================================================
// Resolved (concrete) settings
// ===========================================================================

/// Fully resolved settings with all defaults applied.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedSettings {
    pub locale: String,
    pub ocio_config: Option<String>,
    pub working_space: String,
    pub display_space: String,
    pub frame_rate: String,
    pub proxy_mode: ProxyMode,
    pub proxy_resolution: f32,
    pub auto_save_enabled: bool,
    pub auto_save_interval_seconds: u32,
}

impl Default for ResolvedSettings {
    fn default() -> Self {
        Self {
            locale: "en".to_string(),
            ocio_config: None,
            working_space: "ACEScg".to_string(),
            display_space: "sRGB".to_string(),
            frame_rate: "30".to_string(),
            proxy_mode: ProxyMode::Auto,
            proxy_resolution: 0.5,
            auto_save_enabled: true,
            auto_save_interval_seconds: 120,
        }
    }
}

impl ResolvedSettings {
    /// Collapse a merged [`SettingsLayer`] into concrete values, falling back
    /// to [`ResolvedSettings::default`] for any field left unset.
    pub fn resolve(merged: &SettingsLayer) -> Self {
        let d = ResolvedSettings::default();
        Self {
            locale: merged.locale.clone().unwrap_or(d.locale),
            ocio_config: merged.color.ocio_config.clone(),
            working_space: merged
                .color
                .working_space
                .clone()
                .unwrap_or(d.working_space),
            display_space: merged
                .color
                .display_space
                .clone()
                .unwrap_or(d.display_space),
            frame_rate: merged.playback.frame_rate.clone().unwrap_or(d.frame_rate),
            proxy_mode: merged.playback.proxy_mode.unwrap_or(d.proxy_mode),
            proxy_resolution: merged
                .playback
                .proxy_resolution
                .unwrap_or(d.proxy_resolution),
            auto_save_enabled: merged.auto_save.enabled.unwrap_or(d.auto_save_enabled),
            auto_save_interval_seconds: merged
                .auto_save
                .interval_seconds
                .unwrap_or(d.auto_save_interval_seconds),
        }
    }

    /// Resolve directly from an ordered list of layers (lowest priority first).
    pub fn from_layers(layers: &[SettingsLayer]) -> Self {
        Self::resolve(&SettingsLayer::merge_all(layers))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_when_no_layers() {
        let resolved = ResolvedSettings::from_layers(&[]);
        assert_eq!(resolved, ResolvedSettings::default());
        assert_eq!(resolved.working_space, "ACEScg");
        assert_eq!(resolved.auto_save_interval_seconds, 120);
    }

    #[test]
    fn higher_priority_layer_overrides_lower() {
        let global = SettingsLayer {
            color: ColorLayer {
                working_space: Some("Rec709".into()),
                display_space: Some("sRGB".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let project = SettingsLayer {
            color: ColorLayer {
                working_space: Some("ACEScg".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        // Order: default → global → project
        let resolved = ResolvedSettings::from_layers(&[global, project]);
        // project wins for working_space
        assert_eq!(resolved.working_space, "ACEScg");
        // global still supplies display_space (project left it unset)
        assert_eq!(resolved.display_space, "sRGB");
    }

    #[test]
    fn user_layer_has_highest_priority() {
        let project = SettingsLayer {
            auto_save: AutoSaveLayer {
                enabled: Some(true),
                interval_seconds: Some(120),
            },
            ..Default::default()
        };
        let user = SettingsLayer {
            auto_save: AutoSaveLayer {
                interval_seconds: Some(30),
                ..Default::default()
            },
            ..Default::default()
        };
        let resolved = ResolvedSettings::from_layers(&[project, user]);
        assert_eq!(resolved.auto_save_interval_seconds, 30);
        // enabled still inherited from project layer
        assert!(resolved.auto_save_enabled);
    }

    #[test]
    fn toml_roundtrip_matches_spec_shape() {
        let toml_text = r#"
[color]
ocio_config = "./ocio/config.ocio"
working_space = "ACEScg"
display_space = "sRGB"

[playback]
frame_rate = "30"
proxy_mode = "auto"
proxy_resolution = 0.5

[auto_save]
enabled = true
interval_seconds = 120
"#;
        let layer = SettingsLayer::from_toml(toml_text).unwrap();
        assert_eq!(layer.color.working_space.as_deref(), Some("ACEScg"));
        assert_eq!(layer.playback.proxy_mode, Some(ProxyMode::Auto));
        assert_eq!(layer.auto_save.interval_seconds, Some(120));

        // Re-serialize and re-parse: structure must be preserved.
        let serialized = layer.to_toml().unwrap();
        let back = SettingsLayer::from_toml(&serialized).unwrap();
        assert_eq!(layer, back);
    }

    #[test]
    fn malformed_toml_is_error() {
        assert!(SettingsLayer::from_toml("[color\nbroken").is_err());
    }

    #[test]
    fn empty_toml_is_empty_layer() {
        let layer = SettingsLayer::from_toml("").unwrap();
        assert_eq!(layer, SettingsLayer::default());
    }
}
