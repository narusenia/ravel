// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The three things a caller has to be able to ask before it can render:
//! which compositions exist, which parameters the project declares, and
//! which outputs this machine can write.
//!
//! All three are **non-interactive and machine-readable**, because the
//! interactive mode (`EXPORT-7`) is meant to be a layer that calls these and
//! offers the answers as choices — not a second implementation that asks the
//! document itself.
//!
//! The parameter listing is
//! [`ExposedListing`](ravel_core::exposed::listing::ExposedListing)'s own
//! serialized form, unchanged. That type exists precisely to be the external
//! contract (it hides the binding and writes values natively), so wrapping
//! it in a second schema here would be a second contract to keep in step.

use ravel_core::composition::Document;
use ravel_core::exposed::listing::ExposedListing;
use ravel_core::media::encode::{Availability, EncodeRoute, EncoderAvailability};
use serde::Serialize;

use crate::args::OutputFormat;
use crate::error::{CliError, localized_reason};

/// A composition as a caller of `--comp` sees it.
#[derive(Debug, Serialize)]
pub struct CompEntry {
    pub id: u64,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: [u32; 2],
    /// Frames in the composition — and so the default render range.
    pub duration_frames: u64,
    /// Whether this is what a render with no `--comp` would pick.
    pub root: bool,
}

#[derive(Debug, Serialize)]
struct CompListing {
    compositions: Vec<CompEntry>,
}

/// The document's compositions, in id order so two runs agree.
pub fn compositions(document: &Document) -> Vec<CompEntry> {
    let mut entries: Vec<CompEntry> = document
        .compositions
        .iter()
        .map(|(id, comp)| CompEntry {
            id: id.raw(),
            name: comp.name.clone(),
            width: comp.resolution.0,
            height: comp.resolution.1,
            frame_rate: [comp.frame_rate.num, comp.frame_rate.den],
            duration_frames: comp.duration_frames,
            root: document.root_comp == Some(*id),
        })
        .collect();
    entries.sort_by_key(|entry| entry.id);
    entries
}

/// One render output and whether it can be used.
#[derive(Debug, Serialize)]
pub struct CodecEntry {
    /// The spelling `--format` takes.
    pub format: &'static str,
    /// `image-sequence` or `video`.
    pub kind: &'static str,
    pub available: bool,
    /// How it would be produced, when it can be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    /// Why it cannot, as a stable identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    /// Whether `ravel-cli` has a writer for it at all. A `false` here with
    /// `available: true` is Ravel's gap, not the machine's (`EXPORT-4`).
    pub writable: bool,
    /// The sentence explaining an unusable entry, in the active locale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
struct CodecListing {
    codecs: Vec<CodecEntry>,
}

/// Every enumerated target, usable or not.
///
/// Unavailable rows are **listed with their reason**, never omitted: a
/// caller has to be able to tell "this build cannot do ProRes" from "ProRes
/// is not a thing".
pub fn codecs(encoders: &[EncoderAvailability]) -> Vec<CodecEntry> {
    encoders
        .iter()
        .filter_map(|row| {
            let format = OutputFormat::from_target(row.target)?;
            let (route, reason, message) = match &row.availability {
                Availability::Available(route) => (Some(describe(*route)), None, None),
                Availability::Unavailable(reason) => (
                    None,
                    Some(reason_id(reason)),
                    Some(localized_reason(reason)),
                ),
            };
            Some(CodecEntry {
                format: format.id(),
                kind: match row.target {
                    ravel_core::media::encode::EncodeTarget::ImageSequence(_) => "image-sequence",
                    ravel_core::media::encode::EncodeTarget::Video(_) => "video",
                },
                available: row.is_available(),
                route,
                reason,
                writable: format
                    .sequence_codec(ravel_core::media::encode::PngDepth::Eight)
                    .is_some(),
                message,
            })
        })
        .collect()
}

fn describe(route: EncodeRoute) -> String {
    match route {
        EncodeRoute::Native => "native".to_string(),
        EncodeRoute::FfmpegSoftware { encoder } => format!("ffmpeg:{encoder}"),
        EncodeRoute::Platform { api, encoder } => format!("platform:{}:{encoder}", api.id()),
    }
}

fn reason_id(reason: &ravel_core::media::encode::UnavailableReason) -> &'static str {
    use ravel_core::media::encode::UnavailableReason as R;
    match reason {
        R::FfmpegNotLinked => "ffmpeg-not-linked",
        R::FfmpegEncoderMissing { .. } => "ffmpeg-encoder-missing",
        R::PlatformApiUnavailable { .. } => "platform-api-unavailable",
        R::NoPlatformRouteOnThisOs => "no-platform-route",
        R::NotOffered => "not-offered",
    }
}

/// Render one of the three listings as pretty JSON.
///
/// Pretty rather than compact because these are read by people at least as
/// often as by scripts, and `jq` does not care either way.
pub fn to_json<T: Serialize>(value: &T) -> Result<String, CliError> {
    serde_json::to_string_pretty(value)
        .map_err(|error| CliError::Internal(format!("serializing the listing failed: {error}")))
}

/// `ravel-cli list comps`.
pub fn comps_json(document: &Document) -> Result<String, CliError> {
    to_json(&CompListing {
        compositions: compositions(document),
    })
}

/// `ravel-cli list params`.
pub fn params_json(document: &Document) -> Result<String, CliError> {
    to_json(&ExposedListing::of(document))
}

/// `ravel-cli list codecs`.
pub fn codecs_json(encoders: &[EncoderAvailability]) -> Result<String, CliError> {
    to_json(&CodecListing {
        codecs: codecs(encoders),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::Composition;
    use ravel_core::id::CompId;
    use ravel_core::types::FrameRate;
    use ravel_media::encode::available_encoders;

    fn document() -> Document {
        Document::default()
            .with_composition(Composition::new(
                CompId::new(2),
                "Main",
                (1920, 1080),
                FrameRate::new(30, 1),
                300,
            ))
            .with_composition(Composition::new(
                CompId::new(1),
                "Insert",
                (640, 480),
                FrameRate::new(24, 1),
                48,
            ))
    }

    #[test]
    fn compositions_are_listed_in_id_order_with_the_root_marked() {
        let entries = compositions(&document());
        assert_eq!(
            entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            [1, 2],
            "id order, not insertion order"
        );
        let main = entries.iter().find(|e| e.name == "Main").expect("Main");
        assert!(main.root, "the first composition added became the root");
        assert_eq!(main.duration_frames, 300);
        assert_eq!(main.frame_rate, [30, 1]);
    }

    /// The parameter listing is `ExposedListing`'s own form; the test that
    /// pins its shape lives with it. What matters here is that nothing
    /// re-wraps it.
    #[test]
    fn the_parameter_listing_is_the_core_contract() {
        let json = params_json(&document()).expect("serializes");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(
            value.get("parameters").is_some(),
            "the listing keeps its own top-level key: {json}"
        );
    }

    /// Every row is listed, including the ones that cannot be used, and each
    /// carries either a route or a reason.
    #[test]
    fn unusable_codecs_are_listed_with_a_reason() {
        let rows = codecs(&available_encoders());
        assert_eq!(rows.len(), OutputFormat::ALL.len(), "no row is dropped");
        for row in &rows {
            assert_eq!(
                row.route.is_some(),
                row.reason.is_none(),
                "{} must carry exactly one of a route and a reason",
                row.format
            );
            assert_eq!(row.message.is_some(), !row.available);
        }

        let png = rows.iter().find(|r| r.format == "png").expect("png");
        assert!(png.available && png.writable);
        assert_eq!(png.route.as_deref(), Some("native"));

        // H.265 is refused as policy on every machine, which is the one row
        // whose verdict does not depend on the environment.
        let h265 = rows.iter().find(|r| r.format == "h265").expect("h265");
        assert!(!h265.available);
        assert_eq!(h265.reason, Some("not-offered"));
        assert!(!h265.writable);
    }

    /// Video is enumerable but not yet writable, and the listing has to make
    /// the difference visible rather than implying the machine is at fault.
    #[test]
    fn video_rows_report_that_ravel_has_no_writer_for_them() {
        for row in codecs(&available_encoders()) {
            if row.kind == "video" {
                assert!(!row.writable, "{} claims a writer it has not", row.format);
            } else {
                assert!(row.writable, "{} must always be writable", row.format);
            }
        }
    }
}
