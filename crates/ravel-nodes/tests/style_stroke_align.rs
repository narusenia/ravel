// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `style.stroke`'s `stroke_align` parameter, end to end through `rasterize`.
//!
//! Expected pixels are derived from the band rule in
//! `docs/specifications/procedural-geometry.md` (signed distance, inside
//! negative): for the square's left edge at x = 8 a pixel `x` has
//! `d = 8 - x - 0.5`, and a width-4 stroke covers centre `|d| <= 1.5`, inside
//! `-3.5 <= d <= -0.5`, outside `0.5 <= d <= 3.5`.

use ravel_core::eval::{EvalContext, EvalScope, Evaluator, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{Geometry, Primitive, names};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::types::{FrameBuffer, FrameRate, NodeData, Vec2};
use ravel_nodes::rasterize::RasterizeProcessor;
use ravel_nodes::style::StyleStrokeProcessor;
use std::sync::Arc;

struct Source(Geometry);

impl NodeProcessor for Source {
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

/// A closed 16 px square, (8, 8)..(24, 24).
fn square() -> Geometry {
    let mut geo = Geometry::from_points(vec![
        Vec2(8.0, 8.0),
        Vec2(24.0, 8.0),
        Vec2(24.0, 24.0),
        Vec2(8.0, 24.0),
    ]);
    geo.push_primitive(Primitive::Path {
        verts: 0..4,
        closed: true,
    });
    geo
}

/// source -> style.stroke(params) -> rasterize; returns the style output
/// geometry and the rendered frame.
fn render(params: &[(&str, ParameterValue)]) -> (Geometry, FrameBuffer) {
    let mut registry = NodeRegistry::new();
    register_builtins(&mut registry);
    let make = |key: &str, id: u64, params: &[(&str, ParameterValue)]| {
        let mut node = registry.create_node(key, NodeId::new(id)).unwrap();
        for (k, v) in params {
            node.parameters
                .iter_mut()
                .find(|p| p.key == *k)
                .unwrap_or_else(|| panic!("{key} has no {k} parameter"))
                .value = v.clone();
        }
        node
    };
    let source = make("shape.rect", 1, &[]);
    let mut style_params = vec![("width", ParameterValue::Float(4.0))];
    style_params.extend_from_slice(params);
    let style = make("style.stroke", 2, &style_params);
    let rasterize = make("rasterize", 3, &[("fill", ParameterValue::Bool(false))]);
    let mut graph = Graph::new();
    for n in [&source, &style, &rasterize] {
        graph = graph.add_node(n.clone()).unwrap();
    }
    for (edge, from, to) in [(1, 1, 2), (2, 2, 3)] {
        graph = graph
            .add_edge(
                EdgeId::new(edge),
                NodeId::new(from),
                OutputPortIndex(0),
                NodeId::new(to),
                InputPortIndex(0),
            )
            .unwrap();
    }
    let mut ev = Evaluator::new();
    ev.register(NodeId::new(1), Arc::new(Source(square())));
    ev.register(
        NodeId::new(2),
        Arc::new(StyleStrokeProcessor::from_node(&style)),
    );
    ev.register(
        NodeId::new(3),
        Arc::new(RasterizeProcessor::from_node(&rasterize)),
    );
    let ctx = EvalContext::new(0, FrameRate::new(24, 1), (32, 32));
    let geo = ev.evaluate(&graph, NodeId::new(2), &ctx).unwrap();
    let geo = geo.downcast_ref::<Geometry>().unwrap().clone();
    let fb = ev.evaluate(&graph, NodeId::new(3), &ctx).unwrap();
    (geo, fb.downcast_ref::<FrameBuffer>().unwrap().clone())
}

fn align(name: &str) -> ParameterValue {
    ParameterValue::String(name.into())
}

/// x positions on row 16 (mid left edge) with full alpha.
fn span(fb: &FrameBuffer) -> Vec<u32> {
    (0..16)
        .filter(|&x| fb.as_f32()[((16 * 32 + x) * 4 + 3) as usize] > 0.99)
        .collect()
}

fn column(geo: &Geometry) -> Option<Vec<i32>> {
    geo.primitive_attrs()
        .get(names::STROKE_ALIGN)
        .map(|c| c.as_i32(names::STROKE_ALIGN).unwrap().to_vec())
}

#[test]
fn inside_and_outside_write_the_column_and_rasterize_draws_the_band() {
    let (geo, fb) = render(&[("stroke_align", align("inside"))]);
    assert_eq!(column(&geo), Some(vec![names::STROKE_ALIGN_INSIDE]));
    assert_eq!(span(&fb), vec![8, 9, 10, 11]);

    let (geo, fb) = render(&[("stroke_align", align("outside"))]);
    assert_eq!(column(&geo), Some(vec![names::STROKE_ALIGN_OUTSIDE]));
    assert_eq!(span(&fb), vec![4, 5, 6, 7]);
}

/// Centre and an absent parameter write no column and draw the centred band
/// (`|d| <= 1.5` covers x = 6..=9), so existing graphs are unchanged.
#[test]
fn center_and_absent_leave_the_picture_unaligned() {
    let (geo_default, fb_default) = render(&[]);
    let (geo_center, fb_center) = render(&[("stroke_align", align("center"))]);
    assert_eq!(column(&geo_default), None);
    assert_eq!(column(&geo_center), None);
    assert_eq!(span(&fb_default), vec![6, 7, 8, 9]);
    assert_eq!(fb_default.as_f32(), fb_center.as_f32());
}
