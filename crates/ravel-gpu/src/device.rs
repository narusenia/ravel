// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! wgpu device / queue initialization and the shared [`GpuContext`].
//!
//! Ravel keeps a single [`GpuContext`] that owns the wgpu [`Device`] and
//! [`Queue`]. The same context is shared between UI rendering (GPUI) and the
//! compute pipeline so textures never need to round-trip across GPU contexts.
//!
//! On macOS the Metal backend is selected automatically; on Windows D3D12 /
//! D3D11 are preferred. Backends can be overridden through the standard
//! `WGPU_BACKEND` environment variable.

use std::sync::Arc;

use crate::error::{GpuError, GpuResult};

/// Shared handle to the GPU device, queue, and adapter.
///
/// Cloning is cheap: the inner wgpu handles are reference counted, and the
/// context is wrapped in an [`Arc`] for sharing across threads (rayon eval
/// workers, the dedicated GPU thread, etc.).
#[derive(Clone)]
pub struct GpuContext {
    inner: Arc<GpuContextInner>,
}

struct GpuContextInner {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    transfers: crate::transfer::stats::TransferCounters,
    dispatch: std::sync::Mutex<crate::dispatch::DispatchState>,
    staging: std::sync::Mutex<crate::staging::StagingPool>,
}

impl GpuContext {
    /// Initialize a GPU context using the platform's preferred backend.
    ///
    /// Returns [`GpuError::NoAdapter`] when no adapter is available (e.g. a
    /// headless CI runner without a GPU), allowing callers to degrade
    /// gracefully or skip GPU work.
    pub async fn new() -> GpuResult<Self> {
        let backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY);
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        let instance = wgpu::Instance::new(desc);

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|_| GpuError::NoAdapter)?;

        let info = adapter.get_info();
        log::info!(
            "selected GPU adapter: {} ({:?}, backend {:?})",
            info.name,
            info.device_type,
            info.backend
        );

        let adapter_limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("ravel-gpu device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits {
                    max_texture_dimension_2d: adapter_limits.max_texture_dimension_2d,
                    max_buffer_size: adapter_limits.max_buffer_size,
                    max_storage_buffer_binding_size: adapter_limits.max_storage_buffer_binding_size,
                    ..wgpu::Limits::default()
                },
                ..Default::default()
            })
            .await
            .map_err(|e| GpuError::DeviceRequest(e.to_string()))?;

        Ok(Self {
            inner: Arc::new(GpuContextInner {
                instance,
                adapter,
                device,
                queue,
                info,
                transfers: Default::default(),
                dispatch: Default::default(),
                staging: Default::default(),
            }),
        })
    }

    /// Blocking convenience wrapper around [`GpuContext::new`].
    ///
    /// Useful from synchronous startup paths; the eval engine never runs on
    /// the tokio runtime, so we block with `pollster` rather than depending on
    /// an async executor.
    pub fn new_blocking() -> GpuResult<Self> {
        pollster::block_on(Self::new())
    }

    /// Build a context from wgpu handles owned elsewhere (e.g. GPUI's wgpu
    /// instance), enabling a shared GPU context between UI and compute.
    pub fn from_handles(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Self {
        let info = adapter.get_info();
        Self {
            inner: Arc::new(GpuContextInner {
                instance,
                adapter,
                device,
                queue,
                info,
                transfers: Default::default(),
                dispatch: Default::default(),
                staging: Default::default(),
            }),
        }
    }

    /// The logical wgpu device.
    #[inline]
    pub fn device(&self) -> &wgpu::Device {
        &self.inner.device
    }

    /// The command queue.
    #[inline]
    pub fn queue(&self) -> &wgpu::Queue {
        &self.inner.queue
    }

    /// The physical adapter.
    #[inline]
    pub fn adapter(&self) -> &wgpu::Adapter {
        &self.inner.adapter
    }

    /// The wgpu instance.
    #[inline]
    pub fn instance(&self) -> &wgpu::Instance {
        &self.inner.instance
    }

    /// Adapter metadata (name, backend, device type).
    #[inline]
    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.inner.info
    }

    /// CPU↔GPU transfer counters for work submitted through this context.
    #[inline]
    pub fn transfer_stats(&self) -> crate::transfer::stats::TransferSnapshot {
        self.inner.transfers.snapshot()
    }

    #[inline]
    pub(crate) fn transfer_counters(&self) -> &crate::transfer::stats::TransferCounters {
        &self.inner.transfers
    }

    /// Borrow a readback staging buffer of exactly `size` bytes.
    ///
    /// Recycled through [`crate::staging::StagingPool`]: a steady stream of
    /// same-resolution readbacks reuses one buffer instead of allocating per
    /// frame. Return it with [`Self::release_staging`] after unmapping.
    pub(crate) fn acquire_staging(&self, size: u64) -> crate::staging::StagingLease {
        self.inner
            .staging
            .lock()
            .expect("staging pool poisoned")
            .acquire(self.device(), &self.inner.transfers, size)
    }

    /// Return an unmapped staging buffer for reuse.
    pub(crate) fn release_staging(&self, lease: crate::staging::StagingLease) {
        self.inner
            .staging
            .lock()
            .expect("staging pool poisoned")
            .release(lease);
    }

    /// Record a declaratively-described compute dispatch into the frame's
    /// shared command encoder.
    ///
    /// The dispatch is **not** submitted immediately: it joins the batch
    /// described in [`crate::dispatch`] and is submitted at the next flush
    /// point (readback of a batched output, [`Self::wait`], explicit
    /// [`Self::flush`], or the batch-size cap).
    pub fn dispatch_compute(&self, dispatch: &crate::dispatch::ComputeDispatch<'_>) {
        self.inner
            .dispatch
            .lock()
            .expect("dispatch state poisoned")
            .record(self.device(), self.queue(), dispatch);
    }

    /// Record a declaratively-described instanced quad draw into the frame's
    /// shared command encoder.
    ///
    /// Batched exactly like [`Self::dispatch_compute`], including the pending-
    /// use bookkeeping for the colour attachment: the pool will not hand the
    /// attachment to a new owner before the batch is submitted, so a caller
    /// may release it as soon as the draw is recorded.
    pub fn draw_quads(&self, draw: &crate::dispatch::QuadDraw<'_>) {
        self.inner
            .dispatch
            .lock()
            .expect("dispatch state poisoned")
            .record_draw(self.device(), self.queue(), draw);
    }

    /// Submit any batched dispatches not yet submitted. A no-op when the
    /// batch is empty.
    pub fn flush(&self) {
        self.inner
            .dispatch
            .lock()
            .expect("dispatch state poisoned")
            .flush(self.queue());
    }

    /// Flush the batch when it still uses `texture` and that texture is
    /// about to be overwritten by an upload.
    pub(crate) fn flush_for_upload(&self, texture: &wgpu::Texture) {
        self.inner
            .dispatch
            .lock()
            .expect("dispatch state poisoned")
            .flush_for_upload(self.queue(), texture);
    }

    /// Flush the batch when it still writes `texture` and that texture is
    /// about to be read back.
    pub(crate) fn flush_for_readback(&self, texture: &wgpu::Texture) {
        self.inner
            .dispatch
            .lock()
            .expect("dispatch state poisoned")
            .flush_for_readback(self.queue(), texture);
    }

    /// Whether the unsubmitted batch still reads or writes `texture`. The
    /// texture pool refuses to reuse such a texture until the flush.
    pub(crate) fn is_pending_use(&self, texture: &wgpu::Texture) -> bool {
        self.inner
            .dispatch
            .lock()
            .expect("dispatch state poisoned")
            .is_pending_use(texture)
    }

    /// Drop cached bind groups referencing the pooled textures `textures`
    /// (by [`PooledTexture`](crate::PooledTexture) id). Called by the pool
    /// when it evicts them: an entry left behind would pin — through its
    /// texture views — VRAM the pool's accounting just released.
    pub(crate) fn evict_dispatch_bind_groups(&self, textures: &[u64]) {
        self.inner
            .dispatch
            .lock()
            .expect("dispatch state poisoned")
            .evict_textures(textures);
    }

    /// Number of cached bind groups (test observation point).
    #[cfg(test)]
    pub(crate) fn cached_bind_group_count(&self) -> usize {
        self.inner
            .dispatch
            .lock()
            .expect("dispatch state poisoned")
            .cached_bind_group_count()
    }

    /// Dispatch batching counters for work recorded through this context.
    #[inline]
    pub fn dispatch_stats(&self) -> crate::dispatch::DispatchSnapshot {
        self.inner
            .dispatch
            .lock()
            .expect("dispatch state poisoned")
            .snapshot()
    }

    /// Block until all previously submitted GPU work has completed and all
    /// pending map callbacks have fired.
    ///
    /// Batched dispatches not yet submitted are flushed first, so after
    /// `wait` returns, everything ever recorded through this context has
    /// completed.
    pub fn wait(&self) {
        self.flush();
        // The result only reports timeouts (which cannot happen for an
        // unbounded wait), so it is safe to ignore.
        let _ = self.inner.device.poll(wgpu::PollType::wait_indefinitely());
    }

    /// Block until one specific submission has completed and its callbacks
    /// have run.
    ///
    /// The narrow counterpart of [`Self::wait`], and the reason a readback no
    /// longer costs a full pipeline sync: it neither submits the pending
    /// dispatch batch nor waits for work this caller does not depend on.
    pub(crate) fn wait_for_submission(&self, submission: &wgpu::SubmissionIndex) -> GpuResult<()> {
        self.inner
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission.clone()),
                timeout: None,
            })
            .map(|_| ())
            .map_err(|e| GpuError::Readback(e.to_string()))
    }

    /// Let the device make progress and run any ready callbacks, without
    /// blocking.
    pub(crate) fn poll_once(&self) {
        let _ = self.inner.device.poll(wgpu::PollType::Poll);
    }

    /// Block until every submission made so far has completed, without
    /// submitting the pending dispatch batch.
    ///
    /// Only the readback's fallback path uses this; ordinary waiting is
    /// [`Self::wait_for_submission`].
    pub(crate) fn poll_blocking(&self) {
        let _ = self.inner.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("adapter", &self.inner.info.name)
            .field("backend", &self.inner.info.backend)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns a context if a GPU is available, otherwise `None` so the test
    /// can skip gracefully on headless CI runners.
    pub(crate) fn try_context() -> Option<GpuContext> {
        GpuContext::new_blocking().ok()
    }

    #[test]
    fn device_initializes_when_gpu_present() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        // A real device exposes a non-empty adapter name.
        assert!(!ctx.adapter_info().name.is_empty());
        ctx.wait();
    }
}
