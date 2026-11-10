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

pub use ravel_core::composition::{MODEL_EXTENSIONS, is_model_path};

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
/// any run has normals) with a [`Primitive::Mesh`] per run.
///
/// Normals are all-or-nothing per geometry: when at least one run carries
/// them, a run without gets **smooth normals computed from its triangles**
/// (area-weighted vertex averages, so the winding decides the facing), which
/// keeps the column the length of `P`; when none does, there is no `N`.
#[derive(Default)]
pub(crate) struct MeshBuilder {
    positions: Vec<Vec3>,
    /// Per run, its file normals, or `None` to be computed in `finish`.
    run_normals: Vec<Option<Vec<[f32; 3]>>>,
    runs: Vec<(Range<usize>, Vec<u32>)>,
}

impl MeshBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
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
        self.run_normals
            .push(normals.filter(|n| n.len() == positions.len()));
        self.runs.push((start..self.positions.len(), indices));
        Ok(())
    }

    /// `None` when no run held a triangle.
    pub(crate) fn finish(self) -> anyhow::Result<Option<Geometry>> {
        if self.runs.is_empty() {
            return Ok(None);
        }
        let normals = if self.run_normals.iter().any(Option::is_some) {
            let mut column = Vec::with_capacity(self.positions.len());
            for ((verts, indices), given) in self.runs.iter().zip(self.run_normals) {
                match given {
                    Some(given) => column.extend(given.iter().map(|n| Vec3(n[0], n[1], n[2]))),
                    None => column.extend(
                        smooth_normals(&self.positions[verts.clone()], indices)
                            .into_iter()
                            .map(|n| Vec3(n[0], n[1], n[2])),
                    ),
                }
            }
            Some(column)
        } else {
            None
        };
        let mut geometry = Geometry::from_points3(self.positions);
        if let Some(normals) = normals {
            geometry
                .points_mut()
                .insert(names::N, AttributeArray::Vec3(normals))?;
        }
        for (verts, indices) in &self.runs {
            geometry.push_mesh(verts.clone(), indices);
        }
        geometry.validate()?;
        Ok(Some(geometry))
    }
}

/// Vertex normals of one run: each triangle's cross product (twice its area,
/// so larger faces weigh more) added to its three vertices, then normalised.
/// A vertex no face reaches, or whose faces cancel, keeps the zero
/// vector rather than a made-up direction.
fn smooth_normals(positions: &[Vec3], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut sums = vec![[0.0f32; 3]; positions.len()];
    for triangle in indices.chunks_exact(3) {
        let [a, b, c] = [0, 1, 2].map(|k| positions[triangle[k] as usize]);
        let (u, v) = (
            [b.0 - a.0, b.1 - a.1, b.2 - a.2],
            [c.0 - a.0, c.1 - a.1, c.2 - a.2],
        );
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        for &vertex in triangle {
            for axis in 0..3 {
                sums[vertex as usize][axis] += n[axis];
            }
        }
    }
    sums.into_iter()
        .map(|n| {
            let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if length > 1e-12 {
                [n[0] / length, n[1] / length, n[2] / length]
            } else {
                [0.0; 3]
            }
        })
        .collect()
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
