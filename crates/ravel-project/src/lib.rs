// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `.ravprj` project file format — the persistence foundation for Ravel.
//!
//! A project is a zip container (see [`container`]) holding four logical parts:
//!
//! - [`manifest::Manifest`] — metadata + on-disk format version
//! - [`Document`] serialized as RON (`document/main.ron`, format v7)
//! - [`settings::SettingsLayer`] — the project's settings override layer
//! - [`ui_state::UiState`] — what the UI was looking at (REQ-UI-013)
//! - an optional [`LayoutDocument`] — the workspace layout the project opted
//!   into shipping (`workspace_layout.toml`)
//!
//! [`ProjectFile`] ties these together with [`ProjectFile::save`] /
//! [`ProjectFile::load`]. Saving always writes a `.bak` of the previous
//! revision; loading transparently runs the [`migration`] chain so that older
//! files open as the current format. All failure modes surface as
//! [`ProjectError`] — corrupt input never panics.
//!
//! Media assets are persisted inside `document/main.ron` as
//! [`AssetPath`](ravel_core::composition::AssetPath) references
//! ([`Document::media_assets`], REQ-PROJ-001): files under the project root
//! are stored relative so the whole project directory can move, everything
//! else stays absolute, and a variable-prefixed path the user set is kept
//! verbatim. [`ProjectFile::to_archive_for_root`] performs that narrowing on
//! save and [`ProjectFile::load`] reverses it, filling each entry's
//! `resolved` absolute path for evaluation.
//!
//! Format v4 dropped `assets/refs.json`. Every version that wrote the entry
//! wrote an empty collection, so a v3 archive that still contains one opens
//! with no information lost — the entry is simply ignored.
//!
//! Format v5 folded the built-in nodes' `_x` / `_y` component parameters into
//! single `Channel2` / `Channel3` vector parameters. That change lives inside
//! `document/main.ron`, which the untyped [`migration`] chain never sees, so
//! [`ProjectFile::from_archive`] applies it as a typed pass over the loaded
//! document ([`Document::fold_component_params`]) for any archive older than
//! v5.
//!
//! Format v6 replaced the `"0:0,1:1"` string that carried `field.curve_remap`'s
//! control points with a structured curve parameter. Same shape of change, same
//! treatment: [`Document::upgrade_curve_params`] runs over the loaded document
//! for any archive older than v6.
//!
//! `workspace_layout.toml` is the newest entry and, like `ui_state.json`, it is
//! **optional in both directions** and therefore does not move
//! `format_version`: an archive without one loads exactly as before (the
//! session keeps the user's own layout), and one that cannot be read degrades
//! to the same thing with a warning. It is also only *written* when the user
//! turned the opt-in on, so an ordinary save produces byte-identical archives
//! to the ones this build produced before the entry existed.

pub mod atomic_write;
pub mod container;
pub mod graph_doc;
pub mod manifest;
pub mod migration;
pub mod paths;
pub mod settings;
pub mod subgraph_template;
pub mod timestamp;
pub mod ui_state;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

use ravel_core::composition::{Composition, Document};
use ravel_core::id::CompId;
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::types::FrameRate;

use crate::graph_doc::{GraphDoc, GraphDocError};
use crate::manifest::{Manifest, RationalRate, Resolution};
use crate::settings::{ResolvedSettings, SettingsLayer};
use crate::ui_state::UiState;
use ravel_ui::layout_doc::LayoutDocument;

/// Aggregate error type for project load/save operations.
#[derive(Debug, Error)]
pub enum ProjectError {
    #[error(transparent)]
    Container(#[from] container::ContainerError),

    #[error(transparent)]
    Migration(#[from] migration::MigrationError),

    #[error(transparent)]
    Graph(#[from] GraphDocError),

    #[error("failed to parse document/main.ron: {0}")]
    DocumentParse(#[source] ron::de::SpannedError),

    #[error("failed to serialize the document to RON: {0}")]
    DocumentSerialize(#[source] ron::Error),

    #[error("the document is structurally invalid: {0}")]
    InvalidDocument(#[from] ravel_core::composition::DocumentValidationError),

    #[error("failed to parse manifest.json: {0}")]
    Manifest(#[source] serde_json::Error),

    #[error("failed to serialize JSON: {0}")]
    JsonSerialize(#[source] serde_json::Error),

    #[error("failed to parse settings.toml: {0}")]
    SettingsParse(#[from] toml::de::Error),

    #[error("failed to serialize workspace_layout.toml: {0}")]
    WorkspaceLayoutSerialize(#[source] ravel_ui::layout_doc::LayoutDocError),

    #[error("failed to serialize settings.toml: {0}")]
    SettingsSerialize(#[from] toml::ser::Error),

    #[error("failed to load both the project ({primary}) and its backup ({backup})")]
    RecoveryFailed {
        primary: Box<ProjectError>,
        backup: Box<ProjectError>,
    },
}

impl ProjectError {
    pub fn is_too_new(&self) -> bool {
        matches!(
            self,
            Self::Migration(migration::MigrationError::TooNew { .. })
        )
    }
}

/// A project load that may have recovered the previous revision from `.bak`.
#[derive(Clone, Debug)]
pub struct ProjectLoad {
    pub project: ProjectFile,
    pub recovered_from: Option<PathBuf>,
}

/// A fully-loaded Ravel project.
#[derive(Clone, Debug)]
pub struct ProjectFile {
    pub manifest: Manifest,
    pub document: Document,
    /// The project-level settings layer (highest priority below the user layer).
    pub settings: SettingsLayer,
    /// Persisted UI state — deliberately outside the document so a
    /// composition switch is neither an undo step nor a saved diff
    /// (REQ-UI-013).
    pub ui_state: UiState,
    /// The workspace layout this project ships, when its author opted in.
    ///
    /// `None` — the default, and what every project written before this entry
    /// existed has — means "open me in whatever layout the user works in".
    /// A layout that *is* present applies to that session only; it never
    /// becomes the user's own default (`ravel-app`'s `layout_persist`).
    pub workspace_layout: Option<LayoutDocument>,
}

impl ProjectFile {
    /// Build a new, empty project with the given name and creation timestamp.
    ///
    /// `created_at` is supplied by the caller (RFC 3339 string) so this crate
    /// stays free of a wall-clock dependency.
    pub fn new(project_name: impl Into<String>, created_at: impl Into<String>) -> Self {
        Self {
            manifest: Manifest::new(project_name, created_at),
            document: Document::default(),
            settings: SettingsLayer::default(),
            ui_state: UiState::default(),
            workspace_layout: None,
        }
    }

    /// Build a project around an existing [`Document`]; the manifest's frame
    /// rate and resolution are stamped from the root composition.
    pub fn from_document(
        project_name: impl Into<String>,
        created_at: impl Into<String>,
        document: Document,
    ) -> Self {
        let mut project = Self::new(project_name, created_at);
        if let Some(root) = document
            .root_comp
            .and_then(|id| document.get_composition(id))
        {
            project.manifest.frame_rate =
                RationalRate::new(root.frame_rate.num, root.frame_rate.den);
            project.manifest.resolution = Resolution::new(root.resolution.0, root.resolution.1);
        }
        project.document = document;
        project
    }

    /// Encode this project into an in-memory [`container::RawArchive`],
    /// leaving every asset reference in the form the document already holds.
    ///
    /// Prefer [`ProjectFile::to_archive_for_root`] on any path that knows
    /// where the archive will live — only that form can store references
    /// relative to the project (REQ-PROJ-001).
    pub fn to_archive(&self) -> Result<container::RawArchive, ProjectError> {
        self.to_archive_for_root(None)
    }

    /// Encode this project for an archive stored in `project_root`.
    ///
    /// Asset references are rewritten against that root: a file inside the
    /// project becomes relative, anything else stays absolute, and a
    /// variable path the user set is preserved. The rewrite applies to the
    /// snapshot being written, never to `self.document`, so saving does not
    /// count as an edit.
    pub fn to_archive_for_root(
        &self,
        project_root: Option<&Path>,
    ) -> Result<container::RawArchive, ProjectError> {
        let mut archive = container::RawArchive::new();

        let manifest_json =
            serde_json::to_string_pretty(&self.manifest).map_err(ProjectError::JsonSerialize)?;
        archive.insert(container::entry::MANIFEST, manifest_json.into_bytes());

        // "A document that saves opens again" is an invariant of the format,
        // not a hope: the writer has no recursion limit of its own, so without
        // this check a nesting depth past `MAX_SUBNET_DEPTH` produces a file
        // that this very build refuses to parse — a loss the user cannot see
        // at save time. Refusing here keeps the document in memory, where it
        // can still be flattened or undone.
        self.document.validate_subnet_depth()?;
        let stored = self.document.clone().with_relativized_assets(project_root);
        let document_ron = document_to_ron(&stored)?;
        archive.insert(container::entry::DOCUMENT, document_ron.into_bytes());

        let settings_toml = self.settings.to_toml()?;
        archive.insert(container::entry::SETTINGS, settings_toml.into_bytes());

        let ui_state_json = self
            .ui_state
            .to_json()
            .map_err(ProjectError::JsonSerialize)?;
        archive.insert(container::entry::UI_STATE, ui_state_json.into_bytes());

        // Opt-in only: an archive without the entry is the norm, and adding it
        // must not change what every other project looks like on disk.
        //
        // The entry is a convenience, so an encode failure drops it instead of
        // failing the save — the read direction degrades the same way, and
        // losing the document because its window arrangement would not encode
        // is never the right trade.
        if let Some(layout) = &self.workspace_layout {
            match layout.to_toml() {
                Ok(toml) => {
                    archive.insert(container::entry::WORKSPACE_LAYOUT, toml.into_bytes());
                }
                Err(err) => {
                    tracing::warn!(%err, "omitting unencodable workspace_layout.toml");
                }
            }
        }

        Ok(archive)
    }

    /// Decode a project from a [`container::RawArchive`], running migrations.
    pub fn from_archive(archive: &container::RawArchive) -> Result<Self, ProjectError> {
        // Manifest: parse untyped, remember the source version (it selects
        // the archive layout below), migrate, then strongly type.
        let manifest_text = archive.require_text(container::entry::MANIFEST)?;
        let mut manifest_value: serde_json::Value =
            serde_json::from_str(manifest_text).map_err(ProjectError::Manifest)?;
        let source_version = migration::read_version(&manifest_value)?;
        migration::migrate_to_current(&mut manifest_value)?;
        let manifest: Manifest =
            serde_json::from_value(manifest_value).map_err(ProjectError::Manifest)?;

        // Document: v3+ archives carry document/main.ron (required — a v3+
        // archive without one is corrupt, not legacy). v1/v2 archives carry
        // only the legacy flat graph (graph/main.ron), which is wrapped in a
        // fresh Document (the archive-level half of the v2→v3 migration).
        let document = if source_version >= 3 {
            let text = archive.require_text(container::entry::DOCUMENT)?;
            let mut registry = NodeRegistry::new();
            register_builtins(&mut registry);
            // `normalize_node_type_aliases`: archives written before the
            // node rename carry `type_key: "video"`; rewrite to the
            // canonical `media` first so the registry-dependent
            // normalizations below see only canonical keys.
            // `normalize_param_ports`: archives written before parameter
            // ports existed deserialize pre-exposed pins (e.g. rasterize
            // `color`) with `is_param: false`; upgrade them so connected
            // pins keep driving their parameter.
            // `normalize_net_in_ports`: archives written before the frame
            // index port existed get `f` appended to each layer's In node.
            // `normalize_variadic_input_ports`: template-declared trailing
            // groups gain membership flags and one empty trailing slot.
            // `sync_subnet_pins`: a subnet's pins are derived from its inner
            // In / Out, so they run last — after the two normalizations above
            // have finished moving those very ports around.
            let document = ron_options()
                .from_str::<Document>(text)
                .map_err(ProjectError::DocumentParse)?;
            // Reject hostile nesting before the recursive compatibility
            // normalizers below get a chance to consume the process stack.
            document.validate_subnet_depth()?;
            document
                .normalize_node_type_aliases()
                .normalize_param_ports()
                .normalize_net_in_ports()
                .normalize_variadic_input_ports(&registry)
                .sync_subnet_pins()
                // Absolute references need no project root, so resolve them
                // here; `load` re-runs this with the real root to reach the
                // relative and variable ones.
                .with_resolved_assets(None, &HashMap::new())
        } else {
            let graph_text = archive.require_text(container::entry::GRAPH)?;
            let graph = GraphDoc::graph_from_ron(graph_text)?;
            // The legacy flat graph is preserved on `Document::graph` but is
            // NOT evaluated: evaluation pulls the root composition's layer
            // networks (REQ-LAYER-007). A fresh root composition seeded from
            // the manifest becomes the editable document content.
            let root = Composition::new(
                CompId::next(),
                "Comp 1",
                (manifest.resolution.width, manifest.resolution.height),
                frame_rate_or_default(manifest.frame_rate),
                300,
            );
            Document::new(graph).with_composition(root)
        };
        // Reject structurally invalid documents on every path (bad frame
        // rates, missing roots, duplicate or exhausted ids) before anything
        // uses them.
        document.validate()?;
        // REQ-LAYER-009: ids minted after the load must never collide with
        // ids stored in the document.
        document.advance_id_counters();
        // v4 → v5: fold `_x` / `_y` component parameters into the `Channel2` /
        // `Channel3` vector parameters the templates now declare. This mints
        // node and edge ids for the `vector.construct` nodes that preserve
        // separately driven component ports, so it runs after the counters
        // have been advanced past the document's own ids.
        let document = if source_version < 5 {
            let folded = document.fold_component_params();
            folded.validate()?;
            folded
        } else {
            document
        };
        // v5 → v6: convert curve parameters stored as `"in:out,…"` strings
        // into `ParameterValue::Curve`. Mints no ids, so its position relative
        // to the fold above is free.
        let document = if source_version < 6 {
            let upgraded = document.upgrade_curve_params();
            upgraded.validate()?;
            upgraded
        } else {
            document
        };
        // v7 → v8: the pipeline became linear, so every authored colour is
        // reinterpreted once. Gated on the source version and nowhere else:
        // `srgb → linear` is not idempotent, so running it twice would darken
        // the project each time it is opened.
        let document = if source_version < 8 {
            let mut registry = NodeRegistry::new();
            register_builtins(&mut registry);
            let (upgraded, report) = document.linearize_colors(&registry);
            upgraded.validate()?;
            report_color_migration(&report);
            upgraded
        } else {
            document
        };

        // Settings (optional — absence yields an empty layer).
        let settings = match archive.get(container::entry::SETTINGS) {
            Some(bytes) => {
                let text = std::str::from_utf8(bytes).map_err(|_| {
                    ProjectError::Container(container::ContainerError::NotUtf8 {
                        name: container::entry::SETTINGS.to_string(),
                    })
                })?;
                SettingsLayer::from_toml(text)?
            }
            None => SettingsLayer::default(),
        };

        // UI state (optional — absence is the pre-REQ-UI-013 layout and
        // every older format version, so it never bumps `format_version`).
        // Unreadable content degrades to the default instead of failing the
        // load: this entry carries no user data, and refusing to open an
        // otherwise intact project over it would be the worse failure. The
        // warning keeps a writer bug visible.
        let mut ui_state = archive
            .get(container::entry::UI_STATE)
            .and_then(|bytes| match std::str::from_utf8(bytes) {
                Ok(text) => UiState::from_json(text)
                    .inspect_err(|err| {
                        tracing::warn!(%err, "ignoring unreadable ui_state.json");
                    })
                    .ok(),
                Err(err) => {
                    tracing::warn!(%err, "ignoring non-UTF-8 ui_state.json");
                    None
                }
            })
            .unwrap_or_default();
        // Normalize at the boundary: a loaded state can never name a
        // composition this document does not have, so a caller reading
        // `active_comp` directly cannot resurrect a dangling id (the root
        // fallback itself stays in `initial_active_comp`).
        ui_state.active_comp = ui_state
            .active_comp
            .filter(|id| document.get_composition(*id).is_some());

        // Workspace layout (optional, opt-in). Unreadable content degrades to
        // "no embedded layout" for the same reason `ui_state.json` does: it
        // carries no user data, and the project itself is intact. A layout
        // written by a newer Ravel lands here too, which is exactly the
        // fallback its version stamp exists for.
        let workspace_layout = archive
            .get(container::entry::WORKSPACE_LAYOUT)
            .and_then(|bytes| match std::str::from_utf8(bytes) {
                Ok(text) => LayoutDocument::from_toml(text)
                    .inspect_err(|err| {
                        tracing::warn!(%err, "ignoring unreadable workspace_layout.toml");
                    })
                    .ok(),
                Err(err) => {
                    tracing::warn!(%err, "ignoring non-UTF-8 workspace_layout.toml");
                    None
                }
            });

        Ok(Self {
            manifest,
            document,
            settings,
            ui_state,
            workspace_layout,
        })
    }

    /// Save the project to `path`, backing up any existing file to `<path>.bak`.
    ///
    /// The directory holding `path` is the project root, so `Save As` into a
    /// new location rewrites asset references to match it.
    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        let archive = self.to_archive_for_root(project_root_of(path).as_deref())?;
        container::write_file(path, &archive)?;
        Ok(())
    }

    /// Load a project from `path`, migrating older format versions in place
    /// and resolving asset references against the directory holding `path`.
    pub fn load(path: &Path) -> Result<Self, ProjectError> {
        let archive = container::read_file(path)?;
        let mut project = Self::from_archive(&archive)?;
        project.document = project
            .document
            .with_resolved_assets(project_root_of(path).as_deref(), &HashMap::new());
        Ok(project)
    }

    /// Load `path`, falling back to its validated `.bak` revision when the
    /// primary archive is unreadable. A project written by a newer Ravel is
    /// never replaced with an older backup: that would silently roll the
    /// user's work back instead of reporting the compatibility problem.
    pub fn load_with_backup(path: &Path) -> Result<ProjectLoad, ProjectError> {
        match Self::load(path) {
            Ok(project) => Ok(ProjectLoad {
                project,
                recovered_from: None,
            }),
            Err(primary) if primary.is_too_new() => Err(primary),
            Err(primary) => {
                let backup = container::backup_path(path);
                if !backup.exists() {
                    return Err(primary);
                }
                match Self::load(&backup) {
                    Ok(project) => Ok(ProjectLoad {
                        project,
                        recovered_from: Some(backup),
                    }),
                    Err(backup) => Err(ProjectError::RecoveryFailed {
                        primary: Box::new(primary),
                        backup: Box::new(backup),
                    }),
                }
            }
        }
    }

    /// Resolve effective settings by layering this project's settings between
    /// optional `global` and `user` layers (`default → global → project →
    /// user`).
    pub fn resolved_settings(
        &self,
        global: Option<&SettingsLayer>,
        user: Option<&SettingsLayer>,
    ) -> ResolvedSettings {
        let mut layers: Vec<SettingsLayer> = Vec::new();
        if let Some(g) = global {
            layers.push(g.clone());
        }
        layers.push(self.settings.clone());
        if let Some(u) = user {
            layers.push(u.clone());
        }
        ResolvedSettings::from_layers(&layers)
    }
}

/// Log what the v7 → v8 colour pass could not convert.
///
/// A load must not silently change a project's look, and it must not
/// silently *fail* to. Every note the pass returns names the node and the
/// parameter, so a colour that did not move can be found and fixed by hand.
fn report_color_migration(report: &ravel_core::composition::ColorMigrationReport) {
    if report.converted > 0 {
        tracing::info!(
            channels = report.converted,
            "reinterpreted authored colours for the linear working space (.ravprj v7 → v8)"
        );
    }
    for note in &report.keyframed {
        tracing::warn!(
            node = note.node.raw(),
            node_type = note.type_key,
            key = note.param,
            "keyframed colour: the keys were converted, but frames between them \
             no longer interpolate the same way"
        );
    }
    for note in &report.unresolved {
        tracing::warn!(
            node = note.node.raw(),
            node_type = note.type_key,
            key = note.param,
            "colour driven by an expression, another node, or a blend: not converted, \
             so it now means linear light instead of a display value"
        );
    }
    for note in &report.undecidable {
        tracing::warn!(
            node = note.node.raw(),
            node_type = note.type_key,
            key = note.param,
            "cannot tell whether this parameter is a colour (unknown node type or \
             undeclared parameter): left unconverted"
        );
    }
}

/// The RON reader options every entry of a project container is parsed with.
///
/// RON's default recursion budget (128) is below what a document nested to
/// [`MAX_SUBNET_DEPTH`](ravel_core::composition::MAX_SUBNET_DEPTH) costs, so
/// leaving it at the default made a saved document unreadable at a nesting
/// depth the writer happily accepted. The budget is stated once, in
/// `ravel-core` beside the depth limit it has to cover, and every reader here
/// takes it from there — a second reader on the default would reintroduce the
/// asymmetry for whichever entry it parses.
fn ron_options() -> ron::Options {
    ron::Options::default().with_recursion_limit(ravel_core::composition::RON_RECURSION_LIMIT)
}

/// Serialize a [`Document`] to pretty RON (same style as [`GraphDoc`]:
/// struct names, two-space indent).
fn document_to_ron(document: &Document) -> Result<String, ProjectError> {
    let config = ron::ser::PrettyConfig::new()
        .struct_names(true)
        .indentor("  ".to_string());
    ron::ser::to_string_pretty(document, config).map_err(ProjectError::DocumentSerialize)
}

/// The project root for an archive stored at `path`: the directory that
/// contains it. Asset references are stored relative to this and resolved
/// against it (REQ-PROJ-001).
///
/// The result is always absolute, because `resolved` is contractually an
/// absolute location — anchoring against a relative `dir/demo.ravprj` would
/// produce `dir/footage/clip.mov`, which breaks the moment anything changes
/// the working directory. A relative argument is absolutised lexically
/// (never canonicalised) so this also works for a `Save As` destination that
/// does not exist yet.
///
/// `None` when there is no directory to anchor against, which leaves
/// references absolute rather than silently rooting them at the process's
/// working directory.
pub fn project_root_of(path: &Path) -> Option<PathBuf> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty())?;
    if parent.is_absolute() {
        return Some(parent.to_path_buf());
    }
    std::env::current_dir().ok().map(|cwd| cwd.join(parent))
}

/// Convert a manifest [`RationalRate`] to a [`FrameRate`]. A zero denominator
/// (corrupt input) falls back to the default rate rather than panicking —
/// [`FrameRate::new`] asserts on it.
fn frame_rate_or_default(rate: RationalRate) -> FrameRate {
    if rate.den == 0 {
        FrameRate::new(30, 1)
    } else {
        FrameRate::new(rate.num, rate.den)
    }
}

/// Best-effort read of an existing project file's `created_at` timestamp, so
/// overwriting a project keeps its original creation time. `None` when the
/// file is missing, unreadable, or lacks the field.
pub fn read_created_at(path: &Path) -> Option<String> {
    let archive = container::read_file(path).ok()?;
    let text = archive.require_text(container::entry::MANIFEST).ok()?;
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(value.get("created_at")?.as_str()?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use ravel_core::animation::channel::AnimationChannel;
    use ravel_core::animation::curve::KeyframeCurve;
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_core::composition::{
        AssetKind, AssetMetadata, AssetPath, AudioSource, BlendMode, Layer, MediaAssetEntry,
        TrackMatte, TrackMatteKind,
    };
    use ravel_core::exposed::{
        ExposedBinding, ExposedParameter, ExposedParameters, ExposedType, ExposedValue,
    };
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex};
    use ravel_core::network as net;
    use ravel_core::runtime::playback::LoopRange;
    use std::collections::BTreeMap;

    use crate::manifest::CURRENT_FORMAT_VERSION;
    use crate::settings::{ColorLayer, ProxyMode};

    fn keyframed_channel(keys: &[(u64, f32)]) -> AnimationChannel {
        let mut curve = KeyframeCurve::new();
        for &(frame, value) in keys {
            curve.insert(frame, value, Interpolation::Linear);
        }
        AnimationChannel::keyframes(curve)
    }

    /// A document exercising everything the v4 format must persist: a layered
    /// root composition (parenting, adjustment, blend mode, solo/mute/locked,
    /// reserved fields), a network with keyframed custom parameters and a
    /// nested subnet, the legacy flat graph, and media assets.
    fn demo_document() -> Document {
        // Layer network: net.in (keyframed custom param) + subnet + net.out.
        let inner = Graph::new()
            .add_node(
                Node::new(NodeId::new(110), "constant").with_output("value", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(111), "grade")
                    .with_input("in", &[DataTypeId::SCALAR])
                    .with_output("out", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(112),
                NodeId::new(110),
                OutputPortIndex(0),
                NodeId::new(111),
                InputPortIndex(0),
            )
            .unwrap();
        let network = Graph::new()
            .add_node(
                Node::new(NodeId::new(100), net::NET_IN_TYPE_KEY)
                    .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
                    .with_output(net::PORT_TIME, DataTypeId::SCALAR)
                    .with_output("intensity", DataTypeId::SCALAR)
                    // Current-format In nodes carry `f`; without it the
                    // load-time port normalization would append one and the
                    // roundtrip would no longer be exact.
                    .with_output(net::PORT_FRAME_INDEX, DataTypeId::SCALAR)
                    .with_param(
                        "intensity",
                        ParameterValue::Channel(keyframed_channel(&[(0, 0.0), (24, 1.0)])),
                    ),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(101), net::NET_OUT_TYPE_KEY)
                    .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(120), "subnet")
                    .with_subnet(inner)
                    .with_output("out", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(121),
                NodeId::new(120),
                OutputPortIndex(0),
                NodeId::new(101),
                InputPortIndex(0),
            )
            .unwrap();

        // A fully-dressed layer: keyframed opacity/audio gain, reserved fields
        // set (time_remap, track_matte), adjustment + parent + solo.
        let hero = Layer::new(LayerId::new(11), "Hero", network)
            .with_time(-10, 5, 120)
            .with_blend_mode(BlendMode::Multiply)
            .with_parent(LayerId::new(12));
        let hero = Layer {
            opacity: keyframed_channel(&[(0, 0.0), (30, 1.0)]),
            audio: Some(AudioSource {
                asset_id: "plate".into(),
                stream_index: 1,
                gain: keyframed_channel(&[(0, 1.0), (30, 0.75)]),
                fade_in_frames: 4,
                fade_out_frames: 8,
                audio_muted: true,
            }),
            adjustment: true,
            solo: true,
            time_remap: Some(keyframed_channel(&[(0, 0.0), (60, 60.0)])),
            track_matte: Some(TrackMatte {
                layer: LayerId::new(12),
                kind: TrackMatteKind::Luma,
            }),
            ..hero
        };
        let matte = Layer {
            muted: true,
            locked: true,
            ..Layer::new(LayerId::new(12), "Matte", Graph::new()).with_time(0, 0, 300)
        };

        let comp = Composition::new(
            CompId::new(1),
            "Hero Comp",
            (1280, 720),
            FrameRate::new(24, 1),
            300,
        )
        .add_layer(hero)
        .add_layer(matte);

        // Legacy flat graph (preserved as-is).
        let flat = Graph::new()
            .add_node(
                Node::new(NodeId::new(1), "constant").with_output("value", DataTypeId::SCALAR),
            )
            .unwrap();

        Document::new(flat)
            .with_composition(comp)
            .with_media_asset("plate", "/tmp/media/plate.mov")
            // The project's external contract (REQ-PROJ-006, format v7).
            .with_exposed_parameters(demo_declarations())
    }

    /// Declarations covering the three shapes an [`ExposedValue`] takes: a
    /// plain constant, a component value, and an asset path.
    fn demo_declarations() -> ExposedParameters {
        ExposedParameters::from_declarations([
            ExposedParameter::new(
                "intensity",
                ExposedType::Float,
                ExposedValue::Float(0.5),
                ExposedBinding::new(NodeId::new(100), "intensity"),
            )
            .unwrap()
            .with_description("How hard the effect hits"),
            ExposedParameter::inferred(
                "tint",
                ExposedValue::Color(ravel_core::types::Color::new(1.0, 0.5, 0.25, 1.0)),
                ExposedBinding::new(NodeId::new(100), "tint"),
            )
            .unwrap(),
            ExposedParameter::inferred(
                "plate",
                ExposedValue::Media(AssetPath::Relative("./footage/plate.mov".into())),
                ExposedBinding::new(NodeId::new(1), "asset_id"),
            )
            .unwrap(),
        ])
        .expect("the names differ")
    }

    fn demo_project() -> ProjectFile {
        let mut project =
            ProjectFile::from_document("Round Trip", "2026-06-22T10:00:00Z", demo_document());
        project.settings.color = ColorLayer {
            working_space: Some("ACEScg".into()),
            ..Default::default()
        };
        project
    }

    /// A current-format archive holding only the required entries — the
    /// layout written before the optional `ui_state.json` entry existed.
    fn archive_without_optional_entries(project: &ProjectFile) -> container::RawArchive {
        let mut archive = container::RawArchive::new();
        archive.insert(
            container::entry::MANIFEST,
            serde_json::to_string_pretty(&project.manifest)
                .unwrap()
                .into_bytes(),
        );
        archive.insert(
            container::entry::DOCUMENT,
            document_to_ron(&project.document).unwrap().into_bytes(),
        );
        archive
    }

    /// The composition's guides survive a save/load cycle, and the format
    /// version does not move: they are an additive field with a `serde`
    /// default, the treatment `Layer.audio` had (SNAP-2).
    #[test]
    fn the_archive_round_trips_the_composition_guides() {
        use ravel_core::composition::Guide;

        let mut project = demo_project();
        let root = project.document.root_comp.expect("root comp");
        let placed = vec![Guide::vertical(960.0), Guide::horizontal(120.5)];
        project.document = ravel_ui::document::update_composition(&project.document, root, {
            let placed = placed.clone();
            |mut comp| {
                comp.guides = placed;
                comp
            }
        })
        .expect("the root composition");

        let back = ProjectFile::from_archive(&project.to_archive().unwrap()).unwrap();
        assert_eq!(back.manifest.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(
            back.document.get_composition(root).unwrap().guides,
            placed,
            "the guides come back in the order they were placed"
        );
        assert_eq!(back.document, project.document);
    }

    /// An archive written before guides existed carries no `guides` field. It
    /// is a current-version archive — no migration was added for the additive
    /// field — so it has to load as it stands, with no guides.
    #[test]
    fn a_current_archive_without_the_guides_field_loads() {
        let project = demo_project();
        let ron = document_to_ron(&project.document).unwrap();
        assert!(ron.contains("guides:"), "the field was there to strip");
        // By line rather than by substring: the serializer ends its lines the
        // platform's way, so a pattern carrying `\n` strips nothing on Windows
        // and the test would pass while proving the opposite.
        let stripped: String = ron
            .lines()
            .filter(|line| !line.trim_start().starts_with("guides:"))
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(
            !stripped.contains("guides:"),
            "every composition lost the field"
        );

        let mut archive = archive_without_optional_entries(&project);
        archive.insert(container::entry::DOCUMENT, stripped.into_bytes());

        let back = ProjectFile::from_archive(&archive).unwrap();
        assert_eq!(back.manifest.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(back.document, project.document, "nothing else moved");
        for comp in back.document.compositions.values() {
            assert!(comp.guides.is_empty());
        }
    }

    /// The active composition survives a save/load cycle (REQ-UI-013).
    #[test]
    fn archive_roundtrip_restores_the_active_composition() {
        let mut project = demo_project();
        let root = project.document.root_comp.expect("root comp");
        // Distinct from the root by construction: `CompId::next()` can
        // still be at 1 in a fresh test process.
        let other = CompId::new(root.raw() + 1000);
        project.document = project.document.clone().with_composition(Composition::new(
            other,
            "Comp 2",
            (1080, 1080),
            FrameRate::new(30, 1),
            120,
        ));
        project.ui_state = ui_state::UiState::with_active_comp(Some(other));

        let back = ProjectFile::from_archive(&project.to_archive().unwrap()).unwrap();
        assert_eq!(back.ui_state.active_comp, Some(other));
        assert_eq!(
            back.ui_state.initial_active_comp(&back.document),
            Some(other)
        );
        assert_ne!(other, root, "the restored composition is not just the root");
        // The switch is UI state only: the document root is untouched.
        assert_eq!(back.document.root_comp, Some(root));
    }

    /// The loop ranges ride the same optional entry, so a project saved with
    /// one comes back with it and the format version stays put.
    #[test]
    fn the_archive_round_trips_a_per_composition_loop_range() {
        let mut project = demo_project();
        let root = project.document.root_comp.expect("demo project has a root");
        project.ui_state.loop_ranges = vec![(root, LoopRange::new(10, 40))];

        let back = ProjectFile::from_archive(&project.to_archive().unwrap()).unwrap();
        assert_eq!(back.manifest.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(
            back.ui_state.loop_ranges(&back.document),
            BTreeMap::from([(root, LoopRange::new(10, 40))])
        );
    }

    /// A current-format archive may omit `ui_state.json`; it must still load,
    /// falling back to the document root. The optional entry never requires a
    /// format migration.
    #[test]
    fn a_current_archive_without_ui_state_loads_and_falls_back_to_the_root() {
        let project = demo_project();
        let archive = archive_without_optional_entries(&project);
        assert!(archive.get(container::entry::UI_STATE).is_none());

        let back = ProjectFile::from_archive(&archive).unwrap();
        assert_eq!(back.manifest.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(back.ui_state, ui_state::UiState::default());
        assert_eq!(
            back.ui_state.initial_active_comp(&back.document),
            back.document.root_comp
        );
    }

    /// A corrupt UI-state entry must not cost the user their project: the
    /// document still loads and the UI falls back to the root composition.
    #[test]
    fn an_unreadable_ui_state_entry_degrades_to_the_default() {
        let project = demo_project();
        let mut archive = project.to_archive().unwrap();
        archive.insert(container::entry::UI_STATE, b"{ not json".to_vec());

        let back = ProjectFile::from_archive(&archive).expect("the project still loads");
        assert_eq!(back.ui_state, ui_state::UiState::default());
        assert_eq!(back.document, project.document);
        assert_eq!(
            back.ui_state.initial_active_comp(&back.document),
            back.document.root_comp
        );
    }

    /// Non-UTF-8 content takes the same degrade path as malformed JSON.
    #[test]
    fn a_non_utf8_ui_state_entry_degrades_to_the_default() {
        let mut archive = demo_project().to_archive().unwrap();
        archive.insert(container::entry::UI_STATE, vec![0xff, 0xfe, 0xfd]);

        let back = ProjectFile::from_archive(&archive).expect("the project still loads");
        assert_eq!(back.ui_state, ui_state::UiState::default());
    }

    /// A persisted id whose composition is gone is dropped while loading, so
    /// no consumer can act on a dangling reference.
    #[test]
    fn a_dangling_active_composition_is_dropped_on_load() {
        let mut project = demo_project();
        project.ui_state = ui_state::UiState::with_active_comp(Some(CompId::new(9_999)));

        let back = ProjectFile::from_archive(&project.to_archive().unwrap()).unwrap();
        assert_eq!(back.ui_state.active_comp, None);
        assert_eq!(
            back.ui_state.initial_active_comp(&back.document),
            back.document.root_comp
        );
    }

    /// Writing the new entry must not change how the archive is versioned.
    #[test]
    fn ui_state_does_not_bump_the_format_version() {
        let archive = demo_project().to_archive().unwrap();
        assert!(archive.get(container::entry::UI_STATE).is_some());
        let manifest: serde_json::Value =
            serde_json::from_str(archive.require_text(container::entry::MANIFEST).unwrap())
                .unwrap();
        assert_eq!(manifest["format_version"], CURRENT_FORMAT_VERSION);
    }

    // -- workspace_layout.toml (opt-in, DOCK-9) ------------------------------

    fn demo_layout() -> LayoutDocument {
        LayoutDocument::new(
            ravel_ui::layout::WorkspaceLayout::new(ravel_ui::layout::LayoutNode::area(vec![
                ravel_ui::layout::PanelInstance::new(
                    ravel_ui::layout::PanelInstanceId(0),
                    ravel_ui::panel::PanelKind::NodeGraph,
                ),
            ]))
            .unwrap(),
        )
    }

    /// An ordinary save writes no layout entry at all, so opting out costs
    /// nothing and older readers see exactly the archive they always did.
    #[test]
    fn a_project_without_the_opt_in_writes_no_layout_entry() {
        let archive = demo_project().to_archive().unwrap();
        assert!(archive.get(container::entry::WORKSPACE_LAYOUT).is_none());
        let back = ProjectFile::from_archive(&archive).unwrap();
        assert_eq!(back.workspace_layout, None);
        assert_eq!(back.manifest.format_version, CURRENT_FORMAT_VERSION);
    }

    /// With the opt-in on, the layout round-trips and the archive's format
    /// version is still untouched — a new optional entry never migrates.
    #[test]
    fn an_embedded_layout_roundtrips_without_bumping_the_format_version() {
        let mut project = demo_project();
        project.workspace_layout = Some(demo_layout());

        let archive = project.to_archive().unwrap();
        assert!(archive.get(container::entry::WORKSPACE_LAYOUT).is_some());
        let back = ProjectFile::from_archive(&archive).unwrap();
        assert_eq!(back.workspace_layout, Some(demo_layout()));
        assert_eq!(back.manifest.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(back.document, project.document);
    }

    /// A corrupt or future-versioned layout entry must not cost the user their
    /// project: it loads as "no embedded layout".
    #[test]
    fn an_unreadable_embedded_layout_degrades_to_none() {
        let future = format!(
            "layout_version = {}\n[layout]\n",
            ravel_ui::layout_doc::LAYOUT_VERSION + 1
        );
        for bytes in [
            b"{ not toml".to_vec(),
            b"layout_version = 1\n[layout]\nwindows".to_vec(),
            vec![0xff, 0xfe, 0xfd],
            future.into_bytes(),
        ] {
            let project = demo_project();
            let mut archive = project.to_archive().unwrap();
            archive.insert(container::entry::WORKSPACE_LAYOUT, bytes);

            let back = ProjectFile::from_archive(&archive).expect("the project still loads");
            assert_eq!(back.workspace_layout, None);
            assert_eq!(back.document, project.document);
        }
    }

    /// A project saved with the opt-in on, then saved again with it off, stops
    /// carrying the entry — turning the toggle back off has to be effective.
    #[test]
    fn clearing_the_opt_in_removes_the_entry_from_the_next_save() {
        let mut project = demo_project();
        project.workspace_layout = Some(demo_layout());
        let with = project.to_archive().unwrap();
        project.workspace_layout = None;
        let without = project.to_archive().unwrap();

        assert!(with.get(container::entry::WORKSPACE_LAYOUT).is_some());
        assert!(without.get(container::entry::WORKSPACE_LAYOUT).is_none());
        assert_eq!(
            without,
            demo_project().to_archive().unwrap(),
            "an opted-out archive is identical to one written before the entry existed"
        );
    }

    /// Hand-craft a pre-v3 archive (manifest + graph/main.ron only).
    fn legacy_archive(manifest_json: &str, graph: &Graph) -> container::RawArchive {
        let mut archive = container::RawArchive::new();
        archive.insert(
            container::entry::MANIFEST,
            manifest_json.as_bytes().to_vec(),
        );
        archive.insert(
            container::entry::GRAPH,
            GraphDoc::graph_to_ron(graph).unwrap().into_bytes(),
        );
        archive
    }

    fn legacy_graph() -> Graph {
        Graph::new()
            .add_node(
                Node::new(NodeId::new(1), "read_media")
                    .with_output("out", DataTypeId::FRAME_BUFFER)
                    .with_position(100.0, 200.0),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(2), "color_correct")
                    .with_input("in", &[DataTypeId::FRAME_BUFFER])
                    .with_output("out", DataTypeId::FRAME_BUFFER)
                    .with_position(300.0, 200.0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap()
    }

    #[test]
    fn archive_roundtrip_restores_the_document_exactly() {
        let project = demo_project();
        let archive = project.to_archive().unwrap();
        let back = ProjectFile::from_archive(&archive).unwrap();

        // Full structural equality: layers, audio sources, networks,
        // keyframes, reserved fields, flat graph, and media assets all survive.
        assert_eq!(back.document, project.document);
        assert_eq!(back.manifest.project_name, "Round Trip");
        assert_eq!(back.settings.color.working_space.as_deref(), Some("ACEScg"));
        // The manifest is stamped from the root composition.
        assert_eq!(back.manifest.frame_rate, RationalRate::new(24, 1));
        assert_eq!(back.manifest.resolution, Resolution::new(1280, 720));
    }

    /// A project's declarations round-trip whole — name, type, default,
    /// description and binding — in the order they are presented, and a
    /// rewrite of the loaded project produces the same bytes (REQ-PROJ-006).
    #[test]
    fn archive_roundtrip_restores_exposed_parameter_declarations() {
        let project = demo_project();
        let archive = project.to_archive().unwrap();
        let back = ProjectFile::from_archive(&archive).unwrap();

        assert_eq!(back.document.exposed_parameters, demo_declarations());
        assert_eq!(
            back.document
                .exposed_parameters
                .iter()
                .map(ExposedParameter::name)
                .collect::<Vec<_>>(),
            ["intensity", "tint", "plate"],
            "the persisted order is the presentation order"
        );

        let declaration = back
            .document
            .exposed_parameters
            .get("intensity")
            .expect("the declaration is addressed by name");
        assert_eq!(declaration.value_type(), ExposedType::Float);
        assert_eq!(declaration.default_value(), &ExposedValue::Float(0.5));
        assert_eq!(declaration.description(), "How hard the effect hits");
        assert_eq!(
            declaration.binding(),
            &ExposedBinding::new(NodeId::new(100), "intensity"),
            "the binding is a node id plus a parameter key, not a path"
        );

        // Diff stability: the same contract writes the same bytes.
        assert_eq!(
            ProjectFile::from_archive(&archive)
                .unwrap()
                .to_archive()
                .unwrap()
                .get(container::entry::DOCUMENT),
            archive.get(container::entry::DOCUMENT),
        );
    }

    /// A headless caller — a CLI render, a template runner — has to be able to
    /// open a `.ravprj` and read its external contract in a machine-readable
    /// form (REQ-RENDER-005) without any part of the application. This test
    /// *is* that path: `ravel-project` never depends on `gpui`, and the whole
    /// route from archive to JSON runs here.
    #[test]
    fn a_loaded_project_lists_its_declarations_as_json() {
        use ravel_core::exposed::listing::ExposedListing;

        let archive = demo_project().to_archive().unwrap();
        let project = ProjectFile::from_archive(&archive).unwrap();

        let listing = ExposedListing::of(&project.document);
        let json = serde_json::to_string(&listing).expect("the listing is machine-readable");

        assert_eq!(
            json,
            concat!(
                r#"{"parameters":["#,
                r#"{"name":"intensity","type":"float","default":0.5,"#,
                r#""description":"How hard the effect hits","resolved":false},"#,
                r#"{"name":"tint","type":"color","default":[1.0,0.5,0.25,1.0],"#,
                r#""description":"","resolved":false},"#,
                r#"{"name":"plate","type":"media","default":"./footage/plate.mov","#,
                r#""description":"","resolved":false}"#,
                r#"]}"#,
            ),
            "the JSON contract names, types, defaults and descriptions — and \
             nothing about where the values land"
        );
    }

    /// The archive a Ravel of `version` wrote: the current writer's output with
    /// the version stamped back and the `exposed_parameters` field — which only
    /// v7 writes — cut out of the document.
    fn archive_without_declarations(version: u32) -> container::RawArchive {
        let mut project = demo_project();
        project.document = project
            .document
            .clone()
            .with_exposed_parameters(ExposedParameters::new());
        project.manifest.format_version = version;

        let mut archive = project.to_archive().unwrap();
        let text = document_to_ron(&project.document).unwrap();
        let kept: Vec<&str> = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("exposed_parameters:"))
            .collect();
        assert_eq!(
            kept.len() + 1,
            text.lines().count(),
            "exactly one line carried the field: {text}"
        );
        archive.insert(container::entry::DOCUMENT, kept.join("\n").into_bytes());
        archive
    }

    /// Every `document/main.ron` written before v7 lacks the declarations
    /// field. All of them must open — with no declarations, which is what a
    /// project with no external contract is — and rewrite cleanly.
    #[test]
    fn a_project_written_before_declarations_existed_opens_with_none() {
        for version in 3..CURRENT_FORMAT_VERSION {
            let archive = archive_without_declarations(version);
            let back = ProjectFile::from_archive(&archive)
                .unwrap_or_else(|err| panic!("a v{version} project still opens: {err}"));
            assert_eq!(back.manifest.format_version, CURRENT_FORMAT_VERSION);
            assert!(
                back.document.exposed_parameters.is_empty(),
                "a v{version} project declares nothing"
            );

            // The rewrite is v7 and still declares nothing: the upgrade
            // invents no contract.
            let reloaded = ProjectFile::from_archive(&back.to_archive().unwrap()).unwrap();
            assert_eq!(reloaded.manifest.format_version, CURRENT_FORMAT_VERSION);
            assert!(reloaded.document.exposed_parameters.is_empty());
            assert_eq!(
                reloaded.document, back.document,
                "the upgrade is idempotent"
            );
        }
    }

    /// A v1/v2 archive carries only the legacy flat graph, so it has no
    /// document to read the field from at all.
    #[test]
    fn a_legacy_flat_graph_project_opens_with_no_declarations() {
        let archive = legacy_archive(
            r#"{
                "format_version": 1,
                "ravel_version": "0.0.1",
                "project_name": "Legacy",
                "created_at": "2026-01-01T00:00:00Z",
                "modified_at": "2026-01-01T00:00:00Z",
                "frame_rate": { "num": 24, "den": 1 },
                "color_space": "aces_1.2"
            }"#,
            &legacy_graph(),
        );
        let back = ProjectFile::from_archive(&archive).unwrap();
        assert_eq!(back.manifest.format_version, CURRENT_FORMAT_VERSION);
        assert!(back.document.exposed_parameters.is_empty());
    }

    #[test]
    fn archive_serialization_is_byte_identical() {
        let project = demo_project();
        // Diff-friendly persistence: encoding twice is byte-identical.
        assert_eq!(project.to_archive().unwrap(), project.to_archive().unwrap());
    }

    #[test]
    fn corrupt_primary_recovers_the_previous_backup_revision() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recover.ravprj");
        let mut first = demo_project();
        first.manifest.project_name = "Previous revision".into();
        first.save(&path).unwrap();

        let mut second = demo_project();
        second.manifest.project_name = "Current revision".into();
        second.save(&path).unwrap();
        std::fs::write(&path, b"interrupted zip archive").unwrap();

        let loaded = ProjectFile::load_with_backup(&path).unwrap();
        assert_eq!(loaded.project.manifest.project_name, "Previous revision");
        assert_eq!(
            loaded.recovered_from.as_deref(),
            Some(container::backup_path(&path).as_path())
        );
    }

    #[test]
    fn too_new_primary_is_not_silently_replaced_by_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.ravprj");
        let project = demo_project();
        project.save(&path).unwrap();
        project.save(&path).unwrap();

        let mut future = project.to_archive().unwrap();
        let mut manifest: serde_json::Value =
            serde_json::from_str(future.require_text(container::entry::MANIFEST).unwrap()).unwrap();
        manifest["format_version"] = serde_json::Value::from(CURRENT_FORMAT_VERSION + 1);
        future.insert(
            container::entry::MANIFEST,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        );
        container::write_file(&path, &future).unwrap();

        assert!(matches!(
            ProjectFile::load_with_backup(&path),
            Err(ProjectError::Migration(
                migration::MigrationError::TooNew { .. }
            ))
        ));
    }

    // -- v4 → v5 component-parameter fold ------------------------------------

    /// A `shape.rect → rasterize → net.out` layer network in the v4 shape:
    /// `center_x` / `center_y` as separate Floats.
    fn v4_shape_network(center: (f32, f32)) -> Graph {
        let shape = Node::new(NodeId::new(500), "shape.rect")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("center_x", ParameterValue::Float(center.0))
            .with_param("center_y", ParameterValue::Float(center.1))
            .with_param("width", ParameterValue::Float(20.0))
            .with_param("height", ParameterValue::Float(20.0));
        let rasterize = Node::new(NodeId::new(501), "rasterize")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::FRAME_BUFFER);
        let in_node = Node::new(NodeId::new(502), net::NET_IN_TYPE_KEY)
            .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
            .with_output(net::PORT_TIME, DataTypeId::SCALAR)
            .with_output(net::PORT_FRAME_INDEX, DataTypeId::SCALAR);
        let out_node = Node::new(NodeId::new(503), net::NET_OUT_TYPE_KEY)
            .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        Graph::new()
            .add_node(shape)
            .unwrap()
            .add_node(rasterize)
            .unwrap()
            .add_node(in_node)
            .unwrap()
            .add_node(out_node)
            .unwrap()
            .add_edge(
                EdgeId::new(510),
                NodeId::new(500),
                OutputPortIndex(0),
                NodeId::new(501),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(511),
                NodeId::new(501),
                OutputPortIndex(0),
                NodeId::new(503),
                InputPortIndex(0),
            )
            .unwrap()
    }

    /// A v4 archive around `network`: stamped `format_version: 4`, document
    /// serialized from the v4-shaped graph.
    fn v4_archive(network: Graph) -> container::RawArchive {
        let comp = Composition::new(
            CompId::new(600),
            "Comp",
            (64, 64),
            FrameRate::new(30, 1),
            100,
        )
        .add_layer(Layer::new(LayerId::new(601), "Shape", network).with_time(0, 0, 100));
        let mut project = ProjectFile::from_document(
            "Legacy",
            "2026-07-30T00:00:00Z",
            Document::default().with_composition(comp),
        );
        project.manifest.format_version = 4;
        project.to_archive().unwrap()
    }

    fn loaded_shape(project: &ProjectFile) -> std::sync::Arc<Node> {
        project
            .document
            .get_composition(CompId::new(600))
            .unwrap()
            .layers[0]
            .network
            .node(NodeId::new(500))
            .expect("shape node")
            .clone()
    }

    fn constant_components(node: &Node, key: &str) -> Vec<f32> {
        match &node
            .parameters
            .iter()
            .find(|p| p.key == key)
            .unwrap_or_else(|| panic!("{key} missing"))
            .value
        {
            ParameterValue::Channel2(chs) => chs
                .iter()
                .map(|ch| match ch.source {
                    ravel_core::animation::channel::ChannelSource::Constant(v) => v,
                    ref other => panic!("{other:?}"),
                })
                .collect(),
            other => panic!("{key} is {other:?}"),
        }
    }

    /// A v4 project opens with its component parameters folded, keeping the
    /// exact values it stored — the shape is generated from the same numbers,
    /// so it renders identically.
    #[test]
    fn a_v4_project_opens_with_its_component_params_folded() {
        let archive = v4_archive(v4_shape_network((12.0, -7.0)));
        let loaded = ProjectFile::from_archive(&archive).unwrap();
        assert_eq!(loaded.manifest.format_version, CURRENT_FORMAT_VERSION);
        let shape = loaded_shape(&loaded);
        assert_eq!(constant_components(&shape, "center"), vec![12.0, -7.0]);
        assert!(
            shape.parameters.iter().all(|p| p.key != "center_x"),
            "the v4 keys are gone"
        );
        // Everything else about the network is untouched.
        let network = &loaded
            .document
            .get_composition(CompId::new(600))
            .unwrap()
            .layers[0]
            .network;
        assert_eq!(network.node_count(), 4, "no node was added or removed");
        assert_eq!(network.edge_count(), 2);
    }

    /// A v4 file that stored only one component fills the other from the
    /// template default.
    #[test]
    fn a_v4_project_with_one_component_fills_the_other_with_the_default() {
        let mut network = v4_shape_network((0.0, 0.0));
        let mut shape = (**network.node(NodeId::new(500)).unwrap()).clone();
        shape.parameters.retain(|p| p.key != "center_y");
        shape
            .parameters
            .iter_mut()
            .find(|p| p.key == "center_x")
            .unwrap()
            .value = ParameterValue::Float(9.0);
        network = network.replace_node(std::sync::Arc::new(shape));

        let loaded = ProjectFile::from_archive(&v4_archive(network)).unwrap();
        assert_eq!(
            constant_components(&loaded_shape(&loaded), "center"),
            vec![9.0, 0.0],
            "the missing component takes the shape.rect default"
        );
    }

    /// The folded value survives the next save/load cycle unchanged, and the
    /// rewritten archive is already v5 (the fold does not run twice).
    #[test]
    fn the_folded_value_roundtrips_through_save_and_load() {
        let loaded =
            ProjectFile::from_archive(&v4_archive(v4_shape_network((3.5, -1.25)))).unwrap();
        let rewritten = loaded.to_archive().unwrap();
        let reloaded = ProjectFile::from_archive(&rewritten).unwrap();
        assert_eq!(reloaded.manifest.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(
            constant_components(&loaded_shape(&reloaded), "center"),
            vec![3.5, -1.25]
        );
        assert_eq!(reloaded.document, loaded.document, "the fold is idempotent");
    }

    /// Both component ports driven by different nodes: the fold inserts a
    /// `vector.construct.vec2` so neither edge is lost, and the document is
    /// still structurally valid.
    #[test]
    fn a_v4_project_with_two_driven_component_ports_gains_a_vector_construct() {
        let network = v4_shape_network((0.0, 0.0));
        let network = network
            .add_node(
                Node::new(NodeId::new(520), "constant")
                    .with_output("value", DataTypeId::SCALAR)
                    .with_param("value", ParameterValue::Float(40.0)),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(521), "constant")
                    .with_output("value", DataTypeId::SCALAR)
                    .with_param("value", ParameterValue::Float(-8.0)),
            )
            .unwrap()
            .expose_param_port(NodeId::new(500), "center_x")
            .unwrap()
            .expose_param_port(NodeId::new(500), "center_y")
            .unwrap();
        let shape = network.node(NodeId::new(500)).unwrap();
        let (x, y) = (
            shape.param_port_index("center_x").unwrap(),
            shape.param_port_index("center_y").unwrap(),
        );
        let network = network
            .add_edge(
                EdgeId::new(530),
                NodeId::new(520),
                OutputPortIndex(0),
                NodeId::new(500),
                x,
            )
            .unwrap()
            .add_edge(
                EdgeId::new(531),
                NodeId::new(521),
                OutputPortIndex(0),
                NodeId::new(500),
                y,
            )
            .unwrap();

        let loaded = ProjectFile::from_archive(&v4_archive(network)).unwrap();
        assert_eq!(loaded.document.validate(), Ok(()));
        let network = &loaded
            .document
            .get_composition(CompId::new(600))
            .unwrap()
            .layers[0]
            .network;
        let construct = network
            .nodes()
            .find(|node| node.type_key == ravel_core::registry::builtin::VECTOR_CONSTRUCT_VEC2)
            .expect("construct inserted");
        assert!(
            construct.id != NodeId::new(500) && network.node_count() == 7,
            "the construct is a fresh node beside the original six"
        );
        // Both drivers now feed the construct's components, and the construct
        // feeds the single folded port.
        let driven = |key: &str| {
            let port = construct.param_port_index(key).unwrap();
            network
                .edges()
                .find(|edge| edge.target == construct.id && edge.target_port == port)
                .map(|edge| edge.source)
        };
        assert_eq!(driven("x"), Some(NodeId::new(520)));
        assert_eq!(driven("y"), Some(NodeId::new(521)));
        let shape = loaded_shape(&loaded);
        let center_port = shape.param_port_index("center").expect("folded port");
        assert_eq!(
            shape.inputs[center_port.0 as usize].accepted_types,
            vec![DataTypeId::VEC2]
        );
        assert!(network.edges().any(|edge| edge.source == construct.id
            && edge.target == shape.id
            && edge.target_port == center_port));
    }

    /// A v4 `attribute.set` folds its `value` family to the arity its stored
    /// `type` reads, and the result survives the next save/load cycle.
    #[test]
    fn a_v4_attribute_set_folds_by_type_and_roundtrips() {
        let network = v4_shape_network((0.0, 0.0))
            .add_node(
                Node::new(NodeId::new(540), "attribute.set")
                    .with_input("geometry", &[DataTypeId::GEOMETRY])
                    .with_output("output", DataTypeId::GEOMETRY)
                    .with_param("name", ParameterValue::String("Cd".into()))
                    .with_param("type", ParameterValue::String("vec3".into()))
                    .with_param("value", ParameterValue::Float(0.25))
                    .with_param("value_y", ParameterValue::Float(0.5))
                    .with_param("value_z", ParameterValue::Float(0.75))
                    .with_param("value_w", ParameterValue::Float(1.0)),
            )
            .unwrap();

        let loaded = ProjectFile::from_archive(&v4_archive(network)).unwrap();
        let attribute_set = |project: &ProjectFile| {
            project
                .document
                .get_composition(CompId::new(600))
                .unwrap()
                .layers[0]
                .network
                .node(NodeId::new(540))
                .expect("attribute.set")
                .clone()
        };
        let node = attribute_set(&loaded);
        let value = node.parameters.iter().find(|p| p.key == "value").unwrap();
        assert!(matches!(value.value, ParameterValue::Channel3(_)));
        assert_eq!(
            value
                .value
                .channels()
                .unwrap()
                .iter()
                .map(|ch| match ch.source {
                    ravel_core::animation::channel::ChannelSource::Constant(v) => v,
                    ref other => panic!("{other:?}"),
                })
                .collect::<Vec<_>>(),
            vec![0.25, 0.5, 0.75],
            "vec3 reads three components; the fourth is dropped"
        );
        assert!(
            node.parameters.iter().all(|p| p.key != "value_y"),
            "the surplus keys are gone"
        );

        let reloaded = ProjectFile::from_archive(&loaded.to_archive().unwrap()).unwrap();
        assert_eq!(attribute_set(&reloaded), node, "the fold is idempotent");
    }

    /// A v4 `attribute.set` whose four `value_*` components were separately
    /// driven keeps every edge: the fold routes them through a
    /// `vector.construct.vec4`, which the folded port accepts alongside
    /// `COLOR`. Before that, four drivable scalar ports became one COLOR port
    /// and the edges were lost.
    #[test]
    fn a_v4_attribute_set_with_driven_components_keeps_its_edges() {
        for (type_name, expected) in [("vec4", 4usize), ("color", 4)] {
            let mut network = v4_shape_network((0.0, 0.0))
                .add_node(
                    Node::new(NodeId::new(540), "attribute.set")
                        .with_input("geometry", &[DataTypeId::GEOMETRY])
                        .with_output("output", DataTypeId::GEOMETRY)
                        .with_param("name", ParameterValue::String("Cd".into()))
                        .with_param("type", ParameterValue::String(type_name.into()))
                        .with_param("value", ParameterValue::Float(0.0))
                        .with_param("value_y", ParameterValue::Float(0.0))
                        .with_param("value_z", ParameterValue::Float(0.0))
                        .with_param("value_w", ParameterValue::Float(0.0)),
                )
                .unwrap();
            // One driver per component, each on its own scalar parameter port.
            let keys = ["value", "value_y", "value_z", "value_w"];
            for (index, key) in keys.iter().enumerate() {
                let driver = NodeId::new(550 + index as u64);
                network = network
                    .add_node(
                        Node::new(driver, "constant")
                            .with_output("value", DataTypeId::SCALAR)
                            .with_param("value", ParameterValue::Float(index as f32)),
                    )
                    .unwrap()
                    .expose_param_port(NodeId::new(540), key)
                    .unwrap();
                let port = network
                    .node(NodeId::new(540))
                    .unwrap()
                    .param_port_index(key)
                    .unwrap();
                network = network
                    .add_edge(
                        EdgeId::new(560 + index as u64),
                        driver,
                        OutputPortIndex(0),
                        NodeId::new(540),
                        port,
                    )
                    .unwrap();
            }

            let loaded = ProjectFile::from_archive(&v4_archive(network)).unwrap();
            assert_eq!(loaded.document.validate(), Ok(()));
            let network = &loaded
                .document
                .get_composition(CompId::new(600))
                .unwrap()
                .layers[0]
                .network;
            let construct = network
                .nodes()
                .find(|node| node.type_key == ravel_core::registry::builtin::VECTOR_CONSTRUCT_VEC4)
                .unwrap_or_else(|| panic!("{type_name}: vec4 construct inserted"));

            // Every original driver still reaches the same component.
            for (index, component) in ["x", "y", "z", "w"].iter().enumerate() {
                let port = construct
                    .param_port_index(component)
                    .unwrap_or_else(|| panic!("{type_name}: {component} exposed"));
                let source = network
                    .edges()
                    .find(|edge| edge.target == construct.id && edge.target_port == port)
                    .map(|edge| edge.source);
                assert_eq!(
                    source,
                    Some(NodeId::new(550 + index as u64)),
                    "{type_name}: {component} keeps its driver"
                );
            }
            assert_eq!(
                construct.inputs.iter().filter(|p| p.is_param).count(),
                expected,
                "{type_name}"
            );

            // …and the construct drives the single folded parameter port.
            let target = network.node(NodeId::new(540)).unwrap();
            let value_port = target
                .param_port_index("value")
                .unwrap_or_else(|| panic!("{type_name}: folded port"));
            assert_eq!(
                target.inputs[value_port.0 as usize].accepted_types,
                vec![DataTypeId::COLOR, DataTypeId::VEC4],
                "{type_name}"
            );
            assert!(
                network.edges().any(|edge| edge.source == construct.id
                    && edge.target == NodeId::new(540)
                    && edge.target_port == value_port),
                "{type_name}: the construct output reaches the folded port"
            );
        }
    }

    /// A v5 archive is left alone: the fold is gated on the source version, so
    /// a legitimately stored `center_x` on a third-party node is not rewritten.
    #[test]
    fn a_v5_project_is_not_folded() {
        let mut archive = v4_archive(v4_shape_network((1.0, 2.0)));
        let mut manifest: serde_json::Value =
            serde_json::from_str(archive.require_text(container::entry::MANIFEST).unwrap())
                .unwrap();
        manifest["format_version"] = serde_json::Value::from(5);
        archive.insert(
            container::entry::MANIFEST,
            serde_json::to_string_pretty(&manifest)
                .unwrap()
                .into_bytes(),
        );
        let loaded = ProjectFile::from_archive(&archive).unwrap();
        let shape = loaded_shape(&loaded);
        assert!(
            shape.parameters.iter().any(|p| p.key == "center_x"),
            "a v5 document keeps whatever it stored"
        );
    }

    // ---- v5 → v6: curve parameters ---------------------------------------

    /// A v5 archive whose layer network holds a `field.curve_remap` with its
    /// control points stored as text, stamped `format_version: 5`.
    fn v5_curve_archive(points: &str) -> container::RawArchive {
        let network = Graph::new()
            .add_node(
                Node::new(NodeId::new(700), "field.curve_remap")
                    .with_input("field", &[DataTypeId::FIELD])
                    .with_output("field", DataTypeId::FIELD)
                    .with_param("points", ParameterValue::String(points.into())),
            )
            .unwrap();
        let comp = Composition::new(
            CompId::new(600),
            "Comp",
            (64, 64),
            FrameRate::new(30, 1),
            100,
        )
        .add_layer(Layer::new(LayerId::new(601), "Field", network).with_time(0, 0, 100));
        let mut project = ProjectFile::from_document(
            "Legacy",
            "2026-07-30T00:00:00Z",
            Document::default().with_composition(comp),
        );
        project.manifest.format_version = 5;
        project.to_archive().unwrap()
    }

    fn loaded_curve(project: &ProjectFile) -> ravel_core::param_curve::CurveParam {
        project
            .document
            .get_composition(CompId::new(600))
            .unwrap()
            .layers[0]
            .network
            .node(NodeId::new(700))
            .expect("curve node")
            .parameters
            .iter()
            .find(|p| p.key == "points")
            .expect("points")
            .value
            .as_curve()
            .expect("points is a Curve")
            .clone()
    }

    /// A v5 project opens with its control points read as a curve that maps
    /// the same inputs to the same outputs it did before.
    #[test]
    fn a_v5_project_opens_with_its_curve_points_upgraded() {
        let loaded = ProjectFile::from_archive(&v5_curve_archive("0:0,0.5:0.8,1:1")).unwrap();
        assert_eq!(loaded.manifest.format_version, CURRENT_FORMAT_VERSION);
        let curve = loaded_curve(&loaded);
        assert_eq!(curve.len(), 3);
        assert_eq!(curve.evaluate(0.0), 0.0);
        assert!((curve.evaluate(0.5) - 0.8).abs() < 1e-6);
        assert!((curve.evaluate(0.75) - 0.9).abs() < 1e-6);
        assert_eq!(curve.evaluate(1.0), 1.0);
        // Out of range it clamps, as the string reader did.
        assert_eq!(curve.evaluate(-5.0), 0.0);
        assert_eq!(curve.evaluate(5.0), 1.0);
    }

    /// Control points that cannot be read do not stop the project from
    /// opening; the parameter becomes the identity curve.
    #[test]
    fn a_v5_project_with_unreadable_curve_points_opens_with_the_identity() {
        let loaded = ProjectFile::from_archive(&v5_curve_archive("not a curve")).unwrap();
        assert_eq!(
            loaded_curve(&loaded),
            ravel_core::param_curve::CurveParam::identity()
        );
    }

    /// The upgraded curve survives the next save/load cycle unchanged, and
    /// the rewritten archive is already v6 (the upgrade does not run twice).
    #[test]
    fn the_upgraded_curve_roundtrips_through_save_and_load() {
        let loaded = ProjectFile::from_archive(&v5_curve_archive("0:0,0.25:0.6,1:2")).unwrap();
        let reloaded = ProjectFile::from_archive(&loaded.to_archive().unwrap()).unwrap();
        assert_eq!(reloaded.manifest.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(loaded_curve(&reloaded), loaded_curve(&loaded));
        assert_eq!(
            reloaded.document, loaded.document,
            "the upgrade is idempotent"
        );
    }

    // -----------------------------------------------------------------
    // v7 → v8: authored colours are reinterpreted for the linear pipeline
    // -----------------------------------------------------------------

    /// A v7 archive whose layer network holds one `constant.color` at
    /// `value` on every colour channel and `alpha` on the fourth.
    fn v7_colour_archive(value: f32, alpha: f32) -> container::RawArchive {
        use ravel_core::animation::channel::AnimationChannel;

        let network = Graph::new()
            .add_node(
                Node::new(NodeId::new(800), "constant.color")
                    .with_output("color", DataTypeId::COLOR)
                    .with_param(
                        "color",
                        ParameterValue::Channel4([
                            AnimationChannel::constant(value),
                            AnimationChannel::constant(value),
                            AnimationChannel::constant(value),
                            AnimationChannel::constant(alpha),
                        ]),
                    ),
            )
            .unwrap();
        let comp = Composition::new(
            CompId::new(700),
            "Comp",
            (64, 64),
            FrameRate::new(30, 1),
            100,
        )
        .add_layer(Layer::new(LayerId::new(701), "Colour", network).with_time(0, 0, 100));
        let mut project = ProjectFile::from_document(
            "Legacy",
            "2026-08-06T00:00:00Z",
            Document::default().with_composition(comp),
        );
        project.manifest.format_version = 7;
        project.to_archive().unwrap()
    }

    fn loaded_colour(project: &ProjectFile) -> Vec<f32> {
        use ravel_core::animation::channel::ChannelSource;
        project
            .document
            .get_composition(CompId::new(700))
            .unwrap()
            .layers[0]
            .network
            .node(NodeId::new(800))
            .expect("colour node")
            .parameters
            .iter()
            .find(|p| p.key == "color")
            .expect("color")
            .value
            .channels()
            .expect("channels")
            .iter()
            .map(|channel| match channel.source {
                ChannelSource::Constant(v) => v,
                ref other => panic!("expected a constant, found {other:?}"),
            })
            .collect()
    }

    /// CM-2: a v7 colour is reinterpreted once, `linear → srgb` returns the
    /// author's number, and alpha is untouched.
    #[test]
    fn a_v7_project_opens_with_its_colours_linearised() {
        let loaded = ProjectFile::from_archive(&v7_colour_archive(0.5, 0.25)).unwrap();
        assert_eq!(loaded.manifest.format_version, CURRENT_FORMAT_VERSION);

        let colour = loaded_colour(&loaded);
        for channel in &colour[..3] {
            assert!((channel - 0.214_041_1).abs() < 1e-5, "{colour:?}");
        }
        assert_eq!(colour[3], 0.25, "alpha carries no transfer function");

        let back = ravel_core::color::convert(
            [colour[0], colour[1], colour[2]],
            ravel_core::color::ColorSpace::WORKING,
            ravel_core::color::ColorSpace::SRGB,
        );
        for channel in back {
            assert!((channel - 0.5).abs() < 1e-5, "{back:?}");
        }
    }

    /// CM-2: authored colours that live outside every node network — a
    /// composition background and an `exposed_parameters` colour default —
    /// are converted too. A walk over node parameters cannot see either.
    #[test]
    fn colours_outside_the_graphs_are_linearised_too() {
        use ravel_core::exposed::{
            ExposedBinding, ExposedParameter, ExposedParameters, ExposedType, ExposedValue,
        };
        use ravel_core::types::Color;

        let mut comp = Composition::new(
            CompId::new(700),
            "Comp",
            (64, 64),
            FrameRate::new(30, 1),
            100,
        );
        comp.background_color = Color::new(0.5, 0.5, 0.5, 1.0);

        let declaration = ExposedParameter::new(
            "tint",
            ExposedType::Color,
            ExposedValue::Color(Color::new(0.5, 0.25, 1.0, 0.5)),
            ExposedBinding::new(NodeId::new(800), "color"),
        )
        .unwrap();
        let document = Document::default()
            .with_composition(comp)
            .with_exposed_parameters(ExposedParameters::from_declarations([declaration]).unwrap());

        let mut project = ProjectFile::from_document("Legacy", "2026-08-06T00:00:00Z", document);
        project.manifest.format_version = 7;
        let loaded = ProjectFile::from_archive(&project.to_archive().unwrap()).unwrap();

        let background = loaded
            .document
            .get_composition(CompId::new(700))
            .unwrap()
            .background_color;
        assert!((background.r - 0.214_041_1).abs() < 1e-5, "{background:?}");
        assert_eq!(background.a, 1.0);

        let ExposedValue::Color(tint) = loaded
            .document
            .exposed_parameters
            .iter()
            .next()
            .expect("the declaration survives")
            .default_value()
        else {
            panic!("expected a colour default");
        };
        assert!((tint.r - 0.214_041_1).abs() < 1e-5, "{tint:?}");
        assert_eq!(tint.a, 0.5, "alpha carries no transfer function");
    }

    /// CM-2: the conversion runs exactly once. It is not idempotent on its
    /// own — the format version is what makes reopening safe.
    #[test]
    fn the_linearised_colour_is_not_converted_a_second_time() {
        let loaded = ProjectFile::from_archive(&v7_colour_archive(0.5, 1.0)).unwrap();
        let archive = loaded.to_archive().unwrap();
        let manifest: serde_json::Value =
            serde_json::from_str(archive.require_text(container::entry::MANIFEST).unwrap())
                .unwrap();
        assert_eq!(
            manifest["format_version"], CURRENT_FORMAT_VERSION,
            "the rewritten archive is v8"
        );
        let reloaded = ProjectFile::from_archive(&archive).unwrap();
        assert_eq!(loaded_colour(&reloaded), loaded_colour(&loaded));
        assert_eq!(reloaded.document, loaded.document);
    }

    /// CM-2: a v8 archive is refused by a build that only knows v7 — the
    /// mechanism that keeps a display-referred build from rendering a linear
    /// project too dark. Tested through the version guard itself, since this
    /// build cannot be made to be an older one.
    #[test]
    fn a_newer_colour_format_is_refused_rather_than_misread() {
        let mut manifest = serde_json::json!({ "format_version": CURRENT_FORMAT_VERSION + 1 });
        assert!(matches!(
            migration::migrate_to_current(&mut manifest),
            Err(migration::MigrationError::TooNew { .. })
        ));
    }

    /// `Layer.audio` was added additively inside format v4. A v4 document
    /// written before that field existed must load, and its first rewrite
    /// must itself remain stable on another load/save cycle.
    #[test]
    fn v4_without_layer_audio_loads_and_rewrites_stably() {
        let mut project = demo_project();
        let comp_id = project.document.root_comp.unwrap();
        let comp = project
            .document
            .get_composition(comp_id)
            .unwrap()
            .as_ref()
            .clone();
        let layers = comp
            .layers
            .iter()
            .cloned()
            .map(|mut layer| {
                layer.audio = None;
                layer
            })
            .collect();
        project
            .document
            .compositions
            .insert(comp_id, std::sync::Arc::new(Composition { layers, ..comp }));

        let mut archive = project.to_archive().unwrap();
        let current = archive
            .require_text(container::entry::DOCUMENT)
            .unwrap()
            .to_string();
        assert!(current.contains("audio: None,"));
        let legacy_v4 = current.replace("audio: None,", "");
        archive.insert(container::entry::DOCUMENT, legacy_v4.into_bytes());

        let loaded = ProjectFile::from_archive(&archive).unwrap();
        assert_eq!(loaded.manifest.format_version, CURRENT_FORMAT_VERSION);
        assert!(
            loaded
                .document
                .get_composition(comp_id)
                .unwrap()
                .layers
                .iter()
                .all(|layer| layer.audio.is_none())
        );

        let rewritten = loaded.to_archive().unwrap();
        let reloaded = ProjectFile::from_archive(&rewritten).unwrap();
        assert_eq!(reloaded.document, loaded.document);
        assert_eq!(rewritten, reloaded.to_archive().unwrap());
    }

    // -----------------------------------------------------------------
    // Asset references (REQ-PROJ-001)
    // -----------------------------------------------------------------

    /// The project directory is the anchor: media stored inside it is
    /// written relative, so moving the whole directory keeps it resolvable.
    #[test]
    fn media_inside_the_project_is_stored_relative_and_survives_a_move() {
        let original = tempfile::tempdir().unwrap();
        let footage = original.path().join("footage");
        std::fs::create_dir_all(&footage).unwrap();
        let clip = footage.join("plate.mov");
        std::fs::write(&clip, b"not really a movie").unwrap();

        let document = Document::default()
            .with_composition(Composition::new(
                CompId::next(),
                "Comp 1",
                (1280, 720),
                FrameRate::new(24, 1),
                120,
            ))
            .with_media_asset("plate", &clip);
        let project = ProjectFile::from_document("Relocatable", "2026-07-26T00:00:00Z", document);

        let project_path = original.path().join("demo.ravprj");
        project.save(&project_path).unwrap();

        // Stored form is relative to the project root.
        let stored =
            ProjectFile::from_archive(&container::read_file(&project_path).unwrap()).unwrap();
        let entry = stored.document.get_media_asset("plate").unwrap();
        assert_eq!(
            entry.path,
            AssetPath::Relative("./footage/plate.mov".into())
        );
        assert!(
            entry.is_offline(),
            "from_archive does not know where the archive lives"
        );

        // Loading through the path resolves it.
        let loaded = ProjectFile::load(&project_path).unwrap();
        assert_eq!(
            loaded.document.get_media_asset("plate").unwrap().resolved,
            Some(clip.clone())
        );

        // Move the entire project directory; the same file resolves again.
        let moved = tempfile::tempdir().unwrap();
        let moved_path = moved.path().join("demo.ravprj");
        std::fs::create_dir_all(moved.path().join("footage")).unwrap();
        std::fs::copy(&clip, moved.path().join("footage/plate.mov")).unwrap();
        std::fs::copy(&project_path, &moved_path).unwrap();
        let reopened = ProjectFile::load(&moved_path).unwrap();
        let moved_clip = moved.path().join("footage/plate.mov");
        assert_eq!(
            reopened.document.get_media_asset("plate").unwrap().resolved,
            Some(moved_clip.clone())
        );
        assert!(moved_clip.exists(), "the resolved path names a real file");
    }

    /// Media outside the project root has no relative form; it stays
    /// absolute rather than growing a `../../..` chain.
    #[test]
    fn media_outside_the_project_stays_absolute() {
        let root = tempfile::tempdir().unwrap();
        let document = Document::default()
            .with_composition(Composition::new(
                CompId::next(),
                "Comp 1",
                (1280, 720),
                FrameRate::new(24, 1),
                120,
            ))
            .with_media_asset("plate", "/elsewhere/plate.mov");
        let project = ProjectFile::from_document("Outside", "2026-07-26T00:00:00Z", document);

        let path = root.path().join("demo.ravprj");
        project.save(&path).unwrap();
        let loaded = ProjectFile::load(&path).unwrap();
        let entry = loaded.document.get_media_asset("plate").unwrap();
        assert_eq!(
            entry.path,
            AssetPath::Absolute(PathBuf::from("/elsewhere/plate.mov"))
        );
        assert_eq!(entry.resolved, Some(PathBuf::from("/elsewhere/plate.mov")));
    }

    /// A `${PROJECT_ROOT}` reference is the user's explicit choice: save
    /// never rewrites it, and load expands it against the current root.
    #[test]
    fn variable_references_survive_a_save_and_resolve_on_load() {
        let root = tempfile::tempdir().unwrap();
        let document = Document::default()
            .with_composition(Composition::new(
                CompId::next(),
                "Comp 1",
                (1280, 720),
                FrameRate::new(24, 1),
                120,
            ))
            .with_media_asset_entry(
                "plate",
                MediaAssetEntry {
                    color_space: None,
                    path: AssetPath::Variable("${PROJECT_ROOT}/footage/plate.mov".into()),
                    kind: AssetKind::Container,
                    metadata: AssetMetadata::default(),
                    // A stale absolute location must not overwrite the
                    // variable form on save.
                    resolved: Some(PathBuf::from("/stale/plate.mov")),
                },
            );
        let project = ProjectFile::from_document("Variable", "2026-07-26T00:00:00Z", document);

        let path = root.path().join("demo.ravprj");
        project.save(&path).unwrap();
        let loaded = ProjectFile::load(&path).unwrap();
        let entry = loaded.document.get_media_asset("plate").unwrap();
        assert_eq!(
            entry.path,
            AssetPath::Variable("${PROJECT_ROOT}/footage/plate.mov".into())
        );
        assert_eq!(entry.resolved, Some(root.path().join("footage/plate.mov")));
    }

    /// Save → load → save must reproduce the same bytes, so a project that
    /// is only opened and re-saved shows no diff.
    #[test]
    fn asset_paths_round_trip_byte_identically() {
        let root = tempfile::tempdir().unwrap();
        let inside = root.path().join("footage/plate.mov");
        let document = Document::default()
            .with_composition(Composition::new(
                CompId::next(),
                "Comp 1",
                (1280, 720),
                FrameRate::new(24, 1),
                120,
            ))
            .with_media_asset("inside", &inside)
            .with_media_asset("outside", "/elsewhere/b.mov");
        let mut project = ProjectFile::from_document("Stable", "2026-07-26T00:00:00Z", document);
        project.manifest.modified_at = "2026-07-26T00:00:00Z".into();

        let path = root.path().join("demo.ravprj");
        let root = project_root_of(&path);
        let first = project.to_archive_for_root(root.as_deref()).unwrap();
        project.save(&path).unwrap();

        let mut reloaded = ProjectFile::load(&path).unwrap();
        reloaded.manifest.modified_at = "2026-07-26T00:00:00Z".into();
        let second = reloaded.to_archive_for_root(root.as_deref()).unwrap();
        assert_eq!(first, second);
    }

    /// A format-v3 document stores bare absolute `PathBuf`s. It must open as
    /// v4 with those paths intact and a kind inferred from the extension.
    #[test]
    fn v3_media_assets_upgrade_to_absolute_references() {
        let document_ron = r#"(
  graph: (nodes: [], edges: [], subnets: []),
  compositions: [],
  root_comp: None,
  media_assets: [
    ("plate", MediaAssetEntry(path: "/legacy/footage/plate.mov")),
    ("still", MediaAssetEntry(path: "/legacy/art/logo.png")),
  ],
)"#;
        let mut archive = container::RawArchive::new();
        archive.insert(
            container::entry::MANIFEST,
            br#"{
  "format_version": 3,
  "ravel_version": "0.1.0",
  "project_name": "Legacy",
  "created_at": "2026-01-01T00:00:00Z",
  "modified_at": "2026-01-02T00:00:00Z",
  "frame_rate": { "num": 24, "den": 1 },
  "resolution": { "width": 1920, "height": 1080 }
}"#
            .to_vec(),
        );
        archive.insert(container::entry::DOCUMENT, document_ron.as_bytes().to_vec());
        // A leftover (always empty) refs.json must not block the load.
        archive.insert(container::entry::ASSETS, br#"{"assets":[]}"#.to_vec());

        let project = ProjectFile::from_archive(&archive).unwrap();
        assert_eq!(project.manifest.format_version, CURRENT_FORMAT_VERSION);

        let plate = project.document.get_media_asset("plate").unwrap();
        assert_eq!(
            plate.path,
            AssetPath::Absolute(PathBuf::from("/legacy/footage/plate.mov"))
        );
        assert_eq!(plate.kind, AssetKind::Container);
        assert_eq!(plate.metadata, AssetMetadata::default());

        let still = project.document.get_media_asset("still").unwrap();
        assert_eq!(still.kind, AssetKind::Still);

        // Absolute references resolve to themselves regardless of the root.
        let resolved = project
            .document
            .with_resolved_assets(Some(Path::new("/somewhere/else")), &HashMap::new());
        assert_eq!(
            resolved.get_media_asset("plate").unwrap().resolved,
            Some(PathBuf::from("/legacy/footage/plate.mov"))
        );
    }

    /// An unresolvable asset is a normal document, not a load failure: the
    /// media node degrades on its own.
    #[test]
    fn an_offline_asset_still_loads_and_validates() {
        let root = tempfile::tempdir().unwrap();
        let document = Document::default()
            .with_composition(Composition::new(
                CompId::next(),
                "Comp 1",
                (1280, 720),
                FrameRate::new(24, 1),
                120,
            ))
            .with_media_asset_entry(
                "gone",
                MediaAssetEntry {
                    color_space: None,
                    path: AssetPath::Variable("${MISSING_VAR}/a.mov".into()),
                    kind: AssetKind::Container,
                    metadata: AssetMetadata::default(),
                    resolved: None,
                },
            );
        let project = ProjectFile::from_document("Offline", "2026-07-26T00:00:00Z", document);
        let path = root.path().join("demo.ravprj");
        project.save(&path).unwrap();

        let loaded = ProjectFile::load(&path).unwrap();
        assert!(
            loaded
                .document
                .get_media_asset("gone")
                .unwrap()
                .is_offline()
        );
    }

    #[test]
    fn current_archives_do_not_contain_the_legacy_graph_entry() {
        let project = demo_project();
        let archive = project.to_archive().unwrap();
        assert!(archive.get(container::entry::DOCUMENT).is_some());
        assert!(archive.get(container::entry::GRAPH).is_none());
    }

    #[test]
    fn from_document_stamps_manifest_from_root_comp() {
        let project = ProjectFile::from_document("Stamped", "t", demo_document());
        assert_eq!(project.manifest.frame_rate, RationalRate::new(24, 1));
        assert_eq!(project.manifest.resolution, Resolution::new(1280, 720));
        assert_eq!(project.manifest.format_version, CURRENT_FORMAT_VERSION);
    }

    // -----------------------------------------------------------------------
    // Subnet nesting: what saves must open again
    // -----------------------------------------------------------------------

    /// A subnet chain `depth` boundaries deep, innermost node last.
    fn subnet_chain(depth: usize) -> Graph {
        let mut inner = Graph::new()
            .add_node(Node::new(NodeId::next(), "constant").with_output("out", DataTypeId::SCALAR))
            .unwrap();
        for _ in 0..depth {
            inner = Graph::new()
                .add_node(
                    Node::new(NodeId::next(), "subnet")
                        .with_subnet(inner)
                        .with_output("out", DataTypeId::SCALAR),
                )
                .unwrap();
        }
        inner
    }

    /// The nesting sits in a layer network — the deepest path a document has
    /// into the file, and therefore the one that decides the limit.
    fn nested_in_a_layer(depth: usize) -> Document {
        let comp = Composition::new(
            CompId::next(),
            "Deep",
            (640, 360),
            FrameRate::new(24, 1),
            100,
        )
        .add_layer(Layer::new(LayerId::next(), "Deep", subnet_chain(depth)));
        Document::default().with_composition(comp)
    }

    /// The invariant `MAX_SUBNET_DEPTH` exists to hold: a document the format
    /// accepts is one this build can write and read back.
    ///
    /// The guard is not the depth check alone — it is the depth check *and*
    /// [`ron_options`]. RON's default budget of 128 recursion levels is one
    /// level short of a layer network nested this deep, which is how a saved
    /// project came to be unopenable (`HIGH-26`).
    /// The nesting a document came in with, so a round trip can be shown to
    /// have kept it rather than merely to have produced something valid: a
    /// load that dropped the inner graphs would satisfy the depth check.
    fn deepest_nesting(document: &Document) -> usize {
        use ravel_core::composition::subnet_depth_exceeds;

        let graphs = std::iter::once(&document.graph).chain(
            document
                .compositions
                .values()
                .flat_map(|comp| comp.layers.iter().map(|layer| &layer.network)),
        );
        graphs
            .map(|graph| {
                (0..)
                    .find(|limit| !subnet_depth_exceeds(graph, *limit))
                    .unwrap()
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn a_document_nested_to_the_limit_survives_a_save_and_a_load() {
        use ravel_core::composition::MAX_SUBNET_DEPTH;

        let document = nested_in_a_layer(MAX_SUBNET_DEPTH);
        assert_eq!(document.validate_subnet_depth(), Ok(()));
        assert_eq!(deepest_nesting(&document), MAX_SUBNET_DEPTH);

        let project = ProjectFile::from_document("Deep", "2026-08-20T00:00:00Z", document);
        let archive = project.to_archive().expect("a valid document is written");
        let back = ProjectFile::from_archive(&archive)
            .expect("what the writer accepted, the reader accepts");
        assert_eq!(
            deepest_nesting(&back.document),
            MAX_SUBNET_DEPTH,
            "the nesting came back, not just something that validates"
        );
    }

    /// The legacy flat graph carries subnets too and is read by a **different**
    /// parser entry ([`GraphDoc::from_ron`]), which only runs for a pre-v3
    /// archive — so reaching it needs a hand-built v2 container, not a document
    /// written by this build.
    #[test]
    fn a_legacy_graph_nested_to_the_limit_still_loads() {
        use ravel_core::composition::MAX_SUBNET_DEPTH;

        let archive = legacy_archive(
            r#"{
                "format_version": 2,
                "ravel_version": "0.1.0",
                "project_name": "Deep",
                "created_at": "2026-08-20T00:00:00Z",
                "modified_at": "2026-08-20T00:00:00Z",
                "frame_rate": { "num": 24, "den": 1 },
                "resolution": { "width": 640, "height": 360 }
            }"#,
            &subnet_chain(MAX_SUBNET_DEPTH),
        );

        let project = ProjectFile::from_archive(&archive)
            .expect("a legacy graph at the document's limit still parses");
        assert_eq!(
            deepest_nesting(&project.document),
            MAX_SUBNET_DEPTH,
            "the legacy graph kept its nesting"
        );
    }

    /// And the other half: nesting the format does not accept is refused
    /// *before* a file exists, not after one that cannot be reopened does.
    #[test]
    fn a_document_nested_past_the_limit_is_refused_by_the_save() {
        use ravel_core::composition::{DocumentValidationError, MAX_SUBNET_DEPTH};

        let project = ProjectFile::from_document(
            "Too deep",
            "2026-08-20T00:00:00Z",
            nested_in_a_layer(MAX_SUBNET_DEPTH + 1),
        );
        assert!(matches!(
            project.to_archive(),
            Err(ProjectError::InvalidDocument(
                DocumentValidationError::SubnetDepthExceeded {
                    limit: MAX_SUBNET_DEPTH
                }
            ))
        ));
    }

    /// The parser's budget keeps a margin over what the depth limit costs, so
    /// a format change that adds RON nesting per subnet does not land on the
    /// boundary unnoticed. Two further levels is one further subnet.
    #[test]
    fn the_parser_budget_has_room_above_the_depth_limit() {
        use ravel_core::composition::MAX_SUBNET_DEPTH;

        let text = document_to_ron(&nested_in_a_layer(MAX_SUBNET_DEPTH)).unwrap();
        let tighter = ron::Options::default()
            .with_recursion_limit(ravel_core::composition::RON_RECURSION_LIMIT - 16);
        assert!(
            tighter.from_str::<Document>(&text).is_ok(),
            "the budget is within 16 recursion levels of the depth limit's cost"
        );
    }

    #[test]
    fn save_load_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ravel_project_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("demo.ravprj");
        let _ = std::fs::remove_file(&path);

        let project = demo_project();
        project.save(&path).unwrap();
        let loaded = ProjectFile::load(&path).unwrap();

        assert_eq!(loaded.document, project.document);
        assert_eq!(loaded.manifest.format_version, CURRENT_FORMAT_VERSION);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    /// An archive persisted before the frame index port existed gains `f`
    /// on its layer In nodes at load: appended last, after existing custom
    /// ports, so index-addressed edges keep pointing at the same port.
    #[test]
    fn load_appends_the_frame_index_port_to_pre_f_in_nodes() {
        let network = Graph::new()
            .add_node(
                Node::new(NodeId::new(200), net::NET_IN_TYPE_KEY)
                    .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
                    .with_output(net::PORT_TIME, DataTypeId::SCALAR)
                    // A legacy custom parameter port, wired below through
                    // its pre-migration output index (2).
                    .with_output("intensity", DataTypeId::SCALAR)
                    .with_param("intensity", ParameterValue::Float(0.5)),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(202), "grade")
                    .with_input("in", &[DataTypeId::SCALAR])
                    .with_output("out", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(201), net::NET_OUT_TYPE_KEY)
                    .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(210),
                NodeId::new(200),
                OutputPortIndex(2),
                NodeId::new(202),
                InputPortIndex(0),
            )
            .unwrap();
        let comp_id = CompId::next();
        let doc = Document::default().with_composition(
            Composition::new(comp_id, "Legacy", (64, 64), FrameRate::new(30, 1), 30)
                .add_layer(Layer::new(LayerId::new(21), "Old", network)),
        );
        let project = ProjectFile::from_document("Legacy", "t", doc);
        let archive = project.to_archive().unwrap();
        let back = ProjectFile::from_archive(&archive).unwrap();

        let comp = back.document.get_composition(comp_id).unwrap();
        let in_node = net::find_in_node(&comp.layers[0].network).unwrap();
        assert_eq!(in_node.outputs.len(), 4);
        let appended = in_node.outputs.last().unwrap();
        assert_eq!(appended.name, net::PORT_FRAME_INDEX);
        assert_eq!(appended.data_type, DataTypeId::SCALAR);
        // The custom port keeps its index, so the edge still reads it.
        assert_eq!(in_node.outputs[2].name, "intensity");
        let edge = comp.layers[0]
            .network
            .edges()
            .find(|e| e.id == EdgeId::new(210))
            .expect("edge survives");
        assert_eq!(edge.source_port, OutputPortIndex(2));
    }

    #[test]
    fn load_advances_the_id_counters_past_document_watermarks() {
        // Watermarks spread across all four id kinds (REQ-LAYER-009).
        let flat = Graph::new()
            .add_node(
                Node::new(NodeId::new(50_000), "constant").with_output("v", DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(50_001), "sink").with_input("in", &[DataTypeId::SCALAR]),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(50_002),
                NodeId::new(50_000),
                OutputPortIndex(0),
                NodeId::new(50_001),
                InputPortIndex(0),
            )
            .unwrap();
        let layer = Layer::new(LayerId::new(50_003), "big", Graph::new());
        let comp = Composition::new(
            CompId::new(50_004),
            "big comp",
            (640, 480),
            FrameRate::new(30, 1),
            100,
        )
        .add_layer(layer);
        let project =
            ProjectFile::from_document("Ids", "t", Document::new(flat).with_composition(comp));

        let dir = std::env::temp_dir().join(format!("ravel_project_ids_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ids.ravprj");
        let _ = std::fs::remove_file(&path);
        project.save(&path).unwrap();
        let _loaded = ProjectFile::load(&path).unwrap();

        assert!(NodeId::next().raw() > 50_001);
        assert!(EdgeId::next().raw() > 50_002);
        assert!(CompId::next().raw() > 50_004);
        assert!(LayerId::next().raw() > 50_003);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn loads_and_migrates_v1_archive() {
        // Hand-craft a v1 archive (old manifest schema, legacy graph only).
        let archive = legacy_archive(
            r#"{
                "format_version": 1,
                "ravel_version": "0.0.1",
                "project_name": "Legacy",
                "created_at": "2026-01-01T00:00:00Z",
                "modified_at": "2026-01-02T00:00:00Z",
                "frame_rate": { "num": 24, "den": 1 },
                "color_space": "aces_1.2"
            }"#,
            &Graph::new(),
        );

        let project = ProjectFile::from_archive(&archive).unwrap();
        assert_eq!(project.manifest.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(project.manifest.color_config.as_deref(), Some("aces_1.2"));
        assert_eq!(project.manifest.resolution.width, 1920);
        // Missing settings default cleanly.
        assert_eq!(project.settings, SettingsLayer::default());

        // v1 → v3: a fresh root composition is seeded from the manifest.
        let root_id = project.document.root_comp.expect("root comp");
        let root = project.document.get_composition(root_id).unwrap().clone();
        assert_eq!(root.name, "Comp 1");
        assert_eq!(root.resolution, (1920, 1080));
        assert_eq!(root.frame_rate, FrameRate::new(24, 1));
        assert_eq!(root.duration_frames, 300);
        assert_eq!(root.layer_count(), 0);
    }

    #[test]
    fn v2_archive_loads_through_the_legacy_graph_path() {
        let archive = legacy_archive(
            r#"{
                "format_version": 2,
                "ravel_version": "0.1.0",
                "project_name": "Flat",
                "created_at": "2026-03-01T00:00:00Z",
                "modified_at": "2026-03-02T00:00:00Z",
                "frame_rate": { "num": 25, "den": 1 },
                "resolution": { "width": 1280, "height": 720 }
            }"#,
            &legacy_graph(),
        );

        let project = ProjectFile::from_archive(&archive).unwrap();
        assert_eq!(project.manifest.format_version, CURRENT_FORMAT_VERSION);

        // The legacy flat graph is preserved on Document::graph …
        assert_eq!(project.document.graph.node_count(), 2);
        assert_eq!(project.document.graph.edge_count(), 1);
        // … and the root composition is seeded from the manifest.
        let root_id = project.document.root_comp.expect("root comp");
        let root = project.document.get_composition(root_id).unwrap();
        assert_eq!(root.resolution, (1280, 720));
        assert_eq!(root.frame_rate, FrameRate::new(25, 1));
    }

    #[test]
    fn corrupt_archive_errors_gracefully() {
        // Valid container but neither a document nor a legacy graph entry.
        let mut archive = container::RawArchive::new();
        archive.insert(
            container::entry::MANIFEST,
            br#"{"format_version":3,"ravel_version":"0.1.0","project_name":"P","created_at":"t","modified_at":"t","frame_rate":{"num":30,"den":1},"resolution":{"width":1,"height":1}}"#
                .to_vec(),
        );
        let err = ProjectFile::from_archive(&archive).unwrap_err();
        assert!(matches!(
            err,
            ProjectError::Container(container::ContainerError::MissingEntry(
                container::entry::DOCUMENT
            ))
        ));
    }

    #[test]
    fn read_created_at_reads_existing_manifest() {
        let dir =
            std::env::temp_dir().join(format!("ravel_project_created_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("created.ravprj");
        let _ = std::fs::remove_file(&path);

        assert_eq!(read_created_at(&path), None);
        ProjectFile::from_document("P", "2026-01-02T03:04:05Z", Document::default())
            .save(&path)
            .unwrap();
        assert_eq!(
            read_created_at(&path).as_deref(),
            Some("2026-01-02T03:04:05Z")
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn resolved_settings_layers_correctly() {
        let mut project = ProjectFile::new("P", "t");
        project.settings.playback.proxy_resolution = Some(0.25);

        let global = SettingsLayer {
            color: ColorLayer {
                working_space: Some("Rec709".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let user = SettingsLayer {
            playback: crate::settings::PlaybackLayer {
                proxy_mode: Some(ProxyMode::Off),
                ..Default::default()
            },
            ..Default::default()
        };

        let resolved = project.resolved_settings(Some(&global), Some(&user));
        assert_eq!(resolved.working_space, "Rec709"); // from global
        assert_eq!(resolved.proxy_resolution, 0.25); // from project
        assert_eq!(resolved.proxy_mode, ProxyMode::Off); // from user
    }

    /// A v3 archive without document/main.ron is corrupt, not "legacy": the
    /// source version selects the layout, so its graph entry is never
    /// consulted.
    #[test]
    fn v3_archive_missing_document_is_not_treated_as_legacy() {
        let archive = legacy_archive(
            r#"{
                "format_version": 3,
                "ravel_version": "0.1.0",
                "project_name": "Strict",
                "created_at": "2026-03-01T00:00:00Z",
                "modified_at": "2026-03-02T00:00:00Z",
                "frame_rate": { "num": 30, "den": 1 },
                "resolution": { "width": 1, "height": 1 }
            }"#,
            &legacy_graph(),
        );
        let err = ProjectFile::from_archive(&archive).unwrap_err();
        assert!(matches!(
            err,
            ProjectError::Container(container::ContainerError::MissingEntry(
                container::entry::DOCUMENT
            ))
        ));
    }

    /// A structurally invalid v3 document (here: zero frame-rate
    /// denominator, which would panic playback) is rejected at load with a
    /// typed error instead of being adopted.
    #[test]
    fn v3_archive_with_invalid_document_is_rejected() {
        let mut comp = Composition::new(
            CompId::new(1),
            "Broken",
            (16, 16),
            FrameRate::new(30, 1),
            10,
        );
        comp.frame_rate = FrameRate { num: 30, den: 0 };
        let document = Document::default().with_composition(comp);

        let mut archive = container::RawArchive::new();
        archive.insert(
            container::entry::MANIFEST,
            br#"{
                "format_version": 3,
                "ravel_version": "0.1.0",
                "project_name": "Broken",
                "created_at": "2026-03-01T00:00:00Z",
                "modified_at": "2026-03-02T00:00:00Z",
                "frame_rate": { "num": 30, "den": 1 },
                "resolution": { "width": 16, "height": 16 }
            }"#
            .to_vec(),
        );
        archive.insert(
            container::entry::DOCUMENT,
            ron::to_string(&document).unwrap().into_bytes(),
        );

        let err = ProjectFile::from_archive(&archive).unwrap_err();
        assert!(matches!(err, ProjectError::InvalidDocument(_)));
    }
}
