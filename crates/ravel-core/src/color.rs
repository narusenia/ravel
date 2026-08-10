// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Colour spaces, transfer functions, primary matrices, 3D LUTs, and the one
//! shared quantiser (`docs/implementation/color-management-plan.md`, CM-1).
//!
//! # Two axes, never one enum
//!
//! A colour space is [`Primaries`] **and** [`Transfer`], kept apart. Folding
//! them into a single `enum ColorSpace { Srgb, Rec2020, AcesCg, … }` hides
//! the question the rest of the pipeline has to answer — *is this value
//! encoded or linear?* — inside a name, and the ingest path (`CM-2`) and the
//! OCIO path (`CM-7`) both need it in the type.
//!
//! Ravel's working space is [`ColorSpace::LINEAR_REC709`]: Rec.709 primaries
//! with the transfer function removed. Compositing, blending, opacity and
//! blur happen there; the display and output transforms convert out of it.
//!
//! # Purity
//!
//! This module performs no I/O. [`CubeLut::parse`] takes the file's text, not
//! its path — the caller reads the file. The one piece of state is
//! [`to_display_rgba8`]'s lazily built boundary table, which is a memo of a
//! pure function of compile-time constants: it changes no answer, only how
//! long the answer takes.
//!
//! # Out-of-domain values
//!
//! Every transfer function accepts the whole real line. Negative inputs use
//! the odd extension (`-f(-v)`) and values above 1.0 continue the encoded
//! branch, so the functions stay monotonic and finite for the out-of-range
//! values a 32-bit float compositor routinely carries. Nothing here panics
//! and nothing here clamps — only [`quantize_u8`] / [`quantize_u16`] clamp,
//! because a byte has nowhere to put 1.4.
//!
//! # Chromatic adaptation is deferred
//!
//! [`Primaries::rgb_to_xyz`] carries each set's native white point (D65 for
//! Rec.709 and Rec.2020, ACES white for AP1), and [`primaries_matrix`]
//! composes them without a chromatic adaptation transform. Every conversion
//! Ravel performs today is Rec.709 → Rec.709, where the matrix is the
//! identity and the question does not arise; the Bradford adaptation a real
//! AP1 conversion needs arrives with OCIO in `CM-7`.

// ===========================================================================
// Transfer
// ===========================================================================

/// An opto-electronic transfer function: the encoding a colour space applies
/// on top of linear light.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Transfer {
    /// No encoding — the value *is* linear light. The working space.
    #[default]
    Linear,
    /// IEC 61966-2-1 sRGB: a linear segment near black, a 2.4 power curve
    /// above it.
    Srgb,
    /// ITU-R BT.709 camera OETF: a linear segment near black, a 0.45 power
    /// curve above it.
    Rec709,
    /// SMPTE ST 2084 (perceptual quantiser), normalised so `1.0` is
    /// 10 000 cd/m².
    Pq,
}

/// sRGB: slope of the linear segment.
const SRGB_PHI: f64 = 12.92;
/// sRGB: the linear value where the segments meet.
const SRGB_LINEAR_BREAK: f64 = 0.003_130_8;
/// sRGB: the encoded value where the segments meet.
///
/// Deliberately `SRGB_PHI * SRGB_LINEAR_BREAK` rather than the rounded
/// `0.04045` the standard prints. The two published constants do not name
/// the same point (`12.92 × 0.0031308 = 0.04044994`), so using both makes
/// [`Transfer::decode`] and [`Transfer::encode`] disagree by up to 5e-6 for
/// inputs in the gap. Deriving one from the other makes them exact inverses
/// and moves the whole error into a single bounded step at the seam, which
/// `srgb_seam_jump_is_bounded` pins.
const SRGB_ENCODED_BREAK: f64 = SRGB_PHI * SRGB_LINEAR_BREAK;
const SRGB_ALPHA: f64 = 0.055;
const SRGB_GAMMA: f64 = 2.4;

/// Rec.709: slope of the linear segment.
const REC709_PHI: f64 = 4.5;
const REC709_LINEAR_BREAK: f64 = 0.018;
/// Rec.709: the encoded value where the segments meet, derived from the
/// linear break for the same reason as [`SRGB_ENCODED_BREAK`].
const REC709_ENCODED_BREAK: f64 = REC709_PHI * REC709_LINEAR_BREAK;
/// Rec.709: the power-branch scale, **derived** rather than the standard's
/// printed `1.099`.
///
/// BT.709 rounds its constants far more coarsely than sRGB does: with
/// `1.099` the power branch evaluates to `0.081284` at the `0.018` break
/// while the linear branch gives `0.081`, a step of 2.8e-4 that costs 5.5e-5
/// on a round trip through the seam — fifty times the criterion. `alpha` is
/// therefore solved from the break point instead,
/// `alpha = (1 - phi·x0) / (1 - x0^gamma)`, which makes the two branches
/// meet exactly and the pair exact inverses. `alpha - beta` stays 1, so
/// `encode(1.0)` is still `1.0`; the largest deviation from the printed
/// constants anywhere in `0..=1` is under 1e-4, well inside a 16-bit code.
/// `rec709_branches_meet_at_the_break` pins the derivation.
const REC709_ALPHA: f64 = 1.099_296_1;
const REC709_BETA: f64 = REC709_ALPHA - 1.0;
const REC709_GAMMA: f64 = 0.45;

/// ST 2084 constants (`m1`, `m2`, `c1`, `c2`, `c3`).
const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;
/// `dY/dN` of the PQ EOTF at `N = 1`.
///
/// ST 2084 is defined on `0..=1` and has a **pole** just past it: the
/// denominator `c2 - c3·N^(1/m2)` reaches zero at `N ≈ 1.992`, so the naive
/// formula returns a negative value and then an infinity for inputs a float
/// compositor can perfectly well produce. Above 1 both directions therefore
/// continue as a straight line at the curve's own slope — monotonic, finite,
/// and still exactly invertible. `pq_extrapolation_slope_matches_the_curve`
/// pins this constant against a numerical derivative.
const PQ_SLOPE_AT_ONE: f64 = 9.554_2;

/// Extend `f` — defined for non-negative inputs — to the whole real line as
/// an odd function. Keeps monotonicity and never produces a NaN from a
/// negative base in `powf`.
fn odd(v: f64, f: impl Fn(f64) -> f64) -> f64 {
    if v < 0.0 { -f(-v) } else { f(v) }
}

impl Transfer {
    /// Encoded value → linear light.
    pub fn decode(self, value: f32) -> f32 {
        self.decode_f64(f64::from(value)) as f32
    }

    /// Linear light → encoded value.
    pub fn encode(self, value: f32) -> f32 {
        self.encode_f64(f64::from(value)) as f32
    }

    /// The `f64` kernel. Every transfer function is evaluated in double
    /// precision and rounded once on the way out: PQ raises to the power
    /// 78.84, which in `f32` alone loses enough digits to break the
    /// round-trip criterion.
    fn decode_f64(self, value: f64) -> f64 {
        match self {
            Self::Linear => value,
            Self::Srgb => odd(value, |v| {
                if v <= SRGB_ENCODED_BREAK {
                    v / SRGB_PHI
                } else {
                    ((v + SRGB_ALPHA) / (1.0 + SRGB_ALPHA)).powf(SRGB_GAMMA)
                }
            }),
            Self::Rec709 => odd(value, |v| {
                if v <= REC709_ENCODED_BREAK {
                    v / REC709_PHI
                } else {
                    ((v + REC709_BETA) / REC709_ALPHA).powf(1.0 / REC709_GAMMA)
                }
            }),
            Self::Pq => odd(value, |v| {
                if v > 1.0 {
                    return 1.0 + (v - 1.0) * PQ_SLOPE_AT_ONE;
                }
                let e = v.powf(1.0 / PQ_M2);
                let num = (e - PQ_C1).max(0.0);
                let den = PQ_C2 - PQ_C3 * e;
                (num / den).powf(1.0 / PQ_M1)
            }),
        }
    }

    fn encode_f64(self, value: f64) -> f64 {
        match self {
            Self::Linear => value,
            Self::Srgb => odd(value, |v| {
                if v <= SRGB_LINEAR_BREAK {
                    SRGB_PHI * v
                } else {
                    (1.0 + SRGB_ALPHA) * v.powf(1.0 / SRGB_GAMMA) - SRGB_ALPHA
                }
            }),
            Self::Rec709 => odd(value, |v| {
                if v <= REC709_LINEAR_BREAK {
                    REC709_PHI * v
                } else {
                    REC709_ALPHA * v.powf(REC709_GAMMA) - REC709_BETA
                }
            }),
            Self::Pq => odd(value, |v| {
                if v > 1.0 {
                    return 1.0 + (v - 1.0) / PQ_SLOPE_AT_ONE;
                }
                let y = v.powf(PQ_M1);
                ((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y)).powf(PQ_M2)
            }),
        }
    }

    /// Every variant, for exhaustive tests and UI enumeration.
    pub const ALL: [Self; 4] = [Self::Linear, Self::Srgb, Self::Rec709, Self::Pq];
}

// ===========================================================================
// Primaries
// ===========================================================================

/// A 3×3 matrix in row-major order, held in `f64` so a composed conversion
/// does not accumulate `f32` error before it reaches a pixel.
pub type Mat3 = [[f64; 3]; 3];

/// The chromaticity set of a colour space, independent of its encoding.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Primaries {
    /// ITU-R BT.709 / sRGB, D65. Ravel's working primaries.
    #[default]
    Rec709,
    /// ITU-R BT.2020, D65.
    Rec2020,
    /// ACES AP1 (what ACEScg uses), ACES white.
    ApOne,
}

impl Primaries {
    /// The RGB → CIE XYZ matrix for this set, at its native white point.
    pub fn rgb_to_xyz(self) -> Mat3 {
        match self {
            Self::Rec709 => [
                [0.412_390_799_3, 0.357_584_339_4, 0.180_480_788_4],
                [0.212_639_005_9, 0.715_168_678_8, 0.072_192_315_4],
                [0.019_330_818_7, 0.119_194_779_8, 0.950_532_152_2],
            ],
            Self::Rec2020 => [
                [0.636_958_048_3, 0.144_616_903_6, 0.168_880_975_1],
                [0.262_700_212_0, 0.677_998_071_9, 0.059_301_716_1],
                [0.0, 0.028_072_693_0, 1.060_985_057_7],
            ],
            Self::ApOne => [
                [0.662_454_181_7, 0.134_004_206_1, 0.156_187_687_4],
                [0.272_228_716_8, 0.674_081_566_1, 0.053_689_717_1],
                [-0.005_574_649_5, 0.004_060_733_6, 1.010_339_100_9],
            ],
        }
    }

    /// The CIE XYZ → RGB matrix for this set.
    pub fn xyz_to_rgb(self) -> Mat3 {
        invert(self.rgb_to_xyz()).expect("a primary matrix is always invertible")
    }

    /// Every variant, for exhaustive tests and UI enumeration.
    pub const ALL: [Self; 3] = [Self::Rec709, Self::Rec2020, Self::ApOne];
}

/// The matrix converting linear RGB in `from` to linear RGB in `to`.
///
/// The identity when the two agree — the only case the shipped pipeline
/// reaches, since the working space and every display Ravel targets today
/// are Rec.709.
pub fn primaries_matrix(from: Primaries, to: Primaries) -> Mat3 {
    if from == to {
        return IDENTITY;
    }
    multiply(to.xyz_to_rgb(), from.rgb_to_xyz())
}

/// The 3×3 identity.
pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// `a × b`.
pub fn multiply(a: Mat3, b: Mat3) -> Mat3 {
    let mut out = [[0.0f64; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (col, cell) in out_row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[row][k] * b[k][col]).sum();
        }
    }
    out
}

/// `m × v`.
pub fn apply(m: Mat3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// The inverse of `m`, or `None` when it is singular.
pub fn invert(m: Mat3) -> Option<Mat3> {
    let cofactor = |r: usize, c: usize| {
        let rows: Vec<usize> = (0..3).filter(|&i| i != r).collect();
        let cols: Vec<usize> = (0..3).filter(|&i| i != c).collect();
        let minor =
            m[rows[0]][cols[0]] * m[rows[1]][cols[1]] - m[rows[0]][cols[1]] * m[rows[1]][cols[0]];
        if (r + c).is_multiple_of(2) {
            minor
        } else {
            -minor
        }
    };
    let det = m[0][0] * cofactor(0, 0) + m[0][1] * cofactor(0, 1) + m[0][2] * cofactor(0, 2);
    if det.abs() < f64::EPSILON {
        return None;
    }
    let mut out = [[0.0f64; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (col, cell) in out_row.iter_mut().enumerate() {
            // Transposed on purpose: the inverse is the *adjugate* over the
            // determinant, and the adjugate is the transpose of the cofactor
            // matrix.
            *cell = cofactor(col, row) / det;
        }
    }
    Some(out)
}

// ===========================================================================
// ColorSpace
// ===========================================================================

/// A colour space: which primaries, and which encoding on top of them.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct ColorSpace {
    #[serde(default)]
    pub primaries: Primaries,
    #[serde(default)]
    pub transfer: Transfer,
}

impl ColorSpace {
    /// sRGB — Rec.709 primaries, sRGB transfer. What an 8-bit image file
    /// holds unless it says otherwise.
    pub const SRGB: Self = Self::new(Primaries::Rec709, Transfer::Srgb);
    /// Ravel's working space: Rec.709 primaries, no transfer function.
    pub const LINEAR_REC709: Self = Self::new(Primaries::Rec709, Transfer::Linear);
    /// Rec.709 video — Rec.709 primaries and the BT.709 camera OETF.
    pub const REC709: Self = Self::new(Primaries::Rec709, Transfer::Rec709);
    /// ACEScg — AP1 primaries, linear.
    pub const ACES_CG: Self = Self::new(Primaries::ApOne, Transfer::Linear);
    /// Rec.2020 with the PQ transfer (HDR10).
    pub const REC2020_PQ: Self = Self::new(Primaries::Rec2020, Transfer::Pq);

    /// The space compositing happens in. Every other space in the pipeline
    /// is defined by its conversion to and from this one.
    pub const WORKING: Self = Self::LINEAR_REC709;

    /// The space every 8-bit exit targets: the viewer, the PNG writer, and
    /// the video encoder. Fixed at sRGB until `CM-8` lets the user choose a
    /// display.
    pub const DISPLAY: Self = Self::SRGB;

    pub const fn new(primaries: Primaries, transfer: Transfer) -> Self {
        Self {
            primaries,
            transfer,
        }
    }

    /// Whether values in this space are linear light (no decode needed).
    pub fn is_linear(self) -> bool {
        self.transfer == Transfer::Linear
    }

    /// The named spaces, for parsing a metadata string and for naming one
    /// back. Only combinations that have a conventional name appear; an
    /// arbitrary `Primaries` × `Transfer` pair is still constructible, it
    /// just has nothing to be called.
    pub const NAMED: [(&'static str, Self); 5] = [
        ("srgb", Self::SRGB),
        ("linear_rec709", Self::LINEAR_REC709),
        ("rec709", Self::REC709),
        ("acescg", Self::ACES_CG),
        ("rec2020_pq", Self::REC2020_PQ),
    ];

    /// Parse a colour-space name as written in file metadata or a project
    /// file. Case- and separator-insensitive, with the aliases the formats
    /// Ravel ingests actually use (`bt709`, `scene-linear`, …).
    ///
    /// `None` for a name this build does not know — the caller then falls
    /// through to the next tier of the resolution order rather than guessing.
    pub fn from_name(name: &str) -> Option<Self> {
        let key: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        match key.as_str() {
            "srgb" | "iec6196621" => Some(Self::SRGB),
            "linear" | "linearrec709" | "scenelinear" | "lin" | "linsrgb" => {
                Some(Self::LINEAR_REC709)
            }
            "rec709" | "bt709" | "itu709" => Some(Self::REC709),
            "acescg" | "aces" | "acesap1" => Some(Self::ACES_CG),
            "rec2020pq" | "bt2020pq" | "pq" | "hdr10" | "smpte2084" => Some(Self::REC2020_PQ),
            _ => None,
        }
    }

    /// The canonical name of this space, when it has one.
    pub fn name(self) -> Option<&'static str> {
        Self::NAMED
            .iter()
            .find(|(_, space)| *space == self)
            .map(|(name, _)| *name)
    }

    /// Decode one RGB triple from this space into linear light with **this
    /// space's own primaries**. Use [`convert`] to reach another set.
    pub fn to_linear(self, rgb: [f32; 3]) -> [f32; 3] {
        rgb.map(|c| self.transfer.decode(c))
    }

    /// Encode one linear RGB triple, already in this space's primaries.
    pub fn from_linear(self, rgb: [f32; 3]) -> [f32; 3] {
        rgb.map(|c| self.transfer.encode(c))
    }
}

/// Convert one RGB triple from `from` to `to`: decode, rotate primaries,
/// re-encode.
///
/// Alpha is never passed through here. It carries no transfer function, so
/// the callers that hold RGBA convert the first three components and copy
/// the fourth (`docs/specifications/color-management.md`).
pub fn convert(rgb: [f32; 3], from: ColorSpace, to: ColorSpace) -> [f32; 3] {
    if from == to {
        return rgb;
    }
    let linear = from.to_linear(rgb);
    let rotated = if from.primaries == to.primaries {
        linear
    } else {
        let m = primaries_matrix(from.primaries, to.primaries);
        let v = apply(
            m,
            [
                f64::from(linear[0]),
                f64::from(linear[1]),
                f64::from(linear[2]),
            ],
        );
        [v[0] as f32, v[1] as f32, v[2] as f32]
    };
    to.from_linear(rotated)
}

// ===========================================================================
// Quantisation
// ===========================================================================

/// The one quantiser. Every 8-bit exit — the viewer, the PNG writer, the
/// FFmpeg encoder — goes through it.
///
/// Round to nearest, not truncate: truncation cannot map `1.0` to `255`, so
/// an 8-bit round trip through it loses the top code. Before `CM-1` the
/// viewer rounded and the FFmpeg encoder truncated, which put display and
/// video output one LSB apart (`HIGH-25`).
///
/// `NaN` survives `clamp` and saturates to `0` in the cast, which is the
/// behaviour the previous viewer conversion had and its golden test pins.
pub fn quantize_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Working space → the display space, quantised to 8 bit. **The** exit.
///
/// Encoding and quantisation belong together and in this order — separating
/// them is how the four exits drifted apart before `CM-1`
/// (`docs/specifications/color-management.md`).
///
/// The viewer takes that pair from here. The render exits reach the same
/// place by a different road: [`to_output_space`] encodes the frame while it
/// is still `f32` and the sequence writers then call [`quantize_u8`], because
/// a 16-bit or EXR exit needs the encoded float. **The two roads still agree
/// bit for bit** — [`DISPLAY_CODE_THRESHOLDS`] is bisected against exactly
/// `quantize_u8(transfer.encode(v))`, which is what that second road
/// computes, and `the_display_table_reproduces_the_transfer_function` is what
/// keeps it true. The agreement used to come from sharing the code; it now
/// comes from the table's construction, so that test is load-bearing.
///
/// Alpha is coverage, not light: quantised, never encoded.
///
/// [`to_output_space`]: crate::media::encode::to_output_space
///
/// # No `powf` per pixel
///
/// The colour channels come from [`DISPLAY_CODE_THRESHOLDS`], not from
/// evaluating the transfer function. The result is the same value the
/// `encode`-then-`quantize` pair produces — bit for bit, by construction of
/// the table — for a fraction of the cost. Numbers and conditions are in
/// `docs/implementation/perf-baseline.md`.
///
/// This shaves the CPU cost; it does not make the transform free. `CM-7`
/// bakes it into a GPU 3D LUT applied in the wgpu path, which removes the
/// per-pixel work from the CPU altogether — the table changes nothing about
/// that argument, because it changes nothing about the transform's
/// definition.
pub fn to_display_rgba8(rgba: [f32; 4]) -> [u8; 4] {
    let bounds: &[f32; 255] = &DISPLAY_CODE_THRESHOLDS;
    [
        display_code_u8(bounds, rgba[0]),
        display_code_u8(bounds, rgba[1]),
        display_code_u8(bounds, rgba[2]),
        quantize_u8(rgba[3]),
    ]
}

/// The smallest linear value that displays as code `k`, for `k = 1..=255`.
///
/// The colour channels of [`to_display_rgba8`] are a **monotonic** map from a
/// float onto 256 codes, so the transfer function only ever decides *how many
/// of 255 boundaries a value has passed*. Tabulating the boundaries replaces
/// the `powf` with one binary search over 255 floats.
///
/// The entries are deliberately **not** `decode((k - 0.5) / 255)`. That is the
/// ideal inverse, and the function being replaced is not ideal: it rounds the
/// encoded value to `f32`, and `quantize_u8` then rounds again in `f32`. Each
/// entry is instead the smallest `f32` that the original expression maps to
/// `k`, found by bisecting the **bit patterns** of `0.0..=1.0` — which are
/// ordered as integers for non-negative floats. Both steps of the original are
/// monotonic, so the table reproduces it exactly rather than approximately;
/// `the_display_table_reproduces_the_transfer_function` is the check.
///
/// Only the 8-bit exit can do this. [`to_display_rgba16`] has 65 535
/// boundaries, where the table stops being the cheap answer.
///
/// The table assumes the display and working spaces share primaries, so that
/// [`ColorSpace::from_linear`] is the transfer function alone with no matrix
/// in front of it. That holds for every display Ravel targets today and
/// `the_display_space_shares_the_working_primaries` fails the day it stops.
static DISPLAY_CODE_THRESHOLDS: std::sync::LazyLock<[f32; 255]> = std::sync::LazyLock::new(|| {
    let exact = |value: f32| quantize_u8(ColorSpace::DISPLAY.transfer.encode(value));
    std::array::from_fn(|index| {
        let code = (index + 1) as u8;
        let (mut low, mut high) = (0u32, 1.0f32.to_bits());
        while low < high {
            let mid = low + (high - low) / 2;
            if exact(f32::from_bits(mid)) < code {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        f32::from_bits(low)
    })
});

/// One linear colour channel → its display code.
///
/// `partition_point` counts the boundaries at or below `linear`, which *is*
/// the code. The out-of-domain cases fall out of the comparison rather than
/// needing a branch: a negative value and a `NaN` pass none of them and land
/// on `0`, anything at or above the last boundary lands on `255` — the same
/// answers [`quantize_u8`]'s clamp gives.
fn display_code_u8(bounds: &[f32; 255], linear: f32) -> u8 {
    bounds.partition_point(|&bound| bound <= linear) as u8
}

/// [`to_display_rgba8`] at 16 bits, for the deep PNG sequence.
pub fn to_display_rgba16(rgba: [f32; 4]) -> [u16; 4] {
    let encoded = ColorSpace::DISPLAY.from_linear([rgba[0], rgba[1], rgba[2]]);
    [
        quantize_u16(encoded[0]),
        quantize_u16(encoded[1]),
        quantize_u16(encoded[2]),
        quantize_u16(rgba[3]),
    ]
}

/// [`quantize_u8`] for 16-bit exits (the deep PNG sequence writer).
pub fn quantize_u16(value: f32) -> u16 {
    (value.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
}

/// The inverse: one 8-bit RGBA sample straight into the working space.
///
/// This is the ingest rule (`CM-2`) — normalise, then remove the transfer
/// function in the same step, so no encoded value ever reaches the graph.
/// It lives here rather than in the decoder because it is pure, and because
/// every ingest path has to agree on it exactly the way every exit has to
/// agree on [`quantize_u8`].
///
/// **Alpha is not converted.** Coverage carries no transfer function; running
/// it through one would change what "half covered" means.
pub fn ingest_rgba8(rgba: [u8; 4], input: ColorSpace) -> [f32; 4] {
    let norm = |b: u8| f32::from(b) / 255.0;
    let rgb = convert(
        [norm(rgba[0]), norm(rgba[1]), norm(rgba[2])],
        input,
        ColorSpace::WORKING,
    );
    [rgb[0], rgb[1], rgb[2], norm(rgba[3])]
}

// ===========================================================================
// 3D LUT (.cube)
// ===========================================================================

/// Why a `.cube` file could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CubeError {
    #[error("missing LUT_3D_SIZE")]
    MissingSize,
    #[error("unsupported LUT size {0} (2..=256)")]
    BadSize(usize),
    #[error("line {line}: expected three floats, found {found:?}")]
    BadEntry { line: usize, found: String },
    #[error("expected {expected} entries, found {found}")]
    WrongEntryCount { expected: usize, found: usize },
    #[error("LUT_1D_SIZE is not supported; this reader handles 3D LUTs only")]
    OneDimensional,
}

/// A 3D lookup table parsed from Adobe's `.cube` format.
///
/// Entries are stored in the file's own order — red varies fastest, then
/// green, then blue.
#[derive(Clone, Debug, PartialEq)]
pub struct CubeLut {
    size: usize,
    domain_min: [f32; 3],
    domain_max: [f32; 3],
    entries: Vec<[f32; 3]>,
}

impl CubeLut {
    /// Parse `.cube` text. Takes the text rather than a path: this module
    /// performs no I/O.
    pub fn parse(text: &str) -> Result<Self, CubeError> {
        let mut size = None;
        let mut domain_min = [0.0f32; 3];
        let mut domain_max = [1.0f32; 3];
        let mut entries = Vec::new();

        for (index, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let head = parts.next().unwrap_or("");
            match head {
                "TITLE" => continue,
                "LUT_1D_SIZE" => return Err(CubeError::OneDimensional),
                "LUT_3D_SIZE" => {
                    let n: usize = parts
                        .next()
                        .and_then(|v| v.parse().ok())
                        .ok_or(CubeError::MissingSize)?;
                    if !(2..=256).contains(&n) {
                        return Err(CubeError::BadSize(n));
                    }
                    size = Some(n);
                }
                "DOMAIN_MIN" | "DOMAIN_MAX" => {
                    let triple = parse_triple(parts, index + 1, line)?;
                    if head == "DOMAIN_MIN" {
                        domain_min = triple;
                    } else {
                        domain_max = triple;
                    }
                }
                _ => {
                    let triple = parse_triple(line.split_whitespace(), index + 1, line)?;
                    entries.push(triple);
                }
            }
        }

        let size = size.ok_or(CubeError::MissingSize)?;
        let expected = size * size * size;
        if entries.len() != expected {
            return Err(CubeError::WrongEntryCount {
                expected,
                found: entries.len(),
            });
        }
        Ok(Self {
            size,
            domain_min,
            domain_max,
            entries,
        })
    }

    /// Edge length of the cube.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The input range the table is defined over.
    pub fn domain(&self) -> ([f32; 3], [f32; 3]) {
        (self.domain_min, self.domain_max)
    }

    /// The grid entries in the file's order — red fastest, then green, then
    /// blue, so entry `r + size * (g + size * b)` is the point `(r, g, b)`.
    ///
    /// Exposed for `CM-7`, which uploads the table to the GPU as a texture and
    /// interpolates it in the display shader. [`Self::sample`] stays the
    /// definition the shader is checked against.
    pub fn entries(&self) -> &[[f32; 3]] {
        &self.entries
    }

    /// Sample the table with trilinear interpolation.
    ///
    /// Inputs outside the domain are clamped to the nearest edge of the
    /// cube — a LUT has no values beyond its own extent, and extrapolating a
    /// grade is how highlights turn into garbage.
    ///
    /// The cell index is clamped to `size - 2` so that `base + 1` is always a
    /// real grid point, and the fraction is then measured **from the clamped
    /// base** rather than from the unclamped floor. Measuring it from the floor
    /// is what made the top of the domain collapse: at `coord == size - 1` the
    /// base clamped down one cell while the fraction stayed `0`, so a size-3
    /// identity LUT mapped `1.0` to `0.5`. Only size-2 tables were covered, and
    /// there the two happen to agree.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let last = (self.size - 1) as f32;
        let mut base = [0usize; 3];
        let mut frac = [0f32; 3];
        for axis in 0..3 {
            let span = self.domain_max[axis] - self.domain_min[axis];
            let normalized = if span.abs() < f32::EPSILON {
                0.0
            } else {
                (rgb[axis] - self.domain_min[axis]) / span
            };
            let coord = (normalized.clamp(0.0, 1.0) * last).clamp(0.0, last);
            // `parse` refuses a size below 2, so `size - 2` cannot wrap.
            base[axis] = (coord as usize).min(self.size - 2);
            frac[axis] = coord - base[axis] as f32;
        }

        let mut out = [0f32; 3];
        for corner in 0..8 {
            let dr = corner & 1;
            let dg = (corner >> 1) & 1;
            let db = (corner >> 2) & 1;
            let weight =
                lerp_weight(frac[0], dr) * lerp_weight(frac[1], dg) * lerp_weight(frac[2], db);
            if weight == 0.0 {
                continue;
            }
            let entry = self.entry(base[0] + dr, base[1] + dg, base[2] + db);
            for axis in 0..3 {
                out[axis] += weight * entry[axis];
            }
        }
        out
    }

    /// The entry at grid coordinates, red fastest.
    fn entry(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.entries[r + self.size * (g + self.size * b)]
    }
}

fn lerp_weight(frac: f32, corner: usize) -> f32 {
    if corner == 0 { 1.0 - frac } else { frac }
}

fn parse_triple<'a>(
    parts: impl Iterator<Item = &'a str>,
    line: usize,
    raw: &str,
) -> Result<[f32; 3], CubeError> {
    // `collect` into a `Result`, not `filter_map`: dropping the tokens that
    // do not parse would read `0 foo 0 0` as a valid triple and put a LUT
    // entry the file never contained into the grade.
    let values = parts
        .map(|v| v.parse::<f32>())
        .collect::<Result<Vec<f32>, _>>()
        .map_err(|_| CubeError::BadEntry {
            line,
            found: raw.to_string(),
        })?;
    if values.len() != 3 {
        return Err(CubeError::BadEntry {
            line,
            found: raw.to_string(),
        });
    }
    Ok([values[0], values[1], values[2]])
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A spread that reaches both sides of every seam plus the out-of-range
    /// values a float compositor really carries.
    fn probe_values() -> Vec<f32> {
        let mut values = vec![
            -4.0,
            -1.0,
            -0.5,
            -0.05,
            -1e-6,
            0.0,
            1e-8,
            1e-4,
            0.0031308,
            0.018,
            0.04045,
            0.081,
            0.1,
            0.18,
            0.5,
            0.735_356_9,
            0.9,
            1.0,
            1.5,
            4.0,
            16.0,
        ];
        for step in 0..=256 {
            values.push(step as f32 / 256.0);
        }
        values
    }

    /// CM-1: "the transfer functions round-trip within 1e-6 for every input".
    #[test]
    fn transfer_round_trips_within_tolerance() {
        for transfer in Transfer::ALL {
            for value in probe_values() {
                // 1e-6 absolute inside the unit interval, scaled above it:
                // one `f32` ulp at 16.0 is already 1e-6, so an absolute
                // bound out there would measure the storage, not the maths.
                let tolerance = 1e-6 * value.abs().max(1.0);
                let back = transfer.decode(transfer.encode(value));
                assert!(
                    (back - value).abs() <= tolerance,
                    "{transfer:?}: {value} -> {} -> {back}",
                    transfer.encode(value)
                );
                let forward = transfer.encode(transfer.decode(value));
                assert!(
                    (forward - value).abs() <= tolerance,
                    "{transfer:?}: decode/encode of {value} gave {forward}"
                );
            }
        }
    }

    /// CM-1: the step at the piecewise seam is bounded.
    ///
    /// Continuity is *not* claimed for sRGB: `0.04045` and `0.0031308` are
    /// rounded and do not name the same point, so no conforming
    /// implementation is continuous there. What is asserted is the size of
    /// the step — evaluated as the difference between the two closed forms
    /// at the break, not as a finite difference across it, which would
    /// measure the slope rather than the jump.
    #[test]
    fn srgb_seam_jump_is_bounded() {
        let linear_side = SRGB_PHI * SRGB_LINEAR_BREAK;
        let power_side = (1.0 + SRGB_ALPHA) * SRGB_LINEAR_BREAK.powf(1.0 / SRGB_GAMMA) - SRGB_ALPHA;
        assert!(
            (linear_side - power_side).abs() < 1e-6,
            "sRGB encode seam steps by {}",
            power_side - linear_side
        );
        // And the decode threshold is the encode threshold, so the pair is
        // an exact inverse on both sides of the step.
        assert_eq!(SRGB_ENCODED_BREAK, linear_side);
    }

    /// The Rec.709 branches are made to meet exactly, because the standard's
    /// printed `1.099` leaves a 2.8e-4 step that breaks the round-trip
    /// criterion. This is the check that keeps [`REC709_ALPHA`] honest.
    #[test]
    fn rec709_branches_meet_at_the_break() {
        let linear_side = REC709_PHI * REC709_LINEAR_BREAK;
        let power_side = REC709_ALPHA * REC709_LINEAR_BREAK.powf(REC709_GAMMA) - REC709_BETA;
        assert!(
            (linear_side - power_side).abs() < 1e-6,
            "Rec.709 encode seam steps by {}",
            power_side - linear_side
        );
        // The offset preserves `encode(1.0) == 1.0`.
        assert!((Transfer::Rec709.encode(1.0) - 1.0).abs() < 1e-6);
    }

    /// [`PQ_SLOPE_AT_ONE`] is a hand-solved constant; this is what stops it
    /// drifting away from the curve it continues.
    #[test]
    fn pq_extrapolation_slope_matches_the_curve() {
        let eps = 1e-4f64;
        let numeric = (Transfer::Pq.decode_f64(1.0) - Transfer::Pq.decode_f64(1.0 - eps)) / eps;
        assert!(
            (numeric - PQ_SLOPE_AT_ONE).abs() / PQ_SLOPE_AT_ONE < 1e-3,
            "numerical slope {numeric} vs constant {PQ_SLOPE_AT_ONE}"
        );
        assert!((Transfer::Pq.decode(1.0) - 1.0).abs() < 1e-6);
    }

    /// CM-1: no panic and no folded ordering outside `0..=1`.
    #[test]
    fn transfer_is_monotonic_outside_the_unit_interval() {
        for transfer in Transfer::ALL {
            let mut previous_encode = f32::NEG_INFINITY;
            let mut previous_decode = f32::NEG_INFINITY;
            let mut value = -8.0f32;
            while value <= 8.0 {
                let encoded = transfer.encode(value);
                let decoded = transfer.decode(value);
                assert!(
                    encoded.is_finite(),
                    "{transfer:?} encode({value}) = {encoded}"
                );
                assert!(
                    decoded.is_finite(),
                    "{transfer:?} decode({value}) = {decoded}"
                );
                assert!(
                    encoded >= previous_encode,
                    "{transfer:?} encode is not monotonic at {value}"
                );
                assert!(
                    decoded >= previous_decode,
                    "{transfer:?} decode is not monotonic at {value}"
                );
                previous_encode = encoded;
                previous_decode = decoded;
                value += 1.0 / 64.0;
            }
        }
    }

    /// The number the CM-3 regression test is built on: a 50 % composite of
    /// black and white is `0.5` linear, which displays as sRGB 0.7354 — code
    /// 188, not 128.
    #[test]
    fn linear_half_displays_as_188() {
        let encoded = Transfer::Srgb.encode(0.5);
        assert!((encoded - 0.735_356_9).abs() < 1e-6, "{encoded}");
        assert_eq!(quantize_u8(encoded), 188);
    }

    /// CM-1: the primary matrices round-trip to the identity.
    #[test]
    fn primary_matrices_round_trip() {
        for primaries in Primaries::ALL {
            let round = multiply(primaries.xyz_to_rgb(), primaries.rgb_to_xyz());
            for (row, expected_row) in round.iter().zip(IDENTITY.iter()) {
                for (cell, expected) in row.iter().zip(expected_row.iter()) {
                    assert!((cell - expected).abs() < 1e-9, "{round:?}");
                }
            }
        }
        for from in Primaries::ALL {
            for to in Primaries::ALL {
                let there = primaries_matrix(from, to);
                let back = primaries_matrix(to, from);
                let round = multiply(back, there);
                for (row, expected_row) in round.iter().zip(IDENTITY.iter()) {
                    for (cell, expected) in row.iter().zip(expected_row.iter()) {
                        assert!(
                            (cell - expected).abs() < 1e-9,
                            "{from:?} -> {to:?}: {round:?}"
                        );
                    }
                }
            }
        }
        assert_eq!(
            primaries_matrix(Primaries::Rec709, Primaries::Rec709),
            IDENTITY
        );
    }

    /// CM-1: every `Primaries` × `Transfer` pair constructs and round-trips.
    #[test]
    fn every_colour_space_combination_round_trips() {
        for primaries in Primaries::ALL {
            for transfer in Transfer::ALL {
                let space = ColorSpace::new(primaries, transfer);
                assert_eq!(space.primaries, primaries);
                assert_eq!(space.transfer, transfer);
                assert_eq!(space.is_linear(), transfer == Transfer::Linear);
                for other_primaries in Primaries::ALL {
                    for other_transfer in Transfer::ALL {
                        let other = ColorSpace::new(other_primaries, other_transfer);
                        let source = [0.2f32, 0.55, 0.8];
                        let there = convert(source, space, other);
                        let back = convert(there, other, space);
                        for (a, b) in back.iter().zip(source.iter()) {
                            assert!(
                                (a - b).abs() < 1e-5,
                                "{space:?} -> {other:?} -> back gave {back:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn names_round_trip_and_aliases_resolve() {
        for (name, space) in ColorSpace::NAMED {
            assert_eq!(ColorSpace::from_name(name), Some(space));
            assert_eq!(space.name(), Some(name));
        }
        assert_eq!(ColorSpace::from_name("sRGB"), Some(ColorSpace::SRGB));
        assert_eq!(ColorSpace::from_name("BT.709"), Some(ColorSpace::REC709));
        assert_eq!(
            ColorSpace::from_name("scene-linear"),
            Some(ColorSpace::LINEAR_REC709)
        );
        assert_eq!(ColorSpace::from_name("gremlin"), None);
    }

    #[test]
    fn convert_between_identical_spaces_is_the_identity() {
        let source = [0.13f32, 0.0, 1.7];
        assert_eq!(convert(source, ColorSpace::SRGB, ColorSpace::SRGB), source);
    }

    #[test]
    fn srgb_to_working_space_decodes_the_transfer_only() {
        let converted = convert([0.5, 0.5, 0.5], ColorSpace::SRGB, ColorSpace::WORKING);
        for channel in converted {
            assert!((channel - 0.214_041_14).abs() < 1e-6, "{channel}");
        }
    }

    /// CM-1: quantisation at the boundaries and the midpoint.
    #[test]
    fn quantisation_boundaries() {
        assert_eq!(quantize_u8(0.0), 0);
        assert_eq!(quantize_u8(1.0), 255);
        assert_eq!(quantize_u8(-1.0), 0);
        assert_eq!(quantize_u8(2.0), 255);
        assert_eq!(quantize_u8(0.5), 128);
        // Rounding, not truncation: half a code up lands on the next code.
        assert_eq!(quantize_u8(0.5 / 255.0), 1);
        assert_eq!(quantize_u8(0.49 / 255.0), 0);
        assert_eq!(quantize_u8(254.5 / 255.0), 255);
        assert_eq!(quantize_u8(f32::NAN), 0);

        assert_eq!(quantize_u16(0.0), 0);
        assert_eq!(quantize_u16(1.0), 65535);
        assert_eq!(quantize_u16(0.5), 32768);
        assert_eq!(quantize_u16(-3.0), 0);
        assert_eq!(quantize_u16(f32::NAN), 0);
    }

    /// CM-2: an sRGB sample arrives linear, a linear sample is untouched
    /// (no double application), and alpha is never converted.
    #[test]
    fn ingest_decodes_only_the_colour_channels() {
        let srgb = ingest_rgba8([128, 128, 128, 128], ColorSpace::SRGB);
        for channel in &srgb[..3] {
            assert!((channel - 0.215_860_5).abs() < 1e-5, "{srgb:?}");
        }
        assert!(
            (srgb[3] - 128.0 / 255.0).abs() < 1e-6,
            "alpha was converted"
        );

        // Already linear: normalised and nothing else.
        let linear = ingest_rgba8([128, 128, 128, 128], ColorSpace::LINEAR_REC709);
        for channel in linear {
            assert!((channel - 128.0 / 255.0).abs() < 1e-6, "{linear:?}");
        }

        // The endpoints are fixed points of every transfer function, so an
        // opaque black or white pixel is unmoved whichever space it is in.
        assert_eq!(
            ingest_rgba8([0, 0, 0, 255], ColorSpace::SRGB),
            [0.0, 0.0, 0.0, 1.0]
        );
        assert_eq!(
            ingest_rgba8([255, 255, 255, 255], ColorSpace::SRGB),
            [1.0, 1.0, 1.0, 1.0]
        );
    }

    /// The ingest and the 8-bit exit are exact inverses, which is what makes
    /// CM-4's "import a PNG, export a PNG, get the same bytes" hold.
    #[test]
    fn ingest_and_display_round_trip_every_code() {
        for code in 0..=255u8 {
            let ingested = ingest_rgba8([code, code, code, code], ColorSpace::SRGB);
            assert_eq!(
                to_display_rgba8(ingested),
                [code; 4],
                "code {code} did not survive ingest -> display"
            );
        }
    }

    /// CM-3's regression number, at the exit: a 50 % composite of black and
    /// white is `0.5` in linear light and displays as 188, not 128.
    #[test]
    fn a_half_composite_displays_as_188() {
        assert_eq!(to_display_rgba8([0.5, 0.5, 0.5, 1.0]), [188, 188, 188, 255]);
        assert_eq!(to_display_rgba8([0.0, 1.0, 0.5, 0.5]), [0, 255, 188, 128]);
        // Alpha is quantised, never encoded — otherwise 0.5 coverage would
        // display as 188 too.
        assert_eq!(to_display_rgba8([0.0; 4])[3], 0);
        assert_eq!(to_display_rgba16([0.5, 0.5, 0.5, 0.5])[3], 32768);
    }

    /// What [`to_display_rgba8`] did before the boundary table: evaluate the
    /// transfer function, then quantise. Spelled out from the public
    /// primitives so the table is checked against the definition rather than
    /// against a copy of itself.
    fn encode_then_quantize(rgba: [f32; 4]) -> [u8; 4] {
        let encoded = ColorSpace::DISPLAY.from_linear([rgba[0], rgba[1], rgba[2]]);
        [
            quantize_u8(encoded[0]),
            quantize_u8(encoded[1]),
            quantize_u8(encoded[2]),
            quantize_u8(rgba[3]),
        ]
    }

    /// The boundary table is built from the transfer function alone, with no
    /// primary matrix in front of it. That is only the display transform
    /// while the two spaces share primaries.
    #[test]
    fn the_display_space_shares_the_working_primaries() {
        assert_eq!(
            ColorSpace::DISPLAY.primaries,
            ColorSpace::WORKING.primaries,
            "to_display_rgba8's table skips the primary matrix; a display \
             space with other primaries needs the matrix back"
        );
    }

    /// The differential test the boundary table stands on: the table must
    /// agree with `encode`-then-`quantise` **exactly**, not within a code.
    ///
    /// The interesting inputs are the boundaries themselves — that is where a
    /// table built from the ideal inverse rather than from the real function
    /// would drift by one code — so every boundary is probed together with its
    /// two `f32` neighbours. The rest is breadth: the 256 codes an sRGB round
    /// trip produces, the out-of-domain values a float compositor carries
    /// (negatives, above one, subnormals, infinities, `NaN`), and 200 000
    /// pseudo-random floats from a fixed seed.
    #[test]
    fn the_display_table_reproduces_the_transfer_function() {
        let check = |value: f32, what: &str| {
            let rgba = [value, value, value, 0.5];
            let table = to_display_rgba8(rgba);
            let reference = encode_then_quantize(rgba);
            assert_eq!(
                table,
                reference,
                "{what}: {value} (bits {:#010x}) gave {table:?}, expected {reference:?}",
                value.to_bits()
            );
        };

        for bound in DISPLAY_CODE_THRESHOLDS.iter().copied() {
            check(bound, "boundary");
            check(f32::from_bits(bound.to_bits() - 1), "just below a boundary");
            check(f32::from_bits(bound.to_bits() + 1), "just above a boundary");
        }

        for code in 0..=255u8 {
            check(ingest_rgba8([code; 4], ColorSpace::SRGB)[0], "sRGB code");
            check(f32::from(code) / 255.0, "linear ramp");
        }

        for value in [
            0.0,
            -0.0,
            -1e-30,
            -0.5,
            -4.0,
            1.0,
            1.0 + f32::EPSILON,
            1.5,
            1e30,
            f32::MIN_POSITIVE,
            f32::MIN_POSITIVE / 4.0, // subnormal
            -f32::MIN_POSITIVE / 4.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            -f32::NAN,
        ] {
            check(value, "out of domain");
        }

        // A fixed-seed LCG rather than a dependency: reproducible, and a
        // failure names the value it failed on.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..200_000 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let unit = ((state >> 11) as f32) / ((1u64 << 53) as f32);
            check(unit, "random in 0..1");
            // The same draw spread over the range a compositor really holds.
            check(unit * 8.0 - 4.0, "random in -4..4");
        }
    }

    /// Measurement harness for the display-transform numbers in
    /// `docs/implementation/perf-baseline.md`. It measures rather than
    /// asserts, so it stays out of the normal run:
    ///
    /// ```text
    /// cargo test -p ravel-core --release measure_display_transform_cost \
    ///     -- --ignored --nocapture
    /// ```
    ///
    /// The three paths are timed **alternately, one round each**, because the
    /// machine this is run on is never quiet: interleaving makes a load spike
    /// hit all three rather than whichever one happens to be under the clock
    /// while it lasts. Read the ratios; the absolute values belong to whatever
    /// else was running.
    #[test]
    #[ignore = "measurement harness; run with --release --ignored --nocapture"]
    fn measure_display_transform_cost() {
        use std::hint::black_box;
        use std::time::Instant;

        const ROUNDS: usize = 6;
        let count = 1920usize * 1080 * 4;
        let pixels: Vec<f32> = (0..count).map(|i| (i % 511) as f32 / 510.0).collect();

        let quantise_only = |out: &mut Vec<u8>| {
            out.clear();
            for pixel in pixels.chunks_exact(4) {
                out.extend_from_slice(&[
                    quantize_u8(pixel[0]),
                    quantize_u8(pixel[1]),
                    quantize_u8(pixel[2]),
                    quantize_u8(pixel[3]),
                ]);
            }
        };
        let transform_powf = |out: &mut Vec<u8>| {
            out.clear();
            for pixel in pixels.chunks_exact(4) {
                out.extend_from_slice(&encode_then_quantize([
                    pixel[0], pixel[1], pixel[2], pixel[3],
                ]));
            }
        };
        let transform_table = |out: &mut Vec<u8>| {
            out.clear();
            for pixel in pixels.chunks_exact(4) {
                out.extend_from_slice(&to_display_rgba8([pixel[0], pixel[1], pixel[2], pixel[3]]));
            }
        };

        let mut buffer = Vec::with_capacity(count);
        let mut rounds = [[0f64; 3]; ROUNDS];
        let mut powf_bytes = Vec::new();
        let mut table_bytes = Vec::new();
        for round in rounds.iter_mut() {
            let start = Instant::now();
            quantise_only(&mut buffer);
            round[0] = start.elapsed().as_secs_f64() * 1e3;
            black_box(&buffer);

            let start = Instant::now();
            transform_powf(&mut buffer);
            round[1] = start.elapsed().as_secs_f64() * 1e3;
            powf_bytes = std::mem::take(&mut buffer);
            buffer = Vec::with_capacity(count);

            let start = Instant::now();
            transform_table(&mut buffer);
            round[2] = start.elapsed().as_secs_f64() * 1e3;
            table_bytes = std::mem::take(&mut buffer);
            buffer = Vec::with_capacity(count);
        }

        // The claim the timings are worth anything at all: the two transforms
        // produce the same bytes.
        assert_eq!(
            powf_bytes, table_bytes,
            "the boundary table changed the output"
        );

        let median = |column: usize| {
            let mut values: Vec<f64> = rounds.iter().map(|round| round[column]).collect();
            values.sort_by(f64::total_cmp);
            values[values.len() / 2]
        };
        for (index, round) in rounds.iter().enumerate() {
            println!(
                "round {index}: quantise {:.2} ms | powf {:.2} ms | table {:.2} ms",
                round[0], round[1], round[2]
            );
        }
        let (quantise, powf, table) = (median(0), median(1), median(2));
        println!(
            "median (ms, 1920x1080): quantise {quantise:.2} | powf {powf:.2} | table {table:.2}"
        );
        println!("table vs powf: {:.1}x", powf / table);
        println!("table vs quantise-only: {:.2}x the floor", table / quantise);
    }

    const IDENTITY_CUBE: &str = "\
TITLE \"identity\"
# a comment
LUT_3D_SIZE 2
DOMAIN_MIN 0.0 0.0 0.0
DOMAIN_MAX 1.0 1.0 1.0
0.0 0.0 0.0
1.0 0.0 0.0
0.0 1.0 0.0
1.0 1.0 0.0
0.0 0.0 1.0
1.0 0.0 1.0
0.0 1.0 1.0
1.0 1.0 1.0
";

    /// CM-1: a known LUT returns known values, including between grid points.
    #[test]
    fn cube_lut_parses_and_interpolates() {
        let lut = CubeLut::parse(IDENTITY_CUBE).unwrap();
        assert_eq!(lut.size(), 2);
        assert_eq!(lut.domain(), ([0.0; 3], [1.0; 3]));

        for probe in [
            [0.0f32, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.25, 0.5, 0.75],
            [0.5, 0.5, 0.5],
        ] {
            let sampled = lut.sample(probe);
            for (a, b) in sampled.iter().zip(probe.iter()) {
                assert!(
                    (a - b).abs() < 1e-6,
                    "identity LUT moved {probe:?} to {sampled:?}"
                );
            }
        }
        // Out of domain clamps to the cube's edge instead of extrapolating.
        assert_eq!(lut.sample([-1.0, 2.0, 0.5]), [0.0, 1.0, 0.5]);
    }

    #[test]
    fn cube_lut_inverts_red_when_the_table_says_so() {
        // Same grid, red inverted: the interpolation must follow the table,
        // not the input.
        let inverted = IDENTITY_CUBE
            .lines()
            .map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() == 3 && parts[0].parse::<f32>().is_ok() {
                    let r: f32 = parts[0].parse().unwrap();
                    format!("{} {} {}", 1.0 - r, parts[1], parts[2])
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let lut = CubeLut::parse(&inverted).unwrap();
        let sampled = lut.sample([0.25, 0.5, 0.75]);
        assert!((sampled[0] - 0.75).abs() < 1e-6, "{sampled:?}");
        assert!((sampled[1] - 0.5).abs() < 1e-6, "{sampled:?}");
        assert!((sampled[2] - 0.75).abs() < 1e-6, "{sampled:?}");
    }

    /// A size-3 identity table has to be an identity at **both** ends. The top
    /// of the domain used to collapse onto the second-to-last grid point
    /// (`1.0` came back as `0.5`) because the interpolation fraction was
    /// measured from the unclamped floor; only size-2 tables were covered, and
    /// there the clamp is a no-op.
    #[test]
    fn cube_lut_reaches_the_last_grid_point() {
        let mut text = String::from("LUT_3D_SIZE 3\n");
        for b in 0..3 {
            for g in 0..3 {
                for r in 0..3 {
                    let f = |v: usize| v as f32 / 2.0;
                    text.push_str(&format!("{} {} {}\n", f(r), f(g), f(b)));
                }
            }
        }
        let lut = CubeLut::parse(&text).unwrap();
        for probe in [
            [0.0f32, 0.0, 0.0],
            [0.25, 0.5, 0.75],
            [0.5, 0.5, 0.5],
            [1.0, 1.0, 1.0],
            [2.0, 1.0, 0.0],
        ] {
            let expect = probe.map(|v: f32| v.clamp(0.0, 1.0));
            let sampled = lut.sample(probe);
            for (a, b) in sampled.iter().zip(expect.iter()) {
                assert!(
                    (a - b).abs() < 1e-6,
                    "identity LUT moved {probe:?} to {sampled:?}"
                );
            }
        }
    }

    #[test]
    fn cube_lut_rejects_malformed_files() {
        assert_eq!(CubeLut::parse("0.0 0.0 0.0\n"), Err(CubeError::MissingSize));
        assert_eq!(
            CubeLut::parse("LUT_1D_SIZE 32\n"),
            Err(CubeError::OneDimensional)
        );
        assert_eq!(
            CubeLut::parse("LUT_3D_SIZE 1\n"),
            Err(CubeError::BadSize(1))
        );
        assert!(matches!(
            CubeLut::parse("LUT_3D_SIZE 2\n0.0 0.0 0.0\n"),
            Err(CubeError::WrongEntryCount {
                expected: 8,
                found: 1
            })
        ));
        assert!(matches!(
            CubeLut::parse("LUT_3D_SIZE 2\nnot a colour\n"),
            Err(CubeError::BadEntry { .. })
        ));
        // A line with three *readable* numbers among four tokens is not a
        // triple with a stray word in it; it is a damaged file.
        assert!(matches!(
            CubeLut::parse("LUT_3D_SIZE 2\n0.0 foo 0.0 0.0\n"),
            Err(CubeError::BadEntry { .. })
        ));
        assert!(matches!(
            CubeLut::parse("LUT_3D_SIZE 2\nDOMAIN_MIN 0.0 x 0.0\n"),
            Err(CubeError::BadEntry { .. })
        ));
    }
}
