// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `text.to_path` on the CPU reference rasterizer (REQ-MOGRAPH-004,
//! typography-plan unit 5).
//!
//! Two pictures, both built from the real registry templates and evaluated
//! through a real [`Evaluator`]:
//!
//! 1. **A distorted string.** `text.layout -> text.to_path ->
//!    field.apply(P, add)` fed by `field.noise x field.constant`. The
//!    acceptance criterion of REQ-MOGRAPH-004 that this unit owns — "the
//!    converted geometry is affected by fields" — is that the *letter shapes*
//!    change, not that the characters move: the same field applied to the
//!    unconverted layout could only shift whole characters about.
//! 2. **Two counters that stay holes.** Per-character colour on the Instance
//!    domain, converted to paths, rasterized. `rasterize` fills a run of
//!    **consecutive** same-style closed paths as one non-zero region, so a
//!    conversion that interleaved two characters' contours would separate a
//!    counter from its own outer contour and fill the hole in. Two characters
//!    of *different* colour is what makes the ordering load-bearing rather
//!    than incidental.
//!
//! **No pixel value is written down in this file.** Every assertion is a
//! relation: between the converted geometry and the geometry the field
//! rewrote, between the modulated picture and the same graph without the
//! modulating node, or between the two crossings of a row through an `o`.
//!
//! CPU only: `RasterizeProcessor::from_node` is the zeno reference
//! rasterizer, so neither test needs a GPU adapter.

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor};
use ravel_core::geometry::{Geometry, names};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::types::{FrameBuffer, FrameRate, Vec2};
use ravel_nodes::field::{
    ApplyFieldProcessor, AttributeFieldProcessor, ConstantFieldProcessor, MultiplyFieldProcessor,
    NoiseFieldProcessor,
};
use ravel_nodes::rasterize::RasterizeProcessor;
use ravel_nodes::text::{FontProcessor, LayoutProcessor, ToPathProcessor};
use std::sync::Arc;

const CANVAS: (u32, u32) = (320, 140);
/// Em size in composition pixels: large enough that a stem covers whole
/// pixels and a counter is several pixels wide.
const SIZE: f32 = 72.0;
/// The bundled Geist Regular's ascent is 1.005 em, and `anchor = "top"` puts
/// the first baseline there.
const BASELINE: f32 = SIZE * 1.005;
/// Peak outline displacement in composition pixels — `field.constant` times
/// the noise, which runs to about ±1. An eighth of the em: plainly a distorted
/// letter, and still nowhere near the frame edges.
const WOBBLE: f32 = 9.0;
/// Noise frequency in cycles per composition pixel. Low enough that
/// neighbouring outline points move together (a wobble, not a shredded
/// outline) and high enough that a whole glyph is not one flat sample.
const NOISE_FREQUENCY: f32 = 0.03;
/// Off the default, so a seed the processor forgot to read would show.
const NOISE_SEED: i32 = 17;

fn ctx() -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), CANVAS)
}

fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::new();
    register_builtins(&mut registry);
    registry
}

fn text(value: &str) -> ParameterValue {
    ParameterValue::String(value.into())
}

/// A node from its real registry template, with `params` overridden.
///
/// Going through the template is what makes this golden for the *shipped*
/// node: a parameter key that stops existing, or a default that moves, shows
/// up here.
fn node(
    registry: &NodeRegistry,
    type_key: &str,
    id: u64,
    params: &[(&str, ParameterValue)],
) -> Node {
    let mut node = registry
        .create_node(type_key, NodeId::new(id))
        .unwrap_or_else(|| panic!("{type_key} is not registered"));
    for (key, value) in params {
        node.parameters
            .iter_mut()
            .find(|parameter| parameter.key == *key)
            .unwrap_or_else(|| panic!("{type_key} has no {key} parameter"))
            .value = value.clone();
    }
    node
}

fn processor(node: &Node) -> Arc<dyn NodeProcessor> {
    match node.type_key.as_str() {
        "text.font" => Arc::new(FontProcessor),
        "text.layout" => Arc::new(LayoutProcessor),
        "text.to_path" => Arc::new(ToPathProcessor),
        "field.noise" => Arc::new(NoiseFieldProcessor::from_node(node)),
        "field.constant" => Arc::new(ConstantFieldProcessor),
        "field.multiply" => Arc::new(MultiplyFieldProcessor),
        "field.attribute" => Arc::new(AttributeFieldProcessor::from_node(node)),
        "field.apply" => Arc::new(ApplyFieldProcessor::from_node(node)),
        "rasterize" => Arc::new(RasterizeProcessor::from_node(node)),
        other => panic!("no CPU processor wired for {other}"),
    }
}

/// Build the graph and an evaluator with every node's processor registered.
/// `edges` are `(from node, to node, to port)`; every output is port 0.
fn wire(nodes: &[Node], edges: &[(u64, u64, u32)]) -> (Graph, Evaluator) {
    let mut graph = Graph::new();
    let mut evaluator = Evaluator::new();
    for node in nodes {
        graph = graph.add_node(node.clone()).expect("node ids are unique");
        evaluator.register(node.id, processor(node));
    }
    for (i, (from, to, port)) in edges.iter().enumerate() {
        graph = graph
            .add_edge(
                EdgeId::new(i as u64 + 1),
                NodeId::new(*from),
                OutputPortIndex(0),
                NodeId::new(*to),
                InputPortIndex(*port),
            )
            .expect("the wiring is type-compatible");
    }
    (graph, evaluator)
}

fn render(graph: &Graph, evaluator: &mut Evaluator, output: u64) -> FrameBuffer {
    evaluator
        .evaluate(graph, NodeId::new(output), &ctx())
        .expect("evaluation succeeds")
        .downcast_ref::<FrameBuffer>()
        .expect("the CPU rasterizer answers a FrameBuffer")
        .clone()
}

fn geometry(graph: &Graph, evaluator: &mut Evaluator, output: u64) -> Geometry {
    evaluator
        .evaluate(graph, NodeId::new(output), &ctx())
        .expect("evaluation succeeds")
        .downcast_ref::<Geometry>()
        .expect("the node answers a Geometry")
        .clone()
}

fn positions(geometry: &Geometry) -> Vec<Vec2> {
    geometry
        .points()
        .get(names::P)
        .expect("the conversion writes P on the point domain")
        .as_vec2(names::P)
        .expect("a Vec2 column")
        .to_vec()
}

fn pixel(frame: &FrameBuffer, x: u32, y: u32) -> [f32; 4] {
    let index = ((y * frame.width + x) * 4) as usize;
    frame.as_f32()[index..index + 4]
        .try_into()
        .expect("four channels")
}

/// Alpha moments of the whole frame: the drawn area, and the alpha-weighted
/// variance of the covered pixels about their own centroid, per axis.
///
/// Subpixel and threshold-free — no pixel is classified in or out — so the
/// readings move continuously with the modulation instead of stepping. A
/// rigid translation of the ink leaves all three unchanged; only a change of
/// *shape* moves the spreads.
struct Moments {
    coverage: f64,
    spread_x: f64,
    spread_y: f64,
}

fn moments(frame: &FrameBuffer) -> Moments {
    let data = frame.as_f32();
    let (mut coverage, mut mx, mut my, mut mxx, mut myy) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
    for y in 0..frame.height {
        for x in 0..frame.width {
            let alpha = data[((y * frame.width + x) * 4 + 3) as usize] as f64;
            if alpha == 0.0 {
                continue;
            }
            let (dx, dy) = (x as f64, y as f64);
            coverage += alpha;
            mx += alpha * dx;
            my += alpha * dy;
            mxx += alpha * dx * dx;
            myy += alpha * dy * dy;
        }
    }
    if coverage == 0.0 {
        return Moments {
            coverage: 0.0,
            spread_x: 0.0,
            spread_y: 0.0,
        };
    }
    Moments {
        coverage,
        spread_x: mxx / coverage - (mx / coverage).powi(2),
        spread_y: myy / coverage - (my / coverage).powi(2),
    }
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

/// The `text.font -> text.layout -> text.to_path` head every graph here
/// starts with: node 1 is the face, node 2 the layout, node 3 the outlines.
fn conversion_head(registry: &NodeRegistry, string: &str) -> ([Node; 3], [(u64, u64, u32); 2]) {
    (
        [
            node(
                registry,
                "text.font",
                1,
                &[("family", text("Geist")), ("weight", text("regular"))],
            ),
            node(
                registry,
                "text.layout",
                2,
                &[
                    ("text", text(string)),
                    ("size", ParameterValue::Float(SIZE)),
                    // Composition space has its origin at the top-left pixel,
                    // so anchoring at the top of the block is what puts the
                    // glyphs inside the frame without a transform node in
                    // between to confuse what is being tested.
                    ("anchor", text("top")),
                ],
            ),
            node(registry, "text.to_path", 3, &[]),
        ],
        [(1, 2, 0), (2, 3, 0)],
    )
}

// ---------------------------------------------------------------------------
// 1. The distorted string
// ---------------------------------------------------------------------------

/// `text.to_path -> field.apply(P, add)` driven by `field.noise x
/// field.constant`, rasterized at node 8, plus a second `rasterize` at node 9
/// fed from the **unmodulated** outlines. The reference render is a node in
/// the same graph rather than a second graph so that nothing but the field
/// differs between the two pictures.
fn wobbling_text() -> (Graph, Evaluator) {
    let registry = registry();
    let (head, head_edges) = conversion_head(&registry, "Ravel");
    let nodes = [
        head[0].clone(),
        head[1].clone(),
        head[2].clone(),
        node(
            &registry,
            "field.noise",
            4,
            &[
                ("seed", ParameterValue::Int(NOISE_SEED)),
                ("frequency", ParameterValue::Float(NOISE_FREQUENCY)),
                ("octaves", ParameterValue::Int(1)),
            ],
        ),
        node(
            &registry,
            "field.constant",
            5,
            &[("value", ParameterValue::Float(WOBBLE))],
        ),
        node(&registry, "field.multiply", 6, &[]),
        node(
            &registry,
            "field.apply",
            7,
            &[
                // The point of the whole unit: the **point** domain, so the
                // field reaches the outline control points. On the instance
                // domain the same field could only slide whole characters.
                ("domain", text("point")),
                ("target", text(names::P)),
                ("combine", text("add")),
                ("components", text("xy")),
            ],
        ),
        node(&registry, "rasterize", 8, &[]),
        node(&registry, "rasterize", 9, &[]),
    ];
    let mut edges = head_edges.to_vec();
    edges.extend([
        (4, 6, 0),
        (5, 6, 1),
        (3, 7, 0),
        (6, 7, 1),
        (7, 8, 0),
        (3, 9, 0),
    ]);
    wire(&nodes, &edges)
}

/// REQ-MOGRAPH-004: "the converted geometry is affected by fields".
///
/// The picture has to change, and it has to change *shape* — a field that
/// only translated the ink would satisfy "the frames differ" while proving
/// nothing about the outlines.
#[test]
fn a_noise_field_distorts_the_converted_letter_outlines() {
    let (graph, mut evaluator) = wobbling_text();

    // The geometry first: the field rewrote the outline points and nothing
    // else about the structure.
    let converted = geometry(&graph, &mut evaluator, 3);
    let modulated = geometry(&graph, &mut evaluator, 7);
    assert!(
        converted.point_count() > 50,
        "`Ravel` at {SIZE} px has plenty of outline points: {}",
        converted.point_count()
    );
    assert_eq!(
        modulated.point_count(),
        converted.point_count(),
        "the field moves points, it does not add or drop any"
    );
    assert_eq!(
        modulated.primitive_count(),
        converted.primitive_count(),
        "and it leaves the contours alone"
    );

    let displacements: Vec<Vec2> = positions(&converted)
        .iter()
        .zip(positions(&modulated))
        .map(|(before, after)| Vec2(after.0 - before.0, after.1 - before.1))
        .collect();
    let length = |v: &Vec2| (v.0 * v.0 + v.1 * v.1).sqrt();
    let longest = displacements.iter().map(length).fold(0.0f32, f32::max);
    let shortest = displacements
        .iter()
        .map(length)
        .fold(f32::INFINITY, f32::min);
    assert!(
        longest > 0.1 * SIZE,
        "the outline barely moved: the longest displacement is {longest} px"
    );
    assert!(
        longest <= WOBBLE * std::f32::consts::SQRT_2 + 1e-3,
        "a displacement past the field's own amplitude: {longest} px"
    );
    // Not a translation: different points moved by different amounts, which
    // is the difference between a distorted letter and a moved one.
    assert!(
        longest - shortest > 0.05 * SIZE,
        "every point moved alike ({shortest} to {longest} px), so this is a shift, not a distortion"
    );

    // Then the picture.
    let wobbled = render(&graph, &mut evaluator, 8);
    let plain = render(&graph, &mut evaluator, 9);
    let (before, after) = (moments(&plain), moments(&wobbled));
    assert!(
        before.coverage > 500.0 && after.coverage > 500.0,
        "both renders have to put ink on the frame: {} then {}",
        before.coverage,
        after.coverage,
    );

    // The two pictures are not the same picture. This is the assertion that
    // fails if `field.apply` turns into a pass-through.
    let differing = (0..CANVAS.1)
        .flat_map(|y| (0..CANVAS.0).map(move |x| (x, y)))
        .filter(|(x, y)| (pixel(&wobbled, *x, *y)[3] - pixel(&plain, *x, *y)[3]).abs() > 0.05)
        .count();
    assert!(
        differing > 200,
        "the field did not change the picture: {differing} pixels differ"
    );

    // And it changed shape, not position. A rigid shift of the ink leaves
    // both spreads exactly where they were.
    let reshaped = |modulated: f64, plain: f64| (modulated - plain).abs() / plain;
    assert!(
        reshaped(after.spread_x, before.spread_x) > 0.01
            || reshaped(after.spread_y, before.spread_y) > 0.01,
        "the ink was moved but not reshaped: spreads {:?} then {:?}",
        (before.spread_x, before.spread_y),
        (after.spread_x, after.spread_y),
    );
}

// ---------------------------------------------------------------------------
// 2. The counters that stay holes
// ---------------------------------------------------------------------------

/// Two `o`s, coloured per character on the Instance domain and then
/// converted: `field.attribute(char_progress) -> field.apply(Cd, set, "b")`.
///
/// The per-character colour is what makes the contour ordering matter. With
/// one colour every contour of both letters shares a [`RunKey`] and the
/// non-zero fill opens both counters however the contours are ordered; with
/// two colours a run ends at each character boundary, so a counter that does
/// not immediately follow its own outer contour lands in a run of its own and
/// fills in.
fn two_coloured_letters() -> (Graph, Evaluator) {
    let registry = registry();
    let (head, head_edges) = conversion_head(&registry, "oo");
    let nodes = [
        head[0].clone(),
        head[1].clone(),
        head[2].clone(),
        node(
            &registry,
            "field.attribute",
            4,
            &[
                // 0 for the first character and 1 for the second, straight
                // out of the layout, so the two letters cannot share a run.
                ("name", text(names::CHAR_PROGRESS)),
                ("component", text("x")),
                ("normalize", ParameterValue::Bool(false)),
            ],
        ),
        node(
            &registry,
            "field.apply",
            5,
            &[
                ("domain", text("instance")),
                ("target", text(names::CD)),
                ("combine", text("set")),
                // Blue only: `Cd` is created opaque white, so writing every
                // component would take the alpha to the field's value and
                // make one of the letters invisible.
                ("components", text("b")),
            ],
        ),
        node(&registry, "rasterize", 6, &[]),
    ];
    let mut edges = head_edges.to_vec();
    // The colour lands on the instances *before* the conversion, so the
    // conversion is what has to carry it onto the primitives.
    edges.clear();
    edges.extend([(1, 2, 0), (2, 5, 0), (4, 5, 1), (5, 3, 0), (3, 6, 0)]);
    wire(&nodes, &edges)
}

/// A glyph counter is a **hole**, and it stays one after the conversion.
///
/// The guard on the contour ordering `expand_instances` promises: one
/// character's contours stay consecutive and in the source's own order.
/// Interleaving them separates each counter from its outer contour, and
/// because the two letters here are different colours the two would then be
/// in different fill runs — the hole would fill in.
#[test]
fn a_glyph_counter_stays_a_hole_after_the_conversion() {
    let (graph, mut evaluator) = two_coloured_letters();

    // The conversion has to have carried the per-character colour onto the
    // primitive domain, or the rest of this test proves nothing about
    // ordering: with one colour any ordering opens the counters.
    let converted = geometry(&graph, &mut evaluator, 3);
    let colors = converted
        .primitive_attrs()
        .get(names::CD)
        .expect("the per-character colour has to reach the primitives")
        .as_color(names::CD)
        .expect("a Color column")
        .to_vec();
    assert_eq!(colors.len(), 4, "two letters of two contours each");
    assert_eq!(colors[0], colors[1], "a letter's contours share its colour");
    assert_eq!(colors[2], colors[3]);
    assert_ne!(
        colors[0], colors[2],
        "the two letters have to differ, or the ordering does not matter"
    );

    let frame = render(&graph, &mut evaluator, 6);
    // Halfway up the x-height, which for the bundled face is about half an
    // em: inside both stems of each `o` and clear of its curves.
    let row = (BASELINE - SIZE * 0.25) as u32;
    let runs = ink_runs(&frame, row);
    assert_eq!(
        runs.len(),
        4,
        "row {row} has to cross two stems per letter and nothing else: {runs:?}"
    );

    // Inset past the antialiased fringe of each stem: those pixels are
    // partially covered by design. Everything between the two stems of a
    // letter has to be *exactly* transparent — an under-inked counter would
    // still be a filled one.
    for counter in 0..2 {
        let (left, right) = (runs[counter * 2].1 + 2, runs[counter * 2 + 1].0 - 2);
        assert!(
            right > left,
            "counter {counter} has to be wider than its own antialiasing: {left}..{right}"
        );
        for x in left..right {
            let hole = pixel(&frame, x, row);
            assert_eq!(
                hole[3], 0.0,
                "counter {counter} is empty, not merely faint, at ({x},{row}): {hole:?}"
            );
        }
    }

    // And the two letters really did draw in two colours, so the run split
    // the ordering has to survive was actually there in the pixels.
    let stem_colour = |run: usize| pixel(&frame, (runs[run].0 + runs[run].1) / 2, row);
    let (first, second) = (stem_colour(0), stem_colour(2));
    assert_ne!(
        first[2], second[2],
        "the two letters drew in one colour: {first:?} and {second:?}"
    );
}
