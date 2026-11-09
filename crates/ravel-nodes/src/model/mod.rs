// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Model import (REQ-3D-008): glTF / GLB and Wavefront OBJ into a [`Scene`].
//!
//! Geometry and normals only — materials, textures, animation and skinning
//! are out of scope (`docs/implementation/3d-scene-plan.md`, unit 9). A
//! glTF node hierarchy becomes nested [`Scene`]s, so a transform on a parent
//! node composes onto its children exactly as `scene.add` nesting does; OBJ
//! has no hierarchy, so each of its objects is one flat entry.
//!
//! # Basis
//!
//! glTF and OBJ are `+X` right, `+Y` up, `+Z` toward the viewer. Scene space
//! is `+X` right, `+Y` down, `+Z` away. The change of basis is therefore
//! `(x, y, z) → (x, -y, -z)`: a half turn about `X`, whose determinant is
//! `+1`. It is a **rotation, not a reflection**, so triangle winding keeps
//! its meaning and a normal converts with the same map as a position. Both
//! files are right-handed, as is scene space; a conversion that flipped only
//! one axis would have turned the model inside out. [`to_scene_space`] is the
//! single place the sign flip lives, and everything that reads a file —
//! positions, normals, node translations and rotations — goes through it.
//! Scale is unchanged by a conjugation with a diagonal matrix.

mod gltf;
mod node;
mod obj;

pub use node::MeshImportProcessor;

use anyhow::{Context as _, bail};
use ravel_core::geometry::{AttributeArray, Geometry, Primitive, names};
use ravel_core::scene::{Mat4, Scene};
use ravel_core::types::Vec3;
use std::ops::Range;
use std::path::Path;

/// File extensions [`load`] reads, lower case, without the dot.
pub const MODEL_EXTENSIONS: [&str; 3] = ["gltf", "glb", "obj"];

/// Whether `path` names a model file, by extension alone.
pub fn is_model_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            MODEL_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// A glTF / OBJ vector — position or normal — in scene space.
pub(crate) fn to_scene_space(v: [f32; 3]) -> [f32; 3] {
    [v[0], -v[1], -v[2]]
}

/// Read the model at `path` into a [`Scene`] holding its meshes and no camera.
///
/// Fails when the file cannot be read or parsed, or when it holds no
/// triangles at all — an empty scene would render as nothing and hide that
/// the file was the wrong one.
pub fn load(path: &Path) -> anyhow::Result<Scene> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let scene = match extension.as_deref() {
        Some("gltf" | "glb") => gltf::load(path),
        Some("obj") => obj::load(path),
        _ => bail!("{} is not a glTF, GLB or OBJ file", path.display()),
    }
    .with_context(|| format!("failed to read model {}", path.display()))?;
    if scene.flatten().is_empty() {
        bail!("{} contains no triangle meshes", path.display());
    }
    Ok(scene)
}

/// Collects the triangle runs of one geometry: one `P` column (and `N` when
/// every run has normals) with a [`Primitive::Mesh`] per run.
#[derive(Default)]
pub(crate) struct MeshBuilder {
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    /// Every run so far supplied normals. One run without them drops the
    /// column: a partly filled `N` would light the other runs with zeros.
    normals_complete: bool,
    runs: Vec<(Range<usize>, Vec<u32>)>,
}

impl MeshBuilder {
    pub(crate) fn new() -> Self {
        Self {
            normals_complete: true,
            ..Self::default()
        }
    }

    /// Add one indexed triangle run. `positions` and `normals` are already in
    /// scene space; `indices` are relative to this run's first vertex.
    /// Errors on an index past the run or a length that is not whole
    /// triangles, so a malformed file is refused here instead of reaching the
    /// renderer.
    pub(crate) fn push_run(
        &mut self,
        positions: Vec<[f32; 3]>,
        normals: Option<Vec<[f32; 3]>>,
        indices: Vec<u32>,
    ) -> anyhow::Result<()> {
        if !indices.len().is_multiple_of(3) {
            bail!(
                "{} indices is not a whole number of triangles",
                indices.len()
            );
        }
        if let Some(bad) = indices.iter().find(|&&i| i as usize >= positions.len()) {
            bail!(
                "index {bad} is past the {} vertices of its mesh",
                positions.len()
            );
        }
        if indices.is_empty() {
            return Ok(());
        }
        let start = self.positions.len();
        self.positions
            .extend(positions.iter().map(|p| Vec3(p[0], p[1], p[2])));
        match normals.filter(|n| n.len() == positions.len()) {
            Some(normals) => self
                .normals
                .extend(normals.iter().map(|n| Vec3(n[0], n[1], n[2]))),
            None => self.normals_complete = false,
        }
        self.runs.push((start..self.positions.len(), indices));
        Ok(())
    }

    /// `None` when no run held a triangle.
    pub(crate) fn finish(self) -> anyhow::Result<Option<Geometry>> {
        if self.runs.is_empty() {
            return Ok(None);
        }
        let mut geometry = Geometry::from_points3(self.positions);
        if self.normals_complete {
            geometry
                .points_mut()
                .insert(names::N, AttributeArray::Vec3(self.normals))?;
        }
        for (verts, indices) in &self.runs {
            geometry.push_mesh(verts.clone(), indices);
        }
        geometry.validate()?;
        Ok(Some(geometry))
    }
}

/// Every mesh of `scene` in one geometry, positions and normals baked into
/// scene space. This is what lets a downstream geometry node modulate an
/// imported model: a [`Scene`] is not an input any of them takes.
///
/// Normals follow the inverse transpose of each object's matrix, so a
/// non-uniformly scaled node still lights correctly; they are re-normalised
/// and left absent when any object has none.
pub fn flatten_to_geometry(scene: &Scene) -> anyhow::Result<Geometry> {
    let mut positions = Vec::new();
    let mut normals: Vec<Vec3> = Vec::new();
    let mut normals_complete = true;
    let mut meshes: Vec<(Range<usize>, Vec<u32>)> = Vec::new();
    for object in scene.flatten() {
        let source = &object.geometry;
        let Some(column) = source.points().get(names::P) else {
            continue;
        };
        let AttributeArray::Vec3(local) = column.as_ref() else {
            continue;
        };
        let base = positions.len();
        positions.extend(local.iter().map(|p| {
            let world = object.world_transform.transform_point3([p.0, p.1, p.2]);
            Vec3(world[0], world[1], world[2])
        }));
        match source.points().get(names::N).map(|n| n.as_ref()) {
            Some(AttributeArray::Vec3(local_normals)) => {
                let matrix = normal_matrix(&object.world_transform);
                normals.extend(local_normals.iter().map(|n| {
                    let v = mul3(&matrix, [n.0, n.1, n.2]);
                    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                    let v = if length > 1e-12 {
                        [v[0] / length, v[1] / length, v[2] / length]
                    } else {
                        v
                    };
                    Vec3(v[0], v[1], v[2])
                }));
            }
            _ => normals_complete = false,
        }
        for primitive in source.primitives() {
            let Primitive::Mesh { verts, .. } = primitive else {
                continue;
            };
            let Some(indices) = source.mesh_indices(primitive) else {
                continue;
            };
            meshes.push(((verts.start + base)..(verts.end + base), indices.to_vec()));
        }
    }
    let mut geometry = Geometry::from_points3(positions);
    if normals_complete && !normals.is_empty() {
        geometry
            .points_mut()
            .insert(names::N, AttributeArray::Vec3(normals))?;
    }
    for (verts, indices) in &meshes {
        geometry.push_mesh(verts.clone(), indices);
    }
    geometry.validate()?;
    Ok(geometry)
}

/// The cofactor matrix of `m`'s upper-left 3x3, row-major — the inverse
/// transpose up to a positive factor, so it needs no division. A reflection's
/// negative determinant flips every cofactor, which is undone by the sign.
fn normal_matrix(m: &Mat4) -> [[f32; 3]; 3] {
    let a = |r: usize, c: usize| m.element(r, c);
    let cof =
        |r0: usize, r1: usize, c0: usize, c1: usize| a(r0, c0) * a(r1, c1) - a(r0, c1) * a(r1, c0);
    let cofactors = [
        [cof(1, 2, 1, 2), -cof(1, 2, 0, 2), cof(1, 2, 0, 1)],
        [-cof(0, 2, 1, 2), cof(0, 2, 0, 2), -cof(0, 2, 0, 1)],
        [cof(0, 1, 1, 2), -cof(0, 1, 0, 2), cof(0, 1, 0, 1)],
    ];
    let determinant =
        a(0, 0) * cofactors[0][0] + a(0, 1) * cofactors[0][1] + a(0, 2) * cofactors[0][2];
    if determinant < 0.0 {
        cofactors.map(|row| row.map(|v| -v))
    } else {
        cofactors
    }
}

fn mul3(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}
