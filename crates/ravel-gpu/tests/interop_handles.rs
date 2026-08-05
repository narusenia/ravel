// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Backend-native handle extraction (`ravel_gpu::interop`, `GPUBK-8`).
//!
//! The interesting property is not "a pointer came back" but "the pointer that
//! came back names the object Ravel is actually using". Nothing here can
//! dereference it — that needs the platform SDK, which this crate deliberately
//! does not depend on — so the assertions are the ones reachable without one:
//! the handle exists exactly when the backend says it should, it is non-null,
//! it is stable for a given object, and two distinct textures never share one.
//!
//! Skips gracefully when no GPU adapter is available, like the other GPU tests
//! here. On a backend with no interop support (Vulkan, GL, software) the
//! accessors must return `None` rather than a bogus pointer, and that is
//! asserted too — which is what makes this test meaningful on a Linux runner.

use std::sync::{Arc, Mutex};

use ravel_core::types::FrameBuffer;
use ravel_gpu::interop;
use ravel_gpu::{GpuContext, GpuFrameBuffer, TexturePool};

fn try_context() -> Option<GpuContext> {
    GpuContext::new_blocking().ok()
}

fn frame(
    ctx: &GpuContext,
    pool: &Arc<Mutex<TexturePool>>,
    width: u32,
    height: u32,
) -> GpuFrameBuffer {
    let fb = FrameBuffer::new_zeroed(width, height);
    GpuFrameBuffer::from_frame_buffer(ctx.clone(), pool, &fb).expect("upload frame")
}

fn pool(ctx: &GpuContext) -> Arc<Mutex<TexturePool>> {
    Arc::new(Mutex::new(TexturePool::new(ctx.clone(), 64 * 1024 * 1024)))
}

#[test]
fn device_handle_matches_the_declared_backend() {
    let Some(ctx) = try_context() else {
        eprintln!("skipping device_handle_matches_the_declared_backend: no GPU adapter available");
        return;
    };

    let expected = interop::native_api(&ctx);
    // SAFETY: the handle is only inspected, never dereferenced or released,
    // and `ctx` outlives it.
    let device = unsafe { interop::native_device(&ctx) };

    assert_eq!(
        device.map(|d| d.api()),
        expected,
        "native_api must predict exactly when a device handle is available"
    );

    match device {
        Some(device) => {
            assert!(!device.as_ptr().is_null());
            assert_eq!(device.handle().api(), device.api());
        }
        None => assert!(
            expected.is_none(),
            "no device handle on a backend that claims interop support"
        ),
    }
}

#[test]
fn device_handle_is_stable_across_calls() {
    let Some(ctx) = try_context() else {
        eprintln!("skipping device_handle_is_stable_across_calls: no GPU adapter available");
        return;
    };
    // SAFETY: as above.
    let (a, b) = unsafe { (interop::native_device(&ctx), interop::native_device(&ctx)) };
    assert_eq!(
        a.map(|d| d.as_ptr()),
        b.map(|d| d.as_ptr()),
        "one context has one native device"
    );

    // A clone shares the same device: the OFX host may hold its own clone.
    let clone = ctx.clone();
    // SAFETY: as above.
    let c = unsafe { interop::native_device(&clone) };
    assert_eq!(a.map(|d| d.as_ptr()), c.map(|d| d.as_ptr()));
}

#[test]
fn texture_handle_is_non_null_and_per_texture() {
    let Some(ctx) = try_context() else {
        eprintln!("skipping texture_handle_is_non_null_and_per_texture: no GPU adapter available");
        return;
    };
    let expected = interop::native_api(&ctx);
    let pool = pool(&ctx);

    // Different sizes, so the pool cannot hand out one texture for both.
    let first = frame(&ctx, &pool, 8, 8);
    let second = frame(&ctx, &pool, 16, 16);

    // SAFETY: both frames outlive the handles, which are only inspected.
    let (a, b) = unsafe {
        (
            interop::native_texture(&first),
            interop::native_texture(&second),
        )
    };

    assert_eq!(a.map(|t| t.api()), expected);
    assert_eq!(b.map(|t| t.api()), expected);

    let (Some(a), Some(b)) = (a, b) else {
        assert!(
            expected.is_none(),
            "no texture handle on a backend that claims interop support"
        );
        return;
    };

    assert!(!a.as_ptr().is_null());
    assert!(!b.as_ptr().is_null());
    assert_ne!(
        a.as_ptr(),
        b.as_ptr(),
        "two live frames must name two textures"
    );

    // The same frame yields the same texture every time.
    // SAFETY: as above.
    let again = unsafe { interop::native_texture(&first) }.expect("still available");
    assert_eq!(a.as_ptr(), again.as_ptr());

    // A clone of the frame is the same texture, not a copy.
    let cloned = first.clone();
    // SAFETY: as above.
    let from_clone = unsafe { interop::native_texture(&cloned) }.expect("still available");
    assert_eq!(a.as_ptr(), from_clone.as_ptr());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_reports_metal() {
    use ravel_gpu::interop::NativeApi;

    let Some(ctx) = try_context() else {
        eprintln!("skipping macos_reports_metal: no GPU adapter available");
        return;
    };
    // The default backend selection on macOS is Metal; a run that overrides
    // `WGPU_BACKEND` is out of scope for this assertion.
    if interop::native_api(&ctx) != Some(NativeApi::Metal) {
        eprintln!("skipping macos_reports_metal: context is not on the Metal backend");
        return;
    }

    let pool = pool(&ctx);
    let gpu_frame = frame(&ctx, &pool, 4, 4);
    // SAFETY: `ctx` and `gpu_frame` outlive the handles, only inspected here.
    let (device, texture) = unsafe {
        (
            interop::native_device(&ctx).expect("MTLDevice"),
            interop::native_texture(&gpu_frame).expect("MTLTexture"),
        )
    };

    assert_eq!(device.api(), NativeApi::Metal);
    assert_eq!(texture.api(), NativeApi::Metal);
    assert!(!device.as_ptr().is_null());
    assert!(!texture.as_ptr().is_null());
    assert_ne!(device.as_ptr(), texture.as_ptr());
}
