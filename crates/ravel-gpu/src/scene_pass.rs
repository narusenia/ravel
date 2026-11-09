// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The render pass `scene.render` draws a 3D scene with.
//!
//! [`GpuContext::draw_quads`] draws instanced quads from procedurally expanded
//! data and has no vertex buffers and no depth. A scene is the opposite shape:
//! triangle meshes that are large, change rarely, and must hide one another.
//! [`ScenePass`] is that second pipeline, recorded into the same frame-shared
//! encoder as every other pass ([`GpuContext::draw_scene`]).
//!
//! # What lives where
//!
//! | Data | Type | Rebuilt |
//! |---|---|---|
//! | Static mesh: vertices, normals, UVs, indices, vertex colours | [`MeshBuffers`] | only when the caller makes a new one |
//! | Per object: matrices, colour, flags | [`ScenePass::objects`] | every pass |
//!
//! The split is the point. A transform is applied in the vertex shader, never
//! baked into a vertex, so animating an object rewrites 160 bytes of its object
//! record and leaves its mesh alone. This module never decides *when* a
//! [`MeshBuffers`] is stale — that is the caller's identity judgement, because
//! only the caller knows what a mesh is made of. It only makes a mesh that
//! survives from pass to pass cost nothing: the pass takes it by reference.
//!
//! # Draw order
//!
//! One render pass, one colour attachment, one depth attachment, calls in slice
//! order. The caller puts opaque meshes first ([`DrawClass::Opaque`] writes
//! depth) and the depth-sorted translucent meshes and path batches after
//! ([`DrawClass::Blended`] and [`SceneCall::Quads`] test depth and do not write
//! it).
//!
//! # Shader contract
//!
//! The mesh shader declares `@group(0) @binding(0)` the object records,
//! `@group(1) @binding(0)` the vertex colours, `@group(2) @binding(0)` the
//! sampled texture, and takes vertex buffer 0 as [`MeshVertex`] at locations
//! 0 (position), 1 (normal) and 2 (uv). Entry points are `mesh_vertex` and
//! `mesh_fragment`. An object is drawn as instance `object`, so the shader
//! reads its record by `instance_index`.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use wgpu::util::DeviceExt as _;

use crate::binding::{BindingDesc, BindingKind, ShaderVisibility};
use crate::device::GpuContext;
use crate::dispatch::TextureBinding;
use crate::raster::{ColorTarget, RasterPipeline};
use crate::shader::CompiledShader;

/// The depth format of a scene's depth attachment.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// The smallest object buffer a pass binds. A runtime-sized array binding
/// must hold at least one element, and an empty scene has none to give.
const MIN_OBJECT_BYTES: usize = 256;

static NEXT_MESH_ID: AtomicU64 = AtomicU64::new(1);

/// One vertex of a static mesh, in the layout the vertex shader reads.
///
/// 32 bytes, the figure `docs/implementation/perf-baseline.md` sizes the
/// upload budget with.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshVertex {
    /// Object-space position.
    pub position: [f32; 3],
    /// Object-space normal. Interpolated and handed to the fragment stage;
    /// nothing shades with it yet.
    pub normal: [f32; 3],
    /// Texture coordinate, `(0, 0)` at the top-left texel.
    pub uv: [f32; 2],
}

/// How a mesh call interacts with the depth attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawClass {
    /// Tests and **writes** depth. Draw these first.
    Opaque,
    /// Tests depth and does not write it, so a translucent surface never
    /// hides what is drawn after it.
    Blended,
}

/// A mesh's static GPU buffers.
///
/// Created once and shared by reference into as many [`ScenePass`]es as the
/// caller likes; this crate never rebuilds one on its own.
pub struct MeshBuffers {
    id: u64,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    colors: wgpu::BindGroup,
    byte_size: u64,
}

impl MeshBuffers {
    /// Upload a mesh.
    ///
    /// `colors` is one straight-alpha RGBA per vertex, or empty for a mesh
    /// with no vertex colours (the shader is told so by the object record's
    /// flags and never reads the placeholder bound here).
    ///
    /// Every index must be below `vertices.len()`; the caller validates, as
    /// `Geometry::validate` already does for a mesh primitive.
    pub fn new(
        ctx: &GpuContext,
        pipelines: &ScenePipelines,
        vertices: &[MeshVertex],
        colors: &[[f32; 4]],
        indices: &[u32],
    ) -> Self {
        let device = ctx.device();
        let placeholder = [[1.0f32; 4]];
        let color_bytes: &[u8] = if colors.is_empty() {
            bytemuck::cast_slice(&placeholder)
        } else {
            bytemuck::cast_slice(colors)
        };
        // wgpu rejects a zero-sized buffer, and a mesh with no triangles is a
        // legal (empty) value; its draw is skipped by `index_count == 0`.
        let vertex_bytes: &[u8] = if vertices.is_empty() {
            &[0u8; size_of::<MeshVertex>()]
        } else {
            bytemuck::cast_slice(vertices)
        };
        let index_bytes: &[u8] = if indices.is_empty() {
            &[0u8; 4]
        } else {
            bytemuck::cast_slice(indices)
        };
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scene mesh vertices"),
            contents: vertex_bytes,
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scene mesh indices"),
            contents: index_bytes,
            usage: wgpu::BufferUsages::INDEX,
        });
        let color_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scene mesh colors"),
            contents: color_bytes,
            usage: wgpu::BufferUsages::STORAGE,
        });
        let colors = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene mesh colors"),
            layout: &pipelines.mesh_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: color_buffer.as_entire_binding(),
            }],
        });
        Self {
            id: NEXT_MESH_ID.fetch_add(1, Ordering::Relaxed),
            vertices: vertex_buffer,
            indices: index_buffer,
            index_count: indices.len() as u32,
            colors,
            byte_size: (vertex_bytes.len() + index_bytes.len() + color_bytes.len()) as u64,
        }
    }

    /// Identity of this allocation: unique and never reused, so "the second
    /// frame drew the same buffers" is a comparison of two numbers.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Number of indices the mesh draws.
    pub fn index_count(&self) -> u32 {
        self.index_count
    }

    /// Bytes of GPU memory the three buffers hold.
    pub fn byte_size(&self) -> u64 {
        self.byte_size
    }
}

/// The depth attachment of a scene pass, sized to the colour target.
pub struct DepthBuffer {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl DepthBuffer {
    /// A `width` x `height` depth attachment. The pass clears it to the far
    /// plane, so a held one is reusable across frames.
    pub fn new(ctx: &GpuContext, width: u32, height: u32) -> Self {
        let texture = ctx.device().create_texture(&wgpu::TextureDescriptor {
            label: Some("scene depth"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            _texture: texture,
            view,
            width,
            height,
        }
    }

    /// Size in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// The two mesh pipelines (depth-writing and depth-testing) and the three
/// bind-group layouts of the shader contract.
pub struct ScenePipelines {
    opaque: wgpu::RenderPipeline,
    blended: wgpu::RenderPipeline,
    objects_layout: wgpu::BindGroupLayout,
    mesh_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
}

/// The depth state the blended mesh pipeline and the depth-tested quad
/// pipeline share: compare, never write.
pub(crate) fn blended_depth_state() -> wgpu::DepthStencilState {
    depth_state(false)
}

fn depth_state(write: bool) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(write),
        depth_compare: Some(wgpu::CompareFunction::LessEqual),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

fn layout(ctx: &GpuContext, label: &str, binding: BindingDesc) -> wgpu::BindGroupLayout {
    ctx.device()
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &[binding.to_wgpu()],
        })
}

impl ScenePipelines {
    /// Build both pipelines from `shader` (entry points `mesh_vertex` /
    /// `mesh_fragment`), drawing into a colour attachment described by
    /// `target`.
    pub fn new(ctx: &GpuContext, shader: &CompiledShader, target: ColorTarget) -> Self {
        let device = ctx.device();
        let objects_layout = layout(
            ctx,
            "scene objects",
            BindingDesc::new(
                0,
                BindingKind::ReadOnlyStorageBuffer,
                ShaderVisibility::VERTEX_FRAGMENT,
            ),
        );
        let mesh_layout = layout(
            ctx,
            "scene mesh colors",
            BindingDesc::new(
                0,
                BindingKind::ReadOnlyStorageBuffer,
                ShaderVisibility::VERTEX,
            ),
        );
        let texture_layout = layout(
            ctx,
            "scene texture",
            BindingDesc::new(0, BindingKind::InputTexture, ShaderVisibility::FRAGMENT),
        );
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene mesh"),
            bind_group_layouts: &[
                Some(&objects_layout),
                Some(&mesh_layout),
                Some(&texture_layout),
            ],
            immediate_size: 0,
        });
        let attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 12,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 24,
                shader_location: 2,
            },
        ];
        let buffers = [wgpu::VertexBufferLayout {
            array_stride: size_of::<MeshVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &attributes,
        }];
        let targets = [Some(target.to_wgpu())];
        let build = |write_depth: bool, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader.module,
                    entry_point: Some("mesh_vertex"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &buffers,
                },
                // Two-sided: a mesh's winding is not part of its contract
                // yet (an extruded back cap is wound by its author), and the
                // depth test decides what is visible.
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(depth_state(write_depth)),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader.module,
                    entry_point: Some("mesh_fragment"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &targets,
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            opaque: build(true, "scene mesh opaque"),
            blended: build(false, "scene mesh blended"),
            objects_layout,
            mesh_layout,
            texture_layout,
        }
    }
}

/// The path / sprite draw data a scene pass shares between its
/// [`SceneCall::Quads`] calls.
///
/// The storage buffers are the three the rasterizer's shader reads (vertices,
/// items, vertex colours), bound at `@binding(1..=3)`; each call supplies its
/// own uniform at `@binding(0)`, which is how one batch carries a different
/// depth per object.
pub struct QuadBatch<'a> {
    /// A pipeline from [`RasterPipeline::new_depth_tested`].
    pub pipeline: &'a RasterPipeline,
    /// The three read-only storage buffers' bytes, non-empty each.
    pub storage: [&'a [u8]; 3],
}

/// One draw of a [`ScenePass`].
pub enum SceneCall<'a> {
    /// A mesh as one object.
    Mesh {
        /// The mesh's static buffers.
        buffers: &'a MeshBuffers,
        /// Index of this object's record in [`ScenePass::objects`].
        object: u32,
        /// The texture the fragment stage samples; `None` binds the pass's
        /// placeholder.
        texture: Option<&'a TextureBinding>,
        /// Whether the draw writes depth.
        class: DrawClass,
    },
    /// A run of the [`QuadBatch`]'s items.
    Quads {
        /// Item indices to draw.
        instances: Range<u32>,
        /// The uniform block at `@binding(0)` for this call.
        uniform: &'a [u8],
    },
}

/// One declaratively-described scene render pass.
pub struct ScenePass<'a> {
    /// Debug label.
    pub label: &'a str,
    /// The mesh pipelines.
    pub pipelines: &'a ScenePipelines,
    /// The colour attachment, cleared to transparent.
    pub color: &'a TextureBinding,
    /// The depth attachment, cleared to the far plane.
    pub depth: &'a DepthBuffer,
    /// Serialized object records, indexed by [`SceneCall::Mesh::object`].
    pub objects: &'a [u8],
    /// A texture to bind where a call samples nothing.
    pub placeholder: &'a TextureBinding,
    /// The shared path draw data, when any call is [`SceneCall::Quads`].
    pub quads: Option<QuadBatch<'a>>,
    /// The draws, in painter's order.
    pub calls: &'a [SceneCall<'a>],
}

fn buffer_with(
    device: &wgpu::Device,
    label: &str,
    bytes: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytes,
        usage,
    })
}

fn texture_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    texture: &TextureBinding,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("scene texture"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&texture.view),
        }],
    })
}

/// Record `pass` into `encoder`.
///
/// Nothing here is cached across passes: the object buffer is new bytes every
/// frame by construction, and the bind groups over it and over a pooled
/// texture are cheap next to the draws they feed. The mesh buffers are the
/// expensive part and they arrive already built.
pub(crate) fn record(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pass: &ScenePass<'_>,
) {
    let mut objects = pass.objects.to_vec();
    if objects.len() < MIN_OBJECT_BYTES {
        objects.resize(MIN_OBJECT_BYTES, 0);
    }
    let objects_buffer = buffer_with(device, pass.label, &objects, wgpu::BufferUsages::STORAGE);
    let objects_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(pass.label),
        layout: &pass.pipelines.objects_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: objects_buffer.as_entire_binding(),
        }],
    });

    let mut textures: HashMap<u64, wgpu::BindGroup> = HashMap::new();
    let mut texture_for = |texture: &TextureBinding| -> u64 {
        textures
            .entry(texture.id)
            .or_insert_with(|| texture_group(device, &pass.pipelines.texture_layout, texture));
        texture.id
    };
    // Resolve every call's texture group up front: the render pass borrows
    // the groups for its whole life, so none can be created inside it.
    let call_textures: Vec<Option<u64>> = pass
        .calls
        .iter()
        .map(|call| match call {
            SceneCall::Mesh { texture, .. } => {
                Some(texture_for(texture.unwrap_or(pass.placeholder)))
            }
            SceneCall::Quads { .. } => None,
        })
        .collect();

    // The path batch: shared storage, one bind group per call (the uniform
    // differs), the texture slot filled by the placeholder.
    let quad_storage: Vec<wgpu::Buffer> = pass
        .quads
        .as_ref()
        .map(|batch| {
            batch
                .storage
                .iter()
                .map(|bytes| buffer_with(device, pass.label, bytes, wgpu::BufferUsages::STORAGE))
                .collect()
        })
        .unwrap_or_default();
    let quad_groups: Vec<Option<wgpu::BindGroup>> = pass
        .calls
        .iter()
        .map(|call| match (call, &pass.quads) {
            (SceneCall::Quads { uniform, .. }, Some(batch)) => {
                let uniform = buffer_with(device, pass.label, uniform, wgpu::BufferUsages::UNIFORM);
                let mut entries = vec![wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                }];
                for (index, buffer) in quad_storage.iter().enumerate() {
                    entries.push(wgpu::BindGroupEntry {
                        binding: index as u32 + 1,
                        resource: buffer.as_entire_binding(),
                    });
                }
                entries.push(wgpu::BindGroupEntry {
                    binding: quad_storage.len() as u32 + 1,
                    resource: wgpu::BindingResource::TextureView(&pass.placeholder.view),
                });
                Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(pass.label),
                    layout: batch.pipeline.bind_group_layout(),
                    entries: &entries,
                }))
            }
            _ => None,
        })
        .collect();

    let color_attachments = [Some(wgpu::RenderPassColorAttachment {
        view: &pass.color.view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            store: wgpu::StoreOp::Store,
        },
    })];
    let mut render = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(pass.label),
        color_attachments: &color_attachments,
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &pass.depth.view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(1.0),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    for (index, call) in pass.calls.iter().enumerate() {
        match call {
            SceneCall::Mesh {
                buffers,
                object,
                class,
                ..
            } => {
                if buffers.index_count == 0 {
                    continue;
                }
                let texture = call_textures[index].expect("mesh calls resolved a texture");
                render.set_pipeline(match class {
                    DrawClass::Opaque => &pass.pipelines.opaque,
                    DrawClass::Blended => &pass.pipelines.blended,
                });
                render.set_bind_group(0, &objects_group, &[]);
                render.set_bind_group(1, &buffers.colors, &[]);
                render.set_bind_group(2, &textures[&texture], &[]);
                render.set_vertex_buffer(0, buffers.vertices.slice(..));
                render.set_index_buffer(buffers.indices.slice(..), wgpu::IndexFormat::Uint32);
                render.draw_indexed(0..buffers.index_count, 0, *object..*object + 1);
            }
            SceneCall::Quads { instances, .. } => {
                let (Some(batch), Some(group)) = (&pass.quads, &quad_groups[index]) else {
                    continue;
                };
                render.set_pipeline(batch.pipeline.pipeline());
                render.set_bind_group(0, group, &[]);
                render.draw(0..6, instances.clone());
            }
        }
    }
}

/// Every texture a pass reads or writes, for the batcher's pending-use
/// bookkeeping.
pub(crate) fn touched<'a>(pass: &'a ScenePass<'a>) -> impl Iterator<Item = &'a TextureBinding> {
    pass.calls
        .iter()
        .filter_map(|call| match call {
            SceneCall::Mesh { texture, .. } => *texture,
            SceneCall::Quads { .. } => None,
        })
        .chain(std::iter::once(pass.placeholder))
}
