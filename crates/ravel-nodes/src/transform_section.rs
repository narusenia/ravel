// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The **transform section**: the parameters a template declares with
//! `NodeTemplate::with_transform_section`, applied to the node's geometry
//! output after its own processor has run
//! (`docs/implementation/node-transform-section-plan.md`).
//!
//! No node is inserted and nothing is hidden from the node editor — the
//! values are parameters of the node the user is already looking at, the way
//! `shape.rect`'s `center` always was. The applying code is
//! [`crate::geometry::apply_transform`], `geometry.transform`'s own body, so a
//! declared section and an explicitly wired `geometry.transform` cannot drift.

use crate::geometry::apply_transform;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::Geometry;
use ravel_core::graph::Node;
use ravel_core::registry::builtin::has_transform_section;
use ravel_core::types::NodeData;
use std::borrow::Cow;
use std::sync::Arc;

/// Wraps `inner` in the transform section when `node`'s type declares one,
/// and hands `inner` straight back when it does not.
///
/// The registry is the single source of the answer: `processor_for_node`
/// calls this once for every built-in it returns rather than naming type keys
/// a second time.
pub fn wrap(node: &Node, inner: Arc<dyn NodeProcessor>) -> Arc<dyn NodeProcessor> {
    if has_transform_section(&node.type_key) {
        Arc::new(TransformSection { inner })
    } else {
        inner
    }
}

/// `inner`'s output, put through the section's parameters when it is a
/// [`Geometry`].
struct TransformSection {
    inner: Arc<dyn NodeProcessor>,
}

impl NodeProcessor for TransformSection {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let out = self.inner.process(node, ctx, inputs, params, scope)?;
        // A node that emits something else (a frame buffer, a multi-output
        // record) passes through untouched: the section is geometry-only, and
        // failing here would make a mis-declaration a runtime error instead of
        // a no-op.
        let Some(geometry) = out.downcast_ref::<Geometry>() else {
            return Ok(out);
        };
        // The values are read from `params` on every evaluation, never
        // captured here. `GpuEvalHooks::sync` answers a parameter edit on a
        // processor that opts out of rebuilding with `invalidate_node` alone,
        // so `wrap` is not called again and anything captured would stay at
        // its pre-edit value forever (`eval_hooks.rs`, `InvalidationHint::Params`).
        Ok(match apply_transform(geometry, params)? {
            Cow::Borrowed(_) => out,
            Cow::Owned(transformed) => Arc::new(transformed),
        })
    }

    fn is_time_dependent(&self) -> bool {
        self.inner.is_time_dependent()
    }

    /// The inner processor's answer, unchanged. Returning `true`
    /// unconditionally would take the eight processors that opt out back to a
    /// rebuild per edit tick — for the GPU ones, a shader recompile and a
    /// pipeline creation each time a slider moves.
    fn rebuild_on_node_change(&self) -> bool {
        self.inner.rebuild_on_node_change()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::GeometryTransformProcessor;
    use ravel_core::eval::Evaluator;
    use ravel_core::geometry::names;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::registry::TRANSFORM_SECTION_PARAMS;
    use ravel_core::types::{FrameBuffer, FrameRate, Vec2};

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (64, 64))
    }

    /// A source that emits a fixed geometry and **opts out of rebuilding**,
    /// which is the combination the wrapper has to stay correct under: an edit
    /// reaches such a node as `invalidate_node` alone, so `wrap` never runs
    /// again and anything captured at construction would stay stale.
    struct Points(Vec<Vec2>);

    impl NodeProcessor for Points {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(Geometry::from_points(self.0.clone())))
        }

        fn rebuild_on_node_change(&self) -> bool {
            false
        }
    }

    /// A source that emits something the section has no business touching.
    struct Frame;

    impl NodeProcessor for Frame {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(FrameBuffer::from_f32(1, 1, vec![0.0; 4])))
        }
    }

    /// Hands back the value it was given, so a test can compare `Arc`s.
    struct Fixed(Arc<dyn NodeData>);

    impl NodeProcessor for Fixed {
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

    /// A source node carrying the section's parameters, as a template that
    /// declared the section would hand them to the evaluator.
    fn section_node(id: u64, params: &[(&str, ParameterValue)]) -> Node {
        let mut node = Node::new(NodeId::new(id), "test.section")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("translate", ParameterValue::vec3(0.0, 0.0, 0.0))
            .with_param("rotation", ParameterValue::vec3(0.0, 0.0, 0.0))
            .with_param("scale", ParameterValue::vec3(1.0, 1.0, 1.0))
            .with_param("use_centroid", ParameterValue::Bool(true))
            .with_param("pivot", ParameterValue::vec3(0.0, 0.0, 0.0));
        for (key, value) in params {
            let slot = node
                .parameters
                .iter_mut()
                .find(|param| &param.key == key)
                .expect("the section declares this parameter");
            slot.value = value.clone();
        }
        node
    }

    fn positions(value: &Arc<dyn NodeData>) -> Vec<Vec2> {
        value
            .downcast_ref::<Geometry>()
            .expect("output is Geometry")
            .points()
            .get(names::P)
            .expect("P")
            .as_vec2(names::P)
            .expect("Vec2 P")
            .to_vec()
    }

    /// The section **is** `geometry.transform`, so the fixture is that
    /// relation rather than a table of coordinates: a wrapped source and the
    /// same parameters on a `geometry.transform` inserted downstream have to
    /// land on the same points.
    #[test]
    fn the_section_agrees_with_a_geometry_transform_downstream() {
        let input = vec![Vec2(1.0, 0.0), Vec2(3.0, 2.0)];
        let params = [
            ("translate", ParameterValue::vec3(5.0, -2.0, 0.0)),
            ("rotation", ParameterValue::vec3(0.0, 0.0, 30.0)),
            ("scale", ParameterValue::vec3(2.0, 0.5, 1.0)),
            ("use_centroid", ParameterValue::Bool(false)),
            ("pivot", ParameterValue::vec3(1.0, 1.0, 0.0)),
        ];

        let graph = Graph::new()
            .add_node(section_node(1, &params))
            .expect("source");
        let mut ev = Evaluator::new();
        ev.register(
            NodeId::new(1),
            Arc::new(TransformSection {
                inner: Arc::new(Points(input.clone())),
            }),
        );
        let sectioned = ev
            .evaluate(&graph, NodeId::new(1), &ctx())
            .expect("the section evaluates");

        let mut transform = section_node(2, &params);
        transform.type_key = "geometry.transform".into();
        transform.inputs = Node::new(NodeId::new(0), "x")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .inputs;
        let plain =
            Node::new(NodeId::new(1), "test.source").with_output("output", DataTypeId::GEOMETRY);
        let graph = Graph::new()
            .add_node(plain)
            .expect("source")
            .add_node(transform)
            .expect("transform")
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .expect("edge");
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(Points(input)));
        ev.register(NodeId::new(2), Arc::new(GeometryTransformProcessor));
        let chained = ev
            .evaluate(&graph, NodeId::new(2), &ctx())
            .expect("the chain evaluates");

        assert_eq!(positions(&sectioned), positions(&chained));
    }

    /// Identity is fixed at **`Arc` identity**, not merely equal contents: a
    /// section whose parameters say "do nothing" shares its inner value
    /// wholesale, the way `geometry.transform` always has.
    #[test]
    fn an_identity_section_hands_back_the_inner_value_itself() {
        let node = section_node(1, &[]);
        let inner: Arc<dyn NodeData> = Arc::new(Geometry::from_points(vec![Vec2(1.0, 2.0)]));
        let out = TransformSection {
            inner: Arc::new(Fixed(inner.clone())),
        }
        .process(
            &node,
            &ctx(),
            &[],
            &ResolvedParams::default(),
            &mut Evaluator::new(),
        )
        .expect("the section evaluates");
        assert!(
            Arc::ptr_eq(&inner, &out),
            "an identity section rebuilt the geometry instead of sharing it"
        );
    }

    /// Declaring the section on a node that emits something else is a no-op,
    /// not a type error: no template does it, and `wrap` is the one place that
    /// would otherwise turn such a mistake into a failed render.
    #[test]
    fn a_non_geometry_output_passes_through() {
        let out = TransformSection {
            inner: Arc::new(Frame),
        }
        .process(
            &section_node(1, &[]),
            &ctx(),
            &[],
            &ResolvedParams::default(),
            &mut Evaluator::new(),
        )
        .expect("the section evaluates");
        assert!(out.downcast_ref::<FrameBuffer>().is_some());
    }

    /// An edit reaches a section wrapped around a processor that opts out of
    /// rebuilding, on the path that only invalidates. Capturing the values in
    /// `wrap` would leave this node at its pre-edit transform forever.
    #[test]
    fn an_edit_reaches_a_section_that_is_never_re_registered() {
        let wrapped: Arc<dyn NodeProcessor> = Arc::new(TransformSection {
            inner: Arc::new(Points(vec![Vec2(1.0, 0.0)])),
        });
        assert!(
            !wrapped.rebuild_on_node_change(),
            "the wrapper must answer with the inner processor's own answer"
        );

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), wrapped);
        let graph = Graph::new().add_node(section_node(1, &[])).expect("source");
        let before = positions(
            &ev.evaluate(&graph, NodeId::new(1), &ctx())
                .expect("evaluates"),
        );

        let edited = Graph::new()
            .add_node(section_node(
                1,
                &[("translate", ParameterValue::vec3(4.0, 7.0, 0.0))],
            ))
            .expect("source");
        ev.invalidate_node(NodeId::new(1));
        let after = positions(
            &ev.evaluate(&edited, NodeId::new(1), &ctx())
                .expect("evaluates"),
        );

        assert_eq!(before, [Vec2(1.0, 0.0)]);
        assert_eq!(after, [Vec2(5.0, 7.0)]);
    }

    /// The section's spelling is the registry's, not a second list kept here.
    #[test]
    fn the_section_reads_the_parameters_the_registry_declares() {
        let node = section_node(1, &[]);
        for key in TRANSFORM_SECTION_PARAMS {
            assert!(
                node.parameters.iter().any(|param| param.key == key),
                "{key} is declared by with_transform_section but not read here"
            );
        }
    }
}
