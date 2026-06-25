// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPU <-> CPU data transfer utilities.
//!
//! * CPU -> GPU: [`upload_texture`] uploads tightly-packed pixel data via
//!   `Queue::write_texture`.
//! * GPU -> CPU: [`read_texture`] copies a texture into a mappable buffer,
//!   waits for the copy, and returns tightly-packed pixel data (row padding
//!   removed).
//!
//! `copy_texture_to_buffer` requires each row to be aligned to
//! [`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`] (256) bytes; [`padded_bytes_per_row`]
//! computes the padded stride and the readback path strips it again.

use crate::device::GpuContext;
use crate::error::{GpuError, GpuResult};
use crate::texture_pool::TextureKey;

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

/// Upload tightly-packed pixel `data` into `texture`.
///
/// `data` must contain exactly `width * height * bytes_per_pixel` bytes for the
/// texture's key.
pub fn upload_texture(ctx: &GpuContext, texture: &wgpu::Texture, key: TextureKey, data: &[u8]) {
    let bpp = key.format.block_copy_size(None).unwrap_or(4);
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
/// Blocks until the GPU copy completes. The texture's usage must include
/// [`wgpu::TextureUsages::COPY_SRC`].
pub fn read_texture(
    ctx: &GpuContext,
    texture: &wgpu::Texture,
    key: TextureKey,
) -> GpuResult<Vec<u8>> {
    let bpp = key.format.block_copy_size(None).unwrap_or(4);
    let unpadded_bpr = key
        .width
        .checked_mul(bpp)
        .expect("row byte count overflows u32");
    let padded_bpr = padded_bytes_per_row(key.width, bpp);
    let buffer_size = padded_bpr as u64 * key.height as u64;

    let staging = ctx.device().create_buffer(&wgpu::BufferDescriptor {
        label: Some("ravel readback staging"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

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
            buffer: &staging,
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
    ctx.queue().submit(Some(encoder.finish()));

    let (tx, rx) = std::sync::mpsc::channel();
    staging
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });

    // Drive the device until the map callback fires.
    ctx.wait();
    rx.recv()
        .map_err(|_| GpuError::Readback("map callback dropped".to_string()))?
        .map_err(|e| GpuError::Readback(e.to_string()))?;

    let mut out = Vec::with_capacity((unpadded_bpr * key.height) as usize);
    {
        let view = staging.slice(..).get_mapped_range();
        for row in 0..key.height as usize {
            let start = row * padded_bpr as usize;
            let end = start + unpadded_bpr as usize;
            out.extend_from_slice(&view[start..end]);
        }
    }
    staging.unmap();

    Ok(out)
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
}
