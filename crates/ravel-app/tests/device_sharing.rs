// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The host side of the device-sharing contract (`GPUBK-9`, REQ-GPU-001).
//!
//! Sharing one device between UI rendering and compute needs two halves. The
//! `ravel-gpu` half — a context built on someone else's device is a first-class
//! context — is pinned by `crates/ravel-gpu/tests/device_sharing.rs`. This file
//! pins the half that lives here: **the application host may name the entry
//! points at all.**
//!
//! That is not a tautology. `GpuContext::from_handles` is `pub(crate)`, the
//! entry points are reachable only as `ravel_gpu::interop::*`, and until
//! `GPUBK-9` the lint (`scripts/lint-patterns.sh`) rejected any mention of
//! `interop` from this crate — the same rule that keeps backend-native handles
//! out of the UI. `GPUBK-9` split that rule in two, because receiving the
//! toolkit's device is not the escape handing a pointer out is, and this crate
//! is the one place allowed to make that call. The test fails to compile if the
//! entry points are made `pub(crate)`, renamed, or moved, and
//! `mise run lint:patterns` fails if the split is reverted — the occurrences
//! below are what exercises the allowance.
//!
//! It deliberately does not wire GPUI's device: gpui publishes no accessor for
//! the device its renderer uses, and on macOS that renderer is Metal-native
//! rather than wgpu-backed. Closing that gap is a patch on the `gpui-ce-ravel`
//! fork, whose policy is in `docs/specifications/architecture.md`.

use ravel_gpu::GpuContext;

#[test]
fn the_host_can_name_the_device_sharing_entry_points() {
    // Item bindings rather than calls: constructing a `wgpu::Instance` here
    // would mean naming `wgpu` in the UI crate, which is exactly what the GPU
    // façade forbids. What has to hold is that the host *can* reach the entry
    // points — the behaviour on the other side of them is `ravel-gpu`'s test.
    let _accepts_the_toolkits_device = ravel_gpu::interop::context_from_wgpu;
    let _exposes_the_shared_instance = ravel_gpu::interop::wgpu_instance;
}

#[test]
fn the_host_can_read_the_instance_a_surface_must_be_created_on() {
    // A toolkit that shares its device also has to create its surfaces on the
    // same instance, so the host must be able to ask Ravel for it. Skips
    // without an adapter, like the other GPU tests.
    let Some(ctx) = GpuContext::new_blocking().ok() else {
        eprintln!(
            "skipping the_host_can_read_the_instance_a_surface_must_be_created_on: no GPU adapter"
        );
        return;
    };

    // Bound without naming its type: `ravel-app` must not depend on wgpu.
    let _instance = ravel_gpu::interop::wgpu_instance(&ctx);
}
