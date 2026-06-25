// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Transfer hardware-decoded frames to CPU-accessible pixel formats.

use ffmpeg_the_third::ffi::{av_hwframe_transfer_data, AVPixelFormat};
use ffmpeg_the_third::util::frame;

use ravel_core::media::{MediaError, MediaResult};

/// Known hardware pixel formats that require transfer to a CPU format.
fn is_hw_pixel_format(fmt: AVPixelFormat) -> bool {
    matches!(
        fmt,
        AVPixelFormat::VIDEOTOOLBOX
            | AVPixelFormat::CUDA
            | AVPixelFormat::D3D11
            | AVPixelFormat::D3D11VA_VLD
            | AVPixelFormat::DXVA2_VLD
            | AVPixelFormat::VAAPI
            | AVPixelFormat::QSV
    )
}

/// If `hw_frame` uses a hardware pixel format, transfer it to a
/// CPU-accessible format via `av_hwframe_transfer_data`.
///
/// If the frame is already in a software format, this is a no-op and
/// the original frame reference is returned.
pub(crate) fn ensure_sw_frame(hw_frame: &frame::Video) -> MediaResult<Option<frame::Video>> {
    let raw_format = unsafe { (*hw_frame.as_ptr()).format };
    let pix_fmt = AVPixelFormat(raw_format);

    if !is_hw_pixel_format(pix_fmt) {
        return Ok(None);
    }

    let mut sw_frame = frame::Video::empty();

    let ret = unsafe {
        av_hwframe_transfer_data(
            sw_frame.as_mut_ptr(),
            hw_frame.as_ptr(),
            0, // flags
        )
    };

    if ret < 0 {
        return Err(MediaError::DecodeError(format!(
            "hardware frame transfer failed (error {ret})"
        )));
    }

    Ok(Some(sw_frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sw_frame_passes_through() {
        // An empty frame has format NONE (-1), which is not a HW format.
        let frame = frame::Video::empty();
        let result = ensure_sw_frame(&frame).unwrap();
        assert!(result.is_none());
    }
}
