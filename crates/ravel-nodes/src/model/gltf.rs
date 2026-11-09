// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! glTF 2.0 (`.gltf` and `.glb`) reading.
//!
//! The `gltf` crate runs without its `import` feature, which would also pull
//! an image decoder for textures nobody here uses; buffers are therefore
//! resolved by hand: the GLB binary chunk, a `data:` URI, or a file beside the
//! model. Only `TRIANGLES` primitives are read — points, lines, strips and
//! fans are skipped — and only `POSITION`, `NORMAL` and the indices.

use super::{MeshBuilder, to_scene_space};
use anyhow::{Context as _, bail, ensure};
use base64::Engine as _;
use gltf::buffer::Source;
use ravel_core::geometry::Geometry;
use ravel_core::geometry::rotation::Quat;
use ravel_core::scene::{Scene, SceneObject, Transform3D};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

/// A node chain deeper than this is a cycle or an attack, not a model.
const MAX_DEPTH: usize = 256;

pub(super) fn load(path: &Path) -> anyhow::Result<Scene> {
    let bytes = std::fs::read(path).context("cannot read the file")?;
    let mut document = gltf::Gltf::from_slice(&bytes).context("not a valid glTF file")?;
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let blob = document.blob.take();
    let buffers = buffers(&document, blob, directory)?;

    let converter = Converter {
        buffers,
        geometries: RefCell::default(),
    };
    let roots: Vec<gltf::Node> = match document
        .default_scene()
        .or_else(|| document.scenes().next())
    {
        Some(scene) => scene.nodes().collect(),
        // A file may hold meshes and no scene; every node nothing else
        // parents is then a root.
        None => {
            let children: HashSet<usize> = document
                .nodes()
                .flat_map(|node| node.children().map(|child| child.index()))
                .collect();
            document
                .nodes()
                .filter(|node| !children.contains(&node.index()))
                .collect()
        }
    };
    let mut scene = Scene::new();
    for node in roots {
        if let Some(object) = converter.object(&node, 0)? {
            scene = scene.with_object(object);
        }
    }
    Ok(scene)
}

/// The bytes of every buffer, each checked against its declared length — the
/// accessor reader indexes into them and does not bounds-check against the
/// declaration.
fn buffers(
    document: &gltf::Gltf,
    mut blob: Option<Vec<u8>>,
    directory: &Path,
) -> anyhow::Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for buffer in document.buffers() {
        let data = match buffer.source() {
            Source::Bin => blob
                .take()
                .context("the file refers to a binary chunk it does not have")?,
            Source::Uri(uri) => match uri.strip_prefix("data:") {
                Some(rest) => {
                    let (_, encoded) = rest
                        .split_once(";base64,")
                        .context("only base64 data URIs are supported")?;
                    base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .map_err(|error| anyhow::anyhow!("invalid base64 buffer: {error}"))?
                }
                None => std::fs::read(external_buffer_path(directory, uri)?)
                    .with_context(|| format!("cannot read buffer file {uri}"))?,
            },
        };
        ensure!(
            data.len() >= buffer.length(),
            "buffer {} holds {} bytes but declares {}",
            buffer.index(),
            data.len(),
            buffer.length()
        );
        out.push(data);
    }
    Ok(out)
}

/// Where a glTF buffer URI points: a relative reference resolved beside the
/// model, with `%XX` escapes decoded. A URI that leaves the model's directory
/// (an absolute path, a `..` component, a scheme other than `data:`) is
/// refused, so opening a model never reads an arbitrary file into a mesh.
fn external_buffer_path(directory: &Path, uri: &str) -> anyhow::Result<std::path::PathBuf> {
    use std::path::Component;
    let decoded = percent_encoding::percent_decode_str(uri)
        .decode_utf8()
        .with_context(|| format!("buffer URI {uri} is not UTF-8 once decoded"))?;
    let relative = Path::new(decoded.as_ref());
    ensure!(
        !decoded.contains(':')
            && relative
                .components()
                .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
        "buffer URI {uri} points outside the model's folder"
    );
    Ok(directory.join(relative))
}

struct Converter {
    buffers: Vec<Vec<u8>>,
    /// One geometry per glTF mesh, shared by every node that instances it.
    geometries: RefCell<HashMap<usize, Option<Arc<Geometry>>>>,
}

impl Converter {
    /// One node as a scene object, or `None` when it and its subtree draw
    /// nothing. A node with children becomes a nested scene so its transform
    /// composes onto theirs; a childless mesh node is a plain geometry.
    fn object(&self, node: &gltf::Node, depth: usize) -> anyhow::Result<Option<SceneObject>> {
        ensure!(
            depth < MAX_DEPTH,
            "the node hierarchy is deeper than {MAX_DEPTH}"
        );
        let transform = transform_of(node);
        let geometry = match node.mesh() {
            Some(mesh) => self.geometry(&mesh)?,
            None => None,
        };
        let mut children = Vec::new();
        for child in node.children() {
            if let Some(object) = self.object(&child, depth + 1)? {
                children.push(object);
            }
        }
        Ok(match (geometry, children.is_empty()) {
            (None, true) => None,
            (Some(geometry), true) => Some(SceneObject::geometry(geometry, transform)),
            (geometry, false) => {
                let mut nested = Scene::new();
                if let Some(geometry) = geometry {
                    nested =
                        nested.with_object(SceneObject::geometry(geometry, Transform3D::IDENTITY));
                }
                for child in children {
                    nested = nested.with_object(child);
                }
                Some(SceneObject::scene(Arc::new(nested), transform))
            }
        })
    }

    fn geometry(&self, mesh: &gltf::Mesh) -> anyhow::Result<Option<Arc<Geometry>>> {
        if let Some(cached) = self.geometries.borrow().get(&mesh.index()) {
            return Ok(cached.clone());
        }
        let mut builder = MeshBuilder::new();
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let reader = primitive
                .reader(|buffer| self.buffers.get(buffer.index()).map(|data| data.as_slice()));
            let Some(positions) = reader.read_positions() else {
                bail!("mesh {} has a primitive without POSITION", mesh.index());
            };
            let positions: Vec<[f32; 3]> = positions.map(to_scene_space).collect();
            let normals = reader
                .read_normals()
                .map(|normals| normals.map(to_scene_space).collect::<Vec<_>>());
            let indices: Vec<u32> = match reader.read_indices() {
                Some(indices) => indices.into_u32().collect(),
                None => (0..positions.len() as u32).collect(),
            };
            builder
                .push_run(positions, normals, indices)
                .with_context(|| format!("mesh {}", mesh.index()))?;
        }
        let geometry = builder.finish()?.map(Arc::new);
        self.geometries
            .borrow_mut()
            .insert(mesh.index(), geometry.clone());
        Ok(geometry)
    }
}

/// A node's local transform in scene space.
///
/// glTF gives translation, rotation (a quaternion) and scale; a `matrix` is
/// decomposed into them first. The basis change is a conjugation by the half
/// turn `diag(1, -1, -1)`: translation maps like a point, the rotation keeps
/// its angle and about-axis `(x, y, z)` becomes `(x, -y, -z)`, and scale is
/// untouched.
fn transform_of(node: &gltf::Node) -> Transform3D {
    let (translation, rotation, scale) = node.transform().decomposed();
    let [rx, ry, rz, rw] = rotation;
    let converted = Quat(rx, -ry, -rz, rw).normalized();
    let euler = converted.to_euler_zyx();
    Transform3D {
        translate: to_scene_space(translation),
        rotate: [
            euler.0.to_degrees(),
            euler.1.to_degrees(),
            euler.2.to_degrees(),
        ],
        scale,
        pivot: [0.0; 3],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_uri_resolves_beside_the_model_with_escapes_decoded() {
        let dir = Path::new("/models/ship");
        assert_eq!(
            external_buffer_path(dir, "mesh%20data.bin").unwrap(),
            dir.join("mesh data.bin")
        );
        assert_eq!(
            external_buffer_path(dir, "./parts/hull.bin").unwrap(),
            dir.join("./parts/hull.bin")
        );
    }

    #[test]
    fn a_buffer_uri_that_leaves_the_model_folder_is_refused() {
        let dir = Path::new("/models/ship");
        for uri in [
            "../secret.bin",
            "parts/../../secret.bin",
            "%2E%2E/secret.bin",
            "/etc/passwd",
            "file:///etc/passwd",
            "C:/secret.bin",
        ] {
            assert!(
                external_buffer_path(dir, uri).is_err(),
                "{uri} was accepted"
            );
        }
    }
}
