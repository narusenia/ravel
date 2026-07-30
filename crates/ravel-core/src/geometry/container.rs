// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The column-oriented `Geometry` container with four attribute domains.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use super::attribute::{AttrName, AttributeArray, AttributeSet, AttributeType, GeometryError};
use super::names;
use crate::id::DataTypeId;
use crate::types::{GeometricData, NodeData, Rect, Transform2D, Vec2, Vec3};

/// A primitive built from a contiguous run of point indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Primitive {
    /// A polyline/path over `verts` into the point domain.
    Path { verts: Range<usize>, closed: bool },
}

/// The `P` column of one attribute domain, read at whichever dimension the
/// geometry carries: `Vec2` in 2D, `Vec3` in 3D (REQ-3D-003).
///
/// Every node that reads positions goes through this instead of
/// `as_vec2(names::P)`, so "handled / dimension-agnostic / explicit error" is
/// a choice each call site has to make rather than a panic or a silent empty
/// result. The classification is the position dimension table in
/// `docs/specifications/procedural-geometry.md`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Positions<'a> {
    D2(&'a [Vec2]),
    D3(&'a [Vec3]),
}

impl<'a> Positions<'a> {
    /// Reads a position column, rejecting every type that is not a position.
    pub fn from_column(column: &'a AttributeArray) -> Result<Self, GeometryError> {
        match column {
            AttributeArray::Vec2(values) => Ok(Self::D2(values)),
            AttributeArray::Vec3(values) => Ok(Self::D3(values)),
            other => Err(GeometryError::PositionTypeMismatch {
                name: names::P.into(),
                actual: other.attr_type(),
            }),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::D2(values) => values.len(),
            Self::D3(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn attr_type(&self) -> AttributeType {
        match self {
            Self::D2(_) => AttributeType::Vec2,
            Self::D3(_) => AttributeType::Vec3,
        }
    }

    /// Number of components, 2 or 3.
    pub fn dimension(&self) -> usize {
        match self {
            Self::D2(_) => 2,
            Self::D3(_) => 3,
        }
    }

    /// The borrowed 2D slice, or `None` for 3D positions. The zero-copy fast
    /// path every existing 2D consumer keeps taking.
    pub fn planar(&self) -> Option<&'a [Vec2]> {
        match self {
            Self::D2(values) => Some(values),
            Self::D3(_) => None,
        }
    }

    /// The 2D slice, or an error naming `operation` when the geometry is 3D.
    /// Used by operations whose definition is planar (arc length, the
    /// analytic rasterizer).
    pub fn require_planar(&self, operation: &'static str) -> Result<&'a [Vec2], GeometryError> {
        self.planar().ok_or_else(|| GeometryError::RequiresPlanarP {
            operation,
            name: names::P.into(),
            actual: self.attr_type(),
        })
    }

    /// The xy projection: borrowed in 2D, materialized in 3D. Only for
    /// consumers that are documented as planar-by-construction (field
    /// sampling); anything whose result would silently lose meaning must use
    /// [`Self::require_planar`] instead.
    pub fn projected(&self) -> Cow<'a, [Vec2]> {
        match self {
            Self::D2(values) => Cow::Borrowed(values),
            Self::D3(values) => Cow::Owned(values.iter().map(|v| Vec2(v.0, v.1)).collect()),
        }
    }

    /// Position `index` as a 3D vector; 2D positions read back with `z = 0`.
    pub fn get3(&self, index: usize) -> Option<Vec3> {
        match self {
            Self::D2(values) => values.get(index).map(|v| Vec3(v.0, v.1, 0.0)),
            Self::D3(values) => values.get(index).copied(),
        }
    }

    /// Every position as a 3D vector; 2D positions read back with `z = 0`.
    /// The extra zero term is exact in binary floating point, so a 2D input
    /// produces bit-identical arithmetic.
    pub fn iter3(&self) -> Box<dyn Iterator<Item = Vec3> + 'a> {
        match *self {
            Self::D2(values) => Box::new(values.iter().map(|v| Vec3(v.0, v.1, 0.0))),
            Self::D3(values) => Box::new(values.iter().copied()),
        }
    }
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
    /// Source geometries stamped by the instance domain.
    instance_sources: Vec<Arc<Geometry>>,
    detail: AttributeSet,
}

impl Geometry {
    /// An empty geometry with no points, primitives, or instances.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a 2D point cloud carrying the required `P` and stable `index`
    /// standard attributes.
    pub fn from_points(positions: Vec<Vec2>) -> Self {
        Self::from_position_column(AttributeArray::Vec2(positions))
    }

    /// Builds a 3D point cloud (REQ-3D-003). The `P` column is `Vec3`; every
    /// other standard attribute is identical to [`Self::from_points`].
    pub fn from_points3(positions: Vec<Vec3>) -> Self {
        Self::from_position_column(AttributeArray::Vec3(positions))
    }

    fn from_position_column(positions: AttributeArray) -> Self {
        let index: Vec<i32> = (0..positions.len() as i32).collect();
        let mut points = AttributeSet::new();
        points
            .insert(names::P, positions)
            .expect("first column cannot mismatch");
        points
            .insert(names::INDEX, AttributeArray::I32(index))
            .expect("index column matches P length");
        Self {
            points,
            ..Self::default()
        }
    }

    /// The `P` column of `domain`, or `None` when the domain has no `P`.
    ///
    /// `P` may be `Vec2` or `Vec3` and the dimension is chosen per domain, so
    /// a 2D instance source placed by 3D instances is a valid geometry
    /// (REQ-3D-003).
    pub fn positions(&self, domain: Domain) -> Option<Result<Positions<'_>, GeometryError>> {
        self.attribute_set(domain)
            .get(names::P)
            .map(|column| Positions::from_column(column))
    }

    /// Validates cross-domain invariants. Called after construction or a
    /// batch of mutations, mirroring the "validate at construction" rule from
    /// the procedural geometry spec.
    pub fn validate(&self) -> Result<(), GeometryError> {
        // `P` is a position column in either dimension; the choice is made per
        // domain, and a column is homogeneous by construction, so nothing
        // else is needed to keep the dimension consistent inside a domain.
        for domain in [Domain::Point, Domain::Instance] {
            if let Some(positions) = self.positions(domain) {
                positions?;
            }
        }
        if self.points.get(names::P).is_none() && self.point_count() > 0 {
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
        self.instance_sources.first()
    }

    pub fn set_instance_source(&mut self, source: Option<Arc<Geometry>>) {
        self.instance_sources = source.into_iter().collect();
    }

    /// Source geometries available to the instance domain.
    pub fn instance_sources(&self) -> &[Arc<Geometry>] {
        &self.instance_sources
    }

    /// Replaces the source geometries available to the instance domain.
    pub fn set_instance_sources(&mut self, sources: Vec<Arc<Geometry>>) {
        self.instance_sources = sources;
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

    pub(crate) fn attribute_set_mut(&mut self, domain: Domain) -> &mut AttributeSet {
        match domain {
            Domain::Point => &mut self.points,
            Domain::Primitive => &mut self.primitive_attrs,
            Domain::Instance => &mut self.instances,
            Domain::Detail => &mut self.detail,
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

    /// The xy extent of the point positions. `Rect` is a 2D value, so a 3D
    /// geometry reports the extent of its projection — depth is a
    /// `scene.render` concern, not a container one.
    fn positions_bounds(&self) -> Option<Rect> {
        let positions = self.positions(Domain::Point)?.ok()?;
        let mut components = positions.iter3();
        let first = components.next()?;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (first.0, first.1, first.0, first.1);
        for v in components {
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
    fn single_instance_source_api_wraps_first_plural_source() {
        let first = Arc::new(Geometry::from_points(vec![Vec2(1.0, 0.0)]));
        let second = Arc::new(Geometry::from_points(vec![Vec2(2.0, 0.0)]));
        let mut geo = Geometry::new();

        geo.set_instance_sources(vec![first.clone(), second.clone()]);
        assert_eq!(geo.instance_sources().len(), 2);
        assert!(Arc::ptr_eq(geo.instance_source().unwrap(), &first));

        geo.set_instance_source(Some(second.clone()));
        assert_eq!(geo.instance_sources().len(), 1);
        assert!(Arc::ptr_eq(geo.instance_source().unwrap(), &second));

        geo.set_instance_source(None);
        assert!(geo.instance_sources().is_empty());
        assert!(geo.instance_source().is_none());
    }

    #[test]
    fn validate_rejects_wrong_p_type() {
        for domain in [Domain::Point, Domain::Instance] {
            let mut geo = Geometry::new();
            geo.attribute_set_mut(domain)
                .insert(names::P, AttributeArray::F32(vec![1.0]))
                .unwrap();
            assert!(matches!(
                geo.validate(),
                Err(GeometryError::PositionTypeMismatch { .. })
            ));
        }
    }

    /// `P` is a position column in either dimension, chosen per domain
    /// (REQ-3D-003): 3D points, and 2D points placed by 3D instances, are both
    /// valid geometry.
    #[test]
    fn validate_accepts_three_dimensional_positions() {
        let geo = Geometry::from_points3(vec![Vec3(1.0, 2.0, 3.0), Vec3(-4.0, 5.0, -6.0)]);
        assert_eq!(geo.validate(), Ok(()));
        assert_eq!(geo.point_count(), 2);
        assert_eq!(
            geo.points().get(names::INDEX).unwrap().as_i32(names::INDEX),
            Ok(&[0, 1][..])
        );

        let mut mixed = Geometry::from_points(vec![Vec2(0.0, 0.0)]);
        mixed
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec3(vec![Vec3(0.0, 0.0, 1.0), Vec3(0.0, 0.0, 2.0)]),
            )
            .unwrap();
        assert_eq!(mixed.validate(), Ok(()));
        assert_eq!(
            mixed.positions(Domain::Point).unwrap().unwrap().dimension(),
            2
        );
        assert_eq!(
            mixed
                .positions(Domain::Instance)
                .unwrap()
                .unwrap()
                .dimension(),
            3
        );
        assert!(mixed.positions(Domain::Detail).is_none());
    }

    #[test]
    fn positions_read_back_as_three_components_in_either_dimension() {
        let planar = Geometry::from_points(vec![Vec2(1.0, 2.0)]);
        let flat = planar.positions(Domain::Point).unwrap().unwrap();
        assert_eq!(flat.planar(), Some(&[Vec2(1.0, 2.0)][..]));
        assert_eq!(flat.get3(0), Some(Vec3(1.0, 2.0, 0.0)));
        assert_eq!(flat.require_planar("test").unwrap(), &[Vec2(1.0, 2.0)]);
        assert!(matches!(flat.projected(), std::borrow::Cow::Borrowed(_)));

        let spatial = Geometry::from_points3(vec![Vec3(1.0, 2.0, 3.0)]);
        let deep = spatial.positions(Domain::Point).unwrap().unwrap();
        assert_eq!(deep.planar(), None);
        assert_eq!(deep.get3(0), Some(Vec3(1.0, 2.0, 3.0)));
        assert_eq!(deep.projected().as_ref(), &[Vec2(1.0, 2.0)]);
        assert!(matches!(
            deep.require_planar("geometry.demo"),
            Err(GeometryError::RequiresPlanarP {
                operation: "geometry.demo",
                actual: AttributeType::Vec3,
                ..
            })
        ));
    }

    #[test]
    fn bounds_of_three_dimensional_points_is_the_xy_extent() {
        let geo = Geometry::from_points3(vec![Vec3(-1.0, 2.0, 100.0), Vec3(3.0, -4.0, -100.0)]);
        let b = geo.bounds();
        assert_eq!((b.x, b.y, b.width, b.height), (-1.0, -4.0, 4.0, 6.0));
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
