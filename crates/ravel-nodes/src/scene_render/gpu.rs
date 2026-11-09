// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The GPU renderer of `scene.render`.
//!
//! A frame is one [`ScenePass`] (opaque meshes with depth, then the sorted
//! translucent meshes and Path batches) followed by the same `unpremultiply`
//! compute pass the 2D rasterizer ends with, recorded into the frame's shared
//! encoder. Nothing is submitted here.
//!
//! # Static and dynamic data
//!
//! | | Where | Rebuilt |
//! |---|---|---|
//! | Vertices, normals, UVs, vertex colours, indices | [`StaticMeshCache`] → [`MeshBuffers`] | when the mesh's columns change |
//! | Matrices, tint, flags | `ObjectRecord`, one per object | every frame |
//! | Path vertices and draw items | [`PathBatch`] | every frame |
//!
//! A transform is applied in the vertex shader, so moving an object is a
//! 160-byte record, not a re-upload. The path data is flattened per frame as
//! the 2D rasterizer's is: it is projected from the camera, so it changes
//! whenever the camera or the object does.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use ravel_core::geometry::Geometry;
use ravel_core::scene::Mat4;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, DepthBuffer, DrawClass, GpuContext, GpuFrameBuffer,
    MeshBuffers, MeshVertex, PooledTexture, QuadBatch, RasterPipeline, SceneCall, ScenePass,
    ScenePipelines, ShaderManager, TextureBinding, TextureFormat, TextureKey, TexturePool,
    TextureUsage,
};

use super::prepare::{Blended, MeshKey, MeshObject, Prepared, mesh_data};
use crate::gpu_util;
use crate::rasterize::{self, PathBatch};

const SHADER_SRC: &str = include_str!("../shaders/scene_mesh.wgsl");

/// How many frames a mesh's buffers outlive the last frame that drew them.
///
/// A mesh that is absent for a moment (a layer toggled, a scrub across a cut)
/// keeps its buffers for this long instead of being re-uploaded on return,
/// and a mesh that is rebuilt every frame — an instanced object's expansion
/// has fresh columns each time — is dropped a bounded time after it stopped
/// being asked for rather than accumulating.
const KEEP_FRAMES: u64 = 16;

/// One object's dynamic record, the layout `ObjectData` declares in
/// `scene_mesh.wgsl`: 160 bytes, matrices column-major.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ObjectRecord {
    mvp: [f32; 16],
    model: [f32; 16],
    color: [f32; 4],
    /// `x`: the mesh has vertex colours. `y`: sample the bound texture.
    flags: [f32; 4],
}

fn record_of(object: &MeshObject, has_vertex_colors: bool) -> ObjectRecord {
    ObjectRecord {
        mvp: matrix_array(&object.mvp),
        model: matrix_array(&object.model),
        color: [
            object.color.r,
            object.color.g,
            object.color.b,
            object.color.a,
        ],
        flags: [
            f32::from(u8::from(has_vertex_colors)),
            f32::from(u8::from(object.texture.is_some())),
            0.0,
            0.0,
        ],
    }
}

fn matrix_array(matrix: &Mat4) -> [f32; 16] {
    // `Mat4` stores columns, which is what WGSL's `mat4x4` reads.
    matrix.cols
}

struct CachedMesh {
    buffers: Arc<MeshBuffers>,
    has_vertex_colors: bool,
    last_used: u64,
    /// Keeps every column the key's addresses name alive, so an address
    /// cannot be recycled for another column while this entry exists.
    _geometry: Arc<Geometry>,
}

/// The static buffers of every mesh drawn recently.
///
/// **This is the one place a mesh buffer is allocated.** [`Self::allocate`]
/// is the whole seam: a general GPU buffer pool replaces its body (and the
/// `MeshBuffers` it returns) without touching anything that decides *when* a
/// mesh is stale, which is [`MeshKey`]'s job, or what draws it.
#[derive(Default)]
struct StaticMeshCache {
    entries: HashMap<MeshKey, CachedMesh>,
    frame: u64,
    allocations: u64,
}

impl StaticMeshCache {
    fn allocate(
        &mut self,
        ctx: &GpuContext,
        pipelines: &ScenePipelines,
        object: &MeshObject,
    ) -> Option<(Arc<MeshBuffers>, bool)> {
        let data = mesh_data(&object.geometry, object.primitive)?;
        self.allocations += 1;
        let buffers = MeshBuffers::new(ctx, pipelines, &data.vertices, &data.colors, &data.indices);
        Some((Arc::new(buffers), !data.colors.is_empty()))
    }

    /// The buffers for `object`, uploading them only if no live entry has its
    /// key.
    fn get(
        &mut self,
        ctx: &GpuContext,
        pipelines: &ScenePipelines,
        object: &MeshObject,
    ) -> Option<(Arc<MeshBuffers>, bool)> {
        if let Some(entry) = self.entries.get_mut(&object.key) {
            entry.last_used = self.frame;
            return Some((Arc::clone(&entry.buffers), entry.has_vertex_colors));
        }
        let (buffers, has_vertex_colors) = self.allocate(ctx, pipelines, object)?;
        self.entries.insert(
            object.key.clone(),
            CachedMesh {
                buffers: Arc::clone(&buffers),
                has_vertex_colors,
                last_used: self.frame,
                _geometry: Arc::clone(&object.geometry),
            },
        );
        Some((buffers, has_vertex_colors))
    }

    /// Close a frame: age out what has not been drawn for [`KEEP_FRAMES`].
    fn end_frame(&mut self) {
        let frame = self.frame;
        self.entries
            .retain(|_, entry| frame - entry.last_used <= KEEP_FRAMES);
        self.frame += 1;
    }
}

pub(super) struct GpuSceneRenderer {
    ctx: GpuContext,
    pool: Arc<Mutex<TexturePool>>,
    pipelines: ScenePipelines,
    paths: RasterPipeline,
    unpremultiply: Arc<ComputePipeline>,
    /// Bound wherever a call samples nothing; held for the renderer's life
    /// for the reason the 2D rasterizer's placeholder is.
    placeholder: PooledTexture,
    depth: Mutex<Option<DepthBuffer>>,
    cache: Mutex<StaticMeshCache>,
}

impl GpuSceneRenderer {
    pub(super) fn new(
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
    ) -> Self {
        let shader = shaders
            .compile_source("scene_mesh", SHADER_SRC)
            .expect("scene_mesh.wgsl compilation failed");
        let pipelines = ScenePipelines::new(&ctx, &shader, rasterize::scene_color_target());
        let paths = rasterize::depth_tested_pipeline(&ctx, shaders);
        let unpremultiply = rasterize::unpremultiply_pipeline(shaders);
        let placeholder = pool
            .lock()
            .expect("texture pool poisoned")
            .acquire(TextureKey::new(
                1,
                1,
                TextureFormat::Rgba32Float,
                TextureUsage::TEXTURE_BINDING,
            ));
        Self {
            ctx,
            pool,
            pipelines,
            paths,
            unpremultiply,
            placeholder,
            depth: Mutex::new(None),
            cache: Mutex::new(StaticMeshCache::default()),
        }
    }

    /// How many static mesh buffer sets this renderer has ever uploaded. A
    /// second frame of an unchanged scene must not move it.
    pub(super) fn static_allocations(&self) -> u64 {
        self.cache.lock().expect("mesh cache poisoned").allocations
    }

    /// Buffer sets currently held.
    #[cfg(test)]
    pub(super) fn live_buffer_sets(&self) -> usize {
        self.cache
            .lock()
            .expect("mesh cache poisoned")
            .entries
            .len()
    }

    /// Identities of the buffer sets the most recent frame's meshes resolve to.
    #[cfg(test)]
    pub(super) fn buffer_ids(&self, prepared: &Prepared) -> Vec<u64> {
        let mut cache = self.cache.lock().expect("mesh cache poisoned");
        prepared
            .meshes
            .iter()
            .filter_map(|object| cache.get(&self.ctx, &self.pipelines, object))
            .map(|(buffers, _)| buffers.id())
            .collect()
    }

    pub(super) fn render(&self, prepared: &Prepared) -> anyhow::Result<Arc<dyn NodeData>> {
        let (width, height) = prepared.resolution;
        let span = tracing::debug_span!("gpu_scene_render", width, height);
        let _guard = span.enter();

        // Static buffers: looked up, uploaded only on a miss.
        let mut buffers: Vec<Option<Arc<MeshBuffers>>> = Vec::with_capacity(prepared.meshes.len());
        let mut records = Vec::with_capacity(prepared.meshes.len());
        {
            let mut cache = self.cache.lock().expect("mesh cache poisoned");
            for object in &prepared.meshes {
                match cache.get(&self.ctx, &self.pipelines, object) {
                    Some((mesh, has_colors)) => {
                        records.push(record_of(object, has_colors));
                        buffers.push(Some(mesh));
                    }
                    None => {
                        records.push(record_of(object, false));
                        buffers.push(None);
                    }
                }
            }
            cache.end_frame();
        }

        // One bindable texture per distinct frame.
        let mut images = Vec::new();
        let mut image_slot: HashMap<*const (), usize> = HashMap::new();
        for object in &prepared.meshes {
            let Some(frame) = &object.texture else {
                continue;
            };
            let key = Arc::as_ptr(frame) as *const ();
            if let std::collections::hash_map::Entry::Vacant(slot) = image_slot.entry(key) {
                slot.insert(images.len());
                images.push(
                    gpu_util::ensure_gpu(&self.ctx, &self.pool, frame.as_ref())
                        .context("scene.render: binding an image")?,
                );
            }
        }
        let bindings: Vec<TextureBinding> =
            images.iter().map(gpu_util::GpuImage::binding).collect();
        let texture_of = |object: &MeshObject| {
            object
                .texture
                .as_ref()
                .map(|frame| &bindings[image_slot[&(Arc::as_ptr(frame) as *const ())]])
        };

        // Path batch: flattened per frame, one uniform (depth) per object.
        let mut batch = PathBatch::default();
        let ranges: Vec<_> = prepared
            .paths
            .iter()
            .map(|path| batch.push(&path.geometry, prepared.paint))
            .collect();
        let uniforms: Vec<Vec<u8>> = prepared
            .paths
            .iter()
            .map(|path| rasterize::path_params_bytes(prepared.resolution, path.ndc_depth))
            .collect();
        let storage = batch.storage_bytes();

        let mut calls: Vec<SceneCall<'_>> = Vec::new();
        for &index in &prepared.opaque {
            if let Some(mesh) = &buffers[index] {
                calls.push(SceneCall::Mesh {
                    buffers: mesh,
                    object: index as u32,
                    texture: texture_of(&prepared.meshes[index]),
                    class: DrawClass::Opaque,
                });
            }
        }
        for entry in &prepared.blended {
            match *entry {
                Blended::Mesh(index) => {
                    if let Some(mesh) = &buffers[index] {
                        calls.push(SceneCall::Mesh {
                            buffers: mesh,
                            object: index as u32,
                            texture: texture_of(&prepared.meshes[index]),
                            class: DrawClass::Blended,
                        });
                    }
                }
                Blended::Path(index) => {
                    if !ranges[index].is_empty() {
                        calls.push(SceneCall::Quads {
                            instances: ranges[index].clone(),
                            uniform: &uniforms[index],
                        });
                    }
                }
            }
        }

        let (premul, output) = {
            let mut pool = self.pool.lock().expect("texture pool poisoned");
            (
                pool.acquire(rasterize::scene_color_key(width, height)),
                pool.acquire(gpu_util::tex_key_rw(width, height)),
            )
        };
        let premul_binding = premul.binding();
        let output_binding = output.binding();
        let placeholder_binding = self.placeholder.binding();

        let mut depth = self.depth.lock().expect("depth buffer poisoned");
        if depth.as_ref().is_none_or(|d| d.size() != (width, height)) {
            *depth = Some(DepthBuffer::new(&self.ctx, width, height));
        }
        let depth_buffer = depth.as_ref().expect("depth buffer was just ensured");

        let object_bytes: &[u8] = bytemuck::cast_slice(&records);
        self.ctx.draw_scene(&ScenePass {
            label: "scene.render",
            pipelines: &self.pipelines,
            color: &premul_binding,
            depth: depth_buffer,
            objects: object_bytes,
            placeholder: &placeholder_binding,
            quads: (!batch.is_empty()).then(|| QuadBatch {
                pipeline: &self.paths,
                storage: [&storage[0], &storage[1], &storage[2]],
            }),
            calls: &calls,
        });
        self.ctx.dispatch_compute(&ComputeDispatch {
            label: "scene.render unpremultiply",
            pipeline: &self.unpremultiply,
            inputs: std::slice::from_ref(&premul_binding),
            output: &output_binding,
            uniform: &[],
            width,
            height,
        });
        drop(depth);

        // Safe before the batch is submitted: the recorded passes put these
        // textures in the batch's used set, and the pool refuses to hand out
        // a texture the pending batch still touches.
        self.pool
            .lock()
            .expect("texture pool poisoned")
            .release(premul);
        for image in images {
            image.release(&self.pool);
        }
        Ok(Arc::new(GpuFrameBuffer::new(
            self.ctx.clone(),
            &self.pool,
            output,
            width,
            height,
        )))
    }
}

// The shader reads a vertex as three attributes at offsets 0, 12 and 24, and an
// object record as the struct `scene_mesh.wgsl` declares.
const _: () = assert!(size_of::<MeshVertex>() == 32);
const _: () = assert!(size_of::<ObjectRecord>() == 160);
