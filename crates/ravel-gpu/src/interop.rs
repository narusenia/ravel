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
//! mechanically (rule `gpu-native-handle-escape`): the reachable callers are
//! this crate, `ravel-media` (hardware decode) and the future OFX host crate.
//! The types are deliberately **not** re-exported from the crate root, so
//! every use site spells `ravel_gpu::interop`; the lint keys on the handle
//! symbols themselves, so an alias does not launder the escape.
//!
//! [`native_api`] is outside that rule on purpose: it reads the adapter
//! description, hands out no pointer and needs no `unsafe`, so asking which
//! API is live is not leaving the abstraction.
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
//! [`NativeHandle`] and needs its own shape when `GPUBK-12` lands.
//!
//! A host that owns native renderer objects can pass a borrowed device and
//! command queue pair through [`context_from_native`]. The import route
//! enumerates the wgpu adapters for that native API, finds the adapter whose
//! device is the same native object, and returns a [`NativeGpuContext`] that
//! contains the matching abstract [`GpuContext`]. The public wgpu 29 API still
//! has no way to turn an existing `MTLDevice` directly into a
//! `wgpu::hal::ExposedAdapter`; matching an adapter and creating a logical
//! wgpu device from it is the safe public alternative. The native queue remains
//! a separate timeline, so synchronization belongs to the caller (for example
//! an OFX host or the zero-copy viewer).
//!
//! # Device sharing
//!
//! [`context_from_wgpu`] and [`wgpu_instance`] are the other direction and a
//! different layer: they trade in the objects of the *current implementation*
//! (wgpu), not in the platform objects underneath it. They live here because a
//! signature that names the graphics stack belongs where the crate keeps the
//! rest of them (`GPUBK-4`).
//!
//! They exist because REQ-GPU-001 requires the UI and the compute pipeline to
//! share one device, and a shared device is by definition one the caller
//! creates and Ravel accepts. **`GPUBK-9` settled what that means for the
//! lint**: sharing a device is not the escape the handle accessors are, so it
//! is not judged by the same rule. `context_from_wgpu` receives rather than
//! hands out, runs once at startup, and bypasses neither dispatch batching nor
//! the texture pool — every subsystem is built on the context it returns. What
//! it does decide is which device the whole evaluation pipeline runs on, so the
//! callers are `ravel-gpu` and the application host (`ravel-app`) and no one
//! else; `scripts/lint-patterns.sh` enforces that pair as
//! `gpu-device-sharing`, separately from `gpu-native-handle-escape`.
//!
//! The signatures below name `wgpu` types, and that is not a leak to be fixed
//! later: naming the toolkit's device type is the whole job, so replacing the
//! backend moves this boundary with it. `crates/ravel-gpu/tests/device_sharing.rs`
//! pins the contract — a context built from someone else's device runs the
//! abstract API end to end, and the device it runs on is theirs.
//!
//! The GPUI fork now exposes its native pair through a platform-neutral
//! `Window::native_gpu_handles` method. Ravel's application-side window wiring
//! is intentionally still separate: this module only defines the receiving
//! boundary, and a later viewer unit decides when to retain the window and
//! synchronize work between the two command queues.

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
/// ([`GpuContext`], [`GpuFrameBuffer`]) or of a [`NativeGpuContext`] descriptor
/// supplied by an external host. The former keeps the native object alive; the
/// latter is only a typed borrow of an object whose lifetime the caller
/// promised to uphold when calling [`context_from_native`].
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

/// A borrowed native device and command queue supplied by a host renderer,
/// together with an abstract context on the same native device.
///
/// The native pair is opaque to the GPU façade and is useful for native
/// interop consumers that need both objects to agree on a device. It does not
/// own or retain either native object. The [`GpuContext`] is created by
/// [`context_from_native`] after it finds a wgpu adapter with the same native
/// device; the command queue is still a separate native timeline.
#[derive(Clone)]
pub struct NativeGpuContext<'a> {
    api: NativeApi,
    device: NonNull<c_void>,
    command_queue: NonNull<c_void>,
    owner: PhantomData<&'a ()>,
    context: GpuContext,
}

impl<'a> NativeGpuContext<'a> {
    /// Which native graphics API owns both objects.
    #[inline]
    pub fn api(&self) -> NativeApi {
        self.api
    }

    /// The borrowed native device.
    #[inline]
    pub fn device(&self) -> NativeDevice<'a> {
        NativeDevice(NativeHandle::new(self.api, self.device))
    }

    /// The borrowed native command queue.
    #[inline]
    pub fn command_queue(&self) -> NativeHandle<'a> {
        NativeHandle::new(self.api, self.command_queue)
    }

    /// The abstract Ravel context built on the same native device.
    #[inline]
    pub fn gpu_context(&self) -> &GpuContext {
        &self.context
    }
}

/// Accept a host renderer's native device and command queue.
///
/// This is the native import route in a backend-neutral shape. It builds an
/// instance restricted to `api`'s backend, enumerates its adapters, creates a
/// logical wgpu device for each candidate, and returns only when the
/// candidate's native device pointer is the same as `device`. The returned
/// [`NativeGpuContext::gpu_context`] is therefore the abstract API running on
/// the host's physical device.
///
/// Unlike [`context_from_wgpu`], the caller supplies no wgpu objects: on a
/// platform whose renderer is not wgpu-backed there are none to supply, which
/// is the whole reason this route exists.
///
/// # Safety
///
/// The caller must guarantee all of the following for as long as the returned
/// descriptor, or any handle borrowed from it, is used:
///
/// * `device` and `command_queue` are non-null, valid native objects owned by
///   `api`;
/// * `command_queue` belongs to `device` and both objects remain alive; and
/// * neither object is released, destroyed, or given to code that assumes
///   ownership through this descriptor. The descriptor performs no retain or
///   reference-count operation.
/// * The caller passes the renderer's actual device, not a device obtained
///   from a different selection policy. GPUI prefers a non-removable,
///   low-power Metal device while Ravel's default wgpu request prefers
///   high-performance; on a multi-GPU Mac those policies can select different
///   devices. The caller must treat `None` as "no shared device" and must not
///   use a separately-created context with the native pair.
pub async unsafe fn context_from_native<'a>(
    api: NativeApi,
    device: *mut c_void,
    command_queue: *mut c_void,
) -> Option<NativeGpuContext<'a>> {
    let host_device = NonNull::new(device)?;
    let native_command_queue = NonNull::new(command_queue)?;
    let backends = match api {
        NativeApi::Metal => wgpu::Backends::METAL,
        NativeApi::Direct3D12 => wgpu::Backends::DX12,
    };

    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    let instance = wgpu::Instance::new(desc);

    for adapter in instance.enumerate_adapters(backends).await {
        let adapter_limits = adapter.limits();
        let Ok((device, queue)) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("native host device"),
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
        else {
            continue;
        };

        let context = context_from_wgpu(instance.clone(), adapter, device, queue);
        // SAFETY: `context` owns the wgpu device for the duration of this
        // comparison. `native_device` only reads the backend handle and does
        // not transfer ownership of it.
        let Some(wgpu_device) = (unsafe { native_device(&context) }) else {
            continue;
        };
        if wgpu_device.api() != api || wgpu_device.as_ptr() != host_device.as_ptr().cast() {
            continue;
        }

        return Some(NativeGpuContext {
            api,
            device: host_device,
            command_queue: native_command_queue,
            owner: PhantomData,
            context,
        });
    }

    None
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

/// Whether `ctx` names the native device supplied by a host renderer.
///
/// This is deliberately a predicate rather than a handle accessor: the host
/// can decide whether a borrowed texture may be sampled without naming
/// [`NativeDevice`] or [`NativeHandle`] outside this module. A null host
/// pointer is never considered a match.
pub fn native_device_matches(ctx: &GpuContext, api: NativeApi, host_device: *mut c_void) -> bool {
    let Some(host_device) = NonNull::new(host_device) else {
        return false;
    };
    // SAFETY: this only compares the borrowed pointer and never dereferences,
    // retains, releases, or transfers ownership of it.
    let Some(device) = (unsafe { native_device(ctx) }) else {
        return false;
    };
    device.api() == api && device.as_ptr() == host_device.as_ptr()
}

/// Loan a display texture to a native surface consumer after proving that the
/// consumer's device is the one that owns it.
///
/// The callback is the narrow bridge to a toolkit such as GPUI. It must use
/// the pointer only during the callback; the [`GpuFrameBuffer`] itself must be
/// kept alive by the caller until the toolkit has finished its scene. The
/// helper never retains or releases the native object.
pub fn with_surface_texture<T>(
    frame: &GpuFrameBuffer,
    host_device: *mut c_void,
    consume: impl FnOnce(*mut c_void, u32, u32) -> T,
) -> Option<T> {
    if !native_device_matches(frame.context(), NativeApi::Metal, host_device) {
        return None;
    }
    // SAFETY: the frame owns the wgpu texture and remains borrowed for the
    // whole callback. The callback receives a non-owning pointer and cannot
    // outlive that borrow through this function.
    let texture = unsafe { native_texture(frame) }?;
    if texture.api() != NativeApi::Metal {
        return None;
    }
    Some(consume(texture.as_ptr(), frame.width(), frame.height()))
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
