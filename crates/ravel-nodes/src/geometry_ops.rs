// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Geometry operation nodes: `geometry.group_index`, `geometry.repeat`, and
//! the deformers `geometry.bend` / `geometry.twist` / `geometry.taper`.
//!
//! Thin wrappers: the logic lives in `ravel_core::geometry`.

use std::sync::Arc;

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::Geometry;
use ravel_core::geometry::deform::{DeformAxis, DeformSpec, Deformer, deform};
use ravel_core::geometry::distribute::{DistributeAxis, DistributeMode, distribute};
use ravel_core::geometry::index_group::group_index;
use ravel_core::geometry::repeat::{repeat, repeat_step};
use ravel_core::graph::Node;
use ravel_core::types::{NodeData, Vec2};

fn geometry_input<'a>(
    inputs: &'a [Option<Arc<dyn NodeData>>],
    index: usize,
    processor: &str,
) -> anyhow::Result<&'a Geometry> {
    inputs
        .get(index)
        .and_then(|input| input.as_ref())
        .and_then(|input| input.downcast_ref::<Geometry>())
        .with_context(|| format!("{processor}: input {index} is not Geometry"))
}

/// `geometry.group_index`: write a `Bool` group from an index range
/// expression (`"3"`, `"3-7"`, `"3,5,9"`, `"0-20:2"`). Indices past the end
/// of the domain warn and are ignored.
pub struct GeometryGroupIndexProcessor;

impl GeometryGroupIndexProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for GeometryGroupIndexProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "geometry.group_index")?;
        let name = params.str_or("name", "group");
        anyhow::ensure!(!name.is_empty(), "geometry.group_index: `name` is empty");
        Ok(Arc::new(group_index(
            geometry,
            crate::attribute::domain_param(params, "domain", ravel_core::geometry::Domain::Point),
            params.str_or("range", "0"),
            name,
            params.bool_or("invert", false),
        )?))
    }
}

/// `geometry.repeat`: `count` copies of the source, copy `i` placed by the
/// per-copy `translate` / `rotate` (degrees) / `scale` composed `i` times.
/// Copy 0 is the source where it is. Output is an instance geometry shaped like
/// `scatter.*`'s. The transforms act about the source's own origin.
pub struct GeometryRepeatProcessor;

impl GeometryRepeatProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for GeometryRepeatProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        // The source is optional: with none wired the result is the bare
        // instance points, as `scatter.*` gives.
        let source = match inputs.first().and_then(|input| input.as_ref()) {
            None => None,
            Some(_) => Some(Arc::new(
                geometry_input(inputs, 0, "geometry.repeat")?.clone(),
            )),
        };
        let [tx, ty] = params.vec2_or("translate", [10.0, 0.0]);
        let [sx, sy] = params.vec2_or("scale", [1.0, 1.0]);
        let step = repeat_step(Vec2(tx, ty), params.f32_or("rotate", 0.0), Vec2(sx, sy));
        let count = params.i32_or("count", 5).max(0) as usize;
        Ok(Arc::new(repeat(source, count, step)?))
    }
}

/// `geometry.distribute`: `mode` is `min` / `center` / `max` (bounding-box
/// alignment), `centers` (equal centre distance) or `gaps` (equal edge gap);
/// `axis` picks x or y.
pub struct GeometryDistributeProcessor;

impl NodeProcessor for GeometryDistributeProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "geometry.distribute")?;
        let axis = match params.str_or("axis", "x") {
            "y" => DistributeAxis::Y,
            _ => DistributeAxis::X,
        };
        let mode = match params.str_or("mode", "gaps") {
            "min" => DistributeMode::AlignMin,
            "center" => DistributeMode::AlignCenter,
            "max" => DistributeMode::AlignMax,
            "centers" => DistributeMode::SpaceCenters,
            _ => DistributeMode::SpaceGaps,
        };
        Ok(Arc::new(distribute(geometry, axis, mode)?))
    }
}

/// `geometry.bend` / `geometry.twist` / `geometry.taper`: one processor, three
/// deformers, because the axis, range and group handling is the same.
pub struct GeometryDeformProcessor {
    deformer: Deformer,
}

impl GeometryDeformProcessor {
    pub fn bend() -> Self {
        Self {
            deformer: Deformer::Bend,
        }
    }

    pub fn twist() -> Self {
        Self {
            deformer: Deformer::Twist,
        }
    }

    pub fn taper() -> Self {
        Self {
            deformer: Deformer::Taper,
        }
    }
}

impl NodeProcessor for GeometryDeformProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "geometry deformer")?;
        let spec = DeformSpec {
            deformer: self.deformer,
            axis: match params.str_or("axis", "x") {
                "y" => DeformAxis::Y,
                _ => DeformAxis::X,
            },
            start: params.f32_or("start", 0.0),
            end: params.f32_or("end", 100.0),
            amount: params.f32_or("amount", 0.0),
            group: params.str_or("group", ""),
        };
        Ok(Arc::new(deform(geometry, &spec)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::ApplyFieldProcessor;
    use ravel_core::eval::{Evaluator, ResolvedValue};
    use ravel_core::geometry::{AttributeArray, ConstantField, Domain, FieldValue};
    use ravel_core::types::FrameRate;

    pub(super) fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (64, 64))
    }

    pub(super) fn run(
        processor: &dyn NodeProcessor,
        inputs: Vec<Arc<dyn NodeData>>,
        params: &[(&str, ResolvedValue)],
    ) -> anyhow::Result<Geometry> {
        let mut resolved = ResolvedParams::default();
        for (key, value) in params {
            resolved.set(key, value.clone());
        }
        let inputs: Vec<Option<Arc<dyn NodeData>>> = inputs.into_iter().map(Some).collect();
        let node = Node::new(ravel_core::id::NodeId::new(1), "test");
        let out = processor.process(&node, &ctx(), &inputs, &resolved, &mut Evaluator::new())?;
        Ok(out.downcast_ref::<Geometry>().unwrap().clone())
    }

    fn s(value: &str) -> ResolvedValue {
        ResolvedValue::Str(value.into())
    }

    fn points(n: usize) -> Arc<dyn NodeData> {
        Arc::new(Geometry::from_points(
            (0..n).map(|i| Vec2(i as f32, 0.0)).collect(),
        ))
    }

    fn flags(geometry: &Geometry, name: &str) -> Vec<bool> {
        match geometry.points().get(name).unwrap().as_ref() {
            AttributeArray::Bool(v) => v.clone(),
            other => panic!("not Bool: {other:?}"),
        }
    }

    #[test]
    fn group_index_node_reads_its_parameters() {
        let p = GeometryGroupIndexProcessor;
        let out = run(
            &p,
            vec![points(6)],
            &[("range", s("0-4:2")), ("name", s("even"))],
        )
        .unwrap();
        assert_eq!(flags(&out, "even"), [true, false, true, false, true, false]);
        let out = run(
            &p,
            vec![points(4)],
            &[("range", s("1")), ("invert", ResolvedValue::Bool(true))],
        )
        .unwrap();
        assert_eq!(flags(&out, "group"), [true, false, true, true]);
        // An index past the end warns and flags nothing; it does not fail.
        let out = run(&p, vec![points(3)], &[("range", s("40"))]).unwrap();
        assert_eq!(flags(&out, "group"), [false; 3]);
    }

    /// Stands in for the `geometry.transform` integration of the plan:
    /// `geometry.transform` has no `group` parameter yet, so the generated
    /// group is consumed by `field.apply`, which already honours it. Only the
    /// targeted elements move; the others are bit-identical.
    #[test]
    fn group_index_group_confines_field_apply_in_place_of_transform() {
        let geometry = points(5);
        let grouped = run(
            &GeometryGroupIndexProcessor,
            vec![geometry.clone()],
            &[("range", s("1,3")), ("name", s("pick"))],
        )
        .unwrap();
        let moved = run_apply(&grouped, "pick");
        let before = grouped
            .positions(Domain::Point)
            .unwrap()
            .unwrap()
            .projected();
        let after = moved.positions(Domain::Point).unwrap().unwrap().projected();
        for i in 0..5 {
            if i == 1 || i == 3 {
                assert_eq!(after[i], Vec2(before[i].0 + 10.0, before[i].1 + 10.0));
            } else {
                assert_eq!(after[i], before[i], "element {i} must not move");
            }
        }
    }

    #[test]
    fn repeat_node_reads_count_and_the_per_copy_transform() {
        let out = run(
            &GeometryRepeatProcessor,
            vec![points(1)],
            &[
                ("count", ResolvedValue::Int(3)),
                ("translate", ResolvedValue::Vec2([2.0, 0.0])),
            ],
        )
        .unwrap();
        let p = out
            .instances()
            .get("P")
            .unwrap()
            .as_vec2("P")
            .unwrap()
            .to_vec();
        assert_eq!(p, [Vec2(0.0, 0.0), Vec2(2.0, 0.0), Vec2(4.0, 0.0)]);
        assert!(out.instance_source().is_some());
        // No source wired: bare instances, not an error.
        let bare = run(&GeometryRepeatProcessor, vec![], &[]).unwrap();
        assert_eq!(bare.instance_count(), 5);
        assert!(bare.instance_source().is_none());
    }

    #[test]
    fn distribute_node_reads_axis_and_mode() {
        use ravel_core::geometry::Primitive;
        let mut g = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(10.0, 0.0),
            Vec2(20.0, 0.0),
            Vec2(50.0, 0.0),
            Vec2(80.0, 0.0),
            Vec2(100.0, 0.0),
        ]);
        for i in 0..3 {
            g.push_primitive(Primitive::Path {
                verts: i * 2..i * 2 + 2,
                closed: false,
            });
        }
        let g: Arc<dyn NodeData> = Arc::new(g);
        let xs = |mode: &str, axis: &str| {
            let out = run(
                &GeometryDistributeProcessor,
                vec![g.clone()],
                &[("mode", s(mode)), ("axis", s(axis))],
            )
            .unwrap();
            let p = out.points().get("P").unwrap().as_vec2("P").unwrap();
            p.iter().map(|v| v.0).collect::<Vec<_>>()
        };
        assert_eq!(xs("gaps", "x"), [0.0, 10.0, 30.0, 60.0, 80.0, 100.0]);
        assert_eq!(xs("min", "x"), [0.0, 10.0, 0.0, 30.0, 0.0, 20.0]);
        // Nothing varies on y, so the y axis leaves x alone.
        assert_eq!(xs("gaps", "y"), [0.0, 10.0, 20.0, 50.0, 80.0, 100.0]);
    }

    #[test]
    fn deformer_nodes_read_axis_range_amount_and_group() {
        let g: Arc<dyn NodeData> = Arc::new(Geometry::from_points(vec![
            Vec2(10.0, 8.0),
            Vec2(60.0, 8.0),
            Vec2(8.0, 60.0),
        ]));
        let positions = |g: &Geometry| g.points().get("P").unwrap().as_vec2("P").unwrap().to_vec();
        // Taper along x over 0..100 by half: the point at x = 60 narrows to
        // 8 * (1 - 0.5 * 0.6) = 5.6; the one before `start` would not move.
        let out = run(
            &GeometryDeformProcessor::taper(),
            vec![g.clone()],
            &[
                ("amount", ResolvedValue::Float(0.5)),
                ("start", ResolvedValue::Float(20.0)),
                ("end", ResolvedValue::Float(100.0)),
            ],
        )
        .unwrap();
        let p = positions(&out);
        assert_eq!(p[0], Vec2(10.0, 8.0));
        assert!((p[1].1 - 8.0 * (1.0 - 0.5 * 0.5)).abs() < 1e-5);
        // axis = y reads the other coordinate: x = 8 narrows at y = 60.
        let out = run(
            &GeometryDeformProcessor::taper(),
            vec![g.clone()],
            &[("amount", ResolvedValue::Float(0.5)), ("axis", s("y"))],
        )
        .unwrap();
        assert!((positions(&out)[2].0 - 8.0 * (1.0 - 0.5 * 0.6)).abs() < 1e-5);
        // Each node type is its own deformer.
        let amount = [("amount", ResolvedValue::Float(90.0))];
        let bent = run(&GeometryDeformProcessor::bend(), vec![g.clone()], &amount).unwrap();
        let twisted = run(&GeometryDeformProcessor::twist(), vec![g], &amount).unwrap();
        assert_ne!(positions(&bent), positions(&twisted));
    }

    fn run_apply(geometry: &Geometry, group: &str) -> Geometry {
        let field: Arc<dyn NodeData> = Arc::new(FieldValue::new(ConstantField(10.0)));
        run(
            &ApplyFieldProcessor,
            vec![Arc::new(geometry.clone()), field],
            &[
                ("domain", s("point")),
                ("target", s("P")),
                ("combine", s("add")),
                ("group", s(group)),
            ],
        )
        .unwrap()
    }
}
