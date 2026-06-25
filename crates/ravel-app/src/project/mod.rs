// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `.ravprj` project file format — the persistence foundation for Ravel.
//!
//! A project is a zip container (see [`container`]) holding four logical parts:
//!
//! - [`manifest::Manifest`] — metadata + on-disk format version
//! - [`Graph`] serialized as RON (see [`graph_doc`])
//! - [`asset::AssetCollection`] — external media references
//! - [`settings::SettingsLayer`] — the project's settings override layer
//!
//! [`ProjectFile`] ties these together with [`ProjectFile::save`] /
//! [`ProjectFile::load`]. Saving always writes a `.bak` of the previous
//! revision; loading transparently runs the [`migration`] chain so that older
//! files open as the current format. All failure modes surface as
//! [`ProjectError`] — corrupt input never panics.

pub mod asset;
pub mod container;
pub mod graph_doc;
pub mod manifest;
pub mod migration;
pub mod paths;
pub mod settings;

use std::path::Path;
use thiserror::Error;

use ravel_core::graph::Graph;

use crate::project::asset::AssetCollection;
use crate::project::graph_doc::{GraphDoc, GraphDocError};
use crate::project::manifest::Manifest;
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
    pub graph: Graph,
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
            graph: Graph::new(),
            assets: AssetCollection::new(),
            settings: SettingsLayer::default(),
        }
    }

    /// Encode this project into an in-memory [`container::RawArchive`].
    pub fn to_archive(&self) -> Result<container::RawArchive, ProjectError> {
        let mut archive = container::RawArchive::new();

        let manifest_json =
            serde_json::to_string_pretty(&self.manifest).map_err(ProjectError::JsonSerialize)?;
        archive.insert(container::entry::MANIFEST, manifest_json.into_bytes());

        let graph_ron = GraphDoc::graph_to_ron(&self.graph)?;
        archive.insert(container::entry::GRAPH, graph_ron.into_bytes());

        let assets_json = self.assets.to_json().map_err(ProjectError::JsonSerialize)?;
        archive.insert(container::entry::ASSETS, assets_json.into_bytes());

        let settings_toml = self.settings.to_toml()?;
        archive.insert(container::entry::SETTINGS, settings_toml.into_bytes());

        Ok(archive)
    }

    /// Decode a project from a [`container::RawArchive`], running migrations.
    pub fn from_archive(archive: &container::RawArchive) -> Result<Self, ProjectError> {
        // Manifest: parse untyped, migrate, then strongly type.
        let manifest_text = archive.require_text(container::entry::MANIFEST)?;
        let mut manifest_value: serde_json::Value =
            serde_json::from_str(manifest_text).map_err(ProjectError::Manifest)?;
        migration::migrate_to_current(&mut manifest_value)?;
        let manifest: Manifest =
            serde_json::from_value(manifest_value).map_err(ProjectError::Manifest)?;

        // Graph (required).
        let graph_text = archive.require_text(container::entry::GRAPH)?;
        let graph = GraphDoc::graph_from_ron(graph_text)?;

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
            graph,
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

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};

    use crate::project::asset::{AssetId, AssetPath, AssetRef};
    use crate::project::manifest::CURRENT_FORMAT_VERSION;
    use crate::project::settings::{ColorLayer, ProxyMode};

    fn demo_project() -> ProjectFile {
        let graph = Graph::new()
            .add_node(
                ravel_core::graph::Node::new(NodeId::new(1), "read_media")
                    .with_output("out", DataTypeId::FRAME_BUFFER)
                    .with_position(100.0, 200.0),
            )
            .unwrap()
            .add_node(
                ravel_core::graph::Node::new(NodeId::new(2), "color_correct")
                    .with_input("in", &[DataTypeId::FRAME_BUFFER])
                    .with_output("out", DataTypeId::FRAME_BUFFER)
                    .with_position(300.0, 200.0),
            )
            .unwrap();
        let graph = graph
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();

        let mut project = ProjectFile::new("Round Trip", "2026-06-22T10:00:00Z");
        project.graph = graph;
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

    #[test]
    fn archive_roundtrip_preserves_graph() {
        let project = demo_project();
        let archive = project.to_archive().unwrap();
        let back = ProjectFile::from_archive(&archive).unwrap();

        assert_eq!(back.graph.node_count(), 2);
        assert_eq!(back.graph.edge_count(), 1);
        assert_eq!(back.manifest.project_name, "Round Trip");
        assert_eq!(back.assets.assets.len(), 1);
        assert_eq!(back.settings.color.working_space.as_deref(), Some("ACEScg"));

        // Graph documents must be byte-identical after projection.
        let a = GraphDoc::from_graph(&project.graph);
        let b = GraphDoc::from_graph(&back.graph);
        assert_eq!(a, b);
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

        assert_eq!(loaded.graph.node_count(), 2);
        assert_eq!(loaded.graph.edge_count(), 1);
        assert_eq!(loaded.manifest.format_version, CURRENT_FORMAT_VERSION);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(container::backup_path(&path));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn loads_and_migrates_v1_archive() {
        // Hand-craft a v1 archive (old manifest schema) and load it.
        let mut archive = container::RawArchive::new();
        archive.insert(
            container::entry::MANIFEST,
            br#"{
                "format_version": 1,
                "ravel_version": "0.0.1",
                "project_name": "Legacy",
                "created_at": "2026-01-01T00:00:00Z",
                "modified_at": "2026-01-02T00:00:00Z",
                "frame_rate": { "num": 24, "den": 1 },
                "color_space": "aces_1.2"
            }"#
            .to_vec(),
        );
        archive.insert(
            container::entry::GRAPH,
            b"GraphDoc(nodes:[],edges:[])".to_vec(),
        );

        let project = ProjectFile::from_archive(&archive).unwrap();
        assert_eq!(project.manifest.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(project.manifest.color_config.as_deref(), Some("aces_1.2"));
        assert_eq!(project.manifest.resolution.width, 1920);
        // Missing assets/settings default cleanly.
        assert!(project.assets.assets.is_empty());
        assert_eq!(project.settings, SettingsLayer::default());
    }

    #[test]
    fn corrupt_archive_errors_gracefully() {
        // Valid zip but missing the required graph entry.
        let mut archive = container::RawArchive::new();
        archive.insert(container::entry::MANIFEST, br#"{"format_version":2,"ravel_version":"0.1.0","project_name":"P","created_at":"t","modified_at":"t","frame_rate":{"num":30,"den":1},"resolution":{"width":1,"height":1}}"#.to_vec());
        let err = ProjectFile::from_archive(&archive).unwrap_err();
        assert!(matches!(
            err,
            ProjectError::Container(container::ContainerError::MissingEntry(_))
        ));
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
}
