// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shaping, line breaking, glyph outlines, and the per-character instance
//! geometry `text.layout` produces (REQ-MOGRAPH-004, typography-plan unit 2).
//!
//! # Why rustybuzz and not cosmic-text
//!
//! cosmic-text exists to get text onto a screen: it owns a glyph cache and
//! hands back rasterised coverage, not outlines and not cluster boundaries.
//! What this module needs is the shaping result itself — which glyphs, at
//! which offsets, belonging to which grapheme cluster — so it drives
//! `rustybuzz` directly and keeps cosmic-text's layout layer out of the path.
//! Outlines come from `ttf_parser`, which `rustybuzz::Face` already derefs to,
//! so the plan's swash + zeno pair is not needed here: the same parsed face
//! that shaped the run also draws it.
//!
//! # Units
//!
//! Shaping happens in font units (`units_per_em`) and is scaled to
//! composition pixels exactly once, in [`layout_text`]. Positions are in
//! composition space, which the rasterizer reads as **pixels with the origin
//! at the top left and Y growing downwards** (`rasterize/mod.rs`), so glyph
//! outlines — Y-up by the OpenType convention — are mirrored on the way out.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ttf_parser::{GlyphId, OutlineBuilder};
use unicode_bidi::ParagraphBidiInfo;
use unicode_linebreak::BreakOpportunity;

use crate::geometry::{AttributeArray, Geometry, GeometryError, Primitive, names};
use crate::types::Vec2;

use super::font::FontRef;

/// One glyph of a shaped cluster: which glyph, and where it sits relative to
/// the cluster's pen position, in font units.
///
/// Also half of the key that makes two identical clusters share one instance
/// source, which is why it is `Hash` and holds integers rather than the scaled
/// floats: `Eq` on a shaped result has to be exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PlacedGlyph {
    pub(crate) id: u16,
    pub(crate) x: i32,
    pub(crate) y: i32,
}

/// One shaped grapheme cluster — what becomes one instance.
///
/// A cluster is not a codepoint and not a `char`: `ﬁ` is one cluster of one
/// ligature glyph over two codepoints, and `か` + U+3099 is one cluster of two
/// glyphs over two codepoints. Both are a single instance, because a single
/// instance is what a user counts as a character.
#[derive(Clone, Debug)]
pub(crate) struct ShapedCluster {
    /// Byte offset of the cluster in the paragraph it was shaped from.
    pub(crate) byte: usize,
    /// The glyphs this cluster draws, in draw order.
    pub(crate) glyphs: Vec<PlacedGlyph>,
    /// Pen advance in font units.
    pub(crate) advance: i32,
    /// The cluster starts with a whitespace character, so a line may end by
    /// dropping it.
    pub(crate) whitespace: bool,
}

/// Shape one paragraph into clusters, in the order they are drawn.
///
/// Bidirectional runs are resolved with `unicode-bidi` and shaped separately,
/// each with its own direction, then concatenated in the visual order
/// `visual_runs` returns — so a right-to-left run comes back already reversed
/// and the caller can place clusters by walking the slice forwards.
///
/// The reordering is per **paragraph**, not per line: a wrapped line of
/// mixed-direction text is therefore ordered against its paragraph rather than
/// against itself. That is exact for single-direction paragraphs, which is all
/// v1 targets (typography-plan: "v1 は横書き"); per-line reordering belongs
/// with the vertical-writing unit, which rebuilds the line walk anyway.
pub(crate) fn shape_paragraph(face: &rustybuzz::Face<'_>, text: &str) -> Vec<ShapedCluster> {
    if text.is_empty() {
        return Vec::new();
    }
    let bidi = ParagraphBidiInfo::new(text, None);
    let (levels, runs) = bidi.visual_runs(0..text.len());
    let mut clusters = Vec::new();
    for run in runs {
        let rtl = levels[run.start].is_rtl();
        let start = run.start;
        shape_run(face, &text[run], rtl, start, &mut clusters);
    }
    clusters
}

/// Shape one single-direction run and append its clusters.
///
/// `offset` is where the run starts in the paragraph, because rustybuzz
/// reports cluster indices relative to the slice it was given.
fn shape_run(
    face: &rustybuzz::Face<'_>,
    text: &str,
    rtl: bool,
    offset: usize,
    out: &mut Vec<ShapedCluster>,
) {
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    buffer.set_direction(if rtl {
        rustybuzz::Direction::RightToLeft
    } else {
        rustybuzz::Direction::LeftToRight
    });
    // Script and language are guessed from the content: a text node carries no
    // language tag, and guessing is what picks up Arabic joining or Devanagari
    // reordering without the user declaring anything.
    buffer.guess_segment_properties();
    let shaped = rustybuzz::shape(face, &[], buffer);

    let infos = shaped.glyph_infos();
    let positions = shaped.glyph_positions();
    let mut index = 0;
    while index < infos.len() {
        let cluster = infos[index].cluster;
        let mut glyphs = Vec::new();
        let mut advance = 0;
        let mut pen = 0;
        // Consecutive glyphs sharing a cluster value are one grapheme
        // cluster (rustybuzz's default `MonotoneGraphemes` level), and their
        // offsets are relative to the pen as it walks through them.
        while index < infos.len() && infos[index].cluster == cluster {
            let position = positions[index];
            glyphs.push(PlacedGlyph {
                id: infos[index].glyph_id as u16,
                x: pen + position.x_offset,
                y: position.y_offset,
            });
            pen += position.x_advance;
            advance += position.x_advance;
            index += 1;
        }
        let byte = offset + cluster as usize;
        out.push(ShapedCluster {
            byte,
            glyphs,
            advance,
            whitespace: text[byte - offset..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace),
        });
    }
}

/// Byte offsets in `text` where a line is **allowed** to break, per UAX #14.
///
/// Mandatory breaks are not included: paragraphs are split on `\n` before
/// shaping, so the only mandatory opportunity left is the one at the end of
/// the text, which is never a wrap point.
pub(crate) fn break_offsets(text: &str) -> Vec<usize> {
    unicode_linebreak::linebreaks(text)
        .filter(|(offset, kind)| *kind == BreakOpportunity::Allowed && *offset < text.len())
        .map(|(offset, _)| offset)
        .collect()
}

// ===========================================================================
// Parameters
// ===========================================================================

/// How the lines of a text block sit horizontally against the origin.
///
/// The origin is the anchor, not a margin: `Left` starts every line at `x = 0`,
/// `Center` centres each line on `x = 0`, `Right` ends each line there. That
/// makes the alignment and the layer's own anchor point one decision instead
/// of two, which is what a motion-graphics tool needs — a title that grows
/// from its centre must not drift because its text got longer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    /// Stretch the whitespace of every line but a paragraph's last so the
    /// lines fill `wrap_width`. Falls back to `Left` when there is no wrap
    /// width to fill or no whitespace to stretch.
    Justify,
}

/// The `align` parameter's dropdown options, in order.
pub const TEXT_ALIGNS: [&str; 4] = ["left", "center", "right", "justify"];

impl Align {
    /// The alignment a [`TEXT_ALIGNS`] name stands for; anything else is
    /// `Left`, because a foreign value comes from a hand-edited document
    /// rather than from the dropdown.
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "center" => Self::Center,
            "right" => Self::Right,
            "justify" => Self::Justify,
            _ => Self::Left,
        }
    }
}

/// Which horizontal line of the text block lands on `y = 0`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VerticalAnchor {
    /// The first line's baseline. The default, because it is the only anchor
    /// that does not move when the font's vertical metrics change.
    #[default]
    Baseline,
    /// The top of the first line's ascent.
    Top,
    /// The middle of the block, ascent to descent.
    Center,
    /// The bottom of the last line's descent.
    Bottom,
}

/// The `anchor` parameter's dropdown options, in order.
pub const TEXT_ANCHORS: [&str; 4] = ["baseline", "top", "center", "bottom"];

impl VerticalAnchor {
    /// The anchor a [`TEXT_ANCHORS`] name stands for; anything else is
    /// `Baseline`.
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "top" => Self::Top,
            "center" => Self::Center,
            "bottom" => Self::Bottom,
            _ => Self::Baseline,
        }
    }
}

/// Everything `text.layout` decides beyond the face and the string.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutParams {
    /// Em size in composition pixels.
    pub size: f32,
    /// Extra pen advance after every character, in composition pixels. Part
    /// of each character's `advance` attribute, and excluded from a line's
    /// measured width after the last character — so tracking does not shift a
    /// centred line.
    pub tracking: f32,
    /// Baseline-to-baseline distance in composition pixels. Zero or negative
    /// takes the face's own `ascender - descender + line_gap`.
    pub leading: f32,
    pub align: Align,
    /// Wrap lines that would exceed this many composition pixels. Zero or
    /// negative wraps only at the `\n` in the string.
    pub wrap_width: f32,
    pub anchor: VerticalAnchor,
}

impl Default for LayoutParams {
    fn default() -> Self {
        Self {
            size: DEFAULT_SIZE,
            tracking: 0.0,
            leading: 0.0,
            align: Align::Left,
            wrap_width: 0.0,
            anchor: VerticalAnchor::Baseline,
        }
    }
}

/// The em size a new `text.layout` node starts at, in composition pixels.
pub const DEFAULT_SIZE: f32 = 100.0;

/// What can go wrong laying text out.
#[derive(Debug, thiserror::Error)]
pub enum TextError {
    /// The resolved face's bytes did not parse as a shapeable font. The face
    /// index was built by parsing the same file, so this means the file
    /// changed underneath the index, or the container declares a face this
    /// shaper cannot read.
    #[error("the resolved font face could not be parsed for shaping")]
    FaceParse,
    /// Building the instance geometry failed.
    #[error(transparent)]
    Geometry(#[from] GeometryError),
}

// ===========================================================================
// Line breaking
// ===========================================================================

/// One cluster scaled to composition pixels, ready to place.
struct Fitted {
    glyphs: Vec<PlacedGlyph>,
    /// Pen movement in pixels, tracking included.
    advance: f32,
    whitespace: bool,
}

/// One laid-out line.
struct Line {
    clusters: Vec<Fitted>,
    /// Ink width in pixels: the clusters' pen movement less the tracking that
    /// would have followed the last one.
    width: f32,
    /// The line ends its paragraph, so justification leaves it alone.
    last_in_paragraph: bool,
}

/// Greedily break one shaped paragraph into lines at `wrap_width`.
///
/// The paragraph is shaped **once** and the lines are cut out of that one
/// result, rather than re-shaped per line. The cost is that a kerning pair or
/// a ligature spanning the break keeps the width it had mid-line; the gain is
/// that wrapping a thousand characters shapes them once instead of once per
/// candidate line. If a break ever needs to change the shaping, harfbuzz
/// reports `UNSAFE_TO_BREAK` per cluster and the fix is to re-shape only the
/// two affected lines — the walk below is already indexed by cluster.
///
/// A whitespace cluster never triggers a break: it would be dropped at the
/// line end anyway, so measuring it would break a line one word early.
fn wrap_paragraph(
    clusters: &[ShapedCluster],
    breaks: &[usize],
    scale: f32,
    tracking: f32,
    wrap_width: f32,
) -> Vec<Line> {
    let advances: Vec<f32> = clusters
        .iter()
        .map(|cluster| cluster.advance as f32 * scale + tracking)
        .collect();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut pen = 0.0;
    let mut last_break: Option<usize> = None;
    let mut index = 0;
    while index < clusters.len() {
        if index > start && breaks.binary_search(&clusters[index].byte).is_ok() {
            last_break = Some(index);
        }
        if wrap_width > 0.0
            && index > start
            && !clusters[index].whitespace
            && pen + advances[index] > wrap_width
        {
            // No break opportunity on this line: cut before the character
            // that overflowed, so an unbreakable run still wraps instead of
            // running off the composition.
            let cut = last_break.unwrap_or(index);
            lines.push(cut_line(
                &clusters[start..cut],
                &advances[start..cut],
                tracking,
                false,
            ));
            start = cut;
            last_break = None;
            pen = advances[start..index].iter().sum();
        }
        pen += advances[index];
        index += 1;
    }
    lines.push(cut_line(
        &clusters[start..],
        &advances[start..],
        tracking,
        true,
    ));
    lines
}

/// One line out of a shaped run, with its trailing whitespace dropped.
///
/// Trailing whitespace becomes no instance at all: it has no ink, and keeping
/// it would push a centred or right-aligned line sideways by however many
/// spaces happened to sit at the wrap point.
fn cut_line(
    clusters: &[ShapedCluster],
    advances: &[f32],
    tracking: f32,
    last_in_paragraph: bool,
) -> Line {
    let mut end = clusters.len();
    while end > 0 && clusters[end - 1].whitespace {
        end -= 1;
    }
    Line {
        clusters: (0..end)
            .map(|index| Fitted {
                glyphs: clusters[index].glyphs.clone(),
                advance: advances[index],
                whitespace: clusters[index].whitespace,
            })
            .collect(),
        width: if end == 0 {
            0.0
        } else {
            advances[..end].iter().sum::<f32>() - tracking
        },
        last_in_paragraph,
    }
}

// ===========================================================================
// Glyph outlines
// ===========================================================================

/// Two-thirds, the weight that raises a quadratic control point to the two
/// cubic ones describing the same curve.
const QUAD_TO_CUBIC: f32 = 2.0 / 3.0;

/// How close, in font units, a contour's last point has to be to its first
/// before the two are treated as the same point.
const CLOSE_EPSILON: f32 = 0.5;

/// Collects glyph outlines into the point columns of one cluster's source
/// geometry.
///
/// Curves stay curves. Quadratics are raised to cubics and both are stored as
/// `in_tan` / `out_tan` offsets — the representation `rasterize` already
/// flattens for pen-drawn paths (REQ-UI-011) — so a glyph stays smooth at any
/// zoom rather than being frozen at the flatness of the size it was laid out
/// at, and `text.to_path` inherits real control points to hand to a field.
///
/// Font units are Y-up and composition pixels are Y-down, so every coordinate
/// is mirrored on the way in; that reverses every contour's winding at once,
/// which leaves non-zero fill — and therefore the counter of an `o` — exactly
/// as it was.
struct OutlineSink {
    points: Vec<Vec2>,
    in_tans: Vec<Vec2>,
    out_tans: Vec<Vec2>,
    contours: Vec<Range<usize>>,
    /// First point index of the contour being built.
    start: usize,
    /// Where the pen is, in font units, so tangents can be computed before the
    /// mirror is applied.
    pen: (f32, f32),
    /// Where the current contour began, in font units.
    origin: (f32, f32),
    /// The glyph's own offset inside the cluster, in composition pixels.
    offset: Vec2,
    /// Font units to composition pixels.
    scale: f32,
}

impl OutlineSink {
    fn new(scale: f32) -> Self {
        Self {
            points: Vec::new(),
            in_tans: Vec::new(),
            out_tans: Vec::new(),
            contours: Vec::new(),
            start: 0,
            pen: (0.0, 0.0),
            origin: (0.0, 0.0),
            offset: Vec2(0.0, 0.0),
            scale,
        }
    }

    /// Start reading a new glyph, placed at `offset` pixels from the cluster's
    /// pen position.
    fn begin_glyph(&mut self, offset: Vec2) {
        self.close_contour();
        self.offset = offset;
    }

    /// A font-unit position as a composition-space point.
    fn point(&self, x: f32, y: f32) -> Vec2 {
        Vec2(
            self.offset.0 + x * self.scale,
            self.offset.1 - y * self.scale,
        )
    }

    /// A font-unit offset as a composition-space offset. Free of the glyph
    /// offset — a tangent is a difference, not a position.
    fn vector(&self, x: f32, y: f32) -> Vec2 {
        Vec2(x * self.scale, -y * self.scale)
    }

    fn push(&mut self, x: f32, y: f32, in_tan: Vec2) {
        self.points.push(self.point(x, y));
        self.in_tans.push(in_tan);
        self.out_tans.push(Vec2(0.0, 0.0));
        self.pen = (x, y);
    }

    /// Tangent of the segment leaving the point the pen is on.
    fn set_out_tan(&mut self, out_tan: Vec2) {
        if let Some(last) = self.out_tans.last_mut() {
            *last = out_tan;
        }
    }

    /// Finish the contour under construction, if any.
    ///
    /// A glyph contour is closed by definition, so `close` is not what decides
    /// it — the CFF and `glyf` readers both call it, but a contour left open by
    /// a malformed glyph still has to fill rather than stroke. When the last
    /// point has come back to the first, that duplicate is dropped and its
    /// arriving tangent moves onto the first point, which is where the closing
    /// segment actually ends.
    fn close_contour(&mut self) {
        if self.points.len() <= self.start {
            return;
        }
        if (self.pen.0 - self.origin.0).abs() < CLOSE_EPSILON
            && (self.pen.1 - self.origin.1).abs() < CLOSE_EPSILON
            && self.points.len() > self.start + 1
        {
            let arriving = self.in_tans[self.points.len() - 1];
            self.points.pop();
            self.in_tans.pop();
            self.out_tans.pop();
            self.in_tans[self.start] = arriving;
        }
        self.contours.push(self.start..self.points.len());
        self.start = self.points.len();
    }

    /// The collected contours as one geometry of closed path primitives.
    fn into_geometry(mut self) -> Result<Geometry, GeometryError> {
        self.close_contour();
        if self.points.is_empty() {
            return Ok(Geometry::new());
        }
        let mut geometry = Geometry::from_points(std::mem::take(&mut self.points));
        geometry
            .points_mut()
            .insert(names::IN_TAN, AttributeArray::Vec2(self.in_tans))?;
        geometry
            .points_mut()
            .insert(names::OUT_TAN, AttributeArray::Vec2(self.out_tans))?;
        for verts in self.contours {
            geometry.push_primitive(Primitive::Path {
                verts,
                closed: true,
            });
        }
        Ok(geometry)
    }
}

impl OutlineBuilder for OutlineSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close_contour();
        self.origin = (x, y);
        self.push(x, y, Vec2(0.0, 0.0));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.push(x, y, Vec2(0.0, 0.0));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (px, py) = self.pen;
        let out_tan = self.vector(QUAD_TO_CUBIC * (x1 - px), QUAD_TO_CUBIC * (y1 - py));
        self.set_out_tan(out_tan);
        let in_tan = self.vector(QUAD_TO_CUBIC * (x1 - x), QUAD_TO_CUBIC * (y1 - y));
        self.push(x, y, in_tan);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (px, py) = self.pen;
        let out_tan = self.vector(x1 - px, y1 - py);
        self.set_out_tan(out_tan);
        let in_tan = self.vector(x2 - x, y2 - y);
        self.push(x, y, in_tan);
    }

    fn close(&mut self) {
        self.close_contour();
    }
}

/// The outlines of one cluster's glyphs, as one geometry in cluster-local
/// composition pixels with the pen position at the origin.
fn cluster_geometry(
    face: &rustybuzz::Face<'_>,
    glyphs: &[PlacedGlyph],
    scale: f32,
) -> Result<Geometry, GeometryError> {
    let mut sink = OutlineSink::new(scale);
    for glyph in glyphs {
        sink.begin_glyph(Vec2(glyph.x as f32 * scale, -(glyph.y as f32) * scale));
        // `None` means the glyph has no outline — a space, or a glyph the face
        // draws with a bitmap this module does not read. Either way there is
        // nothing to collect and the instance stays, so the character still
        // counts and still advances the pen.
        face.outline_glyph(GlyphId(glyph.id), &mut sink);
    }
    sink.into_geometry()
}

// ===========================================================================
// Layout
// ===========================================================================

/// Where the time of one [`layout_text_timed`] call went.
///
/// The three stages the typography plan's baseline asks to be told apart.
/// `shaping` is rustybuzz turning characters into positioned glyphs;
/// `outlines` is reading the glyph contours out of the face, once per
/// **distinct** cluster; `placement` is everything else — line breaking,
/// alignment, and writing the attribute columns.
///
/// Timing is always collected rather than switched on: it is three clock
/// reads per layout plus two per distinct cluster, microseconds against a
/// call that shapes a paragraph, and it means the perf harness measures
/// exactly the code a frame runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutTiming {
    /// rustybuzz, per paragraph.
    pub shaping: Duration,
    /// Glyph contour extraction, once per distinct cluster.
    pub outlines: Duration,
    /// The remainder: line breaking, alignment, attribute columns.
    pub placement: Duration,
}

/// Lay `text` out in `font` and return the per-character instance geometry
/// (typography-plan unit 2).
///
/// One instance per grapheme cluster, carrying `index`, `P`, `rot`, `scale`,
/// `char_index`, `word_index`, `line_index`, `char_progress`, `advance` and
/// `source_index`; the glyph outlines sit in `instance_sources`, one entry per
/// **distinct** shaped cluster, so a page of text holding forty `e`s holds one
/// `e` outline. That is the same shape `scatter.*` produces, which is what
/// lets the existing instance path in `rasterize` draw text without knowing
/// what text is — and what keeps the design intact when instance columns move
/// onto the GPU.
///
/// A character with no ink still gets an instance: a space is a character a
/// user counts and a `char_index` a stagger steps over. What does *not* get an
/// instance is whitespace trailing a line, and the `\n` itself.
pub fn layout_text(
    font: &FontRef,
    text: &str,
    params: &LayoutParams,
) -> Result<Geometry, TextError> {
    layout_text_timed(font, text, params).map(|(geometry, _)| geometry)
}

/// [`layout_text`], reporting where the time went.
///
/// The measurement entry point (`examples/text_layout_baseline.rs`); the
/// layout itself is identical, so a number it reports is a number a frame
/// pays.
pub fn layout_text_timed(
    font: &FontRef,
    text: &str,
    params: &LayoutParams,
) -> Result<(Geometry, LayoutTiming), TextError> {
    let started = Instant::now();
    let mut timing = LayoutTiming::default();
    let face =
        rustybuzz::Face::from_slice(&font.data, font.face_index).ok_or(TextError::FaceParse)?;
    let upem = face.units_per_em();
    if upem <= 0 {
        return Err(TextError::FaceParse);
    }
    let scale = params.size / upem as f32;
    let ascent = f32::from(face.ascender()) * scale;
    let descent = -f32::from(face.descender()) * scale;
    let leading = if params.leading > 0.0 {
        params.leading
    } else {
        (i32::from(face.ascender()) - i32::from(face.descender()) + i32::from(face.line_gap()))
            as f32
            * scale
    };

    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        // A CRLF document leaves the CR at the end of the paragraph, where it
        // would shape into a visible cluster.
        let paragraph = paragraph.strip_suffix('\r').unwrap_or(paragraph);
        let shaping = Instant::now();
        let clusters = shape_paragraph(&face, paragraph);
        timing.shaping += shaping.elapsed();
        let breaks = break_offsets(paragraph);
        lines.extend(wrap_paragraph(
            &clusters,
            &breaks,
            scale,
            params.tracking,
            params.wrap_width,
        ));
    }

    // `split` always yields at least one paragraph, so there is at least one
    // line and the subtraction below is safe.
    let last_line = (lines.len() - 1) as f32;
    let first_baseline = match params.anchor {
        VerticalAnchor::Baseline => 0.0,
        VerticalAnchor::Top => ascent,
        VerticalAnchor::Center => (ascent - descent - last_line * leading) / 2.0,
        VerticalAnchor::Bottom => -descent - last_line * leading,
    };

    let mut positions = Vec::new();
    let mut advances = Vec::new();
    let mut char_indices = Vec::new();
    let mut word_indices = Vec::new();
    let mut line_indices = Vec::new();
    let mut source_indices = Vec::new();
    let mut sources: Vec<Arc<Geometry>> = Vec::new();
    let mut by_glyphs: HashMap<Vec<PlacedGlyph>, usize> = HashMap::new();
    let mut word = 0;
    // The first character of the text starts word 0 rather than word 1.
    let mut after_gap = false;

    for (line_index, line) in lines.iter().enumerate() {
        let spaces = line
            .clusters
            .iter()
            .filter(|cluster| cluster.whitespace)
            .count();
        let justified = params.align == Align::Justify
            && !line.last_in_paragraph
            && spaces > 0
            && params.wrap_width > line.width;
        let extra = if justified {
            (params.wrap_width - line.width) / spaces as f32
        } else {
            0.0
        };
        let width = line.width + extra * spaces as f32;
        let mut pen = match params.align {
            Align::Left | Align::Justify => 0.0,
            Align::Center => -width / 2.0,
            Align::Right => -width,
        };
        let baseline = first_baseline + line_index as f32 * leading;
        for (char_index, cluster) in line.clusters.iter().enumerate() {
            if cluster.whitespace {
                after_gap = true;
            } else if after_gap {
                word += 1;
                after_gap = false;
            }
            let source = match by_glyphs.get(&cluster.glyphs) {
                Some(&index) => index,
                None => {
                    let index = sources.len();
                    let outline = Instant::now();
                    let geometry = cluster_geometry(&face, &cluster.glyphs, scale)?;
                    timing.outlines += outline.elapsed();
                    sources.push(Arc::new(geometry));
                    by_glyphs.insert(cluster.glyphs.clone(), index);
                    index
                }
            };
            let advance = cluster.advance + if cluster.whitespace { extra } else { 0.0 };
            positions.push(Vec2(pen, baseline));
            advances.push(advance);
            char_indices.push(char_index as i32);
            word_indices.push(word);
            line_indices.push(line_index as i32);
            source_indices.push(source as i32);
            pen += advance;
        }
        // A line break ends a word as surely as a space does.
        after_gap = true;
    }

    let count = positions.len();
    let progress = (0..count)
        .map(|index| {
            if count > 1 {
                index as f32 / (count - 1) as f32
            } else {
                0.0
            }
        })
        .collect();
    let mut geometry = Geometry::new();
    let instances = geometry.instances_mut();
    instances.insert(
        names::INDEX,
        AttributeArray::I32((0..count as i32).collect()),
    )?;
    instances.insert(names::P, AttributeArray::Vec2(positions))?;
    instances.insert(names::ROT, AttributeArray::F32(vec![0.0; count]))?;
    instances.insert(
        names::SCALE,
        AttributeArray::Vec2(vec![Vec2(1.0, 1.0); count]),
    )?;
    instances.insert(names::CHAR_INDEX, AttributeArray::I32(char_indices))?;
    instances.insert(names::WORD_INDEX, AttributeArray::I32(word_indices))?;
    instances.insert(names::LINE_INDEX, AttributeArray::I32(line_indices))?;
    instances.insert(names::CHAR_PROGRESS, AttributeArray::F32(progress))?;
    instances.insert(names::ADVANCE, AttributeArray::F32(advances))?;
    instances.insert(names::SOURCE_INDEX, AttributeArray::I32(source_indices))?;
    geometry.set_instance_sources(sources);
    // Placement is the remainder rather than its own clock, so the three
    // stages always sum to the call: a stage nobody instrumented shows up
    // here instead of vanishing.
    timing.placement = started
        .elapsed()
        .saturating_sub(timing.shaping)
        .saturating_sub(timing.outlines);
    Ok((geometry, timing))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bundled Latin face, read from the repository rather than embedded
    /// again: `font.rs` compiles this one into the binary already, and the
    /// tests here need a second, Japanese face beside it.
    pub(super) const GEIST: &[u8] = include_bytes!("../../../../assets/fonts/Geist-Regular.ttf");
    /// The bundled Japanese face. Its combining voiced-sound mark is what makes
    /// "characters ≠ codepoints" demonstrable on a face that ships with Ravel
    /// rather than one the host happens to have installed.
    pub(super) const NOTO_JP: &[u8] =
        include_bytes!("../../../../assets/fonts/NotoSansJP-Regular.otf");

    fn face(data: &'static [u8]) -> rustybuzz::Face<'static> {
        rustybuzz::Face::from_slice(data, 0).expect("a bundled face parses")
    }

    #[test]
    fn ascii_shapes_one_cluster_per_character() {
        let clusters = shape_paragraph(&face(GEIST), "Hello World");
        assert_eq!(clusters.len(), 11);
        assert!(clusters.iter().all(|cluster| cluster.glyphs.len() == 1));
        assert!(clusters[5].whitespace, "the space has to be marked as one");
        assert!(!clusters[0].whitespace);
        assert_eq!(
            clusters
                .iter()
                .map(|cluster| cluster.byte)
                .collect::<Vec<_>>(),
            (0..11).collect::<Vec<_>>(),
            "ASCII clusters sit one byte apart, in logical order"
        );
    }

    /// Two codepoints, one glyph, one cluster: Geist substitutes the `fi`
    /// ligature, and harfbuzz merges the two clusters into the one the
    /// substitution produced. The completion criterion "characters ≠
    /// codepoints", from the Latin side.
    #[test]
    fn a_latin_ligature_shapes_two_codepoints_into_one_cluster() {
        let text = "fi";
        assert_eq!(text.chars().count(), 2, "the fixture is two codepoints");
        let clusters = shape_paragraph(&face(GEIST), text);
        assert_eq!(clusters.len(), 1, "one ligature, one instance");
        assert_eq!(clusters[0].glyphs.len(), 1);
        let separate = shape_paragraph(&face(GEIST), "f|i");
        assert_ne!(
            clusters[0].glyphs[0].id, separate[0].glyphs[0].id,
            "the ligature glyph is not the standalone `f`, so a substitution \
             really happened"
        );
    }

    /// Three codepoints, two clusters — the count is neither the codepoint
    /// count nor one, so a cluster walk that fell back to `chars()` would be
    /// visibly wrong here.
    #[test]
    fn a_partial_ligature_leaves_the_unligated_codepoint_its_own_cluster() {
        let text = "ffi";
        assert_eq!(text.chars().count(), 3);
        let clusters = shape_paragraph(&face(GEIST), text);
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0].byte, 0);
        assert_eq!(clusters[1].byte, 2, "the second cluster starts at the `i`");
    }

    /// `か` + U+3099 is two codepoints and one grapheme cluster, and Noto Sans
    /// JP resolves it to the same glyph as the precomposed `が` — so the
    /// assertion is not just "one cluster" but "the *right* one glyph".
    #[test]
    fn a_combining_voiced_mark_stays_in_its_base_cluster() {
        let text = "\u{304B}\u{3099}";
        assert_eq!(text.chars().count(), 2, "the fixture is two codepoints");
        let clusters = shape_paragraph(&face(NOTO_JP), text);
        assert_eq!(clusters.len(), 1, "one grapheme cluster, not two");
        assert_eq!(clusters[0].glyphs.len(), 1);
        let precomposed = shape_paragraph(&face(NOTO_JP), "\u{304C}");
        assert_eq!(
            clusters[0].glyphs[0].id, precomposed[0].glyphs[0].id,
            "composition has to land on the precomposed glyph"
        );
        assert_eq!(clusters[0].advance, precomposed[0].advance);
    }

    #[test]
    fn break_opportunities_sit_before_the_word_that_follows() {
        assert_eq!(break_offsets("Hello world"), vec![6]);
        assert_eq!(break_offsets("one two three"), vec![4, 8]);
        assert_eq!(break_offsets("unbreakable"), Vec::<usize>::new());
    }
}

#[cfg(test)]
mod layout_tests {
    use super::tests::{GEIST, NOTO_JP};
    use super::*;
    use crate::geometry::Domain;

    /// A `FontRef` over a bundled face. Built by hand rather than through
    /// `FontLibrary`: layout has no interest in how a face was selected, and
    /// hand-building keeps the host's installed fonts out of the assertions.
    fn font(data: &[u8]) -> FontRef {
        FontRef {
            family: "bundled".into(),
            weight: 400,
            italic: false,
            data: Arc::new(data.to_vec()),
            face_index: 0,
            is_fallback: false,
        }
    }

    /// Deliberately none of the defaults: a fixture built out of `Default`
    /// would pass whether or not the parameters are read.
    fn params() -> LayoutParams {
        LayoutParams {
            size: 37.0,
            ..Default::default()
        }
    }

    fn geist(text: &str, params: &LayoutParams) -> Geometry {
        layout_text(&font(GEIST), text, params).expect("a bundled face lays out")
    }

    fn positions(geometry: &Geometry) -> Vec<Vec2> {
        geometry
            .positions(Domain::Instance)
            .expect("the instance domain carries P")
            .expect("P is a position column")
            .planar()
            .expect("horizontal layout is planar")
            .to_vec()
    }

    fn floats(geometry: &Geometry, name: &str) -> Vec<f32> {
        geometry
            .instances()
            .get(name)
            .unwrap_or_else(|| panic!("the instance domain carries {name}"))
            .as_f32(name)
            .expect("an F32 column")
            .to_vec()
    }

    fn ints(geometry: &Geometry, name: &str) -> Vec<i32> {
        geometry
            .instances()
            .get(name)
            .unwrap_or_else(|| panic!("the instance domain carries {name}"))
            .as_i32(name)
            .expect("an I32 column")
            .to_vec()
    }

    /// The pen extent of every line, `(min x, max x)` over `P` and
    /// `P + advance`. What alignment actually moves, and the only bound that
    /// does not depend on which glyphs the string happens to use.
    fn pen_bounds(geometry: &Geometry) -> (f32, f32) {
        let positions = positions(geometry);
        let advances = floats(geometry, names::ADVANCE);
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for (position, advance) in positions.iter().zip(&advances) {
            min = min.min(position.0);
            max = max.max(position.0 + advance);
        }
        (min, max)
    }

    /// The completion criterion "character count = instance count", on a
    /// string whose characters, codepoints and clusters all agree.
    #[test]
    fn every_ascii_character_becomes_one_instance() {
        let text = "Hello World";
        let geometry = geist(text, &params());
        assert_eq!(geometry.instance_count(), text.chars().count());
        assert_eq!(geometry.instance_count(), 11);
        assert_eq!(
            ints(&geometry, names::INDEX),
            (0..11).collect::<Vec<_>>(),
            "index counts through the whole string"
        );
    }

    /// The other half of that criterion: two codepoints that shape into one
    /// cluster are **one** instance, so the count follows clusters and not
    /// `chars()`.
    #[test]
    fn a_cluster_of_several_codepoints_is_one_instance() {
        let ligature = "fi";
        assert_eq!(ligature.chars().count(), 2);
        assert_eq!(geist(ligature, &params()).instance_count(), 1);

        let voiced = "\u{304B}\u{3099}";
        assert_eq!(voiced.chars().count(), 2);
        let geometry = layout_text(&font(NOTO_JP), voiced, &params()).expect("noto lays out");
        assert_eq!(geometry.instance_count(), 1);

        let partial = "ffi";
        assert_eq!(partial.chars().count(), 3);
        assert_eq!(
            geist(partial, &params()).instance_count(),
            2,
            "neither the codepoint count nor one"
        );
    }

    /// One outline per distinct shaped cluster, shared by every instance that
    /// draws it — the completion criterion, checked through `source_index`
    /// because that is how an instance actually reaches its outline.
    #[test]
    fn identical_characters_share_one_instance_source() {
        let geometry = geist("AB A", &params());
        assert_eq!(geometry.instance_count(), 4);
        assert_eq!(
            geometry.sources().len(),
            3,
            "A, B and the space — the repeated A must not add a fourth"
        );
        let indices = ints(&geometry, names::SOURCE_INDEX);
        let source = |instance: usize| {
            geometry.sources()[indices[instance] as usize]
                .geometry()
                .expect("glyph outlines are geometry sources")
                .clone()
        };
        assert!(
            Arc::ptr_eq(&source(0), &source(3)),
            "both `A`s have to reach the same outline"
        );
        assert!(!Arc::ptr_eq(&source(0), &source(1)));
    }

    /// A glyph outline is a set of closed paths with bezier tangents, which is
    /// what `rasterize` flattens and fills. `o` has two contours, and the
    /// counter is what makes the winding matter.
    #[test]
    fn a_glyph_outline_is_closed_paths_with_tangents() {
        let geometry = geist("o", &params());
        let outline = geometry.sources()[0]
            .geometry()
            .expect("a glyph source is geometry");
        assert_eq!(
            outline.primitives().len(),
            2,
            "the letter and its counter: {:?}",
            outline.primitives()
        );
        assert!(
            outline
                .primitives()
                .iter()
                .all(|primitive| matches!(primitive, Primitive::Path { closed: true, .. })),
            "glyph contours are closed"
        );
        let tangents = outline
            .points()
            .get(names::OUT_TAN)
            .expect("outlines carry out_tan")
            .as_vec2(names::OUT_TAN)
            .expect("a Vec2 column")
            .to_vec();
        assert!(
            tangents
                .iter()
                .any(|tangent| tangent.0 != 0.0 || tangent.1 != 0.0),
            "a round letter has curved segments"
        );
        // Y-down composition space: the glyph sits above its baseline, so its
        // outline points are negative in Y.
        let points = outline
            .positions(Domain::Point)
            .expect("outline points")
            .expect("planar")
            .planar()
            .expect("planar")
            .to_vec();
        // Y is mirrored on the way in, so ink above the baseline is negative.
        // A round letter overshoots the baseline by a percent or two, which is
        // why the bound is not zero — but it must not reach a descender.
        let overshoot = 0.03 * 37.0;
        assert!(
            points.iter().all(|point| point.1 <= overshoot),
            "the `o` may only overshoot the baseline, not descend below it: {points:?}"
        );
        assert!(
            points.iter().any(|point| point.1 < -0.4 * 37.0),
            "the x-height of the `o` has to reach above the baseline: {points:?}"
        );
    }

    /// Every attribute of the typography plan's table, on a string that makes
    /// each of them say something different: two lines, two words on the
    /// first, and a tracking that is not zero.
    #[test]
    fn every_per_character_attribute_is_written() {
        let mut params = params();
        params.tracking = 3.5;
        params.leading = 61.0;
        let geometry = geist("ab cd\nef", &params);
        assert_eq!(
            geometry.instance_count(),
            7,
            "five on the first line, two on the second"
        );

        assert_eq!(ints(&geometry, names::INDEX), vec![0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(
            ints(&geometry, names::CHAR_INDEX),
            vec![0, 1, 2, 3, 4, 0, 1],
            "char_index restarts on each line, unlike index"
        );
        assert_eq!(
            ints(&geometry, names::WORD_INDEX),
            vec![0, 0, 0, 1, 1, 2, 2],
            "the space and the line break each start a new word"
        );
        assert_eq!(
            ints(&geometry, names::LINE_INDEX),
            vec![0, 0, 0, 0, 0, 1, 1]
        );

        let progress = floats(&geometry, names::CHAR_PROGRESS);
        assert_eq!(progress.len(), 7);
        assert!((progress[0] - 0.0).abs() < 1e-6);
        assert!((progress[6] - 1.0).abs() < 1e-6);
        assert!(
            progress.windows(2).all(|pair| pair[1] > pair[0]),
            "char_progress has to increase: {progress:?}"
        );

        let advances = floats(&geometry, names::ADVANCE);
        let untracked = floats(
            &geist(
                "ab cd\nef",
                &LayoutParams {
                    tracking: 0.0,
                    ..params
                },
            ),
            names::ADVANCE,
        );
        assert!(
            advances
                .iter()
                .zip(&untracked)
                .all(|(advance, bare)| (advance - bare - params.tracking).abs() < 1e-3),
            "every advance is the glyph's plus exactly the tracking: {advances:?} against {untracked:?}"
        );

        let rotations = floats(&geometry, names::ROT);
        assert_eq!(rotations, vec![0.0; 7]);
        let scales = geometry
            .instances()
            .get(names::SCALE)
            .expect("scale")
            .as_vec2(names::SCALE)
            .expect("a Vec2 column")
            .to_vec();
        assert_eq!(scales, vec![Vec2(1.0, 1.0); 7]);

        let positions = positions(&geometry);
        assert!(
            (positions[5].1 - positions[0].1 - params.leading).abs() < 0.01,
            "the second line sits one leading below the first: {positions:?}"
        );
    }

    /// `align` is measured against the origin, not a margin, so a centred
    /// block straddles `x = 0` and a right-aligned one ends there.
    #[test]
    fn alignment_places_the_line_against_the_origin() {
        let text = "Hello World";
        let mut params = params();

        params.align = Align::Left;
        let (left_min, left_max) = pen_bounds(&geist(text, &params));
        assert!(
            left_min.abs() < 0.01,
            "left starts at the origin: {left_min}"
        );
        let width = left_max - left_min;
        assert!(width > 100.0);

        params.align = Align::Center;
        let (min, max) = pen_bounds(&geist(text, &params));
        assert!(
            (min + max).abs() < 0.01,
            "a centred line straddles the origin: {min}..{max}"
        );
        assert!((max - min - width).abs() < 0.01, "centring must not resize");

        params.align = Align::Right;
        let (min, max) = pen_bounds(&geist(text, &params));
        assert!(max.abs() < 0.01, "right ends at the origin: {min}..{max}");
        assert!((max - min - width).abs() < 0.01);
    }

    /// `wrap_width` breaks at a UAX #14 opportunity, drops the space it broke
    /// at, and leaves no line wider than the limit.
    #[test]
    fn wrap_width_breaks_lines_at_word_boundaries() {
        let mut params = params();
        params.wrap_width = 140.0;
        let text = "one two three four";
        let geometry = geist(text, &params);

        let lines = ints(&geometry, names::LINE_INDEX);
        let line_count = lines.last().copied().unwrap_or(0) + 1;
        assert!(line_count > 1, "140 px has to force a wrap: {lines:?}");
        assert_eq!(
            geometry.instance_count(),
            text.chars().count() - (line_count as usize - 1),
            "each break consumes exactly the space it broke at"
        );

        let positions = positions(&geometry);
        let advances = floats(&geometry, names::ADVANCE);
        for line in 0..line_count {
            let extent = positions
                .iter()
                .zip(&advances)
                .zip(&lines)
                .filter(|((_, _), index)| **index == line)
                .map(|((position, advance), _)| position.0 + advance)
                .fold(f32::NEG_INFINITY, f32::max);
            assert!(
                extent <= params.wrap_width + 0.01,
                "line {line} runs to {extent}, past the {} px limit",
                params.wrap_width
            );
        }
        // The face's own `ascender - descender + line_gap` at this size.
        let auto_leading = 37.0 * (1005.0 + 295.0) / 1000.0;
        assert!(
            positions.iter().zip(&lines).all(|(position, line)| {
                (position.1 - *line as f32 * auto_leading).abs() < 0.01
            }),
            "wrapped lines step by the face's own leading: {positions:?}"
        );
    }

    /// Justification stretches the whitespace of every line but a paragraph's
    /// last, so the wrapped lines end exactly at `wrap_width`.
    #[test]
    fn justified_lines_fill_the_wrap_width() {
        let mut params = params();
        // Wide enough that the wrapped line holds whitespace to stretch: a
        // line of one long word has nothing to justify and stays as it is.
        params.wrap_width = 260.0;
        params.align = Align::Justify;
        let geometry = geist("one two three four", &params);

        let lines = ints(&geometry, names::LINE_INDEX);
        let last = lines.last().copied().expect("instances exist");
        assert!(last > 0, "the fixture has to wrap");
        let positions = positions(&geometry);
        let advances = floats(&geometry, names::ADVANCE);
        for line in 0..last {
            let extent = positions
                .iter()
                .zip(&advances)
                .zip(&lines)
                .filter(|((_, _), index)| **index == line)
                .map(|((position, advance), _)| position.0 + advance)
                .fold(f32::NEG_INFINITY, f32::max);
            assert!(
                (extent - params.wrap_width).abs() < 0.01,
                "justified line {line} ends at {extent}, not at the wrap width"
            );
        }
        let ragged = positions
            .iter()
            .zip(&advances)
            .zip(&lines)
            .filter(|((_, _), index)| **index == last)
            .map(|((position, advance), _)| position.0 + advance)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            ragged < params.wrap_width - 1.0,
            "a paragraph's last line stays ragged: {ragged}"
        );
    }

    /// The vertical anchor moves the whole block without changing its height.
    #[test]
    fn the_vertical_anchor_moves_the_block() {
        let mut params = params();
        params.leading = 61.0;
        let text = "ab
cd";

        params.anchor = VerticalAnchor::Baseline;
        let baseline = positions(&geist(text, &params));
        assert_eq!(baseline[0].1, 0.0, "the first baseline is the origin");

        params.anchor = VerticalAnchor::Top;
        let top = positions(&geist(text, &params));
        assert!(
            top[0].1 > 0.0,
            "anchoring at the top pushes the first baseline down by the ascent"
        );

        params.anchor = VerticalAnchor::Bottom;
        let bottom = positions(&geist(text, &params));
        assert!(
            bottom[0].1 < -params.leading,
            "anchoring at the bottom lifts the block above the origin: {bottom:?}"
        );

        params.anchor = VerticalAnchor::Center;
        let center = positions(&geist(text, &params));
        assert!(
            center[0].1 < top[0].1 && center[0].1 > bottom[0].1,
            "centred sits between the two"
        );
        for placed in [&top, &bottom, &center] {
            assert!(
                (placed[3].1 - placed[0].1 - params.leading).abs() < 0.01,
                "the anchor must not restretch the block: {placed:?}"
            );
        }
    }

    #[test]
    fn newlines_make_lines_rather_than_instances() {
        let geometry = geist("a\n\nb", &params());
        assert_eq!(geometry.instance_count(), 2, "the newlines have no ink");
        assert_eq!(
            ints(&geometry, names::LINE_INDEX),
            vec![0, 2],
            "an empty paragraph still consumes a line"
        );
    }

    #[test]
    fn trailing_whitespace_is_not_an_instance() {
        assert_eq!(geist("ab  ", &params()).instance_count(), 2);
        assert_eq!(
            geist("a b", &params()).instance_count(),
            3,
            "an interior space is still a character"
        );
    }

    #[test]
    fn an_empty_string_lays_out_to_nothing() {
        let geometry = geist("", &params());
        assert_eq!(geometry.instance_count(), 0);
        assert!(geometry.sources().is_empty());
        assert!(geometry.validate().is_ok());
    }

    /// Nothing about layout may fail the evaluation on a face that resolved:
    /// the geometry it returns has to satisfy the container's own invariants.
    #[test]
    fn the_layout_geometry_validates() {
        let mut params = params();
        params.wrap_width = 140.0;
        params.align = Align::Justify;
        assert!(
            geist(
                "one two three four
five",
                &params
            )
            .validate()
            .is_ok()
        );
    }
}
