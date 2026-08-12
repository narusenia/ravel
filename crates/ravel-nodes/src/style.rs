// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Style nodes: they write the fill/stroke attributes and nothing else.
//!
//! `Geometry → FrameBuffer` stays `rasterize`'s alone
//! (`docs/specifications/procedural-geometry.md`), so these nodes only put
//! `fill` / `Cd` / `stroke_width` / `stroke_color` on a domain and leave the
//! drawing to it. Everything between here and the rasterizer — `field.apply`
//! above all — therefore modulates the look like any other attribute.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{AttributeValue, Domain, attribute_set, attribute_set_in_group, names};
use ravel_core::graph::Node;
use ravel_core::types::{Color, NodeData};
use std::sync::Arc;

use crate::attribute::{domain_param, geometry_input};

/// What `rasterize` falls back to when nobody wrote the attribute, which is
/// what the elements outside a `group` have to keep looking like. They are the
/// `rasterize` template's own parameter defaults; see
/// [`ravel_core::geometry::attribute_set_in_group`].
const UNSET_FILL: bool = true;
const UNSET_STROKE_WIDTH: f32 = 0.0;
const UNSET_COLOR: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

pub struct StyleFillProcessor;

impl StyleFillProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for StyleFillProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "style.fill")?;
        let domain = domain_param(params, "domain", Domain::Primitive);
        let group = params.str_or("group", "");
        let with_flag = attribute_set_in_group(
            geometry,
            domain,
            names::FILL,
            AttributeValue::Bool(params.bool_or("enabled", true)),
            group,
            AttributeValue::Bool(UNSET_FILL),
        )?;
        // `Cd` is the fill colour (the standard attribute table says so), so a
        // fill style writes it. It is the same column `field.apply` and
        // `field.ramp` drive, which is why a per-element gradient belongs
        // *after* this node rather than before it.
        Ok(Arc::new(attribute_set_in_group(
            &with_flag,
            domain,
            names::CD,
            AttributeValue::Color(color_param(params)),
            group,
            AttributeValue::Color(UNSET_COLOR),
        )?))
    }
}

pub struct StyleStrokeProcessor;

impl StyleStrokeProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for StyleStrokeProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "style.stroke")?;
        let domain = domain_param(params, "domain", Domain::Primitive);
        let group = params.str_or("group", "");
        let with_width = attribute_set_in_group(
            geometry,
            domain,
            names::STROKE_WIDTH,
            AttributeValue::F32(params.f32_or("width", 1.0)),
            group,
            AttributeValue::F32(UNSET_STROKE_WIDTH),
        )?;
        let with_color = attribute_set_in_group(
            &with_width,
            domain,
            names::STROKE_COLOR,
            AttributeValue::Color(color_param(params)),
            group,
            AttributeValue::Color(UNSET_COLOR),
        )?;
        // Cap and join are Detail attributes: one shape for the whole
        // geometry, so neither `domain` nor `group` applies to them.
        let with_cap = attribute_set(
            &with_color,
            Domain::Detail,
            names::CAP,
            AttributeValue::I32(cap_param(params)),
        )?;
        Ok(Arc::new(attribute_set(
            &with_cap,
            Domain::Detail,
            names::JOIN,
            AttributeValue::I32(join_param(params)),
        )?))
    }
}

pub struct StyleDashProcessor;

impl StyleDashProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for StyleDashProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "style.dash")?;
        // The pattern is written through as typed: `rasterize` owns the
        // parsing (and the warning for a malformed one), so the node editor
        // keeps showing what the user typed.
        let with_pattern = attribute_set(
            geometry,
            Domain::Detail,
            names::DASH,
            AttributeValue::Str(params.str_or("pattern", "").to_owned()),
        )?;
        Ok(Arc::new(attribute_set(
            &with_pattern,
            Domain::Detail,
            names::DASH_OFFSET,
            AttributeValue::F32(params.f32_or("offset", 0.0)),
        )?))
    }
}

fn cap_param(params: &ResolvedParams) -> i32 {
    match params.str_or("cap", "") {
        "butt" => names::CAP_BUTT,
        "square" => names::CAP_SQUARE,
        _ => names::CAP_ROUND,
    }
}

fn join_param(params: &ResolvedParams) -> i32 {
    match params.str_or("join", "") {
        "miter" => names::JOIN_MITER,
        "bevel" => names::JOIN_BEVEL,
        _ => names::JOIN_ROUND,
    }
}

fn color_param(params: &ResolvedParams) -> Color {
    let [r, g, b, a] = params.vec4_or("color", [1.0, 1.0, 1.0, 1.0]);
    Color::new(r, g, b, a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::geometry::{AttributeArray, Geometry, Primitive};
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::{FrameRate, Vec2};

    /// Emits a fixed Geometry; stands in for the upstream shape.
    struct GeoSource(Geometry);

    impl NodeProcessor for GeoSource {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::new(self.0.clone()))
        }
    }

    /// Two one-point paths, so the Primitive domain has two elements.
    fn two_paths() -> Geometry {
        let mut geo = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(1.0, 0.0),
            Vec2(2.0, 0.0),
            Vec2(3.0, 0.0),
        ]);
        geo.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        geo.push_primitive(Primitive::Path {
            verts: 2..4,
            closed: false,
        });
        geo
    }

    /// Run a chain of style nodes (in order) over `geo` through a real
    /// evaluator, so the parameter names the processors read are the ones the
    /// nodes actually carry.
    fn run(geo: Geometry, nodes: Vec<(&str, Vec<(&str, ParameterValue)>)>) -> Geometry {
        let mut graph = Graph::new()
            .add_node(
                Node::new(NodeId::new(1), "test.source").with_output("out", DataTypeId::GEOMETRY),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(GeoSource(geo)));
        let mut last = NodeId::new(1);
        for (index, (type_key, params)) in nodes.into_iter().enumerate() {
            let id = NodeId::new(index as u64 + 2);
            let mut node = Node::new(id, type_key)
                .with_input("geometry", &[DataTypeId::GEOMETRY])
                .with_output("output", DataTypeId::GEOMETRY);
            for (key, value) in params {
                node = node.with_param(key, value);
            }
            graph = graph
                .add_node(node)
                .unwrap()
                .add_edge(
                    EdgeId::new(index as u64 + 1),
                    last,
                    OutputPortIndex(0),
                    id,
                    InputPortIndex(0),
                )
                .unwrap();
            ev.register(
                id,
                match type_key {
                    "style.fill" => Arc::new(StyleFillProcessor) as Arc<dyn NodeProcessor>,
                    "style.dash" => Arc::new(StyleDashProcessor),
                    _ => Arc::new(StyleStrokeProcessor),
                },
            );
            last = id;
        }
        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (100, 100));
        let output = ev.evaluate(&graph, last, &ctx).unwrap();
        output.downcast_ref::<Geometry>().unwrap().clone()
    }

    fn color(rgba: [f32; 4]) -> ParameterValue {
        ParameterValue::Channel4(
            rgba.map(ravel_core::animation::channel::AnimationChannel::constant),
        )
    }

    /// The style attributes land on the chosen domain and nothing else moves:
    /// positions, primitives and the columns the node does not name stay as
    /// they were.
    #[test]
    fn style_nodes_write_their_own_columns_and_leave_the_rest_alone() {
        let mut geo = two_paths();
        geo.primitive_attrs_mut()
            .insert("weight", AttributeArray::F32(vec![4.0, 5.0]))
            .unwrap();
        let out = run(
            geo.clone(),
            vec![
                (
                    "style.fill",
                    vec![
                        ("enabled", ParameterValue::Bool(false)),
                        ("color", color([1.0, 0.0, 0.0, 1.0])),
                    ],
                ),
                (
                    "style.stroke",
                    vec![
                        ("width", ParameterValue::Float(3.0)),
                        ("color", color([0.0, 0.0, 1.0, 1.0])),
                    ],
                ),
            ],
        );

        let prims = out.primitive_attrs();
        assert_eq!(
            prims
                .get(names::FILL)
                .unwrap()
                .as_bool(names::FILL)
                .unwrap(),
            &[false, false]
        );
        assert_eq!(
            prims
                .get(names::STROKE_WIDTH)
                .unwrap()
                .as_f32(names::STROKE_WIDTH)
                .unwrap(),
            &[3.0, 3.0]
        );
        assert_eq!(
            prims.get(names::CD).unwrap().as_color(names::CD).unwrap(),
            &[Color::new(1.0, 0.0, 0.0, 1.0); 2]
        );
        assert_eq!(
            prims
                .get(names::STROKE_COLOR)
                .unwrap()
                .as_color(names::STROKE_COLOR)
                .unwrap(),
            &[Color::new(0.0, 0.0, 1.0, 1.0); 2]
        );

        // Untouched: the column that was already there, the positions, and the
        // other domains.
        assert_eq!(
            prims.get("weight").unwrap().as_f32("weight").unwrap(),
            &[4.0, 5.0]
        );
        assert_eq!(
            out.points().get(names::P).unwrap(),
            geo.points().get(names::P).unwrap()
        );
        assert_eq!(out.primitives().len(), 2);
        assert!(out.points().get(names::FILL).is_none());
        assert!(out.detail().get(names::CD).is_none());
    }

    /// `group` restricts the write: the elements it does not flag keep the
    /// value they had, and a fresh column seeds them with what `rasterize`
    /// does in the absence of the attribute.
    #[test]
    fn a_group_leaves_the_elements_outside_it_unchanged() {
        let mut geo = two_paths();
        geo.primitive_attrs_mut()
            .insert("mask", AttributeArray::Bool(vec![false, true]))
            .unwrap();
        geo.primitive_attrs_mut()
            .insert(names::STROKE_WIDTH, AttributeArray::F32(vec![7.0, 7.0]))
            .unwrap();
        let out = run(
            geo,
            vec![(
                "style.stroke",
                vec![
                    ("width", ParameterValue::Float(2.0)),
                    ("color", color([0.0, 1.0, 0.0, 1.0])),
                    ("group", ParameterValue::String("mask".into())),
                ],
            )],
        );

        let prims = out.primitive_attrs();
        assert_eq!(
            prims
                .get(names::STROKE_WIDTH)
                .unwrap()
                .as_f32(names::STROKE_WIDTH)
                .unwrap(),
            &[7.0, 2.0],
            "the primitive outside the group keeps its own width"
        );
        assert_eq!(
            prims
                .get(names::STROKE_COLOR)
                .unwrap()
                .as_color(names::STROKE_COLOR)
                .unwrap(),
            &[UNSET_COLOR, Color::new(0.0, 1.0, 0.0, 1.0)],
            "a new column seeds the elements outside the group with the unset colour"
        );
    }

    /// Styles do not accumulate: the second node's value is what reaches the
    /// rasterizer.
    #[test]
    fn applying_a_style_twice_lets_the_last_one_win() {
        let out = run(
            two_paths(),
            vec![
                (
                    "style.fill",
                    vec![
                        ("enabled", ParameterValue::Bool(true)),
                        ("color", color([1.0, 0.0, 0.0, 1.0])),
                    ],
                ),
                (
                    "style.fill",
                    vec![
                        ("enabled", ParameterValue::Bool(false)),
                        ("color", color([0.0, 0.0, 1.0, 1.0])),
                    ],
                ),
            ],
        );

        let prims = out.primitive_attrs();
        assert_eq!(
            prims
                .get(names::FILL)
                .unwrap()
                .as_bool(names::FILL)
                .unwrap(),
            &[false, false]
        );
        assert_eq!(
            prims.get(names::CD).unwrap().as_color(names::CD).unwrap(),
            &[Color::new(0.0, 0.0, 1.0, 1.0); 2]
        );
    }

    /// Cap, join and dash are Detail: one value for the geometry, whatever
    /// `domain` says, and named by the code the rasterizer reads.
    #[test]
    fn cap_join_and_dash_land_on_the_detail_domain() {
        let out = run(
            two_paths(),
            vec![
                (
                    "style.stroke",
                    vec![
                        ("cap", ParameterValue::String("square".into())),
                        ("join", ParameterValue::String("bevel".into())),
                        ("domain", ParameterValue::String("point".into())),
                    ],
                ),
                (
                    "style.dash",
                    vec![
                        ("pattern", ParameterValue::String("4,2".into())),
                        ("offset", ParameterValue::Float(1.5)),
                    ],
                ),
            ],
        );

        let detail = out.detail();
        assert_eq!(
            detail.get(names::CAP).unwrap().as_i32(names::CAP).unwrap(),
            &[names::CAP_SQUARE]
        );
        assert_eq!(
            detail
                .get(names::JOIN)
                .unwrap()
                .as_i32(names::JOIN)
                .unwrap(),
            &[names::JOIN_BEVEL]
        );
        assert_eq!(
            detail
                .get(names::DASH)
                .unwrap()
                .as_str(names::DASH)
                .unwrap(),
            &["4,2".to_owned()]
        );
        assert_eq!(
            detail
                .get(names::DASH_OFFSET)
                .unwrap()
                .as_f32(names::DASH_OFFSET)
                .unwrap(),
            &[1.5]
        );
        // The per-element columns still followed `domain`.
        assert!(out.points().get(names::STROKE_WIDTH).is_some());
    }

    /// An unset cap or join is round — the shape the rasterizer drew before
    /// the attributes existed, so a default `style.stroke` cannot change an
    /// existing picture.
    #[test]
    fn the_default_cap_and_join_are_round() {
        let out = run(two_paths(), vec![("style.stroke", vec![])]);
        assert_eq!(
            out.detail()
                .get(names::CAP)
                .unwrap()
                .as_i32(names::CAP)
                .unwrap(),
            &[names::CAP_ROUND]
        );
        assert_eq!(
            out.detail()
                .get(names::JOIN)
                .unwrap()
                .as_i32(names::JOIN)
                .unwrap(),
            &[names::JOIN_ROUND]
        );
    }

    /// The domain parameter decides which set the columns land on — the
    /// Instance domain is what makes a scatter of 500 copies stylable.
    #[test]
    fn the_domain_parameter_selects_the_attribute_set() {
        let mut geo = Geometry::new();
        geo.instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(4.0, 0.0)]),
            )
            .unwrap();
        geo.set_instance_source(Some(Arc::new(two_paths())));
        let out = run(
            geo,
            vec![(
                "style.stroke",
                vec![
                    ("width", ParameterValue::Float(5.0)),
                    ("domain", ParameterValue::String("instance".into())),
                ],
            )],
        );
        assert_eq!(
            out.instances()
                .get(names::STROKE_WIDTH)
                .unwrap()
                .as_f32(names::STROKE_WIDTH)
                .unwrap(),
            &[5.0, 5.0]
        );
        assert!(out.primitive_attrs().get(names::STROKE_WIDTH).is_none());
    }
}
