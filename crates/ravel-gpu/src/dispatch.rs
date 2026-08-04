// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Declarative GPU work with resource reuse and frame-level batching.
//!
//! Node processors describe one compute dispatch — N input textures, one
//! output storage texture, one uniform block — as a [`ComputeDispatch`] and
//! hand it to [`GpuContext::dispatch_compute`]. The one drawing path describes
//! its render pass as a [`QuadDraw`] and hands it to
//! [`GpuContext::draw_quads`]. Both record into the same batch. The context
//! then does the three things every processor used to repeat by hand:
//!
//! * **Uniform reuse.** The uniform bytes key the cache directly; an
//!   identical parameter block binds the same `wgpu::Buffer` instead of
//!   paying a `create_buffer_init` per dispatch.
//! * **Bind group reuse.** The bind group is cached by the identity of
//!   everything it references — the pipeline, the texture identities, and
//!   the uniform content — so re-evaluating a node with unchanged parameters
//!   over the same pooled textures creates nothing new. A [`QuadDraw`] is the
//!   exception and is stated as one in [`QuadDraw::storage`]: its buffers hold
//!   the frame's flattened geometry, which is new bytes every draw, so neither
//!   they nor the bind group referencing them can be reused.
//! * **Frame batching.** The dispatch is recorded into one command encoder
//!   shared by the whole frame instead of one encoder plus `queue.submit`
//!   per node. Batched work is submitted only at well-defined flush points:
//!   before a readback of a texture the batch writes, before an upload into
//!   a texture the batch uses, on [`GpuContext::wait`], on an explicit
//!   [`GpuContext::flush`], or once the batch grows past
//!   [`MAX_PENDING_DISPATCHES`]. In the application the viewer readback
//!   ([`crate::GpuFrameBuffer::to_frame_buffer`]) is the per-frame flush, so
//!   a frame's dispatches submit exactly once.
//!
//! Deferred submission changes one contract of the texture pool: a texture
//! released while the batch still references it must not be handed out again
//! until the batch is flushed — its queued reads and writes have to see the
//! contents the recording expected. [`TexturePool::acquire`] therefore skips
//! textures the pending batch still uses (see [`DispatchState::is_pending_use`]).
//!
//! Both caches are bounded LRU maps ([`UNIFORM_CACHE_CAPACITY`],
//! [`BIND_GROUP_CACHE_CAPACITY`]). A cached bind group holds texture views
//! and a view pins the underlying texture, so the caches must not outlive
//! the pool's accounting: when [`TexturePool`](crate::TexturePool) evicts a
//! texture it invalidates every bind group referencing it
//! ([`DispatchState::evict_textures`]), and the bytes it reports as freed
//! really are freed. The LRU capacity is the secondary bound, limiting how
//! many entries can pile up between pool evictions.
//!
//! [`GpuContext::dispatch_compute`]: crate::GpuContext::dispatch_compute
//! [`GpuContext::draw_quads`]: crate::GpuContext::draw_quads
//! [`GpuContext::wait`]: crate::GpuContext::wait
//! [`GpuContext::flush`]: crate::GpuContext::flush
//! [`TexturePool::acquire`]: crate::TexturePool::acquire

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::compute::ComputePipeline;
use crate::raster::RasterPipeline;

/// Dispatches the shared encoder holds before it flushes itself.
///
/// Evaluation is expected to flush at the frame's readback long before this;
/// the cap exists for hosts with no per-frame flush (and as a bound on how
/// many pool textures the pending batch can keep out of circulation).
const MAX_PENDING_DISPATCHES: u32 = 64;

/// Uniform buffers kept for content-addressed reuse. Uniforms are a few tens
/// of bytes each, so a generous cap costs little and covers scrubbing.
const UNIFORM_CACHE_CAPACITY: usize = 256;

/// Bind groups kept for identity-based reuse. Each entry pins its textures
/// through their views; the pool invalidates entries when it evicts their
/// textures, so this is only the secondary bound on how many entries pile
/// up between pool evictions.
const BIND_GROUP_CACHE_CAPACITY: usize = 64;

/// A texture plus its cached default view, ready to be bound to a dispatch.
///
/// Obtained from [`PooledTexture::binding`](crate::PooledTexture::binding) or
/// [`GpuFrameBuffer::binding`](crate::GpuFrameBuffer::binding). The fields are
/// crate-internal: callers only pass the binding through to
/// [`ComputeDispatch`], and the dispatch layer is the only code that looks
/// inside — that is what keeps the cache identities below well-defined.
#[derive(Clone)]
pub struct TextureBinding {
    /// Identity of the pooled texture (unique, never reused). Cached bind
    /// groups key on this rather than on a pointer, so an entry can never be
    /// handed out for a texture that was freed and re-created.
    pub(crate) id: u64,
    pub(crate) texture: Arc<wgpu::Texture>,
    pub(crate) view: wgpu::TextureView,
}

/// One declaratively-described compute dispatch.
///
/// Binding order is the contract: `inputs[0..N]` bind at `@binding(0..N)`,
/// `output` binds at `@binding(N)`, and the uniform block binds at
/// `@binding(N + 1)` — the layout every built-in compute node declares.
pub struct ComputeDispatch<'a> {
    /// Debug label for the bind group and pass.
    pub label: &'a str,
    /// The pipeline to dispatch; shared pipelines come from
    /// [`ShaderManager::compute_pipeline`](crate::ShaderManager::compute_pipeline).
    pub pipeline: &'a Arc<ComputePipeline>,
    /// Input (sampled) textures, in binding order.
    pub inputs: &'a [TextureBinding],
    /// The storage texture the pass writes.
    pub output: &'a TextureBinding,
    /// Serialized uniform block (e.g. `bytemuck::bytes_of(&params)`).
    ///
    /// Empty means the shader takes no parameters: nothing binds at
    /// `@binding(N + 1)` and the layout must not declare the slot. Every
    /// built-in filter has parameters; the rasterizer's unpremultiply pass
    /// reads its extent from the output texture and is the one pass that does
    /// not.
    pub uniform: &'a [u8],
    /// Dispatch grid width in pixels.
    pub width: u32,
    /// Dispatch grid height in pixels.
    pub height: u32,
}

/// One declaratively-described instanced quad draw.
///
/// The graphics counterpart of [`ComputeDispatch`], and binding order is the
/// contract in the same way: the uniform block binds at `@binding(0)` and
/// `storage[0..N]` bind at `@binding(1..N + 1)`. The pass clears `target` to
/// transparent and then draws `instance_count` quads of six vertices each,
/// with no vertex buffers — the shader expands each quad from the vertex and
/// instance indices.
pub struct QuadDraw<'a> {
    /// Debug label for the buffers, the bind group, and the pass.
    pub label: &'a str,
    /// The pipeline to draw with.
    pub pipeline: &'a RasterPipeline,
    /// Serialized uniform block bound at `@binding(0)`. Non-empty: the one
    /// render pipeline always needs its target resolution, so — unlike
    /// [`ComputeDispatch::uniform`] — there is no parameterless case.
    pub uniform: &'a [u8],
    /// Read-only storage buffers bound at `@binding(1..N + 1)`, in order.
    ///
    /// The bytes are uploaded into fresh buffers on every draw: they carry the
    /// frame's flattened geometry, which differs frame to frame, so there is
    /// nothing a cache could hand back. Each slice must be non-empty — a
    /// zero-sized buffer is not a valid binding, and a shader reading an empty
    /// array needs a one-element placeholder from the caller that knows the
    /// element type.
    pub storage: &'a [&'a [u8]],
    /// The colour attachment. Its format must match the
    /// [`ColorTarget`](crate::ColorTarget) the pipeline was built with.
    pub target: &'a TextureBinding,
    /// Quads to draw. Zero is legal and records a pass that only clears.
    pub instance_count: u32,
}

/// Point-in-time view of the dispatch batching counters.
///
/// Recorded per [`GpuContext`](crate::GpuContext), following the
/// `transfer_stats` idiom, so
/// tests can assert how much GPU-object creation a sequence of evaluations
/// actually performed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DispatchSnapshot {
    /// Batched command buffers actually submitted to the queue.
    pub submits: u64,
    /// Uniform buffers created (uniform cache misses).
    pub uniform_buffers_created: u64,
    /// Bind groups created: cache misses, plus one per [`QuadDraw`], whose
    /// group references buffers rebuilt for that draw and is never cached.
    pub bind_groups_created: u64,
}

impl DispatchSnapshot {
    /// Counter increments between `self` (earlier) and `later`.
    pub fn delta(&self, later: &DispatchSnapshot) -> DispatchSnapshot {
        DispatchSnapshot {
            submits: later.submits.wrapping_sub(self.submits),
            uniform_buffers_created: later
                .uniform_buffers_created
                .wrapping_sub(self.uniform_buffers_created),
            bind_groups_created: later
                .bind_groups_created
                .wrapping_sub(self.bind_groups_created),
        }
    }
}

/// Identity of a uniform block: the bytes themselves. Blocks are a few tens
/// of bytes, so keying on the content directly — rather than on a hash that
/// could in principle collide and bind the wrong parameters — costs nothing.
type UniformKey = Box<[u8]>;

/// Identity of a bind group: everything the group references.
///
/// Textures key by their pooled identity, which is never reused, so a stale
/// entry cannot alias a re-created texture. The pipeline keys by the address
/// of the shared [`ComputePipeline`]; the entry holds an `Arc` clone so that
/// address stays valid for as long as the entry can be handed out.
#[derive(PartialEq, Eq, Hash)]
struct BindGroupKey {
    pipeline: usize,
    inputs: Vec<u64>,
    output: u64,
    uniform: UniformKey,
}

struct BindGroupEntry {
    group: wgpu::BindGroup,
    tick: u64,
    /// Keeps the pipeline allocation — and with it the key's pointer
    /// identity — alive for the entry's lifetime.
    #[allow(dead_code)]
    pipeline: Arc<ComputePipeline>,
}

/// Raw texture identity used for pending-use tracking.
///
/// `Arc<wgpu::Texture>` derefs to the same address as the `&wgpu::Texture`
/// the transfer helpers receive, so the pool, the batcher, and
/// `read_texture` / `upload_texture` all agree on the identity. A stale
/// pointer here can only cause a spurious flush (safe direction): pending
/// work keeps its textures alive through the recorded bind groups, so an
/// address in these sets belongs to a live texture until the flush.
fn texture_ptr(texture: &wgpu::Texture) -> usize {
    texture as *const wgpu::Texture as usize
}

/// Per-[`GpuContext`](crate::GpuContext) batching state: the frame's shared
/// command encoder,
/// the textures it still uses, and the uniform / bind group caches.
///
/// Everything lives behind one mutex (held by `GpuContext`); evaluation
/// records from a single worker thread, so the lock is never contended in
/// practice and is only there to make the shared context `Sync`.
#[derive(Default)]
pub(crate) struct DispatchState {
    encoder: Option<wgpu::CommandEncoder>,
    pending_dispatches: u32,
    /// Textures the pending batch reads or writes (by raw pointer).
    used: HashSet<usize>,
    /// Textures the pending batch writes (subset of `used`).
    written: HashSet<usize>,
    uniforms: HashMap<UniformKey, (wgpu::Buffer, u64)>,
    bind_groups: HashMap<BindGroupKey, BindGroupEntry>,
    tick: u64,
    submits: u64,
    uniform_buffers_created: u64,
    bind_groups_created: u64,
}

impl DispatchState {
    /// The buffer for `bytes`, creating it only on a content miss.
    fn uniform_buffer(&mut self, device: &wgpu::Device, label: &str, bytes: &[u8]) -> wgpu::Buffer {
        self.tick += 1;
        let tick = self.tick;
        if let Some((buffer, entry_tick)) = self.uniforms.get_mut(bytes) {
            *entry_tick = tick;
            return buffer.clone();
        }
        use wgpu::util::DeviceExt as _;
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        self.uniform_buffers_created += 1;
        self.uniforms.insert(bytes.into(), (buffer.clone(), tick));
        while self.uniforms.len() > UNIFORM_CACHE_CAPACITY {
            let Some(oldest) = self
                .uniforms
                .iter()
                .min_by_key(|(_, (_, tick))| tick)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.uniforms.remove(&oldest);
        }
        buffer
    }

    /// The bind group for this exact (pipeline, textures, uniform) tuple.
    fn bind_group(
        &mut self,
        device: &wgpu::Device,
        dispatch: &ComputeDispatch<'_>,
        buffer: Option<&wgpu::Buffer>,
    ) -> wgpu::BindGroup {
        let key = BindGroupKey {
            pipeline: Arc::as_ptr(dispatch.pipeline) as usize,
            inputs: dispatch.inputs.iter().map(|input| input.id).collect(),
            output: dispatch.output.id,
            uniform: dispatch.uniform.into(),
        };
        self.tick += 1;
        let tick = self.tick;
        if let Some(entry) = self.bind_groups.get_mut(&key) {
            entry.tick = tick;
            return entry.group.clone();
        }

        let mut entries = Vec::with_capacity(dispatch.inputs.len() + 2);
        for (index, input) in dispatch.inputs.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: index as u32,
                resource: wgpu::BindingResource::TextureView(&input.view),
            });
        }
        entries.push(wgpu::BindGroupEntry {
            binding: dispatch.inputs.len() as u32,
            resource: wgpu::BindingResource::TextureView(&dispatch.output.view),
        });
        // A parameterless pass declares no slot at `N + 1`; binding one anyway
        // would not match the layout.
        if let Some(buffer) = buffer {
            entries.push(wgpu::BindGroupEntry {
                binding: dispatch.inputs.len() as u32 + 1,
                resource: buffer.as_entire_binding(),
            });
        }
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(dispatch.label),
            layout: dispatch.pipeline.bind_group_layout(),
            entries: &entries,
        });

        self.bind_groups_created += 1;
        self.bind_groups.insert(
            key,
            BindGroupEntry {
                group: group.clone(),
                tick,
                pipeline: dispatch.pipeline.clone(),
            },
        );
        while self.bind_groups.len() > BIND_GROUP_CACHE_CAPACITY {
            let Some(oldest) = self
                .bind_groups
                .iter()
                .min_by_key(|(_, entry)| entry.tick)
                .map(|(key, _)| BindGroupKey {
                    pipeline: key.pipeline,
                    inputs: key.inputs.clone(),
                    output: key.output,
                    uniform: key.uniform.clone(),
                })
            else {
                break;
            };
            self.bind_groups.remove(&oldest);
        }
        group
    }

    /// Record `dispatch` into the shared encoder, flushing first if the
    /// batch is full.
    pub(crate) fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dispatch: &ComputeDispatch<'_>,
    ) {
        if self.pending_dispatches >= MAX_PENDING_DISPATCHES {
            self.flush(queue);
        }
        let buffer = (!dispatch.uniform.is_empty())
            .then(|| self.uniform_buffer(device, dispatch.label, dispatch.uniform));
        let group = self.bind_group(device, dispatch, buffer.as_ref());
        let encoder = self.encoder.get_or_insert_with(|| {
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ravel dispatch batch"),
            })
        });
        dispatch
            .pipeline
            .dispatch(encoder, &group, dispatch.width, dispatch.height);
        for input in dispatch.inputs {
            self.used.insert(texture_ptr(&input.texture));
        }
        self.used.insert(texture_ptr(&dispatch.output.texture));
        self.written.insert(texture_ptr(&dispatch.output.texture));
        self.pending_dispatches += 1;
    }

    /// Record `draw` into the shared encoder, flushing first if the batch is
    /// full.
    ///
    /// The attachment joins the batch's used *and* written sets, so a caller
    /// that returns it to the pool immediately after recording — which the
    /// rasterizer does with its premultiplied intermediate — cannot have it
    /// handed to a new owner before the recorded pass has run.
    pub(crate) fn record_draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        draw: &QuadDraw<'_>,
    ) {
        if self.pending_dispatches >= MAX_PENDING_DISPATCHES {
            self.flush(queue);
        }
        let (uniform, storage) = {
            // The span name is read by `ravel-nodes/examples/perf_baseline.rs`
            // and recorded in `docs/implementation/perf-baseline.md`: it
            // separates the cost of writing the draw data from the CPU
            // flatten that produced it.
            let upload = tracing::debug_span!(
                "raster_upload",
                bytes = draw.storage.iter().map(|bytes| bytes.len()).sum::<usize>()
            );
            let _guard = upload.enter();
            use wgpu::util::DeviceExt as _;
            let uniform = self.uniform_buffer(device, draw.label, draw.uniform);
            let storage: Vec<wgpu::Buffer> = draw
                .storage
                .iter()
                .map(|bytes| {
                    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some(draw.label),
                        contents: bytes,
                        usage: wgpu::BufferUsages::STORAGE,
                    })
                })
                .collect();
            (uniform, storage)
        };

        let mut entries = Vec::with_capacity(storage.len() + 1);
        entries.push(wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform.as_entire_binding(),
        });
        for (index, buffer) in storage.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: index as u32 + 1,
                resource: buffer.as_entire_binding(),
            });
        }
        // Not cached: the storage buffers above are new, so an entry keyed by
        // what this group references could never be hit again.
        //
        // That makes this the one recording path whose bind group and buffers
        // are dropped before the batch is submitted — the compute path holds
        // both alive in `self.bind_groups` / the uniform cache. It is safe
        // because the command buffer keeps its own reference to every resource
        // a recorded pass touches, so the Rust handles going out of scope here
        // does not free anything the pending encoder still needs. Do not
        // "fix" this by extending the caches: an entry keyed by these buffers
        // would never be reused and would pin their VRAM until eviction.
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(draw.label),
            layout: draw.pipeline.bind_group_layout(),
            entries: &entries,
        });
        self.bind_groups_created += 1;

        let encoder = self.encoder.get_or_insert_with(|| {
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ravel dispatch batch"),
            })
        });
        draw.pipeline
            .draw_quads(encoder, &group, &draw.target.view, draw.instance_count);
        self.used.insert(texture_ptr(&draw.target.texture));
        self.written.insert(texture_ptr(&draw.target.texture));
        self.pending_dispatches += 1;
    }

    /// Submit the pending batch, if any. Afterwards every pooled texture is
    /// safe to reuse again.
    pub(crate) fn flush(&mut self, queue: &wgpu::Queue) {
        if let Some(encoder) = self.encoder.take() {
            queue.submit(Some(encoder.finish()));
            self.submits += 1;
        }
        self.pending_dispatches = 0;
        self.used.clear();
        self.written.clear();
    }

    /// Flush when `texture` is about to be overwritten by `write_texture`
    /// while the pending batch still reads or writes it: the batched commands
    /// execute *after* the write, so without the flush the stale batch would
    /// clobber the fresh upload.
    pub(crate) fn flush_for_upload(&mut self, queue: &wgpu::Queue, texture: &wgpu::Texture) {
        if self.used.contains(&texture_ptr(texture)) {
            self.flush(queue);
        }
    }

    /// Flush when `texture` is about to be read back while the pending batch
    /// still writes it, so the copy sees the batch's output.
    pub(crate) fn flush_for_readback(&mut self, queue: &wgpu::Queue, texture: &wgpu::Texture) {
        if self.written.contains(&texture_ptr(texture)) {
            self.flush(queue);
        }
    }

    /// Whether the pending batch still reads or writes `texture`. The pool
    /// refuses to hand such a texture to a new owner until the flush.
    pub(crate) fn is_pending_use(&self, texture: &wgpu::Texture) -> bool {
        self.used.contains(&texture_ptr(texture))
    }

    /// Drop every cached bind group referencing one of `textures` (pooled
    /// texture ids). The pool calls this when it evicts them, so no entry
    /// outlives the texture it pins. Dropping an entry that the pending
    /// batch already recorded is safe: the recorded command buffer keeps
    /// its own references — the cache entry is only a reuse shortcut.
    pub(crate) fn evict_textures(&mut self, textures: &[u64]) {
        self.bind_groups.retain(|key, _| {
            !textures.contains(&key.output) && !key.inputs.iter().any(|id| textures.contains(id))
        });
    }

    /// Number of cached bind groups (test observation point for the
    /// pool-driven invalidation above).
    #[cfg(test)]
    pub(crate) fn cached_bind_group_count(&self) -> usize {
        self.bind_groups.len()
    }

    pub(crate) fn snapshot(&self) -> DispatchSnapshot {
        DispatchSnapshot {
            submits: self.submits,
            uniform_buffers_created: self.uniform_buffers_created,
            bind_groups_created: self.bind_groups_created,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::texture_desc::{TextureFormat, TextureUsage};
    use crate::texture_pool::{TextureKey, TexturePool};
    use crate::{
        BindingDesc, BindingKind, GpuContext, ShaderManager, ShaderVisibility, upload_texture,
    };

    fn try_context() -> Option<GpuContext> {
        GpuContext::new_blocking().ok()
    }

    /// A minimal shader with the declarative contract: one input, one output,
    /// one uniform block.
    const SCALE_SRC: &str = r#"
struct Params {
    scale: f32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
};
@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba32float, write>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let c = textureLoad(input_tex, coord, 0);
    textureStore(output_tex, coord, vec4<f32>(c.rgb * params.scale, c.a));
}
"#;

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct ScaleParams {
        scale: f32,
        _pad: [f32; 3],
    }

    fn rw_key(width: u32, height: u32) -> TextureKey {
        TextureKey::new(
            width,
            height,
            TextureFormat::Rgba32Float,
            TextureUsage::TEXTURE_BINDING
                | TextureUsage::STORAGE_BINDING
                | TextureUsage::COPY_SRC
                | TextureUsage::COPY_DST,
        )
    }

    struct Rig {
        ctx: GpuContext,
        pipeline: Arc<ComputePipeline>,
        pool: TexturePool,
    }

    fn rig_on(ctx: &GpuContext, pool: TexturePool) -> Rig {
        let mut shaders = ShaderManager::new(ctx.clone());
        let layout = [
            BindingDesc::new(0, BindingKind::InputTexture, ShaderVisibility::COMPUTE),
            BindingDesc::new(
                1,
                BindingKind::OutputStorageTexture,
                ShaderVisibility::COMPUTE,
            ),
            BindingDesc::new(2, BindingKind::UniformBuffer, ShaderVisibility::COMPUTE),
        ];
        let pipeline = shaders
            .compute_pipeline("scale_test", SCALE_SRC, "main", &layout, [8, 8])
            .expect("scale shader compiles");
        Rig {
            ctx: ctx.clone(),
            pipeline,
            pool,
        }
    }

    fn rig(ctx: &GpuContext) -> Rig {
        rig_on(ctx, TexturePool::new(ctx.clone(), 64 * 1024 * 1024))
    }

    impl Rig {
        fn dispatch_scale(
            &self,
            input: &TextureBinding,
            output: &TextureBinding,
            scale: f32,
            width: u32,
            height: u32,
        ) {
            let params = ScaleParams {
                scale,
                _pad: [0.0; 3],
            };
            self.ctx.dispatch_compute(&ComputeDispatch {
                label: "scale_test",
                pipeline: &self.pipeline,
                inputs: std::slice::from_ref(input),
                output,
                uniform: bytemuck::bytes_of(&params),
                width,
                height,
            });
        }
    }

    fn read_texture_pixels(
        ctx: &GpuContext,
        texture: &crate::PooledTexture,
        width: u32,
        height: u32,
    ) -> Vec<f32> {
        let raw = crate::read_texture(ctx, &texture.texture, texture.key).expect("readback");
        let floats: &[f32] = bytemuck::cast_slice(&raw);
        assert_eq!(floats.len(), (width * height * 4) as usize);
        floats.to_vec()
    }

    /// The completion contract: two identical dispatches create one bind
    /// group and one uniform buffer total, and batch into a single submit.
    #[test]
    fn identical_dispatches_reuse_resources_and_submit_once() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut rig = rig(&ctx);
        let key = rw_key(8, 8);
        let input = rig.pool.acquire(key);
        upload_texture(
            &ctx,
            &input.texture,
            key,
            bytemuck::cast_slice(&[0.5f32; 8 * 8 * 4]),
        );
        let input_binding = input.binding();

        let output = rig.pool.acquire(key);
        let output_binding = output.binding();
        let before = ctx.dispatch_stats();
        rig.dispatch_scale(&input_binding, &output_binding, 2.0, 8, 8);
        rig.dispatch_scale(&input_binding, &output_binding, 2.0, 8, 8);
        rig.ctx.flush();
        let stats = before.delta(&ctx.dispatch_stats());
        assert_eq!(
            stats.bind_groups_created, 1,
            "identical dispatches share one bind group"
        );
        assert_eq!(
            stats.uniform_buffers_created, 1,
            "identical uniforms share one buffer"
        );
        assert_eq!(stats.submits, 1, "one frame of dispatches submits once");

        // The result must still be correct: the batch really ran.
        let pixels = read_texture_pixels(&ctx, &output, 8, 8);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert!(
                px[..3].iter().all(|&v| (v - 1.0).abs() < 1e-6),
                "pixel {i}: 0.5 scaled by 2.0 must read back as 1.0, got {px:?}"
            );
            assert!((px[3] - 0.5).abs() < 1e-6, "pixel {i}: alpha is preserved");
        }
    }

    /// Distinct uniform contents must not alias: each gets its own buffer and
    /// the readback sees the right value for each dispatch.
    #[test]
    fn different_uniform_contents_do_not_alias() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut rig = rig(&ctx);
        let key = rw_key(4, 4);
        let input = rig.pool.acquire(key);
        upload_texture(
            &ctx,
            &input.texture,
            key,
            bytemuck::cast_slice(&[0.5f32; 4 * 4 * 4]),
        );
        let input_binding = input.binding();

        let out_a = rig.pool.acquire(key);
        let out_b = rig.pool.acquire(key);
        let binding_a = out_a.binding();
        let binding_b = out_b.binding();
        let before = ctx.dispatch_stats();
        rig.dispatch_scale(&input_binding, &binding_a, 1.0, 4, 4);
        rig.dispatch_scale(&input_binding, &binding_b, 4.0, 4, 4);
        let stats = before.delta(&ctx.dispatch_stats());
        assert_eq!(stats.uniform_buffers_created, 2);
        assert_eq!(stats.bind_groups_created, 2);

        let a = read_texture_pixels(&ctx, &out_a, 4, 4);
        let b = read_texture_pixels(&ctx, &out_b, 4, 4);
        assert!(
            a.chunks_exact(4)
                .all(|px| px[..3].iter().all(|&v| (v - 0.5).abs() < 1e-6)),
            "scale 1.0 keeps 0.5"
        );
        assert!(
            b.chunks_exact(4)
                .all(|px| px[..3].iter().all(|&v| (v - 2.0).abs() < 1e-6)),
            "scale 4.0 doubles to 2.0"
        );
    }

    /// A readback of a texture the batch writes flushes the batch first —
    /// the caller never submits explicitly.
    #[test]
    fn readback_flushes_pending_writes() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut rig = rig(&ctx);
        let key = rw_key(4, 4);
        let input = rig.pool.acquire(key);
        upload_texture(
            &ctx,
            &input.texture,
            key,
            bytemuck::cast_slice(&[1.0f32; 4 * 4 * 4]),
        );
        let output = rig.pool.acquire(key);
        let input_binding = input.binding();
        let output_binding = output.binding();

        let before = ctx.dispatch_stats();
        rig.dispatch_scale(&input_binding, &output_binding, 3.0, 4, 4);
        // No explicit flush: the readback must see the dispatch's output.
        let pixels = read_texture_pixels(&ctx, &output, 4, 4);
        assert!(
            pixels
                .chunks_exact(4)
                .all(|px| px[..3].iter().all(|&v| (v - 3.0).abs() < 1e-6)),
            "1.0 scaled by 3.0 must read back as 3.0"
        );
        assert_eq!(
            before.delta(&ctx.dispatch_stats()).submits,
            1,
            "the readback flushed exactly the one pending batch"
        );
    }

    /// A readback of an *unrelated* texture must not drag the pending batch
    /// with it.
    ///
    /// This is the observable half of "the readback no longer waits for the
    /// whole device" (`HIGH-04`): the readback used to end in
    /// `GpuContext::wait`, which submits the batch before waiting, so reading
    /// one texture cost a submit and a full pipeline sync for dispatches the
    /// caller never asked about. With the wait narrowed to the copy's own
    /// submission the batch stays pending.
    #[test]
    fn a_readback_does_not_submit_unrelated_batched_work() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut rig = rig(&ctx);
        let key = rw_key(4, 4);
        let input = rig.pool.acquire(key);
        upload_texture(
            &ctx,
            &input.texture,
            key,
            bytemuck::cast_slice(&[1.0f32; 4 * 4 * 4]),
        );
        let batched_output = rig.pool.acquire(key);
        let input_binding = input.binding();
        let batched_binding = batched_output.binding();
        rig.dispatch_scale(&input_binding, &batched_binding, 2.0, 4, 4);

        // A texture the pending batch neither reads nor writes.
        let unrelated = rig.pool.acquire(key);
        upload_texture(
            &ctx,
            &unrelated.texture,
            key,
            bytemuck::cast_slice(&[0.5f32; 4 * 4 * 4]),
        );
        let before = ctx.dispatch_stats();
        let pixels = read_texture_pixels(&ctx, &unrelated, 4, 4);
        assert!(
            pixels
                .chunks_exact(4)
                .all(|px| px[..3].iter().all(|&v| (v - 0.5).abs() < 1e-6)),
            "the unrelated texture read back its own contents"
        );
        assert_eq!(
            before.delta(&ctx.dispatch_stats()).submits,
            0,
            "reading one texture must not submit the batch that writes another"
        );

        // The batch is still pending and still correct once it is flushed.
        rig.ctx.flush();
        let batched = read_texture_pixels(&ctx, &batched_output, 4, 4);
        assert!(
            batched
                .chunks_exact(4)
                .all(|px| px[..3].iter().all(|&v| (v - 2.0).abs() < 1e-6)),
            "the deferred dispatch still produced its result"
        );
    }

    /// A texture the pending batch still uses must not be handed to a new
    /// owner; after the flush the pool reuses it again.
    #[test]
    fn pool_skips_textures_the_pending_batch_uses() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut rig = rig(&ctx);
        let key = rw_key(4, 4);
        let input = rig.pool.acquire(key);
        upload_texture(
            &ctx,
            &input.texture,
            key,
            bytemuck::cast_slice(&[1.0f32; 4 * 4 * 4]),
        );
        let output = rig.pool.acquire(key);
        let created_before = rig.pool.total_created();
        let input_binding = input.binding();
        let output_binding = output.binding();
        rig.dispatch_scale(&input_binding, &output_binding, 2.0, 4, 4);

        // Released while still pending: the pool must not hand it back out.
        rig.pool.release(output);
        let other = rig.pool.acquire(key);
        assert_eq!(
            rig.pool.total_created(),
            created_before + 1,
            "a pending texture is skipped, forcing a fresh allocation"
        );
        rig.pool.release(other);

        // After the flush the skipped texture circulates again.
        rig.ctx.flush();
        let reused = rig.pool.acquire(key);
        assert_eq!(
            rig.pool.total_created(),
            created_before + 1,
            "post-flush acquires reuse pooled textures"
        );
        rig.pool.release(reused);
    }

    /// Pool eviction invalidates the bind groups pinning the evicted texture:
    /// no cache entry may outlive a texture whose VRAM the pool's accounting
    /// just counted as freed.
    #[test]
    fn pool_eviction_invalidates_cached_bind_groups() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        // A 1-byte idle budget evicts every texture the moment it comes back.
        let mut rig = rig_on(&ctx, TexturePool::new(ctx.clone(), 1));
        let key = rw_key(4, 4);
        let input = rig.pool.acquire(key);
        upload_texture(
            &ctx,
            &input.texture,
            key,
            bytemuck::cast_slice(&[1.0f32; 4 * 4 * 4]),
        );
        let output = rig.pool.acquire(key);
        let input_binding = input.binding();
        let output_binding = output.binding();
        rig.dispatch_scale(&input_binding, &output_binding, 2.0, 4, 4);
        assert_eq!(ctx.cached_bind_group_count(), 1);

        // Evict the output texture; the entry referencing it must go with it.
        rig.ctx.flush();
        rig.pool.release(output);
        assert_eq!(rig.pool.idle_count(), 0, "the tiny budget evicts at once");
        assert_eq!(
            ctx.cached_bind_group_count(),
            0,
            "no bind group may keep pinning an evicted texture"
        );

        // Re-dispatching still works: it rebuilds the entry for the fresh
        // texture (and only that — the uniform is still cached).
        let before = ctx.dispatch_stats();
        let output = rig.pool.acquire(key);
        let output_binding = output.binding();
        rig.dispatch_scale(&input_binding, &output_binding, 2.0, 4, 4);
        let stats = before.delta(&ctx.dispatch_stats());
        assert_eq!(stats.bind_groups_created, 1, "entry rebuilt after eviction");
        assert_eq!(
            stats.uniform_buffers_created, 0,
            "uniforms do not pin textures"
        );
        let pixels = read_texture_pixels(&ctx, &output, 4, 4);
        assert!(
            pixels
                .chunks_exact(4)
                .all(|px| px[..3].iter().all(|&v| (v - 2.0).abs() < 1e-6)),
            "1.0 scaled by 2.0 must read back as 2.0"
        );
    }
}
