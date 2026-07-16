// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Instance duplication node processors.
//!
//! Each processor takes an optional source [`Geometry`] input and produces a
//! new [`Geometry`] whose instance domain carries `index`, `P`, `rot`, and
//! `scale` for every copy.  The source geometry is set as `instance_source`
//! so the rasterizer stamps it at each instance position.

use std::f32::consts::PI;
use std::sync::Arc;

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, NodeProcessor};
use ravel_core::geometry::{AttributeArray, Geometry, names};
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

fn populate_instances(geo: &mut Geometry, positions: Vec<Vec2>, rotations: Vec<f32>) {
    let n = positions.len();
    let indices: Vec<i32> = (0..n as i32).collect();
    let scales = vec![Vec2(1.0, 1.0); n];

    geo.instances_mut()
        .insert(names::INDEX, AttributeArray::I32(indices))
        .expect("first column");
    geo.instances_mut()
        .insert(names::P, AttributeArray::Vec2(positions))
        .expect("same length as index");
    geo.instances_mut()
        .insert(names::ROT, AttributeArray::F32(rotations))
        .expect("same length");
    geo.instances_mut()
        .insert(names::SCALE, AttributeArray::Vec2(scales))
        .expect("same length");
}

// ---------------------------------------------------------------------------
// Grid
// ---------------------------------------------------------------------------

pub struct GridProcessor {
    count_x: i32,
    count_y: i32,
    spacing_x: f32,
    spacing_y: f32,
    center_x: f32,
    center_y: f32,
}

impl GridProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            count_x: get_int(node, "count_x", 5),
            count_y: get_int(node, "count_y", 5),
            spacing_x: get_float(node, "spacing_x", 20.0),
            spacing_y: get_float(node, "spacing_y", 20.0),
            center_x: get_float(node, "center_x", 0.0),
            center_y: get_float(node, "center_y", 0.0),
        }
    }
}

impl NodeProcessor for GridProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let source = inputs.first().and_then(|d| d.downcast_ref::<Geometry>());

        let nx = self.count_x.max(1) as usize;
        let ny = self.count_y.max(1) as usize;
        let total_w = (nx as f32 - 1.0) * self.spacing_x;
        let total_h = (ny as f32 - 1.0) * self.spacing_y;
        let origin_x = self.center_x - total_w / 2.0;
        let origin_y = self.center_y - total_h / 2.0;

        let n = nx * ny;
        let mut positions = Vec::with_capacity(n);
        let rotations = vec![0.0; n];

        for iy in 0..ny {
            for ix in 0..nx {
                positions.push(Vec2(
                    origin_x + ix as f32 * self.spacing_x,
                    origin_y + iy as f32 * self.spacing_y,
                ));
            }
        }

        let mut geo = Geometry::new();
        if let Some(src) = source {
            geo.set_instance_source(Some(Arc::new(src.clone())));
        }
        populate_instances(&mut geo, positions, rotations);
        Ok(Box::new(geo))
    }
}

// ---------------------------------------------------------------------------
// Circular
// ---------------------------------------------------------------------------

pub struct CircularProcessor {
    count: i32,
    radius: f32,
    center_x: f32,
    center_y: f32,
    align_rotation: bool,
}

impl CircularProcessor {
    pub fn from_node(node: &Node) -> Self {
        let align = node
            .parameters
            .iter()
            .find(|p| p.key == "align_rotation")
            .and_then(|p| match &p.value {
                ParameterValue::Bool(v) => Some(*v),
                _ => None,
            })
            .unwrap_or(true);

        Self {
            count: get_int(node, "count", 8),
            radius: get_float(node, "radius", 50.0),
            center_x: get_float(node, "center_x", 0.0),
            center_y: get_float(node, "center_y", 0.0),
            align_rotation: align,
        }
    }
}

impl NodeProcessor for CircularProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let source = inputs.first().and_then(|d| d.downcast_ref::<Geometry>());

        let n = self.count.max(1) as usize;
        let mut positions = Vec::with_capacity(n);
        let mut rotations = Vec::with_capacity(n);

        for i in 0..n {
            let angle = 2.0 * PI * i as f32 / n as f32;
            positions.push(Vec2(
                self.center_x + self.radius * angle.cos(),
                self.center_y + self.radius * angle.sin(),
            ));
            rotations.push(if self.align_rotation { angle } else { 0.0 });
        }

        let mut geo = Geometry::new();
        if let Some(src) = source {
            geo.set_instance_source(Some(Arc::new(src.clone())));
        }
        populate_instances(&mut geo, positions, rotations);
        Ok(Box::new(geo))
    }
}

// ---------------------------------------------------------------------------
// Path array — instances along a path, rot from tangent
// ---------------------------------------------------------------------------

pub struct PathArrayProcessor {
    count: i32,
}

impl PathArrayProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            count: get_int(node, "count", 10),
        }
    }
}

impl NodeProcessor for PathArrayProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let path_geo = inputs
            .first()
            .and_then(|d| d.downcast_ref::<Geometry>())
            .context("scatter.path_array expects a path Geometry on input 0")?;

        let source = inputs.get(1).and_then(|d| d.downcast_ref::<Geometry>());

        let positions_col = path_geo
            .points()
            .get(names::P)
            .context("path geometry missing P")?;
        let path_points = positions_col.as_vec2(names::P)?;

        if path_points.len() < 2 {
            return Ok(Box::new(Geometry::new()));
        }

        let segments = cumulative_arc_lengths(path_points);
        let total_len = *segments.last().unwrap();
        if total_len < 1e-9 {
            return Ok(Box::new(Geometry::new()));
        }

        let n = self.count.max(1) as usize;
        let mut positions = Vec::with_capacity(n);
        let mut rotations = Vec::with_capacity(n);

        for i in 0..n {
            let t = if n > 1 {
                i as f32 / (n - 1) as f32
            } else {
                0.0
            };
            let target_len = t * total_len;

            let seg_idx = segments
                .partition_point(|&s| s < target_len)
                .min(segments.len() - 1)
                .max(1);
            let seg_start = segments[seg_idx - 1];
            let seg_end = segments[seg_idx];
            let seg_t = if (seg_end - seg_start).abs() > 1e-9 {
                (target_len - seg_start) / (seg_end - seg_start)
            } else {
                0.0
            };

            let a = path_points[seg_idx - 1];
            let b = path_points[seg_idx];
            let pos = Vec2(a.0 + (b.0 - a.0) * seg_t, a.1 + (b.1 - a.1) * seg_t);
            let tangent = Vec2(b.0 - a.0, b.1 - a.1);
            let rot = tangent.1.atan2(tangent.0);

            positions.push(pos);
            rotations.push(rot);
        }

        let mut geo = Geometry::new();
        if let Some(src) = source {
            geo.set_instance_source(Some(Arc::new(src.clone())));
        }
        populate_instances(&mut geo, positions, rotations);
        Ok(Box::new(geo))
    }
}

fn cumulative_arc_lengths(points: &[Vec2]) -> Vec<f32> {
    let mut lengths = Vec::with_capacity(points.len());
    lengths.push(0.0);
    for i in 1..points.len() {
        let dx = points[i].0 - points[i - 1].0;
        let dy = points[i].1 - points[i - 1].1;
        lengths.push(lengths[i - 1] + (dx * dx + dy * dy).sqrt());
    }
    lengths
}

// ---------------------------------------------------------------------------
// Scatter — deterministic random placement
// ---------------------------------------------------------------------------

pub struct ScatterProcessor {
    count: i32,
    area_x: f32,
    area_y: f32,
    center_x: f32,
    center_y: f32,
    seed: u32,
}

impl ScatterProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            count: get_int(node, "count", 20),
            area_x: get_float(node, "area_x", 200.0),
            area_y: get_float(node, "area_y", 200.0),
            center_x: get_float(node, "center_x", 0.0),
            center_y: get_float(node, "center_y", 0.0),
            seed: get_int(node, "seed", 0) as u32,
        }
    }
}

impl NodeProcessor for ScatterProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let source = inputs.first().and_then(|d| d.downcast_ref::<Geometry>());

        let n = self.count.max(0) as usize;
        let mut positions = Vec::with_capacity(n);
        let mut rotations = Vec::with_capacity(n);

        let half_x = self.area_x / 2.0;
        let half_y = self.area_y / 2.0;

        for i in 0..n {
            let h = hash(self.seed, i as u32);
            let rx = hash_to_f32(h);
            let ry = hash_to_f32(hash(h, 1));
            let rr = hash_to_f32(hash(h, 2));

            positions.push(Vec2(
                self.center_x + (rx * 2.0 - 1.0) * half_x,
                self.center_y + (ry * 2.0 - 1.0) * half_y,
            ));
            rotations.push(rr * 2.0 * PI);
        }

        let mut geo = Geometry::new();
        if let Some(src) = source {
            geo.set_instance_source(Some(Arc::new(src.clone())));
        }
        populate_instances(&mut geo, positions, rotations);
        Ok(Box::new(geo))
    }
}

/// Deterministic hash (Wang hash variant).
fn hash(seed: u32, index: u32) -> u32 {
    let mut h = seed.wrapping_mul(0x9E37_79B9).wrapping_add(index);
    h = (h ^ (h >> 16)).wrapping_mul(0x045D_9F3B);
    h = (h ^ (h >> 16)).wrapping_mul(0x045D_9F3B);
    h ^ (h >> 16)
}

fn hash_to_f32(h: u32) -> f32 {
    (h & 0x00FF_FFFF) as f32 / 0x0100_0000 as f32
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::geometry::Primitive;
    use ravel_core::id::NodeId;
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (100, 100))
    }

    fn make_node(type_key: &str, params: &[(&str, ParameterValue)]) -> Node {
        let mut node = Node::new(NodeId::new(1), type_key);
        for (key, value) in params {
            node = node.with_param(*key, value.clone());
        }
        node
    }

    fn small_square() -> Geometry {
        let mut geo = Geometry::from_points(vec![
            Vec2(-5.0, -5.0),
            Vec2(5.0, -5.0),
            Vec2(5.0, 5.0),
            Vec2(-5.0, 5.0),
        ]);
        geo.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        geo
    }

    fn line_path() -> Geometry {
        let mut geo = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(100.0, 0.0)]);
        geo.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        geo
    }

    // -- Grid ---------------------------------------------------------------

    #[test]
    fn grid_instance_count() {
        let node = make_node(
            "scatter.grid",
            &[
                ("count_x", ParameterValue::Int(3)),
                ("count_y", ParameterValue::Int(4)),
            ],
        );
        let proc = GridProcessor::from_node(&node);
        let src = small_square();
        let refs: Vec<&dyn NodeData> = vec![&src];
        let out = proc.process(&ctx(), &refs).unwrap();
        let geo = out.downcast_ref::<Geometry>().unwrap();

        assert_eq!(geo.instance_count(), 12);
        assert!(geo.instance_source().is_some());
        assert!(geo.instances().get(names::INDEX).is_some());
        assert!(geo.instances().get(names::P).is_some());
        assert!(geo.instances().get(names::ROT).is_some());
        assert!(geo.instances().get(names::SCALE).is_some());
    }

    #[test]
    fn grid_no_source_still_creates_instances() {
        let node = make_node(
            "scatter.grid",
            &[
                ("count_x", ParameterValue::Int(2)),
                ("count_y", ParameterValue::Int(2)),
            ],
        );
        let proc = GridProcessor::from_node(&node);
        let out = proc.process(&ctx(), &[]).unwrap();
        let geo = out.downcast_ref::<Geometry>().unwrap();

        assert_eq!(geo.instance_count(), 4);
        assert!(geo.instance_source().is_none());
    }

    // -- Circular -----------------------------------------------------------

    #[test]
    fn circular_instance_count_and_rotation() {
        let node = make_node(
            "scatter.circular",
            &[
                ("count", ParameterValue::Int(6)),
                ("radius", ParameterValue::Float(50.0)),
                ("align_rotation", ParameterValue::Bool(true)),
            ],
        );
        let proc = CircularProcessor::from_node(&node);
        let src = small_square();
        let refs: Vec<&dyn NodeData> = vec![&src];
        let out = proc.process(&ctx(), &refs).unwrap();
        let geo = out.downcast_ref::<Geometry>().unwrap();

        assert_eq!(geo.instance_count(), 6);

        let rots = geo
            .instances()
            .get(names::ROT)
            .unwrap()
            .as_f32(names::ROT)
            .unwrap();
        assert!((rots[0] - 0.0).abs() < 1e-5);
        assert!((rots[1] - PI / 3.0).abs() < 1e-4);
    }

    // -- Path array ---------------------------------------------------------

    #[test]
    fn path_array_distributes_along_path() {
        let node = make_node("scatter.path_array", &[("count", ParameterValue::Int(5))]);
        let proc = PathArrayProcessor::from_node(&node);

        let path = line_path();
        let src = small_square();
        let refs: Vec<&dyn NodeData> = vec![&path, &src];
        let out = proc.process(&ctx(), &refs).unwrap();
        let geo = out.downcast_ref::<Geometry>().unwrap();

        assert_eq!(geo.instance_count(), 5);

        let positions = geo
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap();
        // First at start, last at end of the 100px line
        assert!((positions[0].0 - 0.0).abs() < 1e-4);
        assert!((positions[4].0 - 100.0).abs() < 1e-4);

        let rots = geo
            .instances()
            .get(names::ROT)
            .unwrap()
            .as_f32(names::ROT)
            .unwrap();
        // Horizontal path → rot ≈ 0
        for r in rots {
            assert!(r.abs() < 1e-4, "rotation along horizontal path: {r}");
        }
    }

    #[test]
    fn path_array_tangent_rotation() {
        let node = make_node("scatter.path_array", &[("count", ParameterValue::Int(3))]);
        let proc = PathArrayProcessor::from_node(&node);

        // Vertical path: rotation should be PI/2
        let mut path = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(0.0, 100.0)]);
        path.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });

        let refs: Vec<&dyn NodeData> = vec![&path];
        let out = proc.process(&ctx(), &refs).unwrap();
        let geo = out.downcast_ref::<Geometry>().unwrap();

        let rots = geo
            .instances()
            .get(names::ROT)
            .unwrap()
            .as_f32(names::ROT)
            .unwrap();
        for r in rots {
            assert!((r - PI / 2.0).abs() < 1e-4, "vertical path tangent: {r}");
        }
    }

    // -- Scatter ------------------------------------------------------------

    #[test]
    fn scatter_deterministic_with_same_seed() {
        let node = make_node(
            "scatter.scatter",
            &[
                ("count", ParameterValue::Int(10)),
                ("seed", ParameterValue::Int(42)),
            ],
        );
        let proc = ScatterProcessor::from_node(&node);

        let out1 = proc.process(&ctx(), &[]).unwrap();
        let out2 = proc.process(&ctx(), &[]).unwrap();

        let g1 = out1.downcast_ref::<Geometry>().unwrap();
        let g2 = out2.downcast_ref::<Geometry>().unwrap();

        let p1 = g1.instances().get(names::P).unwrap();
        let p2 = g2.instances().get(names::P).unwrap();
        assert_eq!(p1.as_vec2(names::P).unwrap(), p2.as_vec2(names::P).unwrap());
    }

    #[test]
    fn scatter_different_seed_produces_different_positions() {
        let node_a = make_node(
            "scatter.scatter",
            &[
                ("count", ParameterValue::Int(10)),
                ("seed", ParameterValue::Int(1)),
            ],
        );
        let node_b = make_node(
            "scatter.scatter",
            &[
                ("count", ParameterValue::Int(10)),
                ("seed", ParameterValue::Int(2)),
            ],
        );

        let out_a = ScatterProcessor::from_node(&node_a)
            .process(&ctx(), &[])
            .unwrap();
        let out_b = ScatterProcessor::from_node(&node_b)
            .process(&ctx(), &[])
            .unwrap();

        let ga = out_a.downcast_ref::<Geometry>().unwrap();
        let gb = out_b.downcast_ref::<Geometry>().unwrap();

        let pa = ga
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap();
        let pb = gb
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap();
        assert_ne!(pa, pb);
    }

    #[test]
    fn scatter_instance_count() {
        let node = make_node("scatter.scatter", &[("count", ParameterValue::Int(15))]);
        let proc = ScatterProcessor::from_node(&node);
        let src = small_square();
        let refs: Vec<&dyn NodeData> = vec![&src];
        let out = proc.process(&ctx(), &refs).unwrap();
        let geo = out.downcast_ref::<Geometry>().unwrap();

        assert_eq!(geo.instance_count(), 15);
        assert!(geo.instance_source().is_some());
    }

    // -- All scatter nodes have required attributes -------------------------

    #[test]
    #[allow(clippy::type_complexity)]
    fn all_scatter_nodes_populate_required_attrs() {
        let src = small_square();
        let path = line_path();

        let cases: Vec<(&str, Box<dyn NodeProcessor>, Vec<&dyn NodeData>)> = vec![
            (
                "grid",
                Box::new(GridProcessor::from_node(&make_node("scatter.grid", &[]))),
                vec![&src as &dyn NodeData],
            ),
            (
                "circular",
                Box::new(CircularProcessor::from_node(&make_node(
                    "scatter.circular",
                    &[],
                ))),
                vec![&src],
            ),
            (
                "path_array",
                Box::new(PathArrayProcessor::from_node(&make_node(
                    "scatter.path_array",
                    &[],
                ))),
                vec![&path as &dyn NodeData, &src],
            ),
            (
                "scatter",
                Box::new(ScatterProcessor::from_node(&make_node(
                    "scatter.scatter",
                    &[],
                ))),
                vec![&src],
            ),
        ];

        for (name, proc, inputs) in &cases {
            let out = proc.process(&ctx(), inputs).unwrap();
            let geo = out.downcast_ref::<Geometry>().unwrap();
            assert!(
                geo.instances().get(names::INDEX).is_some(),
                "{name} missing index"
            );
            assert!(geo.instances().get(names::P).is_some(), "{name} missing P");
            assert!(
                geo.instances().get(names::ROT).is_some(),
                "{name} missing rot"
            );
            assert!(
                geo.instances().get(names::SCALE).is_some(),
                "{name} missing scale"
            );
        }
    }
}
