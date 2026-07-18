// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Geometry-level operations (CPU-only): `geometry.transform` and
//! `geometry.merge`.
//!
//! Operate on whole [`Geometry`] values with copy-on-write attribute
//! columns — untouched columns keep sharing their `Arc` with the input.

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{AttributeArray, AttributeSet, Domain, Geometry, Primitive, names};
use ravel_core::graph::Node;
use ravel_core::types::{Color, NodeData, Vec2, Vec3, Vec4};
use std::sync::Arc;

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

/// `geometry.transform`: scale → rotate → translate around a pivot,
/// applied to the point-domain `P` column and, when instances exist, to
/// the instance placement (`P` transformed, `rot` offset by the rotation,
/// `scale` multiplied component-wise in the instance's local axes).
///
/// `use_centroid` (default on) pivots on the bounding-box center of the
/// point positions (instance positions when there are no points);
/// otherwise `pivot_x` / `pivot_y` is used. Rotation is in degrees.
pub struct GeometryTransformProcessor;

impl GeometryTransformProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for GeometryTransformProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "geometry.transform")?;

        let translate = Vec2(
            params.f32_or("translate_x", 0.0),
            params.f32_or("translate_y", 0.0),
        );
        let rotation = params.f32_or("rotation", 0.0).to_radians();
        let scale = Vec2(params.f32_or("scale_x", 1.0), params.f32_or("scale_y", 1.0));

        if translate == Vec2(0.0, 0.0) && rotation == 0.0 && scale == Vec2(1.0, 1.0) {
            // Identity: share the input wholesale.
            return Ok(inputs[0].as_ref().expect("checked above").clone());
        }

        let pivot = if params.bool_or("use_centroid", true) {
            bounds_center(geometry).unwrap_or(Vec2(0.0, 0.0))
        } else {
            Vec2(params.f32_or("pivot_x", 0.0), params.f32_or("pivot_y", 0.0))
        };

        let (sin_r, cos_r) = rotation.sin_cos();
        let apply = |p: Vec2| -> Vec2 {
            let local = Vec2((p.0 - pivot.0) * scale.0, (p.1 - pivot.1) * scale.1);
            Vec2(
                pivot.0 + translate.0 + cos_r * local.0 - sin_r * local.1,
                pivot.1 + translate.1 + sin_r * local.0 + cos_r * local.1,
            )
        };

        let mut out = geometry.clone();
        if out.points().get(names::P).is_some() {
            for p in out.points_mut().make_mut(names::P)?.as_vec2_mut(names::P)? {
                *p = apply(*p);
            }
        }
        if out.instance_count() > 0 {
            if out.instances().get(names::P).is_some() {
                for p in out
                    .instances_mut()
                    .make_mut(names::P)?
                    .as_vec2_mut(names::P)?
                {
                    *p = apply(*p);
                }
            }
            // Valid instance geometry may omit rot/scale — consumers
            // default them to 0 / (1,1) — so materialize the column from
            // its implicit default before composing.
            let count = out.instance_count();
            if rotation != 0.0 {
                if out.instances().get(names::ROT).is_none() {
                    out.instances_mut()
                        .insert(names::ROT, AttributeArray::F32(vec![0.0; count]))?;
                }
                for r in out
                    .instances_mut()
                    .make_mut(names::ROT)?
                    .as_f32_mut(names::ROT)?
                {
                    *r += rotation;
                }
            }
            if scale != Vec2(1.0, 1.0) {
                if out.instances().get(names::SCALE).is_none() {
                    out.instances_mut().insert(
                        names::SCALE,
                        AttributeArray::Vec2(vec![Vec2(1.0, 1.0); count]),
                    )?;
                }
                for s in out
                    .instances_mut()
                    .make_mut(names::SCALE)?
                    .as_vec2_mut(names::SCALE)?
                {
                    *s = Vec2(s.0 * scale.0, s.1 * scale.1);
                }
            }
        }
        Ok(Arc::new(out))
    }
}

/// `geometry.merge`: concatenates two geometries.
///
/// Points, primitives (vertex ranges re-based onto the combined point
/// list), and instances are appended A-then-B. Attribute columns are the
/// **union** of both sides; a column missing on one side is filled with
/// the typed zero for that side's rows (Houdini semantics). A same-name
/// type conflict is an error. Detail attributes take A wholesale (B's
/// detail only when A has none), and merging two distinct instance
/// sources is unsupported. An unconnected or empty input passes the
/// other side through.
pub struct GeometryMergeProcessor;

impl GeometryMergeProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for GeometryMergeProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let slot = |index: usize| -> Option<&Geometry> {
            inputs
                .get(index)
                .and_then(|input| input.as_ref())
                .and_then(|input| input.downcast_ref::<Geometry>())
        };
        let (a, b) = (slot(0), slot(1));
        let is_empty = |g: &Geometry| {
            g.point_count() == 0
                && g.primitive_count() == 0
                && g.instance_count() == 0
                // A detail-only side still contributes to the merge.
                && g.detail().element_count() == 0
        };
        match (a, b) {
            (None, None) => return Ok(Arc::new(Geometry::new())),
            // One side missing or empty: share the other input wholesale.
            (Some(_), None) | (Some(_), Some(_)) if b.is_none_or(is_empty) => {
                return Ok(inputs[0].as_ref().expect("a present").clone());
            }
            (None, Some(_)) | (Some(_), Some(_)) if a.is_none_or(is_empty) => {
                return Ok(inputs[1].as_ref().expect("b present").clone());
            }
            _ => {}
        }
        let (a, b) = (a.expect("checked"), b.expect("checked"));

        let mut out = Geometry::new();
        // Fill lengths come from the domain's element count, not the
        // attribute set's column length — a side may have primitives (or
        // points/instances) without any attribute columns on that domain.
        *out.points_mut() = concat_attribute_sets(
            a.points(),
            b.points(),
            (a.point_count(), b.point_count()),
            Domain::Point,
        )?;
        *out.primitive_attrs_mut() = concat_attribute_sets(
            a.primitive_attrs(),
            b.primitive_attrs(),
            (a.primitive_count(), b.primitive_count()),
            Domain::Primitive,
        )?;
        *out.instances_mut() = concat_attribute_sets(
            a.instances(),
            b.instances(),
            (a.instance_count(), b.instance_count()),
            Domain::Instance,
        )?;
        // Detail is not a concatenable domain: A wins wholesale.
        *out.detail_mut() = if a.detail().element_count() > 0 {
            a.detail().clone()
        } else {
            b.detail().clone()
        };

        let offset = a.point_count();
        for prim in a.primitives() {
            out.push_primitive(prim.clone());
        }
        for prim in b.primitives() {
            let Primitive::Path { verts, closed } = prim;
            out.push_primitive(Primitive::Path {
                verts: (verts.start + offset)..(verts.end + offset),
                closed: *closed,
            });
        }

        match (a.instance_source(), b.instance_source()) {
            (Some(sa), Some(sb)) if !Arc::ptr_eq(sa, sb) => {
                anyhow::bail!(
                    "geometry.merge: merging two distinct instance sources is unsupported"
                )
            }
            (source_a, source_b) => {
                out.set_instance_source(source_a.or(source_b).cloned());
            }
        }
        Ok(Arc::new(out))
    }
}

/// Concatenates the union of both sides' columns; rows missing on one side
/// are filled with that column type's zero value.
fn concat_attribute_sets(
    a: &AttributeSet,
    b: &AttributeSet,
    (len_a, len_b): (usize, usize),
    domain: Domain,
) -> anyhow::Result<AttributeSet> {
    let mut out = AttributeSet::new();
    let names: Vec<&str> = a
        .iter()
        .map(|(name, _)| name.as_str())
        .chain(
            b.iter()
                .filter(|(name, _)| a.get(name).is_none())
                .map(|(name, _)| name.as_str()),
        )
        .collect();
    for name in names {
        let column = match (a.get(name), b.get(name)) {
            (Some(ca), Some(cb)) if ca.attr_type() != cb.attr_type() => anyhow::bail!(
                "geometry.merge: {domain:?} attribute {name:?} type mismatch ({} vs {})",
                ca.attr_type(),
                cb.attr_type()
            ),
            (ca, cb) => {
                let proto = ca.or(cb).expect("name came from one side");
                concat_columns(
                    ca.map(Arc::as_ref),
                    cb.map(Arc::as_ref),
                    proto,
                    len_a,
                    len_b,
                )
            }
        };
        out.insert(name.to_owned(), column)?;
    }
    Ok(out)
}

/// `a ++ b` with typed-zero fill for a missing side.
fn concat_columns(
    a: Option<&AttributeArray>,
    b: Option<&AttributeArray>,
    proto: &AttributeArray,
    len_a: usize,
    len_b: usize,
) -> AttributeArray {
    macro_rules! concat_as {
        ($variant:ident, $zero:expr) => {{
            let mut merged = match a {
                Some(AttributeArray::$variant(v)) => v.clone(),
                _ => vec![$zero; len_a],
            };
            match b {
                Some(AttributeArray::$variant(v)) => merged.extend(v.iter().cloned()),
                _ => merged.extend(std::iter::repeat_n($zero, len_b)),
            }
            AttributeArray::$variant(merged)
        }};
    }
    match proto {
        AttributeArray::F32(_) => concat_as!(F32, 0.0),
        AttributeArray::Vec2(_) => concat_as!(Vec2, Vec2(0.0, 0.0)),
        AttributeArray::Vec3(_) => concat_as!(Vec3, Vec3(0.0, 0.0, 0.0)),
        AttributeArray::Vec4(_) => concat_as!(Vec4, Vec4(0.0, 0.0, 0.0, 0.0)),
        AttributeArray::Color(_) => concat_as!(Color, Color::TRANSPARENT),
        AttributeArray::I32(_) => concat_as!(I32, 0),
        AttributeArray::Bool(_) => concat_as!(Bool, false),
        AttributeArray::Str(_) => concat_as!(Str, String::new()),
    }
}

/// Bounding-box center of the point positions, falling back to instance
/// positions for instance-only geometry. `None` when both are empty.
fn bounds_center(geometry: &Geometry) -> Option<Vec2> {
    let positions = geometry
        .points()
        .get(names::P)
        .and_then(|c| c.as_vec2(names::P).ok())
        .filter(|p| !p.is_empty())
        .or_else(|| {
            geometry
                .instances()
                .get(names::P)
                .and_then(|c| c.as_vec2(names::P).ok())
                .filter(|p| !p.is_empty())
        })?;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in positions {
        min_x = min_x.min(p.0);
        min_y = min_y.min(p.1);
        max_x = max_x.max(p.0);
        max_y = max_y.max(p.1);
    }
    Some(Vec2((min_x + max_x) * 0.5, (min_y + max_y) * 0.5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (64, 64))
    }

    /// Two points around (2, 0)–(4, 0); bbox center (3, 0).
    fn source_geometry() -> Geometry {
        Geometry::from_points(vec![Vec2(2.0, 0.0), Vec2(4.0, 0.0)])
    }

    /// Source node that always emits the given geometry `Arc`.
    struct Fixed(Arc<Geometry>);
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

    fn eval_transform(params: &[(&str, ParameterValue)], geo: Arc<Geometry>) -> Arc<dyn NodeData> {
        let source =
            Node::new(NodeId::new(1), "test.source").with_output("output", DataTypeId::GEOMETRY);
        let mut node = Node::new(NodeId::new(2), "geometry.transform")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::GEOMETRY);
        for (key, value) in params {
            node = node.with_param(*key, value.clone());
        }
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(node)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(Fixed(geo)));
        ev.register(NodeId::new(2), Arc::new(GeometryTransformProcessor));
        ev.evaluate(&graph, NodeId::new(2), &ctx()).unwrap()
    }

    fn transformed(params: &[(&str, ParameterValue)], geo: Geometry) -> Geometry {
        eval_transform(params, Arc::new(geo))
            .downcast_ref::<Geometry>()
            .unwrap()
            .clone()
    }

    fn point_positions(geo: &Geometry) -> Vec<Vec2> {
        geo.points()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap()
            .to_vec()
    }

    #[test]
    fn translate_moves_points() {
        let out = transformed(
            &[
                ("translate_x", ParameterValue::Float(10.0)),
                ("translate_y", ParameterValue::Float(-5.0)),
            ],
            source_geometry(),
        );
        assert_eq!(
            point_positions(&out),
            vec![Vec2(12.0, -5.0), Vec2(14.0, -5.0)]
        );
    }

    #[test]
    fn rotation_uses_degrees_around_the_centroid() {
        // 90° around bbox center (3, 0): (2,0)→(3,-1), (4,0)→(3,1).
        let out = transformed(
            &[("rotation", ParameterValue::Float(90.0))],
            source_geometry(),
        );
        let pos = point_positions(&out);
        assert!((pos[0].0 - 3.0).abs() < 1e-5 && (pos[0].1 + 1.0).abs() < 1e-5);
        assert!((pos[1].0 - 3.0).abs() < 1e-5 && (pos[1].1 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn scale_applies_before_rotation_around_an_explicit_pivot() {
        // Pivot (0,0), scale x2, rotate 90°, translate (1,0):
        // (2,0) → scale (4,0) → rotate (0,4) → translate (1,4).
        let out = transformed(
            &[
                ("use_centroid", ParameterValue::Bool(false)),
                ("scale_x", ParameterValue::Float(2.0)),
                ("scale_y", ParameterValue::Float(2.0)),
                ("rotation", ParameterValue::Float(90.0)),
                ("translate_x", ParameterValue::Float(1.0)),
            ],
            source_geometry(),
        );
        let pos = point_positions(&out);
        assert!((pos[0].0 - 1.0).abs() < 1e-5 && (pos[0].1 - 4.0).abs() < 1e-5);
        assert!((pos[1].0 - 1.0).abs() < 1e-5 && (pos[1].1 - 8.0).abs() < 1e-5);
    }

    #[test]
    fn instances_compose_placement_rotation_and_scale() {
        let mut geo = Geometry::new();
        geo.instances_mut()
            .insert(names::INDEX, AttributeArray::I32(vec![0]))
            .unwrap();
        geo.instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(2.0, 0.0)]))
            .unwrap();
        geo.instances_mut()
            .insert(names::ROT, AttributeArray::F32(vec![0.5]))
            .unwrap();
        geo.instances_mut()
            .insert(names::SCALE, AttributeArray::Vec2(vec![Vec2(2.0, 3.0)]))
            .unwrap();

        let out = transformed(
            &[
                ("use_centroid", ParameterValue::Bool(false)),
                ("rotation", ParameterValue::Float(90.0)),
                ("scale_x", ParameterValue::Float(2.0)),
                ("scale_y", ParameterValue::Float(2.0)),
            ],
            geo,
        );
        let p = out
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap()[0];
        // (2,0) → scale (4,0) → rotate 90° → (0,4).
        assert!((p.0 - 0.0).abs() < 1e-5 && (p.1 - 4.0).abs() < 1e-5);
        let rot = out
            .instances()
            .get(names::ROT)
            .unwrap()
            .as_f32(names::ROT)
            .unwrap()[0];
        assert!((rot - (0.5 + std::f32::consts::FRAC_PI_2)).abs() < 1e-5);
        let scale = out
            .instances()
            .get(names::SCALE)
            .unwrap()
            .as_vec2(names::SCALE)
            .unwrap()[0];
        assert_eq!(scale, Vec2(4.0, 6.0));
    }

    #[test]
    fn identity_shares_the_input_arc() {
        let input = Arc::new(source_geometry());
        let out = eval_transform(&[], input.clone());
        let out_geo = out.downcast_ref::<Geometry>().unwrap();
        assert!(
            std::ptr::eq(out_geo, input.as_ref()),
            "identity must pass the input Arc through untouched"
        );
    }

    /// Instances without rot/scale columns (valid — consumers default them
    /// to 0 / (1,1)) gain materialized columns so the composition reaches
    /// the nested instance source.
    #[test]
    fn instances_gain_missing_rot_and_scale_columns() {
        let mut geo = Geometry::new();
        geo.instances_mut()
            .insert(names::INDEX, AttributeArray::I32(vec![0, 1]))
            .unwrap();
        geo.instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0)]),
            )
            .unwrap();

        let out = transformed(
            &[
                ("use_centroid", ParameterValue::Bool(false)),
                ("rotation", ParameterValue::Float(90.0)),
                ("scale_x", ParameterValue::Float(2.0)),
                ("scale_y", ParameterValue::Float(3.0)),
            ],
            geo,
        );
        let rot = out
            .instances()
            .get(names::ROT)
            .expect("rot column materialized")
            .as_f32(names::ROT)
            .unwrap()
            .to_vec();
        assert_eq!(rot.len(), 2);
        assert!((rot[0] - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        let scale = out
            .instances()
            .get(names::SCALE)
            .expect("scale column materialized")
            .as_vec2(names::SCALE)
            .unwrap()
            .to_vec();
        assert_eq!(scale, vec![Vec2(2.0, 3.0), Vec2(2.0, 3.0)]);
    }

    #[test]
    fn untouched_columns_keep_structural_sharing() {
        let mut geo = source_geometry();
        geo.points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![1.0, 2.0]))
            .unwrap();
        let input = Arc::new(geo);
        let out = transformed(
            &[("translate_x", ParameterValue::Float(1.0))],
            (*input).clone(),
        );
        // P was rewritten; pscale still shares the input's column.
        let shared = Arc::ptr_eq(
            input.points().get(names::PSCALE).unwrap(),
            out.points().get(names::PSCALE).unwrap(),
        );
        assert!(shared, "pscale column must stay shared");
        assert!(
            !Arc::ptr_eq(
                input.points().get(names::P).unwrap(),
                out.points().get(names::P).unwrap(),
            ),
            "P column must be copied on write"
        );
    }

    fn eval_merge(a: Option<Arc<Geometry>>, b: Option<Arc<Geometry>>) -> Arc<dyn NodeData> {
        let node = Node::new(NodeId::new(3), "geometry.merge")
            .with_input("A", &[DataTypeId::GEOMETRY])
            .with_input("B", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::GEOMETRY);
        let mut graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(3), Arc::new(GeometryMergeProcessor));
        for (slot, geo) in [(0u32, a), (1u32, b)] {
            let Some(geo) = geo else { continue };
            let id = NodeId::new(10 + slot as u64);
            let source = Node::new(id, "test.source").with_output("output", DataTypeId::GEOMETRY);
            graph = graph
                .add_node(source)
                .unwrap()
                .add_edge(
                    EdgeId::new(20 + slot as u64),
                    id,
                    OutputPortIndex(0),
                    NodeId::new(3),
                    InputPortIndex(slot),
                )
                .unwrap();
            ev.register(id, Arc::new(Fixed(geo)));
        }
        ev.evaluate(&graph, NodeId::new(3), &ctx()).unwrap()
    }

    /// Closed unit-square path with a `pscale` column A-side only.
    fn geo_a() -> Geometry {
        let mut geo = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(1.0, 0.0),
            Vec2(1.0, 1.0),
            Vec2(0.0, 1.0),
        ]);
        geo.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        geo.points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![1.0, 2.0, 3.0, 4.0]))
            .unwrap();
        geo
    }

    /// Open two-point path with a `Cd` column B-side only.
    fn geo_b() -> Geometry {
        let mut geo = Geometry::from_points(vec![Vec2(5.0, 5.0), Vec2(6.0, 5.0)]);
        geo.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        geo.points_mut()
            .insert(
                names::CD,
                AttributeArray::Vec3(vec![Vec3(1.0, 0.0, 0.0), Vec3(0.0, 1.0, 0.0)]),
            )
            .unwrap();
        geo
    }

    #[test]
    fn merge_concatenates_points_and_rebases_primitives() {
        let out = eval_merge(Some(Arc::new(geo_a())), Some(Arc::new(geo_b())));
        let geo = out.downcast_ref::<Geometry>().unwrap();
        assert_eq!(geo.point_count(), 6);
        assert_eq!(point_positions(geo)[4], Vec2(5.0, 5.0));
        assert_eq!(geo.primitive_count(), 2);
        let Primitive::Path { verts, closed } = &geo.primitives()[1];
        assert_eq!(*verts, 4..6, "B's vertex range re-based past A's points");
        assert!(!closed);
    }

    #[test]
    fn merge_unions_attributes_with_typed_zero_fill() {
        let out = eval_merge(Some(Arc::new(geo_a())), Some(Arc::new(geo_b())));
        let geo = out.downcast_ref::<Geometry>().unwrap();
        let pscale = geo
            .points()
            .get(names::PSCALE)
            .unwrap()
            .as_f32(names::PSCALE)
            .unwrap();
        assert_eq!(pscale, [1.0, 2.0, 3.0, 4.0, 0.0, 0.0]);
        let cd = geo
            .points()
            .get(names::CD)
            .unwrap()
            .as_vec3(names::CD)
            .unwrap();
        assert_eq!(cd[..4], vec![Vec3(0.0, 0.0, 0.0); 4]);
        assert_eq!(cd[4], Vec3(1.0, 0.0, 0.0));
    }

    #[test]
    fn merge_type_conflict_is_an_error() {
        let mut conflicted = geo_b();
        conflicted
            .points_mut()
            .insert(names::PSCALE, AttributeArray::I32(vec![1, 2]))
            .unwrap();
        let node = Node::new(NodeId::new(3), "geometry.merge")
            .with_input("A", &[DataTypeId::GEOMETRY])
            .with_input("B", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::GEOMETRY);
        let graph = Graph::new()
            .add_node(node)
            .unwrap()
            .add_node(
                Node::new(NodeId::new(10), "test.source")
                    .with_output("output", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(11), "test.source")
                    .with_output("output", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(20),
                NodeId::new(10),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(21),
                NodeId::new(11),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(1),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(3), Arc::new(GeometryMergeProcessor));
        ev.register(NodeId::new(10), Arc::new(Fixed(Arc::new(geo_a()))));
        ev.register(NodeId::new(11), Arc::new(Fixed(Arc::new(conflicted))));
        assert!(ev.evaluate(&graph, NodeId::new(3), &ctx()).is_err());
    }

    /// A side with primitives but no primitive attributes still yields
    /// full-length merged columns (fill length = primitive count, not the
    /// empty attribute set's element count).
    #[test]
    fn merge_fills_primitive_attrs_for_the_attributeless_side() {
        let a = geo_a(); // 1 primitive, no primitive attrs.
        let mut b = geo_b(); // 1 primitive...
        b.primitive_attrs_mut()
            .insert("mat", AttributeArray::I32(vec![7]))
            .unwrap();
        let out = eval_merge(Some(Arc::new(a)), Some(Arc::new(b)));
        let geo = out.downcast_ref::<Geometry>().unwrap();
        assert_eq!(geo.primitive_count(), 2);
        let mat = geo
            .primitive_attrs()
            .get("mat")
            .unwrap()
            .as_i32("mat")
            .unwrap();
        assert_eq!(mat, [0, 7], "A's row zero-filled, B's row appended");
    }

    #[test]
    fn merge_concatenates_instances() {
        let instance_geo = |x: f32| {
            let mut geo = Geometry::new();
            geo.instances_mut()
                .insert(names::INDEX, AttributeArray::I32(vec![0]))
                .unwrap();
            geo.instances_mut()
                .insert(names::P, AttributeArray::Vec2(vec![Vec2(x, 0.0)]))
                .unwrap();
            geo
        };
        let out = eval_merge(
            Some(Arc::new(instance_geo(1.0))),
            Some(Arc::new(instance_geo(2.0))),
        );
        let geo = out.downcast_ref::<Geometry>().unwrap();
        assert_eq!(geo.instance_count(), 2);
        let p = geo
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap();
        assert_eq!(p, [Vec2(1.0, 0.0), Vec2(2.0, 0.0)]);
    }

    /// A detail-only side is not "empty": its detail survives the merge
    /// (A's detail wins; here A is the detail-only side).
    #[test]
    fn merge_keeps_a_detail_only_side() {
        let mut detail_only = Geometry::new();
        detail_only
            .detail_mut()
            .insert("resolution", AttributeArray::Vec2(vec![Vec2(64.0, 64.0)]))
            .unwrap();
        let out = eval_merge(Some(Arc::new(detail_only)), Some(Arc::new(geo_b())));
        let geo = out.downcast_ref::<Geometry>().unwrap();
        assert_eq!(geo.point_count(), 2, "B's points survive");
        let res = geo
            .detail()
            .get("resolution")
            .expect("A's detail survives")
            .as_vec2("resolution")
            .unwrap();
        assert_eq!(res, [Vec2(64.0, 64.0)]);
    }

    #[test]
    fn merge_with_one_side_missing_or_empty_passes_through() {
        let input = Arc::new(geo_a());
        // B unconnected: A's Arc passes through untouched.
        let out = eval_merge(Some(input.clone()), None);
        assert!(std::ptr::eq(
            out.downcast_ref::<Geometry>().unwrap(),
            input.as_ref()
        ));
        // A empty: B passes through.
        let out = eval_merge(Some(Arc::new(Geometry::new())), Some(input.clone()));
        assert!(std::ptr::eq(
            out.downcast_ref::<Geometry>().unwrap(),
            input.as_ref()
        ));
        // Both missing: empty result, no error.
        let out = eval_merge(None, None);
        assert_eq!(out.downcast_ref::<Geometry>().unwrap().point_count(), 0);
    }

    #[test]
    fn missing_input_is_an_error() {
        let node = Node::new(NodeId::new(1), "geometry.transform")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::GEOMETRY);
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(GeometryTransformProcessor));
        assert!(ev.evaluate(&graph, NodeId::new(1), &ctx()).is_err());
    }

    #[test]
    fn is_not_time_dependent() {
        assert!(!GeometryTransformProcessor.is_time_dependent());
    }
}
