// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Font resolution for the text nodes (REQ-MOGRAPH-004).
//!
//! [`FontRef`] is what a `text.font` node produces and the later text nodes
//! consume: the bytes of one font face plus the family, weight and style that
//! were actually resolved. Shaping and outline extraction come later
//! (`docs/implementation/typography-plan.md`, units 2 and 5); this module
//! answers only "which face, and where are its bytes".
//!
//! # Why a directory scan rather than the platform font API
//!
//! The plan sketched reusing the `font-kit` feature that `gpui_platform`
//! already enables. That would put a platform font API — CoreText, DirectWrite
//! or fontconfig — into the crate `ravel-cli` links, and a headless render
//! node must be able to resolve the same font the GUI resolved without
//! acquiring the GUI's system libraries (see `AGENTS.md` on the two shipped
//! binaries). The face metadata this module needs is four fields of the `name`
//! and `OS/2` tables, so it reads them with `ttf-parser` — already in the tree
//! behind `rustybuzz` and `swash`, and pure Rust — from the platform's font
//! directories. Family aliasing (`sans-serif`, a user's `fonts.conf`) is the
//! part that is given up; a motion-graphics document names a concrete family.
//!
//! # Fallback
//!
//! One face is compiled into the binary and indexed like any other, so
//! [`FontLibrary::resolve`] is **infallible**: an unknown family logs one
//! warning and yields that face with [`FontRef::is_fallback`] set, and an
//! evaluation is never failed by a font that is not installed
//! (typography-plan unit 1). Because the embedded face is indexed, asking for
//! its family is a genuine hit on every platform — which is also what makes
//! the tests here independent of the host's installed fonts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use crate::id::DataTypeId;
use crate::types::NodeData;

/// The face compiled into the binary, used as the resolution fallback.
///
/// Geist Regular, one of the bundled UI faces (SIL OFL 1.1 — the license text
/// lives beside the file in `assets/fonts/` and has to travel with any release
/// bundle). Regular alone: it costs ~123 KiB in every binary including
/// `ravel-cli`, and a fallback needs one face, not a family.
const FALLBACK_FONT: &[u8] = include_bytes!("../../../assets/fonts/Geist-Regular.ttf");

/// The family a new `text.font` node asks for.
///
/// The embedded face, so a freshly added node resolves on every machine rather
/// than starting life on a fallback warning.
pub const DEFAULT_FAMILY: &str = "Geist";

/// How deep a font directory is walked before the scan gives up.
///
/// macOS and Linux both nest one or two levels (`.../Supplemental`,
/// `.../truetype/dejavu`); the cap only exists so a symlink loop or a stray
/// mount under a font directory cannot make startup unbounded.
const MAX_SCAN_DEPTH: u32 = 8;

/// Named weights offered by the `text.font` node's `weight` parameter, in
/// ascending order. [`weight_from_name`] maps them to `OS/2` weight classes.
pub const FONT_WEIGHTS: [&str; 9] = [
    "thin",
    "extralight",
    "light",
    "regular",
    "medium",
    "semibold",
    "bold",
    "extrabold",
    "black",
];

/// Styles offered by the `text.font` node's `style` parameter.
///
/// Oblique is not a third entry: a face is either upright or slanted as far as
/// selection is concerned, and the distinction between a true italic and a
/// synthesised slant is a property of the face, not of the request.
pub const FONT_STYLES: [&str; 2] = ["normal", "italic"];

/// The `OS/2` weight class a [`FONT_WEIGHTS`] name stands for.
///
/// A name that is not in the table is read as a number (`"350"`), and anything
/// else is `400` — the parameter is a dropdown, so a foreign value comes from a
/// hand-edited document rather than from the UI.
pub fn weight_from_name(name: &str) -> u16 {
    let name = name.trim();
    match name.to_ascii_lowercase().as_str() {
        "thin" => 100,
        "extralight" => 200,
        "light" => 300,
        "regular" | "normal" => 400,
        "medium" => 500,
        "semibold" => 600,
        "bold" => 700,
        "extrabold" => 800,
        "black" => 900,
        _ => name.parse().unwrap_or(400),
    }
}

/// Whether a [`FONT_STYLES`] name asks for a slanted face.
pub fn style_is_italic(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("italic") || name.trim().eq_ignore_ascii_case("oblique")
}

/// The family name a lookup is keyed on: trimmed and case-folded.
///
/// One normalisation point, applied by [`FontQuery::new`] and by the index, so
/// `"Geist"` and `" geist "` are the same cache entry rather than two entries
/// that load the same file twice.
fn family_key(family: &str) -> String {
    family.trim().to_lowercase()
}

// ===========================================================================
// FontQuery
// ===========================================================================

/// What a `text.font` node asks for: a family, a weight class, and upright or
/// slanted.
///
/// Also the resolution cache's key, which is why the family is normalised on
/// construction and the fields are not public.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FontQuery {
    family: String,
    weight: u16,
    italic: bool,
}

impl FontQuery {
    pub fn new(family: &str, weight: u16, italic: bool) -> Self {
        Self {
            family: family_key(family),
            weight,
            italic,
        }
    }
}

// ===========================================================================
// FontRef
// ===========================================================================

/// One resolved font face ([`DataTypeId::FONT`]).
///
/// `family` / `weight` / `italic` describe the face that was **found**, not
/// what was asked for: a request for a semibold family that ships Regular and
/// Bold answers with one of those, and the Properties panel shows the user
/// which. `data` is shared: two `FontRef`s selecting different weights out of
/// one file hold the same `Arc`.
#[derive(Clone)]
pub struct FontRef {
    /// Family name as the face declares it (`name` ID 16, else ID 1).
    pub family: String,
    /// `OS/2` weight class of the resolved face, 100–900.
    pub weight: u16,
    /// Whether the resolved face is slanted.
    pub italic: bool,
    /// The whole font file, shared between every `FontRef` that reads it.
    pub data: Arc<Vec<u8>>,
    /// Index of the face inside `data`, for a collection (`.ttc` / `.otc`).
    pub face_index: u32,
    /// The requested family was not found (or its file could not be read) and
    /// this is the built-in face standing in for it.
    pub is_fallback: bool,
}

impl std::fmt::Debug for FontRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontRef")
            .field("family", &self.family)
            .field("weight", &self.weight)
            .field("italic", &self.italic)
            .field("bytes", &self.data.len())
            .field("face_index", &self.face_index)
            .field("is_fallback", &self.is_fallback)
            .finish()
    }
}

impl NodeData for FontRef {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::FONT
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn byte_size(&self) -> u64 {
        // The font file counts even though it is shared with the library's own
        // cache and with sibling `FontRef`s: the budget compares sums against
        // a megabyte limit, and under-counting a 4.5 MB CJK face would be the
        // worse error of the two.
        size_of::<Self>() as u64 + self.family.len() as u64 + self.data.len() as u64
    }
}

// ===========================================================================
// FontLibrary
// ===========================================================================

/// One indexed face: everything selection needs, without its bytes.
struct FaceRecord {
    family: String,
    family_key: String,
    weight: u16,
    italic: bool,
    face_index: u32,
    /// `None` for the face compiled into the binary.
    path: Option<PathBuf>,
}

/// An index of the installed faces, plus the caches that make resolution cheap
/// and its results shared.
///
/// The index is built once, by reading every font file under the directories
/// given to [`FontLibrary::new`]. That is the deliberate cost of not linking a
/// platform font API: a machine with several hundred installed faces spends a
/// second or two on the first resolution of the process. Both caches are
/// unbounded and keyed by content — a query, a file path — so they grow with
/// how many distinct fonts a document uses, not with how long it is open.
pub struct FontLibrary {
    faces: Vec<FaceRecord>,
    /// Loaded font files, keyed by path. What makes two `FontRef`s out of one
    /// file share their bytes.
    files: Mutex<HashMap<PathBuf, Arc<Vec<u8>>>>,
    /// Resolved queries. What makes [`FontLibrary::resolve`] return the *same*
    /// `Arc` for the same request, so re-evaluating a graph does not rebuild
    /// (or re-warn about) a font.
    resolved: Mutex<HashMap<FontQuery, Arc<FontRef>>>,
    fallback_data: Arc<Vec<u8>>,
    fallback_family: String,
    fallback_weight: u16,
}

impl FontLibrary {
    /// Index the embedded face and every font file under `dirs`.
    ///
    /// Unreadable directories and unparsable files are skipped silently: a
    /// font directory holding something that is not a font is normal, and one
    /// bad file must not cost the process every other family.
    pub fn new(dirs: &[PathBuf]) -> Self {
        let mut faces = Vec::new();
        collect_faces(FALLBACK_FONT, None, &mut faces);
        // The embedded face is the fallback, so its own metadata has to be
        // readable even if it were somehow the only thing that failed to
        // parse. `every_embedded_font_is_a_font_file` in `ravel-app` and
        // `the_embedded_fallback_face_parses` below both pin that it does.
        let (fallback_family, fallback_weight) = faces
            .first()
            .map(|face| (face.family.clone(), face.weight))
            .unwrap_or_else(|| (String::new(), 400));
        for dir in dirs {
            scan_dir(dir, 0, &mut faces);
        }
        Self {
            faces,
            files: Mutex::new(HashMap::new()),
            resolved: Mutex::new(HashMap::new()),
            fallback_data: Arc::new(FALLBACK_FONT.to_vec()),
            fallback_family,
            fallback_weight,
        }
    }

    /// Resolve `query`, never failing.
    ///
    /// The same query yields the same `Arc` for the life of the library, and a
    /// fallback is warned about once — on the miss that creates the entry —
    /// rather than once per evaluated frame.
    pub fn resolve(&self, query: &FontQuery) -> Arc<FontRef> {
        if let Some(hit) = lock(&self.resolved).get(query) {
            return hit.clone();
        }
        let font = Arc::new(self.resolve_uncached(query));
        if font.is_fallback {
            tracing::warn!(
                requested = %query.family,
                weight = query.weight,
                italic = query.italic,
                fallback = %font.family,
                "font family not found; falling back to the built-in face"
            );
        }
        lock(&self.resolved).insert(query.clone(), font.clone());
        font
    }

    fn resolve_uncached(&self, query: &FontQuery) -> FontRef {
        if let Some(face) = self.best_match(query)
            && let Some(data) = self.face_data(face)
        {
            return FontRef {
                family: face.family.clone(),
                weight: face.weight,
                italic: face.italic,
                data,
                face_index: face.face_index,
                is_fallback: false,
            };
        }
        FontRef {
            family: self.fallback_family.clone(),
            weight: self.fallback_weight,
            italic: false,
            data: self.fallback_data.clone(),
            face_index: 0,
            is_fallback: true,
        }
    }

    /// The indexed face that best answers `query`, or `None` when the family
    /// is not installed.
    ///
    /// Matching the requested slant outranks matching the weight: a request
    /// for a bold italic is better served by the family's regular italic than
    /// by its upright bold, because the slant is the more visible of the two.
    /// Among equally slanted faces the nearest weight class wins, and a tie
    /// (450 between 400 and 500) goes to the lighter one, which is the rule
    /// CSS font matching uses below 400 and the direction that keeps a
    /// synthesised-looking result out of the middle of a family.
    fn best_match(&self, query: &FontQuery) -> Option<&FaceRecord> {
        self.faces
            .iter()
            .filter(|face| face.family_key == query.family)
            .min_by_key(|face| {
                (
                    face.italic != query.italic,
                    face.weight.abs_diff(query.weight),
                    face.weight,
                )
            })
    }

    /// The bytes of an indexed face, loading and caching the file on first
    /// use. `None` when the file has gone away since the index was built.
    fn face_data(&self, face: &FaceRecord) -> Option<Arc<Vec<u8>>> {
        let Some(path) = face.path.as_ref() else {
            return Some(self.fallback_data.clone());
        };
        if let Some(hit) = lock(&self.files).get(path) {
            return Some(hit.clone());
        }
        match std::fs::read(path) {
            Ok(bytes) => {
                let data = Arc::new(bytes);
                lock(&self.files).insert(path.clone(), data.clone());
                Some(data)
            }
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "indexed font file could not be read; falling back"
                );
                None
            }
        }
    }
}

/// The process-wide library, indexing the platform's font directories on first
/// use.
pub fn shared() -> &'static FontLibrary {
    static SHARED: LazyLock<FontLibrary> = LazyLock::new(|| FontLibrary::new(&system_font_dirs()));
    &SHARED
}

/// A poisoned cache is recovered rather than propagated: both maps hold only
/// memoised lookups, so there is no invariant a later reader can be misled by,
/// and a panic in one evaluation must not take every font in the process with
/// it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ===========================================================================
// Indexing
// ===========================================================================

/// Absolute font directories of the host platform.
#[cfg(target_os = "macos")]
const PLATFORM_FONT_DIRS: &[&str] = &[
    "/System/Library/Fonts",
    "/System/Library/Fonts/Supplemental",
    "/Library/Fonts",
];

#[cfg(target_os = "windows")]
const PLATFORM_FONT_DIRS: &[&str] = &[];

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const PLATFORM_FONT_DIRS: &[&str] = &["/usr/share/fonts", "/usr/local/share/fonts"];

/// Per-user font directories, relative to the home directory.
#[cfg(target_os = "macos")]
const USER_FONT_DIRS: &[&str] = &["Library/Fonts"];

#[cfg(target_os = "windows")]
const USER_FONT_DIRS: &[&str] = &[];

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const USER_FONT_DIRS: &[&str] = &[".fonts", ".local/share/fonts"];

/// Where [`shared`] looks for installed faces.
///
/// The environment-derived entries are appended unconditionally rather than
/// under a `cfg`: `SystemRoot` and `LOCALAPPDATA` are Windows variables, so on
/// the other platforms they are simply absent, and one code path is one that
/// can be read on the machine it is written on. Directories that do not exist
/// are skipped by the scan.
fn system_font_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = PLATFORM_FONT_DIRS.iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = PathBuf::from(home);
        dirs.extend(USER_FONT_DIRS.iter().map(|rel| home.join(rel)));
    }
    if let Some(root) = std::env::var_os("SystemRoot") {
        dirs.push(PathBuf::from(root).join("Fonts"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Microsoft/Windows/Fonts"));
    }
    dirs
}

/// Whether a path names a font container this module can parse.
fn is_font_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "ttf" | "otf" | "ttc" | "otc"
            )
        })
}

fn scan_dir(dir: &Path, depth: u32, out: &mut Vec<FaceRecord>) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, depth + 1, out);
        } else if is_font_file(&path)
            && let Ok(data) = std::fs::read(&path)
        {
            collect_faces(&data, Some(&path), out);
        }
    }
}

/// Index every face in one font container. `path` is `None` for the embedded
/// face.
fn collect_faces(data: &[u8], path: Option<&Path>, out: &mut Vec<FaceRecord>) {
    let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
    for face_index in 0..count {
        let Ok(face) = ttf_parser::Face::parse(data, face_index) else {
            continue;
        };
        let Some(family) = family_name(&face) else {
            continue;
        };
        out.push(FaceRecord {
            family_key: family_key(&family),
            family,
            weight: face.weight().to_number(),
            italic: face.is_italic(),
            face_index,
            path: path.map(Path::to_path_buf),
        });
    }
}

/// The family a face belongs to: the typographic family (`name` ID 16) when it
/// has one, else the legacy family (ID 1), preferring a Unicode record of
/// either over a legacy Macintosh one.
///
/// The ID 16 / ID 1 distinction matters for exactly the weights this module
/// has to select between. A four-weight family stores ID 1 as
/// `"Geist SemiBold"` on the semibold face, so keying on ID 1 alone would
/// split one family into four that no `weight` parameter could reach; ID 16
/// says `"Geist"` on all four. Faces that predate ID 16 only have ID 1, and
/// for those the two agree.
fn family_name(face: &ttf_parser::Face<'_>) -> Option<String> {
    // Slots in preference order: typographic before legacy, Unicode before
    // Macintosh. Only the first record of each kind is kept, which is the one
    // the platform would pick too.
    let mut candidates: [Option<String>; 4] = [None, None, None, None];
    for name in face.names() {
        let slot = match (name.name_id, name.is_unicode()) {
            (ttf_parser::name_id::TYPOGRAPHIC_FAMILY, true) => 0,
            (ttf_parser::name_id::FAMILY, true) => 1,
            (ttf_parser::name_id::TYPOGRAPHIC_FAMILY, false) => 2,
            (ttf_parser::name_id::FAMILY, false) => 3,
            _ => continue,
        };
        if candidates[slot].is_none() {
            candidates[slot] = name_text(&name);
        }
    }
    candidates
        .into_iter()
        .flatten()
        .find(|name| !name.trim().is_empty())
}

/// One `name` record as text, or `None` for an encoding this module cannot
/// read.
///
/// `ttf-parser` decodes Unicode records only, and Apple's older system faces
/// — Helvetica, Courier, Geneva, all of them `.ttc` collections still shipped
/// in `/System/Library/Fonts` — carry **nothing but** Macintosh/Roman
/// records. Without this, asking for `"Helvetica"` on macOS silently fell
/// back to the built-in face. Roman is decoded only while it stays inside
/// ASCII, where it agrees with UTF-8 byte for byte; above `0x7F` it is its own
/// code page, and a face with non-ASCII names has a Unicode record that the
/// preference order above reaches first anyway.
fn name_text(name: &ttf_parser::name::Name<'_>) -> Option<String> {
    if let Some(text) = name.to_string() {
        return Some(text);
    }
    if name.platform_id == ttf_parser::PlatformId::Macintosh
        && name.encoding_id == 0
        && name.name.is_ascii()
    {
        return Some(String::from_utf8_lossy(name.name).into_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A library with no directories to scan: only the embedded face is
    /// indexed, so every assertion below holds identically on macOS, Windows
    /// and a bare CI container.
    fn embedded_only() -> FontLibrary {
        FontLibrary::new(&[])
    }

    /// The fallback has to be a parsable face, or every unresolved family
    /// would answer with an empty family name and no glyphs.
    #[test]
    fn the_embedded_fallback_face_parses() {
        let library = embedded_only();
        assert_eq!(library.fallback_family, DEFAULT_FAMILY);
        assert_eq!(library.fallback_weight, 400);
        assert!(!library.fallback_data.is_empty());
    }

    /// An installed family resolves to itself. The embedded face is indexed
    /// like any other, which is what lets this test name a real family without
    /// depending on the host's fonts.
    #[test]
    fn an_indexed_family_resolves_to_its_own_face() {
        let library = embedded_only();
        let font = library.resolve(&FontQuery::new("Geist", 400, false));
        assert_eq!(font.family, "Geist");
        assert_eq!(font.weight, 400);
        assert!(!font.italic);
        assert!(
            !font.is_fallback,
            "an installed family must not report a fallback"
        );
        assert!(font.data.len() > 1024);
    }

    /// Family matching ignores case and surrounding space, so a hand-typed
    /// family name in a document resolves the same as a picked one.
    #[test]
    fn family_matching_ignores_case_and_padding() {
        let library = embedded_only();
        let font = library.resolve(&FontQuery::new("  gEiSt ", 400, false));
        assert!(!font.is_fallback);
        assert_eq!(font.family, "Geist");
    }

    /// An unknown family yields the built-in face instead of an error: a font
    /// that is not installed must not fail the evaluation (typography-plan
    /// unit 1).
    #[test]
    fn an_unknown_family_falls_back_to_the_embedded_face() {
        let library = embedded_only();
        let font = library.resolve(&FontQuery::new("No Such Family ZZZ", 700, true));
        assert!(font.is_fallback, "an unknown family must report a fallback");
        assert_eq!(font.family, "Geist");
        assert!(
            font.data.len() > 1024,
            "the fallback has to carry usable font bytes"
        );
    }

    /// The resolution cache returns the *same* `Arc`, not an equal value: a
    /// value comparison would pass even with no cache at all, and what the
    /// cache buys is that a re-evaluated graph neither reloads the file nor
    /// re-warns about a fallback.
    #[test]
    fn the_same_query_resolves_to_the_same_arc() {
        let library = embedded_only();
        let first = library.resolve(&FontQuery::new("Geist", 400, false));
        let second = library.resolve(&FontQuery::new("geist", 400, false));
        assert!(
            Arc::ptr_eq(&first, &second),
            "the same query must be answered from the cache"
        );
    }

    /// Two queries that land on one **file** share its bytes, so selecting a
    /// weight a family does not have costs no second copy of a 4.5 MB face.
    ///
    /// Driven from records that point at a real file on disk: the embedded
    /// face answers out of `fallback_data`, which is one `Arc` whether or not
    /// the file cache exists — a version of this test written against it
    /// passes with the cache deleted.
    #[test]
    fn queries_landing_on_one_file_share_its_bytes() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("two-weights.ttf");
        std::fs::write(&path, FALLBACK_FONT).expect("write the fixture face");

        let mut library = embedded_only();
        library.faces = vec![file_face("Test", 400, &path), file_face("Test", 700, &path)];

        let regular = library.resolve(&FontQuery::new("Test", 400, false));
        let bold = library.resolve(&FontQuery::new("Test", 700, false));
        assert!(!regular.is_fallback && !bold.is_fallback);
        assert_ne!(regular.weight, bold.weight, "two different faces");
        assert!(
            Arc::ptr_eq(&regular.data, &bold.data),
            "one font file must be read and held once"
        );
    }

    fn file_face(family: &str, weight: u16, path: &Path) -> FaceRecord {
        FaceRecord {
            path: Some(path.to_path_buf()),
            ..face(family, weight, false)
        }
    }

    /// A weight the family does not carry resolves to the nearest one it does
    /// — not to the fallback.
    #[test]
    fn a_missing_weight_resolves_to_the_nearest_installed_one() {
        let library = embedded_only();
        for requested in [100, 300, 500, 900] {
            let font = library.resolve(&FontQuery::new("Geist", requested, false));
            assert!(
                !font.is_fallback,
                "weight {requested} must stay inside the family"
            );
            assert_eq!(font.weight, 400);
        }
    }

    /// The slant is matched ahead of the weight, and the nearest weight breaks
    /// the remaining tie towards the lighter face.
    #[test]
    fn selection_prefers_the_slant_then_the_nearest_weight() {
        let library = FontLibrary {
            faces: vec![
                face("Test", 400, false),
                face("Test", 700, false),
                face("Test", 400, true),
            ],
            files: Mutex::new(HashMap::new()),
            resolved: Mutex::new(HashMap::new()),
            fallback_data: Arc::new(Vec::new()),
            fallback_family: String::new(),
            fallback_weight: 400,
        };

        let bold_italic = library
            .best_match(&FontQuery::new("Test", 700, true))
            .expect("the family is indexed");
        assert!(
            bold_italic.italic && bold_italic.weight == 400,
            "the slant outranks the weight: got {} italic={}",
            bold_italic.weight,
            bold_italic.italic
        );

        let semibold = library
            .best_match(&FontQuery::new("Test", 550, false))
            .expect("the family is indexed");
        assert_eq!(semibold.weight, 400, "a tie goes to the lighter face");
    }

    fn face(family: &str, weight: u16, italic: bool) -> FaceRecord {
        FaceRecord {
            family: family.to_string(),
            family_key: family_key(family),
            weight,
            italic,
            face_index: 0,
            // A metadata-only record: `best_match` never reads the bytes.
            path: None,
        }
    }

    /// Faces on disk are found and indexed by their declared family, not by
    /// their file name. Driven from a temporary directory rather than the
    /// platform's, so the assertion does not depend on what the host has
    /// installed.
    #[test]
    fn a_face_on_disk_is_indexed_by_its_declared_family() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("some-unrelated-file-name.ttf");
        std::fs::write(&path, FALLBACK_FONT).expect("write the fixture face");
        std::fs::write(dir.path().join("notes.txt"), b"not a font").expect("write a decoy");

        let library = FontLibrary::new(&[dir.path().to_path_buf()]);
        let font = library.resolve(&FontQuery::new("Geist", 400, false));
        assert!(!font.is_fallback);
        // Two records now claim the family — the embedded face and the file —
        // and either answers correctly.
        assert_eq!(font.family, "Geist");
    }

    /// Nested font directories are walked: both macOS and Linux nest a level.
    #[test]
    fn nested_font_directories_are_scanned() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let nested = dir.path().join("vendor").join("faces");
        std::fs::create_dir_all(&nested).expect("create the nested dirs");
        std::fs::write(nested.join("face.ttf"), FALLBACK_FONT).expect("write the fixture face");

        let mut faces = Vec::new();
        scan_dir(dir.path(), 0, &mut faces);
        assert_eq!(
            faces.len(),
            1,
            "the nested face must be indexed exactly once"
        );
        assert_eq!(faces[0].family, "Geist");
    }

    /// Every dropdown option maps to a distinct weight class, so the parameter
    /// cannot offer two names that select the same face.
    #[test]
    fn every_weight_option_names_a_distinct_class() {
        let weights: Vec<u16> = FONT_WEIGHTS.iter().map(|n| weight_from_name(n)).collect();
        assert_eq!(weights, vec![100, 200, 300, 400, 500, 600, 700, 800, 900]);
    }

    #[test]
    fn weight_names_accept_numbers_and_reject_nonsense() {
        assert_eq!(weight_from_name("SemiBold"), 600);
        assert_eq!(weight_from_name("350"), 350);
        assert_eq!(weight_from_name("chunky"), 400);
        assert_eq!(weight_from_name(""), 400);
    }

    #[test]
    fn only_the_slanted_style_names_are_italic() {
        assert!(style_is_italic("italic"));
        assert!(style_is_italic("Oblique"));
        assert!(!style_is_italic("normal"));
        assert!(!style_is_italic(""));
    }

    /// Apple's older `.ttc` system faces carry only Macintosh/Roman name
    /// records, and reading them is what makes `"Helvetica"` resolvable on
    /// macOS at all. Driven through a synthetic record rather than the file,
    /// which exists on one of the three platforms.
    #[test]
    fn a_macintosh_roman_name_record_is_read_as_ascii() {
        let roman = |bytes: &'static [u8]| ttf_parser::name::Name {
            platform_id: ttf_parser::PlatformId::Macintosh,
            encoding_id: 0,
            language_id: 0,
            name_id: ttf_parser::name_id::FAMILY,
            name: bytes,
        };
        assert_eq!(
            name_text(&roman(b"Helvetica")).as_deref(),
            Some("Helvetica")
        );
        // Above ASCII, Roman is its own code page — such a face carries a
        // Unicode record, which the preference order reaches first.
        assert_eq!(name_text(&roman(b"Caf\xe9")), None);
        // A non-Roman Macintosh encoding is not guessed at either.
        let mut japanese = roman(b"\x82\xa0");
        japanese.encoding_id = 1;
        assert_eq!(name_text(&japanese), None);
    }

    #[test]
    fn only_font_containers_are_indexed() {
        assert!(is_font_file(Path::new("/f/a.ttf")));
        assert!(is_font_file(Path::new("/f/a.OTF")));
        assert!(is_font_file(Path::new("/f/a.ttc")));
        assert!(!is_font_file(Path::new("/f/a.txt")));
        assert!(!is_font_file(Path::new("/f/LICENSE")));
    }

    #[test]
    fn a_font_ref_accounts_for_its_file() {
        let library = embedded_only();
        let font = library.resolve(&FontQuery::new("Geist", 400, false));
        assert!(font.byte_size() >= font.data.len() as u64);
        assert_eq!(font.data_type_id(), DataTypeId::FONT);
    }
}
