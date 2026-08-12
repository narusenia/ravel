// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! wgpu device / queue initialization and the shared [`GpuContext`].
//!
//! Ravel keeps a single [`GpuContext`] that owns the wgpu device and queue;
//! every compute node and the whole compositing chain run on it, so textures
//! never round-trip between contexts inside evaluation.
//!
//! **It is not yet shared with GPUI.** Sharing one device between UI rendering
//! and compute is what `REQ-GPU-001` asks for and what
//! [`interop::context_from_wgpu`](crate::interop::context_from_wgpu) exists
//! for. `GPUBK-9` pinned that contract from this side — a context built on
//! someone else's device is a first-class context, and
//! `crates/ravel-gpu/tests/device_sharing.rs` fails if it stops being one — but
//! the host cannot hold up its end yet: gpui publishes no accessor for the
//! device its renderer uses, and on macOS that renderer is Metal-native rather
//! than wgpu-backed. Closing the gap is a patch on the `gpui-ce-ravel` fork,
//! whose scope and upstream-tracking cost are stated in
//! `docs/specifications/architecture.md`.
//!
//! On macOS the Metal backend is selected automatically; on Windows D3D12 is
//! preferred. Backends can be overridden through the standard `WGPU_BACKEND`
//! environment variable, which is an **escape hatch, not a supported
//! configuration**: `WGPU_BACKEND=vulkan` on macOS runs through MoltenVK, which
//! is useful for exercising the Vulkan path on a machine that has no Linux, but
//! puts a translation layer under Metal and leaves
//! [`interop`](crate::interop) unable to hand out native handles (see
//! [`interop::native_api`](crate::interop::native_api)).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::error::{GpuError, GpuResult};

/// The graphics API a [`GpuContext`] is actually running on.
///
/// Stated in this crate's own vocabulary, like
/// [`TextureFormat`](crate::TextureFormat) and
/// [`ShaderTarget`](crate::ShaderTarget), so reading which backend is live
/// does not require naming the backend library.
///
/// **Not [`NativeApi`](crate::interop::NativeApi).** This enum answers "what is
/// executing right now" and covers every backend Ravel can run on; `NativeApi`
/// answers the narrower "whose objects can be handed out through the interop
/// exit", which only Metal and D3D12 can. `interop::native_api` derives the
/// second from the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuBackend {
    /// Vulkan (Windows, Linux, Android, macOS via MoltenVK).
    Vulkan,
    /// Apple Metal.
    Metal,
    /// Direct3D 12 (Windows).
    Dx12,
    /// OpenGL / OpenGL ES / WebGL2.
    Gl,
    /// WebGPU in a browser.
    BrowserWebGpu,
    /// A stub backend that executes nothing — usable for tests, never for
    /// rendering.
    Noop,
}

impl GpuBackend {
    fn from_wgpu(backend: wgpu::Backend) -> Self {
        match backend {
            wgpu::Backend::Vulkan => Self::Vulkan,
            wgpu::Backend::Metal => Self::Metal,
            wgpu::Backend::Dx12 => Self::Dx12,
            wgpu::Backend::Gl => Self::Gl,
            wgpu::Backend::BrowserWebGpu => Self::BrowserWebGpu,
            wgpu::Backend::Noop => Self::Noop,
        }
    }
}

/// What kind of hardware an adapter represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceType {
    /// Unknown or unclassified.
    Other,
    /// Integrated GPU sharing memory with the CPU.
    IntegratedGpu,
    /// Discrete GPU with its own memory.
    DiscreteGpu,
    /// Virtualized or hosted GPU.
    VirtualGpu,
    /// Software rasterizer running on the CPU.
    Cpu,
}

impl DeviceType {
    fn from_wgpu(device_type: wgpu::DeviceType) -> Self {
        match device_type {
            wgpu::DeviceType::Other => Self::Other,
            wgpu::DeviceType::IntegratedGpu => Self::IntegratedGpu,
            wgpu::DeviceType::DiscreteGpu => Self::DiscreteGpu,
            wgpu::DeviceType::VirtualGpu => Self::VirtualGpu,
            wgpu::DeviceType::Cpu => Self::Cpu,
        }
    }
}

/// Identity of the adapter a [`GpuContext`] selected.
///
/// The fields Ravel reports and reasons about — what the logs name, what a
/// performance record has to be attributed to, and which backend the interop
/// exit can speak. Deliberately narrower than the backend's own descriptor:
/// PCI bus ids and subgroup sizes have no consumer here, and every field
/// carried is a field some backend would have to be able to answer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AdapterInfo {
    /// Human-readable adapter name.
    pub name: String,
    /// Backend-specific vendor id (usually a PCI vendor id).
    pub vendor: u32,
    /// Backend-specific device id (usually a PCI device id).
    pub device: u32,
    /// What kind of hardware this is.
    pub device_type: DeviceType,
    /// Driver name, when the backend reports one.
    pub driver: String,
    /// Driver version details, when the backend reports them.
    pub driver_info: String,
    /// The API this adapter is driven through.
    pub backend: GpuBackend,
}

/// A diagnostic classification for a device-loss callback.
///
/// This is deliberately Ravel's vocabulary rather than wgpu's public enum.
/// `Destroyed` is an explicit teardown and therefore does not mark the
/// device as requiring recovery; every other callback reason currently maps
/// to `Unknown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuLossReason {
    /// The backend reported a loss without a more specific Ravel diagnosis.
    Unknown,
    /// The device was explicitly destroyed by its owner.
    Destroyed,
}

/// Shared device state observed by contexts and GPU-owned resources.
///
/// The state has exactly two values: `epoch`, identifying the device
/// generation, and `lost`, identifying whether that generation is usable.
/// This unit does not replace devices, so `epoch` remains zero here; later
/// recovery work may advance it without adding an intermediate phase enum.
#[derive(Clone)]
pub struct GpuDeviceState {
    inner: Arc<GpuDeviceStateInner>,
}

struct GpuDeviceStateInner {
    epoch: AtomicU64,
    lost: AtomicBool,
}

impl Default for GpuDeviceState {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuDeviceState {
    /// Create a fresh state, primarily for headless hosts and tests that
    /// inject a device-loss callback without requiring an adapter.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(GpuDeviceStateInner {
                epoch: AtomicU64::new(0),
                lost: AtomicBool::new(false),
            }),
        }
    }

    /// The current device epoch.
    #[inline]
    pub fn epoch(&self) -> u64 {
        self.inner.epoch.load(Ordering::Acquire)
    }

    /// Whether the current device epoch has been lost.
    #[inline]
    pub fn lost(&self) -> bool {
        self.inner.lost.load(Ordering::Acquire)
    }

    /// Record a diagnostic loss notification.
    ///
    /// Returns `true` only for the first actual loss. Explicit destruction is
    /// intentionally ignored, and repeated callbacks are coalesced.
    pub fn record_loss(&self, reason: GpuLossReason) -> bool {
        if reason == GpuLossReason::Destroyed {
            return false;
        }
        !self.inner.lost.swap(true, Ordering::AcqRel)
    }

    /// Snapshot the two state values for diagnostics and tests.
    #[inline]
    pub fn snapshot(&self) -> GpuDeviceSnapshot {
        GpuDeviceSnapshot {
            epoch: self.epoch(),
            lost: self.lost(),
        }
    }
}

/// A copyable observation of [`GpuDeviceState`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuDeviceSnapshot {
    /// The device generation.
    pub epoch: u64,
    /// Whether that generation is lost.
    pub lost: bool,
}

impl GpuDeviceSnapshot {
    /// The device epoch.
    #[inline]
    pub fn epoch(self) -> u64 {
        self.epoch
    }

    /// Whether the device is lost.
    #[inline]
    pub fn lost(self) -> bool {
        self.lost
    }
}

fn map_loss_reason(reason: wgpu::DeviceLostReason) -> GpuLossReason {
    match reason {
        wgpu::DeviceLostReason::Destroyed => GpuLossReason::Destroyed,
        wgpu::DeviceLostReason::Unknown => GpuLossReason::Unknown,
    }
}

fn handle_device_lost(state: &GpuDeviceState, reason: wgpu::DeviceLostReason) {
    let _ = state.record_loss(map_loss_reason(reason));
}

fn register_device_lost_callback(device: &wgpu::Device, state: GpuDeviceState) {
    device.set_device_lost_callback(move |reason, _message| {
        handle_device_lost(&state, reason);
    });
}

impl AdapterInfo {
    fn from_wgpu(info: &wgpu::AdapterInfo) -> Self {
        Self {
            name: info.name.clone(),
            vendor: info.vendor,
            device: info.device,
            device_type: DeviceType::from_wgpu(info.device_type),
            driver: info.driver.clone(),
            driver_info: info.driver_info.clone(),
            backend: GpuBackend::from_wgpu(info.backend),
        }
    }
}

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
    /// Never read: everything the adapter is asked at startup is already in
    /// `info`. Held so the adapter cannot outlive its context — and so the
    /// context owns the whole set a shared device arrives as
    /// (`interop::context_from_wgpu`).
    #[allow(dead_code)]
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: AdapterInfo,
    state: GpuDeviceState,
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

        let info = AdapterInfo::from_wgpu(&adapter.get_info());
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

        let state = GpuDeviceState::new();
        register_device_lost_callback(&device, state.clone());
        Ok(Self {
            inner: Arc::new(GpuContextInner {
                instance,
                adapter,
                device,
                queue,
                info,
                state,
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
    ///
    /// Reachable from outside the crate as
    /// [`interop::context_from_wgpu`](crate::interop::context_from_wgpu):
    /// receiving a device someone else created is, by definition, naming the
    /// backend, so it belongs to the façade's documented hole rather than to
    /// the abstract API (`GPUBK-4`; the contract itself is `GPUBK-9`). Ravel
    /// does not install a device-loss callback on this path: wgpu replaces
    /// callbacks, and the host owns the callback that drives its recovery.
    pub(crate) fn from_handles(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
    ) -> Self {
        let info = AdapterInfo::from_wgpu(&adapter.get_info());
        Self {
            inner: Arc::new(GpuContextInner {
                instance,
                adapter,
                device,
                queue,
                info,
                state: GpuDeviceState::new(),
                transfers: Default::default(),
                dispatch: Default::default(),
                staging: Default::default(),
            }),
        }
    }

    /// The logical wgpu device.
    ///
    /// Crate-internal, and the reason the rest of this crate exists: a caller
    /// that can reach the device can build anything, which is exactly what a
    /// backend swap must not have to chase down (`GPUBK-4`). The abstract
    /// counterparts are [`Self::dispatch_compute`], [`Self::draw_quads`],
    /// [`TexturePool`](crate::TexturePool) and [`transfer`](crate::transfer).
    #[inline]
    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.inner.device
    }

    /// The command queue. Crate-internal for the same reason as
    /// [`Self::device`].
    #[inline]
    pub(crate) fn queue(&self) -> &wgpu::Queue {
        &self.inner.queue
    }

    /// The wgpu instance this context was built on.
    ///
    /// Reachable from outside the crate as
    /// [`interop::wgpu_instance`](crate::interop::wgpu_instance), the
    /// counterpart of
    /// [`interop::context_from_wgpu`](crate::interop::context_from_wgpu): the
    /// instance is what a second consumer needs in order to be handed the same
    /// device.
    #[inline]
    pub(crate) fn instance(&self) -> &wgpu::Instance {
        &self.inner.instance
    }

    /// Adapter metadata (name, backend, device type).
    #[inline]
    pub fn adapter_info(&self) -> &AdapterInfo {
        &self.inner.info
    }

    /// The shared device state observed by this context and every resource
    /// built from it.
    #[inline]
    pub fn device_state(&self) -> GpuDeviceState {
        self.inner.state.clone()
    }

    /// The current device epoch.
    #[inline]
    pub fn epoch(&self) -> u64 {
        self.inner.state.epoch()
    }

    /// Whether the current device epoch is lost.
    #[inline]
    pub fn lost(&self) -> bool {
        self.inner.state.lost()
    }

    /// Inject a device-loss notification without requiring a live adapter.
    ///
    /// Production callbacks use the same state transition internally; this
    /// entry point lets headless coordinators and tests exercise that path.
    #[inline]
    pub fn inject_device_loss(&self, reason: GpuLossReason) -> bool {
        self.inner.state.record_loss(reason)
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
        let _ = self
            .inner
            .dispatch
            .lock()
            .expect("dispatch state poisoned")
            .flush(self.queue());
    }

    /// Submit pending work and wait only for the submission created by that
    /// flush. Unlike [`Self::wait`], this does not wait for older submissions
    /// that are unrelated to the caller's current output.
    pub fn wait_for_pending(&self) -> GpuResult<()> {
        let submission = self
            .inner
            .dispatch
            .lock()
            .expect("dispatch state poisoned")
            .flush(self.queue());
        if let Some(submission) = submission {
            self.wait_for_submission(&submission, None)?;
        }
        Ok(())
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

    /// Wait for one specific submission to complete and its callbacks to run.
    ///
    /// The narrow counterpart of [`Self::wait`], and the reason a readback no
    /// longer costs a full pipeline sync: it neither submits the pending
    /// dispatch batch nor waits for work this caller does not depend on.
    ///
    /// `timeout` bounds how long the calling thread blocks. `None` waits until
    /// the submission completes; `Some(Duration::ZERO)` does not block at all
    /// and just reports the current state. Returns whether the submission had
    /// completed when the wait ended.
    ///
    /// **A wait that ends in a timeout still drives the device.** The backend's
    /// timed wait is only the blocking part: wgpu then reads the fence and
    /// processes every submission that *has* finished, firing the map callbacks
    /// that belong to them (`wgpu-core/src/device/resource.rs`,
    /// `Device::maintain`). That is what makes a zero-timeout wait a complete
    /// replacement for `PollType::Poll` — it does the same progress work and
    /// additionally reports, per submission, whether the wait was satisfied.
    pub(crate) fn wait_for_submission(
        &self,
        submission: &wgpu::SubmissionIndex,
        timeout: Option<std::time::Duration>,
    ) -> GpuResult<bool> {
        match self.inner.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission.clone()),
            timeout,
        }) {
            Ok(_) => Ok(true),
            // `wgpu::PollError` has exactly two variants, and only this one
            // means "not finished yet" — `WrongSubmissionIndex` is a caller bug
            // (this index comes from a successful submit), and device loss or
            // OOM never reaches here at all: wgpu treats those as fatal inside
            // `Device::poll` rather than turning them into a `PollError`
            // (`WaitIdleError::to_poll_error` maps only these two).
            Err(wgpu::PollError::Timeout) => Ok(false),
            Err(e) => Err(GpuError::Readback(e.to_string())),
        }
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

    #[test]
    fn injected_loss_is_shared_and_coalesced() {
        let state = GpuDeviceState::new();
        let clone = state.clone();

        assert_eq!(
            state.snapshot(),
            GpuDeviceSnapshot {
                epoch: 0,
                lost: false
            }
        );
        assert!(state.record_loss(GpuLossReason::Unknown));
        assert_eq!(
            clone.snapshot(),
            GpuDeviceSnapshot {
                epoch: 0,
                lost: true
            }
        );
        assert!(!clone.record_loss(GpuLossReason::Unknown));
        assert_eq!(state.epoch(), 0);
    }

    #[test]
    fn destroyed_loss_is_ignored_but_real_loss_is_not() {
        let state = GpuDeviceState::new();

        assert!(!state.record_loss(GpuLossReason::Destroyed));
        assert!(!state.lost());
        assert!(state.record_loss(GpuLossReason::Unknown));
        assert!(state.lost());
    }

    #[test]
    fn wgpu_loss_reason_mapping_keeps_destroyed_out_of_recovery() {
        let state = GpuDeviceState::new();

        handle_device_lost(&state, wgpu::DeviceLostReason::Destroyed);
        assert!(!state.lost());
        handle_device_lost(&state, wgpu::DeviceLostReason::Unknown);
        assert!(state.lost());
        handle_device_lost(&state, wgpu::DeviceLostReason::Unknown);
        assert_eq!(state.epoch(), 0);
    }

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
