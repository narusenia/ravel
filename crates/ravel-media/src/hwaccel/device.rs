// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! RAII wrapper around FFmpeg's `AVHWDeviceContext`.

use std::ptr;

use ffmpeg_the_third::ffi::{
    AVBufferRef, AVHWDeviceType, av_buffer_ref, av_buffer_unref, av_hwdevice_ctx_create,
};
use tracing::{info, warn};

use super::{HwAccelConfig, HwBackend};
use ravel_core::media::MediaError;

/// Owns an `AVBufferRef*` pointing to an `AVHWDeviceContext`.
///
/// Calls `av_buffer_unref` on drop to release the hardware device.
pub(crate) struct HwDeviceContext {
    device_ref: *mut AVBufferRef,
    backend: HwBackend,
    #[allow(dead_code)]
    device_type: AVHWDeviceType,
}

// SAFETY: The HwDeviceContext is only ever used by a single FfmpegDecoder
// instance which is accessed sequentially.  FFmpeg's hw device contexts
// are reference-counted and safe to move between threads as long as they
// are not concurrently accessed.
unsafe impl Send for HwDeviceContext {}

impl HwDeviceContext {
    /// Try to create a hardware device context by iterating over the
    /// preferred backends in `config`.
    ///
    /// Returns `Ok(Some(...))` on success, `Ok(None)` if all backends
    /// failed and `allow_sw_fallback` is true, or `Err(...)` if
    /// fallback is disabled and all backends failed.
    pub(crate) fn try_create(config: &HwAccelConfig) -> Result<Option<Self>, MediaError> {
        for &backend in &config.preferred_backends {
            let device_type = backend.to_av_device_type();
            let mut device_ref: *mut AVBufferRef = ptr::null_mut();

            let ret = unsafe {
                av_hwdevice_ctx_create(
                    &mut device_ref,
                    device_type,
                    ptr::null(),     // default device
                    ptr::null_mut(), // no options
                    0,               // flags
                )
            };

            if ret >= 0 && !device_ref.is_null() {
                info!(backend = backend.name(), "hardware decoder initialized");
                return Ok(Some(Self {
                    device_ref,
                    backend,
                    device_type,
                }));
            }

            warn!(
                backend = backend.name(),
                err = ret,
                "hardware decoder unavailable, trying next"
            );
        }

        if config.allow_sw_fallback {
            info!("no hardware decoder available, using software decode");
            Ok(None)
        } else {
            Err(MediaError::Other(
                "no hardware decoder available and SW fallback disabled".into(),
            ))
        }
    }

    /// The active backend.
    pub(crate) fn backend(&self) -> HwBackend {
        self.backend
    }

    /// Create a new reference to the underlying `AVBufferRef`.
    ///
    /// The caller is responsible for ensuring the returned pointer is
    /// eventually freed with `av_buffer_unref`.  Typically this is done
    /// by assigning it to `AVCodecContext.hw_device_ctx`, which FFmpeg
    /// unrefs automatically when the codec context is freed.
    ///
    /// # Safety
    ///
    /// The returned pointer is valid as long as this `HwDeviceContext`
    /// (or the new ref) is alive.
    pub(crate) unsafe fn new_ref(&self) -> *mut AVBufferRef {
        unsafe { av_buffer_ref(self.device_ref) }
    }
}

impl Drop for HwDeviceContext {
    fn drop(&mut self) {
        if !self.device_ref.is_null() {
            unsafe {
                av_buffer_unref(&mut self.device_ref);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_create_with_default_config() {
        crate::decoder::init_ffmpeg();
        let config = HwAccelConfig::default();
        let result = HwDeviceContext::try_create(&config);
        // Should always succeed (either HW or None with fallback).
        assert!(result.is_ok());
    }

    #[test]
    fn try_create_with_empty_backends_returns_none() {
        crate::decoder::init_ffmpeg();
        let config = HwAccelConfig {
            preferred_backends: vec![],
            allow_sw_fallback: true,
        };
        let result = HwDeviceContext::try_create(&config).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn try_create_no_fallback_empty_backends_errors() {
        crate::decoder::init_ffmpeg();
        let config = HwAccelConfig {
            preferred_backends: vec![],
            allow_sw_fallback: false,
        };
        assert!(HwDeviceContext::try_create(&config).is_err());
    }
}
