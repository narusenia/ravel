// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Embedded asset source: Ravel's own icons with a fallback to the
//! gpui-component icon set.
//!
//! Ravel icons are vendored Lucide SVGs under `assets/icons/` (ISC licensed,
//! see `assets/icons/LICENSE`). Only icons that are actually used get
//! vendored — do not bulk-import the full Lucide set. The `ui-design-impl`
//! skill documents the vendoring procedure.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_component::IconNamed;
use ravel_ui::panel::PanelKind;
use rust_embed::RustEmbed;

/// Ravel-vendored icons, embedded at compile time.
#[derive(RustEmbed)]
#[folder = "../../assets"]
#[include = "icons/**/*.svg"]
struct RavelEmbed;

/// Serves Ravel icons first, then falls back to the gpui-component asset set
/// so built-in widget icons (chevrons, checks, …) resolve too.
pub struct RavelAssets;

impl AssetSource for RavelAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(file) = RavelEmbed::get(path) {
            return Ok(Some(file.data));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut entries: Vec<SharedString> = RavelEmbed::iter()
            .filter(|p| p.starts_with(path))
            .map(|p| p.to_string().into())
            .collect();
        entries.extend(gpui_component_assets::Assets.list(path)?);
        Ok(entries)
    }
}

/// Ravel-specific icons (vendored Lucide names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RavelIcon {
    Outliner,
    NodeGraph,
    Timeline,
    Viewer,
    Dopesheet,
    Properties,
    MediaBin,
    CurveEditor,
    Waveform,
    Vectorscope,
    Histogram,
    Parade,
    TextEditor,
    ShaderEditor,
    LuaConsole,
    RenderQueue,
    /// Viewer toolbar: fit the composition to the panel.
    ZoomFit,
    /// Viewer toolbar: 100% (one comp pixel per screen pixel).
    ZoomActualSize,
    /// Viewer toolbar: proportional grid overlay.
    GridOverlay,
    /// Viewer toolbar: action/title safe-area overlay.
    SafeAreas,
}

impl RavelIcon {
    pub fn for_panel(kind: PanelKind) -> Self {
        match kind {
            PanelKind::Outliner => Self::Outliner,
            PanelKind::NodeGraph => Self::NodeGraph,
            PanelKind::Timeline => Self::Timeline,
            PanelKind::Viewer => Self::Viewer,
            PanelKind::Dopesheet => Self::Dopesheet,
            PanelKind::Properties => Self::Properties,
            PanelKind::MediaBin => Self::MediaBin,
            PanelKind::CurveEditor => Self::CurveEditor,
            PanelKind::Waveform => Self::Waveform,
            PanelKind::Vectorscope => Self::Vectorscope,
            PanelKind::Histogram => Self::Histogram,
            PanelKind::Parade => Self::Parade,
            PanelKind::TextEditor => Self::TextEditor,
            PanelKind::ShaderEditor => Self::ShaderEditor,
            PanelKind::LuaConsole => Self::LuaConsole,
            PanelKind::RenderQueue => Self::RenderQueue,
        }
    }
}

impl IconNamed for RavelIcon {
    fn path(self) -> SharedString {
        match self {
            Self::Outliner => "icons/list-tree.svg",
            Self::NodeGraph => "icons/workflow.svg",
            Self::Timeline => "icons/layers.svg",
            Self::Viewer => "icons/monitor-play.svg",
            Self::Dopesheet => "icons/diamond.svg",
            Self::Properties => "icons/sliders-horizontal.svg",
            Self::MediaBin => "icons/clapperboard.svg",
            Self::CurveEditor => "icons/spline.svg",
            Self::Waveform => "icons/audio-waveform.svg",
            Self::Vectorscope => "icons/radar.svg",
            Self::Histogram => "icons/chart-column.svg",
            Self::Parade => "icons/chart-bar-big.svg",
            Self::TextEditor => "icons/type.svg",
            Self::ShaderEditor => "icons/braces.svg",
            Self::LuaConsole => "icons/terminal.svg",
            Self::RenderQueue => "icons/list-video.svg",
            Self::ZoomFit => "icons/maximize.svg",
            Self::ZoomActualSize => "icons/square-square.svg",
            Self::GridOverlay => "icons/grid-3x3.svg",
            Self::SafeAreas => "icons/frame.svg",
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_panel_icon_is_embedded() {
        for kind in PanelKind::ALL {
            let path = RavelIcon::for_panel(kind).path();
            assert!(
                RavelEmbed::get(path.as_ref()).is_some(),
                "missing embedded icon for {kind:?}: {path}"
            );
        }
    }

    #[test]
    fn fallback_serves_component_icons() {
        let loaded = RavelAssets
            .load(&gpui_component::IconName::ChevronDown.path())
            .unwrap();
        assert!(loaded.is_some(), "gpui-component fallback icons must load");
    }

    #[test]
    fn license_is_vendored_alongside_icons() {
        // ISC attribution must travel with the vendored SVGs.
        assert!(
            std::path::Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../assets/icons/LICENSE"
            ))
            .exists()
        );
    }
}
