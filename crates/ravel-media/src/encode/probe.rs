// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The real environment behind
//! [`enumerate_encoders`](ravel_core::media::encode::enumerate_encoders).
//!
//! Everything here reports the *running* build and host: which FFmpeg (if
//! any) is linked, which encoders it registers, and whether the platform's
//! own encoding API is reachable. The decision logic that consumes these
//! answers is in `ravel-core` and is pure, so the awkward environments — no
//! FFmpeg, no VideoToolbox — are covered by tests there with a hand-built
//! probe rather than requiring such a machine.

use ravel_core::media::encode::{EncoderAvailability, EncoderProbe, PlatformApi};

/// Queries the linked FFmpeg build and the host platform.
///
/// Cheap to construct and stateless; the FFmpeg lookups it performs are
/// registry reads, not device opens.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeProbe;

impl EncoderProbe for RuntimeProbe {
    fn ffmpeg_linked(&self) -> bool {
        cfg!(feature = "ffmpeg")
    }

    fn has_ffmpeg_encoder(&self, name: &str) -> bool {
        #[cfg(feature = "ffmpeg")]
        {
            crate::decoder::init_ffmpeg();
            ffmpeg_the_third::encoder::find_by_name(name).is_some()
        }
        #[cfg(not(feature = "ffmpeg"))]
        {
            let _ = name;
            false
        }
    }

    fn platform_api_available(&self, api: PlatformApi) -> bool {
        if !api.is_native_to_build_target() {
            return false;
        }
        match api {
            // Present on every supported macOS / Windows version; the
            // per-codec question is answered by the encoder lookup instead.
            PlatformApi::VideoToolbox | PlatformApi::MediaFoundation => true,
            // VA-API is a driver, not part of the OS: without a render node
            // there is nothing to encode on even when the wrapper exists.
            PlatformApi::Vaapi => has_drm_render_node(),
        }
    }
}

/// Whether the kernel exposes a DRM render node for VA-API to bind to.
fn has_drm_render_node() -> bool {
    let Ok(entries) = std::fs::read_dir("/dev/dri") else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("renderD"))
    })
}

/// Enumerate every render output with its availability on this machine.
///
/// The list always covers the full target table: unavailable entries carry
/// the reason so callers can show a disabled row that explains itself.
pub fn available_encoders() -> Vec<EncoderAvailability> {
    ravel_core::media::encode::enumerate_encoders(&RuntimeProbe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::media::ImageFormat;
    use ravel_core::media::encode::{EncodeRoute, EncodeTarget};

    #[test]
    fn image_sequences_are_available_in_every_build() {
        let rows = available_encoders();
        for format in [ImageFormat::Png, ImageFormat::Exr] {
            let entry = rows
                .iter()
                .find(|r| r.target == EncodeTarget::ImageSequence(format))
                .expect("image sequence targets are always enumerated");
            assert_eq!(
                entry.route(),
                Some(EncodeRoute::Native),
                "{format} sequences must be usable regardless of FFmpeg",
            );
        }
    }

    #[test]
    fn probe_reports_ffmpeg_according_to_the_build() {
        assert_eq!(RuntimeProbe.ffmpeg_linked(), cfg!(feature = "ffmpeg"));
    }

    #[test]
    fn foreign_platform_apis_are_never_reported_available() {
        for api in [
            PlatformApi::VideoToolbox,
            PlatformApi::MediaFoundation,
            PlatformApi::Vaapi,
        ] {
            if !api.is_native_to_build_target() {
                assert!(
                    !RuntimeProbe.platform_api_available(api),
                    "{api} cannot exist on this build target",
                );
            }
        }
    }

    #[test]
    fn every_enumerated_row_explains_itself() {
        for entry in available_encoders() {
            assert_ne!(
                entry.route().is_some(),
                entry.reason().is_some(),
                "{:?} must carry exactly one of a route and a reason",
                entry.target,
            );
        }
    }
}
