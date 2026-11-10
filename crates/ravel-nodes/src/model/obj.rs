// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Wavefront OBJ reading.
//!
//! Each `o` / `g` object becomes one geometry. Faces are triangulated and
//! `v/vt/vn` tuples are merged into single-index vertices by the parser, so
//! a normal is per vertex. Texture coordinates and the `mtllib` materials
//! are ignored — a missing or broken `.mtl` is not an error.

use super::{MeshBuilder, to_scene_space};
use anyhow::Context as _;
use ravel_core::scene::{Scene, SceneObject, Transform3D};
use std::path::Path;
use std::sync::Arc;

pub(super) fn load(path: &Path) -> anyhow::Result<Scene> {
    let options = tobj::LoadOptions {
        triangulate: true,
        single_index: true,
        ..tobj::LoadOptions::default()
    };
    let (models, _materials) = tobj::load_obj(path, &options).context("not a valid OBJ file")?;
    let mut scene = Scene::new();
    for model in models {
        let mesh = model.mesh;
        let positions: Vec<[f32; 3]> = mesh
            .positions
            .chunks_exact(3)
            .map(|p| to_scene_space([p[0], p[1], p[2]]))
            .collect();
        let normals = (!mesh.normals.is_empty()).then(|| {
            mesh.normals
                .chunks_exact(3)
                .map(|n| to_scene_space([n[0], n[1], n[2]]))
                .collect::<Vec<_>>()
        });
        let mut builder = MeshBuilder::new();
        builder
            .push_run(positions, normals, mesh.indices)
            .with_context(|| format!("object {:?}", model.name))?;
        if let Some(geometry) = builder.finish()? {
            scene = scene.with_object(SceneObject::geometry(
                Arc::new(geometry),
                Transform3D::IDENTITY,
            ));
        }
    }
    Ok(scene)
}
