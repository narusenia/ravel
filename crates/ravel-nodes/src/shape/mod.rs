// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shape generation node processors.
//!
//! Each processor outputs a [`Geometry`] containing a single closed path
//! primitive built from the shape's point positions.

use std::f32::consts::PI;
use std::sync::Arc;

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{AttributeArray, Geometry, Primitive, names};
use ravel_core::graph::Node;
use ravel_core::types::{NodeData, Vec2};

// ---------------------------------------------------------------------------
// Rectangle
// ---------------------------------------------------------------------------

pub struct RectProcessor;

impl RectProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for RectProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let center_x = params.f32_or("center_x", 0.0);
        let center_y = params.f32_or("center_y", 0.0);
        let width = params.f32_or("width", 100.0);
        let height = params.f32_or("height", 100.0);

        let hw = width / 2.0;
        let hh = height / 2.0;

        let points = vec![
            Vec2(center_x - hw, center_y - hh),
            Vec2(center_x + hw, center_y - hh),
            Vec2(center_x + hw, center_y + hh),
            Vec2(center_x - hw, center_y + hh),
        ];

        let mut geo = Geometry::from_points(points);
        geo.detail_mut().insert(
            names::ANCHOR,
            AttributeArray::Vec2(vec![Vec2(center_x, center_y)]),
        )?;
        geo.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        Ok(Arc::new(geo))
    }
}

// ---------------------------------------------------------------------------
// Ellipse (polyline approximation)
// ---------------------------------------------------------------------------

const DEFAULT_ELLIPSE_SEGMENTS: i32 = 32;

pub struct EllipseProcessor;

impl EllipseProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for EllipseProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let center_x = params.f32_or("center_x", 0.0);
        let center_y = params.f32_or("center_y", 0.0);
        let radius_x = params.f32_or("radius_x", 50.0);
        let radius_y = params.f32_or("radius_y", 50.0);
        let segments = params.i32_or("segments", DEFAULT_ELLIPSE_SEGMENTS);

        let n = segments.max(3) as usize;
        let mut points = Vec::with_capacity(n);

        for i in 0..n {
            let angle = 2.0 * PI * i as f32 / n as f32;
            points.push(Vec2(
                center_x + radius_x * angle.cos(),
                center_y + radius_y * angle.sin(),
            ));
        }

        let count = points.len();
        let mut geo = Geometry::from_points(points);
        geo.detail_mut().insert(
            names::ANCHOR,
            AttributeArray::Vec2(vec![Vec2(center_x, center_y)]),
        )?;
        geo.push_primitive(Primitive::Path {
            verts: 0..count,
            closed: true,
        });
        Ok(Arc::new(geo))
    }
}

// ---------------------------------------------------------------------------
// Regular polygon
// ---------------------------------------------------------------------------

pub struct PolygonProcessor;

impl PolygonProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for PolygonProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let center_x = params.f32_or("center_x", 0.0);
        let center_y = params.f32_or("center_y", 0.0);
        let radius = params.f32_or("radius", 50.0);
        let sides = params.i32_or("sides", 6);

        let n = sides.max(3) as usize;
        let mut points = Vec::with_capacity(n);

        let offset = -PI / 2.0;
        for i in 0..n {
            let angle = offset + 2.0 * PI * i as f32 / n as f32;
            points.push(Vec2(
                center_x + radius * angle.cos(),
                center_y + radius * angle.sin(),
            ));
        }

        let count = points.len();
        let mut geo = Geometry::from_points(points);
        geo.detail_mut().insert(
            names::ANCHOR,
            AttributeArray::Vec2(vec![Vec2(center_x, center_y)]),
        )?;
        geo.push_primitive(Primitive::Path {
            verts: 0..count,
            closed: true,
        });
        Ok(Arc::new(geo))
    }
}

// ---------------------------------------------------------------------------
// Star
// ---------------------------------------------------------------------------

pub struct StarProcessor;

impl StarProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for StarProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let center_x = params.f32_or("center_x", 0.0);
        let center_y = params.f32_or("center_y", 0.0);
        let outer_radius = params.f32_or("outer_radius", 50.0);
        let inner_radius = params.f32_or("inner_radius", 25.0);
        let point_count = params.i32_or("points", 5);

        let n = point_count.max(3) as usize;
        let total = n * 2;
        let mut points = Vec::with_capacity(total);

        let offset = -PI / 2.0;
        for i in 0..total {
            let angle = offset + 2.0 * PI * i as f32 / total as f32;
            let radius = if i % 2 == 0 {
                outer_radius
            } else {
                inner_radius
            };
            points.push(Vec2(
                center_x + radius * angle.cos(),
                center_y + radius * angle.sin(),
            ));
        }

        let count = points.len();
        let mut geo = Geometry::from_points(points);
        geo.detail_mut().insert(
            names::ANCHOR,
            AttributeArray::Vec2(vec![Vec2(center_x, center_y)]),
        )?;
        geo.push_primitive(Primitive::Path {
            verts: 0..count,
            closed: true,
        });
        Ok(Arc::new(geo))
    }
}

// ---------------------------------------------------------------------------
// Custom path (placeholder — awaits ParameterValue::PathPoints)
// ---------------------------------------------------------------------------

pub struct CustomPathProcessor;

impl CustomPathProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for CustomPathProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(Geometry::new()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::NodeId;
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (100, 100))
    }

    fn run(node: &Node, proc: Arc<dyn NodeProcessor>) -> Geometry {
        let graph = Graph::new().add_node(node.clone()).unwrap();
        let mut ev = Evaluator::new();
        ev.register(node.id, proc);
        let out = ev.evaluate(&graph, node.id, &ctx()).unwrap();
        out.downcast_ref::<Geometry>().unwrap().clone()
    }

    fn bounds_of(geo: &Geometry) -> (f32, f32, f32, f32) {
        let b = <Geometry as ravel_core::types::GeometricData>::bounds(geo);
        (b.x, b.y, b.width, b.height)
    }

    fn make_node(type_key: &str, params: &[(&str, ParameterValue)]) -> Node {
        let mut node = Node::new(NodeId::new(1), type_key);
        for (key, value) in params {
            node = node.with_param(*key, value.clone());
        }
        node
    }

    // -- Rect ---------------------------------------------------------------

    #[test]
    fn rect_point_count_and_closed() {
        let node = make_node("shape.rect", &[]);
        let geo = run(&node, Arc::new(RectProcessor::from_node(&node)));
        assert_eq!(geo.point_count(), 4);
        assert_eq!(geo.primitive_count(), 1);
        assert!(matches!(
            geo.primitives()[0],
            Primitive::Path { closed: true, .. }
        ));
        assert!(geo.validate().is_ok());
    }

    #[test]
    fn rect_bounding_box() {
        let node = make_node(
            "shape.rect",
            &[
                ("center_x", ParameterValue::Float(50.0)),
                ("center_y", ParameterValue::Float(50.0)),
                ("width", ParameterValue::Float(40.0)),
                ("height", ParameterValue::Float(20.0)),
            ],
        );
        let geo = run(&node, Arc::new(RectProcessor::from_node(&node)));
        let (x, y, w, h) = bounds_of(&geo);
        assert!((x - 30.0).abs() < 1e-5);
        assert!((y - 40.0).abs() < 1e-5);
        assert!((w - 40.0).abs() < 1e-5);
        assert!((h - 20.0).abs() < 1e-5);
    }

    // -- Ellipse ------------------------------------------------------------

    #[test]
    fn ellipse_point_count_and_closed() {
        let node = make_node("shape.ellipse", &[("segments", ParameterValue::Int(16))]);
        let geo = run(&node, Arc::new(EllipseProcessor::from_node(&node)));
        assert_eq!(geo.point_count(), 16);
        assert_eq!(geo.primitive_count(), 1);
        assert!(matches!(
            geo.primitives()[0],
            Primitive::Path { closed: true, .. }
        ));
        assert!(geo.validate().is_ok());
    }

    #[test]
    fn ellipse_bounding_box_approximation() {
        let node = make_node(
            "shape.ellipse",
            &[
                ("radius_x", ParameterValue::Float(30.0)),
                ("radius_y", ParameterValue::Float(20.0)),
                ("segments", ParameterValue::Int(64)),
            ],
        );
        let geo = run(&node, Arc::new(EllipseProcessor::from_node(&node)));
        let (x, y, w, h) = bounds_of(&geo);
        assert!((x + 30.0).abs() < 0.5);
        assert!((y + 20.0).abs() < 0.5);
        assert!((w - 60.0).abs() < 1.0);
        assert!((h - 40.0).abs() < 1.0);
    }

    #[test]
    fn ellipse_clamps_minimum_segments() {
        let node = make_node("shape.ellipse", &[("segments", ParameterValue::Int(1))]);
        let geo = run(&node, Arc::new(EllipseProcessor::from_node(&node)));
        assert!(geo.point_count() >= 3);
    }

    // -- Polygon ------------------------------------------------------------

    #[test]
    fn polygon_hexagon() {
        let node = make_node(
            "shape.polygon",
            &[
                ("radius", ParameterValue::Float(50.0)),
                ("sides", ParameterValue::Int(6)),
            ],
        );
        let geo = run(&node, Arc::new(PolygonProcessor::from_node(&node)));
        assert_eq!(geo.point_count(), 6);
        assert_eq!(geo.primitive_count(), 1);
        assert!(matches!(
            geo.primitives()[0],
            Primitive::Path { closed: true, .. }
        ));
        assert!(geo.validate().is_ok());
    }

    #[test]
    fn polygon_bounding_box() {
        let node = make_node(
            "shape.polygon",
            &[
                ("radius", ParameterValue::Float(40.0)),
                ("sides", ParameterValue::Int(4)),
            ],
        );
        let geo = run(&node, Arc::new(PolygonProcessor::from_node(&node)));
        let (x, y, w, h) = bounds_of(&geo);
        // 4-sided polygon starting at -PI/2: vertices at (0,-40),(40,0),(0,40),(-40,0)
        assert!((x + 40.0).abs() < 1e-4);
        assert!((y + 40.0).abs() < 1e-4);
        assert!((w - 80.0).abs() < 1e-3);
        assert!((h - 80.0).abs() < 1e-3);
    }

    // -- Star ---------------------------------------------------------------

    #[test]
    fn star_point_count() {
        let node = make_node("shape.star", &[("points", ParameterValue::Int(5))]);
        let geo = run(&node, Arc::new(StarProcessor::from_node(&node)));
        assert_eq!(geo.point_count(), 10);
        assert_eq!(geo.primitive_count(), 1);
        assert!(matches!(
            geo.primitives()[0],
            Primitive::Path { closed: true, .. }
        ));
        assert!(geo.validate().is_ok());
    }

    #[test]
    fn star_bounding_box_bounded_by_outer_radius() {
        let node = make_node(
            "shape.star",
            &[
                ("outer_radius", ParameterValue::Float(60.0)),
                ("inner_radius", ParameterValue::Float(30.0)),
                ("points", ParameterValue::Int(5)),
            ],
        );
        let geo = run(&node, Arc::new(StarProcessor::from_node(&node)));
        let (x, y, w, h) = bounds_of(&geo);
        assert!(x >= -60.0 - 1e-4 && y >= -60.0 - 1e-4);
        assert!(w <= 120.0 + 1e-3 && h <= 120.0 + 1e-3);
    }

    // -- Custom path --------------------------------------------------------

    #[test]
    fn custom_path_returns_empty_geometry() {
        let node = make_node("shape.custom_path", &[]);
        let geo = run(&node, Arc::new(CustomPathProcessor::from_node(&node)));
        assert_eq!(geo.point_count(), 0);
        assert_eq!(geo.primitive_count(), 0);
        assert!(geo.detail().get(names::ANCHOR).is_none());
        assert!(geo.validate().is_ok());
    }

    #[test]
    fn generated_shapes_set_anchor_to_their_center() {
        let center = [
            ("center_x", ParameterValue::Float(12.0)),
            ("center_y", ParameterValue::Float(-7.0)),
        ];
        let shapes: Vec<(Node, Arc<dyn NodeProcessor>)> = vec![
            (make_node("shape.rect", &center), Arc::new(RectProcessor)),
            (
                make_node("shape.ellipse", &center),
                Arc::new(EllipseProcessor),
            ),
            (
                make_node("shape.polygon", &center),
                Arc::new(PolygonProcessor),
            ),
            (make_node("shape.star", &center), Arc::new(StarProcessor)),
        ];

        for (node, processor) in &shapes {
            let geometry = run(node, processor.clone());
            let anchor = geometry
                .detail()
                .get(names::ANCHOR)
                .expect("generated shape must set anchor")
                .as_vec2(names::ANCHOR)
                .unwrap();
            assert_eq!(anchor, &[Vec2(12.0, -7.0)]);
        }
    }

    // -- P attribute present ------------------------------------------------

    #[test]
    fn all_shapes_have_p_attribute() {
        let shapes: Vec<(Node, Arc<dyn NodeProcessor>)> = vec![
            (make_node("shape.rect", &[]), Arc::new(RectProcessor)),
            (make_node("shape.ellipse", &[]), Arc::new(EllipseProcessor)),
            (make_node("shape.polygon", &[]), Arc::new(PolygonProcessor)),
            (make_node("shape.star", &[]), Arc::new(StarProcessor)),
        ];

        for (node, proc) in &shapes {
            let geo = run(node, proc.clone());
            assert!(
                geo.points().get(names::P).is_some(),
                "shape must have P attribute"
            );
            assert!(
                geo.points().get(names::INDEX).is_some(),
                "shape must have index attribute"
            );
        }
    }
}
