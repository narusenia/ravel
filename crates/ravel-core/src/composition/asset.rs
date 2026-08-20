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
//! Classification reads absoluteness first and only treats a **leading**
//! `${` as a variable, so `Display` and `parse` are inverses: `${` is legal
//! inside a file name, and a variable must supply the leading path component.
//!
//! The string form keeps `document/main.ron` readable and — crucially — makes
//! the format-v3 shape (`MediaAssetEntry { path: PathBuf }`, always absolute)
//! deserialize unchanged as [`AssetPath::Absolute`], so the v3 → v4 document
//! upgrade needs no text rewriting.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::color::ColorSpace;
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
    /// Absoluteness is decided **first**, and a variable is recognised only
    /// when the string *starts* with `${`. Both restrictions exist to keep
    /// `parse(path.to_string()) == path`: `${` is legal inside a file name,
    /// so treating it as a variable marker anywhere would silently turn
    /// `/footage/a${b.mov` into an unresolvable reference. A variable must
    /// therefore supply the leading path component, which is the only form
    /// the model ever writes (`${PROJECT_ROOT}/…`).
    pub fn parse(text: &str) -> Self {
        if is_absolute_any_platform(text) {
            AssetPath::Absolute(PathBuf::from(text))
        } else if text.starts_with("${") {
            AssetPath::Variable(text.to_string())
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
            // Join through `Component`s rather than replacing `\` in the
            // whole string: on POSIX a backslash is an ordinary character
            // in a file name, and rewriting it would split one file into
            // two directory levels. `components()` splits on the host's
            // real separator, so the stored form is `/`-joined on every
            // platform without corrupting a name.
            let rel = rel
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
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

/// One audio stream of a container, cached so a stream picker can list what
/// the file holds without reopening it.
///
/// `stream_index` is the index **inside the container** — the value
/// [`AudioSource::stream_index`](crate::composition::AudioSource) carries and
/// the decoder seeks by — not the ordinal among the audio streams. A clip
/// with video on stream 0 and audio on stream 1 records `stream_index: 1`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioStreamMetadata {
    #[serde(default)]
    pub stream_index: usize,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub sample_rate: u32,
    #[serde(default)]
    pub channels: u32,
}

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
    /// The container's audio streams, in container order. Added after
    /// format v4 shipped, so a document written before it carries an empty
    /// list while `audio_stream_count` still records how many streams the
    /// file had.
    #[serde(default)]
    pub audio_streams: Vec<AudioStreamMetadata>,
    #[serde(default)]
    pub file_size: u64,
}

impl AssetMetadata {
    pub fn has_audio(&self) -> bool {
        self.audio_stream_count > 0 || !self.audio_streams.is_empty()
    }

    /// Container index of the stream a new audio source should play: the
    /// first audio stream of the container.
    ///
    /// `None` for silent media **and** for an older document whose metadata
    /// records only `audio_stream_count`: the container index of an audio
    /// stream cannot be derived from a count, and guessing `0` would pick
    /// the video stream of every muxed clip.
    pub fn first_audio_stream_index(&self) -> Option<usize> {
        self.audio_streams.first().map(|stream| stream.stream_index)
    }
}

// ===========================================================================
// MediaAssetEntry
// ===========================================================================

/// The parameter a `media` node holds its asset reference in.
///
/// Named once, here, because three places have to agree on it: the node
/// template that declares it, the processor that reads it, and the exposed
/// parameter contract that is allowed to write it. A fourth — the "which
/// layers use this asset?" query behind the delete confirmation — has to find
/// it too.
pub const MEDIA_ASSET_PARAM_KEY: &str = "asset_id";

/// The node type keys whose [`MEDIA_ASSET_PARAM_KEY`] parameter is an asset
/// reference.
///
/// `"video"` is the pre-normalization alias
/// (`Document::normalize_node_type_aliases`): a loaded document only holds
/// `"media"`, but a document assembled in memory can still carry the old key.
pub const MEDIA_TYPE_KEYS: [&str; 2] = ["media", "video"];

/// The display name a freshly imported file starts out with: its file stem.
///
/// Shared with the import path so the name an asset is created with and the
/// name uniqueness is checked against are derived the same way. A path with no
/// usable stem still has to be called something, and `"asset"` is what the
/// import path has always fallen back to.
pub fn name_from_path(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "asset".to_string())
}

/// One entry of [`Document::media_assets`](crate::composition::Document).
///
/// `name`, `path`, `kind`, and `metadata` are persisted; `resolved` is not.
/// The host injects `resolved` whenever the mapping from persisted path to
/// disk location can change — after a load, an import, or a `Save As`. A
/// `None` `resolved` means **offline**: the `media` node yields a transparent
/// frame instead of failing the whole evaluation.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MediaAssetEntry {
    /// What the asset is called in the UI. **Nothing references an asset by
    /// this**: the map key is an
    /// [`AssetId`](crate::id::AssetId), so the name can be edited and can
    /// repeat without any reference changing meaning
    /// (`docs/implementation/asset-identity-plan.md`).
    ///
    /// Added in `.ravprj` v9, where it took over the string that used to be
    /// the key. `default` so a v8 entry still deserializes; the v8 → v9
    /// upgrade then fills it with that former key, which is why the upgrade
    /// renames nothing.
    #[serde(default)]
    pub name: String,
    pub path: AssetPath,
    pub kind: AssetKind,
    pub metadata: AssetMetadata,
    /// The colour space the file's samples are in, **set explicitly by the
    /// user**. `None` — the only value anything writes today — means "infer
    /// it", and [`MediaAssetEntry::input_color_space`] then reads the
    /// metadata and finally the extension. Explicit always wins: a `.exr`
    /// really can carry sRGB-encoded values, and the person who knows that
    /// must be able to say so (`CM-2`; the UI to set it is `CM-8`).
    #[serde(default)]
    pub color_space: Option<ColorSpace>,
    /// The exposed declaration that created this entry, by declaration name
    /// (`ravel_core::exposed::apply`). `None` for everything else — every
    /// asset the import path mints.
    ///
    /// Ownership is this field and **never the name**: applying the same media
    /// value twice has to land on the same entry, and since `name` became
    /// editable and repeatable (`AID-3`) a derived name can neither be looked
    /// up reliably nor trusted. A user who renames an asset to `exposed:foo`
    /// must not thereby hand the declaration `foo` the right to overwrite that
    /// asset's path.
    ///
    /// Added after `.ravprj` v9 without a format bump, like [`Layer::audio`]
    /// and [`Composition::guides`](super::Composition::guides): an older file
    /// has no declaration-owned entries except the pre-v9 ones the v8 → v9
    /// upgrade recognises by their `exposed:` name.
    ///
    /// [`Layer::audio`]: super::Layer::audio
    #[serde(default)]
    pub exposed_owner: Option<String>,
    #[serde(skip)]
    pub resolved: Option<PathBuf>,
}

/// Which tier of the resolution order supplied an asset's input colour
/// space. Tiers 2 and 3 are *guesses*, and the media node logs which one it
/// used so a wrong-looking clip can be traced to the guess that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorSpaceSource {
    /// The user said so (`MediaAssetEntry::color_space`).
    Explicit,
    /// Read from the file's own metadata (`AssetMetadata::color_space`).
    Metadata,
    /// Guessed from the file extension.
    ExtensionDefault,
}

/// Extensions whose samples are floating point, and therefore already
/// scene-linear unless something says otherwise.
///
/// Everything else — PNG, JPEG, TIFF, DPX, and every container FFmpeg opens
/// — is an integer format, and an integer format that does not declare a
/// colour space is sRGB in practice. TIFF and DPX can hold float or log
/// data; they are deliberately *not* listed, because guessing linear for a
/// display-referred file double-brightens it, while guessing sRGB for a
/// linear file is the milder error and is the one the user can correct.
const LINEAR_EXTENSIONS: &[&str] = &["exr", "hdr"];

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
    #[serde(default)]
    name: String,
    path: AssetPath,
    #[serde(default, deserialize_with = "deserialize_present_kind")]
    kind: Option<AssetKind>,
    #[serde(default)]
    metadata: AssetMetadata,
    #[serde(default)]
    color_space: Option<ColorSpace>,
    #[serde(default)]
    exposed_owner: Option<String>,
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
            name: repr.name,
            path: repr.path,
            kind,
            metadata: repr.metadata,
            color_space: repr.color_space,
            exposed_owner: repr.exposed_owner,
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
            name: name_from_path(&path),
            kind: AssetKind::infer_from_path(&path),
            metadata: AssetMetadata::default(),
            color_space: None,
            exposed_owner: None,
            resolved: Some(path.clone()),
            path: AssetPath::Absolute(path),
        }
    }

    /// Whether this asset currently has no location on disk.
    pub fn is_offline(&self) -> bool {
        self.resolved.is_none()
    }

    /// The colour space this asset's samples are in, and which tier of the
    /// resolution order said so.
    ///
    /// The order is fixed (`CM-2`): the user's explicit setting, then the
    /// file's own metadata, then the extension. It never returns "unknown" —
    /// decoding has to put the values *somewhere*, and a wrong guess that is
    /// reported is recoverable while a refusal to decode is not.
    pub fn input_color_space(&self) -> (ColorSpace, ColorSpaceSource) {
        if let Some(explicit) = self.color_space {
            return (explicit, ColorSpaceSource::Explicit);
        }
        if let Some(from_metadata) = self
            .metadata
            .color_space
            .as_deref()
            .and_then(ColorSpace::from_name)
        {
            return (from_metadata, ColorSpaceSource::Metadata);
        }
        (
            self.extension_color_space(),
            ColorSpaceSource::ExtensionDefault,
        )
    }

    /// Tier 3: float formats are scene-linear, integer formats are sRGB.
    fn extension_color_space(&self) -> ColorSpace {
        // A sequence's persisted path points at its representative frame, so
        // the extension is the same either way.
        let text = match &self.path {
            AssetPath::Absolute(path) => path.to_string_lossy().into_owned(),
            AssetPath::Relative(rel) | AssetPath::Variable(rel) => rel.clone(),
        };
        let extension = Path::new(&text)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if LINEAR_EXTENSIONS.contains(&extension.as_str()) {
            ColorSpace::LINEAR_REC709
        } else {
            ColorSpace::SRGB
        }
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

    /// `Display` and `parse` must be exact inverses, or "save → load → save"
    /// silently rewrites a reference into a different file.
    #[test]
    fn display_and_parse_round_trip_for_every_form() {
        let cases = [
            AssetPath::Absolute(PathBuf::from("/footage/clip.mov")),
            AssetPath::Absolute(PathBuf::from(r"C:\media\clip.mov")),
            AssetPath::Absolute(PathBuf::from(r"\\share\media\clip.mov")),
            // A backslash is an ordinary character in a POSIX file name.
            AssetPath::Absolute(PathBuf::from(r"/footage/a\b.mov")),
            // `${` inside a name must not be mistaken for a variable.
            AssetPath::Absolute(PathBuf::from("/footage/a${b.mov")),
            AssetPath::Relative("./footage/clip.mov".into()),
            AssetPath::Relative("footage/a${b.mov".into()),
            AssetPath::Relative("日本語/クリップ.mov".into()),
            AssetPath::Variable("${PROJECT_ROOT}/footage/clip.mov".into()),
            AssetPath::Variable("${MEDIA}/a/b.mov".into()),
        ];
        for case in cases {
            assert_eq!(AssetPath::parse(&case.to_string()), case, "{case:?}");
        }
    }

    /// CM-2: an explicit setting beats the file's metadata, which beats the
    /// extension default. All three tiers in one place, because the order is
    /// the whole rule.
    #[test]
    fn input_colour_space_follows_the_resolution_order() {
        // Tier 3: extension only.
        let png = MediaAssetEntry::from_absolute("/f/plate.png");
        assert_eq!(
            png.input_color_space(),
            (ColorSpace::SRGB, ColorSpaceSource::ExtensionDefault)
        );
        let exr = MediaAssetEntry::from_absolute("/f/plate.EXR");
        assert_eq!(
            exr.input_color_space(),
            (
                ColorSpace::LINEAR_REC709,
                ColorSpaceSource::ExtensionDefault
            )
        );
        // A container with no metadata falls to the integer default too.
        let mov = MediaAssetEntry::from_absolute("/f/clip.mov");
        assert_eq!(
            mov.input_color_space(),
            (ColorSpace::SRGB, ColorSpaceSource::ExtensionDefault)
        );

        // Tier 2 beats tier 3: an EXR that declares sRGB is sRGB.
        let mut tagged = exr.clone();
        tagged.metadata.color_space = Some("sRGB".into());
        assert_eq!(
            tagged.input_color_space(),
            (ColorSpace::SRGB, ColorSpaceSource::Metadata)
        );
        // An unrecognised metadata string is not a guess — fall through.
        let mut gibberish = exr.clone();
        gibberish.metadata.color_space = Some("aces_1.2".into());
        assert_eq!(
            gibberish.input_color_space().1,
            ColorSpaceSource::ExtensionDefault
        );

        // Tier 1 beats both.
        let mut explicit = tagged.clone();
        explicit.color_space = Some(ColorSpace::REC709);
        assert_eq!(
            explicit.input_color_space(),
            (ColorSpace::REC709, ColorSpaceSource::Explicit)
        );
    }

    /// The explicit setting is persisted, and a document written before the
    /// field existed still loads.
    #[test]
    fn explicit_colour_space_round_trips_and_defaults_to_none() {
        let mut entry = MediaAssetEntry::from_absolute("/f/plate.exr");
        entry.color_space = Some(ColorSpace::SRGB);
        let text = ron::ser::to_string(&entry).unwrap();
        let back: MediaAssetEntry = ron::from_str(&text).unwrap();
        assert_eq!(back.color_space, Some(ColorSpace::SRGB));

        let legacy: MediaAssetEntry =
            ron::from_str(r#"MediaAssetEntry(path: "/f/plate.exr", kind: Still)"#).unwrap();
        assert_eq!(legacy.color_space, None);
        assert_eq!(
            legacy.input_color_space().1,
            ColorSpaceSource::ExtensionDefault
        );
    }

    /// A file name containing `${` is a real path, not a variable: treating
    /// it as one would make the asset permanently offline.
    #[test]
    fn a_brace_token_inside_a_name_does_not_become_a_variable() {
        let path = AssetPath::parse("/footage/a${b.mov");
        assert_eq!(
            path,
            AssetPath::Absolute(PathBuf::from("/footage/a${b.mov"))
        );
        assert_eq!(
            path.resolve(Some(Path::new("/proj")), &HashMap::new()),
            Some(PathBuf::from("/footage/a${b.mov"))
        );
    }

    /// On POSIX a backslash is part of the file name, so relativizing must
    /// not split it into two directory levels.
    #[cfg(unix)]
    #[test]
    fn a_backslash_in_a_posix_name_survives_relativization() {
        let entry = MediaAssetEntry::from_absolute(r"/proj/footage/a\b.mov");
        let saved = entry.relativized(Some(Path::new("/proj")));
        assert_eq!(saved.path, AssetPath::Relative(r"./footage/a\b.mov".into()));
        assert_eq!(
            saved
                .resolved_against(Some(Path::new("/proj")), &HashMap::new())
                .resolved,
            Some(PathBuf::from(r"/proj/footage/a\b.mov"))
        );
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
            name: "a".into(),
            path: AssetPath::Variable("${MEDIA}/a.mov".into()),
            kind: AssetKind::Container,
            metadata: AssetMetadata::default(),
            color_space: None,
            exposed_owner: None,
            resolved: Some(PathBuf::from("/proj/a.mov")),
        };
        assert_eq!(
            variable.relativized(Some(Path::new("/proj"))).path,
            AssetPath::Variable("${MEDIA}/a.mov".into()),
        );

        let offline = MediaAssetEntry {
            name: "gone".into(),
            path: AssetPath::Relative("./gone.mov".into()),
            kind: AssetKind::Container,
            metadata: AssetMetadata::default(),
            color_space: None,
            exposed_owner: None,
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
            name: "clip".into(),
            color_space: None,
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
                audio_streams: vec![AudioStreamMetadata {
                    stream_index: 1,
                    codec: Some("aac".into()),
                    sample_rate: 48_000,
                    channels: 2,
                }],
                file_size: 1234,
            },
            exposed_owner: None,
            resolved: Some(PathBuf::from("/proj/footage/clip.mov")),
        };
        let text = ron::to_string(&entry).unwrap();
        let back: MediaAssetEntry = ron::from_str(&text).unwrap();

        // `resolved` is deliberately not persisted.
        assert_eq!(back.resolved, None);
        assert_eq!(
            back,
            MediaAssetEntry {
                exposed_owner: None,
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

    /// The audio stream list is an additive field: an entry persisted before
    /// it existed loads with an empty list, keeps its stream count, and still
    /// reports that the file has audio.
    #[test]
    fn an_entry_without_the_stream_list_keeps_its_audio_count() {
        let legacy = r#"(path: "/abs/clip.mov", kind: Container, metadata: (width: Some(1920), audio_stream_count: 2))"#;
        let entry: MediaAssetEntry = ron::from_str(legacy).unwrap();
        assert_eq!(entry.metadata.audio_stream_count, 2);
        assert!(entry.metadata.audio_streams.is_empty());
        assert!(entry.metadata.has_audio());
        // A count alone cannot name a container stream index.
        assert_eq!(entry.metadata.first_audio_stream_index(), None);
    }

    /// The first audio stream is identified by its **container** index, so a
    /// muxed clip picks stream 1, not the video stream 0.
    #[test]
    fn first_audio_stream_index_is_the_container_index() {
        let metadata = AssetMetadata {
            audio_stream_count: 2,
            audio_streams: vec![
                AudioStreamMetadata {
                    stream_index: 1,
                    codec: Some("aac".into()),
                    sample_rate: 48_000,
                    channels: 2,
                },
                AudioStreamMetadata {
                    stream_index: 2,
                    codec: Some("pcm_s16le".into()),
                    sample_rate: 44_100,
                    channels: 1,
                },
            ],
            ..AssetMetadata::default()
        };
        assert_eq!(metadata.first_audio_stream_index(), Some(1));
        assert!(metadata.has_audio());

        let silent = AssetMetadata::default();
        assert!(!silent.has_audio());
        assert_eq!(silent.first_audio_stream_index(), None);
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
