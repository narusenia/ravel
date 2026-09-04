// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `text.font → text.layout → rasterize` on the CPU reference rasterizer
//! (REQ-MOGRAPH-004, typography-plan unit 2).
//!
//! The unit tests beside the processors check the shaping and the attribute
//! columns; this one checks the claim those columns exist for — that the glyph
//! instances land on the existing instance path in `rasterize` and put ink on
//! a frame. It is the first test in the repository in which text is visible.
//!
//! No golden image and no GPU: the plan's verification section says the
//! correctness of shaping is pinned by cluster counts, advances and
//! attributes, not by pixel comparison against a face. What is asserted here
//! is where the ink is and where it is not — inside the letters' band, with
//! nothing above the cap height and nothing below the baseline, and with the
//! counter of an `o` transparent (the rasterizer's fill runs; see the
//! `rasterize` module header).

use ravel_core::eval::{EvalContext, Evaluator};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::types::{FrameBuffer, FrameRate};
use ravel_nodes::{rasterize, text};
use std::sync::Arc;

const WIDTH: u32 = 160;
const HEIGHT: u32 = 80;
/// Em size in composition pixels. Large enough that a stem covers whole
/// pixels, small enough to leave margins inside the frame.
const SIZE: f32 = 48.0;
/// Where `anchor = "top"` puts the first baseline: the face's ascent, which
/// is 1.005 em for the bundled Geist Regular.
const BASELINE: f32 = SIZE * 1.005;

fn pixel(frame: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
    let index = ((y * frame.width + x) * 4) as usize;
    frame.as_f32()[index..index + 4]
        .try_into()
        .expect("four channels")
}

/// Total alpha of one pixel row.
fn row_coverage(frame: &FrameBuffer, y: u32) -> f32 {
    (0..frame.width).map(|x| pixel(frame, x, y)[3]).sum()
}

/// Render `text` through `text.font → text.layout → rasterize`.
///
/// Composition space has its origin at the top-left pixel, so the layout is
/// anchored at the **top** of its block rather than on its baseline: that
/// puts the first baseline one ascent down and the glyphs inside the frame,
/// without a transform node in between to confuse what is being tested.
fn render(text_value: &str) -> FrameBuffer {
    let font = Node::new(NodeId::new(1), "text.font")
        .with_output("font", DataTypeId::FONT)
        .with_param("family", ParameterValue::String("Geist".into()))
        .with_param("weight", ParameterValue::String("regular".into()))
        .with_param("style", ParameterValue::String("normal".into()));
    let layout = Node::new(NodeId::new(2), "text.layout")
        .with_input("font", &[DataTypeId::FONT])
        .with_output("output", DataTypeId::GEOMETRY)
        .with_param("text", ParameterValue::String(text_value.into()))
        .with_param("size", ParameterValue::Float(SIZE))
        .with_param("tracking", ParameterValue::Float(0.0))
        .with_param("leading", ParameterValue::Float(0.0))
        .with_param("align", ParameterValue::String("left".into()))
        .with_param("wrap_width", ParameterValue::Float(0.0))
        .with_param("anchor", ParameterValue::String("top".into()));
    let raster = Node::new(NodeId::new(3), "rasterize")
        .with_input("geometry", &[DataTypeId::GEOMETRY])
        .with_output("frame", DataTypeId::FRAME_BUFFER)
        .with_param("fill", ParameterValue::Bool(true))
        .with_param("stroke_width", ParameterValue::Float(0.0));

    let graph = Graph::new()
        .add_node(font)
        .expect("the font node")
        .add_node(layout)
        .expect("the layout node")
        .add_node(raster)
        .expect("the rasterize node")
        .add_edge(
            EdgeId::new(1),
            NodeId::new(1),
            OutputPortIndex(0),
            NodeId::new(2),
            InputPortIndex(0),
        )
        .expect("font to layout")
        .add_edge(
            EdgeId::new(2),
            NodeId::new(2),
            OutputPortIndex(0),
            NodeId::new(3),
            InputPortIndex(0),
        )
        .expect("layout to rasterize");

    let mut evaluator = Evaluator::new();
    for node in graph.nodes() {
        let processor: Arc<dyn ravel_core::eval::NodeProcessor> = match node.type_key.as_str() {
            "text.font" => Arc::new(text::FontProcessor),
            "text.layout" => Arc::new(text::LayoutProcessor),
            "rasterize" => Arc::new(rasterize::RasterizeProcessor::from_node(node)),
            other => panic!("the fixture graph has no {other} node"),
        };
        evaluator.register(node.id, processor);
    }
    let ctx = EvalContext::new(0, FrameRate::new(30, 1), (WIDTH, HEIGHT));
    let value = evaluator
        .evaluate(&graph, NodeId::new(3), &ctx)
        .expect("the text graph evaluates");
    value
        .downcast_ref::<FrameBuffer>()
        .expect("rasterize produces a frame")
        .clone()
}

/// Text puts ink on the frame, and only in the band between the cap height
/// and the baseline.
#[test]
fn text_rasterizes_to_ink_on_its_own_baseline() {
    let frame = render("Ravel");

    let ink: f32 = (0..HEIGHT).map(|y| row_coverage(&frame, y)).sum();
    assert!(ink > 100.0, "the letters have to cover pixels: {ink}");

    // Cap height is roughly 0.7 em, so the row a third of the way up from the
    // baseline is inside every capital and every lowercase x-height.
    let inside = row_coverage(&frame, BASELINE as u32 - 12);
    assert!(
        inside > 5.0,
        "the row through the middle of the letters has to be covered: {inside}"
    );

    // Three rows below the baseline: `Ravel` has no descender, so only the
    // antialiased edge of the baseline may reach here.
    let below = row_coverage(&frame, BASELINE as u32 + 3);
    assert!(
        below < 1.0,
        "nothing may hang below the baseline of `Ravel`: {below}"
    );

    // The cap height of the bundled face is about 0.73 em, so the rows
    // between the top of the block and the top of the `R` stay empty. Ink up
    // there would mean the outlines were not mirrored into the Y-down
    // composition space.
    for y in 0..8 {
        let above = row_coverage(&frame, y);
        assert!(
            above < 0.01,
            "ink on row {y}, above the cap height: {above}"
        );
    }
}

/// An empty string is a valid document state — a text node the user has not
/// typed into yet — and has to rasterize to an empty frame rather than fail.
#[test]
fn empty_text_rasterizes_to_an_empty_frame() {
    let frame = render("");
    let ink: f32 = (0..HEIGHT).map(|y| row_coverage(&frame, y)).sum();
    assert_eq!(ink, 0.0, "an empty string draws nothing");
}

/// The runs of covered pixels in one row: `(start, end)` for each stretch of
/// alpha above a half.
fn ink_runs(frame: &FrameBuffer, y: u32) -> Vec<(u32, u32)> {
    let mut runs = Vec::new();
    let mut start = None;
    for x in 0..frame.width {
        match (pixel(frame, x, y)[3] > 0.5, start) {
            (true, None) => start = Some(x),
            (false, Some(from)) => {
                runs.push((from, x));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        runs.push((from, frame.width));
    }
    runs
}

/// A glyph counter is a **hole**, not more ink.
///
/// The outline of an `o` is two closed contours wound against each other, and
/// the rasterizer used to fill each of them on its own — which put the second
/// one's area back inside the first. A row through the middle of the letter
/// therefore has to cross ink exactly twice, with fully transparent pixels
/// between the two crossings.
#[test]
fn a_glyph_counter_stays_transparent() {
    let frame = render("o");
    // Halfway up the x-height, which for the bundled face is about half an
    // em: inside both stems of the `o` and clear of its curves.
    let row = (BASELINE - SIZE * 0.25) as u32;
    let runs = ink_runs(&frame, row);
    assert_eq!(
        runs.len(),
        2,
        "row {row} has to cross the two stems of the `o` and nothing else: {runs:?}"
    );

    // Inset past the antialiased fringe of each stem: those pixels are
    // partially covered by design. Everything between them has to be
    // *exactly* transparent — an under-inked counter would still be a filled
    // one.
    let (left, right) = (runs[0].1 + 2, runs[1].0 - 2);
    assert!(
        right > left,
        "the counter has to be wider than its own antialiasing: {left}..{right}"
    );
    for x in left..right {
        let hole = pixel(&frame, x, row);
        assert_eq!(
            hole[3], 0.0,
            "the counter is empty, not merely faint, at ({x},{row}): {hole:?}"
        );
    }
}
