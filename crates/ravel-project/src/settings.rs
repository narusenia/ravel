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

use std::path::Path;

use ravel_core::cache_budget::CacheBudgetConfig;
use ravel_ui::node_editor::EdgeStyle;
use serde::{Deserialize, Serialize};

/// Bytes in one mebibyte. Limits are written in MiB in `settings.toml` — the
/// budget counts bytes, and a settings file full of ten-digit numbers is not
/// something anyone edits correctly.
const MIB: u64 = 1024 * 1024;

/// The smallest cache tier limit a setting may hold, in MiB.
///
/// Not zero: a zero ceiling evicts every entry as it is produced, which is a
/// way to make the application appear to hang rather than a cache size anyone
/// wants. One 1080p RGBA f32 frame is about 32 MiB, so anything below that is
/// already degenerate; 1 MiB is the floor that keeps the setting honest without
/// pretending to know the user's working set.
pub const MIN_CACHE_LIMIT_MB: f64 = 1.0;

/// The largest cache tier limit a setting may hold, in MiB (1 TiB).
///
/// A bound rather than a technical limit, for the reason the frame rate is
/// bounded where it is parsed: an unbounded field turns a stray keystroke into
/// a number the accounting can only be surprised by.
pub const MAX_CACHE_LIMIT_MB: f64 = 1024.0 * 1024.0;

/// Read the global settings layer from its platform location.
pub fn read_global_settings() -> SettingsLayer {
    read_global_settings_at(crate::paths::global_settings_path().as_deref())
}

/// Read one global settings layer from an explicit path.
///
/// A missing, unreadable, or malformed file is no override. This is the same
/// launch-safe behaviour the GUI uses, and keeps headless callers from having
/// to duplicate TOML and filesystem error handling.
pub fn read_global_settings_at(path: Option<&Path>) -> SettingsLayer {
    let Some(path) = path else {
        return SettingsLayer::default();
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SettingsLayer::default();
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not read the settings file");
            return SettingsLayer::default();
        }
    };
    match SettingsLayer::from_toml(&text) {
        Ok(layer) => layer,
        Err(error) => {
            tracing::warn!(
                %error,
                path = %path.display(),
                "ignoring the settings file; starting on the defaults"
            );
            SettingsLayer::default()
        }
    }
}

/// The MiB figure `value` names, or `None` for one no tier limit may hold.
///
/// **The one range check for a cache limit**, used both where the value is
/// typed (`ravel-app`'s settings dialog) and where it is applied
/// ([`usable_cache_budget`]) — the dialog is not the only writer, and two
/// copies of a range are two ranges.
///
/// A fraction of a MiB is truncated: the setting's unit is whole MiB, and
/// refusing `512.5` would be pedantry rather than safety.
pub fn cache_limit_mb(value: f64) -> Option<u64> {
    (value.is_finite() && (MIN_CACHE_LIMIT_MB..=MAX_CACHE_LIMIT_MB).contains(&value))
        .then(|| value.trunc() as u64)
}

/// The simulation reserve share `value` names, or `None` for one no tier may
/// hold. [`cache_limit_mb`]'s argument, for the share.
///
/// `CacheBudget` clamps this defensively, so an out-of-range share is not
/// dangerous — but a `NaN` passes `clamp` untouched and silently turns the
/// reserve into zero, which is the protection quietly disappearing rather than
/// a setting being refused.
pub fn cache_sim_reserve_ratio(value: f64) -> Option<f32> {
    (value.is_finite() && (0.0..=1.0).contains(&value)).then_some(value as f32)
}

/// The absolute path `text` names, or `None` when it names none — an empty
/// field ("no location of my own") and a relative path are both that.
///
/// Relative is refused rather than resolved because the working directory of a
/// desktop application is not something the user chose: the cache would land
/// wherever Ravel happened to be launched from, and move when that changed.
pub fn cache_root_setting(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    (!trimmed.is_empty() && Path::new(trimmed).is_absolute()).then_some(trimmed)
}

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

/// The bundled light theme, from `assets/themes/ravel.json`.
///
/// The default here rather than in the loader: it is what an unset
/// `appearance.light_theme` resolves to, and the startup path applies the
/// resolved value like any other
/// (`ravel-app`'s `app_settings::apply_resolved_appearance`).
pub const DEFAULT_LIGHT_THEME: &str = "Ravel Light";

/// The bundled dark theme, from `assets/themes/ravel.json`.
pub const DEFAULT_DARK_THEME: &str = "Ravel Dark";

/// Which of the two themes the UI wears.
///
/// [`AppearanceMode::System`] is why this is not `gpui_component::ThemeMode`:
/// the component knows only light and dark, while "follow the OS" is a third
/// choice the user can make and the default one — it has to survive in the
/// settings file rather than be flattened into whatever the OS happened to
/// report when it was written.
///
/// **`System` samples the OS appearance when the appearance is applied and does
/// not track a change made while Ravel is running** — deliberate, not an
/// oversight: it is what Ravel already did before the setting existed, and
/// following the OS live needs a window-appearance observer that is its own unit
/// of work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppearanceMode {
    #[default]
    System,
    Light,
    Dark,
}

impl AppearanceMode {
    /// Every mode, in the order the Appearance page offers them.
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    /// The value this mode is written as, which is also the value its dropdown
    /// option carries — the TOML spelling and the UI's option id are the same
    /// string so no third mapping can disagree with `serde`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// The mode `value` names, or `None` when it names no mode.
    pub fn from_value(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == value)
    }
}

/// Appearance settings (all fields optional for layering).
///
/// The two theme names are separate so that switching mode does not lose the
/// other mode's choice: a user who picked a custom dark theme keeps it after a
/// day spent in light mode.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppearanceLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_mode: Option<AppearanceMode>,
    /// Theme used while the UI is light. A name the theme registry does not
    /// have falls back at apply time rather than at read time, because the
    /// registry is still filling in when the settings are first read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_theme: Option<String>,
    /// Theme used while the UI is dark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark_theme: Option<String>,
}

impl AppearanceLayer {
    fn merge(&self, over: &AppearanceLayer) -> AppearanceLayer {
        AppearanceLayer {
            theme_mode: over.theme_mode.or(self.theme_mode),
            light_theme: over
                .light_theme
                .clone()
                .or_else(|| self.light_theme.clone()),
            dark_theme: over.dark_theme.clone().or_else(|| self.dark_theme.clone()),
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
    /// Whether Stop returns the playhead to the frame playback started from
    /// instead of rewinding to frame 0. Off is what Ravel has always done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_returns_to_play_start: Option<bool>,
}

impl PlaybackLayer {
    fn merge(&self, over: &PlaybackLayer) -> PlaybackLayer {
        PlaybackLayer {
            frame_rate: over.frame_rate.clone().or_else(|| self.frame_rate.clone()),
            proxy_mode: over.proxy_mode.or(self.proxy_mode),
            proxy_resolution: over.proxy_resolution.or(self.proxy_resolution),
            stop_returns_to_play_start: over
                .stop_returns_to_play_start
                .or(self.stop_returns_to_play_start),
        }
    }
}

/// Startup settings (all fields optional for layering).
///
/// What a document with nothing in it starts as — both the launch document and
/// the one `File ▸ New` builds, because they are the same "nothing to inherit
/// from" case.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupLayer {
    /// Whether a fresh document is given one empty composition. On is what
    /// Ravel has always done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_composition: Option<bool>,
}

impl StartupLayer {
    fn merge(&self, over: &StartupLayer) -> StartupLayer {
        StartupLayer {
            create_composition: over.create_composition.or(self.create_composition),
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

/// Cache budget settings (all fields optional for layering).
///
/// Limits are the totals `CacheBudget` enforces per tier — for VRAM that is
/// the whole allowance, resident textures and the texture pool's idle
/// reserve together, because the pool's idle share is whatever the resident
/// side leaves over (`cache-plan.md`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CacheLayer {
    /// Where the disk tier stores its files. `None` means the default
    /// location under the global config directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Total VRAM the caches may occupy, in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_limit_mb: Option<u64>,
    /// Total host memory the caches may occupy, in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_limit_mb: Option<u64>,
    /// Total disk spill allowance, in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_limit_mb: Option<u64>,
    /// Share of every tier held back for simulation state (`0.0`–`1.0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sim_reserve_ratio: Option<f32>,
    /// Whether the disk tier is used at all. The layer itself is `CACHE-11`;
    /// the switch exists so a project can be written against it now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_enabled: Option<bool>,
}

impl CacheLayer {
    fn merge(&self, over: &CacheLayer) -> CacheLayer {
        CacheLayer {
            root: over.root.clone().or_else(|| self.root.clone()),
            vram_limit_mb: over.vram_limit_mb.or(self.vram_limit_mb),
            ram_limit_mb: over.ram_limit_mb.or(self.ram_limit_mb),
            disk_limit_mb: over.disk_limit_mb.or(self.disk_limit_mb),
            sim_reserve_ratio: over.sim_reserve_ratio.or(self.sim_reserve_ratio),
            disk_enabled: over.disk_enabled.or(self.disk_enabled),
        }
    }
}

/// Node editor settings (all fields optional for layering).
///
/// The edge style is a *preference*, not a property of a project or of one
/// window: it belongs wherever the user last chose it and should hold across
/// projects, which is what the global layer expresses and neither the panel's
/// own state nor `ui_state.json` can (`node-graph-readability-plan.md`).
///
/// `flow_direction` joins this section with the top-down flow mode (`NGR-5`).
/// It is deliberately absent until then: a setting that changes nothing is a
/// dead row on the settings screen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeEditorLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_style: Option<EdgeStyle>,
}

impl NodeEditorLayer {
    fn merge(&self, over: &NodeEditorLayer) -> NodeEditorLayer {
        NodeEditorLayer {
            edge_style: over.edge_style.or(self.edge_style),
        }
    }
}

/// A single, partial settings layer as read from one `settings.toml`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsLayer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(default)]
    pub appearance: AppearanceLayer,
    #[serde(default)]
    pub color: ColorLayer,
    #[serde(default)]
    pub playback: PlaybackLayer,
    #[serde(default)]
    pub startup: StartupLayer,
    #[serde(default)]
    pub auto_save: AutoSaveLayer,
    #[serde(default)]
    pub cache: CacheLayer,
    #[serde(default)]
    pub node_editor: NodeEditorLayer,
}

impl SettingsLayer {
    /// Fold `over` (higher priority) onto `self`, returning the merged layer.
    pub fn merge(&self, over: &SettingsLayer) -> SettingsLayer {
        SettingsLayer {
            locale: over.locale.clone().or_else(|| self.locale.clone()),
            appearance: self.appearance.merge(&over.appearance),
            color: self.color.merge(&over.color),
            playback: self.playback.merge(&over.playback),
            startup: self.startup.merge(&over.startup),
            auto_save: self.auto_save.merge(&over.auto_save),
            cache: self.cache.merge(&over.cache),
            node_editor: self.node_editor.merge(&over.node_editor),
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
    pub theme_mode: AppearanceMode,
    /// Theme to wear while light. What is *in force* may differ: a name no
    /// theme in the registry carries falls back when it is applied
    /// (`ravel-app`'s `app_settings::apply_resolved_appearance`), and this keeps
    /// naming the theme the settings asked for, so a theme file that arrives
    /// later is picked up instead of being forgotten.
    pub light_theme: String,
    /// Theme to wear while dark.
    pub dark_theme: String,
    pub ocio_config: Option<String>,
    pub working_space: String,
    pub display_space: String,
    pub frame_rate: String,
    pub proxy_mode: ProxyMode,
    pub proxy_resolution: f32,
    /// Whether Stop returns to the frame playback started from.
    pub stop_returns_to_play_start: bool,
    /// Whether a fresh document is given one empty composition.
    pub startup_creates_composition: bool,
    pub auto_save_enabled: bool,
    pub auto_save_interval_seconds: u32,
    pub cache_root: Option<String>,
    pub cache_vram_limit_mb: u64,
    pub cache_ram_limit_mb: u64,
    pub cache_disk_limit_mb: u64,
    pub cache_sim_reserve_ratio: f32,
    pub cache_disk_enabled: bool,
    /// How the node editor draws its edges.
    pub node_editor_edge_style: EdgeStyle,
}

impl Default for ResolvedSettings {
    fn default() -> Self {
        Self {
            locale: "en".to_string(),
            // The launch behaviour a user without a settings file has always
            // had: follow the OS, wearing the bundled Ravel themes.
            theme_mode: AppearanceMode::System,
            light_theme: DEFAULT_LIGHT_THEME.to_string(),
            dark_theme: DEFAULT_DARK_THEME.to_string(),
            ocio_config: None,
            working_space: "ACEScg".to_string(),
            display_space: "sRGB".to_string(),
            frame_rate: "30".to_string(),
            proxy_mode: ProxyMode::Auto,
            proxy_resolution: 0.5,
            // Both of these default to what Ravel already did: Stop rewinds to
            // frame 0, and a fresh document opens on one empty composition.
            stop_returns_to_play_start: false,
            startup_creates_composition: true,
            auto_save_enabled: true,
            auto_save_interval_seconds: 120,
            // The budget owns the canonical numbers; restating them here
            // would give the process two sets of defaults to disagree over.
            cache_root: None,
            cache_vram_limit_mb: CacheBudgetConfig::DEFAULT_VRAM_BYTES / MIB,
            cache_ram_limit_mb: CacheBudgetConfig::DEFAULT_RAM_BYTES / MIB,
            cache_disk_limit_mb: CacheBudgetConfig::DEFAULT_DISK_BYTES / MIB,
            cache_sim_reserve_ratio: CacheBudgetConfig::DEFAULT_SIM_RESERVE_RATIO,
            // `CACHE-11` builds the disk tier; nothing writes it yet.
            cache_disk_enabled: false,
            // The style the editor has always started on.
            node_editor_edge_style: EdgeStyle::Bezier,
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
            theme_mode: merged.appearance.theme_mode.unwrap_or(d.theme_mode),
            light_theme: merged
                .appearance
                .light_theme
                .clone()
                .unwrap_or(d.light_theme),
            dark_theme: merged.appearance.dark_theme.clone().unwrap_or(d.dark_theme),
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
            stop_returns_to_play_start: merged
                .playback
                .stop_returns_to_play_start
                .unwrap_or(d.stop_returns_to_play_start),
            startup_creates_composition: merged
                .startup
                .create_composition
                .unwrap_or(d.startup_creates_composition),
            auto_save_enabled: merged.auto_save.enabled.unwrap_or(d.auto_save_enabled),
            auto_save_interval_seconds: merged
                .auto_save
                .interval_seconds
                .unwrap_or(d.auto_save_interval_seconds),
            cache_root: merged.cache.root.clone(),
            cache_vram_limit_mb: merged.cache.vram_limit_mb.unwrap_or(d.cache_vram_limit_mb),
            cache_ram_limit_mb: merged.cache.ram_limit_mb.unwrap_or(d.cache_ram_limit_mb),
            cache_disk_limit_mb: merged.cache.disk_limit_mb.unwrap_or(d.cache_disk_limit_mb),
            cache_sim_reserve_ratio: merged
                .cache
                .sim_reserve_ratio
                .unwrap_or(d.cache_sim_reserve_ratio),
            cache_disk_enabled: merged.cache.disk_enabled.unwrap_or(d.cache_disk_enabled),
            node_editor_edge_style: merged
                .node_editor
                .edge_style
                .unwrap_or(d.node_editor_edge_style),
        }
    }

    /// Resolve directly from an ordered list of layers (lowest priority first).
    pub fn from_layers(layers: &[SettingsLayer]) -> Self {
        Self::resolve(&SettingsLayer::merge_all(layers))
    }

    /// The cache limits these settings imply.
    ///
    /// The one conversion from settings to
    /// [`CacheBudgetConfig`]: MiB to bytes, and a disabled disk tier resolves
    /// to a zero allowance rather than to a separate flag the budget would
    /// have to know about.
    pub fn cache_budget(&self) -> CacheBudgetConfig {
        CacheBudgetConfig {
            vram_bytes: self.cache_vram_limit_mb.saturating_mul(MIB),
            ram_bytes: self.cache_ram_limit_mb.saturating_mul(MIB),
            disk_bytes: if self.cache_disk_enabled {
                self.cache_disk_limit_mb.saturating_mul(MIB)
            } else {
                0
            },
            sim_reserve_ratio: self.cache_sim_reserve_ratio,
        }
    }
}

/// The budget `settings` imply, with anything the file could hold but the
/// accounting cannot use replaced by the built-in default and warned about.
///
/// The settings dialog's rows refuse these values already, and this is not a
/// second opinion about them: `settings.toml` and a project's settings layer
/// are hand-editable and reach the budget without passing a row at all — and
/// `ravel-cli` has no rows to pass. A `vram_limit_mb = 0` written there would
/// otherwise arrive as a ceiling that evicts everything the moment it is
/// produced. Both checks call the same functions ([`cache_limit_mb`],
/// [`cache_sim_reserve_ratio`]), so the range is stated once.
///
/// The **disk** limit is left alone: nothing charges `Tier::Disk` yet
/// (`CACHE-11`), and the default `disk_enabled = false` resolves it to a zero
/// allowance regardless.
pub fn usable_cache_budget(settings: &ResolvedSettings) -> CacheBudgetConfig {
    let defaults = ResolvedSettings::default();
    let mut settings = settings.clone();
    settings.cache_vram_limit_mb = usable_limit_mb(
        settings.cache_vram_limit_mb,
        defaults.cache_vram_limit_mb,
        "vram",
    );
    settings.cache_ram_limit_mb = usable_limit_mb(
        settings.cache_ram_limit_mb,
        defaults.cache_ram_limit_mb,
        "ram",
    );
    settings.cache_sim_reserve_ratio =
        cache_sim_reserve_ratio(f64::from(settings.cache_sim_reserve_ratio)).unwrap_or_else(|| {
            tracing::warn!(
                sim_reserve_ratio = settings.cache_sim_reserve_ratio,
                "unusable simulation cache reserve in the settings; using the default"
            );
            defaults.cache_sim_reserve_ratio
        });
    // The MiB→bytes conversion stays in `ResolvedSettings::cache_budget`, so
    // the corrected values go back through it rather than around it.
    settings.cache_budget()
}

/// One tier limit, or the default with a warning.
fn usable_limit_mb(value: u64, default: u64, tier: &'static str) -> u64 {
    cache_limit_mb(value as f64).unwrap_or_else(|| {
        tracing::warn!(
            limit_mb = value,
            tier,
            "cache limit out of range in the settings; using the default"
        );
        default
    })
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
    fn cache_defaults_match_the_budget_constants() {
        let resolved = ResolvedSettings::from_layers(&[]);
        let config = resolved.cache_budget();
        assert_eq!(config.vram_bytes, CacheBudgetConfig::DEFAULT_VRAM_BYTES);
        assert_eq!(config.ram_bytes, CacheBudgetConfig::DEFAULT_RAM_BYTES);
        assert_eq!(
            config.sim_reserve_ratio,
            CacheBudgetConfig::DEFAULT_SIM_RESERVE_RATIO
        );
    }

    #[test]
    fn cache_layer_merges_field_by_field() {
        let global = SettingsLayer {
            cache: CacheLayer {
                ram_limit_mb: Some(4096),
                sim_reserve_ratio: Some(0.5),
                ..Default::default()
            },
            ..Default::default()
        };
        let project = SettingsLayer {
            cache: CacheLayer {
                ram_limit_mb: Some(1024),
                ..Default::default()
            },
            ..Default::default()
        };
        let resolved = ResolvedSettings::from_layers(&[global, project]);
        assert_eq!(resolved.cache_ram_limit_mb, 1024);
        // The ratio the project left unset still comes from global.
        assert_eq!(resolved.cache_sim_reserve_ratio, 0.5);
        assert_eq!(resolved.cache_budget().ram_bytes, 1024 * MIB);
    }

    /// The rules this crate owns, checked here rather than only in the crates
    /// that call them: a hand-edited `settings.toml` reaches the budget without
    /// passing a dialog row, and `ravel-cli` has no rows at all.
    #[test]
    fn an_unusable_cache_setting_falls_back_to_the_default() {
        let defaults = ResolvedSettings::default();
        let unusable = |vram: u64, ram: u64, ratio: f32| {
            usable_cache_budget(&ResolvedSettings {
                cache_vram_limit_mb: vram,
                cache_ram_limit_mb: ram,
                cache_sim_reserve_ratio: ratio,
                ..defaults.clone()
            })
        };

        // Below MIN (a zero ceiling evicts everything it produces), above MAX,
        // and the `NaN` that would pass `CacheBudget`'s own `clamp` and zero
        // the simulation reserve without a word.
        let budget = unusable(0, MAX_CACHE_LIMIT_MB as u64 + 1, f32::NAN);
        assert_eq!(budget.vram_bytes, defaults.cache_vram_limit_mb * MIB);
        assert_eq!(budget.ram_bytes, defaults.cache_ram_limit_mb * MIB);
        assert_eq!(budget.sim_reserve_ratio, defaults.cache_sim_reserve_ratio);

        // The bounds themselves are usable, so the range is closed on both ends.
        let budget = unusable(MIN_CACHE_LIMIT_MB as u64, MAX_CACHE_LIMIT_MB as u64, 1.0);
        assert_eq!(budget.vram_bytes, MIB);
        assert_eq!(budget.ram_bytes, MAX_CACHE_LIMIT_MB as u64 * MIB);
        assert_eq!(budget.sim_reserve_ratio, 1.0);

        // The cache location is refused rather than resolved when it is not
        // absolute — the working directory is not something the user chose.
        assert!(cache_root_setting("relative/cache").is_none());
        assert!(cache_root_setting("  ").is_none());
    }

    #[test]
    fn a_disabled_disk_tier_resolves_to_no_allowance() {
        let layer = SettingsLayer {
            cache: CacheLayer {
                disk_limit_mb: Some(8192),
                disk_enabled: Some(false),
                ..Default::default()
            },
            ..Default::default()
        };
        let resolved = ResolvedSettings::from_layers(&[layer]);
        assert_eq!(resolved.cache_disk_limit_mb, 8192);
        assert_eq!(resolved.cache_budget().disk_bytes, 0);
    }

    /// The appearance defaults have to be the behaviour a user without a
    /// settings file already had: follow the OS, wearing the bundled themes.
    /// Changing them would change how Ravel looks for everyone who never
    /// opened the Appearance page.
    #[test]
    fn appearance_defaults_keep_the_current_launch_behaviour() {
        let resolved = ResolvedSettings::from_layers(&[]);
        assert_eq!(resolved.theme_mode, AppearanceMode::System);
        assert_eq!(resolved.light_theme, "Ravel Light");
        assert_eq!(resolved.dark_theme, "Ravel Dark");
    }

    /// The two theme names override independently, so choosing a dark theme
    /// does not silently reset the light one.
    #[test]
    fn appearance_layer_merges_field_by_field() {
        let global = SettingsLayer {
            appearance: AppearanceLayer {
                theme_mode: Some(AppearanceMode::Dark),
                light_theme: Some("Custom Light".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let project = SettingsLayer {
            appearance: AppearanceLayer {
                dark_theme: Some("Custom Dark".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let resolved = ResolvedSettings::from_layers(&[global, project]);
        assert_eq!(resolved.theme_mode, AppearanceMode::Dark);
        assert_eq!(resolved.light_theme, "Custom Light");
        assert_eq!(resolved.dark_theme, "Custom Dark");
    }

    /// The mode's TOML spelling and the id its dropdown option carries are one
    /// string: a round trip through both directions has to agree with `serde`.
    #[test]
    fn appearance_mode_values_round_trip() {
        for mode in AppearanceMode::ALL {
            assert_eq!(AppearanceMode::from_value(mode.as_str()), Some(mode));
            let layer = SettingsLayer::from_toml(&format!(
                "[appearance]\ntheme_mode = \"{}\"\n",
                mode.as_str()
            ))
            .expect("the mode parses");
            assert_eq!(layer.appearance.theme_mode, Some(mode));
        }
        assert_eq!(AppearanceMode::from_value("sepia"), None);
        // An unknown mode is a broken layer, which the reader degrades to no
        // overrides at all rather than to a half-read one.
        assert!(SettingsLayer::from_toml("[appearance]\ntheme_mode = \"sepia\"\n").is_err());
    }

    /// A settings file written before the appearance section existed still
    /// reads, which is why the format needs no version bump.
    #[test]
    fn a_settings_file_without_the_appearance_section_still_reads() {
        let layer =
            SettingsLayer::from_toml("locale = \"ja\"\n\n[playback]\nframe_rate = \"24\"\n")
                .expect("an older settings file parses");
        assert_eq!(layer.appearance, AppearanceLayer::default());
        let resolved = ResolvedSettings::resolve(&layer);
        assert_eq!(resolved.theme_mode, AppearanceMode::System);
        assert_eq!(resolved.light_theme, DEFAULT_LIGHT_THEME);
    }

    #[test]
    fn toml_roundtrip_matches_spec_shape() {
        let toml_text = r#"
[appearance]
theme_mode = "dark"
light_theme = "Ravel Light"
dark_theme = "Ravel Dark"

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

[cache]
vram_limit_mb = 1024
ram_limit_mb = 2048
disk_limit_mb = 4096
sim_reserve_ratio = 0.25
disk_enabled = false
"#;
        let layer = SettingsLayer::from_toml(toml_text).unwrap();
        assert_eq!(layer.appearance.theme_mode, Some(AppearanceMode::Dark));
        assert_eq!(layer.appearance.light_theme.as_deref(), Some("Ravel Light"));
        assert_eq!(layer.color.working_space.as_deref(), Some("ACEScg"));
        assert_eq!(layer.playback.proxy_mode, Some(ProxyMode::Auto));
        assert_eq!(layer.auto_save.interval_seconds, Some(120));
        assert_eq!(layer.cache.vram_limit_mb, Some(1024));
        assert_eq!(layer.cache.disk_enabled, Some(false));

        // Re-serialize and re-parse: structure must be preserved.
        let serialized = layer.to_toml().unwrap();
        let back = SettingsLayer::from_toml(&serialized).unwrap();
        assert_eq!(layer, back);
    }

    /// Without a settings file the editor draws what it always drew.
    #[test]
    fn the_edge_style_default_is_the_current_behaviour() {
        assert_eq!(
            ResolvedSettings::from_layers(&[]).node_editor_edge_style,
            EdgeStyle::Bezier
        );
    }

    /// A new section has to ride the same merge direction as the old ones —
    /// later layers win, earlier layers still supply what the later one left
    /// unset. Both halves are pinned here because `merge_all`'s direction
    /// cannot be restated per section.
    #[test]
    fn the_node_editor_section_inherits_and_is_overridden_like_every_other() {
        let global = SettingsLayer {
            node_editor: NodeEditorLayer {
                edge_style: Some(EdgeStyle::Step),
            },
            ..Default::default()
        };

        // Inheritance: the project layer says nothing, so global stands.
        let inherited = ResolvedSettings::from_layers(&[global.clone(), SettingsLayer::default()]);
        assert_eq!(inherited.node_editor_edge_style, EdgeStyle::Step);

        // Override: the later layer wins.
        let project = SettingsLayer {
            node_editor: NodeEditorLayer {
                edge_style: Some(EdgeStyle::Straight),
            },
            ..Default::default()
        };
        let overridden = ResolvedSettings::from_layers(&[global, project]);
        assert_eq!(overridden.node_editor_edge_style, EdgeStyle::Straight);
    }

    /// The section round-trips through the file, and a settings file written
    /// before it existed still reads — which is why no format version moves.
    #[test]
    fn the_node_editor_section_round_trips_and_is_optional() {
        let layer = SettingsLayer::from_toml("[node_editor]\nedge_style = \"straight\"\n")
            .expect("the section parses");
        assert_eq!(layer.node_editor.edge_style, Some(EdgeStyle::Straight));
        let back = SettingsLayer::from_toml(&layer.to_toml().unwrap()).unwrap();
        assert_eq!(layer, back);

        let older = SettingsLayer::from_toml("locale = \"ja\"\n").expect("an older file parses");
        assert_eq!(older.node_editor, NodeEditorLayer::default());
        assert_eq!(
            ResolvedSettings::resolve(&older).node_editor_edge_style,
            EdgeStyle::Bezier
        );
    }

    /// Both switches added for the transport/startup unit default to the
    /// behaviour Ravel already had, so a user without a settings file sees no
    /// change: Stop rewinds to frame 0 and a fresh document has one
    /// composition.
    #[test]
    fn the_stop_and_startup_defaults_keep_the_current_behaviour() {
        let resolved = ResolvedSettings::from_layers(&[]);
        assert!(!resolved.stop_returns_to_play_start);
        assert!(resolved.startup_creates_composition);
    }

    /// Both switches ride the same merge direction as every other field, and a
    /// settings file written before they existed still reads — which is why no
    /// format version moves.
    #[test]
    fn the_stop_and_startup_switches_merge_and_are_optional() {
        let global = SettingsLayer {
            playback: PlaybackLayer {
                stop_returns_to_play_start: Some(true),
                ..Default::default()
            },
            startup: StartupLayer {
                create_composition: Some(false),
            },
            ..Default::default()
        };

        // Inheritance: the project layer says nothing, so global stands.
        let inherited = ResolvedSettings::from_layers(&[global.clone(), SettingsLayer::default()]);
        assert!(inherited.stop_returns_to_play_start);
        assert!(!inherited.startup_creates_composition);

        // Override: the later layer wins, field by field.
        let project = SettingsLayer {
            startup: StartupLayer {
                create_composition: Some(true),
            },
            ..Default::default()
        };
        let overridden = ResolvedSettings::from_layers(&[global, project]);
        assert!(overridden.startup_creates_composition);
        // The playback switch the project left unset still comes from global.
        assert!(overridden.stop_returns_to_play_start);

        // Round trip, and an older file reads as "neither overridden".
        let text = "[playback]\nstop_returns_to_play_start = true\n\
                    \n[startup]\ncreate_composition = false\n";
        let layer = SettingsLayer::from_toml(text).expect("the sections parse");
        assert_eq!(layer.playback.stop_returns_to_play_start, Some(true));
        assert_eq!(layer.startup.create_composition, Some(false));
        assert_eq!(
            SettingsLayer::from_toml(&layer.to_toml().unwrap()).unwrap(),
            layer
        );

        let older = SettingsLayer::from_toml("[playback]\nframe_rate = \"24\"\n")
            .expect("an older file parses");
        assert_eq!(older.playback.stop_returns_to_play_start, None);
        assert_eq!(older.startup, StartupLayer::default());
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
