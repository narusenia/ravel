// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Device sharing (`interop::context_from_wgpu`, `GPUBK-9`).
//!
//! REQ-GPU-001's acceptance condition "the UI framework and the GPU device are
//! shared" rests on one property of this crate: a [`GpuContext`] built on
//! graphics objects **someone else created** must be a first-class context, not
//! a degraded one. The UI toolkit owns the device in that arrangement, so
//! everything Ravel builds — shader compilation, pipelines, the texture pool,
//! the dispatch batcher, the staging pool, readback — has to work on a device
//! this crate never requested.
//!
//! The tests below stand in for the toolkit: they create the instance, adapter,
//! device and queue the way a windowing library does, hand them over, and then
//! use only the abstract API. Three things are asserted, because "it did not
//! crash" is not the same as "it ran on the caller's device":
//!
//! 1. the abstract API completes a dispatch and a readback on the foreign
//!    device, and the context reports the caller's instance and adapter;
//! 2. resources Ravel allocates are validated against the **caller's** device
//!    limits — proof by error scope, which is per-device, that the device was
//!    adopted rather than re-created;
//! 3. the native interop descriptor accepts a host-owned device/queue pair; on
//!    macOS that pair is compared with the wgpu device used by the abstract
//!    API, which is what an OFX host or GPUI-backed viewer depends on.
//!
//! Breaking the contract fails these mechanically: making the entry point
//! `pub(crate)` or moving it out of `interop` fails compilation, and replacing
//! the body of `context_from_wgpu` with `GpuContext::new_blocking()` fails 1
//! and 2 (verified). It does **not** fail 3, and that is a fact worth writing
//! down rather than papering over: on Apple Silicon `MTLCreateSystemDefaultDevice`
//! hands out one process-wide device, so two independent wgpu devices report the
//! same `id<MTLDevice>`. A native pointer is therefore evidence about the
//! hardware, not about which `wgpu::Device` Ravel holds — the identity proofs
//! are 1 and 2.
//!
//! On macOS, GPUI's Metal renderer and wgpu both obtain the process system
//! default `MTLDevice`. The native test below receives the same shape exposed
//! by the fork's `Window::native_gpu_handles` accessor and compares it with
//! the wgpu device before exercising the abstract API on that device.
//!
//! Skips gracefully when no GPU adapter is available, like the other GPU tests
//! here.

use std::sync::Arc;

#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::runtime::ProtocolObject;
#[cfg(target_os = "macos")]
use objc2_metal::{MTLCopyAllDevices, MTLCreateSystemDefaultDevice, MTLDevice};

use ravel_gpu::compute::ComputePipeline;
use ravel_gpu::interop;
use ravel_gpu::{
    BindingDesc, BindingKind, ComputeDispatch, GpuBackend, GpuContext, ShaderManager,
    ShaderVisibility, TextureFormat, TextureKey, TexturePool, TextureUsage, read_texture,
    upload_texture,
};

#[cfg(target_os = "macos")]
// `MTLCreateSystemDefaultDevice` pulls CoreGraphics into the link on macOS.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {}

/// The graphics objects a UI toolkit would already own by the time Ravel starts.
struct HostGpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl HostGpu {
    /// Build them the way a toolkit does: one instance, one adapter, one device,
    /// one queue, with the toolkit's own choice of limits.
    ///
    /// `max_texture_dimension_2d` is a parameter because the caller's limits are
    /// what test 2 detects. `None` asks for the adapter's maximum, which is what
    /// [`GpuContext::new`] would also request.
    fn new(max_texture_dimension_2d: Option<u32>) -> Option<Self> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY);
        let instance = wgpu::Instance::new(desc);

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok()?;

        let adapter_limits = adapter.limits();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("host device (stands in for the UI toolkit)"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits {
                max_texture_dimension_2d:
                    max_texture_dimension_2d.unwrap_or(adapter_limits.max_texture_dimension_2d),
                max_buffer_size: adapter_limits.max_buffer_size,
                max_storage_buffer_binding_size: adapter_limits.max_storage_buffer_binding_size,
                ..wgpu::Limits::default()
            },
            ..Default::default()
        }))
        .ok()?;

        Some(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// Hand the objects over while keeping a clone of each, so the test can ask
    /// afterwards whether the context is running on exactly these.
    fn share(&self) -> GpuContext {
        interop::context_from_wgpu(
            self.instance.clone(),
            self.adapter.clone(),
            self.device.clone(),
            self.queue.clone(),
        )
    }
}

#[test]
fn a_shared_context_runs_the_abstract_api_end_to_end() {
    let Some(host) = HostGpu::new(None) else {
        eprintln!("skipping a_shared_context_runs_the_abstract_api_end_to_end: no GPU adapter");
        return;
    };
    let ctx = host.share();

    // The context must report the caller's objects, not ones of its own: the
    // instance is compared by identity (wgpu handles compare by the object they
    // refer to), and the adapter through the description Ravel derived from it.
    assert_eq!(
        interop::wgpu_instance(&ctx),
        &host.instance,
        "the shared context must expose the host's instance — a surface created \
         on a different instance cannot be used with this device"
    );
    let host_info = host.adapter.get_info();
    let expected_backend = match host_info.backend {
        wgpu::Backend::Vulkan => GpuBackend::Vulkan,
        wgpu::Backend::Metal => GpuBackend::Metal,
        wgpu::Backend::Dx12 => GpuBackend::Dx12,
        wgpu::Backend::Gl => GpuBackend::Gl,
        wgpu::Backend::BrowserWebGpu => GpuBackend::BrowserWebGpu,
        wgpu::Backend::Noop => GpuBackend::Noop,
    };
    let info = ctx.adapter_info();
    assert_eq!(
        (info.name.as_str(), info.vendor, info.device, info.backend),
        (
            host_info.name.as_str(),
            host_info.vendor,
            host_info.device,
            expected_backend
        ),
        "the shared context must describe the host's adapter"
    );

    assert_abstract_api_runs(ctx.clone());
}

/// Exercise the whole abstract GPU path on a context supplied by a host.
fn assert_abstract_api_runs(ctx: GpuContext) {
    let width = 4u32;
    let height = 4u32;
    let format = TextureFormat::Rgba32Float;

    let mut shaders = ShaderManager::new(ctx.clone());
    let compiled = shaders.compile("invert").expect("compile invert");
    let pipeline = Arc::new(ComputePipeline::new(
        &ctx,
        &compiled,
        "main",
        &[
            BindingDesc::new(0, BindingKind::InputTexture, ShaderVisibility::COMPUTE),
            BindingDesc::new(
                1,
                BindingKind::OutputStorageTexture(TextureFormat::Rgba32Float),
                ShaderVisibility::COMPUTE,
            ),
        ],
        [8, 8],
    ));

    let mut pool = TexturePool::new(ctx.clone(), 64 * 1024 * 1024);
    let input = pool.acquire(TextureKey::new(
        width,
        height,
        format,
        TextureUsage::TEXTURE_BINDING | TextureUsage::COPY_DST,
    ));
    let output = pool.acquire(TextureKey::new(
        width,
        height,
        format,
        TextureUsage::STORAGE_BINDING | TextureUsage::COPY_SRC,
    ));

    let pixel_count = (width * height) as usize;
    let mut data = Vec::<f32>::with_capacity(pixel_count * 4);
    for i in 0..pixel_count {
        let v = i as f32 / pixel_count as f32;
        data.extend_from_slice(&[v, 0.25, 0.5, 1.0]);
    }
    upload_texture(&ctx, &input, bytemuck::cast_slice(&data));

    ctx.dispatch_compute(&ComputeDispatch {
        label: "invert",
        pipeline: &pipeline,
        inputs: &[input.binding()],
        output: &output.binding(),
        uniform: &[],
        width,
        height,
    });

    let raw = read_texture(&ctx, &output).expect("readback from the shared device");
    let result: &[f32] = bytemuck::cast_slice(&raw);
    assert_eq!(result.len(), data.len());
    for i in 0..pixel_count {
        let base = i * 4;
        let eps = 1e-5;
        assert!(
            (result[base] - (1.0 - data[base])).abs() < eps,
            "r at {i}: the dispatch did not run on the shared device"
        );
        assert!((result[base + 3] - data[base + 3]).abs() < eps, "a at {i}");
    }
}

#[test]
fn a_shared_context_allocates_against_the_hosts_limits() {
    // The toolkit asks for a *lower* texture limit than the adapter supports.
    // A context that adopted the device inherits that ceiling; one that quietly
    // requested its own device would get the adapter's, the way
    // `GpuContext::new` does.
    const HOST_MAX_DIM: u32 = 2048;

    let Some(host) = HostGpu::new(Some(HOST_MAX_DIM)) else {
        eprintln!("skipping a_shared_context_allocates_against_the_hosts_limits: no GPU adapter");
        return;
    };
    if host.adapter.limits().max_texture_dimension_2d <= HOST_MAX_DIM {
        eprintln!(
            "skipping a_shared_context_allocates_against_the_hosts_limits: the adapter's own \
             limit is not above the host's, so the check cannot distinguish the two devices"
        );
        return;
    }

    let ctx = host.share();

    // An error scope belongs to one device and captures only that device's
    // validation errors, which makes it the portable way to ask "did Ravel
    // allocate on *this* device?" — no backend-specific handle involved.
    let scope = host.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut pool = TexturePool::new(ctx.clone(), 64 * 1024 * 1024);
    let oversized = pool.acquire(TextureKey::new(
        HOST_MAX_DIM * 2,
        4,
        TextureFormat::Rgba8Unorm,
        TextureUsage::TEXTURE_BINDING | TextureUsage::COPY_DST,
    ));
    let error = pollster::block_on(scope.pop());
    // The texture is invalid by construction; drop it before anything can use it.
    drop(oversized);

    assert!(
        error.is_some(),
        "allocating past the host device's max_texture_dimension_2d ({HOST_MAX_DIM}) raised no \
         error on the host device — the context is not running on the device it was given"
    );
}

/// The interop handle path still works when the device came from outside.
///
/// An OFX host (REQ-PLUGIN-001) reports the render device to its plugins, and
/// on a shared-device build that device is the toolkit's — so the accessors must
/// answer on a context they did not create. The pointer is also compared with
/// the one taken from the host's own handle, but read that comparison for what
/// it is: `MTLDevice` is a process-wide singleton on Apple Silicon, so equality
/// here says the two agree about the hardware, not that they are the same
/// `wgpu::Device`. Device identity is asserted by the two tests above.
///
/// Metal only: comparing a D3D12 pointer needs the COM binding this crate
/// deliberately does not depend on (see `interop.rs`).
#[cfg(target_os = "macos")]
#[test]
fn a_shared_context_reports_the_hosts_native_device() {
    let Some(host) = HostGpu::new(None) else {
        eprintln!("skipping a_shared_context_reports_the_hosts_native_device: no GPU adapter");
        return;
    };
    let ctx = host.share();

    let Some(expected_api) = interop::native_api(&ctx) else {
        eprintln!(
            "skipping a_shared_context_reports_the_hosts_native_device: backend {:?} has no \
             interop support (WGPU_BACKEND override?)",
            ctx.adapter_info().backend
        );
        return;
    };
    assert_eq!(expected_api, interop::NativeApi::Metal);

    // SAFETY: both pointers are compared only. Neither is dereferenced,
    // retained, or released, and `host`/`ctx` outlive the comparison.
    let shared = unsafe { interop::native_device(&ctx) }.expect("Metal device from the context");
    let host_ptr = unsafe {
        let hal = host
            .device
            .as_hal::<wgpu::hal::api::Metal>()
            .expect("Metal device from the host handle");
        core::ptr::from_ref(&**hal.raw_device())
            .cast::<core::ffi::c_void>()
            .cast_mut()
    };

    assert_eq!(
        shared.as_ptr(),
        host_ptr,
        "the context reports a different MTLDevice than the host created — the device was not \
         shared"
    );
}

/// Reproduce the Metal device selection in GPUI's `MetalRenderer::create_device`.
///
/// GPUI prefers a non-removable, low-power device and falls back to the system
/// default only when enumeration is empty. Keeping this rule in the test makes
/// the native pair stand in for the fork's `Window::native_gpu_handles` result,
/// rather than accidentally testing `MTLCreateSystemDefaultDevice` instead.
#[cfg(target_os = "macos")]
fn gpui_metal_device() -> Option<Retained<ProtocolObject<dyn MTLDevice>>> {
    let devices = MTLCopyAllDevices().to_vec();
    devices
        .into_iter()
        .min_by_key(|device| (device.isRemovable(), !device.isLowPower()))
        .or_else(|| MTLCreateSystemDefaultDevice())
}

/// The fork's GPUI Metal accessor and the wgpu Metal backend must identify the
/// same physical device before the abstract API is exercised.
///
/// On a single-GPU Mac this identity is guaranteed. On a Mac with multiple
/// GPUs, GPUI's low-power preference and Ravel's HighPerformance preference
/// can select different devices; that is a normal configuration, so this test
/// reports the mismatch and skips. ZC-3 and later must add the explicit
/// multi-GPU device handoff and synchronization needed to cover that case.
#[cfg(target_os = "macos")]
#[test]
fn a_native_host_pair_matches_wgpu_and_runs_the_abstract_api() {
    let Some(native_device) = gpui_metal_device() else {
        eprintln!(
            "skipping a_native_host_pair_matches_wgpu_and_runs_the_abstract_api: no Metal device"
        );
        return;
    };
    let Some(native_queue) = native_device.newCommandQueue() else {
        eprintln!(
            "skipping a_native_host_pair_matches_wgpu_and_runs_the_abstract_api: no Metal queue"
        );
        return;
    };
    let Some(host) = HostGpu::new(None) else {
        eprintln!(
            "skipping a_native_host_pair_matches_wgpu_and_runs_the_abstract_api: no GPU adapter"
        );
        return;
    };
    if host.adapter.get_info().backend != wgpu::Backend::Metal {
        eprintln!(
            "skipping a_native_host_pair_matches_wgpu_and_runs_the_abstract_api: backend {:?} is not Metal",
            host.adapter.get_info().backend
        );
        return;
    }

    let native_device_ptr = Retained::as_ptr(&native_device).cast_mut().cast();
    let native_queue_ptr = Retained::as_ptr(&native_queue).cast_mut().cast();

    let host_ctx = host.share();
    // SAFETY: `host_ctx` keeps the wgpu Metal device alive while the borrowed
    // pointer is compared; no native object is retained or released.
    let Some(wgpu_device) = (unsafe { interop::native_device(&host_ctx) }) else {
        eprintln!(
            "skipping a_native_host_pair_matches_wgpu_and_runs_the_abstract_api: wgpu did not \
             expose a Metal device"
        );
        return;
    };
    if wgpu_device.as_ptr() != native_device_ptr {
        eprintln!(
            "skipping a_native_host_pair_matches_wgpu_and_runs_the_abstract_api: GPUI's \
             low-power device ({native_device_ptr:p}) differs from wgpu HighPerformance \
             device ({:?}); multi-GPU handoff is deferred to ZC-3+",
            wgpu_device.as_ptr()
        );
        return;
    }

    // SAFETY: both retained Objective-C objects remain alive through every
    // assertion below; the import only borrows their non-null pointers and
    // builds its own wgpu instance to match them against.
    let native = pollster::block_on(unsafe {
        interop::context_from_native(
            interop::NativeApi::Metal,
            native_device_ptr,
            native_queue_ptr,
        )
    });
    let Some(native) = native else {
        eprintln!(
            "skipping a_native_host_pair_matches_wgpu_and_runs_the_abstract_api: no wgpu \
             Metal adapter/device matched GPUI's selected device"
        );
        return;
    };

    assert_eq!(native.api(), interop::NativeApi::Metal);
    assert_eq!(native.device().as_ptr(), native_device_ptr);
    assert_eq!(native.command_queue().as_ptr(), native_queue_ptr);

    // The import itself selected the matching adapter/device. This is the
    // identity proof that the context used below runs on GPUI's device.
    let ctx = native.gpu_context().clone();
    // SAFETY: `ctx` is alive for the comparison and the pointer is not used as
    // an owning handle.
    let imported_device = unsafe { interop::native_device(&ctx) }
        .expect("the imported wgpu context exposes its Metal device");
    assert_eq!(
        native.device().as_ptr(),
        imported_device.as_ptr(),
        "the imported wgpu context must use GPUI's selected Metal device"
    );

    // The preceding identity proof is what lets the abstract Ravel API run on
    // the same physical Metal device as the GPUI renderer. Exercise that API
    // on the context returned by `context_from_native`, not on a separately
    // constructed host context.
    assert_eq!(interop::native_api(&ctx), Some(interop::NativeApi::Metal));
    assert_abstract_api_runs(ctx);
}
