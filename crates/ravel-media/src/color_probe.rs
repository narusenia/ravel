// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Colour metadata of still images, read from the file header alone.
//!
//! FFmpeg reports nothing for EXR and PNG stills, so the import probe asks
//! this module instead (`docs/specifications/color-management.md`, tier 2 of
//! the input colour space resolution). No pixels are decoded, and any read
//! error yields `(None, None)`: an unknown or uninterpretable file falls
//! through to the extension default, it never fails the import and is never
//! guessed at.
//!
//! # Mapping
//!
//! Chromaticities (EXR `chromaticities`, PNG `cHRM`) match a known primaries
//! set when all four points (R, G, B, white) are within [`XY_TOLERANCE`] in
//! CIE xy. Known: Rec.709, Rec.2020, ACES AP1.
//!
//! | Source | Result |
//! |---|---|
//! | EXR, `colorInteropID` `lin_rec709_scene` / `lin_rec2020_scene` / `lin_ap1_scene` | `(Rec709 / Rec2020 / ApOne, Linear)` |
//! | EXR, `colorInteropID` `srgb_rec709_scene` | `(Rec709, Srgb)` |
//! | EXR, any other `colorInteropID` | `(None, None)` (supersedes `chromaticities`) |
//! | EXR, no id, `chromaticities` recognised | `(primaries, Linear)` (EXR is linear by convention) |
//! | EXR, attribute absent or unrecognised | `(None, None)` |
//! | PNG, `cICP` (highest priority, decides alone): primaries 1 / 9, transfer 8 / 13 / 1, 6, 14, 15 / 16, matrix 0, full range | `(Rec709 / Rec2020, Linear / Srgb / Rec709 / Pq)` |
//! | PNG, `cICP` with any other code, a non-zero matrix or narrow range | `(None, None)` |
//! | PNG, `iCCP` present | `(None, None)` (profiles are not parsed; `iCCP` overrides the rest) |
//! | PNG, `sRGB` | `(Rec709, Srgb)` |
//! | PNG, `gAMA` = 1.0 and `cHRM` absent | `(Rec709, Linear)` |
//! | PNG, `gAMA` = 1.0 and `cHRM` recognised | `(primaries, Linear)` |
//! | anything else (e.g. `gAMA` 1/2.2, which is not the sRGB curve) | `(None, None)` |
//!
//! `gAMA` = 1.0 without `cHRM` takes only the declared transfer: Rec.709 is
//! the PNG extension default anyway, so the result is never worse than the
//! default.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use ravel_core::color::{Primaries, Transfer};
use ravel_core::media::{MediaInfo, StreamInfo};

/// Largest CIE xy distance, per coordinate, still counted as the same primaries.
const XY_TOLERANCE: f32 = 0.002;
/// Largest distance from 1.0 still counted as a linear PNG `gAMA`.
const GAMMA_TOLERANCE: f32 = 0.001;

type Xy = (f32, f32);
/// Red, green, blue, white.
type Chromaticities = [Xy; 4];

const REC709: Chromaticities = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06), (0.3127, 0.3290)];
const REC2020: Chromaticities = [
    (0.708, 0.292),
    (0.170, 0.797),
    (0.131, 0.046),
    (0.3127, 0.3290),
];
const AP1: Chromaticities = [
    (0.713, 0.293),
    (0.165, 0.830),
    (0.128, 0.044),
    (0.32168, 0.33767),
];

/// The colour declared by `path`'s header, by extension. `(None, None)` for
/// other extensions and for anything unreadable.
pub fn probe_still(path: &Path) -> (Option<Primaries>, Option<Transfer>) {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("exr") => probe_exr(path),
        Some("png") => probe_png(path),
        _ => (None, None),
    }
}

/// Fill the first video stream's colour fields from `path`'s header, when the
/// probe left both undeclared. A container that declared either is left alone.
pub fn fill_still_color(info: &mut MediaInfo, path: &Path) {
    let Some(video) = info.streams.iter_mut().find_map(|s| match s {
        StreamInfo::Video(v) => Some(v),
        _ => None,
    }) else {
        return;
    };
    if video.color_primaries.is_none() && video.color_transfer.is_none() {
        (video.color_primaries, video.color_transfer) = probe_still(path);
    }
}

fn match_primaries(found: Chromaticities) -> Option<Primaries> {
    let close = |known: &Chromaticities| {
        found
            .iter()
            .zip(known)
            .all(|(a, b)| (a.0 - b.0).abs() <= XY_TOLERANCE && (a.1 - b.1).abs() <= XY_TOLERANCE)
    };
    [
        (REC709, Primaries::Rec709),
        (REC2020, Primaries::Rec2020),
        (AP1, Primaries::ApOne),
    ]
    .into_iter()
    .find(|(known, _)| close(known))
    .map(|(_, primaries)| primaries)
}

fn probe_exr(path: &Path) -> (Option<Primaries>, Option<Transfer>) {
    let Ok(meta) = exr::meta::MetaData::read_from_file(path, false) else {
        return (None, None);
    };
    // `colorInteropID` supersedes `chromaticities` when present, and an id
    // Ravel has no name for is unknown whatever the chromaticities say.
    if let Some(id) = meta.headers.first().and_then(|h| {
        h.own_attributes.other.iter().find_map(|(name, value)| {
            match (name.eq("colorInteropID"), value) {
                (true, exr::meta::attribute::AttributeValue::Text(t)) => Some(t.to_string()),
                _ => None,
            }
        })
    }) {
        return match id.as_str() {
            "lin_rec709_scene" => (Some(Primaries::Rec709), Some(Transfer::Linear)),
            "lin_rec2020_scene" => (Some(Primaries::Rec2020), Some(Transfer::Linear)),
            "lin_ap1_scene" => (Some(Primaries::ApOne), Some(Transfer::Linear)),
            "srgb_rec709_scene" => (Some(Primaries::Rec709), Some(Transfer::Srgb)),
            _ => (None, None),
        };
    }
    let primaries = meta
        .headers
        .first()
        .and_then(|h| h.shared_attributes.chromaticities)
        .and_then(|c| {
            let p = |v: exr::math::Vec2<f32>| (v.x(), v.y());
            match_primaries([p(c.red), p(c.green), p(c.blue), p(c.white)])
        });
    match primaries {
        Some(p) => (Some(p), Some(Transfer::Linear)),
        None => (None, None),
    }
}

/// H.273 code points (`cICP`) onto Ravel's vocabulary. Only RGB, full-range
/// signals with a code Ravel names are interpreted.
fn cicp(c: png::CodingIndependentCodePoints) -> (Option<Primaries>, Option<Transfer>) {
    let primaries = match c.color_primaries {
        1 => Primaries::Rec709,
        9 => Primaries::Rec2020,
        _ => return (None, None),
    };
    let transfer = match c.transfer_function {
        8 => Transfer::Linear,
        13 => Transfer::Srgb,
        1 | 6 | 14 | 15 => Transfer::Rec709,
        16 => Transfer::Pq,
        _ => return (None, None),
    };
    if c.matrix_coefficients != 0 || !c.is_video_full_range_image {
        return (None, None);
    }
    (Some(primaries), Some(transfer))
}

fn probe_png(path: &Path) -> (Option<Primaries>, Option<Transfer>) {
    let Ok(file) = File::open(path) else {
        return (None, None);
    };
    let Ok(reader) = png::Decoder::new(BufReader::new(file)).read_info() else {
        return (None, None);
    };
    let info = reader.info();
    // `cICP` outranks every other colour chunk and decides alone.
    if let Some(c) = info.coding_independent_code_points {
        return cicp(c);
    }
    if info.icc_profile.is_some() {
        return (None, None);
    }
    if info.srgb.is_some() {
        return (Some(Primaries::Rec709), Some(Transfer::Srgb));
    }
    let linear = info
        .gama_chunk
        .is_some_and(|g| (g.into_value() - 1.0).abs() <= GAMMA_TOLERANCE);
    if !linear {
        return (None, None);
    }
    let primaries = match info.chrm_chunk {
        None => Some(Primaries::Rec709),
        Some(c) => {
            let p = |v: (png::ScaledFloat, png::ScaledFloat)| (v.0.into_value(), v.1.into_value());
            match_primaries([p(c.red), p(c.green), p(c.blue), p(c.white)])
        }
    };
    match primaries {
        Some(p) => (Some(p), Some(Transfer::Linear)),
        None => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufWriter;

    use png::{ScaledFloat, SourceChromaticities, SrgbRenderingIntent};

    fn write_png(path: &Path, configure: impl FnOnce(&mut png::Encoder<'_, BufWriter<File>>)) {
        let mut enc = png::Encoder::new(BufWriter::new(File::create(path).unwrap()), 1, 1);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        configure(&mut enc);
        enc.write_header()
            .unwrap()
            .write_image_data(&[0, 0, 0])
            .unwrap();
    }

    fn png_probe(
        configure: impl FnOnce(&mut png::Encoder<'_, BufWriter<File>>),
    ) -> (Option<Primaries>, Option<Transfer>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        write_png(&path, configure);
        probe_still(&path)
    }

    fn write_exr(path: &Path, chromaticities: Option<exr::meta::attribute::Chromaticities>) {
        write_exr_with_id(path, chromaticities, None);
    }

    fn write_exr_with_id(
        path: &Path,
        chromaticities: Option<exr::meta::attribute::Chromaticities>,
        interop_id: Option<&str>,
    ) {
        use exr::prelude::*;
        let mut layer_attrs = LayerAttributes::default();
        if let Some(id) = interop_id {
            layer_attrs.other.insert(
                Text::new_or_panic("colorInteropID"),
                AttributeValue::Text(Text::new_or_panic(id)),
            );
        }
        let layer = Layer::new(
            (2, 2),
            layer_attrs,
            Encoding::FAST_LOSSLESS,
            SpecificChannels::rgb(|_| (0.5_f32, 0.5_f32, 0.5_f32)),
        );
        let mut attrs = ImageAttributes::new(IntegerBounds::from_dimensions((2, 2)));
        attrs.chromaticities = chromaticities;
        let mut image = Image::from_layer(layer);
        image.attributes = attrs;
        image.write().to_file(path).unwrap();
    }

    fn exr_chroma(c: Chromaticities) -> exr::meta::attribute::Chromaticities {
        let v = |(x, y): Xy| exr::math::Vec2(x, y);
        exr::meta::attribute::Chromaticities {
            red: v(c[0]),
            green: v(c[1]),
            blue: v(c[2]),
            white: v(c[3]),
        }
    }

    #[test]
    fn exr_rec2020_chromaticities_are_linear_rec2020() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.exr");
        write_exr(&path, Some(exr_chroma(REC2020)));
        assert_eq!(
            probe_still(&path),
            (Some(Primaries::Rec2020), Some(Transfer::Linear))
        );
    }

    #[test]
    fn exr_without_chromaticities_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.exr");
        write_exr(&path, None);
        assert_eq!(probe_still(&path), (None, None));
    }

    #[test]
    fn exr_with_unrecognised_chromaticities_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.exr");
        let mut odd = REC709;
        odd[1] = (0.2, 0.7);
        write_exr(&path, Some(exr_chroma(odd)));
        assert_eq!(probe_still(&path), (None, None));
    }

    #[test]
    fn exr_known_interop_id_names_the_space() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.exr");
        write_exr_with_id(&path, None, Some("lin_ap1_scene"));
        assert_eq!(
            probe_still(&path),
            (Some(Primaries::ApOne), Some(Transfer::Linear))
        );
        write_exr_with_id(&path, None, Some("srgb_rec709_scene"));
        assert_eq!(
            probe_still(&path),
            (Some(Primaries::Rec709), Some(Transfer::Srgb))
        );
    }

    #[test]
    fn exr_unknown_interop_id_is_unknown_despite_chromaticities() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.exr");
        write_exr_with_id(&path, Some(exr_chroma(REC709)), Some("something_else"));
        assert_eq!(probe_still(&path), (None, None));
    }

    #[test]
    fn exr_interop_id_beats_conflicting_chromaticities() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.exr");
        write_exr_with_id(&path, Some(exr_chroma(REC709)), Some("lin_rec2020_scene"));
        assert_eq!(
            probe_still(&path),
            (Some(Primaries::Rec2020), Some(Transfer::Linear))
        );
    }

    /// Write a PNG, then splice a `cICP` chunk in after `IHDR` (the encoder
    /// cannot write one).
    fn png_with_cicp(
        configure: impl FnOnce(&mut png::Encoder<'_, BufWriter<File>>),
        code: [u8; 4],
    ) -> (Option<Primaries>, Option<Transfer>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        write_png(&path, configure);
        let mut bytes = std::fs::read(&path).unwrap();
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&4u32.to_be_bytes());
        let mut body = b"cICP".to_vec();
        body.extend_from_slice(&code);
        chunk.extend_from_slice(&body);
        chunk.extend_from_slice(&crc32(&body).to_be_bytes());
        // signature (8) + IHDR (4 length + 4 type + 13 data + 4 crc)
        bytes.splice(33..33, chunk);
        std::fs::write(&path, bytes).unwrap();
        probe_still(&path)
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn png_cicp_rec2020_pq() {
        assert_eq!(
            png_with_cicp(|_| {}, [9, 16, 0, 1]),
            (Some(Primaries::Rec2020), Some(Transfer::Pq))
        );
    }

    #[test]
    fn png_cicp_overrides_a_co_present_srgb_chunk() {
        let got = png_with_cicp(
            |e| e.set_source_srgb(SrgbRenderingIntent::Perceptual),
            [9, 8, 0, 1],
        );
        assert_eq!(got, (Some(Primaries::Rec2020), Some(Transfer::Linear)));
    }

    #[test]
    fn png_cicp_unknown_or_unsupported_codes_are_unknown() {
        assert_eq!(png_with_cicp(|_| {}, [5, 13, 0, 1]), (None, None));
        assert_eq!(png_with_cicp(|_| {}, [1, 99, 0, 1]), (None, None));
        assert_eq!(png_with_cicp(|_| {}, [1, 13, 0, 0]), (None, None));
        assert_eq!(png_with_cicp(|_| {}, [1, 13, 1, 1]), (None, None));
        // unknown cICP still wins over a co-present sRGB chunk
        let got = png_with_cicp(
            |e| e.set_source_srgb(SrgbRenderingIntent::Perceptual),
            [5, 13, 0, 1],
        );
        assert_eq!(got, (None, None));
    }

    #[test]
    fn png_srgb_chunk_is_srgb_rec709() {
        let got = png_probe(|e| e.set_source_srgb(SrgbRenderingIntent::Perceptual));
        assert_eq!(got, (Some(Primaries::Rec709), Some(Transfer::Srgb)));
    }

    #[test]
    fn png_linear_gamma_is_linear_rec709() {
        let got = png_probe(|e| e.set_source_gamma(ScaledFloat::new(1.0)));
        assert_eq!(got, (Some(Primaries::Rec709), Some(Transfer::Linear)));
    }

    #[test]
    fn png_linear_gamma_with_rec2020_chrm_is_linear_rec2020() {
        let got = png_probe(|e| {
            e.set_source_gamma(ScaledFloat::new(1.0));
            e.set_source_chromaticities(SourceChromaticities::new(
                REC2020[3], REC2020[0], REC2020[1], REC2020[2],
            ));
        });
        assert_eq!(got, (Some(Primaries::Rec2020), Some(Transfer::Linear)));
    }

    #[test]
    fn png_power_gamma_alone_is_unknown() {
        let got = png_probe(|e| e.set_source_gamma(ScaledFloat::new(1.0 / 2.2)));
        assert_eq!(got, (None, None));
    }

    #[test]
    fn png_without_colour_chunks_is_unknown() {
        assert_eq!(png_probe(|_| {}), (None, None));
    }

    #[test]
    fn png_iccp_overrides_linear_gamma_and_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        let mut info = png::Info::with_size(1, 1);
        info.color_type = png::ColorType::Rgb;
        info.icc_profile = Some(std::borrow::Cow::Borrowed(&[0u8; 8]));
        info.source_gamma = Some(ScaledFloat::new(1.0));
        let enc =
            png::Encoder::with_info(BufWriter::new(File::create(&path).unwrap()), info).unwrap();
        enc.write_header()
            .unwrap()
            .write_image_data(&[0, 0, 0])
            .unwrap();
        assert_eq!(probe_still(&path), (None, None));
    }

    /// The import probes a sequence through its representative (first) frame
    /// (`frame_path(start_frame)`); that frame's header is what counts, not a
    /// later one's.
    #[test]
    fn sequence_reads_the_representative_frame() {
        use ravel_core::media::{ContainerFormat, VideoStreamInfo};
        let dir = tempfile::tempdir().unwrap();
        write_exr(&dir.path().join("shot_0001.exr"), Some(exr_chroma(REC2020)));
        write_exr(&dir.path().join("shot_0002.exr"), None);
        let seq = crate::image_seq::detect_sequence(&dir.path().join("shot_0002.exr")).unwrap();
        let mut info = MediaInfo {
            container: None::<ContainerFormat>,
            container_name: String::new(),
            streams: vec![StreamInfo::Video(VideoStreamInfo {
                stream_index: 0,
                codec: None,
                codec_name: "exr".into(),
                width: 2,
                height: 2,
                frame_rate: ravel_core::types::FrameRate::new(24, 1),
                frame_count: None,
                duration_secs: None,
                pixel_format: String::new(),
                color_primaries: None,
                color_transfer: None,
                color_matrix: None,
            })],
            duration_secs: None,
        };
        fill_still_color(&mut info, &seq.frame_path(seq.start_frame));
        let v = info.first_video().unwrap();
        assert_eq!(
            (v.color_primaries, v.color_transfer),
            (Some(Primaries::Rec2020), Some(Transfer::Linear))
        );
    }

    #[test]
    fn unreadable_or_other_files_are_unknown() {
        assert_eq!(probe_still(Path::new("/nonexistent/a.png")), (None, None));
        assert_eq!(probe_still(Path::new("/nonexistent/a.exr")), (None, None));
        assert_eq!(probe_still(Path::new("a.jpg")), (None, None));
    }
}
