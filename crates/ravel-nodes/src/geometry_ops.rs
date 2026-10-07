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
use ravel_core::geometry::index_group::group_index;
use ravel_core::graph::Node;
use ravel_core::types::NodeData;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::ApplyFieldProcessor;
    use ravel_core::eval::{Evaluator, ResolvedValue};
    use ravel_core::geometry::{AttributeArray, ConstantField, Domain, FieldValue};
    use ravel_core::types::{FrameRate, Vec2};

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
