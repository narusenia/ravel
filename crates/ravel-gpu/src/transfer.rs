// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPU <-> CPU data transfer utilities.
//!
//! * CPU -> GPU: [`upload_texture`] uploads tightly-packed pixel data via
//!   `Queue::write_texture`.
//! * GPU -> CPU: [`read_texture`] / [`read_texture_shared`] copy a texture into
//!   a pooled mappable buffer, wait for *that* copy, and return tightly-packed
//!   pixel data (row padding removed). [`begin_read_texture`] is the same
//!   readback without the wait, for a caller that wants to do something else
//!   while the copy is in flight.
//!
//! `copy_texture_to_buffer` requires each row to be aligned to
//! [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`] (256) bytes; [`padded_bytes_per_row`]
//! computes the padded stride and the readback path strips it again.
//!
//! ## Where the readback's cost went (`issues/high/HIGH-04`)
//!
//! Three things used to make a displayed frame more expensive than the copy
//! it performs, and all three are gone from this module:
//!
//! * **A staging buffer per readback.** The mappable buffer now comes from a
//!   pool keyed by byte size (`staging.rs`), so a
//!   steady stream of same-resolution frames allocates nothing
//!   ([`stats::TransferSnapshot::staging_buffers_created`] stays flat).
//! * **A device-wide wait.** The readback waited for *everything* the device
//!   had been asked to do, which also force-submitted unrelated batched
//!   dispatches. [`PendingReadback`] waits for its own submission index only.
//! * **A second CPU copy.** The bytes used to be rebuilt as a `Vec<f32>` and
//!   then copied again into the frame's `Arc<[u8]>`.
//!   [`read_texture_shared`] hands the caller the shared buffer directly, and
//!   builds it in a single copy when the rows need no de-padding.

use std::sync::Arc;

use crate::device::GpuContext;
use crate::error::{GpuError, GpuResult};
use crate::texture_desc::TextureUsage;
use crate::texture_pool::TextureKey;

/// Per-[`GpuContext`] CPU↔GPU transfer counters.
///
/// Every [`upload_texture`] / [`read_texture`] call is recorded on the
/// context it went through (see `GpuContext::transfer_stats`), so tests
/// and benchmarks can assert how many round trips a pipeline performs
/// without interference from concurrent tests using their own contexts.
pub mod stats {
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Live counters owned by a `GpuContext`.
    #[derive(Default)]
    pub struct TransferCounters {
        uploads: AtomicU64,
        readbacks: AtomicU64,
        upload_bytes: AtomicU64,
        readback_bytes: AtomicU64,
        staging_buffers_created: AtomicU64,
    }

    impl TransferCounters {
        pub(crate) fn record_upload(&self, bytes: u64) {
            self.uploads.fetch_add(1, Ordering::Relaxed);
            self.upload_bytes.fetch_add(bytes, Ordering::Relaxed);
        }

        pub(crate) fn record_readback(&self, bytes: u64) {
            self.readbacks.fetch_add(1, Ordering::Relaxed);
            self.readback_bytes.fetch_add(bytes, Ordering::Relaxed);
        }

        pub(crate) fn record_staging_buffer_created(&self) {
            self.staging_buffers_created.fetch_add(1, Ordering::Relaxed);
        }

        /// Read the current counter values.
        pub fn snapshot(&self) -> TransferSnapshot {
            TransferSnapshot {
                uploads: self.uploads.load(Ordering::Relaxed),
                readbacks: self.readbacks.load(Ordering::Relaxed),
                upload_bytes: self.upload_bytes.load(Ordering::Relaxed),
                readback_bytes: self.readback_bytes.load(Ordering::Relaxed),
                staging_buffers_created: self.staging_buffers_created.load(Ordering::Relaxed),
            }
        }
    }

    /// Immutable view of the transfer counters at one point in time.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct TransferSnapshot {
        pub uploads: u64,
        pub readbacks: u64,
        pub upload_bytes: u64,
        pub readback_bytes: u64,
        /// Readback staging buffers the context allocated.
        ///
        /// Counts *creations*, not uses: with the staging pool doing its job
        /// this stops growing once each readback size has been seen once, which
        /// is what `HIGH-04` asks to be checkable — allocations must not scale
        /// with the frame count.
        pub staging_buffers_created: u64,
    }

    impl TransferSnapshot {
        /// Counter increments between `self` (earlier) and `later`.
        pub fn delta(&self, later: &TransferSnapshot) -> TransferSnapshot {
            TransferSnapshot {
                uploads: later.uploads.wrapping_sub(self.uploads),
                readbacks: later.readbacks.wrapping_sub(self.readbacks),
                upload_bytes: later.upload_bytes.wrapping_sub(self.upload_bytes),
                readback_bytes: later.readback_bytes.wrapping_sub(self.readback_bytes),
                staging_buffers_created: later
                    .staging_buffers_created
                    .wrapping_sub(self.staging_buffers_created),
            }
        }
    }
}

/// Round `unpadded` up to the next multiple of `align`.
#[inline]
pub const fn align_up(unpadded: u32, align: u32) -> u32 {
    if align == 0 {
        unpadded
    } else {
        unpadded.div_ceil(align) * align
    }
}

/// Bytes-per-row padded to the copy alignment required by
/// `copy_texture_to_buffer`.
#[inline]
pub fn padded_bytes_per_row(width: u32, bytes_per_pixel: u32) -> u32 {
    let unpadded = width
        .checked_mul(bytes_per_pixel)
        .expect("row byte count overflows u32");
    align_up(unpadded, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

fn readback_capacity(bytes_per_row: u32, height: u32) -> GpuResult<usize> {
    let bytes = u64::from(bytes_per_row) * u64::from(height);
    usize::try_from(bytes)
        .map_err(|_| GpuError::Readback(format!("readback size {bytes} does not fit in usize")))
}

/// Upload tightly-packed pixel `data` into `texture`.
///
/// `data` must contain exactly `width * height * bytes_per_pixel` bytes for the
/// texture's key. The key's usage must include [`TextureUsage::COPY_DST`].
pub fn upload_texture(ctx: &GpuContext, texture: &wgpu::Texture, key: TextureKey, data: &[u8]) {
    debug_assert!(
        key.usage.contains(TextureUsage::COPY_DST),
        "upload target must be declared COPY_DST"
    );
    let span = tracing::debug_span!(
        "gpu_upload",
        width = key.width,
        height = key.height,
        bytes = data.len()
    );
    let _guard = span.enter();
    // `write_texture` executes before the next submit — which may be the
    // batched dispatch encoder. If that batch still reads or writes this
    // texture, flush it first or its stale commands would land on top of the
    // fresh upload.
    ctx.flush_for_upload(texture);
    ctx.transfer_counters().record_upload(data.len() as u64);
    let bpp = key.format.bytes_per_pixel();
    let bytes_per_row = key
        .width
        .checked_mul(bpp)
        .expect("row byte count overflows u32");
    ctx.queue().write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(key.height),
        },
        wgpu::Extent3d {
            width: key.width,
            height: key.height,
            depth_or_array_layers: 1,
        },
    );
}

/// Read `texture` back into tightly-packed CPU memory (row padding removed).
///
/// Blocks until this readback's GPU copy completes — not until the device is
/// idle. The key's usage must include [`TextureUsage::COPY_SRC`].
pub fn read_texture(
    ctx: &GpuContext,
    texture: &wgpu::Texture,
    key: TextureKey,
) -> GpuResult<Vec<u8>> {
    let span = tracing::debug_span!("gpu_readback", width = key.width, height = key.height);
    let _guard = span.enter();
    begin_read_texture(ctx, texture, key)?.wait_into_vec()
}

/// Read `texture` back into a shared, tightly-packed byte buffer.
///
/// The same readback as [`read_texture`], landing in an [`Arc`] instead of a
/// `Vec`. [`FrameBuffer`](ravel_core::types::FrameBuffer) stores its pixels as
/// `Arc<[u8]>`, so a `Vec` on the way there would be copied once more; when the
/// texture's rows need no de-padding this reaches the shared buffer in a single
/// copy out of the mapped range.
pub fn read_texture_shared(
    ctx: &GpuContext,
    texture: &wgpu::Texture,
    key: TextureKey,
) -> GpuResult<Arc<[u8]>> {
    let span = tracing::debug_span!("gpu_readback", width = key.width, height = key.height);
    let _guard = span.enter();
    begin_read_texture(ctx, texture, key)?.wait_shared()
}

/// Submit a readback of `texture` without waiting for it.
///
/// The copy is on its way to a pooled staging buffer by the time this returns;
/// the returned [`PendingReadback`] is how the caller finds out when the bytes
/// are readable. The key's usage must include [`TextureUsage::COPY_SRC`].
///
/// Unlike [`read_texture`] this records no `gpu_readback` span: the operation
/// outlives the call, so a span here would time the submission rather than the
/// readback.
pub fn begin_read_texture(
    ctx: &GpuContext,
    texture: &wgpu::Texture,
    key: TextureKey,
) -> GpuResult<PendingReadback> {
    debug_assert!(
        key.usage.contains(TextureUsage::COPY_SRC),
        "readback source must be declared COPY_SRC"
    );
    // The copy below is submitted on its own encoder, ahead of any batched
    // dispatches. If the batch still writes this texture, flush it first so
    // the copy sees the batch's output rather than stale contents.
    ctx.flush_for_readback(texture);
    let bpp = key.format.bytes_per_pixel();
    ctx.transfer_counters()
        .record_readback(key.width as u64 * key.height as u64 * bpp as u64);
    let unpadded_bpr = key
        .width
        .checked_mul(bpp)
        .expect("row byte count overflows u32");
    let padded_bpr = padded_bytes_per_row(key.width, bpp);
    let buffer_size = padded_bpr as u64 * key.height as u64;
    // Fail before touching the GPU when the result could not be addressed on
    // this target anyway.
    let capacity = readback_capacity(unpadded_bpr, key.height)?;

    let lease = ctx.acquire_staging(buffer_size);

    let mut encoder = ctx
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ravel readback"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: lease.buffer(),
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: Some(key.height),
            },
        },
        wgpu::Extent3d {
            width: key.width,
            height: key.height,
            depth_or_array_layers: 1,
        },
    );
    let submission = ctx.queue().submit(Some(encoder.finish()));

    let (tx, rx) = std::sync::mpsc::channel();
    lease
        .buffer()
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });

    Ok(PendingReadback {
        ctx: ctx.clone(),
        lease: Some(lease),
        submission,
        map: rx,
        mapped: false,
        capacity,
        padded_bytes_per_row: padded_bpr,
        bytes_per_row: unpadded_bpr,
        rows: key.height,
    })
}

/// A submitted readback whose copy may still be in flight.
///
/// This is the crate's backend-agnostic answer to "the bytes are not here
/// yet": the caller sees a completion state — [`Self::wait_timeout`] bounded,
/// [`Self::is_complete`] non-blocking — and two ways to take the result, never a
/// `map_async` callback or a mapping mode. That is what makes it usable as the
/// shape a genuinely asynchronous readback would keep (`GPUCOMP-10`) — deciding
/// whether to overlap frame N's readback with frame N+1's evaluation is then a
/// scheduling question, not an API change.
///
/// Every wait here is scoped to this readback's own submission index, so none of
/// them ever blocks on unrelated GPU work.
///
/// The staging buffer returns to the pool when the result is taken. **A pending
/// readback dropped without taking its result drops its staging buffer
/// instead**: the mapping may still be outstanding, and handing such a buffer
/// back would make the next borrower's `map_async` panic. Every production path
/// takes the result, so this only costs an allocation on an abandoned readback.
pub struct PendingReadback {
    ctx: GpuContext,
    /// Held until the result is taken or the readback is abandoned.
    lease: Option<crate::staging::StagingLease>,
    /// The copy's submission — the only GPU work this readback waits for.
    submission: wgpu::SubmissionIndex,
    map: std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    mapped: bool,
    /// Tightly-packed byte length of the result.
    capacity: usize,
    padded_bytes_per_row: u32,
    bytes_per_row: u32,
    rows: u32,
}

impl PendingReadback {
    /// Tightly-packed byte length the result will have.
    pub fn len(&self) -> usize {
        self.capacity
    }

    /// Whether the readback carries no bytes (a zero-sized texture).
    pub fn is_empty(&self) -> bool {
        self.capacity == 0
    }

    /// Whether the bytes are readable, without blocking.
    ///
    /// [`Self::wait_timeout`] with a zero timeout. It is a real device query,
    /// not a free flag read, so a caller that only wants the bytes should ask
    /// for them ([`Self::wait_into_vec`] / [`Self::wait_shared`]) rather than
    /// spin here.
    pub fn is_complete(&mut self) -> GpuResult<bool> {
        self.wait_timeout(std::time::Duration::ZERO)
    }

    /// Wait up to `timeout` for the copy to land, reporting whether it did.
    ///
    /// `Ok(false)` means the copy is still in flight and the call may be
    /// repeated. The wait is scoped to this readback's own submission, so it
    /// never blocks on unrelated GPU work, and a wait that ends in a timeout
    /// still lets the device process everything that has finished — including
    /// this copy the moment it completes.
    ///
    /// Prefer a bounded wait over spinning on [`Self::is_complete`]: readback
    /// latency belongs to the adapter (a virtualized or software device is
    /// orders of magnitude slower than a desktop GPU), so a spin bounded by an
    /// iteration count is a timing assumption in disguise.
    pub fn wait_timeout(&mut self, timeout: std::time::Duration) -> GpuResult<bool> {
        if self.mapped {
            return Ok(true);
        }
        self.ctx
            .wait_for_submission(&self.submission, Some(timeout))?;
        // The wait's own answer is not the signal: the map callback is, and it
        // fires from inside the poll above once the copy's submission is
        // retired. Read the channel rather than the wait result.
        self.take_map_result()
    }

    /// Consume the map callback's result if it has fired.
    fn take_map_result(&mut self) -> GpuResult<bool> {
        match self.map.try_recv() {
            Ok(result) => {
                result.map_err(|e| GpuError::Readback(e.to_string()))?;
                self.mapped = true;
                Ok(true)
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(false),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(GpuError::Readback("map callback dropped".to_string()))
            }
        }
    }

    /// Block until the copy lands, then return the bytes in a `Vec`.
    pub fn wait_into_vec(mut self) -> GpuResult<Vec<u8>> {
        self.finish()?;
        let mut out = Vec::with_capacity(self.capacity);
        {
            let lease = self.lease.as_ref().expect("held until the result is taken");
            let view = lease.buffer().slice(..).get_mapped_range();
            self.copy_rows(&view, |row| out.extend_from_slice(row));
        }
        self.recycle();
        Ok(out)
    }

    /// Block until the copy lands, then return the bytes in a shared buffer.
    ///
    /// When the texture's rows are already tightly packed — every RGBA32F width
    /// that is a multiple of 16 px, which includes 1080p and 4K — the mapped
    /// range goes straight into the `Arc` allocation, so the readback costs one
    /// CPU copy in total.
    pub fn wait_shared(mut self) -> GpuResult<Arc<[u8]>> {
        self.finish()?;
        let bytes: Arc<[u8]> = {
            let lease = self.lease.as_ref().expect("held until the result is taken");
            let view = lease.buffer().slice(..).get_mapped_range();
            if self.padded_bytes_per_row == self.bytes_per_row {
                Arc::from(&view[..self.capacity])
            } else {
                let mut out = Vec::with_capacity(self.capacity);
                self.copy_rows(&view, |row| out.extend_from_slice(row));
                out.into()
            }
        };
        self.recycle();
        Ok(bytes)
    }

    /// Hand each row of `view` to `sink` with its padding removed.
    fn copy_rows(&self, view: &[u8], mut sink: impl FnMut(&[u8])) {
        for row in 0..self.rows as usize {
            let start = row * self.padded_bytes_per_row as usize;
            sink(&view[start..start + self.bytes_per_row as usize]);
        }
    }

    /// Block until the mapping is readable.
    fn finish(&mut self) -> GpuResult<()> {
        if self.mapped {
            return Ok(());
        }
        // The copy is this staging buffer's only use, so waiting for its
        // submission is what the readback actually depends on. `GpuContext::wait`
        // would instead submit and wait for every dispatch the context has
        // batched, whether or not this frame needs it (`HIGH-04`).
        //
        // An unbounded wait that returns leaves nothing to retry: wgpu retires
        // every completed submission and fires their map callbacks inside the
        // same call, so the channel carries the result by the time this
        // returns. An empty channel here would mean wgpu reported a completed
        // submission without mapping the buffer it was asked to map — report it
        // instead of silently falling back to a device-wide wait, which is the
        // cost this unit removed.
        self.ctx.wait_for_submission(&self.submission, None)?;
        if !self.take_map_result()? {
            return Err(GpuError::Readback(
                "the readback's submission completed but its buffer was not mapped".to_string(),
            ));
        }
        Ok(())
    }

    /// Unmap the staging buffer and return it to the pool.
    fn recycle(&mut self) {
        if let Some(lease) = self.lease.take() {
            lease.buffer().unmap();
            self.ctx.release_staging(lease);
        }
    }
}

impl Drop for PendingReadback {
    fn drop(&mut self) {
        // Abandoned without taking the result: the mapping may still be
        // outstanding, so the buffer goes away with the lease rather than back
        // into the pool (see the type's documentation).
        if self.lease.take().is_some() {
            log::debug!("readback abandoned before completion; staging buffer discarded");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_up_rounds_to_multiple() {
        assert_eq!(align_up(0, 256), 0);
        assert_eq!(align_up(1, 256), 256);
        assert_eq!(align_up(256, 256), 256);
        assert_eq!(align_up(257, 256), 512);
    }

    #[test]
    fn align_up_zero_alignment_is_identity() {
        assert_eq!(align_up(123, 0), 123);
    }

    #[test]
    fn padded_bytes_per_row_aligns_to_256() {
        // 10 px * 16 bytes (rgba32f) = 160 -> padded to 256.
        assert_eq!(padded_bytes_per_row(10, 16), 256);
        // 16 px * 16 bytes = 256 -> already aligned.
        assert_eq!(padded_bytes_per_row(16, 16), 256);
        // 17 px * 16 bytes = 272 -> padded to 512.
        assert_eq!(padded_bytes_per_row(17, 16), 512);
        // 64 px * 4 bytes (rgba8) = 256 -> aligned.
        assert_eq!(padded_bytes_per_row(64, 4), 256);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn rgba32f_max_texture_capacity_does_not_wrap_at_u32() {
        assert_eq!(readback_capacity(16384 * 16, 16384).unwrap(), 1usize << 32);
    }

    // --- GPU-dependent: skipped without an adapter -------------------------

    use crate::texture_desc::TextureFormat;
    use crate::texture_pool::TexturePool;

    fn try_context() -> Option<GpuContext> {
        GpuContext::new_blocking().ok()
    }

    fn readable_key(width: u32, height: u32) -> TextureKey {
        TextureKey::new(
            width,
            height,
            TextureFormat::Rgba32Float,
            TextureUsage::TEXTURE_BINDING | TextureUsage::COPY_SRC | TextureUsage::COPY_DST,
        )
    }

    /// The completion criterion of `HIGH-04`: a readback allocates no staging
    /// buffer of its own once the size has been seen, so allocations do not
    /// scale with the frame count.
    #[test]
    fn repeated_readbacks_allocate_no_staging_buffer() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut pool = TexturePool::new(ctx.clone(), 64 * 1024 * 1024);
        let key = readable_key(64, 64);
        let texture = pool.acquire(key);
        let pixels = vec![0.5f32; 64 * 64 * 4];
        upload_texture(&ctx, &texture.texture, key, bytemuck::cast_slice(&pixels));

        // The first readback of a size is the one that allocates.
        let cold = ctx.transfer_stats();
        let first = read_texture(&ctx, &texture.texture, key).expect("readback");
        assert_eq!(
            cold.delta(&ctx.transfer_stats()).staging_buffers_created,
            1,
            "the first readback of a size creates its staging buffer"
        );

        let warm = ctx.transfer_stats();
        for _ in 0..16 {
            let bytes = read_texture(&ctx, &texture.texture, key).expect("readback");
            assert_eq!(bytes.len(), first.len());
        }
        let delta = warm.delta(&ctx.transfer_stats());
        assert_eq!(delta.readbacks, 16, "the readbacks really happened");
        assert_eq!(
            delta.staging_buffers_created, 0,
            "16 further readbacks must reuse the pooled staging buffer"
        );
    }

    /// The shared-buffer readback must agree with the `Vec` one byte for byte,
    /// including the padded case where it cannot take the single-copy path
    /// (5 px * 16 B = 80 B rows, padded to 256).
    #[test]
    fn shared_and_vec_readbacks_agree_with_and_without_row_padding() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut pool = TexturePool::new(ctx.clone(), 64 * 1024 * 1024);
        for (width, height) in [(5u32, 4u32), (16, 4)] {
            let key = readable_key(width, height);
            let texture = pool.acquire(key);
            let pixels: Vec<f32> = (0..(width * height * 4)).map(|i| i as f32 * 0.5).collect();
            upload_texture(&ctx, &texture.texture, key, bytemuck::cast_slice(&pixels));

            let owned = read_texture(&ctx, &texture.texture, key).expect("readback");
            let shared = read_texture_shared(&ctx, &texture.texture, key).expect("readback");
            assert_eq!(
                owned.len(),
                (width * height * 16) as usize,
                "{width}x{height}: row padding must be stripped"
            );
            assert_eq!(&owned[..], &shared[..], "{width}x{height}: routes disagree");
            assert_eq!(
                bytemuck::cast_slice::<u8, f32>(&shared),
                &pixels[..],
                "{width}x{height}: pixels survive the round trip"
            );
            pool.release(texture);
        }
    }

    /// A readback started and abandoned must not poison the pool: the next one
    /// still maps successfully (the discarded buffer is not handed back).
    #[test]
    fn an_abandoned_readback_does_not_poison_the_staging_pool() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut pool = TexturePool::new(ctx.clone(), 64 * 1024 * 1024);
        let key = readable_key(8, 8);
        let texture = pool.acquire(key);
        let pixels = vec![1.0f32; 8 * 8 * 4];
        upload_texture(&ctx, &texture.texture, key, bytemuck::cast_slice(&pixels));

        drop(begin_read_texture(&ctx, &texture.texture, key).expect("submit"));
        let bytes = read_texture(&ctx, &texture.texture, key).expect("readback");
        assert_eq!(bytes.len(), 8 * 8 * 16);
    }

    /// A submitted readback completes within a bounded wait, and the
    /// non-blocking check agrees once it has.
    ///
    /// The wait is bounded by **time**, not by a poll count. A poll count would
    /// assert how fast the adapter is relative to how fast this loop runs, and
    /// a virtualized or software device (CI runners) is slower than a desktop
    /// GPU by a margin no constant covers.
    #[test]
    fn a_pending_readback_completes_within_a_bounded_wait() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut pool = TexturePool::new(ctx.clone(), 64 * 1024 * 1024);
        let key = readable_key(32, 32);
        let texture = pool.acquire(key);
        let pixels = vec![0.25f32; 32 * 32 * 4];
        upload_texture(&ctx, &texture.texture, key, bytemuck::cast_slice(&pixels));

        let mut pending = begin_read_texture(&ctx, &texture.texture, key).expect("submit");
        assert_eq!(pending.len(), 32 * 32 * 16);
        assert!(!pending.is_empty());
        // The zero-timeout check must answer rather than fail. Whether it
        // answers "done" this early is a race with the GPU, so only that it
        // answers is asserted.
        pending
            .is_complete()
            .expect("a non-blocking check must not fail");

        assert!(
            pending
                .wait_timeout(std::time::Duration::from_secs(5))
                .expect("bounded wait"),
            "the readback did not complete within 5 s"
        );
        assert!(
            pending.is_complete().expect("state after completion"),
            "a completed readback must keep reporting completion"
        );

        let bytes = pending.wait_shared().expect("bytes");
        assert_eq!(bytemuck::cast_slice::<u8, f32>(&bytes), &pixels[..]);
    }

    /// A zero timeout is a valid "check now" on every backend: it must return a
    /// definite answer rather than erroring, and repeating it must converge.
    #[test]
    fn a_zero_timeout_wait_is_a_non_blocking_check() {
        let Some(ctx) = try_context() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let mut pool = TexturePool::new(ctx.clone(), 64 * 1024 * 1024);
        let key = readable_key(16, 16);
        let texture = pool.acquire(key);
        let pixels = vec![0.75f32; 16 * 16 * 4];
        upload_texture(&ctx, &texture.texture, key, bytemuck::cast_slice(&pixels));

        let mut pending = begin_read_texture(&ctx, &texture.texture, key).expect("submit");
        for _ in 0..8 {
            pending
                .wait_timeout(std::time::Duration::ZERO)
                .expect("a zero timeout is not an error");
        }
        // Whatever the zero-timeout checks reported, the unbounded wait still
        // has to produce the pixels — the checks must not consume the result.
        let bytes = pending.wait_shared().expect("bytes");
        assert_eq!(bytemuck::cast_slice::<u8, f32>(&bytes), &pixels[..]);
    }
}
