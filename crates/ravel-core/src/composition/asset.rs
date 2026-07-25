// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Media asset references (REQ-PROJ-001).
//!
//! A project never embeds media; it stores **references**. A reference is one
//! of three persisted forms — absolute, project-relative, or variable-prefixed
//! — and each resolves to an absolute [`PathBuf`] through
//! [`AssetPath::resolve`]. Resolution is the host application's job: it fills
//! [`MediaAssetEntry::resolved`] after a load, an import, or a `Save As` that
//! moves the project root. Evaluation reads `resolved` and nothing else, so a
//! node never has to know where the project lives.
//!
//! # Persisted form
//!
//! [`AssetPath`] serializes as a **single string** rather than a tagged enum:
//!
//! | Form | Example |
//! |---|---|
//! | [`AssetPath::Absolute`] | `"/Users/me/footage/clip.mov"` |
//! | [`AssetPath::Relative`] | `"./footage/clip.mov"` |
//! | [`AssetPath::Variable`] | `"${PROJECT_ROOT}/footage/clip.mov"` |
//!
//! The string form keeps `document/main.ron` readable and — crucially — makes
//! the format-v3 shape (`MediaAssetEntry { path: PathBuf }`, always absolute)
//! deserialize unchanged as [`AssetPath::Absolute`], so the v3 → v4 document
//! upgrade needs no text rewriting.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::types::FrameRate;

// ===========================================================================
// AssetPath
// ===========================================================================

/// Location of an asset's backing file, in the form it is persisted in.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AssetPath {
    /// An absolute location, used when the media lives outside the project
    /// root.
    Absolute(PathBuf),
    /// A path relative to the project root, e.g. `"./footage/clip.mov"`.
    Relative(String),
    /// A path containing `${NAME}` tokens, e.g.
    /// `"${PROJECT_ROOT}/footage/clip.mov"`. Set only when the user asks for
    /// it; save never rewrites a path into this form.
    Variable(String),
}

impl AssetPath {
    /// Classify a persisted string into one of the three forms.
    ///
    /// The order matters: a `${` token wins over everything (a variable may
    /// expand to an absolute prefix), then absoluteness, then relative.
    pub fn parse(text: &str) -> Self {
        if text.contains("${") {
            AssetPath::Variable(text.to_string())
        } else if is_absolute_any_platform(text) {
            AssetPath::Absolute(PathBuf::from(text))
        } else {
            AssetPath::Relative(text.to_string())
        }
    }

    /// Build the form that best describes `absolute` for a project stored
    /// under `project_root`: relative when the file lives inside the project,
    /// absolute otherwise.
    ///
    /// [`AssetPath::Variable`] is never produced here — a variable path is a
    /// deliberate user choice and is preserved by
    /// [`MediaAssetEntry::relativized`] instead of being recomputed.
    pub fn for_project_root(absolute: &Path, project_root: Option<&Path>) -> Self {
        if let Some(root) = project_root
            && let Ok(rel) = absolute.strip_prefix(root)
        {
            let rel = rel.to_string_lossy().replace('\\', "/");
            if !rel.is_empty() {
                return AssetPath::Relative(format!("./{rel}"));
            }
        }
        AssetPath::Absolute(absolute.to_path_buf())
    }

    /// Resolve this path to an absolute location.
    ///
    /// `project_root` anchors [`AssetPath::Relative`] and is exposed to
    /// variable paths as `${PROJECT_ROOT}`; `vars` supplies additional
    /// substitutions and takes precedence over the implicit `PROJECT_ROOT`.
    /// Returns `None` when the result is still not absolute — an unsaved
    /// project has no root to anchor a relative path against, and that is
    /// "offline", not a panic.
    pub fn resolve(
        &self,
        project_root: Option<&Path>,
        vars: &HashMap<String, String>,
    ) -> Option<PathBuf> {
        match self {
            AssetPath::Absolute(path) => Some(path.clone()),
            AssetPath::Relative(rel) => project_root.map(|root| root.join(strip_leading_dot(rel))),
            AssetPath::Variable(template) => {
                let mut table = HashMap::new();
                if let Some(root) = project_root {
                    table.insert(
                        "PROJECT_ROOT".to_string(),
                        root.to_string_lossy().into_owned(),
                    );
                }
                for (key, value) in vars {
                    table.insert(key.clone(), value.clone());
                }
                let expanded = expand_variables(template, &table);
                // An unexpanded token would silently become a directory name.
                if expanded.contains("${") {
                    return None;
                }
                if is_absolute_any_platform(&expanded) {
                    Some(PathBuf::from(expanded))
                } else {
                    project_root.map(|root| root.join(strip_leading_dot(&expanded)))
                }
            }
        }
    }
}

impl fmt::Display for AssetPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssetPath::Absolute(path) => f.write_str(&path.to_string_lossy()),
            AssetPath::Relative(rel) | AssetPath::Variable(rel) => f.write_str(rel),
        }
    }
}

impl Serialize for AssetPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AssetPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(AssetPath::parse(&text))
    }
}

/// Whether `text` is an absolute path on **any** supported platform.
///
/// `Path::is_absolute` answers for the host only, which would silently demote
/// a Windows path read on macOS (or a POSIX path read on Windows) to
/// [`AssetPath::Relative`] and resolve it against the wrong root. Projects
/// move between platforms, so the classification must not.
fn is_absolute_any_platform(text: &str) -> bool {
    if text.starts_with('/') || text.starts_with("\\\\") {
        return true;
    }
    // Drive-letter prefix: `C:\…` or `C:/…`.
    let bytes = text.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn strip_leading_dot(rel: &str) -> &str {
    rel.strip_prefix("./").unwrap_or(rel)
}

/// Expand `${NAME}` tokens in `input` using `vars`.
///
/// Unknown tokens are left verbatim so that resolution is lossless and
/// debuggable rather than silently dropping path segments. The scan is
/// single-pass and never panics on unbalanced braces.
pub fn expand_variables(input: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'{'
            && let Some(end_rel) = input[i + 2..].find('}')
        {
            let end = i + 2 + end_rel;
            let name = &input[i + 2..end];
            match vars.get(name) {
                Some(value) => out.push_str(value),
                // Preserve the original token when unknown.
                None => out.push_str(&input[i..=end]),
            }
            i = end + 1;
            continue;
        }
        // Copy one UTF-8 character intact.
        let ch_len = utf8_char_len(bytes[i]);
        out.push_str(&input[i..i + ch_len]);
        i += ch_len;
    }
    out
}

fn utf8_char_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

// ===========================================================================
// AssetKind
// ===========================================================================

/// What kind of media an asset holds. One `media` node handles all three
/// (see `docs/implementation/media-import-plan.md`, decision 2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetKind {
    /// A container FFmpeg can open: video with any number of audio streams,
    /// or an audio-only file.
    #[default]
    Container,
    /// A single image. The decoded frame is cached inside the processor.
    Still,
    /// A numbered image sequence. The asset's [`AssetPath`] points at the
    /// representative (first) frame; the individual frames are rebuilt from
    /// these fields by [`AssetKind::sequence_frame_name`].
    Sequence {
        prefix: String,
        suffix: String,
        padding: usize,
        start: u64,
        end: u64,
    },
}

/// Extensions treated as single images rather than FFmpeg containers.
const STILL_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "bmp", "tga", "tif", "tiff", "webp", "gif", "exr", "hdr", "dds", "ico",
    "pnm", "ppm", "pgm", "pbm", "qoi", "avif",
];

impl AssetKind {
    /// Guess a kind from a file extension. Used when upgrading a format-v3
    /// document, whose asset table records only a path.
    ///
    /// Sequences are never guessed: detecting one needs a directory listing,
    /// which persistence must not perform.
    pub fn infer_from_path(path: &Path) -> Self {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if STILL_EXTENSIONS.contains(&ext.as_str()) {
            AssetKind::Still
        } else {
            AssetKind::Container
        }
    }

    /// File name of sequence frame `index` (absolute within the sequence's
    /// own numbering, not an offset from `start`). `None` for non-sequences
    /// and for indices outside `start..=end`.
    pub fn sequence_frame_name(&self, index: u64) -> Option<String> {
        match self {
            AssetKind::Sequence {
                prefix,
                suffix,
                padding,
                start,
                end,
            } => {
                if index < *start || index > *end {
                    return None;
                }
                Some(format!("{prefix}{index:0width$}{suffix}", width = *padding))
            }
            _ => None,
        }
    }

    /// Number of frames in a sequence; `None` for the other kinds.
    pub fn sequence_len(&self) -> Option<u64> {
        match self {
            AssetKind::Sequence { start, end, .. } => Some(end.saturating_sub(*start) + 1),
            _ => None,
        }
    }
}

// ===========================================================================
// AssetMetadata
// ===========================================================================

/// Decoded metadata cached alongside the reference so the media bin can list
/// an asset without touching the file. Every field is optional: persistence
/// never probes, so a freshly upgraded v3 document carries an empty record
/// until something fills it in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AssetMetadata {
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub frame_rate: Option<FrameRate>,
    #[serde(default)]
    pub duration_secs: Option<f64>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub color_space: Option<String>,
    /// Number of audio streams in the container (0 for silent media).
    #[serde(default)]
    pub audio_stream_count: usize,
    #[serde(default)]
    pub file_size: u64,
}

impl AssetMetadata {
    pub fn has_audio(&self) -> bool {
        self.audio_stream_count > 0
    }
}

// ===========================================================================
// MediaAssetEntry
// ===========================================================================

/// One entry of [`Document::media_assets`](crate::composition::Document).
///
/// `path`, `kind`, and `metadata` are persisted; `resolved` is not. The host
/// injects `resolved` whenever the mapping from persisted path to disk
/// location can change — after a load, an import, or a `Save As`. A `None`
/// `resolved` means **offline**: the `media` node yields a transparent frame
/// instead of failing the whole evaluation.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MediaAssetEntry {
    pub path: AssetPath,
    pub kind: AssetKind,
    pub metadata: AssetMetadata,
    #[serde(skip)]
    pub resolved: Option<PathBuf>,
}

/// Deserialization shadow of [`MediaAssetEntry`].
///
/// `kind` is an `Option` rather than `#[serde(default)]` so a format-v3 entry
/// — which has only `path` — can infer its kind from the file extension,
/// which a plain `Default` cannot see.
/// The name must match [`MediaAssetEntry`]: the document is written with
/// RON's `struct_names` enabled, so the serialized form carries it.
#[derive(Deserialize)]
#[serde(rename = "MediaAssetEntry")]
struct MediaAssetEntryRepr {
    path: AssetPath,
    #[serde(default, deserialize_with = "deserialize_present_kind")]
    kind: Option<AssetKind>,
    #[serde(default)]
    metadata: AssetMetadata,
}

/// Read a **bare** [`AssetKind`] into `Some`.
///
/// The field is persisted unwrapped (`kind: Still`), so the `Option` here
/// only distinguishes present from absent. Deserializing it as an `Option`
/// directly would demand RON's `Some(…)` wrapper and reject every file this
/// module writes.
fn deserialize_present_kind<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<AssetKind>, D::Error> {
    AssetKind::deserialize(deserializer).map(Some)
}

impl<'de> Deserialize<'de> for MediaAssetEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let repr = MediaAssetEntryRepr::deserialize(deserializer)?;
        let kind = repr.kind.unwrap_or_else(|| match &repr.path {
            AssetPath::Absolute(path) => AssetKind::infer_from_path(path),
            AssetPath::Relative(rel) | AssetPath::Variable(rel) => {
                AssetKind::infer_from_path(Path::new(rel))
            }
        });
        Ok(MediaAssetEntry {
            path: repr.path,
            kind,
            metadata: repr.metadata,
            // Never persisted: the host re-injects it after the load.
            resolved: None,
        })
    }
}

impl MediaAssetEntry {
    /// An entry for a file whose absolute location is already known — the
    /// import path and every test fixture. The persisted form starts out
    /// absolute; [`MediaAssetEntry::relativized`] narrows it at save time.
    pub fn from_absolute(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            kind: AssetKind::infer_from_path(&path),
            metadata: AssetMetadata::default(),
            resolved: Some(path.clone()),
            path: AssetPath::Absolute(path),
        }
    }

    /// Whether this asset currently has no location on disk.
    pub fn is_offline(&self) -> bool {
        self.resolved.is_none()
    }

    /// A copy with `resolved` recomputed from the persisted path.
    pub fn resolved_against(
        &self,
        project_root: Option<&Path>,
        vars: &HashMap<String, String>,
    ) -> Self {
        Self {
            resolved: self.path.resolve(project_root, vars),
            ..self.clone()
        }
    }

    /// A copy whose persisted path describes `project_root`.
    ///
    /// The absolute location in `resolved` is the source of truth, so saving
    /// the same document into a different directory rewrites the stored path
    /// rather than leaving it pointing at the old root. Two forms are left
    /// alone: [`AssetPath::Variable`], which the user set deliberately, and
    /// an offline entry, whose stored path is the only record of where the
    /// file was.
    pub fn relativized(&self, project_root: Option<&Path>) -> Self {
        match (&self.path, &self.resolved) {
            (AssetPath::Variable(_), _) | (_, None) => self.clone(),
            (_, Some(absolute)) => Self {
                path: AssetPath::for_project_root(absolute, project_root),
                ..self.clone()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parse_classifies_the_three_forms() {
        assert_eq!(
            AssetPath::parse("/abs/clip.mov"),
            AssetPath::Absolute(PathBuf::from("/abs/clip.mov"))
        );
        assert_eq!(
            AssetPath::parse("./footage/clip.mov"),
            AssetPath::Relative("./footage/clip.mov".into())
        );
        assert_eq!(
            AssetPath::parse("${PROJECT_ROOT}/a.mov"),
            AssetPath::Variable("${PROJECT_ROOT}/a.mov".into())
        );
    }

    #[test]
    fn windows_paths_stay_absolute_on_every_host() {
        assert_eq!(
            AssetPath::parse(r"C:\media\clip.mov"),
            AssetPath::Absolute(PathBuf::from(r"C:\media\clip.mov"))
        );
        assert_eq!(
            AssetPath::parse(r"\\share\media\clip.mov"),
            AssetPath::Absolute(PathBuf::from(r"\\share\media\clip.mov"))
        );
        assert_eq!(
            AssetPath::parse("/media/clip.mov"),
            AssetPath::Absolute(PathBuf::from("/media/clip.mov"))
        );
    }

    #[test]
    fn relative_resolves_against_the_project_root() {
        let p = AssetPath::Relative("./footage/clip.mov".into());
        assert_eq!(
            p.resolve(Some(Path::new("/projects/demo")), &HashMap::new()),
            Some(PathBuf::from("/projects/demo/footage/clip.mov"))
        );
    }

    #[test]
    fn relative_without_a_root_is_offline() {
        let p = AssetPath::Relative("./footage/clip.mov".into());
        assert_eq!(p.resolve(None, &HashMap::new()), None);
    }

    #[test]
    fn variable_expands_project_root_and_custom_vars() {
        let p = AssetPath::Variable("${PROJECT_ROOT}/footage/bg.mov".into());
        assert_eq!(
            p.resolve(Some(Path::new("/abs/proj")), &HashMap::new()),
            Some(PathBuf::from("/abs/proj/footage/bg.mov"))
        );

        let p = AssetPath::Variable("${MEDIA}/a/b.mov".into());
        assert_eq!(
            p.resolve(Some(Path::new("/proj")), &vars(&[("MEDIA", "/mnt/media")])),
            Some(PathBuf::from("/mnt/media/a/b.mov"))
        );
    }

    #[test]
    fn unresolvable_variable_is_offline_rather_than_a_literal_directory() {
        let p = AssetPath::Variable("${NOPE}/a.mov".into());
        assert_eq!(p.resolve(Some(Path::new("/proj")), &HashMap::new()), None);
    }

    #[test]
    fn expand_handles_unbalanced_braces_and_multibyte_text() {
        assert_eq!(expand_variables("${NOPE", &HashMap::new()), "${NOPE");
        assert_eq!(
            expand_variables("plain $ text", &HashMap::new()),
            "plain $ text"
        );
        assert_eq!(
            expand_variables("日本語${X}テキスト", &vars(&[("X", "値")])),
            "日本語値テキスト"
        );
    }

    #[test]
    fn for_project_root_prefers_relative_inside_the_project() {
        let inside = Path::new("/proj/footage/clip.mov");
        assert_eq!(
            AssetPath::for_project_root(inside, Some(Path::new("/proj"))),
            AssetPath::Relative("./footage/clip.mov".into())
        );

        let outside = Path::new("/elsewhere/clip.mov");
        assert_eq!(
            AssetPath::for_project_root(outside, Some(Path::new("/proj"))),
            AssetPath::Absolute(outside.to_path_buf())
        );
        assert_eq!(
            AssetPath::for_project_root(inside, None),
            AssetPath::Absolute(inside.to_path_buf())
        );
    }

    #[test]
    fn relativize_then_resolve_round_trips_through_a_moved_root() {
        let entry = MediaAssetEntry::from_absolute("/old/proj/footage/clip.mov");
        let saved = entry.relativized(Some(Path::new("/old/proj")));
        assert_eq!(saved.path, AssetPath::Relative("./footage/clip.mov".into()));

        // The whole project directory moves; the same stored path resolves
        // against the new root.
        let reopened = saved.resolved_against(Some(Path::new("/new/place")), &HashMap::new());
        assert_eq!(
            reopened.resolved,
            Some(PathBuf::from("/new/place/footage/clip.mov"))
        );
    }

    #[test]
    fn relativize_preserves_variable_paths_and_offline_entries() {
        let variable = MediaAssetEntry {
            path: AssetPath::Variable("${MEDIA}/a.mov".into()),
            kind: AssetKind::Container,
            metadata: AssetMetadata::default(),
            resolved: Some(PathBuf::from("/proj/a.mov")),
        };
        assert_eq!(
            variable.relativized(Some(Path::new("/proj"))).path,
            AssetPath::Variable("${MEDIA}/a.mov".into()),
        );

        let offline = MediaAssetEntry {
            path: AssetPath::Relative("./gone.mov".into()),
            kind: AssetKind::Container,
            metadata: AssetMetadata::default(),
            resolved: None,
        };
        assert_eq!(
            offline.relativized(Some(Path::new("/other"))).path,
            AssetPath::Relative("./gone.mov".into()),
        );
    }

    #[test]
    fn kind_is_inferred_from_the_extension() {
        assert_eq!(
            AssetKind::infer_from_path(Path::new("/a/b.PNG")),
            AssetKind::Still
        );
        assert_eq!(
            AssetKind::infer_from_path(Path::new("/a/b.mov")),
            AssetKind::Container
        );
        assert_eq!(
            AssetKind::infer_from_path(Path::new("/a/b")),
            AssetKind::Container
        );
    }

    #[test]
    fn sequence_frame_names_are_zero_padded_and_range_checked() {
        let kind = AssetKind::Sequence {
            prefix: "frame_".into(),
            suffix: ".png".into(),
            padding: 4,
            start: 10,
            end: 12,
        };
        assert_eq!(
            kind.sequence_frame_name(10).as_deref(),
            Some("frame_0010.png")
        );
        assert_eq!(
            kind.sequence_frame_name(12).as_deref(),
            Some("frame_0012.png")
        );
        assert_eq!(kind.sequence_frame_name(9), None);
        assert_eq!(kind.sequence_frame_name(13), None);
        assert_eq!(kind.sequence_len(), Some(3));
        assert_eq!(AssetKind::Still.sequence_len(), None);
    }

    #[test]
    fn entry_round_trips_through_ron() {
        let entry = MediaAssetEntry {
            path: AssetPath::Relative("./footage/clip.mov".into()),
            kind: AssetKind::Sequence {
                prefix: "f_".into(),
                suffix: ".exr".into(),
                padding: 5,
                start: 1,
                end: 240,
            },
            metadata: AssetMetadata {
                width: Some(1920),
                height: Some(1080),
                frame_rate: Some(FrameRate::new(24, 1)),
                duration_secs: Some(10.0),
                codec: Some("h264".into()),
                color_space: Some("sRGB".into()),
                audio_stream_count: 1,
                file_size: 1234,
            },
            resolved: Some(PathBuf::from("/proj/footage/clip.mov")),
        };
        let text = ron::to_string(&entry).unwrap();
        let back: MediaAssetEntry = ron::from_str(&text).unwrap();

        // `resolved` is deliberately not persisted.
        assert_eq!(back.resolved, None);
        assert_eq!(
            back,
            MediaAssetEntry {
                resolved: None,
                ..entry
            }
        );
    }

    /// The format-v3 shape — `MediaAssetEntry { path: PathBuf }` — must load
    /// as an absolute reference with an inferred kind and empty metadata.
    #[test]
    fn legacy_v3_entry_upgrades_in_place() {
        let legacy = r#"(path: "/abs/footage/clip.mov")"#;
        let entry: MediaAssetEntry = ron::from_str(legacy).unwrap();
        assert_eq!(
            entry.path,
            AssetPath::Absolute(PathBuf::from("/abs/footage/clip.mov"))
        );
        assert_eq!(entry.kind, AssetKind::Container);
        assert_eq!(entry.metadata, AssetMetadata::default());
        assert!(entry.is_offline(), "resolution happens after the load");

        let legacy_still = r#"(path: "/abs/plate.exr")"#;
        let entry: MediaAssetEntry = ron::from_str(legacy_still).unwrap();
        assert_eq!(entry.kind, AssetKind::Still);
    }

    #[test]
    fn asset_path_serializes_as_a_plain_string() {
        assert_eq!(
            ron::to_string(&AssetPath::Relative("./a.mov".into())).unwrap(),
            "\"./a.mov\""
        );
        assert_eq!(
            ron::to_string(&AssetPath::Absolute(PathBuf::from("/a.mov"))).unwrap(),
            "\"/a.mov\""
        );
    }
}
