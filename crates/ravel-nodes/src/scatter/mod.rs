// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Instance duplication node processors.
//!
//! Each processor takes an optional source [`Geometry`] input and produces a
//! new [`Geometry`] whose instance domain carries `index`, `P`, `rot`, and
//! `scale` for every copy.  The source geometry is set as `instance_source`
//! so the rasterizer stamps it at each instance position.

use std::f32::consts::PI;
use std::sync::Arc;

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{AttributeArray, Geometry, Primitive, names};
use ravel_core::graph::Node;
use ravel_core::types::{NodeData, Vec2};

fn populate_instances(geo: &mut Geometry, positions: Vec<Vec2>, rotations: Vec<f32>) {
    let n = positions.len();
    let indices: Vec<i32> = (0..n as i32).collect();
    let scales = vec![Vec2(1.0, 1.0); n];

    geo.instances_mut()
        .insert(names::INDEX, AttributeArray::I32(indices))
        .expect("first column");
    geo.instances_mut()
        .insert(names::P, AttributeArray::Vec2(positions))
        .expect("same length as index");
    geo.instances_mut()
        .insert(names::ROT, AttributeArray::F32(rotations))
        .expect("same length");
    geo.instances_mut()
        .insert(names::SCALE, AttributeArray::Vec2(scales))
        .expect("same length");
}

/// Optional source geometry on the first input slot, if connected.
fn source_input(inputs: &[Option<Arc<dyn NodeData>>]) -> Option<&Geometry> {
    inputs
        .first()
        .and_then(|input| input.as_ref())
        .and_then(|input| input.downcast_ref::<Geometry>())
}

// ---------------------------------------------------------------------------
// Grid
// ---------------------------------------------------------------------------

pub struct GridProcessor;

impl GridProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for GridProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let source = source_input(inputs);

        let count_x = params.i32_or("count_x", 5);
        let count_y = params.i32_or("count_y", 5);
        let spacing_x = params.f32_or("spacing_x", 20.0);
        let spacing_y = params.f32_or("spacing_y", 20.0);
        let center_x = params.f32_or("center_x", 0.0);
        let center_y = params.f32_or("center_y", 0.0);

        let nx = count_x.max(1) as usize;
        let ny = count_y.max(1) as usize;
        let total_w = (nx as f32 - 1.0) * spacing_x;
        let total_h = (ny as f32 - 1.0) * spacing_y;
        let origin_x = center_x - total_w / 2.0;
        let origin_y = center_y - total_h / 2.0;

        let n = nx * ny;
        let mut positions = Vec::with_capacity(n);
        let rotations = vec![0.0; n];

        for iy in 0..ny {
            for ix in 0..nx {
                positions.push(Vec2(
                    origin_x + ix as f32 * spacing_x,
                    origin_y + iy as f32 * spacing_y,
                ));
            }
        }

        let mut geo = Geometry::new();
        if let Some(src) = source {
            geo.set_instance_source(Some(Arc::new(src.clone())));
        }
        populate_instances(&mut geo, positions, rotations);
        Ok(Arc::new(geo))
    }
}

// ---------------------------------------------------------------------------
// Circular
// ---------------------------------------------------------------------------

pub struct CircularProcessor;

impl CircularProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for CircularProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let source = source_input(inputs);

        let count = params.i32_or("count", 8);
        let radius = params.f32_or("radius", 50.0);
        let center_x = params.f32_or("center_x", 0.0);
        let center_y = params.f32_or("center_y", 0.0);
        let align_rotation = params.bool_or("align_rotation", true);

        let n = count.max(1) as usize;
        let mut positions = Vec::with_capacity(n);
        let mut rotations = Vec::with_capacity(n);

        for i in 0..n {
            let angle = 2.0 * PI * i as f32 / n as f32;
            positions.push(Vec2(
                center_x + radius * angle.cos(),
                center_y + radius * angle.sin(),
            ));
            rotations.push(if align_rotation { angle } else { 0.0 });
        }

        let mut geo = Geometry::new();
        if let Some(src) = source {
            geo.set_instance_source(Some(Arc::new(src.clone())));
        }
        populate_instances(&mut geo, positions, rotations);
        Ok(Arc::new(geo))
    }
}

// ---------------------------------------------------------------------------
// Path array — instances along a path, rot from tangent
// ---------------------------------------------------------------------------

pub struct PathArrayProcessor;

impl PathArrayProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

/// One straight segment of a path polyline with its global arc-length span.
struct PathSegment {
    a: Vec2,
    b: Vec2,
    cum_start: f32,
    cum_end: f32,
}

impl NodeProcessor for PathArrayProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let path_geo = inputs
            .first()
            .and_then(|input| input.as_ref())
            .and_then(|input| input.downcast_ref::<Geometry>())
            .context("scatter.path_array expects a path Geometry on input 0")?;

        let source = inputs
            .get(1)
            .and_then(|input| input.as_ref())
            .and_then(|input| input.downcast_ref::<Geometry>());

        let positions_col = path_geo
            .points()
            .get(names::P)
            .context("path geometry missing P")?;
        let path_points = positions_col.as_vec2(names::P)?;

        let segments = collect_path_segments(path_geo, path_points);
        let total_len = segments.last().map_or(0.0, |s| s.cum_end);
        if segments.is_empty() || total_len < 1e-9 {
            return Ok(Arc::new(Geometry::new()));
        }

        let n = params.i32_or("count", 10).max(1) as usize;
        let mut positions = Vec::with_capacity(n);
        let mut rotations = Vec::with_capacity(n);

        for i in 0..n {
            let t = if n > 1 {
                i as f32 / (n - 1) as f32
            } else {
                0.0
            };
            let target_len = t * total_len;

            let idx = segments
                .partition_point(|s| s.cum_end < target_len)
                .min(segments.len() - 1);
            let seg = &segments[idx];
            let span = seg.cum_end - seg.cum_start;
            let seg_t = if span > 1e-9 {
                (target_len - seg.cum_start) / span
            } else {
                0.0
            };

            let pos = Vec2(
                seg.a.0 + (seg.b.0 - seg.a.0) * seg_t,
                seg.a.1 + (seg.b.1 - seg.a.1) * seg_t,
            );
            let rot = (seg.b.1 - seg.a.1).atan2(seg.b.0 - seg.a.0);

            positions.push(pos);
            rotations.push(rot);
        }

        let mut geo = Geometry::new();
        if let Some(src) = source {
            geo.set_instance_source(Some(Arc::new(src.clone())));
        }
        populate_instances(&mut geo, positions, rotations);
        Ok(Arc::new(geo))
    }
}

/// Flattens path primitives into a global segment list with cumulative arc
/// lengths.  Closed primitives contribute their closing segment.  A geometry
/// with no primitives falls back to treating the whole P column as one open
/// polyline.
fn collect_path_segments(geo: &Geometry, points: &[Vec2]) -> Vec<PathSegment> {
    let mut segments = Vec::new();
    let mut cum = 0.0f32;

    let mut push_polyline = |verts: &[Vec2], closed: bool, segments: &mut Vec<PathSegment>| {
        if verts.len() < 2 {
            return;
        }
        let mut push = |a: Vec2, b: Vec2, segments: &mut Vec<PathSegment>| {
            let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
            segments.push(PathSegment {
                a,
                b,
                cum_start: cum,
                cum_end: cum + len,
            });
            cum += len;
        };
        for w in verts.windows(2) {
            push(w[0], w[1], segments);
        }
        if closed && verts.len() >= 3 {
            push(verts[verts.len() - 1], verts[0], segments);
        }
    };

    let prims = geo.primitives();
    if prims.is_empty() {
        push_polyline(points, false, &mut segments);
    } else {
        for prim in prims {
            let Primitive::Path { verts, closed } = prim;
            if verts.end > points.len() {
                continue;
            }
            push_polyline(&points[verts.clone()], *closed, &mut segments);
        }
    }
    segments
}

// ---------------------------------------------------------------------------
// Scatter — deterministic random placement
// ---------------------------------------------------------------------------

pub struct ScatterProcessor;

impl ScatterProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for ScatterProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let source = source_input(inputs);

        let count = params.i32_or("count", 20);
        let area_x = params.f32_or("area_x", 200.0);
        let area_y = params.f32_or("area_y", 200.0);
        let center_x = params.f32_or("center_x", 0.0);
        let center_y = params.f32_or("center_y", 0.0);
        let seed = params.i32_or("seed", 0) as u32;

        let n = count.max(0) as usize;
        let mut positions = Vec::with_capacity(n);
        let mut rotations = Vec::with_capacity(n);

        let half_x = area_x / 2.0;
        let half_y = area_y / 2.0;

        for i in 0..n {
            let h = hash(seed, i as u32);
            let rx = hash_to_f32(h);
            let ry = hash_to_f32(hash(h, 1));
            let rr = hash_to_f32(hash(h, 2));

            positions.push(Vec2(
                center_x + (rx * 2.0 - 1.0) * half_x,
                center_y + (ry * 2.0 - 1.0) * half_y,
            ));
            rotations.push(rr * 2.0 * PI);
        }

        let mut geo = Geometry::new();
        if let Some(src) = source {
            geo.set_instance_source(Some(Arc::new(src.clone())));
        }
        populate_instances(&mut geo, positions, rotations);
        Ok(Arc::new(geo))
    }
}

/// Deterministic hash (Wang hash variant).
fn hash(seed: u32, index: u32) -> u32 {
    let mut h = seed.wrapping_mul(0x9E37_79B9).wrapping_add(index);
    h = (h ^ (h >> 16)).wrapping_mul(0x045D_9F3B);
    h = (h ^ (h >> 16)).wrapping_mul(0x045D_9F3B);
    h ^ (h >> 16)
}

fn hash_to_f32(h: u32) -> f32 {
    (h & 0x00FF_FFFF) as f32 / 0x0100_0000 as f32
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (100, 100))
    }

    /// Emits a fixed value; stands in for upstream nodes in evaluator tests.
    struct StubSource(Arc<dyn NodeData>);

    impl NodeProcessor for StubSource {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(self.0.clone())
        }
    }

    /// Evaluate `node` with `proc` in a fresh evaluator, wiring each value in
    /// `inputs` to the input slot of the same index via a stub source.
    fn run(node: &Node, proc: Arc<dyn NodeProcessor>, inputs: &[Arc<dyn NodeData>]) -> Geometry {
        let mut graph = Graph::new().add_node(node.clone()).unwrap();
        let mut ev = Evaluator::new();
        ev.register(node.id, proc);
        for (i, value) in inputs.iter().enumerate() {
            let src_id = NodeId::new(100 + i as u64);
            graph = graph
                .add_node(Node::new(src_id, "test.source").with_output("out", value.data_type_id()))
                .unwrap()
                .add_edge(
                    EdgeId::new(i as u64 + 1),
                    src_id,
                    OutputPortIndex(0),
                    node.id,
                    InputPortIndex(i as u32),
                )
                .unwrap();
            ev.register(src_id, Arc::new(StubSource(value.clone())));
        }
        let out = ev.evaluate(&graph, node.id, &ctx()).unwrap();
        out.downcast_ref::<Geometry>().unwrap().clone()
    }

    fn make_node(type_key: &str, params: &[(&str, ParameterValue)]) -> Node {
        let mut node = Node::new(NodeId::new(1), type_key)
            .with_input("source", &[DataTypeId::GEOMETRY])
            .with_input("instance_source", &[DataTypeId::GEOMETRY]);
        for (key, value) in params {
            node = node.with_param(*key, value.clone());
        }
        node
    }

    fn small_square() -> Geometry {
        let mut geo = Geometry::from_points(vec![
            Vec2(-5.0, -5.0),
            Vec2(5.0, -5.0),
            Vec2(5.0, 5.0),
            Vec2(-5.0, 5.0),
        ]);
        geo.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        geo
    }

    fn line_path() -> Geometry {
        let mut geo = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(100.0, 0.0)]);
        geo.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        geo
    }

    fn arc_geo(geo: Geometry) -> Arc<dyn NodeData> {
        Arc::new(geo)
    }

    // -- Grid ---------------------------------------------------------------

    #[test]
    fn grid_instance_count() {
        let node = make_node(
            "scatter.grid",
            &[
                ("count_x", ParameterValue::Int(3)),
                ("count_y", ParameterValue::Int(4)),
            ],
        );
        let geo = run(
            &node,
            Arc::new(GridProcessor::from_node(&node)),
            &[arc_geo(small_square())],
        );

        assert_eq!(geo.instance_count(), 12);
        assert!(geo.instance_source().is_some());
        assert!(geo.instances().get(names::INDEX).is_some());
        assert!(geo.instances().get(names::P).is_some());
        assert!(geo.instances().get(names::ROT).is_some());
        assert!(geo.instances().get(names::SCALE).is_some());
    }

    #[test]
    fn grid_no_source_still_creates_instances() {
        let node = make_node(
            "scatter.grid",
            &[
                ("count_x", ParameterValue::Int(2)),
                ("count_y", ParameterValue::Int(2)),
            ],
        );
        let geo = run(&node, Arc::new(GridProcessor::from_node(&node)), &[]);

        assert_eq!(geo.instance_count(), 4);
        assert!(geo.instance_source().is_none());
    }

    // -- Circular -----------------------------------------------------------

    #[test]
    fn circular_instance_count_and_rotation() {
        let node = make_node(
            "scatter.circular",
            &[
                ("count", ParameterValue::Int(6)),
                ("radius", ParameterValue::Float(50.0)),
                ("align_rotation", ParameterValue::Bool(true)),
            ],
        );
        let geo = run(
            &node,
            Arc::new(CircularProcessor::from_node(&node)),
            &[arc_geo(small_square())],
        );

        assert_eq!(geo.instance_count(), 6);

        let rots = geo
            .instances()
            .get(names::ROT)
            .unwrap()
            .as_f32(names::ROT)
            .unwrap();
        assert!((rots[0] - 0.0).abs() < 1e-5);
        assert!((rots[1] - PI / 3.0).abs() < 1e-4);
    }

    // -- Path array ---------------------------------------------------------

    #[test]
    fn path_array_distributes_along_path() {
        let node = make_node("scatter.path_array", &[("count", ParameterValue::Int(5))]);
        let geo = run(
            &node,
            Arc::new(PathArrayProcessor::from_node(&node)),
            &[arc_geo(line_path()), arc_geo(small_square())],
        );

        assert_eq!(geo.instance_count(), 5);

        let positions = geo
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap();
        // First at start, last at end of the 100px line
        assert!((positions[0].0 - 0.0).abs() < 1e-4);
        assert!((positions[4].0 - 100.0).abs() < 1e-4);

        let rots = geo
            .instances()
            .get(names::ROT)
            .unwrap()
            .as_f32(names::ROT)
            .unwrap();
        // Horizontal path → rot ≈ 0
        for r in rots {
            assert!(r.abs() < 1e-4, "rotation along horizontal path: {r}");
        }
    }

    #[test]
    fn path_array_tangent_rotation() {
        let node = make_node("scatter.path_array", &[("count", ParameterValue::Int(3))]);

        // Vertical path: rotation should be PI/2
        let mut path = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(0.0, 100.0)]);
        path.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });

        let geo = run(
            &node,
            Arc::new(PathArrayProcessor::from_node(&node)),
            &[arc_geo(path)],
        );

        let rots = geo
            .instances()
            .get(names::ROT)
            .unwrap()
            .as_f32(names::ROT)
            .unwrap();
        for r in rots {
            assert!((r - PI / 2.0).abs() < 1e-4, "vertical path tangent: {r}");
        }
    }

    #[test]
    fn path_array_closed_path_includes_closing_segment() {
        // Closed square, perimeter 40. count=5 → arc lengths 0,10,20,30,40:
        // corners plus the closing segment back to the start.
        let mut path = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(10.0, 0.0),
            Vec2(10.0, 10.0),
            Vec2(0.0, 10.0),
        ]);
        path.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });

        let node = make_node("scatter.path_array", &[("count", ParameterValue::Int(5))]);
        let geo = run(
            &node,
            Arc::new(PathArrayProcessor::from_node(&node)),
            &[arc_geo(path)],
        );

        let positions = geo
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap();
        assert_eq!(positions.len(), 5);
        assert!((positions[3].0 - 0.0).abs() < 1e-4 && (positions[3].1 - 10.0).abs() < 1e-4);
        // Last sample walks the closing segment back to the start point.
        assert!(
            positions[4].0.abs() < 1e-4 && positions[4].1.abs() < 1e-4,
            "closing segment sampled: {:?}",
            positions[4]
        );
    }

    #[test]
    fn path_array_multiple_primitives_do_not_join() {
        // Two disjoint horizontal lines. Samples stay on the primitives and
        // never interpolate across the gap between them.
        let mut path = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(10.0, 0.0),
            Vec2(0.0, 20.0),
            Vec2(10.0, 20.0),
        ]);
        path.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        path.push_primitive(Primitive::Path {
            verts: 2..4,
            closed: false,
        });

        let node = make_node("scatter.path_array", &[("count", ParameterValue::Int(9))]);
        let geo = run(
            &node,
            Arc::new(PathArrayProcessor::from_node(&node)),
            &[arc_geo(path)],
        );

        let positions = geo
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap();
        for p in positions {
            assert!(
                p.1.abs() < 1e-4 || (p.1 - 20.0).abs() < 1e-4,
                "sample must lie on one of the two lines: {p:?}"
            );
        }
    }

    #[test]
    fn path_array_ignores_points_outside_primitives() {
        // P column has a far-away stray point not referenced by the primitive.
        let mut path =
            Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0), Vec2(1000.0, 1000.0)]);
        path.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });

        let node = make_node("scatter.path_array", &[("count", ParameterValue::Int(3))]);
        let geo = run(
            &node,
            Arc::new(PathArrayProcessor::from_node(&node)),
            &[arc_geo(path)],
        );

        let positions = geo
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap();
        for p in positions {
            assert!(
                p.0 <= 10.0 + 1e-4 && p.1.abs() < 1e-4,
                "stray point ignored: {p:?}"
            );
        }
    }

    // -- Scatter ------------------------------------------------------------

    #[test]
    fn scatter_deterministic_with_same_seed() {
        let node = make_node(
            "scatter.scatter",
            &[
                ("count", ParameterValue::Int(10)),
                ("seed", ParameterValue::Int(42)),
            ],
        );

        let g1 = run(&node, Arc::new(ScatterProcessor::from_node(&node)), &[]);
        let g2 = run(&node, Arc::new(ScatterProcessor::from_node(&node)), &[]);

        let p1 = g1.instances().get(names::P).unwrap();
        let p2 = g2.instances().get(names::P).unwrap();
        assert_eq!(p1.as_vec2(names::P).unwrap(), p2.as_vec2(names::P).unwrap());
    }

    #[test]
    fn scatter_different_seed_produces_different_positions() {
        let node_a = make_node(
            "scatter.scatter",
            &[
                ("count", ParameterValue::Int(10)),
                ("seed", ParameterValue::Int(1)),
            ],
        );
        let node_b = make_node(
            "scatter.scatter",
            &[
                ("count", ParameterValue::Int(10)),
                ("seed", ParameterValue::Int(2)),
            ],
        );

        let ga = run(&node_a, Arc::new(ScatterProcessor::from_node(&node_a)), &[]);
        let gb = run(&node_b, Arc::new(ScatterProcessor::from_node(&node_b)), &[]);

        let pa = ga
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap();
        let pb = gb
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap();
        assert_ne!(pa, pb);
    }

    #[test]
    fn scatter_instance_count() {
        let node = make_node("scatter.scatter", &[("count", ParameterValue::Int(15))]);
        let geo = run(
            &node,
            Arc::new(ScatterProcessor::from_node(&node)),
            &[arc_geo(small_square())],
        );

        assert_eq!(geo.instance_count(), 15);
        assert!(geo.instance_source().is_some());
    }

    // -- All scatter nodes have required attributes -------------------------

    #[test]
    fn all_scatter_nodes_populate_required_attrs() {
        type Case = (
            &'static str,
            Node,
            Arc<dyn NodeProcessor>,
            Vec<Arc<dyn NodeData>>,
        );
        let cases: Vec<Case> = vec![
            (
                "grid",
                make_node("scatter.grid", &[]),
                Arc::new(GridProcessor),
                vec![arc_geo(small_square())],
            ),
            (
                "circular",
                make_node("scatter.circular", &[]),
                Arc::new(CircularProcessor),
                vec![arc_geo(small_square())],
            ),
            (
                "path_array",
                make_node("scatter.path_array", &[]),
                Arc::new(PathArrayProcessor),
                vec![arc_geo(line_path()), arc_geo(small_square())],
            ),
            (
                "scatter",
                make_node("scatter.scatter", &[]),
                Arc::new(ScatterProcessor),
                vec![arc_geo(small_square())],
            ),
        ];

        for (name, node, proc, inputs) in &cases {
            let geo = run(node, proc.clone(), inputs);
            assert!(
                geo.instances().get(names::INDEX).is_some(),
                "{name} missing index"
            );
            assert!(geo.instances().get(names::P).is_some(), "{name} missing P");
            assert!(
                geo.instances().get(names::ROT).is_some(),
                "{name} missing rot"
            );
            assert!(
                geo.instances().get(names::SCALE).is_some(),
                "{name} missing scale"
            );
        }
    }
}
