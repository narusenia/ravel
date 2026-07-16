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

    /// Block until all previously submitted GPU work has completed and all
    /// pending map callbacks have fired.
    pub fn wait(&self) {
        // The result only reports timeouts (which cannot happen for an
        // unbounded wait), so it is safe to ignore.
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
