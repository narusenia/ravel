// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `geometry.iterate` — run a body network once per piece (REQ-CORE-013).
//!
//! The input geometry is split by the integer primitive attribute named by
//! the `attribute` parameter (default `piece`), one piece per distinct value
//! in **ascending value order**. The body is the node's own inner graph — a
//! `net.in` / `net.out` pair exactly as a subnet has, so the node editor dives
//! into it unchanged — and is pulled once per piece through
//! [`EvalScope::evaluate_sub`] under [`PathSegment::Iteration`]. That puts each
//! iteration's nodes under their own cache key, so the evaluator's cache,
//! dirty and invalidation paths apply as they do for a subnet.
//!
//! The body's `net.in` offers two bindings: `geometry` (this piece) and
//! `index` (the zero-based iteration number, a `Scalar`). Its `net.out` answers
//! with a `geometry` port. The per-piece results are merged with
//! `geometry.merge`'s rule ([`merge_pair`]).
//!
//! Iteration is one level deep and has a ceiling: a body that reaches another
//! iteration, or more pieces than `max_iterations`, is an evaluation error —
//! never a truncation.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, PathSegment, ResolvedParams};
use ravel_core::geometry::{Geometry, names, ops};
use ravel_core::graph::Node;
use ravel_core::network as net;
use ravel_core::registry::builtin::MAX_ITERATIONS_DEFAULT;
use ravel_core::types::{NodeData, PortRecord, Scalar};
use std::sync::Arc;

use crate::geometry::merge_pair;

pub struct IterateProcessor;

impl IterateProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for IterateProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        if scope
            .path()
            .iter()
            .any(|segment| matches!(segment, PathSegment::Iteration(..)))
        {
            anyhow::bail!("geometry.iterate: iteration cannot be nested");
        }
        let inner = node
            .subnet
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("geometry.iterate: node {} has no body", node.id))?
            .clone();
        let out_node = net::find_out_node(&inner)
            .ok_or_else(|| anyhow::anyhow!("geometry.iterate: body has no net.out node"))?
            .clone();
        let out_port = out_node
            .inputs
            .iter()
            .position(|port| port.name == net::ITERATE_PORT_GEOMETRY)
            .unwrap_or(0);

        let empty = Geometry::new();
        let input = inputs.first().and_then(Option::as_ref);
        let source = match input {
            Some(value) => value
                .downcast_ref::<Geometry>()
                .ok_or_else(|| anyhow::anyhow!("geometry.iterate: input is not a geometry"))?,
            None => &empty,
        };
        let attribute = params.str_or("attribute", names::PIECE);
        let pieces = ops::split_by_piece(source, attribute)
            .map_err(|error| anyhow::anyhow!("geometry.iterate: {error}"))?;
        let max = params
            .i32_or("max_iterations", MAX_ITERATIONS_DEFAULT)
            .max(0) as usize;
        if pieces.len() > max {
            anyhow::bail!(
                "geometry.iterate: {} pieces exceed max_iterations ({max})",
                pieces.len()
            );
        }

        let mut results: Vec<Arc<dyn NodeData>> = Vec::with_capacity(pieces.len());
        for (index, piece) in pieces.into_iter().enumerate() {
            let bindings = vec![
                (
                    net::ITERATE_PORT_GEOMETRY.to_owned(),
                    Arc::new(piece) as Arc<dyn NodeData>,
                ),
                (
                    net::ITERATE_PORT_INDEX.to_owned(),
                    Arc::new(Scalar(index as f32)) as Arc<dyn NodeData>,
                ),
            ];
            let record = scope.evaluate_sub(
                PathSegment::Iteration(node.id, index as u32),
                &inner,
                out_node.id,
                ctx,
                bindings,
            )?;
            let result = record
                .downcast_ref::<PortRecord>()
                .and_then(|record| record.0.get(out_port))
                .ok_or_else(|| anyhow::anyhow!("geometry.iterate: body net.out has no result"))?;
            if result.downcast_ref::<Geometry>().is_none() {
                anyhow::bail!("geometry.iterate: the body's result is not a geometry");
            }
            results.push(result.clone());
        }
        merge_all(results)
    }

    fn is_time_dependent(&self) -> bool {
        // The body may be time-driven; its nodes keep their own scoped caches.
        true
    }
}

/// Merges `results` in order with a balanced pairwise fold: the same result as
/// folding left to right, without re-copying the growing accumulator each step.
fn merge_all(mut results: Vec<Arc<dyn NodeData>>) -> anyhow::Result<Arc<dyn NodeData>> {
    if results.is_empty() {
        return Ok(Arc::new(Geometry::new()));
    }
    while results.len() > 1 {
        let mut next = Vec::with_capacity(results.len().div_ceil(2));
        let mut pairs = results.chunks(2);
        for pair in &mut pairs {
            next.push(match pair {
                [a, b] => merge_pair(Some(a), Some(b))?,
                [only] => only.clone(),
                _ => unreachable!("chunks(2) yields one or two"),
            });
        }
        results = next;
    }
    Ok(results.pop().expect("one result left"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{NetInProcessor, NetOutProcessor};
    use ravel_core::eval::Evaluator;
    use ravel_core::geometry::{AttributeArray, Primitive};
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::{FrameRate, Vec2};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SOURCE: u64 = 1;
    const ITERATE: u64 = 3;
    const IN: u64 = 10;
    const OUT: u64 = 11;
    const TAG: u64 = 12;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (64, 64))
    }

    /// Four two-point paths with `piece` = [2, 0, 2, 1]: pieces 0, 1, 2 hold
    /// 1, 1 and 2 primitives. The first point's x is the primitive's index, so
    /// the result shows which primitives landed where.
    fn pieces_geometry(pieces: &[i32]) -> Geometry {
        let mut points = Vec::new();
        for i in 0..pieces.len() {
            points.push(Vec2(i as f32, 0.0));
            points.push(Vec2(i as f32, 1.0));
        }
        let mut geometry = Geometry::from_points(points);
        for i in 0..pieces.len() {
            geometry.push_primitive(Primitive::Path {
                verts: (2 * i)..(2 * i + 2),
                closed: false,
            });
        }
        geometry
            .primitive_attrs_mut()
            .insert(names::PIECE, AttributeArray::I32(pieces.to_vec()))
            .unwrap();
        geometry
    }

    struct Source(Arc<Geometry>);
    impl NodeProcessor for Source {
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

    /// Body node: stamps every point with a `tag` column holding the
    /// iteration index it was handed, and counts how often it runs.
    struct Tag(Arc<AtomicUsize>);
    impl NodeProcessor for Tag {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.0.fetch_add(1, Ordering::Relaxed);
            let geometry = inputs[0]
                .as_ref()
                .and_then(|value| value.downcast_ref::<Geometry>())
                .expect("a geometry piece");
            let index = inputs[1]
                .as_ref()
                .and_then(|value| value.downcast_ref::<Scalar>())
                .expect("the iteration index")
                .0;
            let mut out = geometry.clone();
            let count = out.point_count();
            out.points_mut()
                .insert("tag", AttributeArray::F32(vec![index; count]))
                .unwrap();
            Ok(Arc::new(out))
        }
    }

    /// `net.in(geometry, index) → tag → net.out(geometry)`.
    fn tagging_body() -> Graph {
        let tag = Node::new(NodeId::new(TAG), "test.tag")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_input("index", &[DataTypeId::SCALAR])
            .with_output("geometry", DataTypeId::GEOMETRY);
        body_through(tag)
    }

    fn body_through(middle: Node) -> Graph {
        let id = middle.id;
        let inner = net::new_iterate_inner_graph(NodeId::new(IN), NodeId::new(OUT));
        // Rewire In → middle → Out in place of the seeded straight edge.
        let straight = inner.edges().next().expect("the seeded edge").id;
        let edge = |n: u64, from: NodeId, from_port: u32, to: NodeId, to_port: u32| {
            (
                EdgeId::new(n),
                from,
                OutputPortIndex(from_port),
                to,
                InputPortIndex(to_port),
            )
        };
        let mut graph = inner
            .remove_edge(straight)
            .unwrap()
            .add_node(middle)
            .unwrap();
        for (e, from, fp, to, tp) in [
            edge(901, NodeId::new(IN), 0, id, 0),
            edge(902, NodeId::new(IN), 1, id, 1),
            edge(903, id, 0, NodeId::new(OUT), 0),
        ] {
            graph = graph.add_edge(e, from, fp, to, tp).unwrap();
        }
        graph
    }

    fn iterate_node(body: Graph, max: i32) -> Node {
        Node::new(NodeId::new(ITERATE), net::ITERATE_TYPE_KEY)
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_output("geometry", DataTypeId::GEOMETRY)
            .with_param("attribute", ParameterValue::String("piece".into()))
            .with_param("max_iterations", ParameterValue::Int(max))
            .with_subnet(body)
    }

    fn setup(geometry: Geometry, body: Graph, max: i32) -> (Evaluator, Graph, Arc<AtomicUsize>) {
        let source = Node::new(NodeId::new(SOURCE), "test.source")
            .with_output("geometry", DataTypeId::GEOMETRY);
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(iterate_node(body.clone(), max))
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(SOURCE),
                OutputPortIndex(0),
                NodeId::new(ITERATE),
                InputPortIndex(0),
            )
            .unwrap();
        let tags = Arc::new(AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(SOURCE), Arc::new(Source(Arc::new(geometry))));
        ev.register(NodeId::new(ITERATE), Arc::new(IterateProcessor));
        register_body(&mut ev, &body, &tags);
        (ev, graph, tags)
    }

    fn register_body(ev: &mut Evaluator, body: &Graph, tags: &Arc<AtomicUsize>) {
        for node in body.nodes() {
            match node.type_key.as_str() {
                key if key == net::NET_IN_TYPE_KEY => {
                    ev.register(node.id, Arc::new(NetInProcessor::from_node(node)));
                }
                key if key == net::NET_OUT_TYPE_KEY => {
                    ev.register(node.id, Arc::new(NetOutProcessor::from_node(node)));
                }
                net::ITERATE_TYPE_KEY => {
                    ev.register(node.id, Arc::new(IterateProcessor));
                    if let Some(inner) = node.subnet.as_deref() {
                        register_body(ev, inner, tags);
                    }
                }
                "test.tag" => {
                    ev.register(node.id, Arc::new(Tag(tags.clone())));
                }
                _ => {}
            }
        }
    }

    fn tags_of(value: &Arc<dyn NodeData>) -> Vec<f32> {
        value
            .downcast_ref::<Geometry>()
            .unwrap()
            .points()
            .get("tag")
            .unwrap()
            .as_f32("tag")
            .unwrap()
            .to_vec()
    }

    fn evaluate(ev: &mut Evaluator, graph: &Graph) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(ev.evaluate(graph, NodeId::new(ITERATE), &ctx())?)
    }

    /// The full error text (`{:?}` carries the whole source chain) of an
    /// evaluation that must fail.
    fn failure(ev: &mut Evaluator, graph: &Graph) -> String {
        match evaluate(ev, graph) {
            Ok(_) => panic!("the evaluation was expected to fail"),
            Err(error) => format!("{error:?}"),
        }
    }

    /// Three `piece` values run the body three times.
    #[test]
    fn three_piece_values_evaluate_the_body_three_times() {
        let (mut ev, graph, tags) = setup(pieces_geometry(&[2, 0, 2, 1]), tagging_body(), 16);
        evaluate(&mut ev, &graph).unwrap();
        assert_eq!(tags.load(Ordering::Relaxed), 3);
    }

    /// The iteration index reaches the body, so each piece comes out
    /// different — and in ascending piece order, not input order.
    #[test]
    fn each_piece_sees_its_own_iteration_index() {
        let (mut ev, graph, _) = setup(pieces_geometry(&[2, 0, 2, 1]), tagging_body(), 16);
        let out = evaluate(&mut ev, &graph).unwrap();
        // Piece 0 = primitive 1, piece 1 = primitive 3, piece 2 = primitives 0, 2.
        assert_eq!(tags_of(&out), [0., 0., 1., 1., 2., 2., 2., 2.]);
        let xs: Vec<f32> = out
            .downcast_ref::<Geometry>()
            .unwrap()
            .points()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap()
            .iter()
            .map(|p| p.0)
            .collect();
        assert_eq!(xs, [1., 1., 3., 3., 0., 0., 2., 2.]);
    }

    /// The merged element counts are the sums of the pieces', per domain.
    #[test]
    fn the_merged_counts_are_the_sum_of_the_pieces() {
        let source = pieces_geometry(&[2, 0, 2, 1, 1, 1]);
        let pieces = ops::split_by_piece(&source, names::PIECE).unwrap();
        assert_eq!(pieces.len(), 3);
        let (points, prims): (usize, usize) = pieces.iter().fold((0, 0), |(p, q), g| {
            (p + g.point_count(), q + g.primitive_count())
        });

        let (mut ev, graph, _) = setup(source.clone(), tagging_body(), 16);
        let out = evaluate(&mut ev, &graph).unwrap();
        let out = out.downcast_ref::<Geometry>().unwrap();
        assert_eq!((out.point_count(), out.primitive_count()), (points, prims));
        assert_eq!(
            (out.point_count(), out.primitive_count()),
            (source.point_count(), source.primitive_count())
        );
        assert_eq!(out.validate(), Ok(()));
    }

    /// More pieces than `max_iterations` is an error, and exactly the limit is
    /// not: the cap refuses, it does not cut.
    #[test]
    fn exceeding_max_iterations_is_an_error_not_a_truncation() {
        let (mut ev, graph, tags) = setup(pieces_geometry(&[0, 1, 2]), tagging_body(), 2);
        let error = failure(&mut ev, &graph);
        assert!(error.contains("max_iterations"), "{error}");
        assert_eq!(
            tags.load(Ordering::Relaxed),
            0,
            "nothing ran before refusing"
        );

        let (mut ev, graph, _) = setup(pieces_geometry(&[0, 1, 2]), tagging_body(), 3);
        let out = evaluate(&mut ev, &graph).unwrap();
        assert_eq!(
            tags_of(&out).len(),
            6,
            "all three pieces survive at the limit"
        );
    }

    /// A body holding another iteration node is refused.
    #[test]
    fn nesting_iteration_is_an_error() {
        let nested = iterate_node(tagging_body_with_ids(20, 21, 22), 16);
        let nested = Node {
            id: NodeId::new(30),
            ..nested
        }
        .with_input("index", &[DataTypeId::SCALAR]);
        let outer_body = body_through_nested(nested);
        let (mut ev, graph, _) = setup(pieces_geometry(&[0, 1]), outer_body, 16);
        let error = failure(&mut ev, &graph);
        assert!(error.contains("nested"), "{error}");
    }

    fn tagging_body_with_ids(in_id: u64, out_id: u64, tag_id: u64) -> Graph {
        let inner = net::new_iterate_inner_graph(NodeId::new(in_id), NodeId::new(out_id));
        let tag = Node::new(NodeId::new(tag_id), "test.tag")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_input("index", &[DataTypeId::SCALAR])
            .with_output("geometry", DataTypeId::GEOMETRY);
        let straight = inner.edges().next().unwrap().id;
        inner
            .remove_edge(straight)
            .unwrap()
            .add_node(tag)
            .unwrap()
            .add_edge(
                EdgeId::new(in_id * 100 + 1),
                NodeId::new(in_id),
                OutputPortIndex(0),
                NodeId::new(tag_id),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(in_id * 100 + 2),
                NodeId::new(in_id),
                OutputPortIndex(1),
                NodeId::new(tag_id),
                InputPortIndex(1),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(in_id * 100 + 3),
                NodeId::new(tag_id),
                OutputPortIndex(0),
                NodeId::new(out_id),
                InputPortIndex(0),
            )
            .unwrap()
    }

    /// `net.in(geometry) → nested iterate → net.out`.
    fn body_through_nested(nested: Node) -> Graph {
        let nested_id = nested.id;
        let inner = net::new_iterate_inner_graph(NodeId::new(IN), NodeId::new(OUT));
        let straight = inner.edges().next().unwrap().id;
        inner
            .remove_edge(straight)
            .unwrap()
            .add_node(nested)
            .unwrap()
            .add_edge(
                EdgeId::new(911),
                NodeId::new(IN),
                OutputPortIndex(0),
                nested_id,
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(912),
                nested_id,
                OutputPortIndex(0),
                NodeId::new(OUT),
                InputPortIndex(0),
            )
            .unwrap()
    }

    /// Fresh evaluators over the same input give the same geometry, whatever
    /// order a hash map would have yielded the pieces in.
    #[test]
    fn evaluating_the_same_input_twice_gives_the_same_result() {
        let pieces = [5, 3, 9, 1, 7, 3, 5, 9, 0, 2];
        let run = || {
            let (mut ev, graph, _) = setup(pieces_geometry(&pieces), tagging_body(), 16);
            let out = evaluate(&mut ev, &graph).unwrap();
            let geometry = out.downcast_ref::<Geometry>().unwrap().clone();
            (
                geometry
                    .points()
                    .get(names::P)
                    .unwrap()
                    .as_vec2(names::P)
                    .unwrap()
                    .to_vec(),
                tags_of(&out),
                geometry
                    .primitive_attrs()
                    .get(names::PIECE)
                    .unwrap()
                    .as_i32(names::PIECE)
                    .unwrap()
                    .to_vec(),
                geometry.primitives().to_vec(),
            )
        };
        let first = run();
        assert_eq!(
            first.2,
            [0, 1, 2, 3, 3, 5, 5, 7, 9, 9],
            "ascending piece order"
        );
        for _ in 0..16 {
            assert_eq!(run(), first);
        }
    }

    /// An edit upstream reaches the new result: the body reruns on the new
    /// pieces, and a piece count that shrinks leaves no stale iteration.
    #[test]
    fn an_upstream_edit_reruns_every_iteration() {
        let (mut ev, graph, tags) = setup(pieces_geometry(&[0, 1, 2]), tagging_body(), 16);
        evaluate(&mut ev, &graph).unwrap();
        assert_eq!(tags.load(Ordering::Relaxed), 3);

        ev.register(
            NodeId::new(SOURCE),
            Arc::new(Source(Arc::new(pieces_geometry(&[0, 0, 1])))),
        );
        ev.mark_dirty(&graph, NodeId::new(SOURCE));
        let out = evaluate(&mut ev, &graph).unwrap();
        assert_eq!(tags.load(Ordering::Relaxed), 5, "two pieces reran");
        assert_eq!(tags_of(&out), [0., 0., 0., 0., 1., 1.]);
    }

    /// No pieces, no body runs, an empty geometry comes out.
    #[test]
    fn a_geometry_without_primitives_iterates_zero_times() {
        let (mut ev, graph, tags) = setup(Geometry::new(), tagging_body(), 16);
        let out = evaluate(&mut ev, &graph).unwrap();
        assert_eq!(tags.load(Ordering::Relaxed), 0);
        assert_eq!(out.downcast_ref::<Geometry>().unwrap().point_count(), 0);
    }

    /// Primitives without the split attribute are an error naming it, not an
    /// implicit single piece.
    #[test]
    fn a_missing_piece_attribute_is_an_error() {
        let mut geometry = pieces_geometry(&[0, 1]);
        geometry.primitive_attrs_mut().remove(names::PIECE);
        let (mut ev, graph, _) = setup(geometry, tagging_body(), 16);
        let error = failure(&mut ev, &graph);
        assert!(error.contains("piece"), "{error}");
    }

    /// A template-made node carries a working body: an empty body returns the
    /// pieces unchanged, so the merge reproduces the input.
    #[test]
    fn a_registry_node_has_a_passthrough_body() {
        // Far from the counter the seeded body ids come from: ids are unique
        // across nested graphs, so a clash would silently swap processors.
        let (source_id, iterate_id) = (NodeId::new(900_001), NodeId::new(900_003));
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        let node = registry
            .create_node(net::ITERATE_TYPE_KEY, iterate_id)
            .expect("the iterate template is registered");
        let body = node.subnet.clone().expect("a seeded body");
        let source = pieces_geometry(&[1, 0, 1]);
        let source_node =
            Node::new(source_id, "test.source").with_output("geometry", DataTypeId::GEOMETRY);
        let graph = Graph::new()
            .add_node(source_node)
            .unwrap()
            .add_node(node)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                source_id,
                OutputPortIndex(0),
                iterate_id,
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(source_id, Arc::new(Source(Arc::new(source.clone()))));
        ev.register(iterate_id, Arc::new(IterateProcessor));
        register_body(&mut ev, &body, &Arc::new(AtomicUsize::new(0)));

        let out = ev.evaluate(&graph, iterate_id, &ctx()).unwrap();
        let out = out.downcast_ref::<Geometry>().unwrap();
        assert_eq!(out.point_count(), source.point_count());
        assert_eq!(out.primitive_count(), source.primitive_count());
    }

    /// One cold evaluation of `geometry.iterate` at 10 / 100 / 1000 pieces,
    /// which is what picks `max_iterations`' default
    /// (`docs/implementation/perf-baseline.md`).
    ///
    ///     cargo test --release -p ravel-nodes --lib iterate::tests::measure \
    ///         -- --ignored --nocapture
    #[test]
    #[ignore = "measurement harness; run with --ignored --nocapture"]
    fn measure_iterate_cost() {
        for count in [10usize, 100, 1000] {
            let pieces: Vec<i32> = (0..count as i32).collect();
            let mut samples = Vec::new();
            for _ in 0..9 {
                let (mut ev, graph, _) =
                    setup(pieces_geometry(&pieces), tagging_body(), count as i32);
                let start = std::time::Instant::now();
                let out = evaluate(&mut ev, &graph).unwrap();
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(tags_of(&out).len(), 2 * count);
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "iterate pieces={count:5} median={:.3} ms min={:.3} ms max={:.3} ms",
                samples[4], samples[0], samples[8]
            );
        }
    }
}
