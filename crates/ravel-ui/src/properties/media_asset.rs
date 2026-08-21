// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property fields of a media asset, and the reverse mapping that applies a
//! field edit back onto its entry (REQ-UI-008, media-import plan unit 6).
//!
//! Two sections: what the probe found (read-only — the numbers describe the
//! file, not a setting) and the reference itself (the persisted path and which
//! of its three forms it is in).
//!
//! # What is *not* here
//!
//! - The display **name**. It is editable, but the MediaBin row owns that
//!   edit; a second editing site for one string is two places to keep in step
//!   for no gain, so the name is read-only here.
//! - The input **colour space** (`CM-8`).
//! - Anything that needs the file: existence, a re-probe, a thumbnail. This
//!   module is called on every document change, on the UI thread, so it
//!   touches no disk. "Offline" therefore means *the reference does not
//!   resolve* ([`MediaAssetEntry::is_offline`]) — a resolvable path whose file
//!   was deleted still reads as online here.
//!
//! # Path forms
//!
//! [`FIELD_PATH`] edits the persisted string directly and [`AssetPath::parse`]
//! classifies the result, so typing an absolute path into a relative reference
//! makes it absolute — the same rule the file on disk is read by.
//! [`FIELD_PATH_KIND`] is the other direction: it keeps the file and rewrites
//! the form, which is why it needs the project root. A form the current
//! location cannot express (relative to a project that has no root yet, or a
//! file outside it) is refused rather than approximated; the select then snaps
//! back to the form the entry still has.

use std::collections::HashMap;
use std::path::Path;

use ravel_core::composition::{AssetKind, AssetPath, MediaAssetEntry};

use super::{PropertyField, PropertySection, PropertyValue, counted_value};
use crate::panels::media_bin::{asset_name, format_duration};

/// Section titles, in display order.
pub const SECTION_ASSET: &str = "properties.section.media_asset";
pub const SECTION_FILE: &str = "properties.section.media_file";

/// Field keys. The ones that name a value every target shares (`name`,
/// `type`, `frame_rate`, `duration`) reuse the existing labels.
pub const FIELD_NAME: &str = "name";
pub const FIELD_KIND: &str = "type";
pub const FIELD_RESOLUTION: &str = "resolution";
pub const FIELD_FRAME_RATE: &str = "frame_rate";
pub const FIELD_DURATION: &str = "duration";
pub const FIELD_CODEC: &str = "codec";
pub const FIELD_AUDIO: &str = "audio";
pub const FIELD_RESOLVED: &str = "resolved_path";
pub const FIELD_PATH_KIND: &str = "path_kind";
pub const FIELD_PATH: &str = "path";

/// [`FIELD_KIND`] values. State words, so locale keys resolved at the display
/// boundary (`panels::properties::read_only_value`).
pub const KIND_CONTAINER: &str = "properties.media.kind_container";
pub const KIND_STILL: &str = "properties.media.kind_still";
/// Carries the frame count through [`counted_value`].
pub const KIND_SEQUENCE: &str = "properties.media.kind_sequence";

/// [`FIELD_PATH_KIND`] options, in menu order. These are *stored* values as
/// well as locale keys: the select answers with the translated label and the
/// host maps it back through the option list, so the language in use never
/// changes what an edit writes.
pub const PATH_ABSOLUTE: &str = "properties.media.path_absolute";
pub const PATH_RELATIVE: &str = "properties.media.path_relative";
pub const PATH_VARIABLE: &str = "properties.media.path_variable";

/// [`FIELD_RESOLVED`] when nothing resolves. The same word the MediaBin row
/// badges an offline asset with, so the two agree.
pub const VALUE_OFFLINE: &str = "media_bin.offline";

/// [`FIELD_AUDIO`] with a stream count substituted, and the value for a
/// silent file.
pub const VALUE_AUDIO_STREAMS: &str = "properties.value.audio_streams";
pub const VALUE_NONE: &str = "properties.value.none";

/// The variable form the kind switch produces. `${PROJECT_ROOT}` is the one
/// substitution [`AssetPath::resolve`] always supplies, so it is the only one
/// a path built here can be sure of.
const PROJECT_ROOT_TOKEN: &str = "${PROJECT_ROOT}";

/// The sections the Properties panel shows for `PropertiesTarget::MediaAsset`.
pub fn sections_for_media_asset(entry: &MediaAssetEntry) -> Vec<PropertySection> {
    vec![
        PropertySection {
            title: SECTION_ASSET.into(),
            fields: probe_fields(entry),
        },
        PropertySection {
            title: SECTION_FILE.into(),
            fields: reference_fields(entry),
        },
    ]
}

/// What the import probe recorded. Every metadata field is optional
/// (persistence never probes), and an absent one gets no row rather than a
/// row saying nothing.
fn probe_fields(entry: &MediaAssetEntry) -> Vec<PropertyField> {
    let metadata = &entry.metadata;
    let mut fields = vec![
        read_only(FIELD_NAME, asset_name(entry)),
        read_only(FIELD_KIND, kind_value(&entry.kind)),
    ];
    if let (Some(width), Some(height)) = (metadata.width, metadata.height) {
        fields.push(read_only(FIELD_RESOLUTION, format!("{width} × {height}")));
    }
    if let Some(frame_rate) = metadata.frame_rate {
        fields.push(read_only(FIELD_FRAME_RATE, format_fps(frame_rate)));
    }
    if let Some(duration) = metadata.duration_secs {
        fields.push(read_only(FIELD_DURATION, format_duration(duration)));
    }
    if let Some(codec) = &metadata.codec {
        fields.push(read_only(FIELD_CODEC, codec.clone()));
    }
    let audio_streams = metadata
        .audio_streams
        .len()
        .max(metadata.audio_stream_count);
    fields.push(read_only(
        FIELD_AUDIO,
        if audio_streams == 0 {
            VALUE_NONE.to_string()
        } else {
            counted_value(VALUE_AUDIO_STREAMS, audio_streams as u64)
        },
    ));
    fields
}

/// The reference: where it lands today, which form it is stored in, and the
/// stored string itself.
fn reference_fields(entry: &MediaAssetEntry) -> Vec<PropertyField> {
    vec![
        read_only(
            FIELD_RESOLVED,
            match &entry.resolved {
                Some(path) => path.to_string_lossy().into_owned(),
                None => VALUE_OFFLINE.to_string(),
            },
        ),
        PropertyField::Enum {
            key: FIELD_PATH_KIND.into(),
            value: path_kind_option(&entry.path).to_string(),
            options: vec![
                PATH_ABSOLUTE.to_string(),
                PATH_RELATIVE.to_string(),
                PATH_VARIABLE.to_string(),
            ],
        },
        PropertyField::String {
            key: FIELD_PATH.into(),
            value: entry.path.to_string(),
        },
    ]
}

/// Apply an edited field onto `entry`, keeping
/// [`MediaAssetEntry::resolved`] in step with the new reference. Returns
/// whether anything changed — an unknown key, a mismatched value type, and a
/// form the current location cannot express all change nothing, exactly as
/// the composition and layer mappings do.
///
/// `project_root` is the directory of the open `.ravprj` (`None` for a project
/// that has never been saved), which is what a relative or variable form is
/// measured against.
pub fn apply_media_asset_field(
    entry: &mut MediaAssetEntry,
    key: &str,
    value: &PropertyValue,
    project_root: Option<&Path>,
) -> bool {
    let path = match (key, value) {
        (FIELD_PATH, PropertyValue::String(text)) => {
            let text = text.trim();
            // A blank reference is not an edit: it resolves to the project
            // root itself and would lose the only record of where the file
            // was.
            if text.is_empty() {
                return false;
            }
            AssetPath::parse(text)
        }
        (FIELD_PATH_KIND, PropertyValue::String(option)) => {
            match converted_path(entry, option, project_root) {
                Some(path) => path,
                None => return false,
            }
        }
        _ => return false,
    };
    if path == entry.path {
        return false;
    }
    entry.path = path;
    entry.resolved = entry.path.resolve(project_root, &HashMap::new());
    true
}

/// Rewrite the entry's path into the form `option` names, keeping the file it
/// points at. `None` when the option is unknown or the location cannot be
/// written that way.
fn converted_path(
    entry: &MediaAssetEntry,
    option: &str,
    project_root: Option<&Path>,
) -> Option<AssetPath> {
    // The file the reference points at. `resolved` for an online asset; an
    // offline one can still be converted when its stored form resolves on its
    // own, which is what an absolute path always does.
    let absolute = match &entry.resolved {
        Some(path) => path.clone(),
        None => entry.path.resolve(project_root, &HashMap::new())?,
    };
    match option {
        PATH_ABSOLUTE => Some(AssetPath::Absolute(absolute)),
        PATH_RELATIVE => project_relative(&absolute, project_root).map(AssetPath::Relative),
        PATH_VARIABLE => project_relative(&absolute, project_root)
            .map(|rel| AssetPath::Variable(format!("{PROJECT_ROOT_TOKEN}/{}", strip_dot(&rel)))),
        _ => None,
    }
}

/// The project-relative spelling of `absolute`, or `None` when there is none —
/// an unsaved project, or a file that does not live under the project root.
fn project_relative(absolute: &Path, project_root: Option<&Path>) -> Option<String> {
    match AssetPath::for_project_root(absolute, project_root) {
        AssetPath::Relative(rel) => Some(rel),
        _ => None,
    }
}

fn strip_dot(rel: &str) -> &str {
    rel.strip_prefix("./").unwrap_or(rel)
}

/// Which [`FIELD_PATH_KIND`] option describes `path`.
fn path_kind_option(path: &AssetPath) -> &'static str {
    match path {
        AssetPath::Absolute(_) => PATH_ABSOLUTE,
        AssetPath::Relative(_) => PATH_RELATIVE,
        AssetPath::Variable(_) => PATH_VARIABLE,
    }
}

fn kind_value(kind: &AssetKind) -> String {
    match kind {
        AssetKind::Container => KIND_CONTAINER.to_string(),
        AssetKind::Still => KIND_STILL.to_string(),
        AssetKind::Sequence { .. } => {
            counted_value(KIND_SEQUENCE, kind.sequence_len().unwrap_or(0))
        }
    }
}

/// `29.97 fps` / `30 fps`. The unit symbol is deliberately not translated
/// (see the notation rule in `docs/dev/add-locale.md`).
fn format_fps(frame_rate: ravel_core::types::FrameRate) -> String {
    let fps = frame_rate.as_f64();
    if (fps - fps.round()).abs() < 0.000_5 {
        format!("{fps:.0} fps")
    } else {
        format!("{fps:.2} fps")
    }
}

fn read_only(key: &str, value: String) -> PropertyField {
    PropertyField::ReadOnly {
        key: key.into(),
        value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::AssetMetadata;
    use ravel_core::types::FrameRate;
    use std::path::PathBuf;

    fn entry(path: AssetPath, resolved: Option<&str>) -> MediaAssetEntry {
        MediaAssetEntry {
            name: "clip".into(),
            path,
            kind: AssetKind::Container,
            metadata: AssetMetadata::default(),
            color_space: None,
            exposed_owner: None,
            resolved: resolved.map(PathBuf::from),
        }
    }

    fn field<'a>(sections: &'a [PropertySection], key: &str) -> Option<&'a PropertyField> {
        sections
            .iter()
            .flat_map(|section| &section.fields)
            .find(|field| field.key() == key)
    }

    fn read_only_text(sections: &[PropertySection], key: &str) -> Option<String> {
        match field(sections, key)? {
            PropertyField::ReadOnly { value, .. } => Some(value.clone()),
            other => panic!("{key} is not read-only: {other:?}"),
        }
    }

    #[test]
    fn absent_metadata_produces_no_row() {
        let sections = sections_for_media_asset(&entry(
            AssetPath::Absolute("/f/clip.mov".into()),
            Some("/f/clip.mov"),
        ));
        for key in [
            FIELD_RESOLUTION,
            FIELD_FRAME_RATE,
            FIELD_DURATION,
            FIELD_CODEC,
        ] {
            assert!(field(&sections, key).is_none(), "{key} should have no row");
        }
        // The rows that always exist do.
        assert_eq!(
            read_only_text(&sections, FIELD_NAME).as_deref(),
            Some("clip")
        );
        assert_eq!(
            read_only_text(&sections, FIELD_KIND).as_deref(),
            Some(KIND_CONTAINER)
        );
        assert_eq!(
            read_only_text(&sections, FIELD_AUDIO).as_deref(),
            Some(VALUE_NONE)
        );
    }

    #[test]
    fn probed_metadata_is_shown_and_a_sequence_counts_its_frames() {
        let mut asset = entry(
            AssetPath::Relative("./seq/f0001.exr".into()),
            Some("/p/seq/f0001.exr"),
        );
        asset.kind = AssetKind::Sequence {
            prefix: "f".into(),
            suffix: ".exr".into(),
            padding: 4,
            start: 1,
            end: 10,
        };
        asset.metadata = AssetMetadata {
            width: Some(1920),
            height: Some(1080),
            frame_rate: Some(FrameRate::new(30_000, 1001)),
            duration_secs: Some(65.5),
            codec: Some("exr".into()),
            audio_stream_count: 2,
            ..AssetMetadata::default()
        };
        let sections = sections_for_media_asset(&asset);
        assert_eq!(
            read_only_text(&sections, FIELD_RESOLUTION).as_deref(),
            Some("1920 × 1080")
        );
        assert_eq!(
            read_only_text(&sections, FIELD_FRAME_RATE).as_deref(),
            Some("29.97 fps")
        );
        assert_eq!(
            read_only_text(&sections, FIELD_DURATION).as_deref(),
            Some("1:05.5")
        );
        assert_eq!(
            read_only_text(&sections, FIELD_KIND),
            Some(counted_value(KIND_SEQUENCE, 10)),
            "a sequence reports its frame count"
        );
        assert_eq!(
            read_only_text(&sections, FIELD_AUDIO),
            Some(counted_value(VALUE_AUDIO_STREAMS, 2))
        );
    }

    #[test]
    fn an_offline_reference_says_so_instead_of_naming_a_file() {
        let sections = sections_for_media_asset(&entry(
            AssetPath::Variable("${MEDIA}/clip.mov".into()),
            None,
        ));
        assert_eq!(
            read_only_text(&sections, FIELD_RESOLVED).as_deref(),
            Some(VALUE_OFFLINE)
        );
        // The stored string is still shown and still editable — it is the only
        // record of where the file was.
        assert!(matches!(
            field(&sections, FIELD_PATH),
            Some(PropertyField::String { value, .. }) if value == "${MEDIA}/clip.mov"
        ));
        assert!(matches!(
            field(&sections, FIELD_PATH_KIND),
            Some(PropertyField::Enum { value, .. }) if value == PATH_VARIABLE
        ));
    }

    /// The whole point of the kind switch: every form of one file, and every
    /// one of them resolving back to that file.
    #[test]
    fn the_kind_switch_rewrites_the_form_and_keeps_the_file() {
        let root = Path::new("/p");
        let file = "/p/footage/clip.mov";
        let mut asset = entry(AssetPath::Absolute(file.into()), Some(file));

        for (option, expected) in [
            (
                PATH_RELATIVE,
                AssetPath::Relative("./footage/clip.mov".into()),
            ),
            (
                PATH_VARIABLE,
                AssetPath::Variable("${PROJECT_ROOT}/footage/clip.mov".into()),
            ),
            (PATH_ABSOLUTE, AssetPath::Absolute(file.into())),
        ] {
            assert!(apply_media_asset_field(
                &mut asset,
                FIELD_PATH_KIND,
                &PropertyValue::String(option.into()),
                Some(root),
            ));
            assert_eq!(asset.path, expected, "switching to {option}");
            assert_eq!(
                asset.resolved.as_deref(),
                Some(Path::new(file)),
                "{option} still resolves to the same file"
            );
            // The same option twice is not a second edit.
            assert!(!apply_media_asset_field(
                &mut asset,
                FIELD_PATH_KIND,
                &PropertyValue::String(option.into()),
                Some(root),
            ));
        }
    }

    #[test]
    fn a_form_the_location_cannot_express_is_refused() {
        let root = Path::new("/p");
        let outside = "/elsewhere/clip.mov";
        let mut asset = entry(AssetPath::Absolute(outside.into()), Some(outside));
        for option in [PATH_RELATIVE, PATH_VARIABLE] {
            assert!(
                !apply_media_asset_field(
                    &mut asset,
                    FIELD_PATH_KIND,
                    &PropertyValue::String(option.into()),
                    Some(root),
                ),
                "{option} cannot describe a file outside the project"
            );
        }
        // An unsaved project has no root to measure against either.
        let mut asset = entry(
            AssetPath::Absolute("/p/clip.mov".into()),
            Some("/p/clip.mov"),
        );
        assert!(!apply_media_asset_field(
            &mut asset,
            FIELD_PATH_KIND,
            &PropertyValue::String(PATH_RELATIVE.into()),
            None,
        ));
        assert_eq!(asset.path, AssetPath::Absolute("/p/clip.mov".into()));
    }

    #[test]
    fn editing_the_path_reclassifies_it_and_re_resolves() {
        let root = Path::new("/p");
        let mut asset = entry(AssetPath::Absolute("/elsewhere/clip.mov".into()), None);

        // Typed as a relative reference: resolved against the project root.
        assert!(apply_media_asset_field(
            &mut asset,
            FIELD_PATH,
            &PropertyValue::String("  ./footage/clip.mov  ".into()),
            Some(root),
        ));
        assert_eq!(asset.path, AssetPath::Relative("./footage/clip.mov".into()));
        assert_eq!(
            asset.resolved.as_deref(),
            Some(Path::new("/p/footage/clip.mov"))
        );

        // A variable with no value left goes offline rather than resolving to
        // a directory called `${MEDIA}`.
        assert!(apply_media_asset_field(
            &mut asset,
            FIELD_PATH,
            &PropertyValue::String("${MEDIA}/clip.mov".into()),
            Some(root),
        ));
        assert_eq!(asset.path, AssetPath::Variable("${MEDIA}/clip.mov".into()));
        assert!(asset.is_offline());
    }

    #[test]
    fn unknown_keys_blank_paths_and_mismatched_types_change_nothing() {
        let mut asset = entry(
            AssetPath::Absolute("/f/clip.mov".into()),
            Some("/f/clip.mov"),
        );
        let before = asset.clone();
        assert!(!apply_media_asset_field(
            &mut asset,
            "nonexistent",
            &PropertyValue::String("/other".into()),
            None,
        ));
        assert!(!apply_media_asset_field(
            &mut asset,
            FIELD_PATH,
            &PropertyValue::Int(3),
            None,
        ));
        assert!(!apply_media_asset_field(
            &mut asset,
            FIELD_PATH,
            &PropertyValue::String("   ".into()),
            None,
        ));
        // The read-only rows route nothing.
        assert!(!apply_media_asset_field(
            &mut asset,
            FIELD_NAME,
            &PropertyValue::String("renamed".into()),
            None,
        ));
        assert_eq!(asset, before);
    }
}
