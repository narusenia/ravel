// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Declarative compute dispatch with resource reuse and frame-level batching.
//!
//! Node processors describe one dispatch — N input textures, one output
//! storage texture, one uniform block — as a [`ComputeDispatch`] and hand it
//! to [`GpuContext::dispatch_compute`]. The context then does the three
//! things every processor used to repeat by hand:
//!
//! * **Uniform reuse.** The uniform bytes are content-hashed; an identical
//!   parameter block binds the same `wgpu::Buffer` instead of paying a
//!   `create_buffer_init` per dispatch.
//! * **Bind group reuse.** The bind group is cached by the identity of
//!   everything it references — the pipeline, the texture identities, and
//!   the uniform content — so re-evaluating a node with unchanged parameters
//!   over the same pooled textures creates nothing new.
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
//! [`BIND_GROUP_CACHE_CAPACITY`]). The bound is what keeps the caches honest
//! against the pool's budget accounting: a cached bind group holds texture
//! views and a view pins the underlying texture, so an unbounded cache would
//! keep VRAM alive that [`TexturePool`] believes it has evicted. The pin is
//! capped at the cache capacity, and the least-recently-used entry — usually
//! one whose textures have already cycled out of the working set — is
//! dropped first.
//!
//! [`GpuContext::dispatch_compute`]: crate::GpuContext::dispatch_compute
//! [`GpuContext::wait`]: crate::GpuContext::wait
//! [`GpuContext::flush`]: crate::GpuContext::flush
//! [`TexturePool::acquire`]: crate::TexturePool::acquire

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::compute::ComputePipeline;

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
/// through their views, so this cap is also the cap on VRAM the cache can
/// keep out of the pool's accounting.
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
    pub uniform: &'a [u8],
    /// Dispatch grid width in pixels.
    pub width: u32,
    /// Dispatch grid height in pixels.
    pub height: u32,
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
    /// Bind groups created (bind group cache misses).
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

/// Identity of a uniform block: a content hash plus the length, so blocks of
/// different sizes can never alias. The hash is deterministic within the
/// process (`DefaultHasher::new`), which is all an in-memory cache needs.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct UniformKey {
    hash: u64,
    len: u32,
}

fn uniform_key(bytes: &[u8]) -> UniformKey {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    UniformKey {
        hash: hasher.finish(),
        len: bytes.len() as u32,
    }
}

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
        let key = uniform_key(bytes);
        self.tick += 1;
        let tick = self.tick;
        if let Some((buffer, entry_tick)) = self.uniforms.get_mut(&key) {
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
        self.uniforms.insert(key, (buffer.clone(), tick));
        while self.uniforms.len() > UNIFORM_CACHE_CAPACITY {
            let Some((&oldest, _)) = self.uniforms.iter().min_by_key(|(_, (_, tick))| tick) else {
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
        uniform: UniformKey,
        buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let key = BindGroupKey {
            pipeline: Arc::as_ptr(dispatch.pipeline) as usize,
            inputs: dispatch.inputs.iter().map(|input| input.id).collect(),
            output: dispatch.output.id,
            uniform,
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
        entries.push(wgpu::BindGroupEntry {
            binding: dispatch.inputs.len() as u32 + 1,
            resource: buffer.as_entire_binding(),
        });
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
                    uniform: key.uniform,
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
        let uniform = uniform_key(dispatch.uniform);
        let buffer = self.uniform_buffer(device, dispatch.label, dispatch.uniform);
        let group = self.bind_group(device, dispatch, uniform, &buffer);
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
            wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
        )
    }

    struct Rig {
        ctx: GpuContext,
        pipeline: Arc<ComputePipeline>,
        pool: TexturePool,
    }

    fn rig(ctx: &GpuContext) -> Rig {
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
            pool: TexturePool::new(ctx.clone(), 64 * 1024 * 1024),
        }
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
}
