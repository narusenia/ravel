// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `mesh.import` — a model file from the document's asset table as geometry.
//!
//! The `asset_id` parameter is the same reference a `media` node holds
//! ([`ravel_core::composition::MEDIA_ASSET_PARAM_KEY`]) and resolves through
//! the same [`MediaAssetEntry::resolved`] path, so a project-relative model
//! moves with the project and a relink reaches it like any footage.
//!
//! Two outputs: `scene` keeps the file's node hierarchy as nested scenes;
//! `geometry` is every mesh baked into one geometry in scene space, which is
//! what the geometry and field nodes take.
//!
//! A missing asset, an offline one, or a file that no longer parses yields
//! empty outputs and one warning per asset — the composition around it still
//! renders, as for `media` — while the import path refuses such a file up
//! front and says why. An unset `asset_id` is a hard error, like `media`.

use ravel_core::composition::MediaAssetEntry;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::Geometry;
use ravel_core::graph::Node;
use ravel_core::id::AssetId;
use ravel_core::scene::Scene;
use ravel_core::types::{NodeData, PortRecord};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The parse of one file, kept until the path or its content changes.
struct Loaded {
    path: PathBuf,
    /// [`MediaAssetEntry::content_revision`]: an overwrite in place keeps the
    /// path, so the path alone would serve the old model.
    revision: u64,
    outcome: Result<(Arc<Scene>, Arc<Geometry>), String>,
}

#[derive(Default)]
pub struct MeshImportProcessor {
    loaded: Mutex<Option<Arc<Loaded>>>,
    /// Asset labels already warned about; evaluation runs every frame.
    warned: Mutex<HashSet<String>>,
}

impl MeshImportProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self::default()
    }

    fn load(&self, path: &std::path::Path, revision: u64) -> Arc<Loaded> {
        let mut slot = self.loaded.lock().expect("model cache lock poisoned");
        if let Some(hit) = slot
            .as_ref()
            .filter(|loaded| loaded.path == path && loaded.revision == revision)
        {
            return Arc::clone(hit);
        }
        let outcome = super::load(path)
            .and_then(|scene| {
                let geometry = super::flatten_to_geometry(&scene)?;
                Ok((Arc::new(scene), Arc::new(geometry)))
            })
            .map_err(|error| format!("{error:#}"));
        let loaded = Arc::new(Loaded {
            path: path.to_path_buf(),
            revision,
            outcome,
        });
        *slot = Some(Arc::clone(&loaded));
        loaded
    }

    fn empty(&self, label: &str, detail: impl FnOnce() -> String) -> Arc<dyn NodeData> {
        let mut warned = self.warned.lock().expect("model warn lock poisoned");
        if warned.insert(label.to_string()) {
            tracing::warn!("mesh.import: asset {label}: {}", detail());
        }
        record(Arc::new(Scene::new()), Arc::new(Geometry::new()))
    }
}

fn record(scene: Arc<Scene>, geometry: Arc<Geometry>) -> Arc<dyn NodeData> {
    Arc::new(PortRecord(vec![
        scene as Arc<dyn NodeData>,
        geometry as Arc<dyn NodeData>,
    ]))
}

impl NodeProcessor for MeshImportProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let reference = params.str_or("asset_id", "");
        anyhow::ensure!(!reference.is_empty(), "mesh.import: asset_id is not set");
        let document = scope
            .document()
            .ok_or_else(|| anyhow::anyhow!("mesh.import: no document set on the evaluator"))?;
        let asset: Option<&MediaAssetEntry> =
            AssetId::from_param_value(reference).and_then(|id| document.get_media_asset(id));
        let Some(asset) = asset else {
            return Ok(self.empty(reference, || {
                "the project has no such asset (offline)".into()
            }));
        };
        let label = format!("{:?} ({reference})", asset.name);
        let Some(path) = asset.resolved.as_ref() else {
            return Ok(self.empty(&label, || {
                format!("offline (unresolved path {})", asset.path)
            }));
        };
        let loaded = self.load(path, asset.content_revision);
        match &loaded.outcome {
            Ok((scene, geometry)) => Ok(record(Arc::clone(scene), Arc::clone(geometry))),
            Err(error) => Ok(self.empty(&label, || error.clone())),
        }
    }
}
