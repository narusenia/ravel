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

/// The built-ins that declare the section, exercised through the registry:
/// every node here is created from its own template, so what is fixed is the
/// declaration and not a second list of parameters kept in the test.
#[cfg(test)]
mod declared_nodes {
    use super::*;
    use crate::geometry::{GeometryFromImageProcessor, GeometryTransformProcessor};
    use crate::shape::RectProcessor;
    use crate::text::{LayoutProcessor, ToPathProcessor};
    use ravel_core::eval::Evaluator;
    use ravel_core::geometry::{names, ops};
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::registry::NodeRegistry;
    use ravel_core::registry::builtin::{TRANSFORM_SECTION_NODES, register_builtins};
    use ravel_core::types::{FrameBuffer, FrameRate, Rect, Vec2};

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (64, 64))
    }

    fn registry() -> NodeRegistry {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        reg
    }

    /// A node of `type_key` with its template's parameters, `params` applied
    /// over them.
    fn node(
        reg: &NodeRegistry,
        id: u64,
        type_key: &str,
        params: &[(&str, ParameterValue)],
    ) -> Node {
        let mut node = reg
            .create_node(type_key, NodeId::new(id))
            .unwrap_or_else(|| panic!("{type_key} is a built-in"));
        for (key, value) in params {
            let slot = node
                .parameters
                .iter_mut()
                .find(|param| &param.key == key)
                .unwrap_or_else(|| panic!("{type_key} declares no {key}"));
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

    /// **The section costs nothing at its defaults.** A document that never
    /// touched these rows evaluates to the very `Arc` the inner processor
    /// produced, for every type that declares the section — which is what
    /// "output unchanged by this feature" means for every existing project.
    #[test]
    fn a_declared_node_at_its_defaults_hands_the_inner_value_straight_back() {
        let reg = registry();
        let geometry: Arc<dyn NodeData> =
            Arc::new(Geometry::from_points(vec![Vec2(1.0, 2.0), Vec2(-3.0, 4.0)]));
        for type_key in TRANSFORM_SECTION_NODES {
            let node = node(&reg, 1, type_key, &[]);
            let wrapped = wrap(&node, Arc::new(Fixed(geometry.clone())));
            let out = wrapped
                .process(
                    &node,
                    &ctx(),
                    &[],
                    &ResolvedParams::default(),
                    &mut Evaluator::new(),
                )
                .expect("the section evaluates");
            assert!(
                Arc::ptr_eq(&geometry, &out),
                "{type_key} is not identity at its template defaults"
            );
        }
    }

    /// `shape.rect`'s `center` keeps meaning what it meant, and the section's
    /// `translate` is an **extra** move on top of it: the two add up, the
    /// intrinsic one first, because the shape is built around `center` before
    /// the section ever sees the geometry.
    #[test]
    fn a_rect_adds_its_center_and_the_sections_translate() {
        let reg = registry();
        let bbox = |params: &[(&str, ParameterValue)]| -> Rect {
            let node = node(&reg, 1, "shape.rect", params);
            let graph = Graph::new().add_node(node.clone()).expect("one node");
            let mut ev = Evaluator::new();
            ev.register(
                NodeId::new(1),
                wrap(&node, Arc::new(RectProcessor::from_node(&node))),
            );
            let out = ev
                .evaluate(&graph, NodeId::new(1), &ctx())
                .expect("the rect evaluates");
            ops::drawn_bounds(out.downcast_ref::<Geometry>().expect("geometry"))
                .expect("a rectangle has bounds")
        };
        let mid = |r: Rect| Vec2(r.x + r.width / 2.0, r.y + r.height / 2.0);

        let origin = bbox(&[]);
        let centred = bbox(&[("center", ParameterValue::vec2(10.0, -4.0))]);
        assert_eq!(
            mid(centred),
            Vec2(mid(origin).0 + 10.0, mid(origin).1 - 4.0),
            "`center` alone must keep doing exactly what it did"
        );

        let both = bbox(&[
            ("center", ParameterValue::vec2(10.0, -4.0)),
            ("translate", ParameterValue::vec3(3.0, 7.0, 0.0)),
        ]);
        assert_eq!(
            mid(both),
            Vec2(mid(centred).0 + 3.0, mid(centred).1 + 7.0),
            "`center` and the section's `translate` add up"
        );
        assert_eq!(
            (both.width, both.height),
            (centred.width, centred.height),
            "a translation changes nothing but the place"
        );
    }

    /// Turning a `text.layout` with the section lands the outlines exactly
    /// where a `geometry.transform` between the layout and `text.to_path`
    /// would: the relation, not a table of glyph coordinates.
    #[test]
    fn rotating_a_text_layout_matches_a_geometry_transform_downstream() {
        let reg = registry();
        let rotation = ParameterValue::vec3(0.0, 0.0, 30.0);

        let layout = node(
            &reg,
            1,
            "text.layout",
            &[
                ("text", ParameterValue::String("Ravel".into())),
                ("rotation", rotation.clone()),
            ],
        );
        let to_path = node(&reg, 2, "text.to_path", &[]);
        let graph = Graph::new()
            .add_node(layout.clone())
            .expect("layout")
            .add_node(to_path.clone())
            .expect("to_path")
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .expect("edge");
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), wrap(&layout, Arc::new(LayoutProcessor)));
        ev.register(NodeId::new(2), Arc::new(ToPathProcessor));
        let sectioned = ev
            .evaluate(&graph, NodeId::new(2), &ctx())
            .expect("the sectioned chain evaluates");

        let plain = node(
            &reg,
            1,
            "text.layout",
            &[("text", ParameterValue::String("Ravel".into()))],
        );
        let transform = node(&reg, 3, "geometry.transform", &[("rotation", rotation)]);
        let graph = Graph::new()
            .add_node(plain.clone())
            .expect("layout")
            .add_node(transform)
            .expect("transform")
            .add_node(to_path)
            .expect("to_path")
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .expect("layout to transform")
            .add_edge(
                EdgeId::new(2),
                NodeId::new(3),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .expect("transform to to_path");
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), wrap(&plain, Arc::new(LayoutProcessor)));
        ev.register(NodeId::new(3), Arc::new(GeometryTransformProcessor));
        ev.register(NodeId::new(2), Arc::new(ToPathProcessor));
        let chained = ev
            .evaluate(&graph, NodeId::new(2), &ctx())
            .expect("the explicit chain evaluates");

        let turned = positions(&sectioned);
        assert!(!turned.is_empty(), "the text has to produce outlines");
        assert_eq!(turned, positions(&chained));
    }

    /// The section turns what `geometry.from_image` places, and
    /// `drawn_bounds` answers with the **outer** rectangle of the turned one:
    /// a 100×50 image at 45° bounds a square of (100 + 50) / sqrt(2) a side.
    #[test]
    fn rotating_an_image_rectangle_bounds_the_turned_rectangle() {
        let reg = registry();
        let bounds = |params: &[(&str, ParameterValue)]| -> Rect {
            let node = node(&reg, 2, "geometry.from_image", params);
            let source = Node::new(NodeId::new(1), "test.image")
                .with_output("output", DataTypeId::FRAME_BUFFER);
            let graph = Graph::new()
                .add_node(source)
                .expect("source")
                .add_node(node.clone())
                .expect("from_image")
                .add_edge(
                    EdgeId::new(1),
                    NodeId::new(1),
                    OutputPortIndex(0),
                    NodeId::new(2),
                    InputPortIndex(0),
                )
                .expect("edge");
            let mut ev = Evaluator::new();
            let frame: Arc<dyn NodeData> =
                Arc::new(FrameBuffer::from_f32(100, 50, vec![0.0; 100 * 50 * 4]));
            ev.register(NodeId::new(1), Arc::new(Fixed(frame)));
            ev.register(
                NodeId::new(2),
                wrap(&node, Arc::new(GeometryFromImageProcessor)),
            );
            let out = ev
                .evaluate(&graph, NodeId::new(2), &ctx())
                .expect("from_image evaluates");
            ops::drawn_bounds(out.downcast_ref::<Geometry>().expect("geometry"))
                .expect("the stamped image has bounds")
        };

        let flat = bounds(&[]);
        assert_eq!((flat.width, flat.height), (100.0, 50.0));

        let turned = bounds(&[("rotation", ParameterValue::vec3(0.0, 0.0, 45.0))]);
        let side = 150.0 / 2.0_f32.sqrt();
        assert!(
            (turned.width - side).abs() < 0.01 && (turned.height - side).abs() < 0.01,
            "a 45-degree turn must bound the outer square, got {turned:?}"
        );
    }
}
