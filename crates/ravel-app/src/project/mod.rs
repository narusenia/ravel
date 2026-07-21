// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `.ravprj` project file format — the persistence foundation for Ravel.
//!
//! A project is a zip container (see [`container`]) holding four logical parts:
//!
//! - [`manifest::Manifest`] — metadata + on-disk format version
//! - [`Document`] serialized as RON (`document/main.ron`, format v3)
//! - [`asset::AssetCollection`] — external media references
//! - [`settings::SettingsLayer`] — the project's settings override layer
//!
//! [`ProjectFile`] ties these together with [`ProjectFile::save`] /
//! [`ProjectFile::load`]. Saving always writes a `.bak` of the previous
//! revision; loading transparently runs the [`migration`] chain so that older
//! files open as the current format. All failure modes surface as
//! [`ProjectError`] — corrupt input never panics.
//!
//! Media assets are persisted inside `document/main.ron` as absolute paths
//! ([`Document::media_assets`]). `assets/refs.json` is retained for the
//! future media-bin asset management (relative paths, proxies, hashes) and
//! is currently written as an empty collection.

pub mod asset;
pub mod container;
pub mod graph_doc;
pub mod manifest;
pub mod migration;
pub mod paths;
pub mod settings;
pub mod timestamp;

use std::path::Path;
use thiserror::Error;

use ravel_core::composition::{Composition, Document};
use ravel_core::id::CompId;
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::types::FrameRate;

use crate::project::asset::AssetCollection;
use crate::project::graph_doc::{GraphDoc, GraphDocError};
use crate::project::manifest::{Manifest, RationalRate, Resolution};
use crate::project::settings::{ResolvedSettings, SettingsLayer};

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

    #[error("failed to parse assets/refs.json: {0}")]
    Assets(#[source] serde_json::Error),

    #[error("failed to serialize JSON: {0}")]
    JsonSerialize(#[source] serde_json::Error),

    #[error("failed to parse settings.toml: {0}")]
    SettingsParse(#[from] toml::de::Error),

    #[error("failed to serialize settings.toml: {0}")]
    SettingsSerialize(#[from] toml::ser::Error),
}

/// A fully-loaded Ravel project.
#[derive(Clone, Debug)]
pub struct ProjectFile {
    pub manifest: Manifest,
    pub document: Document,
    pub assets: AssetCollection,
    /// The project-level settings layer (highest priority below the user layer).
    pub settings: SettingsLayer,
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
            assets: AssetCollection::new(),
            settings: SettingsLayer::default(),
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

    /// Encode this project into an in-memory [`container::RawArchive`].
    pub fn to_archive(&self) -> Result<container::RawArchive, ProjectError> {
        let mut archive = container::RawArchive::new();

        let manifest_json =
            serde_json::to_string_pretty(&self.manifest).map_err(ProjectError::JsonSerialize)?;
        archive.insert(container::entry::MANIFEST, manifest_json.into_bytes());

        let document_ron = document_to_ron(&self.document)?;
        archive.insert(container::entry::DOCUMENT, document_ron.into_bytes());

        let assets_json = self.assets.to_json().map_err(ProjectError::JsonSerialize)?;
        archive.insert(container::entry::ASSETS, assets_json.into_bytes());

        let settings_toml = self.settings.to_toml()?;
        archive.insert(container::entry::SETTINGS, settings_toml.into_bytes());

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

        // Document: v3 archives carry document/main.ron (required — a v3
        // archive without one is corrupt, not legacy). v1/v2 archives carry
        // only the legacy flat graph (graph/main.ron), which is wrapped in a
        // fresh Document (the archive-level half of the v2→v3 migration).
        let document = if source_version >= 3 {
            let text = archive.require_text(container::entry::DOCUMENT)?;
            let mut registry = NodeRegistry::new();
            register_builtins(&mut registry);
            // `normalize_param_ports`: archives written before parameter
            // ports existed deserialize pre-exposed pins (e.g. rasterize
            // `color`) with `is_param: false`; upgrade them so connected
            // pins keep driving their parameter.
            // `normalize_net_in_ports`: archives written before the frame
            // index port existed get `f` appended to each layer's In node.
            // `normalize_variadic_input_ports`: template-declared trailing
            // groups gain membership flags and one empty trailing slot.
            ron::from_str::<Document>(text)
                .map_err(ProjectError::DocumentParse)?
                .normalize_param_ports()
                .normalize_net_in_ports()
                .normalize_variadic_input_ports(&registry)
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

        // Assets (optional — absence yields an empty collection).
        let assets = match archive.get(container::entry::ASSETS) {
            Some(bytes) => {
                let text = std::str::from_utf8(bytes).map_err(|_| {
                    ProjectError::Container(container::ContainerError::NotUtf8 {
                        name: container::entry::ASSETS.to_string(),
                    })
                })?;
                AssetCollection::from_json(text).map_err(ProjectError::Assets)?
            }
            None => AssetCollection::new(),
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

        Ok(Self {
            manifest,
            document,
            assets,
            settings,
        })
    }

    /// Save the project to `path`, backing up any existing file to `<path>.bak`.
    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        let archive = self.to_archive()?;
        container::write_file(path, &archive)?;
        Ok(())
    }

    /// Load a project from `path`, migrating older format versions in place.
    pub fn load(path: &Path) -> Result<Self, ProjectError> {
        let archive = container::read_file(path)?;
        Self::from_archive(&archive)
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

/// Serialize a [`Document`] to pretty RON (same style as [`GraphDoc`]:
/// struct names, two-space indent).
fn document_to_ron(document: &Document) -> Result<String, ProjectError> {
    let config = ron::ser::PrettyConfig::new()
        .struct_names(true)
        .indentor("  ".to_string());
    ron::ser::to_string_pretty(document, config).map_err(ProjectError::DocumentSerialize)
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
    use ravel_core::composition::{BlendMode, Layer, TrackMatte, TrackMatteKind};
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex};
    use ravel_core::network as net;

    use crate::project::asset::{AssetId, AssetPath, AssetRef};
    use crate::project::manifest::CURRENT_FORMAT_VERSION;
    use crate::project::settings::{ColorLayer, ProxyMode};

    fn keyframed_channel(keys: &[(u64, f32)]) -> AnimationChannel {
        let mut curve = KeyframeCurve::new();
        for &(frame, value) in keys {
            curve.insert(frame, value, Interpolation::Linear);
        }
        AnimationChannel::keyframes(curve)
    }

    /// A document exercising everything the v3 format must persist: a layered
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

        // A fully-dressed layer: keyframed opacity, reserved fields set
        // (time_remap, track_matte), adjustment + parent + solo.
        let hero = Layer::new(LayerId::new(11), "Hero", network)
            .with_time(-10, 5, 120)
            .with_blend_mode(BlendMode::Multiply)
            .with_parent(LayerId::new(12));
        let hero = Layer {
            opacity: keyframed_channel(&[(0, 0.0), (30, 1.0)]),
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
    }

    fn demo_project() -> ProjectFile {
        let mut project =
            ProjectFile::from_document("Round Trip", "2026-06-22T10:00:00Z", demo_document());
        project.assets.assets.push(AssetRef {
            id: AssetId("asset_001".into()),
            path: AssetPath::Variable {
                var: "${PROJECT_ROOT}".into(),
                rel: "footage/bg.mov".into(),
            },
            hash: Some("sha256:abc".into()),
            proxy: None,
            metadata: Default::default(),
        });
        project.settings.color = ColorLayer {
            working_space: Some("ACEScg".into()),
            ..Default::default()
        };
        project
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

        // Full structural equality: layers, networks, keyframes, reserved
        // fields, flat graph, and media assets all survive.
        assert_eq!(back.document, project.document);
        assert_eq!(back.manifest.project_name, "Round Trip");
        assert_eq!(back.assets.assets.len(), 1);
        assert_eq!(back.settings.color.working_space.as_deref(), Some("ACEScg"));
        // The manifest is stamped from the root composition.
        assert_eq!(back.manifest.frame_rate, RationalRate::new(24, 1));
        assert_eq!(back.manifest.resolution, Resolution::new(1280, 720));
    }

    #[test]
    fn archive_serialization_is_byte_identical() {
        let project = demo_project();
        // Diff-friendly persistence: encoding twice is byte-identical.
        assert_eq!(project.to_archive().unwrap(), project.to_archive().unwrap());
    }

    #[test]
    fn v3_archives_do_not_contain_the_legacy_graph_entry() {
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
        // Missing assets/settings default cleanly.
        assert!(project.assets.assets.is_empty());
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
            playback: crate::project::settings::PlaybackLayer {
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
