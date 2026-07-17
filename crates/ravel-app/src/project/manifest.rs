// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Project manifest (`manifest.json`) — top-level metadata and version stamp.
//!
//! The manifest is the first entry read from a `.ravprj` archive. Its
//! [`format_version`](Manifest::format_version) drives the migration chain in
//! [`crate::project::migration`], so it is parsed defensively (as untyped JSON
//! first) before being deserialized into the strongly typed [`Manifest`].

use serde::{Deserialize, Serialize};

/// Current on-disk project format version produced by this build of Ravel.
///
/// Incremented whenever the layout or schema of a `.ravprj` archive changes in
/// a way that requires a migration step.
pub const CURRENT_FORMAT_VERSION: u32 = 3;

/// Rational frame rate stored in the manifest (`{ "num": 30, "den": 1 }`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RationalRate {
    pub num: u32,
    pub den: u32,
}

impl RationalRate {
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }
}

impl Default for RationalRate {
    fn default() -> Self {
        Self::new(30, 1)
    }
}

/// Pixel resolution stored in the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

impl Default for Resolution {
    fn default() -> Self {
        Self::new(1920, 1080)
    }
}

/// Strongly typed view of `manifest.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// On-disk format version. Used to trigger migrations on load.
    pub format_version: u32,
    /// Version of Ravel that last wrote the file.
    pub ravel_version: String,
    /// Human-readable project name.
    pub project_name: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 last-modified timestamp.
    pub modified_at: String,
    /// Project frame rate.
    pub frame_rate: RationalRate,
    /// Project resolution.
    pub resolution: Resolution,
    /// Optional colour-management configuration identifier.
    #[serde(default)]
    pub color_config: Option<String>,
}

impl Manifest {
    /// Construct a manifest for a brand-new project stamped with the current
    /// format version.
    pub fn new(project_name: impl Into<String>, created_at: impl Into<String>) -> Self {
        let created = created_at.into();
        Self {
            format_version: CURRENT_FORMAT_VERSION,
            ravel_version: env!("CARGO_PKG_VERSION").to_string(),
            project_name: project_name.into(),
            modified_at: created.clone(),
            created_at: created,
            frame_rate: RationalRate::default(),
            resolution: Resolution::default(),
            color_config: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manifest_uses_current_version() {
        let m = Manifest::new("My Project", "2026-06-22T10:00:00Z");
        assert_eq!(m.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(m.project_name, "My Project");
        assert_eq!(m.created_at, m.modified_at);
        assert_eq!(m.frame_rate, RationalRate::new(30, 1));
    }

    #[test]
    fn manifest_json_roundtrip() {
        let m = Manifest::new("Roundtrip", "2026-06-22T10:00:00Z");
        let json = serde_json::to_string(&m).unwrap();
        let back: Manifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn color_config_defaults_to_none_when_absent() {
        let json = r#"{
            "format_version": 2,
            "ravel_version": "0.1.0",
            "project_name": "P",
            "created_at": "t",
            "modified_at": "t",
            "frame_rate": { "num": 24, "den": 1 },
            "resolution": { "width": 1280, "height": 720 }
        }"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.color_config, None);
        assert_eq!(m.frame_rate, RationalRate::new(24, 1));
    }
}
