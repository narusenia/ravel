// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Format-version migration chain for `.ravprj` archives.
//!
//! When an older project is opened, its `manifest.json` is parsed as untyped
//! JSON and run through a sequential chain of migration steps — `v1 → v2 →
//! …` — until it reaches [`CURRENT_FORMAT_VERSION`]. Each step is a pure
//! function over a [`serde_json::Value`]; steps are applied strictly in
//! ascending version order so that a `v1` file is brought current by composing
//! every intermediate step rather than jumping straight to the latest schema.
//!
//! Keeping migrations at the untyped-JSON layer means a field that no longer
//! exists in the typed [`Manifest`](super::manifest::Manifest) can still be
//! read from an old file and rewritten into the new shape before strong typing
//! is applied.

use serde_json::Value;
use thiserror::Error;

use crate::project::manifest::CURRENT_FORMAT_VERSION;

/// Errors raised during migration.
#[derive(Debug, Error)]
pub enum MigrationError {
    #[error("manifest is missing the `format_version` field")]
    MissingVersion,

    #[error("manifest `format_version` is not an integer")]
    InvalidVersion,

    #[error("project format version {found} is newer than supported version {supported}")]
    TooNew { found: u32, supported: u32 },

    #[error("no migration step registered from version {0}")]
    NoStep(u32),

    #[error("migration step v{from}->v{to} failed: {reason}")]
    StepFailed { from: u32, to: u32, reason: String },
}

/// Read the `format_version` field from a manifest JSON value.
pub fn read_version(manifest: &Value) -> Result<u32, MigrationError> {
    let raw = manifest
        .get("format_version")
        .ok_or(MigrationError::MissingVersion)?;
    let n = raw.as_u64().ok_or(MigrationError::InvalidVersion)?;
    u32::try_from(n).map_err(|_| MigrationError::InvalidVersion)
}

/// Apply a single migration step that advances a manifest by exactly one
/// version. Returns the new version number on success.
fn apply_step(manifest: &mut Value, from: u32) -> Result<u32, MigrationError> {
    match from {
        1 => {
            migrate_v1_to_v2(manifest).map_err(|reason| MigrationError::StepFailed {
                from: 1,
                to: 2,
                reason,
            })?;
            Ok(2)
        }
        2 => {
            migrate_v2_to_v3(manifest).map_err(|reason| MigrationError::StepFailed {
                from: 2,
                to: 3,
                reason,
            })?;
            Ok(3)
        }
        other => Err(MigrationError::NoStep(other)),
    }
}

/// Migrate `manifest` in place until it reaches [`CURRENT_FORMAT_VERSION`].
///
/// Returns `Ok(())` once the manifest is current (a no-op for already-current
/// files). Fails if the file is newer than this build understands or if any
/// intermediate step is missing.
pub fn migrate_to_current(manifest: &mut Value) -> Result<(), MigrationError> {
    let mut version = read_version(manifest)?;

    if version > CURRENT_FORMAT_VERSION {
        return Err(MigrationError::TooNew {
            found: version,
            supported: CURRENT_FORMAT_VERSION,
        });
    }

    while version < CURRENT_FORMAT_VERSION {
        version = apply_step(manifest, version)?;
        // Keep the embedded field consistent after each step.
        if let Some(obj) = manifest.as_object_mut() {
            obj.insert("format_version".to_string(), Value::from(version));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Concrete migration steps
// ---------------------------------------------------------------------------

/// `v1 → v2`: the v1 schema stored a flat `color_space` string and lacked an
/// explicit `resolution` block. v2 renames `color_space` to `color_config` and
/// guarantees a `resolution` object exists.
fn migrate_v1_to_v2(manifest: &mut Value) -> Result<(), String> {
    let obj = manifest
        .as_object_mut()
        .ok_or_else(|| "manifest root is not a JSON object".to_string())?;

    // Rename color_space -> color_config (only if not already present).
    if !obj.contains_key("color_config")
        && let Some(color_space) = obj.remove("color_space")
    {
        obj.insert("color_config".to_string(), color_space);
    }

    // Guarantee a resolution block with sane defaults.
    if !obj.contains_key("resolution") {
        obj.insert(
            "resolution".to_string(),
            serde_json::json!({ "width": 1920, "height": 1080 }),
        );
    }

    Ok(())
}

/// `v2 → v3`: the manifest schema is unchanged — v3 replaces the
/// archive-level `graph/main.ron` entry with `document/main.ron`. That move
/// is handled by [`super::ProjectFile::from_archive`], which wraps a legacy
/// flat graph in a `Document` with a fresh root composition; the manifest
/// only advances its version stamp.
fn migrate_v2_to_v3(_manifest: &mut Value) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::manifest::Manifest;

    fn v1_manifest() -> Value {
        serde_json::json!({
            "format_version": 1,
            "ravel_version": "0.0.1",
            "project_name": "Legacy",
            "created_at": "2026-01-01T00:00:00Z",
            "modified_at": "2026-01-02T00:00:00Z",
            "frame_rate": { "num": 24, "den": 1 },
            "color_space": "aces_1.2"
        })
    }

    #[test]
    fn reads_version() {
        assert_eq!(read_version(&v1_manifest()).unwrap(), 1);
    }

    #[test]
    fn missing_version_errors() {
        let v = serde_json::json!({ "project_name": "x" });
        assert!(matches!(
            read_version(&v),
            Err(MigrationError::MissingVersion)
        ));
    }

    #[test]
    fn v1_migrates_to_current_and_typechecks() {
        let mut m = v1_manifest();
        migrate_to_current(&mut m).unwrap();
        assert_eq!(read_version(&m).unwrap(), CURRENT_FORMAT_VERSION);

        // color_space renamed to color_config, resolution synthesized.
        assert!(m.get("color_space").is_none());
        assert_eq!(m["color_config"], Value::from("aces_1.2"));
        assert_eq!(m["resolution"]["width"], Value::from(1920));

        // The migrated value must deserialize into the current typed Manifest.
        let manifest: Manifest = serde_json::from_value(m).unwrap();
        assert_eq!(manifest.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(manifest.color_config.as_deref(), Some("aces_1.2"));
    }

    #[test]
    fn v2_migrates_to_v3_with_schema_unchanged() {
        let mut m = serde_json::json!({
            "format_version": 2,
            "ravel_version": "0.1.0",
            "project_name": "Mid",
            "created_at": "2026-06-01T00:00:00Z",
            "modified_at": "2026-06-02T00:00:00Z",
            "frame_rate": { "num": 30, "den": 1 },
            "resolution": { "width": 1280, "height": 720 }
        });
        migrate_to_current(&mut m).unwrap();
        assert_eq!(read_version(&m).unwrap(), 3);

        // Only the version stamp advanced; every other field is preserved.
        assert_eq!(m["project_name"], Value::from("Mid"));
        assert_eq!(m["resolution"]["width"], Value::from(1280));
        let manifest: Manifest = serde_json::from_value(m).unwrap();
        assert_eq!(manifest.format_version, CURRENT_FORMAT_VERSION);
    }

    #[test]
    fn current_version_is_noop() {
        let mut m = serde_json::to_value(Manifest::new("P", "t")).unwrap();
        let before = m.clone();
        migrate_to_current(&mut m).unwrap();
        assert_eq!(m, before);
    }

    #[test]
    fn newer_version_is_rejected() {
        let mut m = v1_manifest();
        m["format_version"] = Value::from(CURRENT_FORMAT_VERSION + 1);
        assert!(matches!(
            migrate_to_current(&mut m),
            Err(MigrationError::TooNew { .. })
        ));
    }
}
