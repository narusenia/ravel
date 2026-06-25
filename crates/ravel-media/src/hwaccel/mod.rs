// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Hardware-accelerated video decoding via FFmpeg's hwaccel API.
//!
//! Supports VideoToolbox (macOS) and NVDEC/D3D11VA/AMF (Windows).
//! Falls back to software decoding when hardware acceleration is unavailable.

pub(crate) mod device;
pub(crate) mod transfer;

use ffmpeg_the_third::ffi::AVHWDeviceType;

/// Supported hardware acceleration backends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwBackend {
    #[cfg(target_os = "macos")]
    VideoToolbox,
    #[cfg(target_os = "windows")]
    Cuda,
    #[cfg(target_os = "windows")]
    D3d11va,
}

impl HwBackend {
    /// Map to the corresponding FFmpeg `AVHWDeviceType`.
    pub(crate) fn to_av_device_type(self) -> AVHWDeviceType {
        match self {
            #[cfg(target_os = "macos")]
            Self::VideoToolbox => AVHWDeviceType::VIDEOTOOLBOX,
            #[cfg(target_os = "windows")]
            Self::Cuda => AVHWDeviceType::CUDA,
            #[cfg(target_os = "windows")]
            Self::D3d11va => AVHWDeviceType::D3D11VA,
        }
    }

    /// Human-readable name for logging.
    pub(crate) fn name(self) -> &'static str {
        match self {
            #[cfg(target_os = "macos")]
            Self::VideoToolbox => "VideoToolbox",
            #[cfg(target_os = "windows")]
            Self::Cuda => "CUDA/NVDEC",
            #[cfg(target_os = "windows")]
            Self::D3d11va => "D3D11VA",
        }
    }
}

/// Configuration for hardware acceleration.
pub struct HwAccelConfig {
    /// Ordered list of backends to try (first match wins).
    pub preferred_backends: Vec<HwBackend>,
    /// Fall back to software decoding if all HW backends fail.
    pub allow_sw_fallback: bool,
}

impl Default for HwAccelConfig {
    fn default() -> Self {
        Self {
            preferred_backends: platform_default_backends(),
            allow_sw_fallback: true,
        }
    }
}

/// Return the default ordered list of HW backends for the current platform.
pub fn platform_default_backends() -> Vec<HwBackend> {
    let mut backends = Vec::new();
    #[cfg(target_os = "macos")]
    {
        backends.push(HwBackend::VideoToolbox);
    }
    #[cfg(target_os = "windows")]
    {
        backends.push(HwBackend::Cuda);
        backends.push(HwBackend::D3d11va);
    }
    backends
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_backends_not_empty() {
        let backends = platform_default_backends();
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        assert!(!backends.is_empty());
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert!(backends.is_empty());
    }

    #[test]
    fn hw_backend_name_is_not_empty() {
        for backend in platform_default_backends() {
            assert!(!backend.name().is_empty());
        }
    }

    #[test]
    fn default_config_allows_fallback() {
        let config = HwAccelConfig::default();
        assert!(config.allow_sw_fallback);
    }
}
