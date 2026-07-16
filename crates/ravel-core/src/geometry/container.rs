// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The column-oriented `Geometry` container with four attribute domains.

use std::ops::Range;
use std::sync::Arc;

use super::attribute::{AttrName, AttributeArray, AttributeSet, AttributeType, GeometryError};
use super::names;
use crate::id::DataTypeId;
use crate::types::{GeometricData, NodeData, Rect, Transform2D, Vec2};

/// A primitive built from a contiguous run of point indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Primitive {
    /// A polyline/path over `verts` into the point domain.
    Path { verts: Range<usize>, closed: bool },
}

/// The attribute domain an operation targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    Point,
    Primitive,
    Instance,
    Detail,
}

/// Column-oriented geometry: points, primitives, instances, and detail
/// attributes. Structural sharing via [`AttributeSet`] keeps clones cheap and
/// undo-compatible.
#[derive(Clone, Debug, Default)]
pub struct Geometry {
    points: AttributeSet,
    primitives: Vec<Primitive>,
    primitive_attrs: AttributeSet,
    instances: AttributeSet,
    /// Source geometry stamped by the instance domain, if any.
    instance_source: Option<Arc<Geometry>>,
    detail: AttributeSet,
}

impl Geometry {
    /// An empty geometry with no points, primitives, or instances.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a point cloud carrying the required `P` and stable `index`
    /// standard attributes.
    pub fn from_points(positions: Vec<Vec2>) -> Self {
        let index: Vec<i32> = (0..positions.len() as i32).collect();
        let mut points = AttributeSet::new();
        points
            .insert(names::P, AttributeArray::Vec2(positions))
            .expect("first column cannot mismatch");
        points
            .insert(names::INDEX, AttributeArray::I32(index))
            .expect("index column matches P length");
        Self {
            points,
            ..Self::default()
        }
    }

    /// Validates cross-domain invariants. Called after construction or a
    /// batch of mutations, mirroring the "validate at construction" rule from
    /// the procedural geometry spec.
    pub fn validate(&self) -> Result<(), GeometryError> {
        if let Some(p) = self.points.get(names::P) {
            if p.attr_type() != AttributeType::Vec2 {
                return Err(GeometryError::TypeMismatch {
                    name: names::P.into(),
                    expected: AttributeType::Vec2,
                    actual: p.attr_type(),
                });
            }
        } else if self.point_count() > 0 {
            return Err(GeometryError::AttributeNotFound {
                name: names::P.into(),
            });
        }

        let point_count = self.point_count();
        for prim in &self.primitives {
            let Primitive::Path { verts, .. } = prim;
            if verts.end > point_count || verts.start > verts.end {
                return Err(GeometryError::LengthMismatch {
                    name: names::P.into(),
                    expected: point_count,
                    actual: verts.end,
                });
            }
        }

        if self.primitive_len() != self.primitives.len() && self.primitive_len() != 0 {
            return Err(GeometryError::LengthMismatch {
                name: "primitive attributes".into(),
                expected: self.primitives.len(),
                actual: self.primitive_len(),
            });
        }

        for (name, column) in self.detail.iter() {
            if column.len() != 1 {
                return Err(GeometryError::LengthMismatch {
                    name: name.clone(),
                    expected: 1,
                    actual: column.len(),
                });
            }
        }

        Ok(())
    }

    // ----- Domain access ----------------------------------------------------

    pub fn points(&self) -> &AttributeSet {
        &self.points
    }

    pub fn points_mut(&mut self) -> &mut AttributeSet {
        &mut self.points
    }

    pub fn primitives(&self) -> &[Primitive] {
        &self.primitives
    }

    pub fn push_primitive(&mut self, prim: Primitive) {
        self.primitives.push(prim);
    }

    pub fn primitive_attrs(&self) -> &AttributeSet {
        &self.primitive_attrs
    }

    pub fn primitive_attrs_mut(&mut self) -> &mut AttributeSet {
        &mut self.primitive_attrs
    }

    pub fn instances(&self) -> &AttributeSet {
        &self.instances
    }

    pub fn instances_mut(&mut self) -> &mut AttributeSet {
        &mut self.instances
    }

    pub fn instance_source(&self) -> Option<&Arc<Geometry>> {
        self.instance_source.as_ref()
    }

    pub fn set_instance_source(&mut self, source: Option<Arc<Geometry>>) {
        self.instance_source = source;
    }

    pub fn detail(&self) -> &AttributeSet {
        &self.detail
    }

    pub fn detail_mut(&mut self) -> &mut AttributeSet {
        &mut self.detail
    }

    pub fn attribute_set(&self, domain: Domain) -> &AttributeSet {
        match domain {
            Domain::Point => &self.points,
            Domain::Primitive => &self.primitive_attrs,
            Domain::Instance => &self.instances,
            Domain::Detail => &self.detail,
        }
    }

    // ----- Element counts ----------------------------------------------------

    pub fn point_count(&self) -> usize {
        self.points.element_count()
    }

    pub fn primitive_count(&self) -> usize {
        self.primitives.len()
    }

    pub fn instance_count(&self) -> usize {
        self.instances.element_count()
    }

    fn primitive_len(&self) -> usize {
        self.primitive_attrs.element_count()
    }

    // ----- Summary ------------------------------------------------------------

    /// Debug/properties summary: element counts and attribute listings.
    pub fn summary(&self) -> GeometrySummary {
        GeometrySummary {
            point_count: self.point_count(),
            primitive_count: self.primitive_count(),
            instance_count: self.instance_count(),
            points: self.points.describe(),
            primitives: self.primitive_attrs.describe(),
            instances: self.instances.describe(),
            detail: self.detail.describe(),
        }
    }

    fn positions_bounds(&self) -> Option<Rect> {
        let p = self.points.get(names::P)?;
        let positions = p.as_vec2(names::P).ok()?;
        let (first, rest) = positions.split_first()?;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (first.0, first.1, first.0, first.1);
        for v in rest {
            min_x = min_x.min(v.0);
            min_y = min_y.min(v.1);
            max_x = max_x.max(v.0);
            max_y = max_y.max(v.1);
        }
        Some(Rect {
            x: min_x,
            y: min_y,
            width: max_x - min_x,
            height: max_y - min_y,
        })
    }
}

/// Element counts and per-domain attribute listings for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeometrySummary {
    pub point_count: usize,
    pub primitive_count: usize,
    pub instance_count: usize,
    pub points: Vec<(AttrName, AttributeType)>,
    pub primitives: Vec<(AttrName, AttributeType)>,
    pub instances: Vec<(AttrName, AttributeType)>,
    pub detail: Vec<(AttrName, AttributeType)>,
}

impl NodeData for Geometry {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::GEOMETRY
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl GeometricData for Geometry {
    fn bounds(&self) -> Rect {
        self.positions_bounds().unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        })
    }

    fn transform(&self) -> Transform2D {
        // The container carries no intrinsic transform; placement is an
        // attribute/node concern.
        Transform2D::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_point_geo() -> Geometry {
        Geometry::from_points(vec![Vec2(-1.0, 2.0), Vec2(3.0, -4.0)])
    }

    #[test]
    fn from_points_sets_standard_attributes() {
        let geo = two_point_geo();
        assert_eq!(geo.point_count(), 2);
        assert_eq!(
            geo.points().get(names::P).unwrap().attr_type(),
            AttributeType::Vec2
        );
        assert_eq!(
            geo.points().get(names::INDEX).unwrap().as_i32(names::INDEX),
            Ok(&[0, 1][..])
        );
        assert_eq!(geo.validate(), Ok(()));
    }

    #[test]
    fn custom_attribute_on_point_and_instance_domains() {
        let mut geo = two_point_geo();
        geo.points_mut()
            .insert("heat", AttributeArray::F32(vec![0.5, 1.0]))
            .unwrap();
        geo.instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))
            .unwrap();
        geo.instances_mut()
            .insert(names::ROT, AttributeArray::F32(vec![1.57]))
            .unwrap();
        assert_eq!(geo.validate(), Ok(()));
        assert_eq!(geo.instance_count(), 1);
    }

    #[test]
    fn validate_rejects_wrong_p_type() {
        let mut geo = Geometry::new();
        geo.points_mut()
            .insert(names::P, AttributeArray::F32(vec![1.0]))
            .unwrap();
        assert!(matches!(
            geo.validate(),
            Err(GeometryError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn validate_rejects_out_of_range_primitive() {
        let mut geo = two_point_geo();
        geo.push_primitive(Primitive::Path {
            verts: 0..5,
            closed: false,
        });
        assert!(matches!(
            geo.validate(),
            Err(GeometryError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn validate_rejects_multi_value_detail() {
        let mut geo = two_point_geo();
        geo.detail_mut()
            .insert("comment", AttributeArray::Str(vec!["a".into(), "b".into()]))
            .unwrap();
        assert!(matches!(
            geo.validate(),
            Err(GeometryError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn clone_shares_columns_until_mutation() {
        let original = two_point_geo();
        let mut copy = original.clone();
        assert!(Arc::ptr_eq(
            original.points().get(names::P).unwrap(),
            copy.points().get(names::P).unwrap()
        ));
        copy.points_mut()
            .make_mut(names::P)
            .unwrap()
            .as_vec2_mut(names::P)
            .unwrap()[0] = Vec2(9.0, 9.0);
        assert!(!Arc::ptr_eq(
            original.points().get(names::P).unwrap(),
            copy.points().get(names::P).unwrap()
        ));
        // Untouched column still shared.
        assert!(Arc::ptr_eq(
            original.points().get(names::INDEX).unwrap(),
            copy.points().get(names::INDEX).unwrap()
        ));
    }

    #[test]
    fn geometry_flows_as_node_data() {
        let geo = two_point_geo();
        let data: &dyn NodeData = &geo;
        assert_eq!(data.data_type_id(), DataTypeId::GEOMETRY);
        let roundtrip = data.downcast_ref::<Geometry>().unwrap();
        assert_eq!(roundtrip.point_count(), 2);
    }

    #[test]
    fn bounds_covers_all_points() {
        let geo = two_point_geo();
        let b = geo.bounds();
        assert_eq!((b.x, b.y, b.width, b.height), (-1.0, -4.0, 4.0, 6.0));
    }

    #[test]
    fn summary_lists_counts_and_attributes() {
        let geo = two_point_geo();
        let s = geo.summary();
        assert_eq!(s.point_count, 2);
        assert_eq!(s.primitive_count, 0);
        assert_eq!(s.instance_count, 0);
        assert!(
            s.points
                .iter()
                .any(|(n, t)| n == names::P && *t == AttributeType::Vec2)
        );
        assert!(
            s.points
                .iter()
                .any(|(n, t)| n == names::INDEX && *t == AttributeType::I32)
        );
    }
}
