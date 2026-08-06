// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Render-output encoding: the [`Encoder`] contract and the runtime
//! availability enumeration behind it.
//!
//! # Why this is separate from [`MediaWriter`]
//!
//! [`MediaWriter`](super::MediaWriter) describes a *container*: one file with
//! a video stream and an audio stream, written interleaved. Render output is
//! not always a container — a PNG or EXR sequence is `n` independent files —
//! and it has a lifecycle a container writer does not model: it can be
//! cancelled at a frame boundary and must then leave nothing behind.
//!
//! [`Encoder`] therefore sits *beside* `MediaWriter`, not on top of it:
//! `begin` / `write_frame` / `finish` / `abort` is the render worker's
//! vocabulary, and a container-backed implementation is free to drive a
//! `MediaWriter` internally.
//!
//! # Why availability is computed at runtime
//!
//! The same binary can encode ProRes on a macOS host and nothing but image
//! sequences on a minimal Linux one, because the codec depends on the linked
//! FFmpeg build and the platform's own encoding API. Compiling the list in is
//! therefore wrong. [`enumerate_encoders`] asks an [`EncoderProbe`] and
//! returns **every** target Ravel knows about, each one either available with
//! the route that would serve it or unavailable with a machine-readable
//! [`UnavailableReason`] — so the UI can grey an entry out *and say why*
//! rather than silently omit it.
//!
//! The reasons are structured, not prose: user-visible text is the caller's
//! job through `t!`.

use std::path::{Path, PathBuf};

use super::{ImageFormat, MediaResult, VideoCodec};
use crate::types::FrameBuffer;

// ===========================================================================
// Encoder trait
// ===========================================================================

/// Writes the frames of one render job to disk.
///
/// The call order is `begin` → `write_frame`\* → `finish`, with [`abort`]
/// legal at any point after `begin` instead of `finish`. Implementations
/// reject out-of-order calls rather than guessing.
///
/// **Cancellation must not leave partial output, and must not take anything
/// else with it.** After [`abort`] — and after a drop that follows `begin`
/// without either terminator — nothing the encoder created may remain on
/// disk, and nothing it did not create may have been removed. The render
/// worker cancels at frame boundaries and relies on both halves.
///
/// [`abort`]: Encoder::abort
pub trait Encoder: Send {
    /// Prepare the destination (create directories, open the container, write
    /// the header). Called exactly once, before any frame.
    ///
    /// A `begin` that returns `Err` may have got partway — some directories
    /// made, a header written — so the render worker calls [`abort`]
    /// afterwards. Implementations either undo their own work here or leave
    /// it for that call; either way nothing they created may survive.
    ///
    /// [`abort`]: Encoder::abort
    fn begin(&mut self) -> MediaResult<()>;

    /// Write one frame.
    ///
    /// `index` is the **absolute** frame number, not a counter from zero: a
    /// job rendered as `--range 100-199` writes `frame_0100 …` so that
    /// splitting a render across processes produces one coherent sequence.
    /// Frames arrive in ascending order.
    fn write_frame(&mut self, frame: &FrameBuffer, index: u64) -> MediaResult<()>;

    /// Flush and close the output. After this the written files are final.
    ///
    /// **A `finish` that returns `Err` has not made the output final, and
    /// leaves the encoder in a state where [`abort`] can still remove what it
    /// created.** Closing is not always cheap — a container writes a trailer
    /// and can fail halfway through it — so the render worker calls `abort`
    /// after a failed `finish` rather than trusting `Drop` to notice. An
    /// implementation that has genuinely finalized its output before failing
    /// reports that by making the subsequent `abort` an error, which the
    /// worker logs.
    ///
    /// [`abort`]: Encoder::abort
    fn finish(&mut self) -> MediaResult<()>;

    /// Cancel the job and remove the output it created.
    ///
    /// **Only what this encoder created.** A file that was already on disk and
    /// got overwritten during the run is not removed — deleting a user's
    /// previous render because the new one was cancelled would be worse than
    /// leaving it replaced. Nor is the previous content restored: the write
    /// already happened. A caller that needs the earlier output intact writes
    /// somewhere else.
    ///
    /// Returns the first removal failure, having still attempted the rest;
    /// callers generally log it, because the job is being abandoned anyway.
    fn abort(&mut self) -> MediaResult<()>;
}

// ===========================================================================
// Output description
// ===========================================================================

/// Bits per channel in a PNG sequence.
///
/// Eight is the default because it is what every downstream tool reads
/// without comment. Sixteen exists for hand-off into a grade, where the
/// 8-bit quantisation of a gradient shows as banding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PngDepth {
    #[default]
    Eight,
    Sixteen,
}

impl PngDepth {
    /// Bits per channel.
    pub const fn bits(self) -> u32 {
        match self {
            Self::Eight => 8,
            Self::Sixteen => 16,
        }
    }

    /// Largest storable channel value — the scale factor for `0.0..=1.0`.
    pub const fn max_value(self) -> u32 {
        match self {
            Self::Eight => 255,
            Self::Sixteen => 65_535,
        }
    }
}

/// Which still-image encoder writes a sequence, with its settings.
///
/// A single type rather than an [`ImageFormat`] plus loose options: the bit
/// depth only means something for PNG, and the formats Ravel cannot write
/// (TIFF, DPX) have no variant here, so neither mistake is representable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SequenceCodec {
    /// 8- or 16-bit PNG. Alpha preserved; values clamped to `0.0..=1.0`.
    Png(PngDepth),
    /// 32-bit float EXR. Values pass through untouched.
    Exr,
}

impl SequenceCodec {
    /// The container format, which also decides the file extension.
    pub const fn image_format(self) -> ImageFormat {
        match self {
            Self::Png(_) => ImageFormat::Png,
            Self::Exr => ImageFormat::Exr,
        }
    }

    /// Pair an [`ImageFormat`] with PNG settings, or `None` when Ravel has no
    /// writer for it. The entry point for a CLI flag or a UI menu choice.
    pub const fn from_image_format(format: ImageFormat, png_depth: PngDepth) -> Option<Self> {
        match format {
            ImageFormat::Png => Some(Self::Png(png_depth)),
            ImageFormat::Exr => Some(Self::Exr),
            ImageFormat::Tiff | ImageFormat::Dpx => None,
        }
    }
}

/// Where a numbered image sequence is written and how its files are named.
///
/// The write-side counterpart of [`ImageSequenceInfo`], which describes a
/// sequence that already exists. This one has no frame range: the range
/// belongs to the render job, and the encoder names files from the absolute
/// index it is handed.
///
/// [`ImageSequenceInfo`]: super::ImageSequenceInfo
/// The fields are private because the name components are **validated on
/// construction**. `prefix` and `suffix` are pasted into a file name that is
/// then joined onto `directory`, so a `../` or a path separator in either
/// would put both the write and the `abort` cleanup somewhere the caller
/// never named. Nothing constructs this type today, but `EXPORT-3` exposes
/// both components as CLI arguments.
///
/// The read side ([`ImageSequenceInfo`]) needs no such check: it derives its
/// prefix by splitting a file name that already exists on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageSequenceOutput {
    directory: PathBuf,
    prefix: String,
    suffix: String,
    codec: SequenceCodec,
    padding: usize,
}

impl ImageSequenceOutput {
    /// Describe a sequence to write.
    ///
    /// Fails when `prefix` or `suffix` could escape `directory` — see
    /// [`check_name_component`]. `directory` itself is the caller's choice and
    /// is not constrained.
    pub fn new(
        directory: impl Into<PathBuf>,
        prefix: impl Into<String>,
        suffix: impl Into<String>,
        codec: SequenceCodec,
        padding: usize,
    ) -> MediaResult<Self> {
        let prefix = prefix.into();
        let suffix = suffix.into();
        check_name_component("prefix", &prefix)?;
        check_name_component("suffix", &suffix)?;
        Ok(Self {
            directory: directory.into(),
            prefix,
            suffix,
            codec,
            padding,
        })
    }

    /// Directory the frames are written into.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Filename text before the frame number.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Filename text between the frame number and the extension.
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    /// Encoder and its settings.
    pub fn codec(&self) -> SequenceCodec {
        self.codec
    }

    /// Minimum digits in the frame number, zero-padded.
    pub fn padding(&self) -> usize {
        self.padding
    }

    /// Build the path for a specific absolute frame number.
    ///
    /// Always inside [`directory`](Self::directory), because the components
    /// were checked when this value was built.
    pub fn frame_path(&self, frame: u64) -> PathBuf {
        self.directory.join(super::sequence_file_name(
            &self.prefix,
            frame,
            &self.suffix,
            self.codec.image_format(),
            self.padding,
        ))
    }
}

/// Reject a file-name fragment that could leave the output directory.
///
/// Separators and the drive marker are refused on every platform, not just
/// the host's: a project authored on Linux carrying a `dir\name` prefix must
/// not become a traversal when the render runs on Windows, and `C:foo` is a
/// *drive-relative* path there — resolved against that drive's current
/// directory, not against the output directory. `..` is refused anywhere in the
/// fragment rather than only as a whole component — that also rejects the
/// harmless `shot..a`, which is a price worth paying for a rule with no edge
/// cases.
fn check_name_component(label: &str, value: &str) -> MediaResult<()> {
    let bad = |reason: &str| {
        Err(super::MediaError::InvalidSequenceName(format!(
            "{label} {value:?} {reason}"
        )))
    };
    if value.contains('/') || value.contains('\\') {
        return bad("contains a path separator");
    }
    if value.contains(':') {
        return bad("contains \":\", which names a drive on Windows");
    }
    if value.contains("..") {
        return bad("contains \"..\"");
    }
    if value.contains('\0') {
        return bad("contains a NUL byte");
    }
    Ok(())
}

// ===========================================================================
// Availability enumeration
// ===========================================================================

/// A platform-provided encoding API, reached through FFmpeg's wrapper for it.
///
/// These matter beyond mere availability: routing H.264 or ProRes through the
/// OS keeps the codec licence with the OS or hardware vendor, which is the
/// only route Ravel offers for either.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlatformApi {
    /// Apple VideoToolbox (macOS).
    VideoToolbox,
    /// Microsoft Media Foundation (Windows).
    MediaFoundation,
    /// VA-API (Linux).
    Vaapi,
}

impl PlatformApi {
    /// Stable identifier for locale keys and machine-readable output.
    pub const fn id(self) -> &'static str {
        match self {
            Self::VideoToolbox => "videotoolbox",
            Self::MediaFoundation => "mediafoundation",
            Self::Vaapi => "vaapi",
        }
    }

    /// Whether this API belongs to the operating system the binary targets.
    ///
    /// A `true` here is necessary but not sufficient — VA-API also needs a
    /// render node — so probes narrow it further; a `false` is decisive.
    pub const fn is_native_to_build_target(self) -> bool {
        match self {
            Self::VideoToolbox => cfg!(target_os = "macos"),
            Self::MediaFoundation => cfg!(target_os = "windows"),
            Self::Vaapi => cfg!(target_os = "linux"),
        }
    }
}

impl std::fmt::Display for PlatformApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// One selectable render output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EncodeTarget {
    /// A numbered still-image sequence.
    ImageSequence(ImageFormat),
    /// A video stream. The container is chosen separately.
    Video(VideoCodec),
}

/// How an available target would actually be produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EncodeRoute {
    /// Written by Ravel itself without FFmpeg. Always present, which is what
    /// makes image sequences the guaranteed output path.
    Native,
    /// A software encoder compiled into the linked FFmpeg build.
    FfmpegSoftware {
        /// FFmpeg's name for it, e.g. `libvpx-vp9`.
        encoder: &'static str,
    },
    /// A platform or hardware encoder reached through FFmpeg.
    Platform {
        api: PlatformApi,
        /// FFmpeg's name for the wrapper, e.g. `h264_videotoolbox`.
        encoder: &'static str,
    },
}

/// Why a target cannot be used in this build on this machine.
///
/// Structured rather than a message so the UI can render it through `t!` and
/// the CLI can emit it machine-readably.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    /// The binary was built without the `ffmpeg` feature, so no codec beyond
    /// the native image-sequence writers exists at all.
    FfmpegNotLinked,
    /// FFmpeg is linked but registers none of the encoders that could serve
    /// this target — the classic "this FFmpeg build has no libaom".
    FfmpegEncoderMissing {
        /// Every encoder name that was looked for, in preference order.
        candidates: Vec<&'static str>,
    },
    /// The target is only offered through a platform API that this operating
    /// system has but this host does not provide — a Linux box whose kernel
    /// exposes no VA-API render node, say.
    PlatformApiUnavailable { api: PlatformApi },
    /// The target is only offered through platform APIs, and this operating
    /// system has none of them. ProRes away from macOS is the case: naming
    /// VideoToolbox there would send the reader looking for something that
    /// cannot exist on their machine.
    NoPlatformRouteOnThisOs,
    /// Ravel declines to offer the target regardless of what is installed:
    /// its patent pool is fragmented and the software implementations that
    /// exist are copyleft, which the distributed binary cannot take on.
    NotOffered,
}

/// Whether a target can be used, and either how or why not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// Usable, via this route.
    Available(EncodeRoute),
    /// Not usable, for this reason.
    Unavailable(UnavailableReason),
}

/// One row of the enumeration: a target and its verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderAvailability {
    pub target: EncodeTarget,
    pub availability: Availability,
}

impl EncoderAvailability {
    /// Whether this target can be selected.
    pub fn is_available(&self) -> bool {
        matches!(self.availability, Availability::Available(_))
    }

    /// The route serving this target, or `None` when it is unavailable.
    pub fn route(&self) -> Option<EncodeRoute> {
        match self.availability {
            Availability::Available(route) => Some(route),
            Availability::Unavailable(_) => None,
        }
    }

    /// Why this target is unavailable, or `None` when it is available.
    pub fn reason(&self) -> Option<&UnavailableReason> {
        match &self.availability {
            Availability::Available(_) => None,
            Availability::Unavailable(reason) => Some(reason),
        }
    }
}

/// Answers the environment questions [`enumerate_encoders`] asks.
///
/// Implemented against the linked FFmpeg in `ravel-media`; implemented by
/// hand in tests, which is how the "no FFmpeg at all" environment gets
/// covered on a machine that has FFmpeg.
pub trait EncoderProbe {
    /// Whether the running binary can reach FFmpeg at all.
    fn ffmpeg_linked(&self) -> bool;

    /// Whether FFmpeg registers an encoder under `name`.
    ///
    /// Only consulted when [`ffmpeg_linked`](EncoderProbe::ffmpeg_linked)
    /// is `true`.
    fn has_ffmpeg_encoder(&self, name: &str) -> bool;

    /// Whether `api` is usable on this host.
    fn platform_api_available(&self, api: PlatformApi) -> bool;
}

/// How a target is served, before the probe resolves it.
enum RouteSpec {
    /// Ravel's own writer.
    Native,
    /// A software FFmpeg encoder; candidates in preference order.
    FfmpegSoftware(&'static [&'static str]),
    /// A platform API; one entry per OS, candidates in preference order.
    Platform(&'static [(PlatformApi, &'static [&'static str])]),
    /// Deliberately not offered.
    NotOffered,
}

/// Every target the exporter offers, in presentation order.
///
/// Deliberately narrower than [`VideoCodec`]: that enum covers what the
/// *decoder* understands, while this table is what Ravel is willing to
/// *write* (`docs/implementation/render-export-plan.md`, "v1 の出力形式").
/// DNxHR and VP8 are decode-only for now and so are absent rather than listed
/// as unavailable, which would misreport a policy choice as an environment
/// gap.
const TARGETS: &[(EncodeTarget, RouteSpec)] = &[
    // The guaranteed floor: no FFmpeg, no patents, alpha preserved.
    (
        EncodeTarget::ImageSequence(ImageFormat::Png),
        RouteSpec::Native,
    ),
    // Linear 32-bit float straight out of the evaluator (REQ-CORE-009).
    (
        EncodeTarget::ImageSequence(ImageFormat::Exr),
        RouteSpec::Native,
    ),
    // Royalty-free, BSD-licensed implementations.
    (
        EncodeTarget::Video(VideoCodec::Vp9),
        RouteSpec::FfmpegSoftware(&["libvpx-vp9"]),
    ),
    (
        EncodeTarget::Video(VideoCodec::Av1),
        RouteSpec::FfmpegSoftware(&["libsvtav1", "librav1e", "libaom-av1"]),
    ),
    // Apple's own encoder only: FFmpeg's reverse-engineered ProRes is a
    // trademark and licensing grey area.
    (
        EncodeTarget::Video(VideoCodec::ProRes),
        RouteSpec::Platform(&[(PlatformApi::VideoToolbox, &["prores_videotoolbox"])]),
    ),
    // OS / hardware only: x264 is GPL, which the distributed binary refuses.
    (
        EncodeTarget::Video(VideoCodec::H264),
        RouteSpec::Platform(&[
            (PlatformApi::VideoToolbox, &["h264_videotoolbox"]),
            (PlatformApi::MediaFoundation, &["h264_mf"]),
            (PlatformApi::Vaapi, &["h264_vaapi"]),
        ]),
    ),
    // Three competing pools plus unaffiliated holders.
    (EncodeTarget::Video(VideoCodec::H265), RouteSpec::NotOffered),
];

/// Enumerate every render output with its availability on this host.
///
/// Pure: the environment enters only through `probe`. The result is stable in
/// order and always covers the whole table, so a UI can build its list once
/// and show unavailable entries with their reason.
pub fn enumerate_encoders(probe: &dyn EncoderProbe) -> Vec<EncoderAvailability> {
    TARGETS
        .iter()
        .map(|(target, spec)| EncoderAvailability {
            target: *target,
            availability: resolve(probe, spec),
        })
        .collect()
}

fn resolve(probe: &dyn EncoderProbe, spec: &RouteSpec) -> Availability {
    match spec {
        RouteSpec::Native => Availability::Available(EncodeRoute::Native),
        RouteSpec::NotOffered => Availability::Unavailable(UnavailableReason::NotOffered),
        RouteSpec::FfmpegSoftware(candidates) => {
            if !probe.ffmpeg_linked() {
                return Availability::Unavailable(UnavailableReason::FfmpegNotLinked);
            }
            match first_present(probe, candidates) {
                Some(encoder) => Availability::Available(EncodeRoute::FfmpegSoftware { encoder }),
                None => Availability::Unavailable(UnavailableReason::FfmpegEncoderMissing {
                    candidates: candidates.to_vec(),
                }),
            }
        }
        RouteSpec::Platform(options) => {
            if !probe.ffmpeg_linked() {
                return Availability::Unavailable(UnavailableReason::FfmpegNotLinked);
            }
            let usable: Vec<_> = options
                .iter()
                .filter(|(api, _)| probe.platform_api_available(*api))
                .collect();
            if usable.is_empty() {
                // Name the API this OS would have used — and when the target
                // lists none for this OS, say exactly that rather than naming
                // a foreign one. Reporting "no VideoToolbox" for ProRes on
                // Windows would send the reader looking for something that
                // cannot exist on their machine.
                return Availability::Unavailable(
                    match options
                        .iter()
                        .map(|(api, _)| *api)
                        .find(|api| api.is_native_to_build_target())
                    {
                        Some(api) => UnavailableReason::PlatformApiUnavailable { api },
                        None => UnavailableReason::NoPlatformRouteOnThisOs,
                    },
                );
            }
            for (api, candidates) in &usable {
                if let Some(encoder) = first_present(probe, candidates) {
                    return Availability::Available(EncodeRoute::Platform { api: *api, encoder });
                }
            }
            // The API is there but its FFmpeg wrapper is not compiled in.
            Availability::Unavailable(UnavailableReason::FfmpegEncoderMissing {
                candidates: usable
                    .iter()
                    .flat_map(|(_, candidates)| candidates.iter().copied())
                    .collect(),
            })
        }
    }
}

fn first_present(probe: &dyn EncoderProbe, candidates: &[&'static str]) -> Option<&'static str> {
    candidates
        .iter()
        .copied()
        .find(|name| probe.has_ffmpeg_encoder(name))
}

// ===========================================================================
// Partial-output cleanup
// ===========================================================================

/// Remove `paths`, ignoring the ones that are already gone.
///
/// Shared by every [`Encoder`] implementation's `abort`: the first failure is
/// returned but every path is still attempted, because leaving *more* debris
/// behind after a cancellation is the worse outcome.
pub fn remove_partial_output<I, P>(paths: I) -> MediaResult<()>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut first_error = None;
    for path in paths {
        match std::fs::remove_file(path.as_ref()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(super::MediaError::Io(e));
                }
            }
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A hand-built environment. Every question the enumeration can ask is
    /// answered from these three fields, which is the point: the "no FFmpeg"
    /// and "no VideoToolbox" cases are reachable on any developer machine.
    struct FakeProbe {
        ffmpeg: bool,
        encoders: HashSet<&'static str>,
        apis: HashSet<PlatformApi>,
    }

    impl FakeProbe {
        fn without_ffmpeg() -> Self {
            Self {
                ffmpeg: false,
                encoders: HashSet::new(),
                apis: HashSet::new(),
            }
        }

        fn with(encoders: &[&'static str], apis: &[PlatformApi]) -> Self {
            Self {
                ffmpeg: true,
                encoders: encoders.iter().copied().collect(),
                apis: apis.iter().copied().collect(),
            }
        }
    }

    impl EncoderProbe for FakeProbe {
        fn ffmpeg_linked(&self) -> bool {
            self.ffmpeg
        }

        fn has_ffmpeg_encoder(&self, name: &str) -> bool {
            self.encoders.contains(name)
        }

        fn platform_api_available(&self, api: PlatformApi) -> bool {
            self.apis.contains(&api)
        }
    }

    fn row(rows: &[EncoderAvailability], target: EncodeTarget) -> EncoderAvailability {
        rows.iter()
            .find(|r| r.target == target)
            .unwrap_or_else(|| panic!("{target:?} missing from the enumeration"))
            .clone()
    }

    #[test]
    fn image_sequences_are_available_without_ffmpeg() {
        let rows = enumerate_encoders(&FakeProbe::without_ffmpeg());
        for format in [ImageFormat::Png, ImageFormat::Exr] {
            let entry = row(&rows, EncodeTarget::ImageSequence(format));
            assert_eq!(
                entry.availability,
                Availability::Available(EncodeRoute::Native),
                "{format} must stay available with no FFmpeg",
            );
        }
    }

    #[test]
    fn video_targets_report_missing_ffmpeg_as_the_reason() {
        let rows = enumerate_encoders(&FakeProbe::without_ffmpeg());
        for codec in [VideoCodec::Vp9, VideoCodec::Av1, VideoCodec::ProRes] {
            let entry = row(&rows, EncodeTarget::Video(codec));
            assert!(!entry.is_available(), "{codec} cannot work without FFmpeg");
            assert_eq!(
                entry.reason(),
                Some(&UnavailableReason::FfmpegNotLinked),
                "{codec} must say FFmpeg is missing, not merely be absent",
            );
        }
    }

    #[test]
    fn every_row_carries_a_route_or_a_reason() {
        for probe in [
            FakeProbe::without_ffmpeg(),
            FakeProbe::with(&["libvpx-vp9"], &[PlatformApi::Vaapi]),
        ] {
            let rows = enumerate_encoders(&probe);
            assert_eq!(rows.len(), TARGETS.len());
            for entry in rows {
                assert_eq!(
                    entry.route().is_some(),
                    entry.reason().is_none(),
                    "{:?} is neither routed nor explained",
                    entry.target,
                );
            }
        }
    }

    #[test]
    fn missing_codec_names_the_encoders_it_looked_for() {
        let rows = enumerate_encoders(&FakeProbe::with(&["libvpx-vp9"], &[]));
        assert!(row(&rows, EncodeTarget::Video(VideoCodec::Vp9)).is_available());

        let av1 = row(&rows, EncodeTarget::Video(VideoCodec::Av1));
        match av1.reason() {
            Some(UnavailableReason::FfmpegEncoderMissing { candidates }) => {
                assert!(
                    candidates.contains(&"libsvtav1"),
                    "the reason must name what was searched for: {candidates:?}",
                );
            }
            other => panic!("expected a missing-encoder reason, got {other:?}"),
        }
    }

    #[test]
    fn software_route_picks_the_first_present_candidate() {
        // Preference order is libsvtav1, librav1e, libaom-av1; only the last
        // is installed here.
        let rows = enumerate_encoders(&FakeProbe::with(&["libaom-av1"], &[]));
        assert_eq!(
            row(&rows, EncodeTarget::Video(VideoCodec::Av1)).route(),
            Some(EncodeRoute::FfmpegSoftware {
                encoder: "libaom-av1"
            }),
        );
    }

    #[test]
    fn prores_never_names_videotoolbox_away_from_macos() {
        let rows = enumerate_encoders(&FakeProbe::with(&["prores_videotoolbox"], &[]));
        let reason = row(&rows, EncodeTarget::Video(VideoCodec::ProRes))
            .reason()
            .cloned();
        assert!(
            !row(&rows, EncodeTarget::Video(VideoCodec::ProRes)).is_available(),
            "an installed wrapper must not make ProRes available without the API",
        );
        if cfg!(target_os = "macos") {
            assert_eq!(
                reason,
                Some(UnavailableReason::PlatformApiUnavailable {
                    api: PlatformApi::VideoToolbox,
                }),
            );
        } else {
            assert_eq!(
                reason,
                Some(UnavailableReason::NoPlatformRouteOnThisOs),
                "off macOS there is no VideoToolbox to look for, so it must not be named",
            );
        }
    }

    #[test]
    fn prores_routes_through_videotoolbox_when_the_api_is_there() {
        let rows = enumerate_encoders(&FakeProbe::with(
            &["prores_videotoolbox"],
            &[PlatformApi::VideoToolbox],
        ));
        assert_eq!(
            row(&rows, EncodeTarget::Video(VideoCodec::ProRes)).route(),
            Some(EncodeRoute::Platform {
                api: PlatformApi::VideoToolbox,
                encoder: "prores_videotoolbox",
            }),
        );
    }

    #[test]
    fn h264_reports_the_api_this_os_would_have_used() {
        let rows = enumerate_encoders(&FakeProbe::with(&[], &[]));
        // H.264 lists a route for all three supported systems, so on each of
        // them the reason names that system's own API — never a foreign one.
        let expected = if cfg!(target_os = "macos") {
            UnavailableReason::PlatformApiUnavailable {
                api: PlatformApi::VideoToolbox,
            }
        } else if cfg!(target_os = "windows") {
            UnavailableReason::PlatformApiUnavailable {
                api: PlatformApi::MediaFoundation,
            }
        } else if cfg!(target_os = "linux") {
            UnavailableReason::PlatformApiUnavailable {
                api: PlatformApi::Vaapi,
            }
        } else {
            UnavailableReason::NoPlatformRouteOnThisOs
        };
        assert_eq!(
            row(&rows, EncodeTarget::Video(VideoCodec::H264)).reason(),
            Some(&expected),
        );
    }

    #[test]
    fn a_named_platform_api_is_always_one_this_os_has() {
        // The invariant behind both tests above: whatever target and whatever
        // environment, an API we name must be native to the build target.
        for probe in [
            FakeProbe::without_ffmpeg(),
            FakeProbe::with(&[], &[]),
            FakeProbe::with(&["prores_videotoolbox", "h264_mf"], &[]),
        ] {
            for entry in enumerate_encoders(&probe) {
                if let Some(UnavailableReason::PlatformApiUnavailable { api }) = entry.reason() {
                    assert!(
                        api.is_native_to_build_target(),
                        "{:?} named {api}, which this OS cannot have",
                        entry.target,
                    );
                }
            }
        }
    }

    #[test]
    fn h264_falls_back_to_whichever_platform_api_is_present() {
        let rows = enumerate_encoders(&FakeProbe::with(&["h264_vaapi"], &[PlatformApi::Vaapi]));
        assert_eq!(
            row(&rows, EncodeTarget::Video(VideoCodec::H264)).route(),
            Some(EncodeRoute::Platform {
                api: PlatformApi::Vaapi,
                encoder: "h264_vaapi",
            }),
        );
    }

    #[test]
    fn present_api_without_its_wrapper_blames_the_wrapper() {
        let rows = enumerate_encoders(&FakeProbe::with(&[], &[PlatformApi::VideoToolbox]));
        assert_eq!(
            row(&rows, EncodeTarget::Video(VideoCodec::ProRes)).reason(),
            Some(&UnavailableReason::FfmpegEncoderMissing {
                candidates: vec!["prores_videotoolbox"],
            }),
        );
    }

    #[test]
    fn h265_is_refused_regardless_of_the_environment() {
        let rows = enumerate_encoders(&FakeProbe::with(
            &["hevc_videotoolbox", "libx265"],
            &[
                PlatformApi::VideoToolbox,
                PlatformApi::MediaFoundation,
                PlatformApi::Vaapi,
            ],
        ));
        assert_eq!(
            row(&rows, EncodeTarget::Video(VideoCodec::H265)).reason(),
            Some(&UnavailableReason::NotOffered),
        );
    }

    #[test]
    fn codec_maps_to_its_container_format() {
        assert_eq!(
            SequenceCodec::Png(PngDepth::Sixteen).image_format(),
            ImageFormat::Png,
            "the bit depth must not change the file extension",
        );
        assert_eq!(SequenceCodec::Exr.image_format(), ImageFormat::Exr);
        assert_eq!(PngDepth::default(), PngDepth::Eight);
        assert_eq!(PngDepth::Sixteen.max_value(), 65_535);
    }

    #[test]
    fn only_writable_formats_convert_into_a_codec() {
        assert_eq!(
            SequenceCodec::from_image_format(ImageFormat::Png, PngDepth::Sixteen),
            Some(SequenceCodec::Png(PngDepth::Sixteen)),
        );
        assert_eq!(
            SequenceCodec::from_image_format(ImageFormat::Exr, PngDepth::default()),
            Some(SequenceCodec::Exr),
        );
        for format in [ImageFormat::Tiff, ImageFormat::Dpx] {
            assert_eq!(
                SequenceCodec::from_image_format(format, PngDepth::default()),
                None,
                "{format} has no writer, so it must not produce a codec",
            );
        }
    }

    #[test]
    fn sequence_output_names_files_by_absolute_frame() {
        let output =
            ImageSequenceOutput::new("/out", "beauty_", "", SequenceCodec::Exr, 4).unwrap();
        assert_eq!(
            output.frame_path(100),
            PathBuf::from("/out/beauty_0100.exr"),
        );
        // Beyond the padding width the number is not truncated.
        assert_eq!(
            output.frame_path(123_456),
            PathBuf::from("/out/beauty_123456.exr"),
        );
    }

    #[test]
    fn name_components_cannot_escape_the_output_directory() {
        // Every one of these would otherwise make `frame_path` — and so the
        // write, and so the abort cleanup — land outside `/out`.
        let escapes = [
            "../",
            "../../etc/",
            "a/b",
            "a\\b",
            "..",
            "/absolute",
            "\0nul",
            // Windows drive-relative: `C:frame_0001.png` resolves against
            // drive C's own current directory, not the output directory.
            "C:",
            "C:frame_",
            "\\\\server\\share\\",
        ];
        for bad in escapes {
            assert!(
                matches!(
                    ImageSequenceOutput::new("/out", bad, "", SequenceCodec::Exr, 4),
                    Err(super::super::MediaError::InvalidSequenceName(_)),
                ),
                "prefix {bad:?} must be refused",
            );
            assert!(
                matches!(
                    ImageSequenceOutput::new("/out", "f_", bad, SequenceCodec::Exr, 4),
                    Err(super::super::MediaError::InvalidSequenceName(_)),
                ),
                "suffix {bad:?} must be refused",
            );
        }
    }

    #[test]
    fn ordinary_name_components_are_accepted() {
        for good in ["", "frame_", "shot-010_", "beauty.v2_", "日本語_"] {
            let output = ImageSequenceOutput::new("/out", good, "", SequenceCodec::Exr, 4)
                .unwrap_or_else(|e| panic!("{good:?} should be usable: {e}"));
            assert!(output.frame_path(1).starts_with("/out"));
        }
    }

    #[test]
    fn every_accepted_name_stays_inside_the_directory() {
        let output =
            ImageSequenceOutput::new("/out/renders", "a.b-c_", "_final", SequenceCodec::Exr, 4)
                .unwrap();
        let path = output.frame_path(7);
        assert_eq!(path.parent(), Some(Path::new("/out/renders")));
        assert_eq!(
            path.components().filter(|c| c.as_os_str() == "..").count(),
            0,
        );
    }

    #[test]
    fn remove_partial_output_tolerates_absent_files() {
        let dir = std::env::temp_dir().join("ravel-encode-cleanup-test");
        std::fs::create_dir_all(&dir).unwrap();
        let present = dir.join("present.png");
        std::fs::write(&present, b"x").unwrap();
        let absent = dir.join("absent.png");

        remove_partial_output([&present, &absent]).expect("absent files are not an error");
        assert!(!present.exists());
        let _ = std::fs::remove_dir(&dir);
    }
}
