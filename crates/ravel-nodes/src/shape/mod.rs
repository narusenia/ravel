// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shape generation node processors.
//!
//! Each processor outputs a [`Geometry`] containing a single closed path
//! primitive built from the shape's point positions.

use std::f32::consts::PI;

use ravel_core::eval::{EvalContext, NodeProcessor};
use ravel_core::geometry::{Geometry, Primitive};
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::types::{NodeData, Vec2};

fn get_float(node: &Node, key: &str, default: f32) -> f32 {
    node.parameters
        .iter()
        .find(|p| p.key == key)
        .and_then(|p| match &p.value {
            ParameterValue::Float(v) => Some(*v),
            _ => None,
        })
        .unwrap_or(default)
}

fn get_int(node: &Node, key: &str, default: i32) -> i32 {
    node.parameters
        .iter()
        .find(|p| p.key == key)
        .and_then(|p| match &p.value {
            ParameterValue::Int(v) => Some(*v),
            _ => None,
        })
        .unwrap_or(default)
}

// ---------------------------------------------------------------------------
// Rectangle
// ---------------------------------------------------------------------------

pub struct RectProcessor {
    center_x: f32,
    center_y: f32,
    width: f32,
    height: f32,
}

impl RectProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            center_x: get_float(node, "center_x", 0.0),
            center_y: get_float(node, "center_y", 0.0),
            width: get_float(node, "width", 100.0),
            height: get_float(node, "height", 100.0),
        }
    }
}

impl NodeProcessor for RectProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        _inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let hw = self.width / 2.0;
        let hh = self.height / 2.0;
        let cx = self.center_x;
        let cy = self.center_y;

        let points = vec![
            Vec2(cx - hw, cy - hh),
            Vec2(cx + hw, cy - hh),
            Vec2(cx + hw, cy + hh),
            Vec2(cx - hw, cy + hh),
        ];

        let mut geo = Geometry::from_points(points);
        geo.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        Ok(Box::new(geo))
    }
}

// ---------------------------------------------------------------------------
// Ellipse (polyline approximation)
// ---------------------------------------------------------------------------

const DEFAULT_ELLIPSE_SEGMENTS: i32 = 32;

pub struct EllipseProcessor {
    center_x: f32,
    center_y: f32,
    radius_x: f32,
    radius_y: f32,
    segments: i32,
}

impl EllipseProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            center_x: get_float(node, "center_x", 0.0),
            center_y: get_float(node, "center_y", 0.0),
            radius_x: get_float(node, "radius_x", 50.0),
            radius_y: get_float(node, "radius_y", 50.0),
            segments: get_int(node, "segments", DEFAULT_ELLIPSE_SEGMENTS),
        }
    }
}

impl NodeProcessor for EllipseProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        _inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let n = self.segments.max(3) as usize;
        let mut points = Vec::with_capacity(n);

        for i in 0..n {
            let angle = 2.0 * PI * i as f32 / n as f32;
            points.push(Vec2(
                self.center_x + self.radius_x * angle.cos(),
                self.center_y + self.radius_y * angle.sin(),
            ));
        }

        let count = points.len();
        let mut geo = Geometry::from_points(points);
        geo.push_primitive(Primitive::Path {
            verts: 0..count,
            closed: true,
        });
        Ok(Box::new(geo))
    }
}

// ---------------------------------------------------------------------------
// Regular polygon
// ---------------------------------------------------------------------------

pub struct PolygonProcessor {
    center_x: f32,
    center_y: f32,
    radius: f32,
    sides: i32,
}

impl PolygonProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            center_x: get_float(node, "center_x", 0.0),
            center_y: get_float(node, "center_y", 0.0),
            radius: get_float(node, "radius", 50.0),
            sides: get_int(node, "sides", 6),
        }
    }
}

impl NodeProcessor for PolygonProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        _inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let n = self.sides.max(3) as usize;
        let mut points = Vec::with_capacity(n);

        let offset = -PI / 2.0;
        for i in 0..n {
            let angle = offset + 2.0 * PI * i as f32 / n as f32;
            points.push(Vec2(
                self.center_x + self.radius * angle.cos(),
                self.center_y + self.radius * angle.sin(),
            ));
        }

        let count = points.len();
        let mut geo = Geometry::from_points(points);
        geo.push_primitive(Primitive::Path {
            verts: 0..count,
            closed: true,
        });
        Ok(Box::new(geo))
    }
}

// ---------------------------------------------------------------------------
// Star
// ---------------------------------------------------------------------------

pub struct StarProcessor {
    center_x: f32,
    center_y: f32,
    outer_radius: f32,
    inner_radius: f32,
    points: i32,
}

impl StarProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            center_x: get_float(node, "center_x", 0.0),
            center_y: get_float(node, "center_y", 0.0),
            outer_radius: get_float(node, "outer_radius", 50.0),
            inner_radius: get_float(node, "inner_radius", 25.0),
            points: get_int(node, "points", 5),
        }
    }
}

impl NodeProcessor for StarProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        _inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let n = self.points.max(3) as usize;
        let total = n * 2;
        let mut points = Vec::with_capacity(total);

        let offset = -PI / 2.0;
        for i in 0..total {
            let angle = offset + 2.0 * PI * i as f32 / total as f32;
            let radius = if i % 2 == 0 {
                self.outer_radius
            } else {
                self.inner_radius
            };
            points.push(Vec2(
                self.center_x + radius * angle.cos(),
                self.center_y + radius * angle.sin(),
            ));
        }

        let count = points.len();
        let mut geo = Geometry::from_points(points);
        geo.push_primitive(Primitive::Path {
            verts: 0..count,
            closed: true,
        });
        Ok(Box::new(geo))
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
        _ctx: &EvalContext,
        _inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        Ok(Box::new(Geometry::new()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::geometry::names;
    use ravel_core::id::NodeId;
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (100, 100))
    }

    fn run(proc: &dyn NodeProcessor) -> Geometry {
        let out = proc.process(&ctx(), &[]).unwrap();
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
        let geo = run(&RectProcessor::from_node(&node));
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
        let geo = run(&RectProcessor::from_node(&node));
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
        let geo = run(&EllipseProcessor::from_node(&node));
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
        let geo = run(&EllipseProcessor::from_node(&node));
        let (x, y, w, h) = bounds_of(&geo);
        assert!((x + 30.0).abs() < 0.5);
        assert!((y + 20.0).abs() < 0.5);
        assert!((w - 60.0).abs() < 1.0);
        assert!((h - 40.0).abs() < 1.0);
    }

    #[test]
    fn ellipse_clamps_minimum_segments() {
        let node = make_node("shape.ellipse", &[("segments", ParameterValue::Int(1))]);
        let geo = run(&EllipseProcessor::from_node(&node));
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
        let geo = run(&PolygonProcessor::from_node(&node));
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
        let geo = run(&PolygonProcessor::from_node(&node));
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
        let geo = run(&StarProcessor::from_node(&node));
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
        let geo = run(&StarProcessor::from_node(&node));
        let (x, y, w, h) = bounds_of(&geo);
        assert!(x >= -60.0 - 1e-4 && y >= -60.0 - 1e-4);
        assert!(w <= 120.0 + 1e-3 && h <= 120.0 + 1e-3);
    }

    // -- Custom path --------------------------------------------------------

    #[test]
    fn custom_path_returns_empty_geometry() {
        let node = make_node("shape.custom_path", &[]);
        let geo = run(&CustomPathProcessor::from_node(&node));
        assert_eq!(geo.point_count(), 0);
        assert_eq!(geo.primitive_count(), 0);
        assert!(geo.validate().is_ok());
    }

    // -- P attribute present ------------------------------------------------

    #[test]
    fn all_shapes_have_p_attribute() {
        let shapes: Vec<Box<dyn NodeProcessor>> = vec![
            Box::new(RectProcessor::from_node(&make_node("shape.rect", &[]))),
            Box::new(EllipseProcessor::from_node(&make_node(
                "shape.ellipse",
                &[],
            ))),
            Box::new(PolygonProcessor::from_node(&make_node(
                "shape.polygon",
                &[],
            ))),
            Box::new(StarProcessor::from_node(&make_node("shape.star", &[]))),
        ];

        for proc in &shapes {
            let geo = run(proc.as_ref());
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
