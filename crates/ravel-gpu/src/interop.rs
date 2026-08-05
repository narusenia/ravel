// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Backend-native handles — **the one documented hole in the GPU façade**.
//!
//! Every other module of this crate exists to keep the graphics API out of its
//! callers' vocabulary: [`BindingDesc`](crate::BindingDesc),
//! [`TextureFormat`](crate::TextureFormat) and
//! [`ShaderTarget`](crate::ShaderTarget) name what a shader needs without
//! naming who executes it, so a backend can be replaced without touching a
//! node. This module does the opposite on purpose, for the two requirements
//! that cannot be met any other way:
//!
//! * **REQ-PLUGIN-001 — the OpenFX host.** The OFX GPU Render Suite hands a
//!   plugin its images as native objects (`id<MTLTexture>`, an
//!   `ID3D12Resource`, a CUDA device pointer). Without a way to name Ravel's
//!   textures in those terms, every OFX node in a graph costs a full readback
//!   and re-upload per frame — reintroducing, once per plugin, the CPU
//!   round-trip `issues/closed/HIGH-05` and `issues/high/HIGH-04` removed.
//! * **REQ-GPU-001 — hardware decode.** VideoToolbox, NVDEC and AMF produce
//!   frames that already live in VRAM. Receiving them zero-copy starts with
//!   naming the device those frames must be created against.
//!
//! # Not for node processors
//!
//! A node processor must never reach a handle from here. Doing so pins the
//! node to one backend and silently opts it out of everything the abstraction
//! buys — dispatch batching, uniform and bind-group reuse, the texture pool's
//! lifetime bookkeeping. `scripts/lint-patterns.sh` enforces this
//! mechanically (rule `gpu-interop-escape`): the reachable callers are this
//! crate, `ravel-media` (hardware decode) and the future OFX host crate. The
//! types are deliberately **not** re-exported from the crate root, so every
//! use site spells `ravel_gpu::interop` and the lint can see it.
//!
//! # What a handle is, and is not
//!
//! A [`NativeHandle`] is a **borrowed pointer**. It owns nothing, retains
//! nothing, and is valid only while the Ravel object it was taken from is
//! alive — which the borrow in its lifetime parameter is there to enforce.
//! Nothing here transfers ownership, so nothing here may be released,
//! destroyed, or handed to an API that assumes it may.
//!
//! # Coverage
//!
//! | Backend | Device | Texture |
//! |---|---|---|
//! | Metal | `id<MTLDevice>` | `id<MTLTexture>` |
//! | D3D12 | `ID3D12Device*` | `ID3D12Resource*` |
//! | Vulkan, GL, others | — | — |
//!
//! Vulkan is absent by design rather than omission: `VkImage` is a
//! non-dispatchable `u64` handle, not a pointer, so it does not fit
//! [`NativeHandle`] and needs its own shape when `GPUBK-12` lands. The Metal
//! command queue is absent because the pinned wgpu revision exposes no
//! accessor for it (`wgpu_hal::metal::QueueShared::raw` is private); see the
//! implementation note in `docs/implementation/gpu-backend-plan.md`.
//!
//! # Device sharing
//!
//! [`context_from_wgpu`] and [`wgpu_instance`] are the other direction and a
//! different layer: they trade in the objects of the *current implementation*
//! (wgpu), not in the platform objects underneath it. They live here for the
//! same reason the handles above do — a signature that names the graphics
//! stack is a hole in the façade, and the crate keeps its holes in one place
//! where the `gpu-interop-escape` lint can see them (`GPUBK-4`).
//!
//! They exist because REQ-GPU-001 requires the UI and the compute pipeline to
//! share one device, and a shared device is by definition one the caller
//! creates and Ravel accepts. Sharing GPUI's device therefore trips the lint
//! today: whether the host crate joins the allowed set, or the contract takes
//! another shape entirely, is `GPUBK-9`'s decision, and stating the cost in
//! the lint rather than hiding it is the point.

use core::ffi::c_void;
use core::marker::PhantomData;
use core::ptr::NonNull;

use crate::device::{GpuBackend, GpuContext};
use crate::frame::GpuFrameBuffer;

/// The native graphics API a handle belongs to.
///
/// Closed over the backends whose objects are pointers and that Ravel plans to
/// speak natively (REQ-INFRA-009); a backend that needs interop adds its
/// variant then, exactly as [`ShaderTarget`](crate::ShaderTarget) does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NativeApi {
    /// Apple Metal. Objects are Objective-C instances (`id<MTL…>`).
    Metal,
    /// Direct3D 12. Objects are COM interface pointers.
    Direct3D12,
}

/// A borrowed pointer to an object owned by the native graphics API.
///
/// The lifetime is the borrow of the Ravel object the handle was taken from
/// ([`GpuContext`], [`GpuFrameBuffer`]). That object keeps the native object
/// alive, so a `NativeHandle` cannot outlive what it points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NativeHandle<'a> {
    api: NativeApi,
    ptr: NonNull<c_void>,
    owner: PhantomData<&'a ()>,
}

impl<'a> NativeHandle<'a> {
    /// Wrap a pointer known to belong to `api` and to live at least as long
    /// as `'a`. Private: the only producers are the accessors below.
    fn new(api: NativeApi, ptr: NonNull<c_void>) -> Self {
        Self {
            api,
            ptr,
            owner: PhantomData,
        }
    }

    /// Which API this pointer must be interpreted under.
    #[inline]
    pub fn api(self) -> NativeApi {
        self.api
    }

    /// The pointer, for handing to the native API or an FFI boundary.
    ///
    /// Non-null by construction. It is `*mut` because both OFX and the
    /// platform APIs take it that way; the pointee must still be treated as
    /// borrowed (see the module documentation).
    #[inline]
    pub fn as_ptr(self) -> *mut c_void {
        self.ptr.as_ptr()
    }
}

/// The native device backing a [`GpuContext`]: `id<MTLDevice>` under
/// [`NativeApi::Metal`], `ID3D12Device*` under [`NativeApi::Direct3D12`].
///
/// This is the handle a hardware decoder is configured against (REQ-GPU-001)
/// and the one an OFX host reports to a plugin as the render device
/// (REQ-PLUGIN-001).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NativeDevice<'a>(NativeHandle<'a>);

impl<'a> NativeDevice<'a> {
    /// Which API this device belongs to.
    #[inline]
    pub fn api(self) -> NativeApi {
        self.0.api()
    }

    /// The device pointer.
    #[inline]
    pub fn as_ptr(self) -> *mut c_void {
        self.0.as_ptr()
    }

    /// The handle in its untyped form.
    #[inline]
    pub fn handle(self) -> NativeHandle<'a> {
        self.0
    }
}

/// The native texture behind a [`GpuFrameBuffer`]: `id<MTLTexture>` under
/// [`NativeApi::Metal`], `ID3D12Resource*` under [`NativeApi::Direct3D12`].
///
/// The image an OFX plugin renders into or samples from (REQ-PLUGIN-001).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NativeTexture<'a>(NativeHandle<'a>);

impl<'a> NativeTexture<'a> {
    /// Which API this texture belongs to.
    #[inline]
    pub fn api(self) -> NativeApi {
        self.0.api()
    }

    /// The texture pointer.
    #[inline]
    pub fn as_ptr(self) -> *mut c_void {
        self.0.as_ptr()
    }

    /// The handle in its untyped form.
    #[inline]
    pub fn handle(self) -> NativeHandle<'a> {
        self.0
    }
}

/// Build a [`GpuContext`] on wgpu objects the caller already owns.
///
/// The import counterpart of everything else in this module, and the contract
/// REQ-GPU-001 rests on: the UI (GPUI) and the compute pipeline run on **one**
/// device, so a texture never round-trips between two of them. `Ravel` records
/// its dispatches through the queue given here, so work submitted directly
/// against the same queue by the caller is ordered against Ravel's the way any
/// two submissions to one queue are — a separate queue built from the same
/// device is not (see [`native_device`]'s safety notes).
///
/// The four objects must belong together: the device and queue must come from
/// the adapter, and the adapter from the instance. Nothing checks it, and a
/// mismatched set makes [`native_api`] report a backend whose accessors then
/// answer `None`.
pub fn context_from_wgpu(
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
) -> GpuContext {
    GpuContext::from_handles(instance, adapter, device, queue)
}

/// The wgpu instance `ctx` was built on.
///
/// The pair of [`context_from_wgpu`] in the other direction: a consumer that
/// has to enumerate or create surfaces on the same instance — the case device
/// sharing with a windowing toolkit starts from — needs the instance Ravel is
/// already using rather than one of its own, since adapters and devices from
/// two instances cannot be mixed.
pub fn wgpu_instance(ctx: &GpuContext) -> &wgpu::Instance {
    ctx.instance()
}

/// The [`NativeApi`] `ctx` runs on, or `None` when its backend has no interop
/// support here (Vulkan, GL, or a software adapter).
///
/// Safe and cheap: it reads the adapter description rather than the device, so
/// an OFX host can decide which GPU suite to advertise before it touches an
/// `unsafe` accessor. For a context from [`GpuContext::new`] or
/// [`GpuContext::new_blocking`], a `Some` here means the matching accessor
/// returns `Some`. It is a prediction, not a guarantee: [`context_from_wgpu`]
/// accepts an adapter and a device chosen by the caller, so a context built
/// from a mismatched pair can report a backend whose accessor then answers
/// `None`. The accessors never mislabel a handle either way — each one asks
/// `as_hal` for one specific API and tags the result with that same API, so a
/// `Some` is always the API it says it is.
pub fn native_api(ctx: &GpuContext) -> Option<NativeApi> {
    match ctx.adapter_info().backend {
        GpuBackend::Metal => Some(NativeApi::Metal),
        GpuBackend::Dx12 => Some(NativeApi::Direct3D12),
        _ => None,
    }
}

/// The native device handle behind `ctx`.
///
/// Returns `None` when the context runs on a backend this module does not
/// cover — [`native_api`] answers that question without `unsafe`.
///
/// # Safety
///
/// The returned pointer is **borrowed**, and the caller must uphold all of:
///
/// * It is valid only while `ctx` (or another clone of the same
///   [`GpuContext`]) is alive. The returned value's lifetime enforces this for
///   Rust callers; a pointer copied out across an FFI boundary is on the
///   caller.
/// * It must not be released, destroyed, or otherwise have its ownership
///   assumed. Under Metal it is an unretained `id`; under D3D12 it is a COM
///   pointer whose reference count was **not** incremented, so an `AddRef` is
///   required before any code path that will `Release` it.
/// * Work submitted directly against this device is invisible to Ravel's
///   dispatch batching, and a queue the caller creates from this device is a
///   *separate* timeline from Ravel's. Ordering the two is the caller's job.
///   [`GpuContext::flush`] only **submits** the pending batch — it does not
///   wait for it — so it orders nothing on its own. Either call
///   [`GpuContext::wait`], which submits and then blocks until everything
///   recorded through this context has completed, or share a fence
///   (`MTLSharedEvent`, an `ID3D12Fence`) between the two queues. `flush`
///   alone is enough only when the consumer submits to Ravel's own queue.
/// * All safety requirements of `wgpu-hal` apply, since this is
///   `wgpu::Device::as_hal` with the guard dropped.
pub unsafe fn native_device(ctx: &GpuContext) -> Option<NativeDevice<'_>> {
    let handle = unsafe { device_handle(ctx) }?;
    Some(NativeDevice(handle))
}

/// The native texture handle behind `frame`.
///
/// Returns `None` when the frame's context runs on a backend this module does
/// not cover — [`native_api`] answers that question without `unsafe`.
///
/// # Safety
///
/// Everything [`native_device`] requires, plus:
///
/// * The texture is **pooled**. It returns to the
///   [`TexturePool`](crate::TexturePool) when the last [`GpuFrameBuffer`]
///   clone drops, and the pool may then hand the same texture to an unrelated
///   frame. Holding `frame` alive for as long as the pointer is used is
///   therefore mandatory, not merely a borrow-checker formality.
/// * The contents are whatever Ravel's pending dispatch batch has *completed*,
///   which is not the same as what it has recorded or even submitted. A
///   consumer that does not synchronise through Ravel's queue must wait, not
///   merely flush: [`GpuContext::wait`], a readback of the frame, or a fence
///   shared with the native queue. [`GpuContext::flush`] submits the batch
///   without waiting for it, so on its own it still races the reader.
pub unsafe fn native_texture(frame: &GpuFrameBuffer) -> Option<NativeTexture<'_>> {
    let handle = unsafe { texture_handle(frame) }?;
    Some(NativeTexture(handle))
}

#[cfg(target_os = "macos")]
unsafe fn device_handle(ctx: &GpuContext) -> Option<NativeHandle<'_>> {
    let device = unsafe { ctx.device().as_hal::<wgpu::hal::api::Metal>() }?;
    // `raw_device()` borrows the `Retained<ProtocolObject<dyn MTLDevice>>` the
    // hal device owns; the object itself is kept alive by the wgpu device, not
    // by the guard, so the pointer stays valid after the guard is dropped.
    let ptr = core::ptr::from_ref(&**device.raw_device()).cast::<c_void>();
    Some(NativeHandle::new(
        NativeApi::Metal,
        NonNull::new(ptr.cast_mut())?,
    ))
}

#[cfg(target_os = "macos")]
unsafe fn texture_handle(frame: &GpuFrameBuffer) -> Option<NativeHandle<'_>> {
    let texture = unsafe { frame.texture().as_hal::<wgpu::hal::api::Metal>() }?;
    let ptr = core::ptr::from_ref(texture.raw_handle()).cast::<c_void>();
    Some(NativeHandle::new(
        NativeApi::Metal,
        NonNull::new(ptr.cast_mut())?,
    ))
}

/// Read the COM interface pointer out of a windows-rs interface wrapper.
///
/// Every windows-rs interface is a `#[repr(transparent)]` newtype over a
/// non-null pointer, and `windows_core::Interface::as_raw` is exactly this
/// `transmute_copy`. It is spelled out here because `windows-core` is not a
/// dependency of this crate (and must not become one: `wgpu-hal` owns the
/// D3D12 binding, and a second copy of the Windows crates in the tree is how
/// interface identities start disagreeing).
///
/// # Safety
///
/// `T` must be a windows-rs COM interface type — a `repr(transparent)` wrapper
/// of pointer size around a non-null pointer.
#[cfg(target_os = "windows")]
unsafe fn com_ptr<T>(iface: &T) -> *mut c_void {
    const {
        assert!(size_of::<T>() == size_of::<*mut c_void>());
    }
    unsafe { core::mem::transmute_copy(iface) }
}

#[cfg(target_os = "windows")]
unsafe fn device_handle(ctx: &GpuContext) -> Option<NativeHandle<'_>> {
    let device = unsafe { ctx.device().as_hal::<wgpu::hal::api::Dx12>() }?;
    let ptr = unsafe { com_ptr(device.raw_device()) };
    Some(NativeHandle::new(NativeApi::Direct3D12, NonNull::new(ptr)?))
}

#[cfg(target_os = "windows")]
unsafe fn texture_handle(frame: &GpuFrameBuffer) -> Option<NativeHandle<'_>> {
    let texture = unsafe { frame.texture().as_hal::<wgpu::hal::api::Dx12>() }?;
    let ptr = unsafe { com_ptr(texture.raw_resource()) };
    Some(NativeHandle::new(NativeApi::Direct3D12, NonNull::new(ptr)?))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
unsafe fn device_handle(_ctx: &GpuContext) -> Option<NativeHandle<'_>> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
unsafe fn texture_handle(_frame: &GpuFrameBuffer) -> Option<NativeHandle<'_>> {
    None
}
