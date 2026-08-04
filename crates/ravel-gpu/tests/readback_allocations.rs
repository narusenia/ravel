// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! How many full-frame CPU buffers a readback allocates.
//!
//! "The double copy is gone" (`issues/high/HIGH-04`) is not directly
//! observable — nothing counts memcpys. What *is* observable is that each copy
//! needs somewhere to land, so counting frame-sized heap allocations counts the
//! copies. This test binary installs a global allocator that tallies
//! allocations at least half a frame large and asserts that
//! `GpuFrameBuffer::to_frame_buffer` performs exactly **one**: the frame's own
//! `Arc<[u8]>`.
//!
//! For the record, the route this replaced allocated three — the readback's
//! `Vec<u8>`, the `Vec<f32>` from `bytemuck::cast_slice(&raw).to_vec()`, and
//! the `Arc<[u8]>` that `FrameBuffer::from_f32` copied it into. Reintroducing
//! any of them fails this test.
//!
//! The staging buffer is not part of the count: it is pooled, so the warm-up
//! readback below is the only one that allocates it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ravel_gpu::{
    GpuContext, GpuFrameBuffer, TextureFormat, TextureKey, TexturePool, TextureUsage,
    upload_texture,
};

thread_local! {
    /// Whether this thread is inside a measured region.
    static ARMED: Cell<bool> = const { Cell::new(false) };
    /// Allocations of at least `THRESHOLD` bytes seen while armed.
    static COUNT: Cell<usize> = const { Cell::new(0) };
    /// Byte size an allocation must reach to be counted.
    static THRESHOLD: Cell<usize> = const { Cell::new(usize::MAX) };
}

/// Counts large allocations made by the arming thread.
///
/// Per-thread rather than process-wide: the two tests below run in parallel and
/// each does its own warm-up, so a global tally would count the other test's
/// buffers. All the work being measured is synchronous, including the map
/// callback, which fires inside the polling call on this same thread.
struct CountingAllocator;

impl CountingAllocator {
    fn record(size: usize) {
        if ARMED.try_with(|armed| armed.get()) != Ok(true) {
            return;
        }
        if THRESHOLD.try_with(|t| size >= t.get()) == Ok(true) {
            let _ = COUNT.try_with(|count| count.set(count.get() + 1));
        }
    }
}

// SAFETY: every method forwards to `System` with its arguments unchanged, so
// the `GlobalAlloc` contract is whatever `System` already upholds. The only
// added code is `record`, which must not allocate — a global allocator that
// allocates inside `alloc` recurses until the stack dies. It cannot: the three
// pieces of state are `Cell`s in `thread_local!`s with `const` initializers
// (no lazy heap-allocated box), and they are reached through `try_with`, which
// returns an error instead of running TLS initialization or panicking while
// TLS is being destroyed. Keep it that way — formatting, collecting, or
// locking in `record` would reintroduce the recursion.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // A growing `Vec` reallocates rather than allocating, so a copy built
        // that way has to count too.
        Self::record(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Counts allocations of at least `threshold` bytes performed by `body`.
fn count_large_allocations(threshold: usize, body: impl FnOnce()) -> usize {
    THRESHOLD.with(|t| t.set(threshold));
    COUNT.with(|count| count.set(0));
    ARMED.with(|armed| armed.set(true));
    body();
    ARMED.with(|armed| armed.set(false));
    COUNT.with(|count| count.get())
}

const WIDTH: u32 = 256;
const HEIGHT: u32 = 256;
/// Bytes one frame of pixels occupies (RGBA32F).
const FRAME_BYTES: usize = (WIDTH * HEIGHT) as usize * 16;

fn frame() -> Option<GpuFrameBuffer> {
    let ctx = GpuContext::new_blocking().ok()?;
    let pool = std::sync::Arc::new(std::sync::Mutex::new(TexturePool::new(
        ctx.clone(),
        64 * 1024 * 1024,
    )));
    let key = TextureKey::new(
        WIDTH,
        HEIGHT,
        TextureFormat::Rgba32Float,
        TextureUsage::TEXTURE_BINDING | TextureUsage::COPY_SRC | TextureUsage::COPY_DST,
    );
    let texture = pool.lock().unwrap().acquire(key);
    let pixels = vec![0.5f32; (WIDTH * HEIGHT * 4) as usize];
    upload_texture(&ctx, &texture.texture, key, bytemuck::cast_slice(&pixels));
    Some(GpuFrameBuffer::new(ctx, &pool, texture, WIDTH, HEIGHT))
}

#[test]
fn a_frame_readback_allocates_one_full_frame_buffer() {
    let Some(frame) = frame() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    // Warm the staging pool and any lazily-built driver state.
    let warm = frame.to_frame_buffer().expect("readback");
    assert_eq!(warm.data.len(), FRAME_BYTES);

    let allocations = count_large_allocations(FRAME_BYTES / 2, || {
        let fb = frame.to_frame_buffer().expect("readback");
        std::hint::black_box(fb.data.len());
    });
    assert_eq!(
        allocations, 1,
        "the readback must land straight in the frame's shared buffer: \
         one frame-sized allocation, not one per copy"
    );
}

#[test]
fn a_frame_readback_costs_no_more_than_the_raw_byte_readback() {
    let Some(frame) = frame() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let key = TextureKey::new(
        WIDTH,
        HEIGHT,
        TextureFormat::Rgba32Float,
        TextureUsage::TEXTURE_BINDING | TextureUsage::COPY_SRC | TextureUsage::COPY_DST,
    );
    let _ = frame.to_frame_buffer().expect("readback");

    // `read_texture` is the floor: one buffer for the bytes themselves.
    let raw = count_large_allocations(FRAME_BYTES / 2, || {
        let bytes = ravel_gpu::read_texture(frame.context(), frame.texture(), key).expect("raw");
        std::hint::black_box(bytes.len());
    });
    let framed = count_large_allocations(FRAME_BYTES / 2, || {
        let fb = frame.to_frame_buffer().expect("readback");
        std::hint::black_box(fb.data.len());
    });
    assert_eq!(
        framed, raw,
        "wrapping the readback in a FrameBuffer must not cost an extra full-frame copy"
    );
}
