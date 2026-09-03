// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Golden pixel tests for per-instance modulation (REQ-MOGRAPH-001).
//!
//! Two pictures, both built from the real registry templates and evaluated
//! through a real [`Evaluator`]:
//!
//! 1. **A wavy grid.** `shape.rect -> scatter.grid -> field.apply(scale, set)
//!    -> field.apply(scale, multiply)` fed by `field.falloff`: the instances
//!    near the falloff centre stay their base size and the ones further out
//!    shrink.
//! 2. **A stagger.** `field.attribute(index) x field.constant + field.time`
//!    applied to `rot` with `add`: each instance is turned a little further
//!    than the one before it, and one clock step later the whole pattern has
//!    travelled by exactly one index.
//!
//! **No pixel value is written down in this file.** A frozen number would
//! fail on an unrelated antialiasing change and pass on a broken falloff, so
//! every assertion is a *relation* — between the instances of one frame,
//! between a modulated frame and the same graph without the modulating node,
//! or between two frames of the same graph. What the relations are measured
//! from is [`Footprint`].
//!
//! CPU only: [`RasterizeProcessor::from_node`] is the zeno reference
//! rasterizer, so neither test needs a GPU adapter.

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor};
use ravel_core::geometry::{Geometry, names};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::types::{FrameBuffer, FrameRate};
use ravel_nodes::field::{
    AddFieldProcessor, ApplyFieldProcessor, AttributeFieldProcessor, ConstantFieldProcessor,
    FalloffFieldProcessor, MultiplyFieldProcessor, TimeFieldProcessor,
};
use ravel_nodes::rasterize::RasterizeProcessor;
use ravel_nodes::scatter::GridProcessor;
use ravel_nodes::shape::RectProcessor;
use std::sync::Arc;

/// 64x64 canvas at 24 fps: `time` is then an exact binary fraction of a
/// second, which is what lets the stagger assertions be exact rather than
/// approximate.
const CANVAS: (u32, u32) = (64, 64);
const FPS: u32 = 24;

fn ctx(frame: u64) -> EvalContext {
    EvalContext::new(frame, FrameRate::new(FPS, 1), CANVAS)
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
/// Going through the template rather than hand-building the node is what
/// makes these tests golden for the *shipped* nodes: a parameter key that
/// stops existing, or a default that moves, shows up here.
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

/// The CPU processor for one node. `rasterize` resolves to the zeno reference
/// implementation, which is the whole reason these tests run without a GPU.
fn processor(node: &Node) -> Arc<dyn NodeProcessor> {
    match node.type_key.as_str() {
        "shape.rect" => Arc::new(RectProcessor),
        "scatter.grid" => Arc::new(GridProcessor),
        "field.constant" => Arc::new(ConstantFieldProcessor),
        "field.falloff" => Arc::new(FalloffFieldProcessor::from_node(node)),
        "field.attribute" => Arc::new(AttributeFieldProcessor::from_node(node)),
        "field.time" => Arc::new(TimeFieldProcessor),
        "field.multiply" => Arc::new(MultiplyFieldProcessor),
        "field.add" => Arc::new(AddFieldProcessor),
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

fn render(graph: &Graph, evaluator: &mut Evaluator, output: u64, frame: u64) -> FrameBuffer {
    evaluator
        .evaluate(graph, NodeId::new(output), &ctx(frame))
        .expect("evaluation succeeds")
        .downcast_ref::<FrameBuffer>()
        .expect("the CPU rasterizer answers a FrameBuffer")
        .clone()
}

fn geometry(graph: &Graph, evaluator: &mut Evaluator, output: u64, frame: u64) -> Geometry {
    evaluator
        .evaluate(graph, NodeId::new(output), &ctx(frame))
        .expect("evaluation succeeds")
        .downcast_ref::<Geometry>()
        .expect("field.apply answers a Geometry")
        .clone()
}

// ---------------------------------------------------------------------------
// Measuring one instance out of the frame
// ---------------------------------------------------------------------------

/// What one instance's footprint looks like, measured from the alpha of the
/// square cell of the canvas that instance owns.
///
/// All three are moments of the coverage, not samples of it:
///
/// * `coverage` — the alpha integral, i.e. the drawn area. Grows with
///   `scale`, and is **invariant under rotation**, which is what separates
///   the two tests below.
/// * `spread_x` / `spread_y` — the alpha-weighted variance of the covered
///   pixels about their own centroid, per axis. Grows with `scale`, and for
///   a rectangle turned by `t` about its centre it is
///   `(a^2 sin^2 t + b^2 cos^2 t) / 3`, so it is a **monotone reading of the
///   turn** while `a > b` and the turn stays under a right angle.
///
/// Subpixel and threshold-free: no pixel is classified as in or out, so the
/// readings move continuously with the modulation instead of stepping.
#[derive(Clone, Copy, Debug)]
struct Footprint {
    coverage: f64,
    spread_x: f64,
    spread_y: f64,
}

/// Measure the cell of half-width `half` centred on `center`.
///
/// Cells are sized so that neighbouring instances cannot reach into one
/// another's cell at the largest scale either test produces; each test says
/// what its own margin is.
fn footprint(frame: &FrameBuffer, center: (f32, f32), half: f32) -> Footprint {
    let data = frame.as_f32();
    let bound = |value: f32, limit: u32| value.max(0.0).min(limit as f32) as u32;
    let (x0, x1) = (
        bound(center.0 - half, frame.width),
        bound(center.0 + half, frame.width),
    );
    let (y0, y1) = (
        bound(center.1 - half, frame.height),
        bound(center.1 + half, frame.height),
    );

    // Moments about the cell centre, then shifted onto the centroid, so the
    // reading does not depend on where in the canvas the cell sits.
    let (mut coverage, mut mx, mut my, mut mxx, mut myy) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
    for y in y0..y1 {
        for x in x0..x1 {
            let alpha = data[((y * frame.width + x) * 4 + 3) as usize] as f64;
            if alpha == 0.0 {
                continue;
            }
            let dx = x as f64 + 0.5 - center.0 as f64;
            let dy = y as f64 + 0.5 - center.1 as f64;
            coverage += alpha;
            mx += alpha * dx;
            my += alpha * dy;
            mxx += alpha * dx * dx;
            myy += alpha * dy * dy;
        }
    }
    if coverage == 0.0 {
        return Footprint {
            coverage: 0.0,
            spread_x: 0.0,
            spread_y: 0.0,
        };
    }
    Footprint {
        coverage,
        spread_x: mxx / coverage - (mx / coverage).powi(2),
        spread_y: myy / coverage - (my / coverage).powi(2),
    }
}

/// The raw pixels of one cell, for the one assertion that is an equality of
/// pictures rather than of moments.
fn cell_pixels(frame: &FrameBuffer, center: (f32, f32), half: f32) -> Vec<f32> {
    let data = frame.as_f32();
    let (x0, x1) = ((center.0 - half) as u32, (center.0 + half) as u32);
    let (y0, y1) = ((center.1 - half) as u32, (center.1 + half) as u32);
    let mut pixels = Vec::with_capacity(((x1 - x0) * (y1 - y0) * 4) as usize);
    for y in y0..y1 {
        let row = (y * frame.width + x0) * 4;
        pixels.extend_from_slice(&data[row as usize..(row + (x1 - x0) * 4) as usize]);
    }
    pixels
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance * expected.abs().max(1e-9),
        "{what}: {actual} is not within {}% of {expected}",
        tolerance * 100.0,
    );
}

// ---------------------------------------------------------------------------
// 1. The wavy grid
// ---------------------------------------------------------------------------

/// Half the grid pitch: the cell each instance is measured in.
const GRID_HALF_CELL: f32 = 10.0;
/// Side of the square each instance stamps, before any scale.
const TILE: f32 = 6.0;
/// The scale the grid carries *before* the falloff multiplies it. Off the
/// `1.0` the scatter node writes and off the falloff's own `0..1` range, so
/// that `multiply` and `set` cannot produce the same picture.
const BASE_SCALE: f32 = 2.5;
/// Falloff radii. `inner` is small but non-zero, `outer` is beyond the
/// furthest instance (28.3 px away), so every instance lands strictly inside
/// the ramp and none is clipped to zero.
const INNER_RADIUS: f32 = 3.0;
const OUTER_RADIUS: f32 = 44.0;

/// The nine instance centres, `(-1..=1, -1..=1)` of the grid pitch around
/// the canvas centre.
fn grid_cell(ix: i32, iy: i32) -> (f32, f32) {
    (
        32.0 + ix as f32 * 2.0 * GRID_HALF_CELL,
        32.0 + iy as f32 * 2.0 * GRID_HALF_CELL,
    )
}

/// `shape.rect -> scatter.grid -> field.apply(set) -> field.apply(multiply)`,
/// rasterized at node 7, plus a second `rasterize` at node 8 fed from the
/// *unmodulated* grid. The reference render is a node in the same graph
/// rather than a second graph so that nothing but the falloff differs
/// between the two pictures.
fn wavy_grid() -> (Graph, Evaluator) {
    let registry = registry();
    let nodes = [
        node(
            &registry,
            "shape.rect",
            1,
            &[
                ("center", ParameterValue::vec2(0.0, 0.0)),
                ("width", ParameterValue::Float(TILE)),
                ("height", ParameterValue::Float(TILE)),
            ],
        ),
        node(
            &registry,
            "scatter.grid",
            2,
            &[
                ("count_x", ParameterValue::Int(3)),
                ("count_y", ParameterValue::Int(3)),
                (
                    "spacing",
                    ParameterValue::vec2(2.0 * GRID_HALF_CELL, 2.0 * GRID_HALF_CELL),
                ),
                ("center", ParameterValue::vec2(32.0, 32.0)),
            ],
        ),
        node(
            &registry,
            "field.constant",
            3,
            &[("value", ParameterValue::Float(BASE_SCALE))],
        ),
        node(
            &registry,
            "field.apply",
            4,
            &[
                ("domain", text("instance")),
                ("target", text(names::SCALE)),
                ("combine", text("set")),
                ("components", text("xy")),
            ],
        ),
        node(
            &registry,
            "field.falloff",
            5,
            &[
                ("shape", text("sphere")),
                ("center", ParameterValue::vec3(32.0, 32.0, 0.0)),
                ("inner_radius", ParameterValue::Float(INNER_RADIUS)),
                ("outer_radius", ParameterValue::Float(OUTER_RADIUS)),
            ],
        ),
        node(
            &registry,
            "field.apply",
            6,
            &[
                ("domain", text("instance")),
                ("target", text(names::SCALE)),
                ("combine", text("multiply")),
                ("components", text("xy")),
            ],
        ),
        node(&registry, "rasterize", 7, &[]),
        node(&registry, "rasterize", 8, &[]),
    ];
    wire(
        &nodes,
        &[
            (1, 2, 0),
            (2, 4, 0),
            (3, 4, 1),
            (4, 6, 0),
            (5, 6, 1),
            (6, 7, 0),
            (4, 8, 0),
        ],
    )
}

/// REQ-MOGRAPH-001: "the modulation result (a grid whose scale ripples with a
/// distance falloff) works".
///
/// The largest instance is `BASE_SCALE * TILE` = 15 px across in a 20 px
/// cell, so no instance can be measured inside its neighbour's cell.
#[test]
fn a_distance_falloff_ripples_the_scale_of_a_grid_of_instances() {
    let (graph, mut evaluator) = wavy_grid();
    let modulated = render(&graph, &mut evaluator, 7, 0);
    let unmodulated = render(&graph, &mut evaluator, 8, 0);

    let measure = |frame: &FrameBuffer, ix, iy| footprint(frame, grid_cell(ix, iy), GRID_HALF_CELL);
    let centre = measure(&modulated, 0, 0);
    let edges = [(0, -1), (-1, 0), (1, 0), (0, 1)].map(|(ix, iy)| measure(&modulated, ix, iy));
    let corners = [(-1, -1), (1, -1), (-1, 1), (1, 1)].map(|(ix, iy)| measure(&modulated, ix, iy));

    // Nothing below means anything if an instance is missing.
    for (label, cell) in std::iter::once(("centre", centre))
        .chain(edges.iter().map(|cell| ("edge", *cell)))
        .chain(corners.iter().map(|cell| ("corner", *cell)))
    {
        assert!(cell.coverage > 0.0, "the {label} instance is not drawn");
    }

    // The ripple itself: the further from the falloff centre, the smaller.
    // Distance, not position, so the four edges agree with each other and so
    // do the four corners — a falloff that had turned into a gradient along
    // one axis would break the symmetry while keeping the ordering.
    for edge in &edges {
        assert_close(edge.coverage, edges[0].coverage, 0.01, "edge instances");
    }
    for corner in &corners {
        assert_close(
            corner.coverage,
            corners[0].coverage,
            0.01,
            "corner instances",
        );
    }
    assert!(
        centre.coverage > edges[0].coverage && edges[0].coverage > corners[0].coverage,
        "the scale has to fall off with distance: centre {}, edge {}, corner {}",
        centre.coverage,
        edges[0].coverage,
        corners[0].coverage,
    );

    // `multiply`, not `set`: the falloff is exactly 1 at its own centre, so
    // the centre instance has to come out of the modulated chain as the
    // *identical picture* the unmodulated chain draws. `set` would replace
    // the base scale with that 1.0 and shrink it by BASE_SCALE.
    assert_eq!(
        cell_pixels(&modulated, grid_cell(0, 0), GRID_HALF_CELL),
        cell_pixels(&unmodulated, grid_cell(0, 0), GRID_HALF_CELL),
        "a unit multiply at the falloff centre has to be a no-op",
    );
    // And the modulation did happen: away from the centre the instances are
    // smaller than the unmodulated ones.
    assert!(
        corners[0].coverage < measure(&unmodulated, -1, -1).coverage,
        "the corner instance was not modulated at all",
    );

    // `components = "xy"` writes both components: the tile stays square.
    // An x-only write would leave the y scale at BASE_SCALE and stretch it.
    for (label, cell) in [
        ("centre", centre),
        ("edge", edges[0]),
        ("corner", corners[0]),
    ] {
        assert_close(
            cell.spread_x,
            cell.spread_y,
            0.02,
            &format!("the {label} instance stays square"),
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The stagger
// ---------------------------------------------------------------------------

/// Half the row pitch: the cell each instance of the stagger row owns.
const ROW_HALF_CELL: f32 = 8.0;
/// The bar each instance stamps. Wider than tall so that turning it changes
/// the vertical spread of its footprint; a square would turn invisibly.
const BAR: (f32, f32) = (12.0, 4.0);
/// Radians of turn per index — `field.constant`'s value, off its `1.0`
/// default and small enough that four instances stay well under the right
/// angle where the spread reading stops being monotone.
const STAGGER: f32 = 0.125;
/// Two frames exactly `STAGGER` seconds apart (3 frames at 24 fps), which is
/// what makes the pattern travel by exactly one index between them.
const EARLY: u64 = 6;
const LATE: u64 = 9;

fn row_cell(index: usize) -> (f32, f32) {
    (32.0 - 24.0 + index as f32 * 2.0 * ROW_HALF_CELL, 32.0)
}

/// `field.attribute(index) x field.constant -> + field.time -> apply(rot,
/// add)`, rasterized at node 9. `field.apply` itself is node 8, so the same
/// graph answers both the `rot` column and the picture.
fn stagger_row() -> (Graph, Evaluator) {
    let registry = registry();
    let nodes = [
        node(
            &registry,
            "shape.rect",
            1,
            &[
                ("center", ParameterValue::vec2(0.0, 0.0)),
                ("width", ParameterValue::Float(BAR.0)),
                ("height", ParameterValue::Float(BAR.1)),
            ],
        ),
        node(
            &registry,
            "scatter.grid",
            2,
            &[
                ("count_x", ParameterValue::Int(4)),
                ("count_y", ParameterValue::Int(1)),
                (
                    "spacing",
                    ParameterValue::vec2(2.0 * ROW_HALF_CELL, 2.0 * ROW_HALF_CELL),
                ),
                ("center", ParameterValue::vec2(32.0, 32.0)),
            ],
        ),
        node(
            &registry,
            "field.attribute",
            3,
            &[
                ("name", text(names::INDEX)),
                ("component", text("x")),
                // Raw index, not normalized: the delay per index then does
                // not rescale when the row gains or loses an instance.
                ("normalize", ParameterValue::Bool(false)),
            ],
        ),
        node(
            &registry,
            "field.constant",
            4,
            &[("value", ParameterValue::Float(STAGGER))],
        ),
        node(&registry, "field.multiply", 5, &[]),
        node(
            &registry,
            "field.time",
            6,
            // `seconds`, so the per-index delay is an absolute duration. The
            // `normalized` mode would tie it to the node's own `duration`
            // parameter, and `frame` to the frame rate.
            &[("mode", text("seconds"))],
        ),
        node(&registry, "field.add", 7, &[]),
        node(
            &registry,
            "field.apply",
            8,
            &[
                ("domain", text("instance")),
                ("target", text(names::ROT)),
                ("combine", text("add")),
            ],
        ),
        node(&registry, "rasterize", 9, &[]),
    ];
    wire(
        &nodes,
        &[
            (1, 2, 0),
            (3, 5, 0),
            (4, 5, 1),
            (6, 7, 0),
            (5, 7, 1),
            (2, 8, 0),
            (7, 8, 1),
            (8, 9, 0),
        ],
    )
}

fn rot_column(geometry: &Geometry) -> Vec<f32> {
    geometry
        .instances()
        .get(names::ROT)
        .expect("the scatter node writes rot")
        .as_f32(names::ROT)
        .expect("rot is an F32 column")
        .to_vec()
}

/// REQ-MOGRAPH-001: "instance attributes can be modulated by a field and
/// reach the parameter" — the stagger, which is the case that needs a
/// non-positional driving value.
///
/// One evaluator across both frames, deliberately: a fresh one per frame
/// cannot serve a stale cache entry, so the frozen-picture bug `field.time`
/// exists to prevent would pass unnoticed.
#[test]
fn a_time_and_index_composition_staggers_instance_rotation() {
    // The whole travelling-wave assertion rests on this: one frame step of
    // the clock has to equal one index step of the stagger.
    assert_eq!(
        (LATE - EARLY) as f32 / FPS as f32,
        STAGGER,
        "the two frames are not exactly one index step apart",
    );

    let (graph, mut evaluator) = stagger_row();
    let early_rot = rot_column(&geometry(&graph, &mut evaluator, 8, EARLY));
    let late_rot = rot_column(&geometry(&graph, &mut evaluator, 8, LATE));
    assert_eq!(early_rot.len(), 4, "the row has four instances");

    // Index-proportional *within* a frame: the difference between instance i
    // and instance 0 is i delays. This is the per-instance modulation; the
    // clock alone would give every instance the same value.
    for (index, (early, late)) in early_rot.iter().zip(&late_rot).enumerate() {
        let step = index as f32 * STAGGER;
        assert!(
            (early - early_rot[0] - step).abs() < 1e-6,
            "frame {EARLY}, instance {index}: {early} is not {step} past {}",
            early_rot[0],
        );
        assert!(
            (late - late_rot[0] - step).abs() < 1e-6,
            "frame {LATE}, instance {index}: {late} is not {step} past {}",
            late_rot[0],
        );
        // A phase, not a speed: the clock shifts every instance equally.
        assert!(
            (late - early - STAGGER).abs() < 1e-6,
            "instance {index} moved by {} instead of {STAGGER}",
            late - early,
        );
    }

    let early_frame = render(&graph, &mut evaluator, 9, EARLY);
    let late_frame = render(&graph, &mut evaluator, 9, LATE);
    let measure = |frame: &FrameBuffer| {
        (0..4)
            .map(|index| footprint(frame, row_cell(index), ROW_HALF_CELL))
            .collect::<Vec<_>>()
    };
    let early_cells = measure(&early_frame);
    let late_cells = measure(&late_frame);

    for (index, cell) in early_cells.iter().enumerate() {
        assert!(cell.coverage > 0.0, "instance {index} is not drawn");
        // A turn is not a scale: the area a turned bar covers is the area of
        // the bar. Modulating `scale` by mistake would show up right here.
        assert_close(
            cell.coverage,
            early_cells[0].coverage,
            0.05,
            &format!("instance {index} covers the same area as instance 0"),
        );
    }

    // The picture: each instance is turned further than the one before it,
    // so the vertical spread of its footprint grows along the row.
    for pair in early_cells.windows(2) {
        assert!(
            pair[1].spread_y > pair[0].spread_y * 1.1,
            "the row is not staggered in the pixels: {:?} then {:?}",
            pair[0],
            pair[1],
        );
    }

    // And the stagger travels. One clock step later, instance i - 1 stands
    // exactly where instance i stood, because the clock advanced by one
    // index step. Constant per-instance offsets would move the whole row and
    // fail this; so would a stagger with the wrong coefficient.
    for index in 1..4 {
        assert_close(
            late_cells[index - 1].spread_y,
            early_cells[index].spread_y,
            0.01,
            &format!("instance {} took over instance {index}'s turn", index - 1),
        );
    }
}
