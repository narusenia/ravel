// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `scatter.points`: copy-to-points. One instance of the source geometry on
//! every point of the `points` input.
//!
//! The placement of an instance is read off its point with the 2D rule the
//! other `scatter.*` nodes write:
//!
//! * `P` is the instance's `P`;
//! * `rot` (F32, radians) is the instance's `rot`, 0 when the point has none;
//! * `scale` (Vec2) is the instance's `scale`, `(1, 1)` when the point has
//!   none, and **`pscale` multiplies it as a uniform factor** when the point
//!   has one — so a point drawn as a sprite of radius `pscale` stamps a source
//!   `pscale` times its own size. A point with no `pscale` leaves the source
//!   at its own size rather than inheriting the sprite default of 2.
//!
//! Every other point attribute (`Cd`, `alpha`, `id`, `age`, `velocity`, user
//! columns) is carried onto the instance domain, where fields and styles read
//! it like any instance column; `index` is the instance's own `0..n`.
//!
//! A `Vec3` `P` is an explicit error (3D placement is `InstanceColumns3`'s,
//! not this node's yet), and so are more than [`MAX_POINT_INSTANCES`] points —
//! a cap with an error, not a silent truncation.

use std::sync::Arc;

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{AttributeArray, Domain, Geometry, names};
use ravel_core::graph::Node;
use ravel_core::types::{NodeData, Vec2};

use super::{attach_instance_sources, populate_instances, source_inputs};

/// Most instances one `scatter.points` makes. `particle.simulate` stops at
/// 100 000 by default; ten times that is headroom, and the rasterizer is the
/// limit past it.
pub const MAX_POINT_INSTANCES: usize = 1_000_000;

/// Point attributes the instance writes itself, which are not carried over.
const PLACEMENT: [&str; 5] = [
    names::P,
    names::INDEX,
    names::ROT,
    names::SCALE,
    names::SOURCE_INDEX,
];

pub struct PointsProcessor;

impl PointsProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for PointsProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let target = inputs
            .first()
            .and_then(|input| input.as_ref())
            .and_then(|input| input.downcast_ref::<Geometry>())
            .context("scatter.points expects a point Geometry on input 0")?;
        let sources = source_inputs(inputs, 1);

        let mut geometry = Geometry::new();
        let Some(positions) = target.positions(Domain::Point) else {
            return Ok(Arc::new(geometry));
        };
        let positions = positions?.require_planar("scatter.points")?;
        let count = positions.len();
        anyhow::ensure!(
            count <= MAX_POINT_INSTANCES,
            "scatter.points: {count} points exceed the {MAX_POINT_INSTANCES} instance limit"
        );

        let column = |name: &str| target.points().get(name).filter(|c| c.len() == count);
        let rotations = match column(names::ROT) {
            Some(rot) => rot.as_f32(names::ROT)?.to_vec(),
            None => vec![0.0; count],
        };
        let mut scales = match column(names::SCALE) {
            Some(scale) => scale.as_vec2(names::SCALE)?.to_vec(),
            None => vec![Vec2(1.0, 1.0); count],
        };
        if let Some(pscale) = column(names::PSCALE) {
            for (scale, size) in scales.iter_mut().zip(pscale.as_f32(names::PSCALE)?) {
                *scale = Vec2(scale.0 * size, scale.1 * size);
            }
        }

        populate_instances(&mut geometry, positions.to_vec(), rotations);
        geometry
            .instances_mut()
            .insert(names::SCALE, AttributeArray::Vec2(scales))?;
        for (name, column) in target.points().iter() {
            if !PLACEMENT.contains(&name.as_str()) {
                geometry
                    .instances_mut()
                    .insert(name.clone(), (**column).clone())?;
            }
        }
        attach_instance_sources(&mut geometry, &sources, params)?;
        Ok(Arc::new(geometry))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::registry::NodeRegistry;
    use ravel_core::registry::builtin::register_builtins;
    use ravel_core::types::{Color, FrameRate};

    struct Stub(Arc<dyn NodeData>);

    impl NodeProcessor for Stub {
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

    /// `scatter.points` from the registry, with `points` in slot 0 and each of
    /// `sources` in the variadic group after it.
    fn run(
        points: Geometry,
        sources: Vec<Geometry>,
        params: &[(&str, ParameterValue)],
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        let mut node = registry
            .create_node("scatter.points", NodeId::new(1))
            .unwrap();
        for (key, value) in params {
            node.parameters
                .iter_mut()
                .find(|p| p.key == *key)
                .unwrap()
                .value = value.clone();
        }
        let mut graph = Graph::new().add_node(node.clone()).unwrap();
        let mut ev = Evaluator::new();
        ev.register(node.id, Arc::new(PointsProcessor));
        for (slot, value) in std::iter::once(points)
            .chain(sources)
            .map(|g| Arc::new(g) as Arc<dyn NodeData>)
            .enumerate()
        {
            let id = NodeId::new(100 + slot as u64);
            graph = graph
                .add_node(
                    ravel_core::graph::Node::new(id, "test.source")
                        .with_output("out", DataTypeId::GEOMETRY),
                )
                .unwrap()
                .add_edge(
                    EdgeId::new(slot as u64 + 1),
                    id,
                    OutputPortIndex(0),
                    node.id,
                    InputPortIndex(slot as u32),
                )
                .unwrap();
            if slot >= 1 {
                graph = graph.grow_variadic_input_group(node.id).unwrap();
            }
            ev.register(id, Arc::new(Stub(value)));
        }
        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (100, 100));
        Ok(ev.evaluate(&graph, node.id, &ctx)?)
    }

    fn marker() -> Geometry {
        Geometry::from_points(vec![Vec2(1.0, 0.0)])
    }

    fn instances(out: &Arc<dyn NodeData>) -> &Geometry {
        out.downcast_ref::<Geometry>().unwrap()
    }

    #[test]
    fn one_instance_per_point_takes_its_placement_and_carries_the_rest() {
        let mut points = Geometry::from_points(vec![Vec2(10.0, 20.0), Vec2(-5.0, 7.0)]);
        let set = points.points_mut();
        set.insert(names::ROT, AttributeArray::F32(vec![0.5, 1.5]))
            .unwrap();
        set.insert(names::PSCALE, AttributeArray::F32(vec![2.0, 4.0]))
            .unwrap();
        set.insert(names::ID, AttributeArray::I32(vec![7, 9]))
            .unwrap();
        set.insert(
            names::CD,
            AttributeArray::Color(vec![
                Color::new(1.0, 0.0, 0.0, 1.0),
                Color::new(0.0, 1.0, 0.0, 1.0),
            ]),
        )
        .unwrap();
        let out = run(
            points,
            vec![marker()],
            &[("center_input", ParameterValue::Bool(false))],
        )
        .unwrap();
        let geometry = instances(&out);

        assert_eq!(geometry.instance_count(), 2);
        let at = |name: &str| geometry.instances().get(name).unwrap().clone();
        assert_eq!(
            *at(names::P),
            AttributeArray::Vec2(vec![Vec2(10.0, 20.0), Vec2(-5.0, 7.0)])
        );
        assert_eq!(*at(names::ROT), AttributeArray::F32(vec![0.5, 1.5]));
        // `pscale` is the uniform scale of the stamped source.
        assert_eq!(
            *at(names::SCALE),
            AttributeArray::Vec2(vec![Vec2(2.0, 2.0), Vec2(4.0, 4.0)])
        );
        assert_eq!(*at(names::INDEX), AttributeArray::I32(vec![0, 1]));
        // The rest rides along.
        assert_eq!(*at(names::ID), AttributeArray::I32(vec![7, 9]));
        assert_eq!(
            *at(names::CD),
            AttributeArray::Color(vec![
                Color::new(1.0, 0.0, 0.0, 1.0),
                Color::new(0.0, 1.0, 0.0, 1.0)
            ])
        );
        assert!(geometry.instance_source().is_some());
    }

    #[test]
    fn points_without_placement_columns_stamp_at_rest_size() {
        let out = run(
            Geometry::from_points(vec![Vec2(3.0, 4.0)]),
            vec![marker()],
            &[],
        )
        .unwrap();
        let geometry = instances(&out);
        let scale = geometry.instances().get(names::SCALE).unwrap();
        assert_eq!(**scale, AttributeArray::Vec2(vec![Vec2(1.0, 1.0)]));
        let rot = geometry.instances().get(names::ROT).unwrap();
        assert_eq!(**rot, AttributeArray::F32(vec![0.0]));
    }

    #[test]
    fn a_three_dimensional_target_is_an_explicit_error() {
        let points = Geometry::from_points3(vec![ravel_core::types::Vec3(1.0, 2.0, 3.0)]);
        let error = run(points, vec![marker()], &[]).err().expect("must fail");
        assert!(format!("{error:?}").contains("scatter.points"), "{error:?}");
    }

    #[test]
    fn an_empty_target_makes_no_instances() {
        for points in [Geometry::new(), Geometry::from_points(Vec::new())] {
            let out = run(points, vec![marker()], &[]).unwrap();
            assert_eq!(instances(&out).instance_count(), 0);
        }
    }

    #[test]
    fn more_points_than_the_limit_is_an_error_not_a_truncation() {
        let points = Geometry::from_points(vec![Vec2(0.0, 0.0); MAX_POINT_INSTANCES + 1]);
        let error = run(points, vec![marker()], &[]).err().expect("must fail");
        assert!(format!("{error:?}").contains("instance limit"), "{error:?}");
    }

    #[test]
    fn several_sources_are_dealt_out_over_the_points() {
        let points = Geometry::from_points(vec![Vec2(0.0, 0.0); 4]);
        let out = run(points, vec![marker(), marker()], &[]).unwrap();
        let indices = instances(&out)
            .instances()
            .get(names::SOURCE_INDEX)
            .unwrap()
            .clone();
        assert_eq!(*indices, AttributeArray::I32(vec![0, 1, 0, 1]));
    }
}
