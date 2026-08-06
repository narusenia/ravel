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

/// Write `template` to `path`, creating the parent directory.
///
/// The write is atomic ([`atomic_write`]) for the reason every other Ravel
/// write is: a template half-written over the one it replaces is a template the
/// user has lost, and losing it to a crash during save is the worst moment to
/// lose it.
pub fn save(template: &SubgraphTemplate, path: &Path) -> Result<(), SubgraphTemplateFileError> {
    let text = to_ron(template)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write::write(path, text.as_bytes())?;
    Ok(())
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
pub fn load_dir(dir: &Path) -> Result<Vec<SubgraphTemplate>, SubgraphTemplateFileError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
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
        let path = dir
            .path()
            .join("nested")
            .join(format!("title.{TEMPLATE_EXTENSION}"));
        let template = template();
        save(&template, &path).expect("the parent directory is created");
        assert_eq!(load(&path).expect("it loads"), template);
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
        save(
            &template(),
            &dir.path().join(format!("b.{TEMPLATE_EXTENSION}")),
        )
        .unwrap();
        save(
            &template(),
            &dir.path().join(format!("a.{TEMPLATE_EXTENSION}")),
        )
        .unwrap();
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
        let path = dir.path().join(format!("title.{TEMPLATE_EXTENSION}"));
        save(&template(), &path).unwrap();
        let template = load(&path).expect("it loads");

        let instance = template.instantiate();
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
