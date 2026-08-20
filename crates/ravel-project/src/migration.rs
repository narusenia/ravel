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

use crate::manifest::CURRENT_FORMAT_VERSION;

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
        3 => {
            migrate_v3_to_v4(manifest).map_err(|reason| MigrationError::StepFailed {
                from: 3,
                to: 4,
                reason,
            })?;
            Ok(4)
        }
        4 => {
            migrate_v4_to_v5(manifest).map_err(|reason| MigrationError::StepFailed {
                from: 4,
                to: 5,
                reason,
            })?;
            Ok(5)
        }
        5 => {
            migrate_v5_to_v6(manifest).map_err(|reason| MigrationError::StepFailed {
                from: 5,
                to: 6,
                reason,
            })?;
            Ok(6)
        }
        6 => {
            migrate_v6_to_v7(manifest).map_err(|reason| MigrationError::StepFailed {
                from: 6,
                to: 7,
                reason,
            })?;
            Ok(7)
        }
        7 => {
            migrate_v7_to_v8(manifest).map_err(|reason| MigrationError::StepFailed {
                from: 7,
                to: 8,
                reason,
            })?;
            Ok(8)
        }
        8 => {
            migrate_v8_to_v9(manifest).map_err(|reason| MigrationError::StepFailed {
                from: 8,
                to: 9,
                reason,
            })?;
            Ok(9)
        }
        9 => {
            migrate_v9_to_v10(manifest).map_err(|reason| MigrationError::StepFailed {
                from: 9,
                to: 10,
                reason,
            })?;
            Ok(10)
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

/// `v3 → v4`: the manifest schema is unchanged. v4 changes two things inside
/// the archive, both handled where the affected entry is read:
///
/// - `document/main.ron` stores each media asset as an
///   [`AssetPath`](ravel_core::composition::AssetPath) string plus a kind and
///   metadata record instead of a bare absolute `PathBuf`. A v3 entry parses
///   as [`AssetPath::Absolute`](ravel_core::composition::AssetPath::Absolute)
///   with its kind inferred from the file extension, so the upgrade needs no
///   rewriting here (see `ravel_core::composition::asset`).
/// - `assets/refs.json` is no longer written. Every version that wrote it
///   wrote an empty collection, so ignoring a leftover entry loses nothing.
fn migrate_v3_to_v4(_manifest: &mut Value) -> Result<(), String> {
    Ok(())
}

/// `v4 → v5`: the manifest schema is unchanged. v5 folds the `_x` / `_y`
/// component parameters of the builtin nodes (`center_x` / `center_y`,
/// `translate_x` / `translate_y`, the scalar `geometry.transform` `rotation`,
/// …) into single `Channel2` / `Channel3` vector parameters.
///
/// The change lives inside `document/main.ron`, which this chain never sees:
/// it is deserialized into typed `Node` / `ParameterValue` values, and node
/// parameters are free key/value pairs, so a v4 `center_x` parses intact and
/// merely stops being read. The fold is therefore a typed pass over the
/// loaded document
/// ([`Document::fold_component_params`](ravel_core::composition::Document::fold_component_params)),
/// applied by [`super::ProjectFile::from_archive`] for any archive older than
/// v5. This step only advances the version stamp that gates it.
fn migrate_v4_to_v5(_manifest: &mut Value) -> Result<(), String> {
    Ok(())
}

/// `v5 → v6`: the manifest schema is unchanged. v6 stores a node's curve
/// control points as a structured
/// [`CurveParam`](ravel_core::param_curve::CurveParam) instead of the
/// `"0:0,1:1"` string `field.curve_remap` used to carry.
///
/// Like the v4 → v5 fold above, the change lives inside `document/main.ron`,
/// which this chain never sees: a v5 `points: String(..)` deserializes intact
/// and merely stops matching what the template declares. The conversion is a
/// typed pass over the loaded document
/// ([`Document::upgrade_curve_params`](ravel_core::composition::Document::upgrade_curve_params)),
/// applied by [`super::ProjectFile::from_archive`] for any archive older than
/// v6. This step only advances the version stamp that gates it.
fn migrate_v5_to_v6(_manifest: &mut Value) -> Result<(), String> {
    Ok(())
}

/// `v6 → v7`: the manifest schema is unchanged. v7 adds the project's exposed
/// parameter declarations —
/// [`Document::exposed_parameters`](ravel_core::composition::Document::exposed_parameters),
/// the external contract of REQ-PROJ-006 — to `document/main.ron`.
///
/// **There is no typed pass for this step**, unlike v4 → v5 and v5 → v6 above.
/// Those two changed how an *existing* value is represented, so a loaded
/// document had to be rewritten before it meant the same thing. v7 only adds a
/// field: `exposed_parameters` is `#[serde(default)]`, so a v6 document — which
/// has no such field — reads as a project with zero declarations, which is
/// exactly what it is. Nothing to convert, so this step only advances the
/// version stamp.
///
/// **Why the version was bumped at all**, given that `docs/dev/persistence.md`
/// says an additive field does not need one (`Layer.audio` did not get one):
/// the declarations are a contract other tools consume by name. An older build
/// silently drops what it cannot parse and writes the document back without
/// it, so opening a v7 project in an older Ravel and saving would delete the
/// contract while leaving the artwork intact — a loss the user has no way to
/// see. The bump turns that into [`MigrationError::TooNew`], a refusal to open.
fn migrate_v6_to_v7(_manifest: &mut Value) -> Result<(), String> {
    Ok(())
}

/// `v7 → v8`: the compositing pipeline became linear, so every authored
/// colour means something different.
///
/// The manifest itself is unchanged. The conversion is a **typed pass** over
/// the loaded document
/// ([`Document::linearize_colors`](ravel_core::composition::Document::linearize_colors)),
/// applied by [`super::ProjectFile::from_archive`] for any archive older than
/// v8 — the same shape as the v4 → v5 fold and the v5 → v6 curve upgrade, and
/// for the same reason: this chain never sees `document/main.ron`.
///
/// The bump is what makes the conversion happen exactly once. `srgb → linear`
/// is not idempotent and a stored number carries no record of how often it has
/// been applied, so the version stamp is the only thing standing between a
/// re-opened project and a second, silently wrong conversion. It also refuses
/// the reverse: a v8 project opened by an older, display-referred build would
/// render every colour too dark, and [`MigrationError::TooNew`] stops it.
fn migrate_v7_to_v8(_manifest: &mut Value) -> Result<(), String> {
    Ok(())
}

/// `v8 → v9`: a media asset is identified by a minted id, not by its name.
///
/// The manifest itself is unchanged. The conversion is a **typed pass** over
/// the loaded document
/// ([`Document::upgrade_asset_references`](ravel_core::composition::Document::upgrade_asset_references)),
/// applied by [`super::ProjectFile::from_archive`] for any archive older than
/// v9 — the same shape as the three passes above, and for the same reason:
/// this chain never sees `document/main.ron`. Up to v8 the key of
/// `Document::media_assets` *was* the asset's display string, and all three
/// reference systems (a `media` node's `asset_id` parameter, an `AudioSource`,
/// and the `media` node an exposed declaration binds to) held that string; v9
/// keys the table by `AssetId` and moves the string to
/// `MediaAssetEntry::name`, so every reference has to be rewritten before it
/// means the same thing (`docs/implementation/asset-identity-plan.md`).
///
/// **Why the version was bumped**, given that the pass could be driven by the
/// shape of the data instead — a string key is unmistakably pre-v9: because
/// the rewrite is *lossy on purpose*. A reference naming an asset the table
/// does not hold becomes `AssetId::UNSET`, permanently offline, rather than
/// keeping the name and reconnecting to whatever is imported under it next —
/// and that decision must be taken exactly once, on the archive that was
/// written before ids existed. Running it on a v9 document, where names are
/// free to repeat and to be edited, would take working references offline.
/// The bump also refuses the reverse: an older build opening a v9 project
/// would read no asset at all, and [`MigrationError::TooNew`] stops it before
/// it saves the references away.
fn migrate_v8_to_v9(_manifest: &mut Value) -> Result<(), String> {
    Ok(())
}

/// `v9 → v10`: `ParameterValue` gained the `IntChannel` variant — an
/// animatable integer, an f32 channel rounded on read (the discrete-keyframes
/// plan, unit 1).
///
/// **There is no typed pass**, and nothing to convert: an existing `Int` is
/// still an `Int`, still means the same number, and only becomes an
/// `IntChannel` when the user puts a keyframe on it. Like v6 → v7, this step
/// advances the version stamp and nothing else.
///
/// **Why the stamp moves at all**, given that `docs/dev/persistence.md` says
/// adding a `ParameterValue` variant needs only a `JOURNAL_FORMAT_VERSION`
/// bump: it is what the *absence* of a stamp would cost. Without one, an
/// older build reads the manifest happily and then cannot parse `IntChannel`
/// out of `document/main.ron`, and what it does with that failure is worse
/// than the failure.
/// [`ProjectError::DocumentParse`](super::ProjectError::DocumentParse) is
/// indistinguishable from a truncated or hand-mangled file, so
/// [`ProjectFile::load_with_backup`](super::ProjectFile::load_with_backup)
/// treats it as corruption and opens the `.bak` revision instead — an older
/// snapshot of the same project, which the next save writes over. The user
/// sees a project that opens, with recent work missing. With the stamp, that
/// build stops at the manifest instead — [`MigrationError::TooNew`], which
/// `load_with_backup` refuses to paper over with a backup
/// (`ProjectError::is_too_new`) — and never reaches the document at all.
fn migrate_v9_to_v10(_manifest: &mut Value) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

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

    /// A v3 archive's media assets are absolute `PathBuf`s; the manifest
    /// only advances its stamp, and the document upgrade happens at parse
    /// time in `ravel_core::composition::asset`.
    #[test]
    fn v3_migrates_to_v4_with_schema_unchanged() {
        let mut m = serde_json::json!({
            "format_version": 3,
            "ravel_version": "0.1.0",
            "project_name": "Assets",
            "created_at": "2026-07-01T00:00:00Z",
            "modified_at": "2026-07-02T00:00:00Z",
            "frame_rate": { "num": 24, "den": 1 },
            "resolution": { "width": 3840, "height": 2160 }
        });
        migrate_to_current(&mut m).unwrap();
        assert_eq!(read_version(&m).unwrap(), CURRENT_FORMAT_VERSION);
        assert_eq!(m["project_name"], Value::from("Assets"));
        assert_eq!(m["resolution"]["width"], Value::from(3840));
        let manifest: Manifest = serde_json::from_value(m).unwrap();
        assert_eq!(manifest.format_version, CURRENT_FORMAT_VERSION);
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
        assert_eq!(read_version(&m).unwrap(), CURRENT_FORMAT_VERSION);

        // Only the version stamp advanced; every other field is preserved.
        assert_eq!(m["project_name"], Value::from("Mid"));
        assert_eq!(m["resolution"]["width"], Value::from(1280));
        let manifest: Manifest = serde_json::from_value(m).unwrap();
        assert_eq!(manifest.format_version, CURRENT_FORMAT_VERSION);
    }

    /// v4, v5 and v6 change only what lives inside `document/main.ron`; the
    /// manifest advances its stamp and keeps every other field.
    #[test]
    fn v4_v5_and_v6_migrate_with_the_schema_unchanged() {
        for version in [4, 5, 6] {
            let mut m = serde_json::json!({
                "format_version": version,
                "ravel_version": "0.1.0",
                "project_name": "Params",
                "created_at": "2026-07-01T00:00:00Z",
                "modified_at": "2026-07-02T00:00:00Z",
                "frame_rate": { "num": 60, "den": 1 },
                "resolution": { "width": 1920, "height": 1080 }
            });
            migrate_to_current(&mut m).unwrap();
            assert_eq!(read_version(&m).unwrap(), CURRENT_FORMAT_VERSION);
            assert_eq!(m["project_name"], Value::from("Params"));
            assert_eq!(m["frame_rate"]["num"], Value::from(60));
            let manifest: Manifest = serde_json::from_value(m).unwrap();
            assert_eq!(manifest.format_version, CURRENT_FORMAT_VERSION);
        }
    }

    /// `v9 → v10` adds the `IntChannel` parameter variant, which lives in
    /// `document/main.ron`; the manifest must come through byte-identical
    /// apart from the stamp.
    #[test]
    fn v9_migration_changes_only_the_version_stamp() {
        let mut m = serde_json::json!({
            "format_version": 9,
            "ravel_version": "0.1.0",
            "project_name": "Ints",
            "created_at": "t",
            "modified_at": "t",
            "duration_frames": 120,
            "frame_rate": { "num": 30, "den": 1 },
            "resolution": { "width": 1920, "height": 1080 },
        });
        let mut expected = m.clone();
        expected["format_version"] = Value::from(CURRENT_FORMAT_VERSION);

        migrate_to_current(&mut m).unwrap();

        assert_eq!(m, expected);
    }

    /// `v6 → v7` must touch nothing but the stamp. Asserting on a handful of
    /// fields would still pass if the step started rewriting or dropping some
    /// other key, so compare the whole manifest against the input with only
    /// `format_version` advanced.
    #[test]
    fn v6_migration_changes_only_the_version_stamp() {
        let mut m = serde_json::json!({
            "format_version": 6,
            "ravel_version": "0.1.0",
            "project_name": "P",
            "created_at": "t",
            "modified_at": "t",
            "duration_frames": 120,
            "frame_rate": { "num": 30, "den": 1 },
            "resolution": { "width": 1920, "height": 1080 },
        });
        let mut expected = m.clone();
        expected["format_version"] = Value::from(CURRENT_FORMAT_VERSION);

        migrate_to_current(&mut m).unwrap();

        assert_eq!(m, expected);
    }

    /// Every version this build claims to read has a step registered, so a
    /// `.ravprj` from any past release still opens. Adding a version without
    /// its step fails here rather than in the field.
    #[test]
    fn every_past_version_reaches_the_current_format() {
        for version in 1..CURRENT_FORMAT_VERSION {
            let mut m = v1_manifest();
            m["format_version"] = Value::from(version);
            if version >= 2 {
                // v2 is where `resolution` became mandatory; only a v1 file
                // may arrive without it.
                m["resolution"] = serde_json::json!({ "width": 1920, "height": 1080 });
            }
            migrate_to_current(&mut m)
                .unwrap_or_else(|err| panic!("v{version} has no path to current: {err}"));
            assert_eq!(read_version(&m).unwrap(), CURRENT_FORMAT_VERSION);
            let manifest: Manifest = serde_json::from_value(m)
                .unwrap_or_else(|err| panic!("migrated v{version} is not typed: {err}"));
            assert_eq!(manifest.format_version, CURRENT_FORMAT_VERSION);
        }
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
