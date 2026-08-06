// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! On-disk subgraph templates (`*.ravtpl`, REQ-PLUGIN-005 (2), EXPO-6).
//!
//! A template is a subnet's inner graph plus the exposed parameters it
//! publishes ([`SubgraphTemplate`]). This module is only the file half: the RON
//! projection, the atomic write, and the user-wide directory templates are
//! looked up in.
//!
//! # Why this is not a `.ravprj` change
//!
//! A template is a file the user drops in and shares, not part of any project,
//! so it needs no `.ravprj` format bump: the declarations it carries are added
//! to [`Document::exposed_parameters`](ravel_core::composition::Document) — the
//! field format v7 already has — when the template is instantiated. The
//! keybinding overrides take the same shape and for the same reason
//! ([`crate::paths::GLOBAL_KEYBINDINGS_FILE`]): a preset someone else authored
//! is a file they can drop in.
//!
//! # Why RON, with the same options as the document
//!
//! The template embeds a [`Graph`](ravel_core::graph::Graph), which serializes
//! id-sorted and re-validates through `Graph::from_parts` on the way back, so
//! the file is diff-friendly and a corrupt edge set is rejected rather than
//! loaded. Matching `document/main.ron`'s options (`struct_names`, two-space
//! indent) means a template reads like the part of a project it came from.
//!
//! # What a damaged file loses
//!
//! Declarations are read through [`ExposedParameters`]'s own checked
//! `Deserialize`, exactly as a project's are: an entry that parses but violates
//! an invariant (a blank name, a default contradicting its type, a repeated
//! name) is dropped with a warning and the rest of the template still loads.
//! That shared leniency **is** the "same type, same validation" property EXPO-6
//! asks for — there is no template-side check to keep in step, because there is
//! no template-side type.

use std::path::{Path, PathBuf};

use ravel_core::subgraph_template::SubgraphTemplate;
use thiserror::Error;

use crate::atomic_write;
use crate::paths::global_config_dir;

/// File extension of a subgraph template.
pub const TEMPLATE_EXTENSION: &str = "ravtpl";

/// Directory name, under the global config directory, holding user templates.
pub const TEMPLATES_DIR: &str = "subgraph-templates";

/// Errors raised while reading or writing a subgraph template file.
#[derive(Debug, Error)]
pub enum SubgraphTemplateFileError {
    #[error("failed to serialize subgraph template to RON: {0}")]
    Serialize(#[from] ron::Error),

    #[error("failed to parse subgraph template RON: {0}")]
    Parse(#[from] ron::de::SpannedError),

    #[error("failed to read or write subgraph template: {0}")]
    Io(#[from] std::io::Error),

    #[error("{name:?} is not a usable template name: it {reason}")]
    InvalidName { name: String, reason: &'static str },

    #[error("a template named {0:?} is already there")]
    AlreadyExists(String),

    #[error("there is no template named {0:?} to replace")]
    NotFound(String),
}

/// Serialize `template` to pretty-printed RON.
pub fn to_ron(template: &SubgraphTemplate) -> Result<String, SubgraphTemplateFileError> {
    let config = ron::ser::PrettyConfig::new()
        .struct_names(true)
        .indentor("  ".to_string());
    Ok(ron::ser::to_string_pretty(template, config)?)
}

/// Parse a subgraph template from RON text.
pub fn from_ron(text: &str) -> Result<SubgraphTemplate, SubgraphTemplateFileError> {
    Ok(ron::from_str(text)?)
}

/// The file `name` names inside `dir`, refusing anything that is not a single
/// ordinary file name.
///
/// A template's name is user input — typed into a save dialog, or carried in a
/// file someone shared — and a name is **not** a path. Passing one through as a
/// path is how `../../../project.ravprj` becomes a write, so the whole of the
/// path is built here: `dir`, one checked component, and the extension this
/// module owns.
///
/// The separators, the Windows drive marker and `..` are refused on every
/// platform rather than the host's, the same rule and for the same reason as
/// `ravel_core::media::encode`'s sequence names: a name authored on one system
/// travels to another. A leading `.` is refused too — it produces a hidden file
/// [`load_dir`] would list but a file manager would not — and a `.ravtpl`
/// already on the end is dropped rather than doubled, so "title" and
/// "title.ravtpl" name one file instead of two.
pub fn template_path(dir: &Path, name: &str) -> Result<PathBuf, SubgraphTemplateFileError> {
    let stem = name
        .strip_suffix(&format!(".{TEMPLATE_EXTENSION}"))
        .unwrap_or(name)
        .trim();
    let refuse = |reason: &'static str| {
        Err(SubgraphTemplateFileError::InvalidName {
            name: name.to_string(),
            reason,
        })
    };
    if stem.is_empty() {
        return refuse("is empty");
    }
    if stem.contains('/') || stem.contains('\\') {
        return refuse("contains a path separator");
    }
    if stem.contains(':') {
        return refuse("contains \":\", which names a drive on Windows");
    }
    if stem.contains("..") {
        return refuse("contains \"..\"");
    }
    if stem.contains('\0') {
        return refuse("contains a NUL byte");
    }
    if stem.starts_with('.') {
        return refuse("starts with \".\"");
    }
    Ok(dir.join(format!("{stem}.{TEMPLATE_EXTENSION}")))
}

/// Write `template` into `dir` under `name`, failing if that name is taken.
///
/// Separate from [`replace`] on purpose: "save a new template" and "overwrite
/// the one that is there" are different intentions, and a single `save` that
/// silently does whichever applies turns a mistyped name into a destroyed
/// template. The file is created with `create_new`, so the check and the
/// creation are one operation rather than a race
/// (`ravel_media::encode::sequence`'s `PartialFile` takes the same stance).
pub fn save_new(
    template: &SubgraphTemplate,
    dir: &Path,
    name: &str,
) -> Result<PathBuf, SubgraphTemplateFileError> {
    let path = template_path(dir, name)?;
    let text = to_ron(template)?;
    std::fs::create_dir_all(dir)?;
    let mut file = match std::fs::File::create_new(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(SubgraphTemplateFileError::AlreadyExists(name.to_string()));
        }
        Err(err) => return Err(err.into()),
    };
    std::io::Write::write_all(&mut file, text.as_bytes())?;
    file.sync_all()?;
    Ok(path)
}

/// Replace the template `dir` already holds under `name`.
///
/// Fails when there is nothing to replace: overwriting is only meaningful as an
/// answer to "yes, replace that one", and without a file there the caller meant
/// [`save_new`]. The write is atomic ([`atomic_write`]) because this one *does*
/// stand to lose something — a template half-written over the one it replaces
/// is a template the user no longer has.
pub fn replace(
    template: &SubgraphTemplate,
    dir: &Path,
    name: &str,
) -> Result<PathBuf, SubgraphTemplateFileError> {
    let path = template_path(dir, name)?;
    if !path.is_file() {
        return Err(SubgraphTemplateFileError::NotFound(name.to_string()));
    }
    let text = to_ron(template)?;
    atomic_write::write(&path, text.as_bytes())?;
    Ok(path)
}

/// Read a subgraph template from `path`.
pub fn load(path: &Path) -> Result<SubgraphTemplate, SubgraphTemplateFileError> {
    from_ron(&std::fs::read_to_string(path)?)
}

/// The directory user templates live in
/// (`<config_base>/ravel/subgraph-templates`).
///
/// `None` only when the platform config base cannot be determined, the same
/// condition [`global_config_dir`] reports it for.
pub fn templates_dir() -> Option<PathBuf> {
    global_config_dir().map(|dir| dir.join(TEMPLATES_DIR))
}

/// Every template file in `dir`, loaded, in file-name order.
///
/// A file that fails to load is **skipped with a warning**, not fatal: a
/// directory the user drops files into will eventually hold one that does not
/// parse, and one bad file must not hide the rest of their library. A missing
/// directory is an empty library, not an error.
///
/// Only regular files are read. A symlink is skipped rather than followed:
/// listing a directory must not be a way to make Ravel open a path outside it,
/// and a template library is a place files are dropped into, not a place to
/// build indirections.
pub fn load_dir(dir: &Path) -> Result<Vec<SubgraphTemplate>, SubgraphTemplateFileError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        // `DirEntry::file_type` does not follow the link, which is the point.
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext == TEMPLATE_EXTENSION)
        })
        .collect();
    paths.sort();

    let mut templates = Vec::new();
    for path in paths {
        match load(&path) {
            Ok(template) => templates.push(template),
            Err(err) => {
                tracing::warn!(%err, path = %path.display(), "skipping an unreadable subgraph template");
            }
        }
    }
    Ok(templates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::exposed::{ExposedBinding, ExposedParameter, ExposedParameters, ExposedValue};
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::network::{
        self, NET_IN_TYPE_KEY, NET_OUT_TYPE_KEY, PORT_FRAME, PORT_TIME, SUBNET_TYPE_KEY,
    };

    /// A subnet holding one node whose `text` parameter is published, plus the
    /// declaration over it.
    fn authored() -> (Graph, NodeId, ExposedParameters) {
        let title = NodeId::next();
        let inner = Graph::new()
            .add_node(
                Node::new(NodeId::next(), NET_IN_TYPE_KEY)
                    .with_output(PORT_TIME, DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(
                Node::new(title, "test")
                    .with_output("out", DataTypeId::FRAME_BUFFER)
                    .with_param("text", ParameterValue::String("Ravel".into())),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::next(), NET_OUT_TYPE_KEY)
                    .with_input(PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
            )
            .unwrap();
        let subnet_id = NodeId::next();
        let mut subnet = Node::new(subnet_id, SUBNET_TYPE_KEY);
        network::adopt_subnet_inner(&mut subnet, inner);
        let graph = Graph::new().add_node(subnet).unwrap();
        let declarations = ExposedParameters::from_declarations([ExposedParameter::inferred(
            "headline",
            ExposedValue::String("Ravel".into()),
            ExposedBinding::new(title, "text"),
        )
        .unwrap()
        .with_description("The title card's text")])
        .unwrap();
        (graph, subnet_id, declarations)
    }

    fn template() -> SubgraphTemplate {
        let (graph, subnet_id, declarations) = authored();
        SubgraphTemplate::capture("Title Card", &graph, subnet_id, &declarations).unwrap()
    }

    /// EXPO-6's second completion criterion: declarations survive the round
    /// trip.
    #[test]
    fn a_template_roundtrips_through_ron_with_its_declarations() {
        let template = template();
        let text = to_ron(&template).expect("it serializes");
        let read = from_ron(&text).expect("it parses");
        assert_eq!(read, template);

        let declaration = read.declarations().get("headline").expect("declared");
        assert_eq!(
            declaration.default_value(),
            &ExposedValue::String("Ravel".into())
        );
        assert_eq!(declaration.description(), "The title card's text");
        assert_eq!(
            declaration.binding(),
            template.declarations().get("headline").unwrap().binding(),
            "the binding still names the node inside the template's own graph"
        );
    }

    #[test]
    fn a_template_roundtrips_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("subgraph-templates");
        let template = template();
        let path = save_new(&template, &library, "title").expect("the directory is created");
        assert_eq!(path, library.join(format!("title.{TEMPLATE_EXTENSION}")));
        assert_eq!(load(&path).expect("it loads"), template);
    }

    /// The whole point of taking a *name*: the path is built here, so a name
    /// that tries to be a path cannot reach outside the library — nor, in
    /// particular, overwrite a project next to it.
    #[test]
    fn a_name_that_is_really_a_path_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("templates");
        let victim = dir.path().join("keep.ravprj");
        std::fs::write(&victim, "a project the user would rather keep").unwrap();

        for name in [
            "../keep.ravprj",
            "../../keep",
            "sub/title",
            "sub\\title",
            "C:title",
            "/etc/passwd",
            "",
            "   ",
            ".hidden",
            "..",
        ] {
            assert!(
                matches!(
                    save_new(&template(), &library, name),
                    Err(SubgraphTemplateFileError::InvalidName { .. })
                ),
                "{name:?} must not name a template"
            );
            assert!(matches!(
                template_path(&library, name),
                Err(SubgraphTemplateFileError::InvalidName { .. })
            ));
        }
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "a project the user would rather keep",
            "nothing outside the library was written"
        );
    }

    /// One file per name, whichever way the caller spells the extension.
    #[test]
    fn the_module_owns_the_extension() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            template_path(dir.path(), "title").unwrap(),
            template_path(dir.path(), &format!("title.{TEMPLATE_EXTENSION}")).unwrap()
        );
    }

    /// A new save must not be a way to destroy an existing template, and a
    /// replace must not be a way to create one under a mistyped name. The two
    /// intentions are different, so they are different calls.
    #[test]
    fn saving_a_new_template_and_replacing_one_are_separate_operations() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path();

        assert!(matches!(
            replace(&template(), library, "title"),
            Err(SubgraphTemplateFileError::NotFound(name)) if name == "title"
        ));

        let path = save_new(&template(), library, "title").expect("the name is free");
        assert!(matches!(
            save_new(&template(), library, "title"),
            Err(SubgraphTemplateFileError::AlreadyExists(name)) if name == "title"
        ));

        let (graph, subnet_id, declarations) = authored();
        let renamed =
            SubgraphTemplate::capture("Renamed", &graph, subnet_id, &declarations).unwrap();
        let replaced = replace(&renamed, library, "title").expect("it is there to replace");
        assert_eq!(replaced, path);
        assert_eq!(load(&path).unwrap().name(), "Renamed");
    }

    /// The same leniency a `.ravprj` gets, because it is the same
    /// `Deserialize`: a declaration violating an invariant is dropped and the
    /// template still loads.
    #[test]
    fn an_invalid_declaration_is_dropped_and_the_template_still_loads() {
        let text = to_ron(&template()).unwrap();
        // A second declaration repeating the first name — an ambiguous
        // contract a hand-edit or a merge can produce.
        let duplicated = text.replacen(
            "declarations: [",
            r#"declarations: [
    ExposedParameter(name: "headline", value_type: Int, default: Int(3), description: "", binding: (node: NodeId(1), key: "n")),"#,
            1,
        );
        assert_ne!(duplicated, text, "the declarations list was found");
        let read = from_ron(&duplicated).expect("the rest of the template still loads");
        assert_eq!(read.declarations().len(), 1);
        assert_eq!(
            read.declarations().get("headline").unwrap().default_value(),
            &ExposedValue::Int(3),
            "the first declaration of a name is the one that survives"
        );
    }

    #[test]
    fn a_structurally_broken_file_is_an_error_not_a_panic() {
        assert!(from_ron("not a template").is_err());
        assert!(from_ron("").is_err());
    }

    #[test]
    fn a_directory_lists_its_templates_and_skips_what_it_cannot_read() {
        let dir = tempfile::tempdir().unwrap();
        save_new(&template(), dir.path(), "b").unwrap();
        save_new(&template(), dir.path(), "a").unwrap();
        std::fs::write(
            dir.path().join(format!("broken.{TEMPLATE_EXTENSION}")),
            "not a template",
        )
        .unwrap();
        // A file of another kind is not a template and is not reported as one.
        std::fs::write(dir.path().join("notes.txt"), "hello").unwrap();

        let templates = load_dir(dir.path()).expect("the directory reads");
        assert_eq!(templates.len(), 2, "the broken file is skipped");
        assert!(templates.iter().all(|t| t.name() == "Title Card"));
    }

    /// Listing a library must not be a way to reach outside it. A link named
    /// `*.ravtpl` is skipped, not followed.
    #[cfg(unix)]
    #[test]
    fn a_symlink_in_the_library_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("library");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let target = save_new(&template(), &outside, "elsewhere").unwrap();
        save_new(&template(), &library, "here").unwrap();
        std::os::unix::fs::symlink(
            &target,
            library.join(format!("linked.{TEMPLATE_EXTENSION}")),
        )
        .unwrap();

        assert_eq!(
            load_dir(&library).expect("the directory reads").len(),
            1,
            "only the regular file in the library is loaded"
        );
    }

    #[test]
    fn a_missing_directory_is_an_empty_library() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            load_dir(&dir.path().join("never-created"))
                .expect("a missing directory is not an error")
                .is_empty()
        );
    }

    /// EXPO-6's third completion criterion, taken the whole way: a template
    /// written to a file, read back, stamped into a project, saved as
    /// `.ravprj` and reloaded declares exactly what a project's own
    /// declarations declare — same listing, same `apply`, same validation.
    ///
    /// If the template ever grew a declaration type of its own, this is where
    /// the two would stop agreeing.
    #[test]
    fn a_template_from_a_file_declares_what_a_project_declares() {
        use ravel_core::composition::{Composition, Document, Layer};
        use ravel_core::exposed::apply::{AssetContext, apply, resolve};
        use ravel_core::exposed::listing::ExposedListing;
        use ravel_core::id::{CompId, LayerId};
        use ravel_core::subgraph_template::add_declarations;
        use ravel_core::types::FrameRate;

        let dir = tempfile::tempdir().unwrap();
        let path = save_new(&template(), dir.path(), "title").unwrap();
        let template = load(&path).expect("it loads");

        let instance = template
            .instantiate()
            .expect("the template binds only its own nodes");
        let graph = Graph::new().add_node(instance.node).unwrap();
        let comp = Composition::new(CompId::new(1), "Main", (16, 16), FrameRate::new(30, 1), 100)
            .add_layer(Layer::new(LayerId::next(), "L", graph).with_time(0, 0, 100));
        let document = Document::default().with_composition(comp);
        let (document, renames) = add_declarations(document, instance.declarations);
        assert!(renames.is_empty());

        // Through the project container, which is what a CLI render opens.
        let project = crate::ProjectFile::from_document("T", "2026-08-07T00:00:00Z", document);
        let project_path = dir.path().join("stamped.ravprj");
        project.save(&project_path).expect("it saves");
        let reloaded = crate::ProjectFile::load(&project_path).expect("it loads");

        assert_eq!(
            resolve(&reloaded.document),
            Vec::new(),
            "the template's declaration reaches its parameter after a save/load"
        );
        let listing = ExposedListing::of(&reloaded.document);
        assert_eq!(listing.parameters.len(), 1);
        assert_eq!(listing.parameters[0].name, "headline");
        assert_eq!(listing.parameters[0].description, "The title card's text");
        assert!(listing.parameters[0].resolved);

        let applied = apply(
            reloaded.document,
            &[(
                "headline".to_string(),
                ExposedValue::String("From a file".into()),
            )]
            .into_iter()
            .collect(),
            AssetContext::default(),
        )
        .expect("a string reaches a string parameter");
        assert!(applied.issues.is_empty());
    }

    #[test]
    fn the_template_directory_sits_under_the_app_config_directory() {
        if let Some(dir) = templates_dir() {
            assert!(dir.ends_with(TEMPLATES_DIR));
            assert!(dir.starts_with(global_config_dir().unwrap()));
        }
    }
}
