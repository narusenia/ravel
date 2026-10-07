// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure, copy-on-write operations over geometry attributes and paths.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use thiserror::Error;

use super::absent::{self, absent_column, absent_value};
use super::{
    AttrName, AttributeArray, AttributeSet, AttributeType, Domain, Geometry, GeometryError,
    InstanceColumns, InstanceSource, InstanceTransform, MAX_INSTANCE_DEPTH, Positions, Primitive,
    names,
};
use crate::types::{Color, Rect, Vec2, Vec3, Vec4};

#[derive(Clone, Debug, PartialEq)]
pub enum AttributeValue {
    F32(f32),
    Vec2(Vec2),
    Vec3(Vec3),
    Vec4(Vec4),
    Color(Color),
    I32(i32),
    Bool(bool),
    Str(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggregateMode {
    Average,
    Max,
    First,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    Nearest,
    DistanceWeighted,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathSample {
    pub position: Vec2,
    pub tangent: Vec2,
    pub normal: Vec2,
}

#[derive(Debug, Error)]
pub enum GeometryOpError {
    #[error(transparent)]
    Geometry(#[from] GeometryError),
    #[error("domain has no elements")]
    EmptyDomain,
    #[error("{operation} does not support {attribute_type} attributes")]
    UnsupportedAttributeType {
        operation: &'static str,
        attribute_type: AttributeType,
    },
    #[error("geometry has no non-degenerate path to sample")]
    InvalidPath,
    /// Two primitives claim the same point, so a reordering of the point
    /// domain would permute it twice.
    #[error("{operation} cannot reorder points: primitive vertex runs overlap at point {point}")]
    OverlappingVertexRuns {
        operation: &'static str,
        point: usize,
    },
    /// Instances stamp sources that no primitive owns, so there is no piece
    /// to deal one to.
    #[error("{operation} cannot split a geometry that has instances")]
    HasInstances { operation: &'static str },
    #[error("the {name} attribute is required on the {domain:?} domain and cannot be deleted")]
    RequiredAttribute { name: &'static str, domain: Domain },
}

pub fn attribute_set(
    geometry: &Geometry,
    domain: Domain,
    name: &str,
    value: AttributeValue,
) -> Result<Geometry, GeometryOpError> {
    let count = domain_count(geometry, domain);
    if count == 0 {
        return Err(GeometryOpError::EmptyDomain);
    }
    let mut result = geometry.clone();
    result
        .attribute_set_mut(domain)
        .insert(name, broadcast_value(&value, count))?;
    result.validate()?;
    Ok(result)
}

/// Writes `value` into `name` on `domain`, restricted to the elements `group`
/// flags.
///
/// `group` follows the element-scope convention (REQ-CORE-013): the empty
/// string is every element, and a named `Bool` column restricts the write to
/// the elements it flags. A name that is missing, not `Bool`, or the wrong
/// length warns and affects every element rather than failing the evaluation.
///
/// Elements outside the group keep the column's current value. When the column
/// does not exist yet there is no current value to keep, so they take `unset` —
/// the value that means "nobody wrote this attribute" to whoever reads it
/// (`rasterize`'s own parameter defaults, for the style attributes). `unset`
/// has to have the same type as `value`.
pub fn attribute_set_in_group(
    geometry: &Geometry,
    domain: Domain,
    name: &str,
    value: AttributeValue,
    group: &str,
    unset: AttributeValue,
) -> Result<Geometry, GeometryOpError> {
    let count = domain_count(geometry, domain);
    if count == 0 {
        return Err(GeometryOpError::EmptyDomain);
    }
    let attributes = geometry.attribute_set(domain);
    let Some(selection) = super::field::group_selection(attributes, group, count) else {
        return attribute_set(geometry, domain, name, value);
    };
    let mut column = broadcast_value(&value, count);
    // A column of another type is not "the current value" of this attribute:
    // the write replaces it wholesale, so the elements outside the group fall
    // back to `unset` the same way they would for a missing column.
    let outside = match attributes.get(name) {
        Some(existing) if existing.attr_type() == column.attr_type() && existing.len() == count => {
            existing.as_ref().clone()
        }
        _ => broadcast_value(&unset, count),
    };
    keep_unselected(&mut column, &outside, &selection, name)?;
    let mut result = geometry.clone();
    result.attribute_set_mut(domain).insert(name, column)?;
    result.validate()?;
    Ok(result)
}

/// Overwrite the elements `selection` does *not* flag with `outside`'s values.
fn keep_unselected(
    column: &mut AttributeArray,
    outside: &AttributeArray,
    selection: &[bool],
    name: &str,
) -> Result<(), GeometryOpError> {
    macro_rules! keep {
        ($target:expr, $source:expr) => {
            for (index, inside) in selection.iter().enumerate() {
                if !inside {
                    $target[index] = $source[index];
                }
            }
        };
    }
    match (column, outside) {
        (AttributeArray::F32(target), AttributeArray::F32(source)) => keep!(target, source),
        (AttributeArray::Vec2(target), AttributeArray::Vec2(source)) => keep!(target, source),
        (AttributeArray::Vec3(target), AttributeArray::Vec3(source)) => keep!(target, source),
        (AttributeArray::Vec4(target), AttributeArray::Vec4(source)) => keep!(target, source),
        (AttributeArray::Color(target), AttributeArray::Color(source)) => keep!(target, source),
        (AttributeArray::I32(target), AttributeArray::I32(source)) => keep!(target, source),
        (AttributeArray::Bool(target), AttributeArray::Bool(source)) => keep!(target, source),
        (AttributeArray::Str(target), AttributeArray::Str(source)) => {
            for (index, inside) in selection.iter().enumerate() {
                if !inside {
                    target[index].clone_from(&source[index]);
                }
            }
        }
        (column, outside) => {
            return Err(GeometryError::TypeMismatch {
                name: name.into(),
                expected: column.attr_type(),
                actual: outside.attr_type(),
            }
            .into());
        }
    }
    Ok(())
}

/// Drops the `name` column from `domain` (REQ-CORE-010's "delete"), leaving
/// every other column shared with the input.
///
/// A name the domain does not carry is a no-op rather than an error: a
/// modulation graph deletes its scratch columns downstream of wherever they
/// were written, and an upstream edit that stops writing one must not turn the
/// whole evaluation red.
///
/// `P` is refused on the two position-carrying domains. `Geometry::validate`
/// demands it wherever the domain has elements, so the delete would either
/// fail validation or — when `P` was the only column left — silently empty the
/// domain out from under the caller.
pub fn attribute_delete(
    geometry: &Geometry,
    domain: Domain,
    name: &str,
) -> Result<Geometry, GeometryOpError> {
    if name == names::P && matches!(domain, Domain::Point | Domain::Instance) {
        return Err(GeometryOpError::RequiredAttribute {
            name: names::P,
            domain,
        });
    }
    let mut result = geometry.clone();
    if result.attribute_set_mut(domain).remove(name).is_some() {
        result.validate()?;
    }
    Ok(result)
}

/// Cross-domain promotion reduces to one value and broadcasts it. Detail
/// values are already scalar and are broadcast without applying `mode`.
pub fn promote_attribute(
    geometry: &Geometry,
    source: Domain,
    target: Domain,
    name: &str,
    mode: AggregateMode,
) -> Result<Geometry, GeometryOpError> {
    let source_column = geometry
        .attribute_set(source)
        .get(name)
        .ok_or_else(|| GeometryError::AttributeNotFound { name: name.into() })?;
    let count = domain_count(geometry, target);
    if source_column.is_empty() || count == 0 {
        return Err(GeometryOpError::EmptyDomain);
    }
    let column = if source == target {
        source_column.as_ref().clone()
    } else if source == Domain::Detail {
        repeat_first(source_column, count)?
    } else {
        reduce_and_repeat(source_column, count, mode)?
    };
    let mut result = geometry.clone();
    result.attribute_set_mut(target).insert(name, column)?;
    result.validate()?;
    Ok(result)
}

/// Nearest / distance-weighted attribute transfer between two geometries.
///
/// Distances are evaluated in three components with `z = 0` standing in for a
/// 2D column, so the two sides may differ in dimension and a pair of 2D
/// geometries produces exactly the arithmetic it did before 3D existed.
pub fn attribute_transfer(
    target: &Geometry,
    target_domain: Domain,
    source: &Geometry,
    source_domain: Domain,
    name: &str,
    mode: TransferMode,
) -> Result<Geometry, GeometryOpError> {
    let source_positions: Vec<Vec3> = positions(source, source_domain)?.iter3().collect();
    let target_positions: Vec<Vec3> = positions(target, target_domain)?.iter3().collect();
    let (source_positions, target_positions) = (&source_positions[..], &target_positions[..]);
    let source_values = source
        .attribute_set(source_domain)
        .get(name)
        .ok_or_else(|| GeometryError::AttributeNotFound { name: name.into() })?;
    if source_positions.is_empty() || target_positions.is_empty() {
        return Err(GeometryOpError::EmptyDomain);
    }
    // One grid for the whole call, or none at all when a linear scan is
    // already cheaper than building one.
    let grid = PointGrid::build(source_positions);
    let column = match mode {
        TransferMode::Nearest => {
            let indices = target_positions.iter().map(|target| match &grid {
                Some(grid) => grid.nearest(source_positions, *target),
                None => nearest_index(source_positions, *target),
            });
            select_values(source_values, indices)
        }
        TransferMode::DistanceWeighted => {
            let weights = SparseWeights::of(source_positions, target_positions, grid.as_ref());
            transfer_weighted(source_values, &weights)?
        }
    };
    let mut result = target.clone();
    result
        .attribute_set_mut(target_domain)
        .insert(name, column)?;
    result.validate()?;
    Ok(result)
}

/// The vertices of the first path primitive and whether it is closed.
///
/// Shared by every operation defined on "the" path of a geometry, so they all
/// pick the same primitive and reject the same inputs: 3D positions have no
/// agreed polyline arc length, a mesh has none at all, and a run of fewer
/// than two vertices spans no segment.
fn first_path<'a>(
    geometry: &'a Geometry,
    operation: &'static str,
) -> Result<(&'a [Vec2], bool), GeometryOpError> {
    let points = positions(geometry, Domain::Point)?.require_planar(operation)?;
    geometry.require_paths(operation)?;
    let (range, closed) = geometry
        .primitives()
        .first()
        .and_then(|primitive| match primitive {
            Primitive::Path { verts, closed } => Some((verts.clone(), *closed)),
            // `require_paths` above already rejected every mesh, so this arm
            // is unreachable; `None` keeps the match total without a panic.
            Primitive::Mesh { .. } => None,
        })
        .ok_or(GeometryOpError::InvalidPath)?;
    let path = points.get(range).ok_or(GeometryOpError::InvalidPath)?;
    if path.len() < 2 {
        return Err(GeometryOpError::InvalidPath);
    }
    Ok((path, closed))
}

/// The arc-length segment table of a geometry's first path primitive,
/// built once and sampled many times.
///
/// [`path_sample`] is the one-shot form and builds one of these per call,
/// which is the right shape for a node that reads a single place on a path.
/// A caller that samples the **same** path once per element — one place per
/// character in `text.on_path` — must hold the table across its loop
/// instead: building it walks every vertex, so a rebuild per element is
/// O(elements x vertices).
///
/// Arc length along a 3D polyline has no agreed definition yet (the frame it
/// would return is ambiguous), so a geometry with `Vec3` positions is an
/// explicit error rather than a silent projection onto xy. A mesh has no arc
/// length at all, so it is rejected the same way instead of being skipped —
/// silently sampling the first path of a mixed geometry would answer a
/// question the caller did not ask.
#[derive(Clone, Debug)]
pub struct PathArcTable {
    /// Non-empty, and cumulative length strictly increasing: `push_segment`
    /// drops zero-length segments and [`Self::build`] refuses a table whose
    /// total is degenerate. Both facts are what let [`Self::sample`] be
    /// infallible and binary-search.
    segments: Vec<Segment>,
}

impl PathArcTable {
    /// Walks the first path primitive of `geometry` into a segment table.
    ///
    /// `operation` names the caller in the planar / path-primitive errors.
    pub fn build(geometry: &Geometry, operation: &'static str) -> Result<Self, GeometryOpError> {
        let (path, closed) = first_path(geometry, operation)?;
        let mut segments = Vec::with_capacity(path.len());
        for index in 1..path.len() {
            push_segment(&mut segments, path[index - 1], path[index]);
        }
        if closed {
            push_segment(&mut segments, *path.last().unwrap(), path[0]);
        }
        if segments.last().map_or(0.0, |segment| segment.2) <= f32::EPSILON {
            return Err(GeometryOpError::InvalidPath);
        }
        Ok(Self { segments })
    }

    /// Total arc length, always greater than `f32::EPSILON`.
    pub fn length(&self) -> f32 {
        self.segments.last().map_or(0.0, |segment| segment.2)
    }

    /// Samples at an absolute arc length, clamped to `0..=length()`.
    pub fn sample(&self, distance: f32) -> PathSample {
        let target = distance.clamp(0.0, self.length());
        // The first segment whose cumulative length reaches `target`. A
        // binary search rather than a scan because a per-element caller
        // would otherwise be back to O(elements x vertices) with the table
        // shared; the answer is the same one a scan gives, cumulative length
        // being strictly increasing.
        let index = self
            .segments
            .partition_point(|segment| segment.2 < target)
            .min(self.segments.len() - 1);
        let (start, end, cumulative, length) = self.segments[index];
        let t = ((target - (cumulative - length)) / length).clamp(0.0, 1.0);
        let tangent = normalize(Vec2(end.0 - start.0, end.1 - start.1));
        PathSample {
            position: Vec2(
                start.0 + (end.0 - start.0) * t,
                start.1 + (end.1 - start.1) * t,
            ),
            tangent,
            normal: Vec2(-tangent.1, tangent.0),
        }
    }
}

/// Samples the first path primitive at an absolute, clamped arc length.
///
/// The one-shot form of [`PathArcTable`]: it builds the table, takes one
/// sample and drops it. Sampling the same path repeatedly wants the table
/// held instead.
pub fn path_sample(geometry: &Geometry, distance: f32) -> Result<PathSample, GeometryOpError> {
    Ok(PathArcTable::build(geometry, "attribute.path_sample")?.sample(distance))
}

/// Which points [`connect`] runs a path through, and in what order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectMode<'a> {
    /// Every point, in storage order — which is `index` order, since every
    /// operation that moves points carries `index` along with them.
    Order,
    /// Every point, as a greedy nearest-neighbour chain starting at the first.
    Nearest,
    /// Only the points whose named `Bool` column is true, in storage order.
    Group(&'a str),
}

/// Whether [`connect`] leaves the new path straight or curves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectInterpolation {
    /// Straight segments. The tangent columns are left exactly as they came
    /// in — a path drawn with the pen tool keeps its own curvature.
    Linear,
    /// Catmull-Rom tangents written to `in_tan` / `out_tan`, which is what
    /// `rasterize` flattens into a curve.
    Bezier,
}

/// How many neighbours the grid is asked for before [`ConnectMode::Nearest`]
/// falls back to a scan. Small: the chain only needs the closest *unvisited*
/// point, and past the first few candidates a full scan is the honest answer.
const NEAREST_CHAIN_NEIGHBOURS: usize = 8;

/// Runs one path through the points, adding connectivity without adding
/// points.
///
/// A path primitive spans a **contiguous** run of point indices, so the points
/// are permuted into the order the path visits them (`ConnectMode::Order`
/// permutes nothing). Every attribute column travels with its point, `index`
/// included — a connected point keeps the `index` it was created with rather
/// than being renumbered into its new slot.
///
/// The primitives the input carried are **replaced**, not added to: this is
/// the node that decides the connectivity, and Houdini's Add SOP keeps the
/// points and drops the geometry for the same reason. Instances, instance
/// sources and detail attributes pass through untouched.
///
/// Fewer than two points to connect — an empty input, a single point, a group
/// nobody is in — is a no-op that returns the input unchanged rather than an
/// error, because it is a normal frame of an animated point count.
pub fn connect(
    geometry: &Geometry,
    mode: ConnectMode<'_>,
    interpolation: ConnectInterpolation,
    closed: bool,
) -> Result<Geometry, GeometryOpError> {
    if geometry.point_count() < 2 {
        return Ok(geometry.clone());
    }
    let points = positions(geometry, Domain::Point)?.require_planar("geometry.connect")?;
    // Replacing the primitives would silently drop a mesh's triangles, and
    // the tangents below are planar. Both are explicit errors instead.
    geometry.require_paths("geometry.connect")?;
    let (order, connected) = match mode {
        ConnectMode::Order => ((0..points.len()).collect(), points.len()),
        ConnectMode::Nearest => (nearest_chain(points), points.len()),
        ConnectMode::Group(name) => group_first_order(geometry, name)?,
    };
    if connected < 2 {
        return Ok(geometry.clone());
    }

    let mut result = reordered(geometry, &order)?;
    if interpolation == ConnectInterpolation::Bezier {
        let path: Vec<Vec2> = order[..connected]
            .iter()
            .map(|index| points[*index])
            .collect();
        let (mut in_tans, mut out_tans) = (
            tangent_column(&result, names::IN_TAN, order.len()),
            tangent_column(&result, names::OUT_TAN, order.len()),
        );
        for (vertex, (incoming, outgoing)) in
            catmull_rom_tangents(&path, closed).into_iter().enumerate()
        {
            in_tans[vertex] = incoming;
            out_tans[vertex] = outgoing;
        }
        result
            .points_mut()
            .insert(names::IN_TAN, AttributeArray::Vec2(in_tans))?;
        result
            .points_mut()
            .insert(names::OUT_TAN, AttributeArray::Vec2(out_tans))?;
    }
    result.push_primitive(Primitive::Path {
        verts: 0..connected,
        closed,
    });
    result.validate()?;
    Ok(result)
}

/// The same geometry with its points permuted by `order` and its primitives
/// dropped. Every point column is selected through the same permutation, so
/// values stay attached to the point they described.
fn reordered(geometry: &Geometry, order: &[usize]) -> Result<Geometry, GeometryOpError> {
    let mut result = Geometry::new();
    for (name, column) in geometry.points().iter() {
        result
            .points_mut()
            .insert(name.as_str(), select_values(column, order.iter().copied()))?;
    }
    for (name, column) in geometry.instances().iter() {
        result
            .instances_mut()
            .insert(name.as_str(), column.as_ref().clone())?;
    }
    result.set_sources(geometry.sources().to_vec());
    for (name, column) in geometry.detail().iter() {
        result
            .detail_mut()
            .insert(name.as_str(), column.as_ref().clone())?;
    }
    Ok(result)
}

/// The existing tangent column of `geometry`, or zeros when it has none.
fn tangent_column(geometry: &Geometry, name: &str, count: usize) -> Vec<Vec2> {
    geometry
        .points()
        .get(name)
        .and_then(|column| column.as_vec2(name).ok())
        .filter(|values| values.len() == count)
        .map_or_else(|| vec![Vec2(0.0, 0.0); count], <[Vec2]>::to_vec)
}

/// Group members first, in storage order, then everybody else — the members
/// have to be contiguous for a path to span them.
fn group_first_order(
    geometry: &Geometry,
    name: &str,
) -> Result<(Vec<usize>, usize), GeometryOpError> {
    let column = geometry
        .points()
        .get(name)
        .ok_or_else(|| GeometryError::AttributeNotFound { name: name.into() })?;
    let members = column.as_bool(name)?;
    let mut order: Vec<usize> = (0..members.len()).filter(|index| members[*index]).collect();
    let connected = order.len();
    order.extend((0..members.len()).filter(|index| !members[*index]));
    Ok((order, connected))
}

/// Visit order of a greedy nearest-neighbour chain from the first point.
///
/// Deterministic on every input: distance ties go to the lower index, in the
/// grid and in the scan alike.
fn nearest_chain(points: &[Vec2]) -> Vec<usize> {
    let spatial: Vec<Vec3> = points.iter().map(|p| Vec3(p.0, p.1, 0.0)).collect();
    let grid = PointGrid::build(&spatial);
    let mut visited = vec![false; spatial.len()];
    let mut order = Vec::with_capacity(spatial.len());
    let mut current = 0;
    visited[0] = true;
    order.push(0);
    let mut neighbours = Vec::new();
    while order.len() < spatial.len() {
        current = nearest_unvisited(&spatial, current, &visited, grid.as_ref(), &mut neighbours);
        visited[current] = true;
        order.push(current);
    }
    order
}

/// The unvisited point closest to `from`.
///
/// The grid answers while the chain is young; once its `k` closest candidates
/// have all been visited there is nothing for a spatial index to prune and the
/// scan takes over, which makes the tail of a long chain quadratic. That is
/// the same shape of cost the chain itself has and nobody has asked for a
/// longer one yet; a k-d tree with deletion is the upgrade if they do.
fn nearest_unvisited(
    points: &[Vec3],
    from: usize,
    visited: &[bool],
    grid: Option<&PointGrid>,
    neighbours: &mut Vec<(usize, f32)>,
) -> usize {
    if let Some(grid) = grid {
        grid.k_nearest(points, points[from], NEAREST_CHAIN_NEIGHBOURS, neighbours);
        let closest = neighbours
            .iter()
            .filter(|(index, _)| !visited[*index])
            .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        if let Some((index, _)) = closest {
            return *index;
        }
    }
    (0..points.len())
        .filter(|index| !visited[*index])
        .min_by(|a, b| {
            distance_squared(points[*a], points[from])
                .total_cmp(&distance_squared(points[*b], points[from]))
                .then(a.cmp(b))
        })
        .expect("the caller only asks while a point is unvisited")
}

/// Catmull-Rom `(in_tan, out_tan)` for each vertex of one path.
///
/// The control point of the segment arriving at `P` is `P + in_tan` and the
/// one leaving it is `P + out_tan` (`names::IN_TAN`), so the interior tangent
/// is a sixth of the chord between the neighbours — the standard conversion
/// that makes the cubic pass through the points. An open path's ends have one
/// neighbour and one unused side: a third of the only segment there is, and
/// zero for the side no segment reaches.
fn catmull_rom_tangents(path: &[Vec2], closed: bool) -> Vec<(Vec2, Vec2)> {
    let scaled = |from: Vec2, to: Vec2, divisor: f32| {
        Vec2((to.0 - from.0) / divisor, (to.1 - from.1) / divisor)
    };
    (0..path.len())
        .map(|vertex| {
            let previous = match vertex {
                0 if closed => path.last().copied(),
                0 => None,
                _ => Some(path[vertex - 1]),
            };
            let next = match path.get(vertex + 1) {
                Some(point) => Some(*point),
                None if closed => path.first().copied(),
                None => None,
            };
            let zero = Vec2(0.0, 0.0);
            match (previous, next) {
                (Some(previous), Some(next)) => {
                    let tangent = scaled(previous, next, 6.0);
                    (Vec2(-tangent.0, -tangent.1), tangent)
                }
                (None, Some(next)) => (zero, scaled(path[vertex], next, 3.0)),
                (Some(previous), None) => (scaled(path[vertex], previous, 3.0), zero),
                (None, None) => (zero, zero),
            }
        })
        .collect()
}

/// How [`curve_u`] spaces the path parameter along a primitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveUMode {
    /// Fraction of the primitive's arc length — uneven point spacing shows up
    /// in the values.
    ArcLength,
    /// Fraction of the vertex count — every point is one equal step.
    VertexOrder,
}

/// Writes the path parameter `u` (Houdini's `curveu`) on every point.
///
/// Each path primitive is normalised **on its own**, so a geometry with two
/// paths carries two independent `0..1` ramps rather than one running count.
/// A closed path spends part of its length on the closing segment, so its
/// last point sits just short of 1 rather than on it — the point where `u`
/// wraps back to 0 is the start point, not a duplicate of it.
///
/// Points no path primitive references — loose points, and every point of a
/// degenerate (zero-length or single-vertex) path — get `0.0`.
///
/// Arc length is measured with the same accumulation [`path_sample`] uses, so
/// the two nodes agree on where the halfway point of a path is. It carries
/// the same restrictions for the same reasons: 3D arc length is undefined and
/// a mesh has none at all, so both are explicit errors.
pub fn curve_u(geometry: &Geometry, mode: CurveUMode) -> Result<Geometry, GeometryOpError> {
    let points = positions(geometry, Domain::Point)?.require_planar("attribute.curveu")?;
    geometry.require_paths("attribute.curveu")?;
    let mut column = vec![0.0f32; points.len()];
    for primitive in geometry.primitives() {
        let Primitive::Path { verts, closed } = primitive else {
            // `require_paths` rejected every mesh above.
            continue;
        };
        let path = points
            .get(verts.clone())
            .ok_or(GeometryOpError::InvalidPath)?;
        for (slot, u) in column[verts.clone()]
            .iter_mut()
            .zip(path_parameters(path, *closed, mode))
        {
            *slot = u;
        }
    }
    let mut result = geometry.clone();
    result
        .points_mut()
        .insert(names::U, AttributeArray::F32(column))?;
    result.validate()?;
    Ok(result)
}

/// `u` for each vertex of one polyline.
///
/// The closing segment of a closed path counts towards the total in both
/// modes, which is what keeps `by_vertex_order` a usable stand-in for
/// `by_arc_length` on evenly spaced points: a closed regular polygon reports
/// the same `(n - 1) / n` for its last vertex either way.
fn path_parameters(path: &[Vec2], closed: bool, mode: CurveUMode) -> Vec<f32> {
    let steps = if closed { path.len() } else { path.len() - 1 };
    if path.len() < 2 {
        return vec![0.0; path.len()];
    }
    if mode == CurveUMode::VertexOrder {
        return (0..path.len())
            .map(|index| index as f32 / steps as f32)
            .collect();
    }
    let (at_vertex, total) = vertex_arc_lengths(path, closed);
    if total <= f32::EPSILON {
        return vec![0.0; path.len()];
    }
    at_vertex.iter().map(|length| length / total).collect()
}

/// Cumulative arc length at each vertex of one polyline, and the total
/// (closing segment included when `closed`).
///
/// Shares `push_segment` with `path_sample`: the same cumulative lengths, and
/// the same rule that a zero-length segment does not advance them (a
/// duplicated point therefore repeats its predecessor's length).
fn vertex_arc_lengths(path: &[Vec2], closed: bool) -> (Vec<f32>, f32) {
    let mut segments = Vec::with_capacity(path.len());
    let mut at_vertex = Vec::with_capacity(path.len());
    for (index, point) in path.iter().enumerate() {
        at_vertex.push(segments.last().map_or(0.0, |segment: &Segment| segment.2));
        if let Some(next) = path.get(index + 1) {
            push_segment(&mut segments, *point, *next);
        }
    }
    if closed && let (Some(last), Some(first)) = (path.last(), path.first()) {
        push_segment(&mut segments, *last, *first);
    }
    let total = segments.last().map_or(0.0, |segment| segment.2);
    (at_vertex, total)
}

// ---------------------------------------------------------------------------
// Resample
// ---------------------------------------------------------------------------

/// Segments one **path** may be divided into, across all its `keep_corners`
/// spans together (each span's share is scaled down in proportion, never below
/// one). A tiny `length` would otherwise ask for billions of points; the cap is
/// far above anything a drawn path needs and turns a runaway parameter into a
/// coarse result.
const MAX_PATH_SEGMENTS: usize = 1 << 20;

/// A turn sharper than this at a vertex (cosine of 1 degree) is a corner.
const CORNER_COS: f32 = 0.999_847_7;

/// Re-places the points of every path primitive at even arc-length spacing.
///
/// `length > 0` divides each path into `round(arc / length)` equal segments
/// (at least 1), so the spacing is the nearest *even* one to `length` and the
/// path ends stay exact; otherwise `segments` equal segments are used. With
/// `keep_corners` the vertices where the path turns by more than about a
/// degree stay as points and each stretch between them is divided on its own —
/// in `segments` mode a stretch gets `round(segments * its share of the arc)`.
///
/// Point attributes are read back from the original points the new point falls
/// between: `F32`, vectors and colours interpolate linearly, and the
/// non-interpolable `I32` / `Bool` / `Str` take the nearer endpoint (so `id`
/// stays a real id). `index` is renumbered. `in_tan` / `out_tan` are dropped:
/// they describe the old curve's handles, and a resampled path is a polyline.
///
/// Points that no path references are dropped with the rest of the rebuilt
/// point list. A path with a single vertex or no length cannot be spaced, so it
/// passes through as it is rather than failing; a geometry with no path at all
/// is returned unchanged. Arc length is [`path_sample`]'s, so 3D positions and
/// meshes are explicit errors for the same reasons.
pub fn resample(
    geometry: &Geometry,
    length: f32,
    segments: usize,
    keep_corners: bool,
) -> Result<Geometry, GeometryOpError> {
    if geometry.primitives().is_empty() {
        return Ok(geometry.clone());
    }
    let points = positions(geometry, Domain::Point)?.require_planar("geometry.resample")?;
    geometry.require_paths("geometry.resample")?;

    // `(from, to, t)` per output point, in global point indices.
    let mut samples: Vec<(usize, usize, f32)> = Vec::new();
    let mut primitives = Vec::with_capacity(geometry.primitive_count());
    for primitive in geometry.primitives() {
        let Primitive::Path { verts, closed } = primitive else {
            continue;
        };
        let path = points
            .get(verts.clone())
            .ok_or(GeometryOpError::InvalidPath)?;
        let start = samples.len();
        for (from, to, t) in path_samples(path, *closed, length, segments, keep_corners) {
            samples.push((verts.start + from, verts.start + to, t));
        }
        primitives.push(Primitive::Path {
            verts: start..samples.len(),
            closed: *closed,
        });
    }

    let mut result = geometry.clone();
    let mut resampled = AttributeSet::new();
    for (name, column) in geometry.points().iter() {
        if name.as_str() == names::IN_TAN || name.as_str() == names::OUT_TAN {
            continue;
        }
        resampled.insert(name.as_str(), interpolate_samples(column, &samples))?;
    }
    *result.attribute_set_mut(Domain::Point) = resampled;
    result.set_primitives(primitives);
    renumber_index(&mut result, Domain::Point)?;
    result.validate()?;
    Ok(result)
}

/// The `(from vertex, to vertex, t)` of every point [`resample`] puts on one
/// path, in path order. A degenerate path answers with its own vertices.
fn path_samples(
    path: &[Vec2],
    closed: bool,
    length: f32,
    segments: usize,
    keep_corners: bool,
) -> Vec<(usize, usize, f32)> {
    let vertices = path.len();
    let (at_vertex, total) = vertex_arc_lengths(path, closed);
    if vertices < 2 || total <= f32::EPSILON {
        return (0..vertices).map(|vertex| (vertex, vertex, 0.0)).collect();
    }

    // The vertices the spans run between. An open path always owns its two
    // ends and a closed one its first vertex, so a span never needs to
    // reason about "the start of the loop".
    let mut anchors = vec![0];
    if keep_corners {
        anchors
            .extend((1..vertices - usize::from(!closed)).filter(|v| is_corner(path, *v, closed)));
    }
    if !closed {
        anchors.push(vertices - 1);
    }
    anchors.dedup();

    let spans = if closed {
        anchors.len()
    } else {
        anchors.len() - 1
    };
    // `(from anchor, arc length at it, span length, wanted segments)` per span.
    let mut plan = Vec::with_capacity(spans);
    for span in 0..spans {
        let (a, b) = (anchors[span], anchors[(span + 1) % anchors.len()]);
        let begin = at_vertex[a];
        let span_length = if b > a || (!closed) {
            at_vertex[b] - begin
        } else {
            total - begin + at_vertex[b]
        };
        let wanted = if span_length <= f32::EPSILON {
            0.0
        } else if length > 0.0 {
            f64::from(span_length / length).round()
        } else {
            (segments as f64 * f64::from(span_length / total)).round()
        };
        plan.push((a, begin, span_length, wanted.max(1.0)));
    }
    // One budget for the whole path, shared out in proportion to what each
    // span asked for: a per-span cap would still allow corners x cap points.
    let asked: f64 = plan.iter().map(|span| span.3).sum();
    let scale = (MAX_PATH_SEGMENTS as f64 / asked).min(1.0);
    let mut samples = Vec::new();
    for (a, begin, span_length, wanted) in plan {
        samples.push((a, a, 0.0));
        if span_length <= f32::EPSILON {
            continue;
        }
        let count = ((wanted * scale).floor() as usize).max(1);
        for step in 1..count {
            let distance = begin + span_length * step as f32 / count as f32;
            samples.push(locate(&at_vertex, total, vertices, closed, distance));
        }
    }
    if !closed {
        samples.push((vertices - 1, vertices - 1, 0.0));
    }
    samples
}

/// Whether the path turns by more than a degree at `vertex`.
fn is_corner(path: &[Vec2], vertex: usize, closed: bool) -> bool {
    let count = path.len();
    let (before, after) = if closed {
        (
            path[(vertex + count - 1) % count],
            path[(vertex + 1) % count],
        )
    } else {
        (path[vertex - 1], path[vertex + 1])
    };
    let here = path[vertex];
    let (incoming, outgoing) = (
        Vec2(here.0 - before.0, here.1 - before.1),
        Vec2(after.0 - here.0, after.1 - here.1),
    );
    let lengths = (incoming.0.hypot(incoming.1)) * (outgoing.0.hypot(outgoing.1));
    lengths > f32::EPSILON
        && (incoming.0 * outgoing.0 + incoming.1 * outgoing.1) / lengths < CORNER_COS
}

/// The segment `distance` along the path falls on, as `(from, to, t)`.
fn locate(
    at_vertex: &[f32],
    total: f32,
    vertices: usize,
    closed: bool,
    distance: f32,
) -> (usize, usize, f32) {
    // The last vertex at or before `distance`: with duplicated points several
    // share a length, and the last of them is the one whose outgoing segment
    // has any.
    let from = at_vertex
        .partition_point(|length| *length <= distance)
        .saturating_sub(1);
    let end = if from + 1 < vertices {
        at_vertex[from + 1]
    } else {
        total
    };
    let to = if from + 1 < vertices || closed {
        (from + 1) % vertices
    } else {
        from
    };
    let span = end - at_vertex[from];
    let t = if span > f32::EPSILON {
        ((distance - at_vertex[from]) / span).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (from, to, t)
}

/// One output row per `(from, to, t)` of `column`: a linear blend where the
/// type has one, the nearer endpoint where it does not.
fn interpolate_samples(column: &AttributeArray, samples: &[(usize, usize, f32)]) -> AttributeArray {
    macro_rules! blend {
        ($values:expr, $variant:ident, $mix:expr) => {
            AttributeArray::$variant(
                samples
                    .iter()
                    .map(|(from, to, t)| $mix(&$values[*from], &$values[*to], *t))
                    .collect(),
            )
        };
    }
    fn nearest<T: Clone>(from: &T, to: &T, t: f32) -> T {
        if t < 0.5 { from } else { to }.clone()
    }
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    match column {
        AttributeArray::F32(v) => blend!(v, F32, |a: &f32, b: &f32, t| lerp(*a, *b, t)),
        AttributeArray::Vec2(v) => blend!(v, Vec2, |a: &Vec2, b: &Vec2, t| Vec2(
            lerp(a.0, b.0, t),
            lerp(a.1, b.1, t)
        )),
        AttributeArray::Vec3(v) => blend!(v, Vec3, |a: &Vec3, b: &Vec3, t| Vec3(
            lerp(a.0, b.0, t),
            lerp(a.1, b.1, t),
            lerp(a.2, b.2, t)
        )),
        AttributeArray::Vec4(v) => blend!(v, Vec4, |a: &Vec4, b: &Vec4, t| Vec4(
            lerp(a.0, b.0, t),
            lerp(a.1, b.1, t),
            lerp(a.2, b.2, t),
            lerp(a.3, b.3, t)
        )),
        AttributeArray::Color(v) => blend!(v, Color, |a: &Color, b: &Color, t| Color::new(
            lerp(a.r, b.r, t),
            lerp(a.g, b.g, t),
            lerp(a.b, b.b, t),
            lerp(a.a, b.a, t)
        )),
        AttributeArray::I32(v) => blend!(v, I32, nearest),
        AttributeArray::Bool(v) => blend!(v, Bool, nearest),
        AttributeArray::Str(v) => blend!(v, Str, nearest),
    }
}

// ---------------------------------------------------------------------------
// Measure
// ---------------------------------------------------------------------------

/// What [`measure`] writes. Each quantity lives on the domain it describes,
/// which is what lets a field read it back with `field.attribute`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measure {
    /// Primitive, `F32`: the length of the path, closing segment included for
    /// a closed one.
    Perimeter,
    /// Primitive, `F32`: the **signed** enclosed area. Positive when the path
    /// runs counter-clockwise in the coordinates it is written in, negative
    /// clockwise. A path that crosses itself sums its lobes by winding, so a
    /// figure of eight can read 0 — take the absolute value for "how much
    /// ink". An **open** path is measured as if closed: the area between it
    /// and the chord from its last point back to its first.
    Area,
    /// Point, `F32`: the signed Menger curvature `1 / R` of the circle through
    /// the point and its two neighbours (exact on a circle, positive for a
    /// left turn). The ends of an open path, and any point with a coincident
    /// neighbour, read 0.
    Curvature,
    /// Point, `F32`: the length of the segment leaving the point towards the
    /// next one. The last point of an open path has none and reads 0; the last
    /// point of a closed path owns the closing segment.
    SegmentLength,
    /// Detail, `Vec4`: `(min x, min y, max x, max y)` of the point positions —
    /// the instance positions when there are no points, all zero when there is
    /// nothing at all.
    Bounds,
    /// Primitive, `Vec2`: `(width, height)` of the box around the primitive's
    /// own points.
    Size,
}

impl Measure {
    /// The domain this measurement is written on.
    pub fn domain(self) -> Domain {
        match self {
            Self::Perimeter | Self::Area | Self::Size => Domain::Primitive,
            Self::Curvature | Self::SegmentLength => Domain::Point,
            Self::Bounds => Domain::Detail,
        }
    }

    /// The attribute name used when the caller gives none.
    pub fn default_name(self) -> &'static str {
        match self {
            Self::Perimeter => "perimeter",
            Self::Area => "area",
            Self::Curvature => "curvature",
            Self::SegmentLength => "segment_length",
            Self::Bounds => "bounds",
            Self::Size => "size",
        }
    }
}

/// Writes one geometric measurement as an attribute (empty `name` takes
/// [`Measure::default_name`]), replacing a column of that name on the target
/// domain. Everything else passes through, sharing its columns.
///
/// The path measurements are planar and refuse meshes for the reasons
/// [`path_sample`] does; `Bounds` and `Size` read the xy of 2D or 3D
/// positions.
pub fn measure(
    geometry: &Geometry,
    what: Measure,
    name: &str,
) -> Result<Geometry, GeometryOpError> {
    let name = if name.is_empty() {
        what.default_name()
    } else {
        name
    };
    let column = match what {
        Measure::Bounds | Measure::Size => measure_extent(geometry, what)?,
        _ => measure_paths(geometry, what)?,
    };
    let mut result = geometry.clone();
    result
        .attribute_set_mut(what.domain())
        .insert(name, column)?;
    result.validate()?;
    Ok(result)
}

/// The path-based measurements, one value per primitive or per point.
fn measure_paths(geometry: &Geometry, what: Measure) -> Result<AttributeArray, GeometryOpError> {
    let points = match geometry.positions(Domain::Point) {
        Some(positions) => positions?.require_planar("geometry.measure")?,
        None => &[],
    };
    geometry.require_paths("geometry.measure")?;
    let mut per_point = vec![0.0f32; points.len()];
    let mut per_primitive = Vec::with_capacity(geometry.primitive_count());
    for primitive in geometry.primitives() {
        let Primitive::Path { verts, closed } = primitive else {
            continue;
        };
        let path = points
            .get(verts.clone())
            .ok_or(GeometryOpError::InvalidPath)?;
        // The vertex a segment leaving `index` arrives at, if it has one.
        let next = |index: usize| match (index + 1 < path.len(), *closed && path.len() > 1) {
            (true, _) => Some(path[index + 1]),
            (false, true) => Some(path[0]),
            _ => None,
        };
        per_primitive.push(match what {
            Measure::Perimeter => (0..path.len())
                .filter_map(|i| next(i).map(|n| planar_distance_squared(path[i], n).sqrt()))
                .sum(),
            Measure::Area => {
                (0..path.len())
                    .map(|i| {
                        let (a, b) = (path[i], path[(i + 1) % path.len()]);
                        a.0 * b.1 - b.0 * a.1
                    })
                    .sum::<f32>()
                    / 2.0
            }
            _ => 0.0,
        });
        for (offset, slot) in per_point[verts.clone()].iter_mut().enumerate() {
            *slot = match what {
                Measure::SegmentLength => {
                    next(offset).map_or(0.0, |n| planar_distance_squared(path[offset], n).sqrt())
                }
                Measure::Curvature => {
                    let before = match (offset, *closed) {
                        (0, true) => path.last().copied(),
                        (0, false) => None,
                        _ => Some(path[offset - 1]),
                    };
                    match (before, next(offset)) {
                        (Some(a), Some(c)) => menger_curvature(a, path[offset], c),
                        _ => 0.0,
                    }
                }
                _ => 0.0,
            };
        }
    }
    Ok(AttributeArray::F32(match what {
        Measure::Perimeter | Measure::Area => per_primitive,
        _ => per_point,
    }))
}

/// Signed `1 / R` of the circle through three points; 0 when any two coincide
/// or the three are collinear.
fn menger_curvature(a: Vec2, b: Vec2, c: Vec2) -> f32 {
    let (ab, bc, ac) = (
        planar_distance_squared(a, b).sqrt(),
        planar_distance_squared(b, c).sqrt(),
        planar_distance_squared(a, c).sqrt(),
    );
    let denominator = ab * bc * ac;
    if denominator <= f32::EPSILON {
        return 0.0;
    }
    let cross = (b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0);
    2.0 * cross / denominator
}

/// `Bounds` (one `Vec4` for the detail) and `Size` (one `Vec2` per primitive).
fn measure_extent(geometry: &Geometry, what: Measure) -> Result<AttributeArray, GeometryOpError> {
    let corners = |points: &[Vec2]| {
        points.iter().fold(None, |extent: Option<(Vec2, Vec2)>, p| {
            Some(match extent {
                None => (*p, *p),
                Some((low, high)) => (
                    Vec2(low.0.min(p.0), low.1.min(p.1)),
                    Vec2(high.0.max(p.0), high.1.max(p.1)),
                ),
            })
        })
    };
    let xy = |domain| -> Result<Cow<'_, [Vec2]>, GeometryOpError> {
        Ok(match geometry.positions(domain) {
            Some(positions) => positions?.projected(),
            None => Cow::Borrowed(&[]),
        })
    };
    if what == Measure::Bounds {
        let points = xy(Domain::Point)?;
        let extent = if points.is_empty() {
            corners(&xy(Domain::Instance)?)
        } else {
            corners(&points)
        };
        let (low, high) = extent.unwrap_or((Vec2(0.0, 0.0), Vec2(0.0, 0.0)));
        return Ok(AttributeArray::Vec4(vec![Vec4(
            low.0, low.1, high.0, high.1,
        )]));
    }
    let points = xy(Domain::Point)?;
    geometry
        .primitives()
        .iter()
        .map(|primitive| {
            let run = points
                .get(primitive.verts().clone())
                .ok_or(GeometryOpError::InvalidPath)?;
            let (low, high) = corners(run).unwrap_or((Vec2(0.0, 0.0), Vec2(0.0, 0.0)));
            Ok(Vec2(high.0 - low.0, high.1 - low.1))
        })
        .collect::<Result<Vec<_>, GeometryOpError>>()
        .map(AttributeArray::Vec2)
}

// ---------------------------------------------------------------------------
// Sort
// ---------------------------------------------------------------------------

/// What [`sort`] orders the elements of a domain by.
///
/// Every mode is **ascending**; descending is `sort` again with
/// [`SortMode::Reverse`], which composes without a second parameter on every
/// mode.
#[derive(Clone, Copy, Debug)]
pub enum SortMode<'a> {
    /// Ascending x of the element's position.
    X,
    /// Ascending y of the element's position.
    Y,
    /// Ascending distance from `center`, measured in three components (a 2D
    /// geometry reads `z = 0`, so `center.z` simply offsets every element by
    /// the same amount).
    Radial { center: Vec3 },
    /// Ascending arc length of the closest projection onto the first path
    /// primitive of `path` — the ordering "along this curve".
    AlongPath { path: &'a Geometry },
    /// A shuffle keyed by `seed`, decided by the same hash `scatter.*` places
    /// its points with, so one seed means one arrangement across both.
    Random { seed: u32 },
    /// Ascending value of the named attribute column of the sorted domain.
    Attribute(&'a str),
    /// Storage order, reversed.
    Reverse,
}

/// The comparable key of every element, in storage order.
enum SortKeys {
    /// `f64` rather than `f32` so a [`SortMode::Random`] hash is exact: a
    /// `u32` past 2^24 does not survive a round trip through `f32`, and two
    /// elements whose hashes collided there would be ordered by their index
    /// instead of by the seed.
    Num(Vec<f64>),
    Text(Vec<String>),
}

impl SortKeys {
    fn len(&self) -> usize {
        match self {
            Self::Num(keys) => keys.len(),
            Self::Text(keys) => keys.len(),
        }
    }

    /// Element indices in ascending key order. The sort is stable, so equal
    /// keys keep their storage order and the result is deterministic on every
    /// input — which is what makes `sort(random)` reproducible.
    fn order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.len()).collect();
        match self {
            Self::Num(keys) => order.sort_by(|a, b| keys[*a].total_cmp(&keys[*b])),
            Self::Text(keys) => order.sort_by(|a, b| keys[*a].cmp(&keys[*b])),
        }
        order
    }
}

/// The deterministic per-element hash the procedural nodes share.
///
/// `scatter.*` places its points with it and [`SortMode::Random`] shuffles
/// with it, so the two agree on what a seed means. Not a general-purpose
/// hash: it is a fixed part of those nodes' output, and changing it changes
/// every scattered layout that has ever been saved.
pub fn element_hash(seed: u32, index: u32) -> u32 {
    let mut hash = seed.wrapping_mul(0x9E37_79B9).wrapping_add(index);
    hash = (hash ^ (hash >> 16)).wrapping_mul(0x045D_9F3B);
    hash = (hash ^ (hash >> 16)).wrapping_mul(0x045D_9F3B);
    hash ^ (hash >> 16)
}

/// Reorders the elements of one domain and renumbers `index`.
///
/// Every attribute column of `domain` is selected through the **same**
/// permutation, so a value stays attached to the element it described —
/// `id` included, which is what makes it the identity that survives a sort.
/// `index` is the storage slot rather than a value, so it is renumbered to
/// `0..n` afterwards (and only when the domain already carries one: a sort
/// does not invent columns). The other domains, the instance sources, and the
/// detail attributes pass through untouched — an instance keeps the source it
/// stamped because `source_index` travels with it like any other column.
///
/// **The point domain is permuted inside each primitive's vertex run.** A
/// [`Primitive::Path`] spans a *contiguous* run of points
/// (`geometry::container`), so a permutation that moved a point out of its run
/// would silently rebuild the shape; confining it keeps every `verts` range
/// valid and byte-identical. A point cloud with no primitives is therefore one
/// run and sorts freely — which is the case the stagger orderings care about —
/// and a geometry of paths sorts the vertices within each path. A mesh is an
/// explicit error instead: its triangle indices are relative to `verts.start`,
/// so moving its points would reface it.
///
/// Fewer than two elements is a no-op that returns the input unchanged, which
/// is also the whole of the detail domain: it holds exactly one element and
/// therefore has no order to change.
pub fn sort(
    geometry: &Geometry,
    domain: Domain,
    mode: SortMode<'_>,
) -> Result<Geometry, GeometryOpError> {
    let count = domain_count(geometry, domain);
    if count < 2 {
        return Ok(geometry.clone());
    }
    if domain == Domain::Point {
        geometry.require_paths("geometry.sort")?;
    }

    let order = match mode {
        SortMode::Reverse => (0..count).rev().collect(),
        mode => {
            let keys = sort_keys(geometry, domain, mode)?;
            if keys.len() != count {
                return Err(GeometryError::LengthMismatch {
                    name: "geometry.sort keys".into(),
                    expected: count,
                    actual: keys.len(),
                }
                .into());
            }
            keys.order()
        }
    };
    let order = match domain {
        Domain::Point => within_runs(&order, &vertex_runs(geometry, count)?),
        _ => order,
    };

    let mut result = geometry.clone();
    for (name, column) in geometry.attribute_set(domain).iter() {
        result
            .attribute_set_mut(domain)
            .insert(name.as_str(), select_values(column, order.iter().copied()))?;
    }
    if domain == Domain::Primitive {
        let permuted = order
            .iter()
            .map(|index| geometry.primitives()[*index].clone())
            .collect();
        result.set_primitives(permuted);
    }
    let renumber = result
        .attribute_set(domain)
        .get(names::INDEX)
        .is_some_and(|column| matches!(column.as_ref(), AttributeArray::I32(_)));
    if renumber {
        result.attribute_set_mut(domain).insert(
            names::INDEX,
            AttributeArray::I32((0..count as i32).collect()),
        )?;
    }
    result.validate()?;
    Ok(result)
}

/// The key of every element of `domain` under `mode`.
fn sort_keys(
    geometry: &Geometry,
    domain: Domain,
    mode: SortMode<'_>,
) -> Result<SortKeys, GeometryOpError> {
    let numeric = |values: Vec<f64>| Ok(SortKeys::Num(values));
    match mode {
        SortMode::X => numeric(
            element_positions(geometry, domain)?
                .iter()
                .map(|position| position.0 as f64)
                .collect(),
        ),
        SortMode::Y => numeric(
            element_positions(geometry, domain)?
                .iter()
                .map(|position| position.1 as f64)
                .collect(),
        ),
        SortMode::Radial { center } => numeric(
            element_positions(geometry, domain)?
                .iter()
                .map(|position| distance_squared(*position, center) as f64)
                .collect(),
        ),
        SortMode::AlongPath { path } => {
            let (polyline, closed) = first_path(path, "geometry.sort")?;
            numeric(path_projections(
                &element_positions(geometry, domain)?,
                polyline,
                closed,
            ))
        }
        SortMode::Random { seed } => numeric(
            (0..domain_count(geometry, domain))
                .map(|index| f64::from(element_hash(seed, index as u32)))
                .collect(),
        ),
        SortMode::Attribute(name) => attribute_keys(geometry, domain, name),
        // The caller reverses storage order directly; there is no key.
        SortMode::Reverse => Ok(SortKeys::Num(Vec::new())),
    }
}

/// One position per element of `domain`.
///
/// Points and instances have a `P` column. The primitive domain has none —
/// nothing writes one — so a primitive's position is the mean of its own
/// points, which is what "sort the shapes left to right" means. A primitive
/// with no vertices has no centroid and reads as the origin rather than
/// failing the whole sort.
fn element_positions(geometry: &Geometry, domain: Domain) -> Result<Vec<Vec3>, GeometryOpError> {
    if domain != Domain::Primitive {
        return Ok(positions(geometry, domain)?.iter3().collect());
    }
    let points = positions(geometry, Domain::Point)?;
    Ok(geometry
        .primitives()
        .iter()
        .map(|primitive| {
            let mut sum = Vec3(0.0, 0.0, 0.0);
            let mut vertices = 0.0f32;
            for vertex in primitive.verts().clone() {
                if let Some(point) = points.get3(vertex) {
                    sum = Vec3(sum.0 + point.0, sum.1 + point.1, sum.2 + point.2);
                    vertices += 1.0;
                }
            }
            if vertices == 0.0 {
                Vec3(0.0, 0.0, 0.0)
            } else {
                Vec3(sum.0 / vertices, sum.1 / vertices, sum.2 / vertices)
            }
        })
        .collect())
}

/// Keys read from a named attribute column of `domain`.
///
/// A vector or colour has no order of its own, so its **first** component is
/// the key — the component Houdini's Sort reads by default, and the one that
/// makes `Cd` sort by red and a `Vec2` sort by x.
fn attribute_keys(
    geometry: &Geometry,
    domain: Domain,
    name: &str,
) -> Result<SortKeys, GeometryOpError> {
    let column = geometry
        .attribute_set(domain)
        .get(name)
        .ok_or_else(|| GeometryError::AttributeNotFound { name: name.into() })?;
    macro_rules! first_component {
        ($values:expr, $component:tt) => {
            SortKeys::Num(
                $values
                    .iter()
                    .map(|value| value.$component as f64)
                    .collect(),
            )
        };
    }
    Ok(match column.as_ref() {
        AttributeArray::F32(values) => SortKeys::Num(values.iter().map(|v| *v as f64).collect()),
        AttributeArray::I32(values) => {
            SortKeys::Num(values.iter().map(|v| f64::from(*v)).collect())
        }
        AttributeArray::Bool(values) => {
            SortKeys::Num(values.iter().map(|v| f64::from(u8::from(*v))).collect())
        }
        AttributeArray::Str(values) => SortKeys::Text(values.clone()),
        AttributeArray::Vec2(values) => first_component!(values, 0),
        AttributeArray::Vec3(values) => first_component!(values, 0),
        AttributeArray::Vec4(values) => first_component!(values, 0),
        AttributeArray::Color(values) => first_component!(values, r),
    })
}

/// Arc length along `polyline` of the closest projection of each element.
///
/// Planar by construction: the polyline is 2D (see [`first_path`]) and a 3D
/// element projects onto xy, because "how far along the curve" is a question
/// about the curve's own parameter and depth cannot move it.
fn path_projections(elements: &[Vec3], polyline: &[Vec2], closed: bool) -> Vec<f64> {
    let mut edges: Vec<(Vec2, Vec2)> = polyline.windows(2).map(|pair| (pair[0], pair[1])).collect();
    if closed && let (Some(last), Some(first)) = (polyline.last(), polyline.first()) {
        edges.push((*last, *first));
    }
    elements
        .iter()
        .map(|element| {
            let target = Vec2(element.0, element.1);
            let (mut closest, mut at) = (f32::MAX, 0.0f32);
            let mut travelled = 0.0f32;
            for (start, end) in &edges {
                let edge = Vec2(end.0 - start.0, end.1 - start.1);
                let length_squared = edge.0 * edge.0 + edge.1 * edge.1;
                // A duplicated point makes a zero-length edge: it projects
                // onto its own start and advances nothing.
                let t = if length_squared <= f32::EPSILON {
                    0.0
                } else {
                    (((target.0 - start.0) * edge.0 + (target.1 - start.1) * edge.1)
                        / length_squared)
                        .clamp(0.0, 1.0)
                };
                let projection = Vec2(start.0 + edge.0 * t, start.1 + edge.1 * t);
                let distance = planar_distance_squared(projection, target);
                let length = length_squared.sqrt();
                if distance < closest {
                    closest = distance;
                    at = travelled + length * t;
                }
                travelled += length;
            }
            f64::from(at)
        })
        .collect()
}

/// The point index space split into the runs a permutation may not cross:
/// one per primitive, plus the gaps between and around them.
///
/// Overlapping runs would let a point be permuted twice, which is silent data
/// corruption rather than a wrong picture, so they are an error. Nothing
/// produces them today — every generator writes sequential runs and
/// `geometry.merge` shifts them — and this is what keeps that true.
fn vertex_runs(geometry: &Geometry, count: usize) -> Result<Vec<Range<usize>>, GeometryOpError> {
    let mut runs: Vec<Range<usize>> = geometry
        .primitives()
        .iter()
        .map(|primitive| primitive.verts().clone())
        .filter(|run| !run.is_empty())
        .collect();
    if runs.is_empty() {
        // No primitive claims a point, so the whole domain is one run and a
        // point cloud sorts freely.
        runs.push(0..count);
    }
    runs.sort_by_key(|run| run.start);
    let mut blocks = Vec::with_capacity(runs.len() * 2 + 1);
    let mut cursor = 0;
    for run in runs {
        if run.start < cursor {
            return Err(GeometryOpError::OverlappingVertexRuns {
                operation: "geometry.sort",
                point: run.start,
            });
        }
        if cursor < run.start {
            blocks.push(cursor..run.start);
        }
        cursor = run.end;
        blocks.push(run);
    }
    if cursor < count {
        blocks.push(cursor..count);
    }
    Ok(blocks)
}

/// `order` restricted to each run: the run's own elements, in the order they
/// appear globally, placed back into the run's own slots.
///
/// ponytail: O(runs × count), one filtering pass per run. Bucketing the order
/// by run in a single pass is the upgrade if a composition ever holds enough
/// primitives for this to show.
fn within_runs(order: &[usize], runs: &[Range<usize>]) -> Vec<usize> {
    let mut placed: Vec<usize> = (0..order.len()).collect();
    for run in runs {
        let mut sorted = order.iter().copied().filter(|index| run.contains(index));
        for slot in run.clone() {
            placed[slot] = sorted
                .next()
                .expect("a run has exactly as many elements as slots");
        }
    }
    placed
}

/// Deletes the elements of one domain that `group` selects (or, with `invert`,
/// the ones it does not), and everything that would dangle without them.
///
/// `group` follows the element-scope convention (REQ-CORE-013) with one
/// deliberate difference: **nothing selected is nothing deleted**. Elsewhere an
/// empty or unresolvable group means "every element", which for a deletion
/// would empty the geometry on a half-typed name. Here an empty name, a missing
/// column, a non-`Bool` one, or one of the wrong length returns the input
/// unchanged (the last three already warn through the shared resolver). To
/// delete everything, flag everything, or `invert` an empty selection.
///
/// What goes with a deleted element:
///
/// - **Points**: every primitive that referenced one of them. A path is a
///   contiguous run of points, so a path with a hole is not the same shape;
///   it goes whole (Houdini's rule). Surviving primitives have their `verts`
///   re-packed onto the shorter point list, and their attribute rows follow.
///   A deleted mesh leaves its triangles in the shared index buffer, unread.
/// - **Primitives**: nothing else. Their points stay, because other primitives
///   or a point cloud may still want them.
/// - **Instances**: sources no surviving instance stamps are dropped and
///   `source_index` renumbered, so the list does not carry geometry nobody
///   draws. Without a `source_index` column every instance stamps the first
///   source, which is then only dropped when no instance is left.
///
/// Every column of the blasted domain is selected through the same keep mask,
/// so the survivors are byte-identical to what they were; `index` is
/// renumbered to `0..n` (when the domain carries one) and `id` is not touched.
/// The detail domain has no elements to delete and passes through.
pub fn blast(
    geometry: &Geometry,
    domain: Domain,
    group: &str,
    invert: bool,
) -> Result<Geometry, GeometryOpError> {
    let count = domain_count(geometry, domain);
    if domain == Domain::Detail || count == 0 {
        return Ok(geometry.clone());
    }
    let Some(selected) =
        super::field::group_selection(geometry.attribute_set(domain), group, count)
    else {
        return Ok(geometry.clone());
    };
    // Selected elements go unless inverted; unselected ones go only if inverted.
    let keep: Vec<bool> = selected.iter().map(|inside| *inside == invert).collect();

    let mut result = geometry.clone();
    match domain {
        Domain::Point => {
            // `before[i]` is how many points survive ahead of point `i`, which
            // is both a survivor's new position and a run's new boundary.
            let mut before = Vec::with_capacity(count + 1);
            let mut kept = 0;
            before.push(0);
            for survives in &keep {
                kept += usize::from(*survives);
                before.push(kept);
            }
            let (mut primitives, mut primitive_keep) = (Vec::new(), Vec::new());
            for primitive in geometry.primitives() {
                let verts = primitive.verts();
                let intact = keep[verts.clone()].iter().all(|survives| *survives);
                primitive_keep.push(intact);
                if intact {
                    let verts = before[verts.start]..before[verts.end];
                    primitives.push(match primitive {
                        Primitive::Path { closed, .. } => Primitive::Path {
                            verts,
                            closed: *closed,
                        },
                        Primitive::Mesh { indices, .. } => Primitive::Mesh {
                            verts,
                            indices: indices.clone(),
                        },
                    });
                }
            }
            retain_rows(&mut result, Domain::Point, &keep)?;
            retain_rows(&mut result, Domain::Primitive, &primitive_keep)?;
            result.set_primitives(primitives);
        }
        Domain::Primitive => {
            retain_rows(&mut result, Domain::Primitive, &keep)?;
            let primitives = geometry
                .primitives()
                .iter()
                .zip(&keep)
                .filter(|(_, survives)| **survives)
                .map(|(primitive, _)| primitive.clone())
                .collect();
            result.set_primitives(primitives);
        }
        Domain::Instance => {
            retain_rows(&mut result, Domain::Instance, &keep)?;
            prune_sources(geometry, &mut result, &keep)?;
        }
        Domain::Detail => unreachable!("returned above"),
    }
    for domain in [Domain::Point, Domain::Primitive, Domain::Instance] {
        renumber_index(&mut result, domain)?;
    }
    result.validate()?;
    Ok(result)
}

/// Replaces `domain`'s columns with their rows where `keep` is set. A domain
/// with no columns has nothing to select.
fn retain_rows(
    geometry: &mut Geometry,
    domain: Domain,
    keep: &[bool],
) -> Result<(), GeometryOpError> {
    let mut retained = AttributeSet::new();
    for (name, column) in geometry.attribute_set(domain).iter() {
        let rows = keep
            .iter()
            .enumerate()
            .filter(|(_, survives)| **survives)
            .map(|(row, _)| row);
        retained.insert(name.as_str(), select_values(column, rows))?;
    }
    *geometry.attribute_set_mut(domain) = retained;
    Ok(())
}

/// Drops the instance sources that no surviving instance stamps.
///
/// `original` is the geometry before the rows were removed, since the
/// surviving rows' `source_index` values are read from it.
fn prune_sources(
    original: &Geometry,
    result: &mut Geometry,
    keep: &[bool],
) -> Result<(), GeometryOpError> {
    let sources = original.sources();
    if sources.is_empty() {
        return Ok(());
    }
    if !keep.iter().any(|survives| *survives) {
        result.set_sources(Vec::new());
        return Ok(());
    }
    let Some(column) = original.instances().get(names::SOURCE_INDEX) else {
        return Ok(());
    };
    let indices = column.as_i32(names::SOURCE_INDEX)?;
    let slots: Vec<usize> = keep
        .iter()
        .enumerate()
        .filter(|(_, survives)| **survives)
        .map(|(row, _)| source_slot(sources.len(), Some(indices), row))
        .collect();
    let mut used: Vec<usize> = slots.clone();
    used.sort_unstable();
    used.dedup();
    if used.len() == sources.len() {
        return Ok(());
    }
    result.set_sources(used.iter().map(|slot| sources[*slot].clone()).collect());
    let renumbered = slots
        .iter()
        .map(|slot| used.binary_search(slot).expect("every slot is used") as i32)
        .collect();
    result
        .instances_mut()
        .insert(names::SOURCE_INDEX, AttributeArray::I32(renumbered))?;
    Ok(())
}

/// Bounding-box center of point positions, falling back to instance positions
/// for instance-only geometry. Returns `None` when both are empty.
///
/// Always three components: a 2D geometry reports `z = 0`, which is the same
/// center it reported before 3D positions existed.
pub fn bounds_center(geometry: &Geometry) -> Option<Vec3> {
    let positions = [Domain::Point, Domain::Instance]
        .into_iter()
        .find_map(|domain| {
            geometry
                .positions(domain)?
                .ok()
                .filter(|positions| !positions.is_empty())
        })?;
    let mut min = Vec3(f32::MAX, f32::MAX, f32::MAX);
    let mut max = Vec3(f32::MIN, f32::MIN, f32::MIN);
    for position in positions.iter3() {
        min = Vec3(
            min.0.min(position.0),
            min.1.min(position.1),
            min.2.min(position.2),
        );
        max = Vec3(
            max.0.max(position.0),
            max.1.max(position.1),
            max.2.max(position.2),
        );
    }
    Some(Vec3(
        (min.0 + max.0) * 0.5,
        (min.1 + max.1) * 0.5,
        (min.2 + max.2) * 0.5,
    ))
}

/// Axis-aligned bounds of what a geometry draws: point positions, each
/// instance's source placed through the accumulated [`InstanceTransform`],
/// and the reach of the stroke its **attributes** ask for ([`stroke_reach`]).
///
/// The one answer to "how big is this geometry". [`GeometricData::bounds`]
/// and the Viewer's bbox overlay both come from here, so a rectangle drawn on
/// screen and one a node reads cannot disagree. What `positions_bounds`
/// measures — the Point domain's `P` column — is a *part* of this: an
/// instance geometry places nothing in that domain, so measuring it alone
/// reports a line of glyph origins for a `text.layout` and nothing at all for
/// a `geometry.from_image`.
///
/// # The walk is the rasterizer's walk
///
/// `rasterize::flatten_geometry` carries three things from the root down
/// through every source, and a bbox that does not carry the same three is
/// smaller than the picture:
///
/// * **the accumulated placement**, composed with
///   [`InstanceTransform::compose`], which is the exact affine product (a
///   non-uniform scale under a turn comes out as a `shear`). Flattening
///   ([`expand_instances`]) bakes the same product into the points, so the
///   bounds contain the expanded points.
/// * **the inherited `stroke_width`**, scaled by that placement's
///   [`InstanceTransform::uniform_scale`] at the level it is stroked, because
///   that is what the rasterizer strokes with. The bbox grows by the widest
///   width in play rather than per element (the plan's decision), so what
///   descends is the max of the level's own widest and what it inherited.
/// * **the root's `join`**, read once from the root's Detail and never again.
///   The rasterizer reads `join` / `cap` / `dash` at the node entry and
///   carries them down `Style::shape`; a source's own Detail is not read.
///
/// The instance columns and the cutoff are [`expand_instances`]': the same
/// `P` / `rot` / `scale` / `shear` / `source_index`, the same [`MAX_INSTANCE_DEPTH`],
/// the same clamping of an out-of-range `source_index`. What is flattened and
/// what is measured have to be the same set of elements.
///
/// # What it cannot see
///
/// **The `rasterize` node's own `stroke_width` parameter.** A path carrying no
/// `stroke_width` attribute is still stroked, at the width the node's
/// parameter says, and a geometry does not know which node will draw it — so
/// a function taking one geometry cannot bound that stroke. Attribute-derived
/// widths (what `style.stroke` writes, on any domain) are included; the base
/// parameter is the open half of `LOW-APP-33`.
///
/// `None` when the geometry draws nothing at all — an empty geometry has no
/// rectangle, and a zero-sized one at the origin would be a lie.
///
/// # Cost
///
/// **Walked in full on every call, including once per pointer move**: the
/// Viewer's hover hint asks for the selected nodes' bounds and a click asks
/// for every node's, both through `viewer::geometry::geometry_bounds`, which
/// is this function and a type conversion. Measured rather than assumed,
/// release build, per call, for a flat point cloud:
///
/// | points | per call |
/// |---|---|
/// | 1 000 | 0.37 µs |
/// | 10 000 | 1.9 µs |
/// | 100 000 | 20 µs |
/// | 1 000 000 | 197 µs |
///
/// A source with **no instance domain of its own** is measured once however
/// many instances stamp it, which keeps the instance path
/// `O(sources × points + instances)` — glyph outlines, images and the shapes
/// a `scatter` strews are all that case. A source that nests further is
/// re-walked per instance, because its extent then depends on the placement
/// it is walked with; [`MAX_INSTANCE_DEPTH`] bounds that at four levels. The
/// two `#[ignore]`d `drawn_bounds_costs_…` / `a_stamped_source_is_measured_once…`
/// tests pin both halves.
///
/// A pointer move pays this for the handful of selected nodes, so even a
/// hundred-thousand-point geometry costs ~0.1% of a 60 Hz frame. Caching the
/// rectangle at press time would buy that back and cost a second source of
/// truth for what the bbox is — worth doing only if a profile ever shows this
/// line, which at these numbers it will not. Unlike `MED-GPU-04`, the work
/// here is `O(points)` once per input event, not
/// `O(primitives × resolution)` per frame.
pub fn drawn_bounds(geometry: &Geometry) -> Option<Rect> {
    // `join` comes from the **root's** Detail and nothing else: `rasterize`
    // reads it once at the node entry (`detail_join(geo.detail())`) and
    // carries it down every source through `Style::shape`, so a source's own
    // `join` is never read. Reading it per level would bound a miter the
    // picture does not have — and miss the one it does.
    let miter = root_miter(geometry);
    drawn_bounds_at(geometry, 0, InstanceTransform::IDENTITY, 0.0, miter)
}

/// [`drawn_bounds`] mid-walk, carrying what `rasterize::flatten_geometry`
/// carries: the placement accumulated from the root, the `stroke_width`
/// inherited from the enclosing instance, and the root's join.
///
/// `depth` is counted the way [`expand_at`] counts it — the top-level
/// geometry is depth 0, and the instances of a geometry at
/// [`MAX_INSTANCE_DEPTH`] are not reached.
fn drawn_bounds_at(
    geometry: &Geometry,
    depth: u32,
    placement: InstanceTransform,
    inherited_width: f32,
    miter: bool,
) -> Option<Rect> {
    let (local, own_width, outside) = local_extent(geometry);
    // The **max** of the two, not "its own, else inherited". Per element
    // `rasterize` narrows the attribute over the inherited value, but the
    // bbox grows by one width for the whole geometry, so the only safe upper
    // bound is the widest either of them asks for. An outside-aligned stroke
    // lies wholly outside the path, so its reach is the full width, not half:
    // doubling the width gives `stroke_reach` that reach.
    let width = own_width.max(inherited_width);
    let width = if outside { width * 2.0 } else { width };
    union(
        placed_ink(local, placement, width, miter),
        instance_bounds(geometry, depth, placement, width, miter),
    )
}

/// A geometry's extent in **its own** space, and the widest stroke its own
/// elements ask for.
///
/// The two `O(points)` reads of the walk, kept together so a source stamped
/// by many instances pays them once (see the cache in [`instance_bounds`]).
/// Neither depends on where the geometry is placed.
///
/// The Instance domain's `stroke_width` is deliberately **not** folded in:
/// `rasterize` narrows it onto what that instance *stamps*
/// (`element_style(style, instances, index)`), not onto the host's own
/// primitives, so [`instance_bounds`] passes it down per instance instead —
/// which is both tighter and where it actually applies.
fn local_extent(geometry: &Geometry) -> (Option<Rect>, f32, bool) {
    let widest = [Domain::Detail, Domain::Primitive, Domain::Point]
        .into_iter()
        .filter_map(|domain| {
            geometry
                .attribute_set(domain)
                .get(names::STROKE_WIDTH)?
                .as_f32(names::STROKE_WIDTH)
                .ok()
                .map(|widths| widths.iter().copied().fold(0.0_f32, f32::max))
        })
        .fold(0.0_f32, f32::max);
    // Any outside-aligned primitive: the bound is one per geometry, like the
    // width's, so one such element widens the reach for all of them.
    let outside = geometry
        .primitive_attrs()
        .get(names::STROKE_ALIGN)
        .and_then(|column| column.as_i32(names::STROKE_ALIGN).ok())
        .is_some_and(|aligns| aligns.contains(&names::STROKE_ALIGN_OUTSIDE));
    (
        union(geometry.positions_bounds(), control_hull_bounds(geometry)),
        widest,
        outside,
    )
}

/// The extent of the path control points — anchors together with
/// `P + in_tan` and `P + out_tan` — or `None` when the geometry carries no
/// tangents.
///
/// `positions_bounds` measures the anchors, and **a cubic leaves them**: two
/// anchors on one horizontal line with both tangents pointing up bulge 45 px
/// above it for a handle length of 60, and that bulge is drawn
/// (`rasterize::path_polyline` hands `in_tan` / `out_tan` to
/// `flatten::flatten_path`, which is also what the GPU shader evaluates).
/// Anchors alone would report zero height for it.
///
/// A Bézier never leaves the convex hull of its control points, so this is
/// the bound that cannot be too small, and it stays a **column scan** rather
/// than a per-segment root solve — the walk is paid once per pointer move.
/// Generous only where a curve does not reach its own handles, and it costs
/// text nothing: a font puts its anchors on the extrema, so a glyph's hull
/// and its ink are the same rectangle (measured on the bundled Geist
/// Regular, `"Ravel"` at 72 px, to the last decimal).
///
/// Tangents are a 2D attribute, so a 3D `P` column reads as no hull at all
/// and the anchors answer alone.
fn control_hull_bounds(geometry: &Geometry) -> Option<Rect> {
    let points = geometry.points();
    let positions = points.get(names::P)?.as_vec2(names::P).ok()?;
    let tangents: Vec<&[Vec2]> = [names::IN_TAN, names::OUT_TAN]
        .into_iter()
        .filter_map(|name| points.get(name)?.as_vec2(name).ok())
        .collect();
    if tangents.is_empty() {
        return None;
    }
    placed_bounds(positions.iter().enumerate().flat_map(|(index, p)| {
        std::iter::once(*p).chain(
            tangents
                .iter()
                .filter_map(move |column| column.get(index))
                .map(move |t| Vec2(p.0 + t.0, p.1 + t.1)),
        )
    }))
}

/// Whether the geometry joins its corners with a miter. A Detail attribute —
/// one value for the whole geometry — and absent means round, as it does in
/// the rasterizer.
fn root_miter(geometry: &Geometry) -> bool {
    geometry
        .detail()
        .get(names::JOIN)
        .and_then(|column| column.as_i32(names::JOIN).ok())
        .is_some_and(|joins| joins.first() == Some(&names::JOIN_MITER))
}

/// A local rectangle put where `placement` puts it, grown by the stroke that
/// covers it.
///
/// The reach is measured **after** the placement, because `rasterize` scales
/// the width by the accumulated placement before it strokes
/// (`self.key.stroke_width * self.scale`, and `PathRun::new(…,
/// placement.uniform_scale())` on the GPU path). A reach added in the
/// source's own space and then scaled would be right only for a scale of 1.
fn placed_ink(
    local: Option<Rect>,
    placement: InstanceTransform,
    width: f32,
    miter: bool,
) -> Option<Rect> {
    let rect = placed_rect(local?, placement)?;
    if width <= 0.0 {
        return Some(rect);
    }
    Some(grown(
        rect,
        stroke_reach(width * placement.uniform_scale(), miter),
    ))
}

/// Bounds of what this geometry's instance domain stamps, each source placed
/// by the composition of `placement` with that instance's own transform.
///
/// `InstanceTransform::compose` rather than applying the two placements in
/// turn: it is the exact affine product, carried as one placement so a
/// nesting stays `O(depth)` per instance. A 20×2 image turned a quarter-turn
/// inside a 10×-wide instance is 20×200 once placed, and the bounds say so.
fn instance_bounds(
    geometry: &Geometry,
    depth: u32,
    placement: InstanceTransform,
    inherited_width: f32,
    miter: bool,
) -> Option<Rect> {
    // The depth `rasterize` stops drawing at and `expand_at` stops
    // flattening at. Measuring deeper would bound elements that do not exist.
    if depth >= MAX_INSTANCE_DEPTH {
        return None;
    }
    let offsets = geometry.positions(Domain::Instance)?.ok()?;
    let sources = geometry.sources();
    if sources.is_empty() {
        // Nothing is stamped — `rasterize` draws no instance and `expand_at`
        // drops the domain. The placements are still elements the Viewer
        // marks, and a `scatter.*` whose source input is unwired is exactly
        // that, so the extent of the placements is the honest answer.
        return placed_bounds(offsets.iter3().map(|p| placement.apply(Vec2(p.0, p.1))));
    }
    // A 3D instance domain places nothing: `rasterize` reads `P` as `Vec2`
    // and `expand_at` requires it planar, so both draw nothing here too.
    let offsets = offsets.planar()?;
    let instances = geometry.instances();
    let columns = InstanceColumns::lenient(instances);
    let source_indices = instances
        .get(names::SOURCE_INDEX)
        .and_then(|column| column.as_i32(names::SOURCE_INDEX).ok());
    let widths = instances
        .get(names::STROKE_WIDTH)
        .and_then(|column| column.as_f32(names::STROKE_WIDTH).ok());

    // The `O(points)` half of a source, measured once however many instances
    // stamp it. Only for a source with **no instance domain of its own**:
    // that is exactly when its local extent does not depend on where it is
    // placed, so caching it costs nothing in correctness. Glyph outlines,
    // images and the shapes a `scatter` strews are all this case, which is
    // why the hot path stays `O(sources × points + instances)`; a source
    // that nests further is re-walked per instance, bounded by
    // `MAX_INSTANCE_DEPTH`.
    let mut leaf: Vec<Option<(Option<Rect>, f32, bool)>> = vec![None; sources.len()];

    let mut bounds = None;
    for (index, offset) in offsets.iter().enumerate() {
        let local = columns.placement(index, *offset);
        let next = InstanceTransform::compose(placement, local);
        let width = inherited_width.max(
            widths
                .and_then(|values| values.get(index).copied())
                .unwrap_or(0.0),
        );
        let slot = source_slot(sources.len(), source_indices, index);
        let placed = match &sources[slot] {
            // An image has no contour, so nothing strokes it.
            InstanceSource::Image(image) => placed_rect(image.rect(), next),
            InstanceSource::Geometry(source) if source.instance_count() == 0 => {
                let (extent, own_width, outside) =
                    *leaf[slot].get_or_insert_with(|| local_extent(source));
                let width = width.max(own_width);
                placed_ink(
                    extent,
                    next,
                    if outside { width * 2.0 } else { width },
                    miter,
                )
            }
            InstanceSource::Geometry(source) => {
                drawn_bounds_at(source, depth + 1, next, width, miter)
            }
        };
        bounds = union(bounds, placed);
    }
    bounds
}

/// The rectangle each instance occupies once its source is stamped: the
/// source's own extent times the instance's scale, turn and shear, put at its
/// `P`. One entry per instance, in index order, with the same walk
/// [`drawn_bounds`] uses (so a source's stroke reach is included, a nested
/// instance domain is followed, and an out-of-range `source_index` clamps).
///
/// An instance whose source draws nothing, or a geometry with no source at
/// all, gets the zero-sized rectangle at its own placement — it still is an
/// element, it just has no size. `None` when there is no instance domain, or
/// its `P` is 3D (the walk is planar, as everywhere else).
///
/// Lives beside [`measure`] because `geometry.distribute` is the second reader
/// of "how big is this element" and must not re-derive it.
pub fn instance_extents(geometry: &Geometry) -> Option<Vec<Rect>> {
    let offsets = geometry.positions(Domain::Instance)?.ok()?.planar()?;
    let sources = geometry.sources();
    let instances = geometry.instances();
    let columns = InstanceColumns::lenient(instances);
    let source_indices = instances
        .get(names::SOURCE_INDEX)
        .and_then(|column| column.as_i32(names::SOURCE_INDEX).ok());
    let miter = root_miter(geometry);
    Some(
        offsets
            .iter()
            .enumerate()
            .map(|(index, offset)| {
                let placement = columns.placement(index, *offset);
                let rect = if sources.is_empty() {
                    None
                } else {
                    match &sources[source_slot(sources.len(), source_indices, index)] {
                        InstanceSource::Image(image) => placed_rect(image.rect(), placement),
                        InstanceSource::Geometry(source) => {
                            drawn_bounds_at(source, 1, placement, 0.0, miter)
                        }
                    }
                };
                rect.unwrap_or(Rect {
                    x: placement.offset.0,
                    y: placement.offset.1,
                    width: 0.0,
                    height: 0.0,
                })
            })
            .collect(),
    )
}

/// A source rectangle placed by one instance: the axis-aligned bounds of its
/// four placed corners.
///
/// The corners rather than the rectangle, because a turned rectangle is not
/// one: rotating `(x, y, width, height)` as if it were a rectangle would
/// report the source's own extent at a new position and lose every pixel the
/// turn pushed outside it.
fn placed_rect(rect: Rect, placement: InstanceTransform) -> Option<Rect> {
    placed_bounds(
        [
            Vec2(rect.x, rect.y),
            Vec2(rect.x + rect.width, rect.y),
            Vec2(rect.x + rect.width, rect.y + rect.height),
            Vec2(rect.x, rect.y + rect.height),
        ]
        .into_iter()
        .map(|corner| placement.apply(corner)),
    )
}

/// Axis-aligned bounds of a stream of points, or `None` when it is empty.
fn placed_bounds(points: impl IntoIterator<Item = Vec2>) -> Option<Rect> {
    let mut points = points.into_iter();
    let first = points.next()?;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (first.0, first.1, first.0, first.1);
    for point in points {
        min_x = min_x.min(point.0);
        min_y = min_y.min(point.1);
        max_x = max_x.max(point.0);
        max_y = max_y.max(point.1);
    }
    Some(Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    })
}

/// How far past the path a stroke of `width` can reach, joins included: a
/// miter spike runs out to `miter_limit` half-widths.
///
/// One answer for two callers that have to agree: `rasterize` sizes the
/// rectangle it blends a stroke's coverage into with this, and
/// [`drawn_bounds`] grows a geometry's extent by it. A bbox that computed the
/// reach itself would be a second answer, and the one that is too small is
/// the one that clips the picture.
///
/// `miter` rather than a join enum because that is the whole of what the
/// reach depends on, and the join is spelled `zeno::Join` in the rasterizer —
/// a type this crate does not (and must not) depend on.
///
/// The extra pixel is the antialiased edge: zeno writes coverage into the
/// pixel the boundary passes through, so the outermost covered pixel is one
/// past the geometric reach.
///
/// `width` is a width **already scaled by the placement it is stroked at**:
/// the rasterizer strokes with `stroke_width * placement.uniform_scale()`, so
/// a caller measuring a stamped source has to scale before it asks.
pub fn stroke_reach(width: f32, miter: bool) -> f32 {
    let half_widths = if miter { ZENO_MITER_LIMIT } else { 1.0 };
    width * 0.5 * half_widths + 1.0
}

/// zeno's default miter limit (`zeno::Stroke::default`), which the rasterizer
/// does not change.
///
/// An upper bound on the reach, not the reach itself: zeno also bevels any
/// turn sharper than a right angle, so a miter never actually exceeds √2
/// half-widths. Sizing by the declared limit costs a few pixels of scan and
/// does not depend on that second rule staying true.
const ZENO_MITER_LIMIT: f32 = 4.0;

/// `rect` grown by `reach` on every side.
fn grown(rect: Rect, reach: f32) -> Rect {
    Rect {
        x: rect.x - reach,
        y: rect.y - reach,
        width: rect.width + reach * 2.0,
        height: rect.height + reach * 2.0,
    }
}

/// The smallest rectangle containing both, with either side absent.
fn union(left: Option<Rect>, right: Option<Rect>) -> Option<Rect> {
    match (left, right) {
        (Some(left), Some(right)) => {
            let (x, y) = (left.x.min(right.x), left.y.min(right.y));
            Some(Rect {
                x,
                y,
                width: (left.x + left.width).max(right.x + right.width) - x,
                height: (left.y + left.height).max(right.y + right.height) - y,
            })
        }
        (some, None) | (None, some) => some,
    }
}

fn domain_count(geometry: &Geometry, domain: Domain) -> usize {
    match domain {
        Domain::Point => geometry.point_count(),
        Domain::Primitive => geometry.primitive_count(),
        Domain::Instance => geometry.instance_count(),
        Domain::Detail => 1,
    }
}

fn positions(geometry: &Geometry, domain: Domain) -> Result<Positions<'_>, GeometryOpError> {
    Ok(geometry
        .positions(domain)
        .ok_or_else(|| GeometryError::AttributeNotFound {
            name: names::P.into(),
        })??)
}

pub(super) fn broadcast_value(value: &AttributeValue, count: usize) -> AttributeArray {
    match value {
        AttributeValue::F32(value) => AttributeArray::F32(vec![*value; count]),
        AttributeValue::Vec2(value) => AttributeArray::Vec2(vec![*value; count]),
        AttributeValue::Vec3(value) => AttributeArray::Vec3(vec![*value; count]),
        AttributeValue::Vec4(value) => AttributeArray::Vec4(vec![*value; count]),
        AttributeValue::Color(value) => AttributeArray::Color(vec![*value; count]),
        AttributeValue::I32(value) => AttributeArray::I32(vec![*value; count]),
        AttributeValue::Bool(value) => AttributeArray::Bool(vec![*value; count]),
        AttributeValue::Str(value) => AttributeArray::Str(vec![value.clone(); count]),
    }
}

fn repeat_first(column: &AttributeArray, count: usize) -> Result<AttributeArray, GeometryOpError> {
    macro_rules! first {
        ($values:expr, $variant:ident) => {
            AttributeArray::$variant(vec![
                $values
                    .first()
                    .cloned()
                    .ok_or(GeometryOpError::EmptyDomain)?;
                count
            ])
        };
    }
    Ok(match column {
        AttributeArray::F32(values) => first!(values, F32),
        AttributeArray::Vec2(values) => first!(values, Vec2),
        AttributeArray::Vec3(values) => first!(values, Vec3),
        AttributeArray::Vec4(values) => first!(values, Vec4),
        AttributeArray::Color(values) => first!(values, Color),
        AttributeArray::I32(values) => first!(values, I32),
        AttributeArray::Bool(values) => first!(values, Bool),
        AttributeArray::Str(values) => first!(values, Str),
    })
}

fn reduce_and_repeat(
    column: &AttributeArray,
    count: usize,
    mode: AggregateMode,
) -> Result<AttributeArray, GeometryOpError> {
    if mode == AggregateMode::First {
        return repeat_first(column, count);
    }
    Ok(match column {
        AttributeArray::F32(values) => {
            let value = if mode == AggregateMode::Max {
                values
                    .iter()
                    .copied()
                    .reduce(f32::max)
                    .ok_or(GeometryOpError::EmptyDomain)?
            } else {
                values.iter().sum::<f32>() / values.len() as f32
            };
            AttributeArray::F32(vec![value; count])
        }
        AttributeArray::Vec2(values) => {
            let value = reduce_components(
                values.len(),
                2,
                mode,
                values.iter().map(|v| [v.0, v.1, 0.0, 0.0]),
            );
            AttributeArray::Vec2(vec![Vec2(value[0], value[1]); count])
        }
        AttributeArray::Vec3(values) => {
            let value = reduce_components(
                values.len(),
                3,
                mode,
                values.iter().map(|v| [v.0, v.1, v.2, 0.0]),
            );
            AttributeArray::Vec3(vec![Vec3(value[0], value[1], value[2]); count])
        }
        AttributeArray::Vec4(values) => {
            let value = reduce_components(
                values.len(),
                4,
                mode,
                values.iter().map(|v| [v.0, v.1, v.2, v.3]),
            );
            AttributeArray::Vec4(vec![Vec4(value[0], value[1], value[2], value[3]); count])
        }
        AttributeArray::Color(values) => {
            let mut output = if mode == AggregateMode::Max {
                [f32::NEG_INFINITY; 4]
            } else {
                [0.0; 4]
            };
            for value in values {
                for (slot, input) in output.iter_mut().zip([value.r, value.g, value.b, value.a]) {
                    *slot = if mode == AggregateMode::Max {
                        (*slot).max(input)
                    } else {
                        *slot + input
                    };
                }
            }
            if mode == AggregateMode::Average {
                for value in &mut output {
                    *value /= values.len() as f32;
                }
            }
            AttributeArray::Color(vec![
                Color {
                    r: output[0],
                    g: output[1],
                    b: output[2],
                    a: output[3]
                };
                count
            ])
        }
        AttributeArray::I32(values) => {
            let value = if mode == AggregateMode::Max {
                *values.iter().max().ok_or(GeometryOpError::EmptyDomain)?
            } else {
                (values.iter().map(|value| i64::from(*value)).sum::<i64>() / values.len() as i64)
                    as i32
            };
            AttributeArray::I32(vec![value; count])
        }
        AttributeArray::Bool(_) | AttributeArray::Str(_) => {
            return Err(GeometryOpError::UnsupportedAttributeType {
                operation: "aggregation",
                attribute_type: column.attr_type(),
            });
        }
    })
}

fn reduce_components(
    count: usize,
    components: usize,
    mode: AggregateMode,
    values: impl Iterator<Item = [f32; 4]>,
) -> [f32; 4] {
    let mut output = if mode == AggregateMode::Max {
        [f32::NEG_INFINITY; 4]
    } else {
        [0.0; 4]
    };
    for value in values {
        for index in 0..components {
            output[index] = if mode == AggregateMode::Max {
                output[index].max(value[index])
            } else {
                output[index] + value[index]
            };
        }
    }
    if mode == AggregateMode::Average {
        for value in &mut output[..components] {
            *value /= count as f32;
        }
    }
    output
}

/// Blend `source` into one value per target using precomputed weights.
///
/// Each arm folds over the target's own neighbour list rather than the whole
/// source column, so the work is `target_count × stride` instead of
/// `target_count × source_count`.
fn transfer_weighted(
    source: &AttributeArray,
    weights: &SparseWeights,
) -> Result<AttributeArray, GeometryOpError> {
    let targets = 0..weights.target_count();
    /// Folds every target's neighbours into an accumulated value.
    macro_rules! blend {
        ($values:expr, $variant:ident, $zero:expr, $add:expr) => {
            AttributeArray::$variant(
                targets
                    .map(|target| {
                        weights
                            .weights_of(target)
                            .iter()
                            .fold($zero, |sum, (index, weight)| {
                                #[allow(clippy::redundant_closure_call)]
                                $add(sum, *weight, &$values[*index])
                            })
                    })
                    .collect(),
            )
        };
    }
    Ok(match source {
        AttributeArray::F32(values) => {
            blend!(values, F32, 0.0f32, |sum: f32, w: f32, v: &f32| sum + w * v)
        }
        AttributeArray::Vec2(values) => blend!(
            values,
            Vec2,
            Vec2(0.0, 0.0),
            |sum: Vec2, w: f32, v: &Vec2| Vec2(sum.0 + w * v.0, sum.1 + w * v.1)
        ),
        AttributeArray::Vec3(values) => blend!(
            values,
            Vec3,
            Vec3(0.0, 0.0, 0.0),
            |sum: Vec3, w: f32, v: &Vec3| Vec3(sum.0 + w * v.0, sum.1 + w * v.1, sum.2 + w * v.2)
        ),
        AttributeArray::Vec4(values) => blend!(
            values,
            Vec4,
            Vec4(0.0, 0.0, 0.0, 0.0),
            |sum: Vec4, w: f32, v: &Vec4| Vec4(
                sum.0 + w * v.0,
                sum.1 + w * v.1,
                sum.2 + w * v.2,
                sum.3 + w * v.3
            )
        ),
        AttributeArray::Color(values) => blend!(
            values,
            Color,
            Color::TRANSPARENT,
            |sum: Color, w: f32, v: &Color| Color {
                r: sum.r + w * v.r,
                g: sum.g + w * v.g,
                b: sum.b + w * v.b,
                a: sum.a + w * v.a,
            }
        ),
        // Rounded once at the end, as before: accumulating in f32 and
        // rounding per target is what the exhaustive version did.
        AttributeArray::I32(values) => AttributeArray::I32(
            targets
                .map(|target| {
                    weights
                        .weights_of(target)
                        .iter()
                        .map(|(index, weight)| weight * values[*index] as f32)
                        .sum::<f32>()
                        .round() as i32
                })
                .collect(),
        ),
        AttributeArray::Bool(_) | AttributeArray::Str(_) => {
            return Err(GeometryOpError::UnsupportedAttributeType {
                operation: "distance-weighted transfer",
                attribute_type: source.attr_type(),
            });
        }
    })
}

/// Splits `geometry` into one geometry per distinct value of the `I32`
/// primitive attribute `attribute`, **ordered by ascending value** (never by
/// hash order, so the result is deterministic).
///
/// A piece holds its primitives with their primitive attributes, the points
/// they run over (copied per primitive, so two primitives sharing a point get
/// one each) with the point attributes, and the detail. A point no primitive
/// runs over belongs to no piece and is not in any of them. Instances are not
/// split: a geometry carrying some is an error rather than silently losing
/// them. A geometry without primitives has no pieces.
pub fn split_by_piece(
    geometry: &Geometry,
    attribute: &str,
) -> Result<Vec<Geometry>, GeometryOpError> {
    if geometry.instance_count() > 0 {
        return Err(GeometryOpError::HasInstances {
            operation: "split by piece",
        });
    }
    if geometry.primitive_count() == 0 {
        return Ok(Vec::new());
    }
    geometry.validate()?;
    let column = geometry.primitive_attrs().get(attribute).ok_or_else(|| {
        GeometryError::AttributeNotFound {
            name: attribute.into(),
        }
    })?;
    let mut groups: std::collections::BTreeMap<i32, Vec<usize>> = Default::default();
    for (index, piece) in column.as_i32(attribute)?.iter().enumerate() {
        groups.entry(*piece).or_default().push(index);
    }
    Ok(groups
        .into_values()
        .map(|members| piece_of(geometry, &members))
        .collect())
}

/// The geometry made of the primitives `members` (indices into `geometry`).
fn piece_of(geometry: &Geometry, members: &[usize]) -> Geometry {
    let mut point_sources: Vec<usize> = Vec::new();
    let runs: Vec<Range<usize>> = members
        .iter()
        .map(|&member| {
            let start = point_sources.len();
            point_sources.extend(geometry.primitives()[member].verts().clone());
            start..point_sources.len()
        })
        .collect();

    let mut out = Geometry::new();
    for (name, source) in geometry.points().iter() {
        let column = select_values(source, point_sources.iter().copied());
        out.points_mut()
            .insert(name.clone(), column)
            .expect("columns of one length");
    }
    for (name, source) in geometry.primitive_attrs().iter() {
        let column = select_values(source, members.iter().copied());
        out.primitive_attrs_mut()
            .insert(name.clone(), column)
            .expect("columns of one length");
    }
    *out.detail_mut() = geometry.detail().clone();
    for (&member, verts) in members.iter().zip(runs) {
        match &geometry.primitives()[member] {
            Primitive::Path { closed, .. } => out.push_primitive(Primitive::Path {
                verts,
                closed: *closed,
            }),
            Primitive::Mesh { indices, .. } => {
                out.push_mesh(verts, &geometry.indices()[indices.clone()])
            }
        }
    }
    out
}

fn select_values(source: &AttributeArray, indices: impl Iterator<Item = usize>) -> AttributeArray {
    let indices = indices.collect::<Vec<_>>();
    macro_rules! select {
        ($values:expr, $variant:ident) => {
            AttributeArray::$variant(
                indices
                    .iter()
                    .map(|index| $values[*index].clone())
                    .collect(),
            )
        };
    }
    match source {
        AttributeArray::F32(values) => select!(values, F32),
        AttributeArray::Vec2(values) => select!(values, Vec2),
        AttributeArray::Vec3(values) => select!(values, Vec3),
        AttributeArray::Vec4(values) => select!(values, Vec4),
        AttributeArray::Color(values) => select!(values, Color),
        AttributeArray::I32(values) => select!(values, I32),
        AttributeArray::Bool(values) => select!(values, Bool),
        AttributeArray::Str(values) => select!(values, Str),
    }
}

// ---------------------------------------------------------------------------
// Spatial partition for attribute transfer (MED-CORE-05)
// ---------------------------------------------------------------------------

/// Source-point count below which a linear scan beats building a grid.
///
/// Small transfers are the common case in tests and simple graphs, and they
/// keep the exact arithmetic they always had: below this the grid is never
/// built and every code path here is the original one.
const GRID_MIN_POINTS: usize = 64;

/// How many nearest source points a [`TransferMode::DistanceWeighted`]
/// transfer blends.
///
/// Inverse-distance weighting over *every* source point is O(source × target)
/// and visually indistinguishable from a truncated kernel: the 1/d weights of
/// distant points are tiny before normalisation and negligible after it.
/// Houdini's attribute transfer truncates the same way.
///
/// **Not a parameter.** The `attribute.transfer` node exposes `mode` and
/// nothing else (`crates/ravel-nodes/src/attribute/mod.rs`), so making the
/// neighbour count adjustable is a node signature change and belongs with
/// whoever adds the control. Until then the constant is the contract, and it
/// is chosen so that it only ever engages on inputs big enough for the
/// difference to be invisible: a transfer whose source has at most this many
/// points blends **all** of them, exactly as before.
const DISTANCE_WEIGHTED_NEIGHBOURS: usize = 8;

/// A uniform grid over the source positions of one transfer.
///
/// Deliberately local to this file rather than a general `geometry` facility:
/// attribute transfer is the only op that needs a spatial index today, and
/// the right shape for a shared one is not yet knowable from a single caller.
/// Promote it when a second op asks.
///
/// Queries are **exact** — the ring search below only stops once the grid
/// geometry proves no unscanned cell can hold anything closer — so
/// [`TransferMode::Nearest`] returns precisely what the linear scan returned,
/// ties included.
struct PointGrid {
    min: Vec3,
    /// Edge length of a cell, strictly positive.
    cell: f32,
    /// Cells along x, y, z.
    dims: [usize; 3],
    /// Cell index → the source point indices that fall in it.
    cells: Vec<Vec<u32>>,
}

impl PointGrid {
    /// Index `points`, or `None` when a linear scan is the better answer
    /// (too few points, or an extent of zero on every axis).
    fn build(points: &[Vec3]) -> Option<Self> {
        if points.len() < GRID_MIN_POINTS {
            return None;
        }
        let mut min = points[0];
        let mut max = points[0];
        for p in points {
            min = Vec3(min.0.min(p.0), min.1.min(p.1), min.2.min(p.2));
            max = Vec3(max.0.max(p.0), max.1.max(p.1), max.2.max(p.2));
        }
        let extent = [max.0 - min.0, max.1 - min.1, max.2 - min.2];
        if !extent.iter().all(|e| e.is_finite()) {
            return None;
        }
        // Size the cells off the *occupied* axes only: planar geometry (the
        // usual case) would otherwise get a cube grid whose z dimension is
        // one cell thick and whose x/y cells are far too coarse.
        let spread = extent.iter().filter(|e| **e > 0.0).count();
        if spread == 0 {
            return None; // every point coincides
        }
        let per_axis = (points.len() as f64)
            .powf(1.0 / spread as f64)
            .ceil()
            .max(1.0);
        let longest = extent.iter().cloned().fold(0.0f32, f32::max);
        let cell = (longest / per_axis as f32).max(f32::MIN_POSITIVE);
        let dims = [0, 1, 2].map(|axis| {
            if extent[axis] > 0.0 {
                ((extent[axis] / cell).ceil() as usize + 1).max(1)
            } else {
                1
            }
        });
        let total = dims[0].checked_mul(dims[1])?.checked_mul(dims[2])?;
        // A grid far larger than the point count buys nothing and costs
        // memory; fall back rather than allocate it.
        if total > points.len().saturating_mul(4) + 64 {
            return None;
        }
        let mut grid = Self {
            min,
            cell,
            dims,
            cells: vec![Vec::new(); total],
        };
        for (index, point) in points.iter().enumerate() {
            let coord = grid.coord_of(*point);
            let flat = grid.flatten(coord);
            grid.cells[flat].push(index as u32);
        }
        Some(grid)
    }

    /// Grid coordinate holding `point`, clamped into the grid.
    fn coord_of(&self, point: Vec3) -> [usize; 3] {
        let raw = [
            point.0 - self.min.0,
            point.1 - self.min.1,
            point.2 - self.min.2,
        ];
        [0, 1, 2].map(|axis| {
            let index = (raw[axis] / self.cell).floor();
            if index < 0.0 {
                0
            } else {
                (index as usize).min(self.dims[axis] - 1)
            }
        })
    }

    fn flatten(&self, coord: [usize; 3]) -> usize {
        (coord[2] * self.dims[1] + coord[1]) * self.dims[0] + coord[0]
    }

    /// The largest ring index that can still contain an unscanned cell.
    fn max_ring(&self, centre: [usize; 3]) -> usize {
        (0..3)
            .map(|axis| centre[axis].max(self.dims[axis] - 1 - centre[axis]))
            .max()
            .unwrap_or(0)
    }

    /// Visit every cell at Chebyshev ring exactly `ring` around `centre`.
    fn for_each_in_ring(&self, centre: [usize; 3], ring: usize, mut visit: impl FnMut(&[u32])) {
        let bounds = |axis: usize| {
            let low = centre[axis].saturating_sub(ring);
            let high = (centre[axis] + ring).min(self.dims[axis] - 1);
            low..=high
        };
        for z in bounds(2) {
            for y in bounds(1) {
                for x in bounds(0) {
                    // Only the shell: an interior cell was scanned already.
                    let on_shell = [x, y, z]
                        .iter()
                        .enumerate()
                        .any(|(axis, v)| v.abs_diff(centre[axis]) == ring);
                    if !on_shell {
                        continue;
                    }
                    visit(&self.cells[self.flatten([x, y, z])]);
                }
            }
        }
    }

    /// Index of the point nearest `target`, matching the linear scan's tie
    /// rule (lowest index wins).
    fn nearest(&self, points: &[Vec3], target: Vec3) -> usize {
        let centre = self.coord_of(target);
        let max_ring = self.max_ring(centre);
        let mut best: Option<(f32, usize)> = None;
        for ring in 0..=max_ring {
            self.for_each_in_ring(centre, ring, |bucket| {
                for index in bucket {
                    let index = *index as usize;
                    let distance = distance_squared(points[index], target);
                    let better = match best {
                        None => true,
                        Some((best_distance, best_index)) => {
                            distance < best_distance
                                || (distance == best_distance && index < best_index)
                        }
                    };
                    if better {
                        best = Some((distance, index));
                    }
                }
            });
            // Anything still unscanned sits at least `ring * cell` away, so a
            // best already inside that radius cannot be beaten.
            if let Some((best_distance, _)) = best {
                let reach = ring as f32 * self.cell;
                if best_distance <= reach * reach {
                    break;
                }
            }
        }
        // SAFETY of expect: the grid holds every point and the caller
        // guarantees at least one.
        best.expect("a non-empty grid always yields a nearest point")
            .1
    }

    /// The `k` nearest points to `target`, as `(index, squared distance)`
    /// sorted by **index** so the weights that follow are summed in the same
    /// order the exhaustive version used.
    fn k_nearest(&self, points: &[Vec3], target: Vec3, k: usize, out: &mut Vec<(usize, f32)>) {
        out.clear();
        let centre = self.coord_of(target);
        let max_ring = self.max_ring(centre);
        // Kept sorted by distance while it fills, so the worst of the k is
        // always last and the stopping test is a peek.
        let mut best: Vec<(usize, f32)> = Vec::with_capacity(k + 1);
        for ring in 0..=max_ring {
            self.for_each_in_ring(centre, ring, |bucket| {
                for index in bucket {
                    let index = *index as usize;
                    let distance = distance_squared(points[index], target);
                    if best.len() == k
                        && let Some((_, worst)) = best.last()
                        && distance >= *worst
                    {
                        continue;
                    }
                    let at = best.partition_point(|(_, d)| *d <= distance);
                    best.insert(at, (index, distance));
                    best.truncate(k);
                }
            });
            if best.len() == k
                && let Some((_, worst)) = best.last()
            {
                let reach = ring as f32 * self.cell;
                if *worst <= reach * reach {
                    break;
                }
            }
        }
        best.sort_unstable_by_key(|(index, _)| *index);
        out.extend_from_slice(&best);
    }
}

/// Per-target blending weights, `stride` of them each, in one flat buffer.
///
/// The exhaustive version allocated a `Vec<f32>` of `source_count` per target
/// point — ten thousand allocations for a 10k → 10k transfer, every frame the
/// upstream moves. One buffer of `target_count × stride` replaces all of it.
struct SparseWeights {
    /// `(source index, normalised weight)`, `stride` entries per target.
    entries: Vec<(usize, f32)>,
    stride: usize,
}

impl SparseWeights {
    /// Weight every target against its neighbourhood of source points.
    ///
    /// Below [`GRID_MIN_POINTS`] every source point is a neighbour and the
    /// arithmetic is exactly the exhaustive one, index order included.
    ///
    /// A large source is truncated whether or not a grid was built. Without
    /// that, the paths where [`PointGrid::build`] declines a *large* input —
    /// every point coincident, a non-finite extent, or the sparsity fallback —
    /// would hold `target_count × source_count` pairs at once, which for a
    /// degenerate 10k → 10k transfer is 1.6 GB. The exhaustive version this
    /// replaces peaked at one row (`source_count`) because it dropped each
    /// target's weights immediately, so a full row per target would be a
    /// regression rather than the saving the buffer exists for.
    fn of(source: &[Vec3], targets: &[Vec3], grid: Option<&PointGrid>) -> Self {
        let stride = if grid.is_some() || source.len() >= GRID_MIN_POINTS {
            DISTANCE_WEIGHTED_NEIGHBOURS.min(source.len())
        } else {
            source.len()
        };
        let mut entries = Vec::with_capacity(targets.len() * stride);
        let mut neighbours: Vec<(usize, f32)> = Vec::with_capacity(stride);
        for target in targets {
            match grid {
                Some(grid) => grid.k_nearest(source, *target, stride, &mut neighbours),
                None => {
                    neighbours.clear();
                    neighbours.extend(
                        source
                            .iter()
                            .enumerate()
                            .map(|(index, point)| (index, distance_squared(*point, *target))),
                    );
                    if neighbours.len() > stride {
                        // Nearest first, ties by index so a degenerate source
                        // (every point in one place, which is exactly how a
                        // large input reaches this branch) picks the same
                        // points every frame. Back to index order afterwards,
                        // because that is the order `normalize_into` sums in.
                        neighbours.sort_unstable_by(|(left_index, left), (right_index, right)| {
                            left.total_cmp(right).then(left_index.cmp(right_index))
                        });
                        neighbours.truncate(stride);
                        neighbours.sort_unstable_by_key(|(index, _)| *index);
                    }
                }
            }
            normalize_into(&mut neighbours);
            entries.extend_from_slice(&neighbours);
        }
        Self { entries, stride }
    }

    /// The weights blending into target `index`.
    fn weights_of(&self, index: usize) -> &[(usize, f32)] {
        &self.entries[index * self.stride..(index + 1) * self.stride]
    }

    fn target_count(&self) -> usize {
        self.entries.len().checked_div(self.stride).unwrap_or(0)
    }
}

/// Turn `(index, squared distance)` pairs into normalised inverse-distance
/// weights, in place.
///
/// Mirrors the exhaustive `normalized_weights`: a point sitting on the target
/// takes the whole weight (the first such point in index order), otherwise
/// the weights are `1/d` scaled to sum to one — summed in the order given,
/// which the callers keep as index order.
fn normalize_into(neighbours: &mut [(usize, f32)]) {
    if let Some(hit) = neighbours
        .iter()
        .position(|(_, distance)| *distance <= f32::EPSILON)
    {
        for (position, (_, weight)) in neighbours.iter_mut().enumerate() {
            *weight = if position == hit { 1.0 } else { 0.0 };
        }
        return;
    }
    for (_, weight) in neighbours.iter_mut() {
        *weight = 1.0 / weight.sqrt();
    }
    let total: f32 = neighbours.iter().map(|(_, weight)| *weight).sum();
    for (_, weight) in neighbours.iter_mut() {
        *weight /= total;
    }
}

fn nearest_index(points: &[Vec3], target: Vec3) -> usize {
    points
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            distance_squared(**left, target).total_cmp(&distance_squared(**right, target))
        })
        .map_or(0, |(index, _)| index)
}

/// Squared distance in three components. `z = 0` on both sides contributes an
/// exact `+ 0.0`, so 2D geometry keeps its previous bit pattern.
fn distance_squared(left: Vec3, right: Vec3) -> f32 {
    let x = left.0 - right.0;
    let y = left.1 - right.1;
    let z = left.2 - right.2;
    x * x + y * y + z * z
}

fn planar_distance_squared(left: Vec2, right: Vec2) -> f32 {
    let x = left.0 - right.0;
    let y = left.1 - right.1;
    x * x + y * y
}

/// One polyline segment: `(start, end, cumulative length at `end`, length)`.
type Segment = (Vec2, Vec2, f32, f32);

fn push_segment(segments: &mut Vec<Segment>, start: Vec2, end: Vec2) {
    let length = planar_distance_squared(start, end).sqrt();
    if length > f32::EPSILON {
        let previous = segments.last().map_or(0.0, |segment| segment.2);
        segments.push((start, end, previous + length, length));
    }
}

fn normalize(value: Vec2) -> Vec2 {
    let length = (value.0 * value.0 + value.1 * value.1).sqrt();
    Vec2(value.0 / length, value.1 / length)
}

// ---------------------------------------------------------------------------
// Instance expansion (typography-plan unit 5)
// ---------------------------------------------------------------------------

/// The instance columns [`expand_instances`] consumes rather than passes down.
///
/// `P` / `rot` / `scale` / `shear` become the placement baked into the points, and
/// `source_index` names a source list the expanded geometry no longer has;
/// keeping any of them on the Point domain would describe a placement that
/// has already happened. Everything else — `index`, `Cd`, and the
/// per-character columns `text.layout` writes — descends.
///
/// The 3D placement columns are listed too: they are not read yet (the
/// expansion is planar, like the instance path in `rasterize`), and a Point
/// domain that carried an unapplied `orient` would be a trap once they are.
fn is_placement_attribute(name: &str) -> bool {
    matches!(
        name,
        names::P
            | names::ROT
            | names::SCALE
            | names::SHEAR
            | names::SCALE3
            | names::ORIENT
            | names::SOURCE_INDEX
    )
}

/// Flattens an instance geometry into one geometry of points and primitives.
///
/// Each instance's placement ([`InstanceTransform`]: `P` / `rot` / `scale` / `shear`)
/// is baked into the copy of its source that instance contributes, and the
/// instance's remaining attributes descend onto the Point and Primitive
/// domains of that copy — so a per-character attribute `text.layout` wrote on
/// the Instance domain (`char_index`, `char_progress`, …) becomes something a
/// Point-domain field can read, which is what lets a field distort the glyph
/// outlines themselves (REQ-MOGRAPH-004, typography-plan unit 5).
///
/// `in_tan` / `out_tan` are carried through with the linear part of the
/// placement only, because a tangent is a difference and not a position. That
/// is what keeps a glyph's curves curved after the expansion instead of
/// leaving control points behind at the instance origin.
///
/// **Contour order is load-bearing.** The geometry's own primitives come
/// first, unplaced, then one contiguous block per instance, each in its
/// source's own order. `rasterize` fills a *run of consecutive* same-style
/// closed paths as one non-zero region, which is what opens the counter of an
/// `o`; interleaving two characters' contours would put a counter in a
/// different run from its outer contour and fill the hole in.
///
/// Where a source and its instance both carry a column, **the source's own
/// value wins** and the instance's is dropped, which is how `rasterize`
/// narrows a style per element. Two consequences worth knowing:
///
/// * An instance `Cd` / `alpha` *tints* what it draws in the rasterizer but
///   only *fills in* a missing color here, so expanding a tinted instance of
///   an already-coloured source loses the tint. Glyph outlines carry no
///   colour, so text is unaffected.
/// * Detail attributes are the host's wholesale (`dash`, `cap`, `join`,
///   `anchor`); a source's own detail is dropped, as it is in
///   `geometry.merge`.
///
/// A geometry with no instances, or none that can be placed, is returned as
/// it is — the operation is idempotent, so a `text.to_path` on an already
/// flat geometry is a pass-through rather than an error.
pub fn expand_instances(geometry: &Geometry) -> Result<Geometry, GeometryOpError> {
    expand_at(geometry, 0)
}

fn expand_at(geometry: &Geometry, depth: u32) -> Result<Geometry, GeometryOpError> {
    let instances = geometry.instances();
    if instances.element_count() == 0 || geometry.sources().is_empty() {
        return Ok(geometry.clone());
    }
    // An instance domain without positions places nothing. The rasterizer
    // draws no instance in that case, and the flattening has to agree about
    // what exists rather than inventing an origin for each.
    let Some(offsets) = geometry.positions(Domain::Instance) else {
        return Ok(without_instances(geometry));
    };
    let offsets = offsets?.require_planar("instance expansion")?.to_vec();
    let columns = InstanceColumns::of(geometry.instances())?;
    let source_indices = geometry
        .instances()
        .get(names::SOURCE_INDEX)
        .map(|column| column.as_i32(names::SOURCE_INDEX).map(<[i32]>::to_vec))
        .transpose()?;

    // The blocks, in the order they land in the output: the geometry's own
    // elements first, then one per instance. Nested instances are expanded
    // before their own placement is composed onto them, so a block is always
    // flat by the time it is appended.
    let mut blocks: Vec<(Cow<'_, Geometry>, InstanceTransform, Option<usize>)> =
        vec![(Cow::Borrowed(geometry), InstanceTransform::IDENTITY, None)];
    if depth >= MAX_INSTANCE_DEPTH {
        tracing::warn!(
            "instance expansion: nesting deeper than {MAX_INSTANCE_DEPTH}, dropping {} instances",
            offsets.len()
        );
    } else {
        for (index, offset) in offsets.iter().enumerate() {
            // An image source has no contour to convert, so it cannot become
            // path geometry. Dropping it beats erroring: a `scatter` that
            // stamps both pictures and shapes still converts its shapes.
            let Some(source) =
                select_source(geometry.sources(), source_indices.as_deref(), index).geometry()
            else {
                tracing::warn!(
                    "instance expansion: instance {index} stamps an image, not a geometry"
                );
                continue;
            };
            let placement = columns.placement(index, *offset);
            blocks.push((
                Cow::Owned(expand_at(source, depth + 1)?),
                placement,
                Some(index),
            ));
        }
    }

    let mut points = ColumnAccumulator::new(Domain::Point);
    let mut primitive_attrs = ColumnAccumulator::new(Domain::Primitive);
    let mut out = Geometry::new();
    // Where each block's points landed, so the placement can be baked into
    // them once every column exists.
    let mut point_ranges = Vec::with_capacity(blocks.len());
    for (block, placement, instance) in &blocks {
        let inherited = instance.map(|index| (instances, index));
        let point_count = block.point_count();
        let start = points.len;
        points.push(block, inherited, point_count)?;
        primitive_attrs.push(block, inherited, block.primitive_count())?;
        point_ranges.push((start..start + point_count, *placement));

        let index_offset = out.extend_indices(block.indices());
        for primitive in block.primitives() {
            out.push_primitive(primitive.shifted(start, index_offset));
        }
    }

    *out.points_mut() = points.into_set()?;
    *out.primitive_attrs_mut() = primitive_attrs.into_set()?;
    // Detail is not a concatenable domain, so the host's wins wholesale —
    // the same rule `geometry.merge` applies.
    *out.detail_mut() = geometry.detail().clone();
    bake_placements(out.points_mut(), &point_ranges)?;
    // `index` is creation order *within a domain*, so an expansion has to
    // renumber it the way `sort` does after a permutation: the glyph
    // sources each brought their own 0..n, and the instances brought a
    // character number. Neither is the new domain's creation order. What a
    // point still knows about its character is `char_index` and
    // `char_progress`, which is what the plan put them there for.
    renumber_index(&mut out, Domain::Point)?;
    renumber_index(&mut out, Domain::Primitive)?;
    Ok(out)
}

/// Rewrites `index` on `domain` as that domain's own creation order, when it
/// carries an `I32` one. The rule [`sort`] applies after a permutation.
fn renumber_index(geometry: &mut Geometry, domain: Domain) -> Result<(), GeometryError> {
    let renumber = geometry
        .attribute_set(domain)
        .get(names::INDEX)
        .is_some_and(|column| matches!(column.as_ref(), AttributeArray::I32(_)));
    if !renumber {
        return Ok(());
    }
    let count = domain_count(geometry, domain) as i32;
    geometry
        .attribute_set_mut(domain)
        .insert(names::INDEX, AttributeArray::I32((0..count).collect()))?;
    Ok(())
}

/// The geometry's own points, primitives and detail, with the instance domain
/// and its sources dropped.
///
/// The answer for an instance domain that cannot be placed: the output of an
/// expansion is flat by definition, so the instances cannot be carried
/// through even when nothing could be made of them.
fn without_instances(geometry: &Geometry) -> Geometry {
    let mut out = geometry.clone();
    *out.instances_mut() = AttributeSet::new();
    out.set_sources(Vec::new());
    out
}

/// The source instance `index` stamps: `source_index` clamped into the list.
///
/// The rasterizer's rule ([`select_instance_source`] there), because the two
/// have to reach the same source for the same instance: an out-of-range index
/// selects the last source rather than skipping the instance.
fn select_source<'a>(
    sources: &'a [InstanceSource],
    source_indices: Option<&[i32]>,
    index: usize,
) -> &'a InstanceSource {
    &sources[source_slot(sources.len(), source_indices, index)]
}

/// Which slot of the source list instance `index` stamps.
///
/// Split out of [`select_source`] because [`drawn_bounds`] measures each
/// source once and then needs the *slot* rather than the source, and the
/// clamping rule is one rule.
fn source_slot(source_count: usize, source_indices: Option<&[i32]>, index: usize) -> usize {
    let selected = source_indices.map_or(0, |indices| indices[index].max(0) as usize);
    selected.min(source_count - 1)
}

/// Rewrites `P`, `in_tan` and `out_tan` of each block with that block's
/// placement.
///
/// After the columns are concatenated rather than during, so that a block
/// whose source did not carry tangents still has the zero rows the
/// accumulator filled in — a placement applied to a zero tangent leaves it
/// zero, which is what a corner point means.
fn bake_placements(
    points: &mut AttributeSet,
    blocks: &[(Range<usize>, InstanceTransform)],
) -> Result<(), GeometryError> {
    if blocks
        .iter()
        .all(|(_, placement)| *placement == InstanceTransform::IDENTITY)
    {
        return Ok(());
    }
    if points.get(names::P).is_some() {
        let column = points.make_mut(names::P)?;
        let values = column.as_vec2_mut(names::P)?;
        for (range, placement) in blocks {
            for value in &mut values[range.clone()] {
                *value = placement.apply(*value);
            }
        }
    }
    for name in [names::IN_TAN, names::OUT_TAN] {
        if points.get(name).is_none() {
            continue;
        }
        let column = points.make_mut(name)?;
        let values = column.as_vec2_mut(name)?;
        for (range, placement) in blocks {
            for value in &mut values[range.clone()] {
                *value = placement.apply_vector(*value);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Instance pieces (instance-pieces-plan unit 1)
// ---------------------------------------------------------------------------

/// One piece an instance geometry breaks into: what a single instance
/// stamped, and where that instance came from.
///
/// The sibling of [`expand_instances`]: both walk the instance domain with
/// the same rules (the same source selection, the same placement, the same
/// non-placement columns descending), but an expansion *flattens* the whole
/// geometry into one and a split *divides* it into many. A `scatter.*` asked
/// to hand out pieces wants the second: the point of the mode is that the
/// placement is the scatter's to decide.
#[derive(Clone, Debug)]
pub struct InstancePiece {
    /// What this piece stamps — a geometry, or an image the way the
    /// originating instance stamped one.
    pub source: InstanceSource,
    /// The originating instance's **non-placement** attributes, as a set of
    /// one-element columns: `char_index`, `char_progress`, `index`, and any
    /// column a user's `attribute.*` put on the instance domain.
    ///
    /// This is a piece's provenance, and the reason a scattered glyph can
    /// still be staggered (REQ-MOGRAPH-004): [`attach_piece_attributes`]
    /// hands these to the output instances that stamp the piece. Empty when
    /// the geometry had no instance domain to come from.
    pub attributes: AttributeSet,
}

/// Breaks an instance geometry into one piece per **instance**.
///
/// Per instance, not per entry of [`Geometry::sources`]: the source list is
/// deduplicated (`text.layout` shares one outline between both `a`s of
/// "aa"), so splitting that would collapse repeated characters into one
/// piece and lose the order besides. The instance domain is in character
/// order, which is the order a caller wants to deal them out in.
///
/// Each piece carries its instance's `rot`, `scale` and `shear` **baked in**, and its
/// `P` dropped: a turned character stays turned wherever it is dealt, while
/// where it sat in the original layout is exactly what the caller is
/// replacing. The placement is [`InstanceTransform`] with the offset removed
/// — the one formula, not a second one. A source that has instances of its
/// own keeps them, with the outer placement composed onto each
/// ([`InstanceTransform::compose`], outer ∘ inner), so the inner placement
/// applies first and the nesting depth is unchanged — a piece is never
/// deeper than the geometry it came from, so [`MAX_INSTANCE_DEPTH`] means
/// the same thing on both sides of a split.
///
/// **An image instance becomes a piece**, where [`expand_instances`] drops
/// it. The two differ because they are doing different things: an expansion
/// has to turn the instance into contours and a picture has none, while a
/// split only chooses what gets dealt where, and the rasterizer stamps an
/// image instance perfectly well. Nothing can be baked into a picture,
/// though, so an image piece does not carry its instance's turn or scale —
/// it is stamped at whatever placement the caller gives it.
///
/// A geometry with no instances (or no sources) is one piece: itself. The
/// pass-through keeps the caller from having to ask first, the way
/// `expand_instances` is idempotent on an already flat geometry. An instance
/// domain that cannot be placed answers the way an expansion does — one
/// piece, with the unplaceable instances dropped — rather than inventing an
/// origin for each.
pub fn instance_pieces(geometry: &Geometry) -> Result<Vec<InstancePiece>, GeometryOpError> {
    let instances = geometry.instances();
    if instances.element_count() == 0 || geometry.sources().is_empty() {
        return Ok(vec![whole_piece(geometry.clone())]);
    }
    let Some(offsets) = geometry.positions(Domain::Instance) else {
        return Ok(vec![whole_piece(without_instances(geometry))]);
    };
    let count = offsets?.require_planar("instance pieces")?.len();
    let columns = InstanceColumns::of(instances)?;
    let source_indices = instances
        .get(names::SOURCE_INDEX)
        .map(|column| column.as_i32(names::SOURCE_INDEX).map(<[i32]>::to_vec))
        .transpose()?;

    let mut pieces = Vec::with_capacity(count);
    for index in 0..count {
        // The offset is deliberately absent: `P` is the layout this split
        // exists to replace.
        let placement = columns.placement(index, Vec2(0.0, 0.0));
        let source = match select_source(geometry.sources(), source_indices.as_deref(), index) {
            InstanceSource::Geometry(source) => {
                InstanceSource::Geometry(Arc::new(placed(source, placement)?))
            }
            image => image.clone(),
        };
        pieces.push(InstancePiece {
            source,
            attributes: instance_row(instances, index)?,
        });
    }
    Ok(pieces)
}

/// Broadcasts each piece's provenance row onto the output instances that
/// stamp it.
///
/// The other half of a split (`instance-pieces-plan` unit 3), and the half
/// the requirement actually turns on: dealing glyphs out is only useful if a
/// field can still tell them apart afterwards, and what tells them apart is
/// `char_index` / `char_progress` riding along from
/// [`InstancePiece::attributes`] (REQ-MOGRAPH-004).
///
/// Which output instance gets which row is `source_index`, read with the
/// same clamping rule the rasterizer selects a source by — an instance that
/// stamps piece *i* gets piece *i*'s row, and one row is broadcast to every
/// instance stamping it. A `scatter.grid(500)` dealt five characters
/// therefore sees each `char_index` a hundred times, which is what a stagger
/// reads.
///
/// **A column the output already carries is left alone.** That is the whole
/// rule, and it is what protects the placement: `index`, `P`, `rot`,
/// `scale` and `source_index` are the scatter's own answers, and a source's
/// idea of where it sat is exactly what a split threw away. It also means a
/// user column the scatter happens to write wins over the source's, which
/// is the precedence [`expand_instances`] already applies.
///
/// A piece that does not carry a column contributes that column's absent
/// value for its instances ([`absent`]) — what a reader would see without
/// the column — and the typed zero for a name that is not reserved. That is
/// the fill rule `geometry.merge` uses.
pub fn attach_piece_attributes(
    geometry: &mut Geometry,
    pieces: &[InstancePiece],
) -> Result<(), GeometryError> {
    let count = geometry.instance_count();
    if pieces.is_empty() || count == 0 {
        return Ok(());
    }
    let source_indices = geometry
        .instances()
        .get(names::SOURCE_INDEX)
        .map(|column| column.as_i32(names::SOURCE_INDEX).map(<[i32]>::to_vec))
        .transpose()?;

    // First appearance across the pieces, so the column order does not
    // depend on a hash map's iteration order.
    let mut pending: Vec<AttrName> = Vec::new();
    for piece in pieces {
        for (name, _) in piece.attributes.iter() {
            if geometry.instances().get(name.as_str()).is_some() {
                continue;
            }
            if !pending.iter().any(|seen| seen == name) {
                pending.push(name.clone());
            }
        }
    }

    // `stroke_color` last: a row without one takes the row's `Cd`, which has
    // to be in the output by then.
    pending.sort_by_key(|name| name.as_str() == names::STROKE_COLOR);
    for name in pending {
        let sample = pieces
            .iter()
            .find_map(|piece| piece.attributes.get(name.as_str()))
            .expect("the name came from one of the pieces");
        let attr_type = sample.attr_type();
        let mut accumulated = empty_like(sample);
        for index in 0..count {
            let slot = source_slot(pieces.len(), source_indices.as_deref(), index);
            let row = match pieces[slot].attributes.get(name.as_str()) {
                Some(column) => column.as_ref().clone(),
                None => absent_instance_row(geometry.instances(), name.as_str(), attr_type, index),
            };
            append_rows(name.as_str(), &mut accumulated, &row)?;
        }
        geometry
            .instances_mut()
            .insert(name.as_str(), accumulated)?;
    }
    Ok(())
}

/// One Instance row of `name` for a piece that does not carry it: the
/// attribute's absent value, except that a `stroke_color` follows the same
/// output row's `Cd` (white when the output has none), the way an absent one
/// reads in `rasterize`.
fn absent_instance_row(
    instances: &AttributeSet,
    name: &str,
    attr_type: AttributeType,
    index: usize,
) -> AttributeArray {
    if name == names::STROKE_COLOR && attr_type == AttributeType::Color {
        let fill = instances
            .get(names::CD)
            .and_then(|column| column.as_color(names::CD).ok())
            .and_then(|colors| colors.get(index).copied())
            .unwrap_or(absent::DEFAULT_COLOR);
        return AttributeArray::Color(vec![fill]);
    }
    broadcast_value(&absent_value(Domain::Instance, name, attr_type), 1)
}

/// The whole geometry as its own single piece.
fn whole_piece(geometry: Geometry) -> InstancePiece {
    InstancePiece {
        source: InstanceSource::Geometry(Arc::new(geometry)),
        attributes: AttributeSet::new(),
    }
}

/// `geometry` with `placement` baked in: its points moved, its tangents
/// turned, and its own instances' placements composed under it.
///
/// The instance half is what keeps a nesting honest. Composing rather than
/// recursing is what makes "inner first, then outer" true without adding a
/// level: the inner instance's own placement is still the one nearest its
/// source.
fn placed(geometry: &Geometry, placement: InstanceTransform) -> Result<Geometry, GeometryError> {
    if placement == InstanceTransform::IDENTITY {
        return Ok(geometry.clone());
    }
    let mut out = geometry.clone();
    let point_count = out.point_count();
    bake_placements(out.points_mut(), &[(0..point_count, placement)])?;
    // `anchor` is a position in the same space as the points, so it moves
    // with them. Leaving it behind would matter the moment somebody reads
    // it: `scatter.*`'s `center_input` recentres a piece on its anchor, and
    // a stale one would pull every turned piece off its point.
    if out.detail().get(names::ANCHOR).is_some() {
        let anchor = out.detail_mut().make_mut(names::ANCHOR)?;
        for value in anchor.as_vec2_mut(names::ANCHOR)? {
            *value = placement.apply(*value);
        }
    }

    let instances = out.instances();
    if instances.element_count() == 0 || instances.get(names::P).is_none() {
        return Ok(out);
    }
    let inner: Vec<InstanceTransform> = {
        let offsets = instances
            .get(names::P)
            .expect("checked above")
            .as_vec2(names::P)?
            .to_vec();
        let columns = InstanceColumns::of(instances)?;
        offsets
            .iter()
            .enumerate()
            .map(|(index, offset)| {
                InstanceTransform::compose(placement, columns.placement(index, *offset))
            })
            .collect()
    };
    // `shear` is written only when something is sheared or the column is
    // already there, so a geometry that never shears does not grow a column
    // of zeros in the spreadsheet.
    let write_shear =
        out.instances().get(names::SHEAR).is_some() || inner.iter().any(|t| t.shear != 0.0);
    let instances = out.instances_mut();
    instances.insert(
        names::P,
        AttributeArray::Vec2(inner.iter().map(|t| t.offset).collect()),
    )?;
    // A turn or a scale the nesting did not carry has to be written, not
    // merged into a column that is not there.
    instances.insert(
        names::ROT,
        AttributeArray::F32(inner.iter().map(|t| t.rot).collect()),
    )?;
    instances.insert(
        names::SCALE,
        AttributeArray::Vec2(inner.iter().map(|t| t.scale).collect()),
    )?;
    if write_shear {
        instances.insert(
            names::SHEAR,
            AttributeArray::F32(inner.iter().map(|t| t.shear).collect()),
        )?;
    }
    Ok(out)
}

/// Row `index` of `instances`, minus the placement columns: the set a piece
/// carries as its provenance.
fn instance_row(instances: &AttributeSet, index: usize) -> Result<AttributeSet, GeometryError> {
    let mut row = AttributeSet::new();
    for (name, column) in instances.iter() {
        if is_placement_attribute(name) {
            continue;
        }
        row.insert(name.as_str(), select_values(column, std::iter::once(index)))?;
    }
    Ok(row)
}

/// Concatenates attribute sets of differing shape, one block at a time.
///
/// A name a block does not carry is filled for the block's rows with what a
/// reader would see without the column ([`absent_column`], read off the
/// block): `alpha` 1, `scale` (1, 1), a point's `pscale` 2, and so on, the
/// typed zero only for a name that is not reserved. The fill rule
/// `geometry.merge` uses. Column order follows first appearance, so the
/// output does not depend on `HashMap` iteration order.
///
/// Rows are appended as each block arrives and not kept. Filling a column
/// that first appears partway through needs the earlier blocks only through
/// what [`absent_column`] reads: the block geometry (borrowed) and its
/// effective `Cd` (a `stroke_color` follows it), so that is all that is
/// remembered per block.
struct ColumnAccumulator<'a> {
    domain: Domain,
    columns: Vec<(AttrName, AttributeArray)>,
    blocks: Vec<AccumulatedBlock<'a>>,
    len: usize,
}

struct AccumulatedBlock<'a> {
    geometry: &'a Geometry,
    /// The block's `Cd` rows as they end up in the output, when it has a
    /// colour column.
    cd: Option<Cow<'a, AttributeArray>>,
    count: usize,
}

impl<'a> ColumnAccumulator<'a> {
    fn new(domain: Domain) -> Self {
        Self {
            domain,
            columns: Vec::new(),
            blocks: Vec::new(),
            len: 0,
        }
    }

    /// Appends `count` rows.
    ///
    /// `block`'s own columns on the accumulator's domain win where names
    /// collide; `inherited` is the instance domain and the row inside it
    /// whose values broadcast over every row this block contributes.
    fn push(
        &mut self,
        block: &'a Geometry,
        inherited: Option<(&'a AttributeSet, usize)>,
        count: usize,
    ) -> Result<(), GeometryError> {
        let own = block.attribute_set(self.domain);
        let mut rows: Vec<(&AttrName, Cow<'_, AttributeArray>)> = own
            .iter()
            .map(|(name, column)| (name, Cow::Borrowed(column.as_ref())))
            .collect();
        if let Some((instances, index)) = inherited {
            rows.extend(
                instances
                    .iter()
                    .filter(|(name, _)| {
                        !is_placement_attribute(name) && own.get(name.as_str()).is_none()
                    })
                    .map(|(name, column)| {
                        (
                            name,
                            Cow::Owned(select_values(column, std::iter::repeat_n(index, count))),
                        )
                    }),
            );
        }
        let cd = rows
            .iter()
            .position(|(name, _)| name.as_str() == names::CD)
            .filter(|at| matches!(rows[*at].1.as_ref(), AttributeArray::Color(_)))
            .map(|at| rows[at].1.clone());
        let info = AccumulatedBlock {
            geometry: block,
            cd,
            count,
        };

        for (name, accumulated) in &mut self.columns {
            match rows.iter().find(|(row, _)| *row == name) {
                Some((_, column)) => append_rows(name, accumulated, column)?,
                None => {
                    let fill = fill_missing(self.domain, &info, name, accumulated.attr_type());
                    append_rows(name, accumulated, &fill)?;
                }
            }
        }
        for (name, column) in &rows {
            if self.columns.iter().any(|(seen, _)| seen == *name) {
                continue;
            }
            let mut accumulated = empty_like(column);
            // The rows accumulated before this name appeared.
            for earlier in &self.blocks {
                let fill = fill_missing(self.domain, earlier, name, accumulated.attr_type());
                append_rows(name, &mut accumulated, &fill)?;
            }
            append_rows(name, &mut accumulated, column)?;
            self.columns.push(((*name).clone(), accumulated));
        }
        self.blocks.push(info);
        self.len += count;
        Ok(())
    }

    fn into_set(self) -> Result<AttributeSet, GeometryError> {
        let mut set = AttributeSet::new();
        for (name, column) in self.columns {
            set.insert(name, column)?;
        }
        Ok(set)
    }
}

/// The rows of `name` for a block that does not carry it.
///
/// A `stroke_color` follows the fill colour the block *ends up with*, so a
/// `Cd` the block only has through an instance's row counts.
fn fill_missing(
    domain: Domain,
    block: &AccumulatedBlock<'_>,
    name: &str,
    attr_type: AttributeType,
) -> AttributeArray {
    if name == names::STROKE_COLOR
        && attr_type == AttributeType::Color
        && let Some(cd) = &block.cd
    {
        return cd.as_ref().clone();
    }
    absent_column(block.geometry, domain, name, attr_type, block.count)
}

/// An empty column of the same type.
fn empty_like(column: &AttributeArray) -> AttributeArray {
    macro_rules! empty {
        ($variant:ident) => {
            AttributeArray::$variant(Vec::new())
        };
    }
    match column {
        AttributeArray::F32(_) => empty!(F32),
        AttributeArray::Vec2(_) => empty!(Vec2),
        AttributeArray::Vec3(_) => empty!(Vec3),
        AttributeArray::Vec4(_) => empty!(Vec4),
        AttributeArray::Color(_) => empty!(Color),
        AttributeArray::I32(_) => empty!(I32),
        AttributeArray::Bool(_) => empty!(Bool),
        AttributeArray::Str(_) => empty!(Str),
    }
}

/// Appends `from`'s rows onto `into`.
///
/// A same-name column of a different type is a type error rather than a
/// silent conversion, exactly as it is in `geometry.merge`.
fn append_rows(
    name: &str,
    into: &mut AttributeArray,
    from: &AttributeArray,
) -> Result<(), GeometryError> {
    macro_rules! append {
        ($values:expr, $variant:ident) => {
            match from {
                AttributeArray::$variant(block) => {
                    $values.extend(block.iter().cloned());
                    Ok(())
                }
                other => Err(GeometryError::TypeMismatch {
                    name: name.into(),
                    expected: AttributeType::$variant,
                    actual: other.attr_type(),
                }),
            }
        };
    }
    match into {
        AttributeArray::F32(values) => append!(values, F32),
        AttributeArray::Vec2(values) => append!(values, Vec2),
        AttributeArray::Vec3(values) => append!(values, Vec3),
        AttributeArray::Vec4(values) => append!(values, Vec4),
        AttributeArray::Color(values) => append!(values, Color),
        AttributeArray::I32(values) => append!(values, I32),
        AttributeArray::Bool(values) => append!(values, Bool),
        AttributeArray::Str(values) => append!(values, Str),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn bounds_center_prefers_points_then_falls_back_to_instances() {
        let mut geometry = Geometry::from_points(vec![Vec2(2.0, 4.0), Vec2(8.0, 10.0)]);
        geometry
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(100.0, 200.0), Vec2(300.0, 400.0)]),
            )
            .unwrap();
        assert_eq!(bounds_center(&geometry), Some(Vec3(5.0, 7.0, 0.0)));

        let mut instance_only = Geometry::new();
        instance_only
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(-4.0, 2.0), Vec2(6.0, 8.0)]),
            )
            .unwrap();
        assert_eq!(bounds_center(&instance_only), Some(Vec3(1.0, 5.0, 0.0)));
        assert_eq!(bounds_center(&Geometry::new()), None);
    }

    // ----- drawn_bounds -------------------------------------------------------

    /// A square source centred on its own origin, so a placement's effect on
    /// the measured rectangle is the placement itself.
    fn unit_square() -> Geometry {
        Geometry::from_points(vec![
            Vec2(-1.0, -1.0),
            Vec2(1.0, -1.0),
            Vec2(1.0, 1.0),
            Vec2(-1.0, 1.0),
        ])
    }

    /// A geometry stamping `source` once, at `offset` and turned by `rot`.
    fn one_instance(source: InstanceSource, offset: Vec2, rot: f32) -> Geometry {
        let mut geometry = Geometry::new();
        geometry
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![offset]))
            .expect("one offset");
        if rot != 0.0 {
            geometry
                .instances_mut()
                .insert(names::ROT, AttributeArray::F32(vec![rot]))
                .expect("one turn");
        }
        geometry.set_sources(vec![source]);
        geometry
    }

    fn image_source(width: u32, height: u32) -> InstanceSource {
        InstanceSource::Image(
            crate::geometry::InstanceImage::new(
                Arc::new(crate::types::FrameBuffer::new_zeroed(width, height)),
                width,
                height,
            )
            .expect("a frame buffer is an image source"),
        )
    }

    fn as_tuple(rect: Rect) -> (f32, f32, f32, f32) {
        (rect.x, rect.y, rect.width, rect.height)
    }

    /// Completion criterion: a geometry that places nothing in the Point
    /// domain is no longer measured as a zero rectangle. `text.layout` and
    /// `geometry.from_image` both look like this.
    #[test]
    fn an_instance_only_geometry_is_not_zero_sized() {
        let geometry = one_instance(
            InstanceSource::Geometry(Arc::new(unit_square())),
            Vec2(10.0, 20.0),
            0.0,
        );
        assert_eq!(geometry.point_count(), 0);
        let bounds = drawn_bounds(&geometry).expect("the stamped square has an extent");
        assert_eq!(as_tuple(bounds), (9.0, 19.0, 2.0, 2.0));
    }

    /// An instance domain with no source list stamps nothing, so all there is
    /// to measure is where the placements are — which is what a `scatter.*`
    /// with its source input unwired looks like, and what the Viewer already
    /// marks. Unioned with the points, so the curve a scatter ran along stays
    /// inside the rectangle.
    #[test]
    fn instances_with_nothing_to_stamp_measure_their_placements() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(2.0, 1.0)]);
        geometry
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(-1.0, 5.0)]))
            .expect("one placement");
        assert!(geometry.sources().is_empty());
        let bounds = drawn_bounds(&geometry).expect("both domains place something");
        assert_eq!(as_tuple(bounds), (-1.0, 0.0, 3.0, 5.0));
    }

    /// Completion criterion: an image instance measures the image's own
    /// rectangle, moved to where the instance puts it.
    #[test]
    fn an_image_instance_measures_the_images_rectangle() {
        let geometry = one_instance(image_source(64, 32), Vec2(100.0, -5.0), 0.0);
        let rect = geometry.sources()[0]
            .image()
            .expect("an image source")
            .rect();
        let bounds = drawn_bounds(&geometry).expect("a stamped image has an extent");
        assert_eq!(
            as_tuple(bounds),
            (rect.x + 100.0, rect.y - 5.0, rect.width, rect.height),
            "the image rectangle was not moved to the instance"
        );
    }

    /// Completion criterion: a turned source is bounded by the *circumscribed*
    /// rectangle of its placed corners, not by its own rectangle relabelled.
    ///
    /// A quarter turn would hide the difference on a square, so the source is
    /// a wide rectangle turned by 45°: its half-diagonal is what the bounds
    /// have to reach.
    #[test]
    fn a_turned_source_is_bounded_by_its_placed_corners() {
        let mut source = unit_square();
        source
            .points_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![
                    Vec2(-4.0, -1.0),
                    Vec2(4.0, -1.0),
                    Vec2(4.0, 1.0),
                    Vec2(-4.0, 1.0),
                ]),
            )
            .expect("four corners");
        let upright = drawn_bounds(&one_instance(
            InstanceSource::Geometry(Arc::new(source.clone())),
            Vec2(0.0, 0.0),
            0.0,
        ))
        .expect("an upright stamp has an extent");
        let turned = drawn_bounds(&one_instance(
            InstanceSource::Geometry(Arc::new(source)),
            Vec2(0.0, 0.0),
            std::f32::consts::FRAC_PI_4,
        ))
        .expect("a turned stamp has an extent");

        // (4, 1) turned by 45° reaches 5/√2 on both axes, and (4, -1) reaches
        // 3/√2, so the half-extent is 5/√2 each way.
        let reach = 5.0 / 2.0_f32.sqrt();
        assert!(
            (turned.width - reach * 2.0).abs() < 1e-3 && (turned.height - reach * 2.0).abs() < 1e-3,
            "the corners were not placed: {turned:?}"
        );
        assert!(
            turned.height > upright.height,
            "turning a rectangle has to widen its bounds: {turned:?} vs {upright:?}"
        );
    }

    /// Completion criterion: what [`expand_instances`] stops flattening at is
    /// what `drawn_bounds` stops measuring at. A level past the guard is not
    /// drawn, so bounding it would claim an extent for nothing.
    #[test]
    fn the_depth_guard_bounds_exactly_what_expansion_flattens() {
        let mut level = unit_square();
        let mut measured = Vec::new();
        for _ in 0..=MAX_INSTANCE_DEPTH + 1 {
            let next = one_instance(
                InstanceSource::Geometry(Arc::new(level)),
                Vec2(0.0, 0.0),
                0.0,
            );
            let expanded = expand_instances(&next).expect("a nesting answers");
            measured.push((expanded.point_count() > 0, drawn_bounds(&next).is_some()));
            level = next;
        }
        for (depth, (flattened, bounded)) in measured.iter().enumerate() {
            assert_eq!(
                flattened, bounded,
                "nesting depth {depth}: expansion kept points = {flattened}, \
                 drawn_bounds measured something = {bounded}"
            );
        }
        assert!(
            measured.iter().any(|(flattened, _)| !flattened),
            "the nesting never reached the guard: {measured:?}"
        );
    }

    /// `unit_square` with `width` asked for on the Primitive domain, the
    /// domain `rasterize::element_style` narrows a path's stroke off.
    fn stroked_square(width: f32, join: Option<i32>) -> Geometry {
        let mut geometry = unit_square();
        geometry.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        geometry
            .primitive_attrs_mut()
            .insert(names::STROKE_WIDTH, AttributeArray::F32(vec![width]))
            .expect("one primitive");
        if let Some(join) = join {
            geometry
                .detail_mut()
                .insert(names::JOIN, AttributeArray::I32(vec![join]))
                .expect("one detail value");
        }
        geometry
    }

    /// Completion criterion: widening the stroke grows the bounds by exactly
    /// the reach the rasterizer uses, on every side.
    #[test]
    fn a_stroke_grows_the_bounds_by_its_reach() {
        let bare = drawn_bounds(&stroked_square(0.0, None)).expect("a square has an extent");
        let stroked = drawn_bounds(&stroked_square(40.0, None)).expect("a square has an extent");
        let reach = stroke_reach(40.0, false);
        assert_eq!(
            as_tuple(stroked),
            (
                bare.x - reach,
                bare.y - reach,
                bare.width + reach * 2.0,
                bare.height + reach * 2.0
            )
        );
    }

    /// Completion criterion: a miter reaches further than a round join, so the
    /// bounds have to be wider for it. The rule lives in [`stroke_reach`];
    /// this is the bbox actually asking.
    #[test]
    fn a_miter_join_bounds_wider_than_a_round_one() {
        let miter = drawn_bounds(&stroked_square(40.0, Some(names::JOIN_MITER)))
            .expect("a square has an extent");
        let round = drawn_bounds(&stroked_square(40.0, Some(names::JOIN_ROUND)))
            .expect("a square has an extent");
        assert!(
            miter.width > round.width && miter.height > round.height,
            "a miter spike has to widen the bounds: {miter:?} vs {round:?}"
        );
    }

    /// A zero stroke is the absence of one, not a one-pixel margin: every
    /// geometry carries a `stroke_width` column once `style.stroke` has run on
    /// a group, and the elements outside the group are seeded with zero.
    #[test]
    fn a_zero_stroke_width_grows_nothing() {
        assert_eq!(
            drawn_bounds(&stroked_square(0.0, None)),
            drawn_bounds(&unit_square()),
            "a zero-width stroke reaches nowhere"
        );
    }

    /// A cubic leaves its anchors, so the anchors alone are not the extent.
    ///
    /// Two anchors on one horizontal line with both handles 60 units up:
    /// `y(t) = 180t(1 - t)`, which peaks at **45** in the middle. Measuring
    /// `P` alone reports zero height for a curve that is drawn 45 units tall
    /// (`rasterize::path_polyline` flattens these tangents, so it is drawn).
    ///
    /// The assertion is the apex, not the hull: the control polygon reaches
    /// 60 and the bound may sit anywhere at or above the ink, but never
    /// below it.
    #[test]
    fn a_curve_that_bulges_past_its_anchors_is_still_inside_the_bounds() {
        let anchors = || Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(100.0, 0.0)]);
        let mut geometry = anchors();
        geometry
            .points_mut()
            .insert(
                names::OUT_TAN,
                AttributeArray::Vec2(vec![Vec2(0.0, 60.0), Vec2(0.0, 0.0)]),
            )
            .expect("one out tangent per point");
        geometry
            .points_mut()
            .insert(
                names::IN_TAN,
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(0.0, 60.0)]),
            )
            .expect("one in tangent per point");

        let bounds = drawn_bounds(&geometry).expect("a curve has an extent");
        const APEX: f32 = 45.0;
        assert!(
            bounds.y <= 0.0 && bounds.y + bounds.height >= APEX,
            "the curve peaks at {APEX} and the bounds stop short: {bounds:?}"
        );
        assert_eq!(
            drawn_bounds(&anchors())
                .expect("two points have an extent")
                .height,
            0.0,
            "the same anchors without tangents are a flat line"
        );
    }

    /// An instance stamping `source`, placed at `offset` with `scale`, and
    /// asking for `stroke_width` on its own domain the way `style.stroke`
    /// does.
    fn scaled_instance(source: Geometry, scale: Vec2, stroke_width: f32) -> Geometry {
        let mut geometry = Geometry::new();
        geometry
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))
            .expect("one offset");
        geometry
            .instances_mut()
            .insert(names::SCALE, AttributeArray::Vec2(vec![scale]))
            .expect("one scale");
        geometry
            .instances_mut()
            .insert(names::STROKE_WIDTH, AttributeArray::F32(vec![stroke_width]))
            .expect("one width");
        geometry.set_sources(vec![InstanceSource::Geometry(Arc::new(source))]);
        geometry
    }

    /// The instance's scale multiplies the stroke, because `rasterize` strokes
    /// with `stroke_width * placement.uniform_scale()`.
    ///
    /// A reach measured in the source's own space and left there is right only
    /// at a scale of 1: at `(3, 3)` the bbox grew by 21 while the drawn half
    /// width was 60, so a scattered thick-stroked shape spilled out of its own
    /// rectangle by three quarters of the line.
    #[test]
    fn an_instances_scale_multiplies_the_stroke_it_inherits() {
        let bounds = drawn_bounds(&scaled_instance(unit_square(), Vec2(3.0, 3.0), 40.0))
            .expect("a stamped square has an extent");
        // The square is 2×2 about the origin, so the scale puts it at ±3.
        let reach = stroke_reach(40.0 * 3.0, false);
        assert_eq!(
            as_tuple(bounds),
            (
                -3.0 - reach,
                -3.0 - reach,
                6.0 + reach * 2.0,
                6.0 + reach * 2.0
            ),
            "the reach was not scaled with the placement"
        );
    }

    /// A non-uniform scale collapses to the mean absolute scale, which is the
    /// one number a stroke can have — [`InstanceTransform::uniform_scale`], the
    /// same collapse the rasterizer makes.
    #[test]
    fn a_non_uniform_scale_reaches_by_the_uniform_scale() {
        let bounds = drawn_bounds(&scaled_instance(unit_square(), Vec2(4.0, 1.0), 40.0))
            .expect("a stamped square has an extent");
        let reach = stroke_reach(40.0 * 2.5, false);
        assert_eq!(
            (bounds.y, bounds.height),
            (-1.0 - reach, 2.0 + reach * 2.0),
            "the thin axis was measured with the unscaled reach"
        );
    }

    /// The root's `join` applies to every source, because `rasterize` reads it
    /// once at the node entry and carries it down `Style::shape`.
    ///
    /// Reading it per level made a miter root bound its sources as if they were
    /// round: 40 wide, the miter spike reaches 81 and a round join 21.
    #[test]
    fn the_roots_join_decides_the_reach_of_every_source() {
        let stamp = |join| {
            let mut geometry = Geometry::new();
            geometry
                .instances_mut()
                .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))
                .expect("one offset");
            geometry
                .detail_mut()
                .insert(names::JOIN, AttributeArray::I32(vec![join]))
                .expect("one detail value");
            // The source asks for the width and says nothing about the join.
            geometry.set_sources(vec![InstanceSource::Geometry(Arc::new(stroked_square(
                40.0, None,
            )))]);
            drawn_bounds(&geometry).expect("a stamped square has an extent")
        };
        let miter = stamp(names::JOIN_MITER);
        let round = stamp(names::JOIN_ROUND);
        assert_eq!(
            miter.width - round.width,
            (stroke_reach(40.0, true) - stroke_reach(40.0, false)) * 2.0,
            "the source was bounded with its own join instead of the root's"
        );
        assert!(miter.width > round.width, "{miter:?} vs {round:?}");
    }

    /// Nesting composes through [`InstanceTransform::compose`], which is the
    /// exact affine product: a 20×2 image turned a quarter-turn inside a
    /// 10×-wide instance is a 20 × 20 square, the same as applying the two
    /// placements in turn.
    #[test]
    fn nesting_is_bounded_the_way_the_placements_compose() {
        let mut inner = one_instance(image_source(20, 2), Vec2(3.0, 5.0), FRAC_PI_2);
        inner
            .instances_mut()
            .insert(names::INDEX, AttributeArray::I32(vec![0]))
            .expect("one instance");
        // Built inline rather than with `scaled_instance`, because **both**
        // levels have to carry an offset: `compose` differs from its own
        // arguments reversed only in the offset (the turns add and the scales
        // multiply either way), so two placements at the origin would pin the
        // composition without pinning which one is the outer.
        let mut outer = Geometry::new();
        outer
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(7.0, 11.0)]))
            .expect("one offset");
        outer
            .instances_mut()
            .insert(names::SCALE, AttributeArray::Vec2(vec![Vec2(10.0, 1.0)]))
            .expect("one scale");
        outer.set_sources(vec![InstanceSource::Geometry(Arc::new(inner))]);

        let bounds = drawn_bounds(&outer).expect("a nested image has an extent");
        // Applying the two placements in turn to a corner `(qx, qy)`: the
        // inner one gives `(3 - qy, 5 + qx)`, the outer one `(7 + 10 (3 - qy),
        // 11 + 5 + qx)` = `(37 - 10 qy, 16 + qx)`. With `qx` in ±10 and `qy`
        // in ±1 that is x in 37 ± 10 and y in 16 ± 10. Reversing the two
        // placements would put it somewhere else entirely.
        for (what, got, want) in [
            ("x", bounds.x, 27.0),
            ("y", bounds.y, 6.0),
            ("width", bounds.width, 20.0),
            ("height", bounds.height, 20.0),
        ] {
            assert!(
                (got - want).abs() < 1e-3,
                "{what}: {got} is not {want} — {bounds:?}"
            );
        }
    }

    /// The cost condition, opt-in because it is a measurement rather than an
    /// assertion about behaviour: `cargo test -p ravel-core --release
    /// drawn_bounds_costs -- --ignored --nocapture`.
    ///
    /// A million-point geometry has to cost the same order as the
    /// `positions_bounds` it replaces, because the Viewer pays it once per
    /// pointer move. The numbers behind the table on [`drawn_bounds`].
    ///
    /// Alternating rounds, best of each: a single A-then-B pair charges the
    /// first call for whatever the machine was doing when the test started,
    /// and the answer is a ratio between two numbers that both move.
    #[test]
    #[ignore = "a timing measurement, not a behavioural assertion"]
    fn drawn_bounds_costs_the_same_order_as_positions_bounds() {
        let geometry = Geometry::from_points(
            (0..1_000_000)
                .map(|i| Vec2(i as f32, -(i as f32)))
                .collect(),
        );
        let (mut bare, mut drawn) = (Duration::MAX, Duration::MAX);
        for _ in 0..8 {
            let start = Instant::now();
            let positions = geometry.positions_bounds();
            bare = bare.min(start.elapsed());
            let start = Instant::now();
            let all = drawn_bounds(&geometry);
            drawn = drawn.min(start.elapsed());
            assert_eq!(positions, all, "a flat geometry draws exactly its points");
        }
        println!("1 000 000 points: positions_bounds {bare:?}, drawn_bounds {drawn:?}");
        assert!(
            drawn < bare * 4,
            "drawn_bounds left the order of positions_bounds: {drawn:?} vs {bare:?}"
        );
    }

    /// The other half of the cost promise, opt-in for the same reason: a
    /// source stamped by many instances is measured **once**, so stamping it
    /// a thousand times costs the same order as measuring it alone.
    ///
    /// This is what the `instance_count() == 0` cache in [`instance_bounds`]
    /// buys. Without it the walk is `O(instances × points)`: a thousand
    /// instances of a hundred-thousand-point source is a hundred million
    /// point reads per pointer move, which is two orders out rather than a
    /// constant factor — hence the loose ceiling here.
    #[test]
    #[ignore = "a timing measurement, not a behavioural assertion"]
    fn a_stamped_source_is_measured_once_however_many_instances_stamp_it() {
        const INSTANCES: usize = 1_000;
        let source = Geometry::from_points(
            (0..100_000)
                .map(|i| Vec2(i as f32, -(i as f32)))
                .collect::<Vec<_>>(),
        );
        let mut stamped = Geometry::new();
        stamped
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2((0..INSTANCES).map(|i| Vec2(i as f32, 0.0)).collect()),
            )
            .expect("one column");
        stamped.set_sources(vec![InstanceSource::Geometry(Arc::new(source.clone()))]);

        let (mut once, mut many) = (Duration::MAX, Duration::MAX);
        for _ in 0..8 {
            let start = Instant::now();
            assert!(drawn_bounds(&source).is_some());
            once = once.min(start.elapsed());
            let start = Instant::now();
            assert!(drawn_bounds(&stamped).is_some());
            many = many.min(start.elapsed());
        }
        println!("{INSTANCES} instances of 100 000 points: once {once:?}, stamped {many:?}");
        assert!(
            many < once * 8,
            "the source was re-measured per instance: {many:?} vs {once:?}"
        );
    }

    #[test]
    fn set_broadcasts_without_mutating_input() {
        let geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0)]);
        let result = attribute_set(
            &geometry,
            Domain::Point,
            "weight",
            AttributeValue::F32(0.75),
        )
        .unwrap();
        assert!(geometry.points().get("weight").is_none());
        assert_eq!(
            result
                .points()
                .get("weight")
                .unwrap()
                .as_f32("weight")
                .unwrap(),
            &[0.75, 0.75]
        );
    }

    /// Deleting one column leaves the input untouched and every surviving
    /// column pointing at the *same* allocation, so a scratch column can be
    /// dropped from a heavy geometry without copying it.
    #[test]
    fn delete_drops_one_column_and_keeps_the_rest_shared() {
        let mut geometry =
            Geometry::from_points(vec![Vec2(13.0, -5.0), Vec2(-2.5, 7.25), Vec2(31.0, 11.75)]);
        geometry
            .points_mut()
            .insert("stagger_t", AttributeArray::F32(vec![3.5, -7.25, 11.75]))
            .unwrap();
        geometry
            .points_mut()
            .insert("keep", AttributeArray::F32(vec![-1.5, 2.75, 6.25]))
            .unwrap();

        let result = attribute_delete(&geometry, Domain::Point, "stagger_t").unwrap();

        assert!(result.points().get("stagger_t").is_none());
        assert!(geometry.points().get("stagger_t").is_some());
        assert_eq!(result.point_count(), 3);
        for name in ["keep", names::P, names::INDEX] {
            let before = geometry.points().get(name).unwrap();
            let after = result.points().get(name).unwrap();
            assert!(Arc::ptr_eq(before, after), "{name} was copied, not shared");
        }
    }

    /// A name the domain does not carry is a no-op, not an error: an upstream
    /// edit that stops writing a column must not turn the graph red.
    #[test]
    fn delete_of_a_missing_attribute_changes_nothing() {
        let mut geometry = Geometry::from_points(vec![Vec2(4.5, -6.5), Vec2(9.25, 2.0)]);
        geometry
            .points_mut()
            .insert("weight", AttributeArray::F32(vec![-3.25, 8.5]))
            .unwrap();

        let result = attribute_delete(&geometry, Domain::Point, "never_written").unwrap();

        assert_eq!(result.summary().points, geometry.summary().points);
        assert!(Arc::ptr_eq(
            geometry.points().get("weight").unwrap(),
            result.points().get("weight").unwrap()
        ));
    }

    /// `P` carries the placement the position-carrying domains are validated
    /// on, so its delete is refused there — and only there.
    #[test]
    fn delete_refuses_the_position_column_of_a_position_domain() {
        let mut geometry = Geometry::from_points(vec![Vec2(21.5, -13.25), Vec2(-8.75, 4.5)]);
        geometry
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(101.5, -202.25)]))
            .unwrap();
        geometry
            .detail_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(17.5, 23.25)]))
            .unwrap();

        for domain in [Domain::Point, Domain::Instance] {
            let error = attribute_delete(&geometry, domain, names::P).unwrap_err();
            assert!(
                matches!(
                    error,
                    GeometryOpError::RequiredAttribute { name, domain: refused }
                        if name == names::P && refused == domain
                ),
                "{domain:?} produced {error}"
            );
        }

        // Detail is not a position domain: nothing validates a `P` there, so
        // the same name is an ordinary column and deletes like one.
        let result = attribute_delete(&geometry, Domain::Detail, names::P).unwrap();
        assert!(result.detail().get(names::P).is_none());
        assert!(result.points().get(names::P).is_some());
    }

    /// A group restricts the write to the elements it flags; the others keep
    /// the exact value the column already held.
    #[test]
    fn group_restricted_set_keeps_the_other_elements() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0); 3]);
        geometry
            .points_mut()
            .insert("weight", AttributeArray::F32(vec![1.0, 2.0, 3.0]))
            .unwrap();
        geometry
            .points_mut()
            .insert("mask", AttributeArray::Bool(vec![false, true, false]))
            .unwrap();

        let result = attribute_set_in_group(
            &geometry,
            Domain::Point,
            "weight",
            AttributeValue::F32(9.0),
            "mask",
            AttributeValue::F32(0.0),
        )
        .unwrap();
        assert_eq!(
            result
                .points()
                .get("weight")
                .unwrap()
                .as_f32("weight")
                .unwrap(),
            &[1.0, 9.0, 3.0]
        );
    }

    /// Without a column to keep, the elements outside the group take the
    /// `unset` value — the one that reads as "nobody wrote this attribute".
    #[test]
    fn group_restricted_set_seeds_a_new_column_with_the_unset_value() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0); 3]);
        geometry
            .points_mut()
            .insert("mask", AttributeArray::Bool(vec![true, false, true]))
            .unwrap();

        let result = attribute_set_in_group(
            &geometry,
            Domain::Point,
            "on",
            AttributeValue::Bool(false),
            "mask",
            AttributeValue::Bool(true),
        )
        .unwrap();
        assert_eq!(
            result.points().get("on").unwrap().as_bool("on").unwrap(),
            &[false, true, false]
        );
    }

    /// An unusable group name must not fail the evaluation: a half-typed name
    /// in the node editor falls back to every element, exactly as `field.apply`
    /// resolves it (the two share the resolver).
    #[test]
    fn unusable_group_names_write_every_element() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0); 2]);
        geometry
            .points_mut()
            .insert("not_bool", AttributeArray::F32(vec![1.0, 1.0]))
            .unwrap();
        for group in ["", "typo", "not_bool"] {
            let result = attribute_set_in_group(
                &geometry,
                Domain::Point,
                "weight",
                AttributeValue::F32(4.0),
                group,
                AttributeValue::F32(0.0),
            )
            .unwrap();
            assert_eq!(
                result
                    .points()
                    .get("weight")
                    .unwrap()
                    .as_f32("weight")
                    .unwrap(),
                &[4.0, 4.0],
                "group {group:?}"
            );
        }
    }

    #[test]
    fn promote_aggregates_average_max_and_first() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0); 3]);
        geometry
            .points_mut()
            .insert("value", AttributeArray::F32(vec![1.0, 5.0, 3.0]))
            .unwrap();
        for (mode, expected) in [
            (AggregateMode::Average, 3.0),
            (AggregateMode::Max, 5.0),
            (AggregateMode::First, 1.0),
        ] {
            let result =
                promote_attribute(&geometry, Domain::Point, Domain::Detail, "value", mode).unwrap();
            assert_eq!(
                result
                    .detail()
                    .get("value")
                    .unwrap()
                    .as_f32("value")
                    .unwrap(),
                &[expected]
            );
        }
    }

    #[test]
    fn promote_between_point_instance_and_detail_broadcasts() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0); 2]);
        geometry
            .points_mut()
            .insert("value", AttributeArray::F32(vec![2.0, 6.0]))
            .unwrap();
        geometry
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 0.0); 3]))
            .unwrap();
        let instances = promote_attribute(
            &geometry,
            Domain::Point,
            Domain::Instance,
            "value",
            AggregateMode::Average,
        )
        .unwrap();
        assert_eq!(
            instances
                .instances()
                .get("value")
                .unwrap()
                .as_f32("value")
                .unwrap(),
            &[4.0, 4.0, 4.0]
        );
        let detail = promote_attribute(
            &geometry,
            Domain::Point,
            Domain::Detail,
            "value",
            AggregateMode::Max,
        )
        .unwrap();
        let points = promote_attribute(
            &detail,
            Domain::Detail,
            Domain::Point,
            "value",
            AggregateMode::First,
        )
        .unwrap();
        assert_eq!(
            points
                .points()
                .get("value")
                .unwrap()
                .as_f32("value")
                .unwrap(),
            &[6.0, 6.0]
        );
    }

    #[test]
    fn transfer_is_spatially_accurate() {
        let mut source = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]);
        source
            .points_mut()
            .insert("value", AttributeArray::F32(vec![0.0, 10.0]))
            .unwrap();
        let target = Geometry::from_points(vec![Vec2(1.0, 0.0), Vec2(5.0, 0.0), Vec2(9.0, 0.0)]);
        let nearest = attribute_transfer(
            &target,
            Domain::Point,
            &source,
            Domain::Point,
            "value",
            TransferMode::Nearest,
        )
        .unwrap();
        assert_eq!(
            nearest
                .points()
                .get("value")
                .unwrap()
                .as_f32("value")
                .unwrap(),
            &[0.0, 0.0, 10.0]
        );
        let weighted = attribute_transfer(
            &target,
            Domain::Point,
            &source,
            Domain::Point,
            "value",
            TransferMode::DistanceWeighted,
        )
        .unwrap();
        let values = weighted
            .points()
            .get("value")
            .unwrap()
            .as_f32("value")
            .unwrap();
        assert!((values[0] - 1.0).abs() < 1e-5);
        assert!((values[1] - 5.0).abs() < 1e-5);
        assert!((values[2] - 9.0).abs() < 1e-5);
    }

    #[test]
    fn path_sampling_uses_arc_length_and_returns_frame() {
        let mut geometry =
            Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(3.0, 0.0), Vec2(3.0, 4.0)]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: false,
        });
        let sample = path_sample(&geometry, 5.0).unwrap();
        assert_eq!(sample.position, Vec2(3.0, 2.0));
        assert_eq!(sample.tangent, Vec2(0.0, 1.0));
        assert_eq!(sample.normal, Vec2(-1.0, 0.0));
    }

    /// A table held across many samples answers exactly what the one-shot
    /// [`path_sample`] answers, ends of the range included: the whole point
    /// of hoisting the walk out of a per-element loop is that the numbers do
    /// not move.
    #[test]
    fn a_held_arc_table_answers_what_path_sample_answers() {
        let mut geometry =
            Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(3.0, 0.0), Vec2(3.0, 4.0)]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: false,
        });
        let table = PathArcTable::build(&geometry, "test").unwrap();
        assert_eq!(table.length(), 7.0);
        // Below zero and past the end are clamped, not extrapolated.
        for distance in [-4.0, 0.0, 0.5, 3.0, 5.0, 6.999, 7.0, 100.0] {
            assert_eq!(
                table.sample(distance),
                path_sample(&geometry, distance).unwrap(),
                "the table disagreed with path_sample at {distance}"
            );
        }
    }

    #[test]
    fn a_degenerate_path_has_no_arc_table() {
        let mut geometry = Geometry::from_points(vec![Vec2(2.0, 2.0), Vec2(2.0, 2.0)]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        assert!(matches!(
            PathArcTable::build(&geometry, "test"),
            Err(GeometryOpError::InvalidPath)
        ));
    }

    fn point_order(geometry: &Geometry) -> Vec<Vec2> {
        geometry
            .points()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap()
            .to_vec()
    }

    fn connected_path(geometry: &Geometry) -> (std::ops::Range<usize>, bool) {
        match geometry.primitives() {
            [Primitive::Path { verts, closed }] => (verts.clone(), *closed),
            other => panic!("expected exactly one path, got {other:?}"),
        }
    }

    #[test]
    fn connect_order_makes_one_path_over_every_point_in_index_order() {
        let cloud = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(10.0, 0.0),
            Vec2(10.0, 10.0),
            Vec2(0.0, 10.0),
        ]);
        let wired = connect(
            &cloud,
            ConnectMode::Order,
            ConnectInterpolation::Linear,
            false,
        )
        .unwrap();
        assert_eq!(connected_path(&wired), (0..4, false));
        assert_eq!(point_order(&wired), point_order(&cloud));
        assert_eq!(
            wired.points().get(names::INDEX).unwrap().as_i32("index"),
            Ok(&[0, 1, 2, 3][..])
        );
        assert!(wired.validate().is_ok());
    }

    #[test]
    fn connect_closes_the_path_when_asked() {
        let cloud = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0), Vec2(5.0, 8.0)]);
        let wired = connect(
            &cloud,
            ConnectMode::Order,
            ConnectInterpolation::Linear,
            true,
        )
        .unwrap();
        assert_eq!(connected_path(&wired), (0..3, true));
    }

    /// A point cloud whose storage order zig-zags: the chain has to reorder
    /// it, and has to reorder it the same way every time it is asked.
    #[test]
    fn connect_nearest_chains_by_proximity_and_is_deterministic() {
        let cloud = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(30.0, 0.0),
            Vec2(10.0, 0.0),
            Vec2(20.0, 0.0),
        ]);
        let run = || {
            connect(
                &cloud,
                ConnectMode::Nearest,
                ConnectInterpolation::Linear,
                false,
            )
            .unwrap()
        };
        let wired = run();
        assert_eq!(connected_path(&wired), (0..4, false));
        assert_eq!(
            point_order(&wired),
            [
                Vec2(0.0, 0.0),
                Vec2(10.0, 0.0),
                Vec2(20.0, 0.0),
                Vec2(30.0, 0.0)
            ]
        );
        // Every attribute travels with its point, `index` included: the
        // connected points keep the numbers they were created with.
        assert_eq!(
            wired.points().get(names::INDEX).unwrap().as_i32("index"),
            Ok(&[0, 2, 3, 1][..])
        );
        assert_eq!(point_order(&run()), point_order(&wired));
    }

    /// Big enough for `PointGrid::build` to answer instead of the scan, so
    /// the grid path is the one under test.
    #[test]
    fn connect_nearest_is_deterministic_through_the_spatial_grid() {
        let points: Vec<Vec2> = (0..GRID_MIN_POINTS * 2)
            .map(|index| {
                let step = index as f32;
                Vec2((step * 7.0) % 23.0, (step * 13.0) % 29.0)
            })
            .collect();
        let cloud = Geometry::from_points(points);
        let once = connect(
            &cloud,
            ConnectMode::Nearest,
            ConnectInterpolation::Linear,
            false,
        )
        .unwrap();
        let twice = connect(
            &cloud,
            ConnectMode::Nearest,
            ConnectInterpolation::Linear,
            false,
        )
        .unwrap();
        assert_eq!(point_order(&once), point_order(&twice));
        assert_eq!(once.point_count(), cloud.point_count());
    }

    #[test]
    fn connect_group_links_only_its_members_and_keeps_the_rest() {
        let mut cloud = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(10.0, 0.0),
            Vec2(20.0, 0.0),
            Vec2(30.0, 0.0),
        ]);
        cloud
            .points_mut()
            .insert("wire", AttributeArray::Bool(vec![true, false, true, true]))
            .unwrap();
        let wired = connect(
            &cloud,
            ConnectMode::Group("wire"),
            ConnectInterpolation::Linear,
            false,
        )
        .unwrap();
        assert_eq!(connected_path(&wired), (0..3, false));
        assert_eq!(wired.point_count(), 4, "no point is dropped");
        assert_eq!(
            point_order(&wired),
            [
                Vec2(0.0, 0.0),
                Vec2(20.0, 0.0),
                Vec2(30.0, 0.0),
                Vec2(10.0, 0.0)
            ]
        );
    }

    /// Connecting adds connectivity; it must not rewrite what the points say
    /// about themselves.
    #[test]
    fn connect_carries_every_point_attribute_through_the_permutation() {
        let mut cloud =
            Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(30.0, 0.0), Vec2(10.0, 0.0)]);
        cloud
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![1.0, 2.0, 3.0]))
            .unwrap();
        cloud
            .points_mut()
            .insert(
                names::CD,
                AttributeArray::Color(vec![
                    Color::new(1.0, 0.0, 0.0, 1.0),
                    Color::new(0.0, 1.0, 0.0, 1.0),
                    Color::new(0.0, 0.0, 1.0, 1.0),
                ]),
            )
            .unwrap();
        cloud
            .detail_mut()
            .insert(names::ANCHOR, AttributeArray::Vec2(vec![Vec2(5.0, 5.0)]))
            .unwrap();
        let wired = connect(
            &cloud,
            ConnectMode::Nearest,
            ConnectInterpolation::Linear,
            false,
        )
        .unwrap();
        // Chain order is 0, 2, 1; every column follows it.
        assert_eq!(
            wired.points().get(names::PSCALE).unwrap().as_f32("pscale"),
            Ok(&[1.0, 3.0, 2.0][..])
        );
        assert_eq!(
            wired.points().get(names::CD).unwrap().as_color("Cd"),
            Ok(&[
                Color::new(1.0, 0.0, 0.0, 1.0),
                Color::new(0.0, 0.0, 1.0, 1.0),
                Color::new(0.0, 1.0, 0.0, 1.0),
            ][..])
        );
        assert_eq!(
            wired.detail().get(names::ANCHOR).unwrap().as_vec2("anchor"),
            Ok(&[Vec2(5.0, 5.0)][..])
        );
    }

    #[test]
    fn connect_writes_catmull_rom_tangents_only_in_bezier_mode() {
        let cloud = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0), Vec2(10.0, 10.0)]);
        let straight = connect(
            &cloud,
            ConnectMode::Order,
            ConnectInterpolation::Linear,
            false,
        )
        .unwrap();
        assert!(straight.points().get(names::IN_TAN).is_none());
        assert!(straight.points().get(names::OUT_TAN).is_none());

        let curved = connect(
            &cloud,
            ConnectMode::Order,
            ConnectInterpolation::Bezier,
            false,
        )
        .unwrap();
        let column = |name: &str| {
            curved
                .points()
                .get(name)
                .unwrap()
                .as_vec2(name)
                .unwrap()
                .to_vec()
        };
        // Interior tangent: a sixth of the chord between the neighbours.
        // Ends: a third of their one segment, and zero on the unused side.
        assert_eq!(
            column(names::OUT_TAN),
            [
                Vec2(10.0 / 3.0, 0.0),
                Vec2(10.0 / 6.0, 10.0 / 6.0),
                Vec2(0.0, 0.0)
            ]
        );
        assert_eq!(
            column(names::IN_TAN),
            [
                Vec2(0.0, 0.0),
                Vec2(-10.0 / 6.0, -10.0 / 6.0),
                Vec2(0.0, -10.0 / 3.0)
            ]
        );
    }

    /// A count an animation can pass through: nothing to connect is a no-op,
    /// not an error.
    #[test]
    fn connect_leaves_too_few_points_alone() {
        for cloud in [Geometry::new(), Geometry::from_points(vec![Vec2(1.0, 2.0)])] {
            let wired = connect(
                &cloud,
                ConnectMode::Order,
                ConnectInterpolation::Bezier,
                true,
            )
            .unwrap();
            assert_eq!(wired.point_count(), cloud.point_count());
            assert_eq!(wired.primitive_count(), 0);
        }

        let mut ungrouped = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]);
        ungrouped
            .points_mut()
            .insert("wire", AttributeArray::Bool(vec![false, false]))
            .unwrap();
        let wired = connect(
            &ungrouped,
            ConnectMode::Group("wire"),
            ConnectInterpolation::Linear,
            false,
        )
        .unwrap();
        assert_eq!(wired.primitive_count(), 0);
    }

    /// The node decides the connectivity, so the primitives it was handed are
    /// replaced rather than added to (Houdini's Add SOP keeps the points and
    /// drops the geometry).
    #[test]
    fn connect_replaces_the_primitives_it_was_given() {
        let mut shape = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(10.0, 0.0),
            Vec2(10.0, 10.0),
            Vec2(0.0, 10.0),
        ]);
        shape.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        shape.push_primitive(Primitive::Path {
            verts: 2..4,
            closed: false,
        });
        let wired = connect(
            &shape,
            ConnectMode::Order,
            ConnectInterpolation::Linear,
            false,
        )
        .unwrap();
        assert_eq!(wired.primitive_count(), 1);
        assert_eq!(connected_path(&wired), (0..4, false));
    }

    #[test]
    fn connect_rejects_meshes_and_three_dimensional_positions() {
        let mut mesh = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0), Vec2(0.0, 1.0)]);
        mesh.push_mesh(0..3, &[0, 1, 2]);
        assert!(
            connect(
                &mesh,
                ConnectMode::Order,
                ConnectInterpolation::Linear,
                false
            )
            .is_err()
        );

        let spatial = Geometry::from_points3(vec![Vec3(0.0, 0.0, 1.0), Vec3(1.0, 0.0, 2.0)]);
        assert!(
            connect(
                &spatial,
                ConnectMode::Order,
                ConnectInterpolation::Linear,
                false
            )
            .is_err()
        );
    }

    fn u_of(geometry: &Geometry, mode: CurveUMode) -> Vec<f32> {
        curve_u(geometry, mode)
            .unwrap()
            .points()
            .get(names::U)
            .unwrap()
            .as_f32(names::U)
            .unwrap()
            .to_vec()
    }

    fn path_of(points: Vec<Vec2>, closed: bool) -> Geometry {
        let count = points.len();
        let mut geometry = Geometry::from_points(points);
        geometry.push_primitive(Primitive::Path {
            verts: 0..count,
            closed,
        });
        geometry
    }

    #[test]
    fn curve_u_runs_from_zero_to_one_along_an_open_path() {
        let geometry = path_of(
            vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0), Vec2(20.0, 0.0)],
            false,
        );
        assert_eq!(u_of(&geometry, CurveUMode::ArcLength), [0.0, 0.5, 1.0]);
    }

    /// The two modes are the same ramp only when the points are evenly
    /// spaced. Uneven spacing is exactly what `by_arc_length` exists for.
    #[test]
    fn curve_u_modes_disagree_on_unevenly_spaced_points() {
        let geometry = path_of(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0), Vec2(10.0, 0.0)], false);
        let arc = u_of(&geometry, CurveUMode::ArcLength);
        let vertex = u_of(&geometry, CurveUMode::VertexOrder);
        assert_eq!(arc, [0.0, 0.1, 1.0]);
        assert_eq!(vertex, [0.0, 0.5, 1.0]);
        assert_ne!(arc, vertex);
    }

    /// Each primitive is its own `0..1`: a second path must not continue the
    /// first one's count.
    #[test]
    fn curve_u_normalises_each_primitive_independently() {
        let mut geometry = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(10.0, 0.0),
            Vec2(0.0, 5.0),
            Vec2(2.0, 5.0),
            Vec2(8.0, 5.0),
        ]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        geometry.push_primitive(Primitive::Path {
            verts: 2..5,
            closed: false,
        });
        assert_eq!(
            u_of(&geometry, CurveUMode::ArcLength),
            [0.0, 1.0, 0.0, 0.25, 1.0]
        );
    }

    /// A closed path spends part of its length getting back to the start, so
    /// the last point stops short of 1 — the wrap point is the start itself.
    #[test]
    fn curve_u_reserves_the_closing_segment_of_a_closed_path() {
        let geometry = path_of(
            vec![
                Vec2(0.0, 0.0),
                Vec2(10.0, 0.0),
                Vec2(10.0, 10.0),
                Vec2(0.0, 10.0),
            ],
            true,
        );
        assert_eq!(
            u_of(&geometry, CurveUMode::ArcLength),
            [0.0, 0.25, 0.5, 0.75]
        );
        assert_eq!(
            u_of(&geometry, CurveUMode::VertexOrder),
            [0.0, 0.25, 0.5, 0.75]
        );
    }

    /// A zero-length path has no parameter to report, and a loose point
    /// belongs to no path at all. Neither is an error.
    #[test]
    fn curve_u_reports_zero_where_there_is_no_length() {
        let degenerate = path_of(vec![Vec2(4.0, 4.0); 3], false);
        assert_eq!(u_of(&degenerate, CurveUMode::ArcLength), [0.0, 0.0, 0.0]);

        let mut loose =
            Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0), Vec2(100.0, 100.0)]);
        loose.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        assert_eq!(u_of(&loose, CurveUMode::ArcLength), [0.0, 1.0, 0.0]);
    }

    #[test]
    fn curve_u_rejects_three_dimensional_positions_and_meshes() {
        let mut spatial = Geometry::from_points3(vec![Vec3(0.0, 0.0, 0.0), Vec3(3.0, 0.0, 4.0)]);
        spatial.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        assert!(curve_u(&spatial, CurveUMode::ArcLength).is_err());

        let mut mesh = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0), Vec2(0.0, 1.0)]);
        mesh.push_mesh(0..3, &[0, 1, 2]);
        assert!(curve_u(&mesh, CurveUMode::ArcLength).is_err());
    }

    /// Arc length is planar-only for now: a 3D path has to say so rather than
    /// quietly sample its xy shadow.
    #[test]
    fn path_sampling_rejects_three_dimensional_positions() {
        let mut geometry = Geometry::from_points3(vec![
            Vec3(0.0, 0.0, 0.0),
            Vec3(3.0, 0.0, 4.0),
            Vec3(3.0, 4.0, 4.0),
        ]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: false,
        });
        let error = path_sample(&geometry, 5.0).unwrap_err();
        assert!(matches!(
            error,
            GeometryOpError::Geometry(GeometryError::RequiresPlanarP {
                operation: "attribute.path_sample",
                actual: AttributeType::Vec3,
                ..
            })
        ));
        assert!(
            error.to_string().contains("requires 2D positions"),
            "the message has to say the operation wants a 2D P: {error}"
        );
    }

    /// Attribute operations are primitive-kind-agnostic: they act on columns,
    /// which know nothing about how points are wired into primitives. A mesh
    /// has to survive one unchanged, triangles and all.
    #[test]
    fn attribute_operations_pass_meshes_through_untouched() {
        let mut geometry = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(1.0, 0.0),
            Vec2(1.0, 1.0),
            Vec2(0.0, 1.0),
        ]);
        geometry.push_mesh(0..4, &[0, 1, 2, 0, 2, 3]);

        let out = attribute_set(&geometry, Domain::Point, "heat", AttributeValue::F32(0.25))
            .expect("attribute.set does not care about primitive kinds");

        assert_eq!(out.validate(), Ok(()));
        assert_eq!(out.primitives(), geometry.primitives());
        assert_eq!(out.indices(), geometry.indices());
        assert_eq!(
            out.points().get("heat").unwrap().as_f32("heat"),
            Ok(&[0.25; 4][..])
        );
    }

    /// Arc length is a path notion. A mesh must not be silently skipped in
    /// favour of whatever path shares the geometry, so even a mesh sitting
    /// beside a perfectly good path is refused.
    #[test]
    fn path_sampling_rejects_mesh_primitives() {
        let mut geometry =
            Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(3.0, 0.0), Vec2(3.0, 4.0)]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: false,
        });
        geometry.push_mesh(0..3, &[0, 1, 2]);
        let error = path_sample(&geometry, 5.0).unwrap_err();
        assert!(matches!(
            error,
            GeometryOpError::Geometry(GeometryError::RequiresPathPrimitives {
                operation: "attribute.path_sample",
            })
        ));
        assert!(
            error.to_string().contains("requires path primitives"),
            "the message has to say the operation wants paths: {error}"
        );
    }

    #[test]
    fn bounds_center_of_three_dimensional_points_covers_z() {
        let geometry = Geometry::from_points3(vec![Vec3(2.0, 4.0, -6.0), Vec3(8.0, 10.0, 2.0)]);
        assert_eq!(bounds_center(&geometry), Some(Vec3(5.0, 7.0, -2.0)));
    }

    /// Transfer is dimension-agnostic: the nearest source point is chosen by
    /// three-component distance, so `z` separates points that share `xy`.
    #[test]
    fn transfer_uses_three_component_distance() {
        let mut source = Geometry::from_points3(vec![Vec3(0.0, 0.0, 0.0), Vec3(0.0, 0.0, 10.0)]);
        source
            .points_mut()
            .insert("value", AttributeArray::F32(vec![0.0, 10.0]))
            .unwrap();
        let target = Geometry::from_points3(vec![Vec3(0.0, 0.0, 1.0), Vec3(0.0, 0.0, 9.0)]);
        let nearest = attribute_transfer(
            &target,
            Domain::Point,
            &source,
            Domain::Point,
            "value",
            TransferMode::Nearest,
        )
        .unwrap();
        assert_eq!(
            nearest
                .points()
                .get("value")
                .unwrap()
                .as_f32("value")
                .unwrap(),
            &[0.0, 10.0]
        );
    }

    /// A 2D source and a 3D target still transfer: the missing component is
    /// `z = 0` on the 2D side.
    #[test]
    fn transfer_bridges_a_two_and_a_three_dimensional_side() {
        let mut source = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]);
        source
            .points_mut()
            .insert("value", AttributeArray::F32(vec![0.0, 10.0]))
            .unwrap();
        let target = Geometry::from_points3(vec![Vec3(1.0, 0.0, 0.0), Vec3(9.0, 0.0, 0.0)]);
        let nearest = attribute_transfer(
            &target,
            Domain::Point,
            &source,
            Domain::Point,
            "value",
            TransferMode::Nearest,
        )
        .unwrap();
        assert_eq!(
            nearest
                .points()
                .get("value")
                .unwrap()
                .as_f32("value")
                .unwrap(),
            &[0.0, 10.0]
        );
        assert_eq!(
            nearest.points().get(names::P).unwrap().attr_type(),
            AttributeType::Vec3,
            "the target keeps its own dimension"
        );
    }

    /// A field of source points large enough to build a grid, plus the
    /// targets used to probe it. `20 x 20` clears [`GRID_MIN_POINTS`], and
    /// the value is a smooth linear field so an interpolation error is
    /// readable as a distance.
    #[cfg(test)]
    fn gridded_field() -> (Geometry, Geometry, Vec<Vec2>, Vec<f32>) {
        let mut points = Vec::new();
        let mut values = Vec::new();
        for x in 0..20 {
            for y in 0..20 {
                points.push(Vec2(x as f32, y as f32));
                values.push(x as f32 + y as f32);
            }
        }
        let mut source = Geometry::from_points(points.clone());
        source
            .points_mut()
            .insert("value", AttributeArray::F32(values.clone()))
            .unwrap();
        // Deterministic probe positions scattered across the field.
        let probes: Vec<Vec2> = (0..50)
            .map(|k| Vec2((k as f32 * 7.3) % 19.0, (k as f32 * 3.1) % 19.0))
            .collect();
        (
            source,
            Geometry::from_points(probes.clone()),
            probes,
            values,
        )
    }

    /// The grid is an acceleration structure, not an approximation: on an
    /// input big enough to build one, `Nearest` must return exactly what the
    /// exhaustive scan returns — the same index, ties included.
    #[test]
    fn gridded_nearest_transfer_matches_the_exhaustive_scan() {
        let (source, target, probes, values) = gridded_field();
        assert!(
            source.point_count() >= GRID_MIN_POINTS,
            "this test is pointless unless a grid is actually built"
        );
        let got = attribute_transfer(
            &target,
            Domain::Point,
            &source,
            Domain::Point,
            "value",
            TransferMode::Nearest,
        )
        .unwrap();
        let got = got.points().get("value").unwrap().as_f32("value").unwrap();

        let source_points: Vec<Vec3> = positions(&source, Domain::Point).unwrap().iter3().collect();
        for (probe, actual) in probes.iter().zip(got) {
            let expected = values[nearest_index(&source_points, Vec3(probe.0, probe.1, 0.0))];
            assert_eq!(*actual, expected, "grid disagreed with the linear scan");
        }
    }

    /// What truncating the inverse-distance kernel costs, measured against
    /// the field the transfer is sampling.
    ///
    /// Blending only the nearest [`DISTANCE_WEIGHTED_NEIGHBOURS`] source
    /// points does not merely stay acceptable — it is *substantially more
    /// faithful* than blending all 400. Weighting every point by `1/d` drags
    /// each result toward the global mean, which on this linear field is an
    /// error of nearly ten units; the truncated kernel tracks the field to
    /// within half a unit of grid spacing.
    #[test]
    fn truncated_distance_weighting_tracks_the_field_better_than_blending_everything() {
        let (source, target, probes, values) = gridded_field();
        let got = attribute_transfer(
            &target,
            Domain::Point,
            &source,
            Domain::Point,
            "value",
            TransferMode::DistanceWeighted,
        )
        .unwrap();
        let got = got.points().get("value").unwrap().as_f32("value").unwrap();

        let source_points: Vec<Vec2> = positions(&source, Domain::Point)
            .unwrap()
            .iter3()
            .map(|p| Vec2(p.0, p.1))
            .collect();
        let mut worst_truncated = 0.0f32;
        let mut worst_exhaustive = 0.0f32;
        for (probe, actual) in probes.iter().zip(got) {
            // The pre-truncation reference: every source point, weighted 1/d.
            let squared: Vec<f32> = source_points
                .iter()
                .map(|p| planar_distance_squared(*p, *probe))
                .collect();
            // A probe sitting on a source point takes that point's value
            // whole, exactly as `normalize_into` does. Without this the
            // reference is `inf / inf` = NaN for such a probe, and
            // `f32::max` would drop it — leaving the probe out of the
            // comparison this test exists to make.
            let exhaustive: f32 = match squared.iter().position(|d| *d <= f32::EPSILON) {
                Some(hit) => values[hit],
                None => {
                    let mut weights: Vec<f32> = squared.iter().map(|d| 1.0 / d.sqrt()).collect();
                    let total: f32 = weights.iter().sum();
                    for weight in &mut weights {
                        *weight /= total;
                    }
                    weights.iter().zip(&values).map(|(w, v)| w * v).sum()
                }
            };
            assert!(
                exhaustive.is_finite(),
                "the reference must stay finite, or `f32::max` discards the probe"
            );

            let truth = probe.0 + probe.1;
            worst_truncated = worst_truncated.max((actual - truth).abs());
            worst_exhaustive = worst_exhaustive.max((exhaustive - truth).abs());
        }
        assert!(
            worst_truncated < 0.5,
            "truncated transfer drifted from the field by {worst_truncated}"
        );
        assert!(
            worst_truncated < worst_exhaustive,
            "truncation ({worst_truncated}) should beat blending everything \
             ({worst_exhaustive})"
        );
    }

    /// `PointGrid::build` declines a source whose points all coincide, so the
    /// transfer falls back to scanning every source point. The weight buffer
    /// must still be truncated there: a full row per target is
    /// `target × source` pairs held at once, where the exhaustive version this
    /// replaced peaked at one row.
    #[test]
    fn a_degenerate_source_does_not_get_a_full_weight_row_per_target() {
        let coincident: Vec<Vec3> = (0..4 * GRID_MIN_POINTS)
            .map(|_| Vec3(3.0, 4.0, 0.0))
            .collect();
        assert!(
            PointGrid::build(&coincident).is_none(),
            "a source with no extent has no grid to build"
        );
        let targets: Vec<Vec3> = (0..32).map(|i| Vec3(i as f32, 0.0, 0.0)).collect();

        let weights = SparseWeights::of(&coincident, &targets, None);
        assert_eq!(
            weights.stride, DISTANCE_WEIGHTED_NEIGHBOURS,
            "a large source is truncated with or without a grid"
        );
        assert_eq!(weights.target_count(), targets.len());
        // Still a partition of unity, so the values it blends are unchanged in
        // scale — truncation moves which points contribute, not the total.
        for index in 0..targets.len() {
            let total: f32 = weights.weights_of(index).iter().map(|(_, w)| w).sum();
            assert!(
                (total - 1.0).abs() < 1e-5,
                "target {index} weights sum to {total}"
            );
        }

        // Below the threshold the arithmetic stays exhaustive.
        let small = &coincident[..GRID_MIN_POINTS - 1];
        assert_eq!(
            SparseWeights::of(small, &targets, None).stride,
            small.len(),
            "a small source still blends every point"
        );
    }

    // -----------------------------------------------------------------------
    // Sort
    // -----------------------------------------------------------------------

    /// Four points whose x, y, and radial orders are all different, tagged
    /// with an `id` the permutation can be read off.
    fn sortable_points() -> Geometry {
        let mut geometry = Geometry::from_points(vec![
            Vec2(17.0, 3.0),
            Vec2(-5.0, 11.0),
            Vec2(8.0, -7.0),
            Vec2(2.0, 29.0),
        ]);
        geometry
            .points_mut()
            .insert(names::ID, AttributeArray::I32(vec![90, 91, 92, 93]))
            .unwrap();
        geometry
    }

    /// The `id` column of a domain, which reads back as the permutation the
    /// sort applied.
    fn ids(geometry: &Geometry, domain: Domain) -> Vec<i32> {
        geometry
            .attribute_set(domain)
            .get(names::ID)
            .unwrap()
            .as_i32(names::ID)
            .unwrap()
            .to_vec()
    }

    /// One open path from `(0, 0)` to `(30, 40)`: 50 units long, and diagonal
    /// so that the arc length of a projection is neither an x nor a y order.
    fn diagonal_path() -> Geometry {
        let mut path = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(30.0, 40.0)]);
        path.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        path
    }

    #[test]
    fn each_positional_mode_orders_the_points_its_own_way() {
        let geometry = sortable_points();
        for (mode, expected) in [
            (SortMode::X, [91, 93, 92, 90]),
            (SortMode::Y, [92, 90, 91, 93]),
            (
                SortMode::Radial {
                    center: Vec3(8.0, 3.0, 0.0),
                },
                [90, 92, 91, 93],
            ),
            (SortMode::Reverse, [93, 92, 91, 90]),
        ] {
            let result = sort(&geometry, Domain::Point, mode).unwrap();
            assert_eq!(ids(&result, Domain::Point), expected, "{mode:?}");
            // The storage slot is renumbered; `id` is the identity that
            // survives, and the input is untouched.
            assert_eq!(
                result
                    .points()
                    .get(names::INDEX)
                    .unwrap()
                    .as_i32(names::INDEX)
                    .unwrap(),
                &[0, 1, 2, 3],
                "{mode:?}"
            );
            assert_eq!(ids(&geometry, Domain::Point), [90, 91, 92, 93], "{mode:?}");
        }
    }

    /// `along_path` orders by arc length of the closest projection, which on
    /// a diagonal is neither the x nor the y order of these three points.
    #[test]
    fn along_path_orders_by_the_projected_arc_length() {
        let mut geometry = Geometry::from_points(vec![
            // Projects at 18 of 50 units.
            Vec2(30.0, 0.0),
            // Projects at 32.
            Vec2(0.0, 40.0),
            // Projects at 0.5, and is the closest to the path of the three.
            Vec2(3.0, 4.0),
        ]);
        geometry
            .points_mut()
            .insert(names::ID, AttributeArray::I32(vec![70, 71, 72]))
            .unwrap();
        let path = diagonal_path();

        let result = sort(
            &geometry,
            Domain::Point,
            SortMode::AlongPath { path: &path },
        )
        .unwrap();

        assert_eq!(ids(&result, Domain::Point), [72, 70, 71]);
        // Not the x order (71, 72, 70) and not the y order (70, 72, 71).
        assert_ne!(
            ids(
                &sort(&geometry, Domain::Point, SortMode::X).unwrap(),
                Domain::Point
            ),
            ids(&result, Domain::Point)
        );
        assert_ne!(
            ids(
                &sort(&geometry, Domain::Point, SortMode::Y).unwrap(),
                Domain::Point
            ),
            ids(&result, Domain::Point)
        );
    }

    /// The shuffle is the shared `element_hash` order, so one seed means one
    /// arrangement and a second run reproduces it exactly.
    #[test]
    fn random_is_the_seeded_hash_order_and_reproduces() {
        let mut geometry =
            Geometry::from_points((0..8).map(|i| Vec2(i as f32 * 13.0 - 40.0, 5.5)).collect());
        geometry
            .points_mut()
            .insert(
                names::ID,
                AttributeArray::I32(vec![60, 61, 62, 63, 64, 65, 66, 67]),
            )
            .unwrap();

        let seeded = |seed| sort(&geometry, Domain::Point, SortMode::Random { seed }).unwrap();
        let mut expected: Vec<usize> = (0..8).collect();
        expected.sort_by_key(|index| element_hash(7, *index as u32));

        assert_eq!(
            ids(&seeded(7), Domain::Point),
            expected.iter().map(|i| 60 + *i as i32).collect::<Vec<_>>()
        );
        assert_eq!(
            ids(&seeded(7), Domain::Point),
            ids(&seeded(7), Domain::Point)
        );
        assert_ne!(
            ids(&seeded(7), Domain::Point),
            ids(&seeded(8), Domain::Point)
        );
        assert_ne!(
            ids(&seeded(7), Domain::Point),
            [60, 61, 62, 63, 64, 65, 66, 67],
            "a shuffle that leaves storage order alone is not a shuffle"
        );
    }

    /// Every column type answers a key: the scalars by value, the strings
    /// lexicographically, and a vector or colour by its first component.
    #[test]
    fn sorting_by_an_attribute_reads_every_column_type() {
        let mut geometry =
            Geometry::from_points(vec![Vec2(1.5, 2.5), Vec2(3.5, 4.5), Vec2(5.5, 6.5)]);
        let color = |r: f32| Color {
            r,
            g: 0.42,
            b: 0.42,
            a: 1.0,
        };
        for (name, column) in [
            ("f32", AttributeArray::F32(vec![2.5, -1.25, 7.75])),
            ("i32", AttributeArray::I32(vec![30, -10, 70])),
            ("bool", AttributeArray::Bool(vec![true, false, true])),
            (
                "str",
                AttributeArray::Str(vec!["mango".into(), "apple".into(), "zebra".into()]),
            ),
            (
                "vec2",
                AttributeArray::Vec2(vec![Vec2(2.5, 9.0), Vec2(-1.25, 9.0), Vec2(7.75, 9.0)]),
            ),
            (
                "vec3",
                AttributeArray::Vec3(vec![
                    Vec3(2.5, 9.0, 9.0),
                    Vec3(-1.25, 9.0, 9.0),
                    Vec3(7.75, 9.0, 9.0),
                ]),
            ),
            (
                "vec4",
                AttributeArray::Vec4(vec![
                    Vec4(2.5, 9.0, 9.0, 9.0),
                    Vec4(-1.25, 9.0, 9.0, 9.0),
                    Vec4(7.75, 9.0, 9.0, 9.0),
                ]),
            ),
            (
                "color",
                AttributeArray::Color(vec![color(0.6), color(0.1), color(0.9)]),
            ),
        ] {
            geometry.points_mut().insert(name, column).unwrap();
        }
        geometry
            .points_mut()
            .insert(names::ID, AttributeArray::I32(vec![50, 51, 52]))
            .unwrap();

        for name in ["f32", "i32", "bool", "str", "vec2", "vec3", "vec4", "color"] {
            let result = sort(&geometry, Domain::Point, SortMode::Attribute(name)).unwrap();
            assert_eq!(
                ids(&result, Domain::Point),
                [51, 50, 52],
                "sorted by {name}"
            );
        }

        assert!(matches!(
            sort(&geometry, Domain::Point, SortMode::Attribute("absent")),
            Err(GeometryOpError::Geometry(
                GeometryError::AttributeNotFound { .. }
            ))
        ));
    }

    /// A column left behind by the permutation would silently detach its
    /// values from the elements they describe, so every type in every element
    /// domain is checked against the one permutation the key implies.
    #[test]
    fn every_column_of_every_domain_moves_by_the_same_permutation() {
        /// Ascending `rank` puts the elements in this order.
        const EXPECTED: [usize; 4] = [1, 3, 0, 2];

        for domain in [Domain::Point, Domain::Primitive, Domain::Instance] {
            let mut geometry = Geometry::from_points(vec![
                Vec2(1.5, 2.5),
                Vec2(3.5, 4.5),
                Vec2(5.5, 6.5),
                Vec2(7.5, 8.5),
            ]);
            // The primitive domain needs four primitives to sort. The point
            // domain must stay free of them: a primitive pins its own points
            // into a run, which is what
            // `a_point_sort_stays_inside_each_primitives_vertex_run` covers.
            if domain == Domain::Primitive {
                for vertex in 0..4 {
                    geometry.push_primitive(Primitive::Path {
                        verts: vertex..vertex + 1,
                        closed: false,
                    });
                }
            }
            let color = |r: f32, g: f32| Color {
                r,
                g,
                b: 0.375,
                a: 0.875,
            };
            for (name, column) in [
                ("rank", AttributeArray::I32(vec![30, 10, 40, 20])),
                (
                    names::P,
                    AttributeArray::Vec2(vec![
                        Vec2(11.5, 12.5),
                        Vec2(13.5, 14.5),
                        Vec2(15.5, 16.5),
                        Vec2(17.5, 18.5),
                    ]),
                ),
                ("f32", AttributeArray::F32(vec![7.5, 8.25, 9.125, 10.0625])),
                (
                    "vec3",
                    AttributeArray::Vec3(vec![
                        Vec3(1.25, 2.25, 3.25),
                        Vec3(4.25, 5.25, 6.25),
                        Vec3(7.25, 8.25, 9.25),
                        Vec3(10.25, 11.25, 12.25),
                    ]),
                ),
                (
                    "vec4",
                    AttributeArray::Vec4(vec![
                        Vec4(0.125, 1.125, 2.125, 3.125),
                        Vec4(4.125, 5.125, 6.125, 7.125),
                        Vec4(8.125, 9.125, 10.125, 11.125),
                        Vec4(12.125, 13.125, 14.125, 15.125),
                    ]),
                ),
                (
                    "color",
                    AttributeArray::Color(vec![
                        color(0.125, 0.25),
                        color(0.375, 0.5),
                        color(0.625, 0.75),
                        color(0.875, 0.9375),
                    ]),
                ),
                (names::ID, AttributeArray::I32(vec![41, 42, 43, 44])),
                ("bool", AttributeArray::Bool(vec![true, false, true, false])),
                (
                    "str",
                    AttributeArray::Str(vec![
                        "alpha".into(),
                        "bravo".into(),
                        "charlie".into(),
                        "delta".into(),
                    ]),
                ),
            ] {
                geometry
                    .attribute_set_mut(domain)
                    .insert(name, column)
                    .unwrap();
            }

            let result = sort(&geometry, domain, SortMode::Attribute("rank")).unwrap();

            macro_rules! assert_permuted {
                ($accessor:ident, $name:expr) => {{
                    let before = geometry
                        .attribute_set(domain)
                        .get($name)
                        .unwrap()
                        .$accessor($name)
                        .unwrap()
                        .to_vec();
                    let after = result
                        .attribute_set(domain)
                        .get($name)
                        .unwrap()
                        .$accessor($name)
                        .unwrap();
                    let expected: Vec<_> =
                        EXPECTED.iter().map(|slot| before[*slot].clone()).collect();
                    assert_eq!(
                        after,
                        expected.as_slice(),
                        "{:?} column {} did not follow the permutation",
                        domain,
                        $name
                    );
                }};
            }
            assert_permuted!(as_i32, "rank");
            assert_permuted!(as_vec2, names::P);
            assert_permuted!(as_f32, "f32");
            assert_permuted!(as_vec3, "vec3");
            assert_permuted!(as_vec4, "vec4");
            assert_permuted!(as_color, "color");
            assert_permuted!(as_i32, names::ID);
            assert_permuted!(as_bool, "bool");
            assert_permuted!(as_str, "str");
        }
    }

    /// A path spans a contiguous run, so the permutation is confined to it:
    /// the two paths sort their own vertices and neither borrows the other's.
    #[test]
    fn a_point_sort_stays_inside_each_primitives_vertex_run() {
        let mut geometry = Geometry::from_points(vec![
            Vec2(10.0, 1.0),
            Vec2(2.0, 1.0),
            Vec2(6.0, 1.0),
            Vec2(8.0, 2.0),
            Vec2(4.0, 2.0),
            Vec2(12.0, 2.0),
        ]);
        geometry
            .points_mut()
            .insert(names::ID, AttributeArray::I32(vec![80, 81, 82, 83, 84, 85]))
            .unwrap();
        for verts in [0..3, 3..6] {
            geometry.push_primitive(Primitive::Path {
                verts,
                closed: false,
            });
        }

        let result = sort(&geometry, Domain::Point, SortMode::X).unwrap();

        assert_eq!(ids(&result, Domain::Point), [81, 82, 80, 84, 83, 85]);
        assert_eq!(
            result
                .positions(Domain::Point)
                .unwrap()
                .unwrap()
                .planar()
                .unwrap()
                .iter()
                .map(|p| p.0)
                .collect::<Vec<_>>(),
            // Sorted within each run — a global sort would read
            // 2, 4, 6, 8, 10, 12 and would have moved points between paths.
            [2.0, 6.0, 10.0, 4.0, 8.0, 12.0]
        );
        assert_eq!(result.primitives(), geometry.primitives());
        result.validate().unwrap();
    }

    /// Reordering the primitive domain moves the primitives themselves, so
    /// the vertex runs stay attached to the primitive that owns them.
    #[test]
    fn a_primitive_sort_moves_the_primitives_with_their_attributes() {
        let mut geometry = Geometry::from_points(vec![
            Vec2(20.0, 0.0),
            Vec2(24.0, 0.0),
            Vec2(2.0, 0.0),
            Vec2(6.0, 0.0),
        ]);
        for verts in [0..2, 2..4] {
            geometry.push_primitive(Primitive::Path {
                verts,
                closed: false,
            });
        }
        geometry
            .primitive_attrs_mut()
            .insert(
                "tag",
                AttributeArray::Str(vec!["far".into(), "near".into()]),
            )
            .unwrap();

        // Centroid x is 22 for the first primitive and 4 for the second, so
        // ascending x swaps them.
        let result = sort(&geometry, Domain::Primitive, SortMode::X).unwrap();

        assert_eq!(
            result.primitives(),
            &[
                Primitive::Path {
                    verts: 2..4,
                    closed: false
                },
                Primitive::Path {
                    verts: 0..2,
                    closed: false
                },
            ]
        );
        assert_eq!(
            result
                .primitive_attrs()
                .get("tag")
                .unwrap()
                .as_str("tag")
                .unwrap(),
            &["near".to_owned(), "far".to_owned()]
        );
        // The points did not move: only the primitives did.
        assert_eq!(
            result
                .positions(Domain::Point)
                .unwrap()
                .unwrap()
                .planar()
                .unwrap(),
            [
                Vec2(20.0, 0.0),
                Vec2(24.0, 0.0),
                Vec2(2.0, 0.0),
                Vec2(6.0, 0.0)
            ]
        );
        result.validate().unwrap();
    }

    /// An instance keeps the source it stamps: `source_index` travels with it
    /// and the source list itself is left exactly as it came in.
    #[test]
    fn instances_keep_the_source_they_stamp() {
        let sources: Vec<Arc<Geometry>> = (1..=3)
            .map(|count| {
                Arc::new(Geometry::from_points(
                    (0..count).map(|i| Vec2(i as f32 * 3.5, 1.75)).collect(),
                ))
            })
            .collect();
        let mut geometry = Geometry::new();
        geometry
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(31.0, 0.0), Vec2(-7.0, 0.0), Vec2(13.0, 0.0)]),
            )
            .unwrap();
        geometry
            .instances_mut()
            .insert(names::SOURCE_INDEX, AttributeArray::I32(vec![2, 0, 1]))
            .unwrap();
        geometry
            .instances_mut()
            .insert(names::INDEX, AttributeArray::I32(vec![0, 1, 2]))
            .unwrap();
        geometry.set_instance_sources(sources.clone());

        let result = sort(&geometry, Domain::Instance, SortMode::X).unwrap();

        assert_eq!(
            result
                .instances()
                .get(names::SOURCE_INDEX)
                .unwrap()
                .as_i32(names::SOURCE_INDEX)
                .unwrap(),
            // x order is -7, 13, 31, so the selectors follow: 0, 1, 2.
            &[0, 1, 2]
        );
        assert_eq!(
            result
                .instances()
                .get(names::INDEX)
                .unwrap()
                .as_i32(names::INDEX)
                .unwrap(),
            &[0, 1, 2]
        );
        assert_eq!(result.sources().len(), 3);
        for (before, after) in sources.iter().zip(result.sources()) {
            assert!(
                after
                    .geometry()
                    .is_some_and(|source| Arc::ptr_eq(source, before)),
                "the source list is not the sort's to rewrite"
            );
        }
        result.validate().unwrap();
    }

    /// A mesh's triangle indices are relative to its vertex run, so moving
    /// its points would reface it; that is an error, not a reordering.
    #[test]
    fn a_point_sort_refuses_a_mesh_and_overlapping_runs() {
        let mut mesh = Geometry::from_points(vec![Vec2(9.5, 0.0), Vec2(1.5, 0.0), Vec2(5.5, 4.0)]);
        mesh.push_mesh(0..3, &[0, 1, 2]);
        assert!(matches!(
            sort(&mesh, Domain::Point, SortMode::X),
            Err(GeometryOpError::Geometry(
                GeometryError::RequiresPathPrimitives { .. }
            ))
        ));

        let mut overlapping = Geometry::from_points(vec![
            Vec2(9.5, 0.0),
            Vec2(1.5, 0.0),
            Vec2(5.5, 4.0),
            Vec2(3.5, 8.0),
        ]);
        for verts in [0..3, 1..4] {
            overlapping.push_primitive(Primitive::Path {
                verts,
                closed: false,
            });
        }
        assert!(matches!(
            sort(&overlapping, Domain::Point, SortMode::X),
            Err(GeometryOpError::OverlappingVertexRuns { point: 1, .. })
        ));
    }

    /// A domain too short to reorder, and the detail domain that holds one
    /// element by definition, come back unchanged rather than erroring — an
    /// animated element count passes through both every frame.
    #[test]
    fn a_domain_with_nothing_to_reorder_passes_through() {
        // An empty geometry has no `P` column to read a key out of, so the
        // guard is what makes an empty frame a pass-through rather than an
        // error.
        assert_eq!(
            sort(&Geometry::new(), Domain::Point, SortMode::X)
                .unwrap()
                .point_count(),
            0
        );

        let single = Geometry::from_points(vec![Vec2(6.25, -3.75)]);
        assert_eq!(
            sort(&single, Domain::Point, SortMode::X)
                .unwrap()
                .positions(Domain::Point)
                .unwrap()
                .unwrap()
                .planar()
                .unwrap(),
            [Vec2(6.25, -3.75)]
        );

        let mut detail = sortable_points();
        detail
            .detail_mut()
            .insert("label", AttributeArray::Str(vec!["kept".into()]))
            .unwrap();
        let result = sort(&detail, Domain::Detail, SortMode::X).unwrap();
        assert_eq!(ids(&result, Domain::Point), [90, 91, 92, 93]);
        assert_eq!(
            result
                .detail()
                .get("label")
                .unwrap()
                .as_str("label")
                .unwrap(),
            &["kept".to_owned()]
        );
    }

    // -----------------------------------------------------------------------
    // Instance expansion (typography-plan unit 5)
    // -----------------------------------------------------------------------

    /// A source with two closed contours and curved control points, standing
    /// in for a glyph: an outer contour and its counter, plus the tangents
    /// `text.layout` writes.
    ///
    /// Values are deliberately off every default — no unit scale, no zero
    /// tangent, no origin-centred point — so a step the expansion forgets
    /// cannot be mistaken for a default that happened to be right.
    fn glyph_source() -> Geometry {
        let mut geometry = Geometry::from_points(vec![
            Vec2(2.0, 0.0),
            Vec2(2.0, 6.0),
            Vec2(6.0, 6.0),
            // The counter.
            Vec2(3.0, 1.0),
            Vec2(3.0, 4.0),
            Vec2(5.0, 4.0),
        ]);
        geometry
            .points_mut()
            .insert(
                names::IN_TAN,
                AttributeArray::Vec2(vec![
                    Vec2(0.5, -0.25),
                    Vec2(0.0, 0.0),
                    Vec2(0.0, 0.0),
                    Vec2(-0.75, 0.0),
                    Vec2(0.0, 0.0),
                    Vec2(0.0, 0.0),
                ]),
            )
            .expect("in_tan is one value per point");
        geometry
            .points_mut()
            .insert(
                names::OUT_TAN,
                AttributeArray::Vec2(vec![
                    Vec2(0.0, 1.5),
                    Vec2(0.0, 0.0),
                    Vec2(0.0, 0.0),
                    Vec2(0.0, 2.25),
                    Vec2(0.0, 0.0),
                    Vec2(0.0, 0.0),
                ]),
            )
            .expect("out_tan is one value per point");
        geometry.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: true,
        });
        geometry.push_primitive(Primitive::Path {
            verts: 3..6,
            closed: true,
        });
        geometry
    }

    /// A three-character text layout over `glyph_source`: distinct offsets,
    /// turns and scales, plus the per-character columns unit 2 writes.
    fn laid_out_text() -> Geometry {
        let mut geometry = Geometry::new();
        let instances = geometry.instances_mut();
        instances
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(40.0, 90.0), Vec2(70.0, 90.0), Vec2(100.0, 90.0)]),
            )
            .expect("three offsets");
        instances
            .insert(
                names::ROT,
                AttributeArray::F32(vec![0.0, std::f32::consts::FRAC_PI_2, -0.75]),
            )
            .expect("three turns");
        instances
            .insert(
                names::SCALE,
                AttributeArray::Vec2(vec![Vec2(2.0, 2.0), Vec2(3.0, 3.0), Vec2(1.5, 0.5)]),
            )
            .expect("three scales");
        instances
            .insert(names::INDEX, AttributeArray::I32(vec![0, 1, 2]))
            .expect("three indices");
        instances
            .insert(names::CHAR_INDEX, AttributeArray::I32(vec![0, 1, 2]))
            .expect("three char indices");
        instances
            .insert(names::WORD_INDEX, AttributeArray::I32(vec![3, 3, 4]))
            .expect("three word indices");
        instances
            .insert(names::LINE_INDEX, AttributeArray::I32(vec![7, 7, 7]))
            .expect("three line indices");
        instances
            .insert(
                names::CHAR_PROGRESS,
                AttributeArray::F32(vec![0.0, 0.5, 1.0]),
            )
            .expect("three progresses");
        instances
            .insert(names::ADVANCE, AttributeArray::F32(vec![30.0, 30.0, 26.5]))
            .expect("three advances");
        geometry.set_instance_source(Some(Arc::new(glyph_source())));
        geometry
    }

    fn vec2_column(geometry: &Geometry, name: &str) -> Vec<Vec2> {
        geometry
            .points()
            .get(name)
            .unwrap_or_else(|| panic!("the expansion writes {name}"))
            .as_vec2(name)
            .expect("a Vec2 column")
            .to_vec()
    }

    fn i32_column(set: &AttributeSet, name: &str) -> Vec<i32> {
        set.get(name)
            .unwrap_or_else(|| panic!("the expansion writes {name}"))
            .as_i32(name)
            .expect("an I32 column")
            .to_vec()
    }

    /// The first completion criterion of typography-plan unit 5: the expanded
    /// point count is the sum of the glyph outlines' own point counts.
    #[test]
    fn expanding_instances_sums_the_point_count_of_every_source() {
        let text = laid_out_text();
        let expanded = expand_instances(&text).expect("the layout expands");
        let source_points: usize = (0..text.instance_count())
            .map(|_| glyph_source().point_count())
            .sum();
        assert_eq!(source_points, 18, "three glyphs of six points");
        assert_eq!(expanded.point_count(), source_points);
        assert_eq!(
            expanded.primitive_count(),
            2 * text.instance_count(),
            "every contour of every glyph has to survive"
        );
        // Flat: the output is geometry, not instances of geometry.
        assert_eq!(expanded.instance_count(), 0);
        assert!(expanded.sources().is_empty());
    }

    /// The placement is baked, not dropped: `P`, `rot` and `scale` all move
    /// the outline points.
    #[test]
    fn an_instances_placement_is_baked_into_its_outline_points() {
        let expanded = expand_instances(&laid_out_text()).expect("the layout expands");
        let positions = vec2_column(&expanded, names::P);
        let source = glyph_source();
        let source_positions = source
            .points()
            .get(names::P)
            .expect("P")
            .as_vec2(names::P)
            .expect("Vec2")
            .to_vec();

        let placements = [
            InstanceTransform {
                offset: Vec2(40.0, 90.0),
                rot: 0.0,
                scale: Vec2(2.0, 2.0),
                shear: 0.0,
            },
            InstanceTransform {
                offset: Vec2(70.0, 90.0),
                rot: std::f32::consts::FRAC_PI_2,
                scale: Vec2(3.0, 3.0),
                shear: 0.0,
            },
            InstanceTransform {
                offset: Vec2(100.0, 90.0),
                rot: -0.75,
                scale: Vec2(1.5, 0.5),
                shear: 0.0,
            },
        ];
        for (instance, placement) in placements.iter().enumerate() {
            for (vertex, source_position) in source_positions.iter().enumerate() {
                let expected = placement.apply(*source_position);
                let actual = positions[instance * source_positions.len() + vertex];
                assert!(
                    (actual.0 - expected.0).abs() < 1e-4 && (actual.1 - expected.1).abs() < 1e-4,
                    "instance {instance} vertex {vertex}: {actual:?} is not {expected:?}"
                );
            }
        }
        // And the three glyphs are not on top of each other, which a dropped
        // offset would make them.
        assert!(
            positions[0] != positions[6] && positions[6] != positions[12],
            "the three glyphs have to land in three places: {positions:?}"
        );
    }

    /// A tangent is a difference: the turn and the scale reach it, the offset
    /// does not. Translating it would drag every control point to the
    /// instance origin and straighten the glyph.
    #[test]
    fn tangents_take_the_linear_part_of_the_placement_only() {
        let expanded = expand_instances(&laid_out_text()).expect("the layout expands");
        let in_tans = vec2_column(&expanded, names::IN_TAN);
        let out_tans = vec2_column(&expanded, names::OUT_TAN);

        // Instance 0: scale 2, no turn.
        assert!(
            (in_tans[0].0 - 1.0).abs() < 1e-5 && (in_tans[0].1 - (-0.5)).abs() < 1e-5,
            "an unturned tangent only scales: {:?}",
            in_tans[0]
        );
        // Instance 1 (points 6..12): scale 3 and a quarter turn, so
        // (0, 1.5) becomes (-4.5, 0).
        assert!(
            (out_tans[6].0 - (-4.5)).abs() < 1e-4 && out_tans[6].1.abs() < 1e-4,
            "a turned tangent turns: {:?}",
            out_tans[6]
        );
        // Zero stays zero: a corner point stays a corner.
        assert_eq!(in_tans[1], Vec2(0.0, 0.0));
        assert_eq!(in_tans[7], Vec2(0.0, 0.0));
        // No tangent is anywhere near the instance offsets (40, 70, 100).
        for tangent in in_tans.iter().chain(&out_tans) {
            assert!(
                tangent.0.abs() < 20.0 && tangent.1.abs() < 20.0,
                "a tangent carrying the offset: {tangent:?}"
            );
        }
    }

    /// The second completion criterion of unit 5: the per-character columns
    /// `text.layout` wrote on the Instance domain reach the Point domain, so
    /// a Point-domain field can read them.
    #[test]
    fn per_character_attributes_descend_to_the_point_and_primitive_domains() {
        let expanded = expand_instances(&laid_out_text()).expect("the layout expands");
        // Six points per glyph, three glyphs: every point of a character
        // carries that character's own value.
        let expected = |per_character: [i32; 3]| -> Vec<i32> {
            per_character
                .iter()
                .flat_map(|value| std::iter::repeat_n(*value, 6))
                .collect()
        };
        for (name, per_character) in [
            (names::CHAR_INDEX, [0, 1, 2]),
            (names::WORD_INDEX, [3, 3, 4]),
            (names::LINE_INDEX, [7, 7, 7]),
        ] {
            assert_eq!(
                i32_column(expanded.points(), name),
                expected(per_character),
                "{name} did not descend onto every point of its own character"
            );
            // Primitive domain too: `rasterize` resolves a path's style from
            // there, so a per-character attribute that stopped at the points
            // could not colour a character. Two contours per glyph.
            assert_eq!(
                i32_column(expanded.primitive_attrs(), name),
                per_character
                    .iter()
                    .flat_map(|value| std::iter::repeat_n(*value, 2))
                    .collect::<Vec<_>>(),
            );
        }
        // `index` is the exception: it is creation order within a domain, so
        // the expansion renumbers it the way `sort` does rather than letting
        // either the glyph's or the character's numbering through.
        assert_eq!(
            i32_column(expanded.points(), names::INDEX),
            (0..18).collect::<Vec<_>>()
        );
        let progress = expanded
            .points()
            .get(names::CHAR_PROGRESS)
            .expect("char_progress descends")
            .as_f32(names::CHAR_PROGRESS)
            .expect("an F32 column")
            .to_vec();
        assert_eq!(progress[0], 0.0);
        assert_eq!(progress[6], 0.5);
        assert_eq!(progress[12], 1.0);
        assert!(
            expanded.points().get(names::ADVANCE).is_some(),
            "advance descends like the rest"
        );
    }

    /// The placement columns do **not** descend. A Point-domain `rot` or
    /// `scale` would describe a placement that has already been applied, and
    /// `source_index` would name a source list the expansion no longer has.
    #[test]
    fn the_placement_columns_do_not_descend_onto_the_points() {
        let mut text = laid_out_text();
        text.instances_mut()
            .insert(names::SOURCE_INDEX, AttributeArray::I32(vec![0, 0, 0]))
            .expect("three source indices");
        let expanded = expand_instances(&text).expect("the layout expands");
        for name in [names::ROT, names::SCALE, names::SOURCE_INDEX] {
            assert!(
                expanded.points().get(name).is_none(),
                "{name} is the placement, and the placement is baked in"
            );
            assert!(expanded.primitive_attrs().get(name).is_none());
        }
        // `P` is there, but it is the outline point rather than the
        // character origin.
        let positions = vec2_column(&expanded, names::P);
        assert_ne!(positions[0], Vec2(40.0, 90.0));
    }

    /// The contour-order invariant `rasterize` depends on: one character's
    /// contours are contiguous and in the source's own order.
    ///
    /// The rasterizer fills a *run of consecutive* same-style closed paths as
    /// one non-zero region, which is what opens the counter of an `o`
    /// (`rasterize`'s `FillRun`). Interleaving two characters' contours would
    /// separate a counter from its outer contour whenever the two characters
    /// differ in style, and the hole would fill in.
    #[test]
    fn every_characters_contours_stay_consecutive_and_in_order() {
        let expanded = expand_instances(&laid_out_text()).expect("the layout expands");
        let per_point = i32_column(expanded.points(), names::CHAR_INDEX);

        // Which character each primitive belongs to, read from the points it
        // is built from rather than from the primitive's own column, so the
        // two have to agree.
        let mut owners = Vec::new();
        for primitive in expanded.primitives() {
            let verts = primitive.verts();
            let owner = per_point[verts.start];
            assert!(
                verts.clone().all(|vertex| per_point[vertex] == owner),
                "a primitive spans two characters: {verts:?}"
            );
            owners.push(owner);
        }
        assert_eq!(
            owners,
            vec![0, 0, 1, 1, 2, 2],
            "each character's contours have to sit together, in order"
        );
        // And the vertex runs march forward, which is what "consecutive"
        // means for the point domain the runs index into.
        let starts: Vec<usize> = expanded
            .primitives()
            .iter()
            .map(|primitive| primitive.verts().start)
            .collect();
        assert!(
            starts.windows(2).all(|pair| pair[0] < pair[1]),
            "the contours are out of order: {starts:?}"
        );
    }

    /// A geometry with nothing to expand comes back as it is, so a
    /// `text.to_path` on an already-flat geometry is a pass-through.
    #[test]
    fn a_geometry_without_instances_expands_to_itself() {
        let flat = glyph_source();
        let expanded = expand_instances(&flat).expect("a flat geometry expands");
        assert_eq!(expanded.point_count(), flat.point_count());
        assert_eq!(expanded.primitive_count(), flat.primitive_count());
        assert_eq!(
            vec2_column(&expanded, names::P),
            vec2_column(&flat, names::P)
        );
        // Idempotent: expanding the expansion changes nothing either.
        let again =
            expand_instances(&expand_instances(&laid_out_text()).expect("once")).expect("twice");
        let once = expand_instances(&laid_out_text()).expect("once");
        assert_eq!(vec2_column(&again, names::P), vec2_column(&once, names::P));
    }

    /// A source's own column wins over the instance's, which is how
    /// `rasterize` narrows a style per element.
    #[test]
    fn a_sources_own_column_wins_over_the_instances() {
        let mut source = glyph_source();
        source
            .points_mut()
            .insert(names::ALPHA, AttributeArray::F32(vec![0.25; 6]))
            .expect("one alpha per point");
        let mut text = laid_out_text();
        text.set_instance_source(Some(Arc::new(source)));
        text.instances_mut()
            .insert(names::ALPHA, AttributeArray::F32(vec![0.9, 0.9, 0.9]))
            .expect("three alphas");

        let expanded = expand_instances(&text).expect("the layout expands");
        let alpha = expanded
            .points()
            .get(names::ALPHA)
            .expect("alpha")
            .as_f32(names::ALPHA)
            .expect("an F32 column")
            .to_vec();
        assert_eq!(alpha, vec![0.25; 18], "the source's own alpha has to win");
    }

    /// Sources with different columns concatenate with each missing column
    /// filled by what a reader sees without it, rather than the first
    /// source's schema deciding for the rest. A point with no `pscale` draws
    /// with radius 2 (`rasterize`'s default), so the fill is 2.0 and not the
    /// typed zero, which would draw it with no radius at all.
    #[test]
    fn sources_with_different_columns_fill_with_their_absent_values() {
        let mut plain = Geometry::from_points(vec![Vec2(1.0, 1.0), Vec2(2.0, 2.0)]);
        plain.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        let mut labelled = Geometry::from_points(vec![Vec2(3.0, 3.0)]);
        labelled
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![8.0]))
            .expect("one pscale");

        let mut geometry = Geometry::new();
        geometry
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(50.0, 0.0), Vec2(0.0, 50.0)]),
            )
            .expect("three offsets");
        // The source *without* `pscale` goes first, so the column appears
        // partway through and has to be back-filled for the rows already
        // accumulated as well as forward-filled for the ones after it.
        geometry
            .instances_mut()
            .insert(names::SOURCE_INDEX, AttributeArray::I32(vec![0, 1, 0]))
            .expect("three source indices");
        geometry.set_instance_sources(vec![Arc::new(plain), Arc::new(labelled)]);

        let expanded = expand_instances(&geometry).expect("mixed sources expand");
        assert_eq!(expanded.point_count(), 2 + 1 + 2);
        let pscale = expanded
            .points()
            .get(names::PSCALE)
            .expect("pscale survives")
            .as_f32(names::PSCALE)
            .expect("an F32 column")
            .to_vec();
        assert_eq!(
            pscale,
            vec![2.0, 2.0, 8.0, 2.0, 2.0],
            "the sources that have no pscale read as the default radius"
        );
        expanded
            .validate()
            .expect("every column has to be as long as the point domain");
    }

    /// A path `0..len` of points at the origin.
    fn open_path(len: usize) -> Geometry {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0); len]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..len,
            closed: false,
        });
        geometry
    }

    /// A host shape expanded together with stamped characters that carry
    /// `alpha` and `Cd`: the host's elements read as opaque white, as they did
    /// before the expansion, and not as transparent black.
    #[test]
    fn a_host_shape_keeps_its_default_alpha_and_colour_through_expansion() {
        let mut host = open_path(3);
        let half = AttributeArray::F32(vec![0.5]);
        let red = Color::new(1.0, 0.0, 0.0, 1.0);
        host.instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(10.0, 0.0)]))
            .unwrap();
        host.instances_mut().insert(names::ALPHA, half).unwrap();
        host.instances_mut()
            .insert(names::CD, AttributeArray::Color(vec![red]))
            .unwrap();
        host.set_instance_sources(vec![Arc::new(open_path(2))]);

        let expanded = expand_instances(&host).expect("expands");
        let points = expanded.points();
        assert_eq!(
            points
                .get(names::ALPHA)
                .unwrap()
                .as_f32(names::ALPHA)
                .unwrap(),
            [1.0, 1.0, 1.0, 0.5, 0.5]
        );
        let white = Color::new(1.0, 1.0, 1.0, 1.0);
        let prims = expanded.primitive_attrs();
        assert_eq!(
            prims
                .get(names::ALPHA)
                .unwrap()
                .as_f32(names::ALPHA)
                .unwrap(),
            [1.0, 0.5]
        );
        assert_eq!(
            prims.get(names::CD).unwrap().as_color(names::CD).unwrap(),
            [white, red]
        );
        // The host path's vertices read as the path's own (white) stroke.
        assert_eq!(
            points.get(names::CD).unwrap().as_color(names::CD).unwrap(),
            [white, white, white, red, red]
        );
    }

    fn one_piece(attributes: &[(&str, AttributeArray)]) -> InstancePiece {
        let mut row = AttributeSet::new();
        for (name, column) in attributes {
            row.insert(*name, column.clone()).unwrap();
        }
        InstancePiece {
            source: InstanceSource::Geometry(Arc::new(Geometry::new())),
            attributes: row,
        }
    }

    fn three_instances() -> Geometry {
        let mut geometry = Geometry::new();
        geometry
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 0.0); 3]))
            .unwrap();
        geometry
            .instances_mut()
            .insert(names::SOURCE_INDEX, AttributeArray::I32(vec![0, 1, 2]))
            .unwrap();
        geometry
    }

    /// A piece with no `stroke_color` strokes in its row's fill colour: the
    /// piece's own `Cd`, white if it has none, and an output `Cd` that already
    /// exists decides and stays as it is.
    #[test]
    fn a_piece_without_a_stroke_colour_strokes_in_its_rows_fill_colour() {
        let red = Color::new(1.0, 0.0, 0.0, 1.0);
        let green = Color::new(0.0, 1.0, 0.0, 1.0);
        let blue = Color::new(0.0, 0.0, 1.0, 1.0);
        let white = Color::new(1.0, 1.0, 1.0, 1.0);
        let pieces = [
            one_piece(&[(names::STROKE_COLOR, AttributeArray::Color(vec![green]))]),
            one_piece(&[(names::CD, AttributeArray::Color(vec![red]))]),
            one_piece(&[]),
        ];

        let mut geometry = three_instances();
        attach_piece_attributes(&mut geometry, &pieces).unwrap();
        let get = |name| {
            geometry
                .instances()
                .get(name)
                .unwrap()
                .as_color(name)
                .unwrap()
                .to_vec()
        };
        // Piece 0 and 2 have no `Cd`: the reader's neutral white.
        assert_eq!(get(names::CD), [white, red, white]);
        assert_eq!(get(names::STROKE_COLOR), [green, red, white]);

        // An output `Cd` is the row's fill and is not overwritten.
        let mut geometry = three_instances();
        geometry
            .instances_mut()
            .insert(names::CD, AttributeArray::Color(vec![blue; 3]))
            .unwrap();
        attach_piece_attributes(&mut geometry, &pieces).unwrap();
        let get = |name| {
            geometry
                .instances()
                .get(name)
                .unwrap()
                .as_color(name)
                .unwrap()
                .to_vec()
        };
        assert_eq!(get(names::CD), [blue; 3]);
        assert_eq!(get(names::STROKE_COLOR), [green, blue, blue]);
    }

    /// A reserved column with a constant absent value is filled with it for a
    /// piece that lacks it, and an unreserved one with the typed zero.
    #[test]
    fn a_piece_without_alpha_is_opaque_and_without_a_user_column_is_zero() {
        let pieces = [
            one_piece(&[
                (names::ALPHA, AttributeArray::F32(vec![0.25])),
                ("mine", AttributeArray::F32(vec![7.0])),
            ]),
            one_piece(&[]),
        ];
        let mut geometry = three_instances();
        geometry
            .instances_mut()
            .insert(names::SOURCE_INDEX, AttributeArray::I32(vec![0, 1, 1]))
            .unwrap();
        attach_piece_attributes(&mut geometry, &pieces).unwrap();
        let f32s = |name| {
            geometry
                .instances()
                .get(name)
                .unwrap()
                .as_f32(name)
                .unwrap()
                .to_vec()
        };
        assert_eq!(f32s(names::ALPHA), [0.25, 1.0, 1.0]);
        assert_eq!(f32s("mine"), [7.0, 0.0, 0.0]);
    }

    /// An out-of-range `source_index` selects the last source, the rule the
    /// rasterizer uses, so what is drawn and what is expanded agree.
    #[test]
    fn an_out_of_range_source_index_clamps_to_the_last_source() {
        let mut geometry = Geometry::new();
        geometry
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]),
            )
            .expect("two offsets");
        geometry
            .instances_mut()
            .insert(names::SOURCE_INDEX, AttributeArray::I32(vec![-3, 7]))
            .expect("two source indices");
        // Three sources of distinct sizes, and an over-range index a
        // wrap-around would send to the *middle* one (7 % 3 = 1) rather than
        // the last: clamping and wrapping are told apart here.
        let single = Geometry::from_points(vec![Vec2(1.0, 0.0)]);
        let double = Geometry::from_points(vec![Vec2(0.0, 1.0); 2]);
        let quintuple = Geometry::from_points(vec![Vec2(1.0, 1.0); 5]);
        geometry.set_instance_sources(vec![
            Arc::new(single),
            Arc::new(double),
            Arc::new(quintuple),
        ]);

        let expanded = expand_instances(&geometry).expect("clamped indices expand");
        // A negative index clamps to the first source (one point), an index
        // past the end to the last (five points).
        assert_eq!(expanded.point_count(), 1 + 5);
    }

    /// Instances of instances compose their placements, so a nested source
    /// lands where the rasterizer's own nested walk would draw it.
    #[test]
    fn nested_instances_compose_their_placements() {
        let leaf = Geometry::from_points(vec![Vec2(1.0, 0.0)]);
        let mut inner = Geometry::new();
        inner
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(4.0, 0.0)]))
            .expect("one offset");
        inner
            .instances_mut()
            .insert(names::SCALE, AttributeArray::Vec2(vec![Vec2(3.0, 3.0)]))
            .expect("one scale");
        inner.set_instance_source(Some(Arc::new(leaf)));

        let mut outer = Geometry::new();
        outer
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 100.0)]))
            .expect("one offset");
        outer
            .instances_mut()
            .insert(
                names::ROT,
                AttributeArray::F32(vec![std::f32::consts::FRAC_PI_2]),
            )
            .expect("one turn");
        outer.set_instance_source(Some(Arc::new(inner)));

        let expanded = expand_instances(&outer).expect("nesting expands");
        assert_eq!(expanded.point_count(), 1);
        // Inner puts the leaf point at 4 + 3 = 7 on x; the outer quarter
        // turn takes (7, 0) to (0, 7) and then moves it to (0, 107).
        let placed = vec2_column(&expanded, names::P)[0];
        assert!(
            placed.0.abs() < 1e-3 && (placed.1 - 107.0).abs() < 1e-3,
            "the two placements did not compose: {placed:?}"
        );
    }

    /// Nesting past the guard drops the instances rather than recursing for
    /// ever, and the answer is still flat.
    #[test]
    fn nesting_past_the_depth_guard_drops_the_instances() {
        let mut level = Geometry::from_points(vec![Vec2(1.0, 0.0)]);
        for _ in 0..=MAX_INSTANCE_DEPTH {
            let mut next = Geometry::new();
            next.instances_mut()
                .insert(names::P, AttributeArray::Vec2(vec![Vec2(1.0, 0.0)]))
                .expect("one offset");
            next.set_instance_source(Some(Arc::new(level)));
            level = next;
        }
        let expanded = expand_instances(&level).expect("a deep nesting still answers");
        assert_eq!(expanded.instance_count(), 0, "the answer has to be flat");
        assert!(expanded.sources().is_empty());
        assert_eq!(
            expanded.point_count(),
            0,
            "the level past the guard contributes nothing"
        );
    }

    // ----- instance pieces (instance-pieces-plan unit 1) -------------------

    fn piece_geometry(piece: &InstancePiece) -> &Geometry {
        piece
            .source
            .geometry()
            .expect("this piece stamps a geometry")
    }

    /// One piece per **instance**, not per source. The three characters of
    /// `laid_out_text` share one deduplicated outline, so a split that
    /// counted sources would answer one piece and lose two characters.
    #[test]
    fn a_split_answers_one_piece_per_instance_not_per_source() {
        let text = laid_out_text();
        assert_eq!(text.sources().len(), 1, "the outline is shared");
        let pieces = instance_pieces(&text).expect("a layout splits");
        assert_eq!(pieces.len(), 3);
    }

    /// The order is the instance domain's, which for text is character
    /// order — what makes `index % piece_count` walk a string from the
    /// front.
    #[test]
    fn pieces_keep_the_instance_order() {
        let pieces = instance_pieces(&laid_out_text()).expect("a layout splits");
        let char_indices: Vec<i32> = pieces
            .iter()
            .map(|piece| i32_column(&piece.attributes, names::CHAR_INDEX)[0])
            .collect();
        assert_eq!(char_indices, [0, 1, 2]);
    }

    /// A piece carries the row it came from, minus the placement: that row
    /// is what lets a stagger still read `char_progress` after the piece has
    /// been dealt somewhere else (REQ-MOGRAPH-004).
    #[test]
    fn a_piece_carries_its_instance_row_without_the_placement() {
        let pieces = instance_pieces(&laid_out_text()).expect("a layout splits");
        let row = &pieces[1].attributes;
        assert_eq!(row.element_count(), 1, "one row, broadcast later");
        assert_eq!(i32_column(row, names::CHAR_INDEX), [1]);
        assert_eq!(i32_column(row, names::WORD_INDEX), [3]);
        assert_eq!(
            row.get(names::CHAR_PROGRESS)
                .expect("progress descends")
                .as_f32(names::CHAR_PROGRESS)
                .expect("an F32 column"),
            [0.5]
        );
        for placement in [names::P, names::ROT, names::SCALE, names::SOURCE_INDEX] {
            assert!(
                row.get(placement).is_none(),
                "{placement} is a placement, and the placement is the caller's now"
            );
        }
    }

    /// The turn and the scale are baked into the outline; the layout
    /// position is not, because replacing it is the point of the split.
    #[test]
    fn a_piece_bakes_rot_and_scale_but_drops_the_position() {
        let pieces = instance_pieces(&laid_out_text()).expect("a layout splits");
        // Instance 0: no turn, scale 2, at (40, 90). The glyph's first point
        // is (2, 0), so a baked piece has it at (4, 0) and a piece that kept
        // the layout would have it at (44, 90).
        let first = vec2_column(piece_geometry(&pieces[0]), names::P)[0];
        assert!(
            (first.0 - 4.0).abs() < 1e-4 && first.1.abs() < 1e-4,
            "scale baked, position dropped: {first:?}"
        );
        // Instance 1: a quarter turn and scale 3 take (2, 0) to (0, 6).
        let second = vec2_column(piece_geometry(&pieces[1]), names::P)[0];
        assert!(
            second.0.abs() < 1e-3 && (second.1 - 6.0).abs() < 1e-3,
            "the turn has to be baked too: {second:?}"
        );
    }

    /// `anchor` is a position, so it moves with the points it describes.
    /// `scatter.*` recentres a piece on its anchor, and a stale one would
    /// pull every turned piece off the point it was dealt to.
    #[test]
    fn a_piece_carries_its_anchor_through_the_placement() {
        let mut source = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(4.0, 0.0)]);
        source
            .detail_mut()
            .insert(names::ANCHOR, AttributeArray::Vec2(vec![Vec2(2.0, 0.0)]))
            .expect("one anchor");

        let mut geometry = Geometry::new();
        geometry
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(90.0, 90.0)]))
            .expect("one offset");
        geometry
            .instances_mut()
            .insert(names::SCALE, AttributeArray::Vec2(vec![Vec2(3.0, 3.0)]))
            .expect("one scale");
        geometry.set_instance_source(Some(Arc::new(source)));

        let pieces = instance_pieces(&geometry).expect("the geometry splits");
        let piece = piece_geometry(&pieces[0]);
        // The points tripled, so the anchor has to triple with them.
        assert_eq!(vec2_column(piece, names::P)[1], Vec2(12.0, 0.0));
        let anchor = piece
            .detail()
            .get(names::ANCHOR)
            .expect("the anchor rides along")
            .as_vec2(names::ANCHOR)
            .expect("a Vec2 column")[0];
        assert!(
            (anchor.0 - 6.0).abs() < 1e-4 && anchor.1.abs() < 1e-4,
            "the anchor stayed behind the points it describes: {anchor:?}"
        );
    }

    /// An image instance becomes a piece where an expansion drops it: a
    /// split only chooses what is dealt where, and the rasterizer stamps a
    /// picture perfectly well.
    #[test]
    fn an_image_instance_becomes_a_piece() {
        let mut geometry = Geometry::new();
        geometry
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]),
            )
            .expect("two offsets");
        geometry
            .instances_mut()
            .insert(names::SOURCE_INDEX, AttributeArray::I32(vec![0, 1]))
            .expect("two source indices");
        geometry.set_sources(vec![
            InstanceSource::Geometry(Arc::new(glyph_source())),
            image_source(4, 4),
        ]);

        let pieces = instance_pieces(&geometry).expect("a mixed geometry splits");
        assert_eq!(pieces.len(), 2);
        assert!(pieces[0].source.geometry().is_some());
        assert!(
            pieces[1].source.image().is_some(),
            "the picture is a piece, not a dropped instance"
        );
    }

    /// The inner placement applies first and the outer is composed over it,
    /// which is the order `expand_instances` already draws. Pinned with a
    /// turn *and* a scale, because an order mistake is invisible when either
    /// is the identity.
    #[test]
    fn a_nested_piece_applies_the_inner_placement_first() {
        let leaf = Geometry::from_points(vec![Vec2(1.0, 0.0)]);
        let mut inner = Geometry::new();
        inner
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(4.0, 0.0)]))
            .expect("one offset");
        inner
            .instances_mut()
            .insert(names::SCALE, AttributeArray::Vec2(vec![Vec2(3.0, 3.0)]))
            .expect("one scale");
        inner.set_instance_source(Some(Arc::new(leaf)));

        let mut outer = Geometry::new();
        outer
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 100.0)]))
            .expect("one offset");
        outer
            .instances_mut()
            .insert(
                names::ROT,
                AttributeArray::F32(vec![std::f32::consts::FRAC_PI_2]),
            )
            .expect("one turn");
        outer.set_instance_source(Some(Arc::new(inner)));

        let pieces = instance_pieces(&outer).expect("a nesting splits");
        assert_eq!(pieces.len(), 1);
        let piece = piece_geometry(&pieces[0]);
        // The piece keeps its own nesting; expanding it has to land where
        // the outer instance would have drawn it, minus the layout position
        // the split drops: (0, 107) becomes (0, 7).
        let expanded = expand_instances(piece).expect("the piece expands");
        let placed = vec2_column(&expanded, names::P)[0];
        assert!(
            placed.0.abs() < 1e-3 && (placed.1 - 7.0).abs() < 1e-3,
            "the outer turn has to compose over the inner scale: {placed:?}"
        );
    }

    /// A leaf of four points under two levels of placement, both turned,
    /// non-uniformly scaled and sheared: the shape for which the old
    /// scale-rotate-translate composition was wrong. The outer instance is at
    /// the origin so a split (which drops the layout offset) is comparable.
    fn sheared_nesting(outer_offset: Vec2) -> Geometry {
        let column = |name: &str, values: AttributeArray| (name.to_owned(), values);
        let level = |source: Geometry, columns: Vec<(String, AttributeArray)>| {
            let mut geometry = Geometry::new();
            for (name, values) in columns {
                geometry
                    .instances_mut()
                    .insert(name.as_str(), values)
                    .expect("one row");
            }
            geometry.set_instance_source(Some(Arc::new(source)));
            geometry
        };
        let leaf = Geometry::from_points(vec![
            Vec2(1.0, 0.0),
            Vec2(0.0, 2.0),
            Vec2(-3.0, 1.0),
            Vec2(2.0, -2.5),
        ]);
        let inner = level(
            leaf,
            vec![
                column(names::P, AttributeArray::Vec2(vec![Vec2(1.0, 2.0)])),
                column(names::ROT, AttributeArray::F32(vec![FRAC_PI_2])),
                column(names::SCALE, AttributeArray::Vec2(vec![Vec2(1.5, 0.5)])),
                column(names::SHEAR, AttributeArray::F32(vec![-0.3])),
            ],
        );
        level(
            inner,
            vec![
                column(names::P, AttributeArray::Vec2(vec![outer_offset])),
                column(names::ROT, AttributeArray::F32(vec![0.4])),
                column(names::SCALE, AttributeArray::Vec2(vec![Vec2(2.0, 1.0)])),
                column(names::SHEAR, AttributeArray::F32(vec![0.25])),
            ],
        )
    }

    /// Drawing and flattening are one picture: the bounds of a sheared nesting
    /// contain every point `expand_instances` produces, and are about as wide.
    #[test]
    fn drawn_bounds_contain_the_expanded_points_of_a_sheared_nesting() {
        let nesting = sheared_nesting(Vec2(5.0, -3.0));
        let bounds = drawn_bounds(&nesting).expect("a nesting of points has an extent");
        let expanded = expand_instances(&nesting).expect("the nesting expands");
        let points = vec2_column(&expanded, names::P);
        assert_eq!(points.len(), 4);
        let eps = 1e-3;
        for p in &points {
            assert!(
                p.0 >= bounds.x - eps
                    && p.0 <= bounds.x + bounds.width + eps
                    && p.1 >= bounds.y - eps
                    && p.1 <= bounds.y + bounds.height + eps,
                "{p:?} is outside {bounds:?}"
            );
        }
        let (min_x, max_x) = points.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p.0), hi.max(p.0))
        });
        // Points carry a little reach of their own, so the bounds may exceed
        // them; a bound off by a placement mistake would be far more.
        assert!(
            bounds.width < (max_x - min_x) + 1.0,
            "the bounds are about as wide as the points: {bounds:?} vs {min_x}..{max_x}"
        );
    }

    /// A piece of a sheared nesting lands the same points the original
    /// placement does.
    #[test]
    fn a_piece_of_a_sheared_nesting_has_the_original_points() {
        let nesting = sheared_nesting(Vec2(0.0, 0.0));
        let want = vec2_column(&expand_instances(&nesting).expect("expands"), names::P);
        let pieces = instance_pieces(&nesting).expect("a nesting splits");
        assert_eq!(pieces.len(), 1);
        let got = vec2_column(
            &expand_instances(piece_geometry(&pieces[0])).expect("the piece expands"),
            names::P,
        );
        assert_eq!(want.len(), 4, "the fixture has four leaf points");
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(&want) {
            assert!(
                (g.0 - w.0).abs() < 1e-3 && (g.1 - w.1).abs() < 1e-3,
                "{g:?} != {w:?}"
            );
        }
    }

    /// A split writes a `shear` column only when something is sheared, so a
    /// geometry that never shears does not grow a column of zeros.
    #[test]
    fn a_split_writes_shear_only_when_there_is_some() {
        let pieces = instance_pieces(&sheared_nesting(Vec2(0.0, 0.0))).expect("splits");
        let piece = piece_geometry(&pieces[0]);
        assert!(piece.instances().get(names::SHEAR).is_some());

        let mut inner = Geometry::from_points(vec![Vec2(1.0, 0.0)]);
        inner
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(1.0, 0.0)]))
            .expect("one offset");
        inner.set_instance_source(Some(Arc::new(Geometry::from_points(vec![Vec2(1.0, 1.0)]))));
        let mut outer = Geometry::new();
        outer
            .instances_mut()
            .insert(names::SCALE, AttributeArray::Vec2(vec![Vec2(2.0, 3.0)]))
            .expect("one scale");
        outer
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))
            .expect("one offset");
        outer.set_instance_source(Some(Arc::new(inner)));
        let pieces = instance_pieces(&outer).expect("splits");
        assert!(
            piece_geometry(&pieces[0])
                .instances()
                .get(names::SHEAR)
                .is_none(),
            "nothing sheared, so no column"
        );
    }

    /// A split adds no level, so what is too deep to draw is exactly what
    /// was too deep to draw before it — [`MAX_INSTANCE_DEPTH`] means the
    /// same thing on both sides.
    #[test]
    fn a_split_leaves_the_nesting_depth_alone() {
        let mut level = Geometry::from_points(vec![Vec2(1.0, 0.0)]);
        for _ in 0..=MAX_INSTANCE_DEPTH {
            let mut next = Geometry::new();
            next.instances_mut()
                .insert(names::P, AttributeArray::Vec2(vec![Vec2(1.0, 0.0)]))
                .expect("one offset");
            next.set_instance_source(Some(Arc::new(level)));
            level = next;
        }
        let pieces = instance_pieces(&level).expect("a deep nesting splits");
        assert_eq!(pieces.len(), 1);
        // The piece is one level shallower than the geometry it came from,
        // so it reaches exactly as far as an expansion of the original did.
        let from_piece = expand_instances(piece_geometry(&pieces[0])).expect("the piece expands");
        assert_eq!(from_piece.instance_count(), 0, "the answer stays flat");
        assert_eq!(
            from_piece.point_count(),
            1,
            "one level shallower is one level inside the guard"
        );
    }

    /// No instances, so nothing to divide: the geometry is its own piece,
    /// the way an expansion of a flat geometry is a pass-through.
    #[test]
    fn a_geometry_without_instances_is_one_piece() {
        let flat = glyph_source();
        let pieces = instance_pieces(&flat).expect("a flat geometry splits");
        assert_eq!(pieces.len(), 1);
        assert_eq!(piece_geometry(&pieces[0]).point_count(), flat.point_count());
        assert!(pieces[0].attributes.iter().next().is_none());
    }

    /// An instance domain with no `P` places nothing, and a split answers
    /// what an expansion answers rather than inventing an origin.
    #[test]
    fn instances_that_cannot_be_placed_answer_like_an_expansion() {
        let mut geometry = glyph_source();
        geometry
            .instances_mut()
            .insert(names::CHAR_INDEX, AttributeArray::I32(vec![0, 1]))
            .expect("two rows, no position");
        geometry.set_instance_source(Some(Arc::new(glyph_source())));

        let pieces = instance_pieces(&geometry).expect("an unplaceable domain splits");
        assert_eq!(pieces.len(), 1);
        let piece = piece_geometry(&pieces[0]);
        assert_eq!(piece.instance_count(), 0);
        assert!(piece.sources().is_empty());
        assert_eq!(piece.point_count(), geometry.point_count());
    }

    /// A geometry's own points and primitives keep their place at the front,
    /// unplaced, so a merge of text and a shape keeps drawing both.
    #[test]
    fn a_geometrys_own_elements_come_before_its_instances() {
        let mut geometry = Geometry::from_points(vec![Vec2(-5.0, -5.0)]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..1,
            closed: false,
        });
        geometry
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(60.0, 60.0)]))
            .expect("one offset");
        geometry.set_instance_source(Some(Arc::new(Geometry::from_points(vec![Vec2(1.0, 2.0)]))));

        let expanded = expand_instances(&geometry).expect("a host with its own points expands");
        let positions = vec2_column(&expanded, names::P);
        assert_eq!(
            positions,
            vec![Vec2(-5.0, -5.0), Vec2(61.0, 62.0)],
            "the host's own point stays where it was, in front"
        );
    }

    /// A mesh source expands with its index blob rebased, the same way
    /// `geometry.merge` moves one.
    #[test]
    fn a_mesh_source_expands_with_its_indices() {
        // Two windings, so an index range that was not rebased reads the
        // other source's triangle instead of its own.
        let quad = |triangles: &[u32]| {
            let mut mesh = Geometry::from_points(vec![
                Vec2(0.0, 0.0),
                Vec2(4.0, 0.0),
                Vec2(0.0, 4.0),
                Vec2(4.0, 4.0),
            ]);
            mesh.push_mesh(0..4, triangles);
            Arc::new(mesh)
        };
        let mut geometry = Geometry::new();
        geometry
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(10.0, 0.0), Vec2(0.0, 10.0)]),
            )
            .expect("two offsets");
        geometry
            .instances_mut()
            .insert(names::SOURCE_INDEX, AttributeArray::I32(vec![0, 1]))
            .expect("two source indices");
        geometry.set_instance_sources(vec![quad(&[0, 1, 2]), quad(&[3, 2, 1])]);

        let expanded = expand_instances(&geometry).expect("a mesh source expands");
        assert_eq!(expanded.point_count(), 8);
        assert_eq!(expanded.primitive_count(), 2);
        assert_eq!(expanded.indices(), &[0, 1, 2, 3, 2, 1]);
        let triangles: Vec<&[u32]> = expanded
            .primitives()
            .iter()
            .map(|primitive| {
                expanded
                    .mesh_indices(primitive)
                    .expect("both primitives are meshes")
            })
            .collect();
        assert_eq!(
            triangles,
            vec![&[0u32, 1, 2][..], &[3u32, 2, 1][..]],
            "each mesh has to read its own triangle, not the first blob's"
        );
        expanded
            .validate()
            .expect("the expansion is valid geometry");
    }

    /// 3D instance positions are an error, not a silent projection onto xy —
    /// the rule the position dimension table sets.
    #[test]
    fn three_dimensional_instance_positions_are_rejected() {
        let mut geometry = Geometry::new();
        geometry
            .instances_mut()
            .insert(names::P, AttributeArray::Vec3(vec![Vec3(1.0, 2.0, 3.0)]))
            .expect("one 3D offset");
        geometry.set_instance_source(Some(Arc::new(Geometry::from_points(vec![Vec2(0.0, 0.0)]))));

        let error = expand_instances(&geometry).expect_err("a 3D placement has no 2D answer");
        assert!(
            format!("{error}").contains("2D positions"),
            "the error has to name the dimension: {error}"
        );
    }

    /// An instance domain with no `P` places nothing, exactly as the
    /// rasterizer draws nothing for it, and the answer is still flat.
    #[test]
    fn an_instance_domain_without_positions_expands_to_the_host_alone() {
        let mut geometry = Geometry::from_points(vec![Vec2(7.0, 7.0)]);
        geometry
            .instances_mut()
            .insert(names::INDEX, AttributeArray::I32(vec![0, 1]))
            .expect("two indices");
        geometry.set_instance_source(Some(Arc::new(glyph_source())));

        let expanded = expand_instances(&geometry).expect("a placeless instance domain expands");
        assert_eq!(expanded.point_count(), 1);
        assert_eq!(expanded.instance_count(), 0);
        assert!(expanded.sources().is_empty());
    }

    // ----- blast ---------------------------------------------------------------

    /// One column of every attribute type, each value a pure function of the
    /// **original** row number, so what a row should hold after a deletion is
    /// computed from the survivors' row numbers rather than by running the
    /// code under test again.
    fn typed_rows(rows: &[usize]) -> Vec<(&'static str, AttributeArray)> {
        let f = |row: usize| row as f32 * 0.5 + 1.0;
        vec![
            (
                "t_f32",
                AttributeArray::F32(rows.iter().map(|r| f(*r)).collect()),
            ),
            (
                "t_vec2",
                AttributeArray::Vec2(rows.iter().map(|r| Vec2(f(*r), -f(*r))).collect()),
            ),
            (
                "t_vec3",
                AttributeArray::Vec3(rows.iter().map(|r| Vec3(f(*r), 2.0, 3.0)).collect()),
            ),
            (
                "t_vec4",
                AttributeArray::Vec4(rows.iter().map(|r| Vec4(f(*r), 2.0, 3.0, 4.0)).collect()),
            ),
            (
                "t_color",
                AttributeArray::Color(
                    rows.iter()
                        .map(|r| Color::new(f(*r), 0.1, 0.2, 0.3))
                        .collect(),
                ),
            ),
            (
                "t_i32",
                AttributeArray::I32(rows.iter().map(|r| 1000 + *r as i32).collect()),
            ),
            (
                "t_bool",
                AttributeArray::Bool(rows.iter().map(|r| r % 3 == 0).collect()),
            ),
            (
                "t_str",
                AttributeArray::Str(rows.iter().map(|r| format!("row{r}")).collect()),
            ),
            (
                names::ID,
                AttributeArray::I32(rows.iter().map(|r| 500 + *r as i32).collect()),
            ),
        ]
    }

    fn insert_all(set: &mut AttributeSet, columns: Vec<(&'static str, AttributeArray)>) {
        for (name, column) in columns {
            set.insert(name, column).unwrap();
        }
    }

    /// 8 points in four 2-point paths, 4 primitives, 5 instances stamping
    /// three distinct sources, a detail column, and every attribute type on
    /// each of the three element domains.
    fn blast_subject() -> Geometry {
        let mut geometry =
            Geometry::from_points((0..8).map(|i| Vec2(i as f32, i as f32 * 2.0)).collect());
        insert_all(
            geometry.points_mut(),
            typed_rows(&(0..8).collect::<Vec<_>>()),
        );
        for i in 0..4 {
            geometry.push_primitive(Primitive::Path {
                verts: 2 * i..2 * i + 2,
                closed: i % 2 == 1,
            });
        }
        insert_all(
            geometry.primitive_attrs_mut(),
            typed_rows(&(0..4).collect::<Vec<_>>()),
        );
        geometry
            .primitive_attrs_mut()
            .insert(names::INDEX, AttributeArray::I32((0..4).collect()))
            .unwrap();
        geometry
            .instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2((0..5).map(|i| Vec2(i as f32, 0.0)).collect()),
            )
            .unwrap();
        geometry
            .instances_mut()
            .insert(names::INDEX, AttributeArray::I32((0..5).collect()))
            .unwrap();
        geometry
            .instances_mut()
            .insert(
                names::SOURCE_INDEX,
                AttributeArray::I32(vec![0, 2, 2, 1, 0]),
            )
            .unwrap();
        insert_all(
            geometry.instances_mut(),
            typed_rows(&(0..5).collect::<Vec<_>>()),
        );
        geometry.set_instance_sources(
            (1..=3)
                .map(|n| Arc::new(Geometry::from_points(vec![Vec2(0.0, 0.0); n])))
                .collect(),
        );
        geometry
            .detail_mut()
            .insert("note", AttributeArray::Str(vec!["keep me".into()]))
            .unwrap();
        geometry
    }

    /// `subject` with a `g` Bool group on `domain` flagging `rows`.
    fn grouped(mut subject: Geometry, domain: Domain, rows: &[usize]) -> Geometry {
        let count = domain_count(&subject, domain);
        subject
            .attribute_set_mut(domain)
            .insert(
                "g",
                AttributeArray::Bool((0..count).map(|row| rows.contains(&row)).collect()),
            )
            .unwrap();
        subject
    }

    fn column<'a>(geometry: &'a Geometry, domain: Domain, name: &str) -> &'a AttributeArray {
        geometry.attribute_set(domain).get(name).unwrap_or_else(|| {
            panic!("{domain:?} lacks {name}");
        })
    }

    /// Every domain x every attribute type: the group's rows disappear from
    /// the blasted domain and every other row of every column survives with
    /// its value, which is what a column left behind by the deletion would
    /// break.
    #[test]
    fn blast_deletes_the_group_and_leaves_every_survivor_value_untouched() {
        for (domain, doomed, count) in [
            (Domain::Point, vec![1, 6], 8),
            (Domain::Primitive, vec![0, 3], 4),
            (Domain::Instance, vec![1, 4], 5),
        ] {
            let result = blast(
                &grouped(blast_subject(), domain, &doomed),
                domain,
                "g",
                false,
            )
            .unwrap();
            let survivors: Vec<usize> = (0..count).filter(|row| !doomed.contains(row)).collect();
            assert_eq!(domain_count(&result, domain), survivors.len(), "{domain:?}");
            for (name, expected) in typed_rows(&survivors) {
                assert_eq!(
                    column(&result, domain, name),
                    &expected,
                    "{domain:?} column {name}"
                );
            }
            // The group column itself is just another column: it is filtered
            // and so reads false on every survivor.
            assert_eq!(
                column(&result, domain, "g"),
                &AttributeArray::Bool(vec![false; survivors.len()])
            );
            assert_eq!(
                column(&result, Domain::Detail, "note"),
                &AttributeArray::Str(vec!["keep me".into()])
            );
            assert_eq!(result.validate(), Ok(()), "{domain:?}");
        }
    }

    #[test]
    fn blast_leaves_the_other_domains_alone() {
        let subject = blast_subject();
        let by_primitive = blast(
            &grouped(subject.clone(), Domain::Primitive, &[1]),
            Domain::Primitive,
            "g",
            false,
        )
        .unwrap();
        assert_eq!(by_primitive.point_count(), 8, "points stay");
        assert_eq!(by_primitive.instance_count(), 5);
        let by_instance = blast(
            &grouped(subject, Domain::Instance, &[0]),
            Domain::Instance,
            "g",
            false,
        )
        .unwrap();
        assert_eq!(by_instance.point_count(), 8);
        assert_eq!(by_instance.primitive_count(), 4);
    }

    #[test]
    fn invert_deletes_the_complement_of_the_group() {
        let result = blast(
            &grouped(blast_subject(), Domain::Point, &[0, 1, 2, 3]),
            Domain::Point,
            "g",
            true,
        )
        .unwrap();
        // The kept half is exactly the flagged half: points 0..4, both paths.
        assert_eq!(result.point_count(), 4);
        assert_eq!(
            column(&result, Domain::Point, "t_i32"),
            &AttributeArray::I32(vec![1000, 1001, 1002, 1003])
        );
        assert_eq!(result.primitive_count(), 2);
    }

    /// Deleting a point takes every primitive that referenced it, and the
    /// primitives after it are re-packed onto the shorter point list with
    /// their attribute rows alongside.
    #[test]
    fn deleting_a_point_removes_its_primitives_and_repacks_verts() {
        // Point 3 sits in path 1 (2..4); point 7 in path 3 (6..8).
        let result = blast(
            &grouped(blast_subject(), Domain::Point, &[3, 7]),
            Domain::Point,
            "g",
            false,
        )
        .unwrap();
        assert_eq!(result.point_count(), 6);
        assert_eq!(
            result.primitives(),
            &[
                Primitive::Path {
                    verts: 0..2,
                    closed: false
                },
                // Was 4..6; point 3 ahead of it is gone.
                Primitive::Path {
                    verts: 3..5,
                    closed: false
                },
            ],
            "paths 0 and 2 survive"
        );
        assert_eq!(
            column(&result, Domain::Primitive, "t_i32"),
            &AttributeArray::I32(vec![1000, 1002])
        );
        // The shifted path still spans the points it spanned before.
        let points = column(&result, Domain::Point, "t_i32")
            .as_i32("t_i32")
            .unwrap();
        assert_eq!(points, &[1000, 1001, 1002, 1004, 1005, 1006]);
        assert_eq!(&points[3..5], &[1004, 1005]);
    }

    #[test]
    fn deleting_every_element_leaves_a_valid_empty_geometry() {
        for domain in [Domain::Point, Domain::Primitive, Domain::Instance] {
            let count = domain_count(&blast_subject(), domain);
            let everything: Vec<usize> = (0..count).collect();
            let result = blast(
                &grouped(blast_subject(), domain, &everything),
                domain,
                "g",
                false,
            )
            .unwrap();
            assert_eq!(domain_count(&result, domain), 0, "{domain:?}");
            assert_eq!(result.validate(), Ok(()), "{domain:?}");
        }
        // Emptying the points takes every path with them.
        let no_points = blast(
            &grouped(blast_subject(), Domain::Point, &(0..8).collect::<Vec<_>>()),
            Domain::Point,
            "g",
            false,
        )
        .unwrap();
        assert_eq!(no_points.primitive_count(), 0);
        assert_eq!(no_points.primitive_attrs().element_count(), 0);
        // Inverting an empty selection deletes everything too.
        let inverted = blast(
            &grouped(blast_subject(), Domain::Instance, &[]),
            Domain::Instance,
            "g",
            true,
        )
        .unwrap();
        assert_eq!(inverted.instance_count(), 0);
        assert!(inverted.sources().is_empty());
    }

    #[test]
    fn index_is_repacked_and_id_survives_a_deletion() {
        for (domain, doomed) in [
            (Domain::Point, vec![0, 3]),
            (Domain::Primitive, vec![1]),
            (Domain::Instance, vec![0, 2]),
        ] {
            let subject = grouped(blast_subject(), domain, &doomed);
            let original_ids = column(&subject, domain, names::ID)
                .as_i32(names::ID)
                .unwrap()
                .to_vec();
            let result = blast(&subject, domain, "g", false).unwrap();
            let n = domain_count(&result, domain) as i32;
            if let Some(index) = result.attribute_set(domain).get(names::INDEX) {
                assert_eq!(
                    index.as_i32(names::INDEX).unwrap(),
                    (0..n).collect::<Vec<_>>(),
                    "{domain:?}"
                );
            } else {
                panic!("{domain:?} lost its index column");
            }
            let expected_ids: Vec<i32> = original_ids
                .iter()
                .enumerate()
                .filter(|(row, _)| !doomed.contains(row))
                .map(|(_, id)| *id)
                .collect();
            assert_eq!(
                column(&result, domain, names::ID)
                    .as_i32(names::ID)
                    .unwrap(),
                expected_ids,
                "{domain:?}"
            );
        }
    }

    /// Instances 1 and 2 stamp source 2, instance 3 source 1, instances 0
    /// and 4 source 0. Deleting both stampers of source 2 drops it and
    /// renumbers the rest; deleting nobody who matters drops nothing.
    #[test]
    fn deleting_instances_drops_the_sources_nobody_stamps_any_more() {
        let result = blast(
            &grouped(blast_subject(), Domain::Instance, &[1, 2]),
            Domain::Instance,
            "g",
            false,
        )
        .unwrap();
        assert_eq!(result.sources().len(), 2);
        // The remaining instances are old rows 0, 3, 4 -> sources 0, 1, 0.
        assert_eq!(
            column(&result, Domain::Instance, names::SOURCE_INDEX),
            &AttributeArray::I32(vec![0, 1, 0])
        );
        let point_counts: Vec<usize> = result
            .sources()
            .iter()
            .map(|source| source.geometry().unwrap().point_count())
            .collect();
        assert_eq!(point_counts, [1, 2], "sources 0 and 1 kept, in order");

        // Source 0's two stampers go; sources 1 and 2 stay and shift down.
        let result = blast(
            &grouped(blast_subject(), Domain::Instance, &[0, 4]),
            Domain::Instance,
            "g",
            false,
        )
        .unwrap();
        let point_counts: Vec<usize> = result
            .sources()
            .iter()
            .map(|source| source.geometry().unwrap().point_count())
            .collect();
        assert_eq!(point_counts, [2, 3]);
        assert_eq!(
            column(&result, Domain::Instance, names::SOURCE_INDEX),
            &AttributeArray::I32(vec![1, 1, 0])
        );

        let untouched = blast(
            &grouped(blast_subject(), Domain::Instance, &[4]),
            Domain::Instance,
            "g",
            false,
        )
        .unwrap();
        assert_eq!(untouched.sources().len(), 3);
    }

    /// A deletion is only as dangerous as its selection is precise, so an
    /// empty or unresolvable group deletes nothing rather than everything.
    #[test]
    fn an_empty_or_unresolvable_group_deletes_nothing() {
        let subject = grouped(blast_subject(), Domain::Point, &[0]);
        for group in ["", "nope", "t_f32"] {
            for invert in [false, true] {
                let result = blast(&subject, Domain::Point, group, invert).unwrap();
                assert_eq!(result.point_count(), 8, "{group:?} invert={invert}");
                assert_eq!(result.primitive_count(), 4);
            }
        }
    }

    #[test]
    fn blast_is_deterministic_and_detail_has_nothing_to_delete() {
        let subject = grouped(blast_subject(), Domain::Point, &[2, 5]);
        let first = blast(&subject, Domain::Point, "g", false).unwrap();
        let second = blast(&subject, Domain::Point, "g", false).unwrap();
        assert_eq!(first.primitives(), second.primitives());
        for domain in [Domain::Point, Domain::Primitive, Domain::Instance] {
            let (a, b) = (first.attribute_set(domain), second.attribute_set(domain));
            assert_eq!(a.describe().len(), b.describe().len());
            for (name, column) in a.iter() {
                assert_eq!(Some(column), b.get(name), "{domain:?} {name}");
            }
        }
        let detail = blast(&subject, Domain::Detail, "g", false).unwrap();
        assert_eq!(detail.point_count(), 8);
    }

    #[test]
    fn blasting_points_keeps_surviving_meshes_valid() {
        let mut geometry = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(1.0, 0.0),
            Vec2(0.0, 1.0),
            Vec2(5.0, 5.0),
            Vec2(6.0, 5.0),
            Vec2(5.0, 6.0),
        ]);
        geometry.push_mesh(0..3, &[0, 1, 2]);
        geometry.push_mesh(3..6, &[0, 1, 2]);
        let result = blast(
            &grouped(geometry, Domain::Point, &[0]),
            Domain::Point,
            "g",
            false,
        )
        .unwrap();
        assert_eq!(result.primitive_count(), 1, "the first mesh lost a vertex");
        assert_eq!(result.primitives()[0].verts(), &(2..5));
        assert_eq!(result.validate(), Ok(()));
    }

    // ----- resample ------------------------------------------------------------

    fn path_geometry(points: Vec<Vec2>, closed: bool) -> Geometry {
        let mut geometry = Geometry::from_points(points);
        let count = geometry.point_count();
        geometry.push_primitive(Primitive::Path {
            verts: 0..count,
            closed,
        });
        geometry
    }

    fn xs(geometry: &Geometry) -> Vec<Vec2> {
        geometry
            .points()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap()
            .to_vec()
    }

    fn assert_points(actual: &[Vec2], expected: &[(f32, f32)]) {
        assert_eq!(actual.len(), expected.len(), "{actual:?}");
        for (point, (x, y)) in actual.iter().zip(expected) {
            assert!(
                (point.0 - x).abs() < 1e-4 && (point.1 - y).abs() < 1e-4,
                "{actual:?} vs {expected:?}"
            );
        }
    }

    /// Uneven source vertices come out evenly spaced, by length and by count.
    #[test]
    fn resample_spaces_a_straight_path_evenly() {
        let line = path_geometry(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0), Vec2(10.0, 0.0)], false);
        let by_length = resample(&line, 2.5, 99, false).unwrap();
        assert_points(
            &xs(&by_length),
            &[(0.0, 0.0), (2.5, 0.0), (5.0, 0.0), (7.5, 0.0), (10.0, 0.0)],
        );
        // `length` wins over `segments`; with no length the count decides.
        let by_count = resample(&line, 0.0, 5, false).unwrap();
        assert_points(
            &xs(&by_count),
            &[
                (0.0, 0.0),
                (2.0, 0.0),
                (4.0, 0.0),
                (6.0, 0.0),
                (8.0, 0.0),
                (10.0, 0.0),
            ],
        );
        // A length that does not divide the path snaps to the nearest even one.
        let snapped = resample(&line, 3.0, 0, false).unwrap();
        assert_eq!(snapped.point_count(), 4, "10 / 3 rounds to 3 segments");
        assert_eq!(
            by_length.primitives(),
            &[Primitive::Path {
                verts: 0..5,
                closed: false
            }]
        );
    }

    /// A closed path goes all the way round, ends on no duplicate of its
    /// start, and stays closed.
    #[test]
    fn resample_walks_a_closed_path_once_around() {
        let square = path_geometry(
            vec![
                Vec2(0.0, 0.0),
                Vec2(4.0, 0.0),
                Vec2(4.0, 4.0),
                Vec2(0.0, 4.0),
            ],
            true,
        );
        let result = resample(&square, 0.0, 8, false).unwrap();
        assert_points(
            &xs(&result),
            &[
                (0.0, 0.0),
                (2.0, 0.0),
                (4.0, 0.0),
                (4.0, 2.0),
                (4.0, 4.0),
                (2.0, 4.0),
                (0.0, 4.0),
                (0.0, 2.0),
            ],
        );
        assert_eq!(
            result.primitives(),
            &[Primitive::Path {
                verts: 0..8,
                closed: true
            }]
        );
    }

    #[test]
    fn keep_corners_keeps_the_turns_and_divides_each_stretch() {
        let ell = path_geometry(
            vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0), Vec2(10.0, 5.0)],
            false,
        );
        let kept = xs(&resample(&ell, 4.0, 0, true).unwrap());
        // 10 -> 3 segments, 5 -> 1 segment, and the corner is exactly there.
        assert_eq!(kept.len(), 5);
        assert!(kept.contains(&Vec2(10.0, 0.0)), "{kept:?}");
        assert_eq!(kept.last(), Some(&Vec2(10.0, 5.0)));
        assert_points(
            &kept,
            &[
                (0.0, 0.0),
                (10.0 / 3.0, 0.0),
                (20.0 / 3.0, 0.0),
                (10.0, 0.0),
                (10.0, 5.0),
            ],
        );
        // Without it the corner is stepped over: 15 / 4 = 3.75 spacing.
        let loose = xs(&resample(&ell, 4.0, 0, false).unwrap());
        assert!(!loose.contains(&Vec2(10.0, 0.0)), "{loose:?}");
        assert_points(
            &loose,
            &[
                (0.0, 0.0),
                (3.75, 0.0),
                (7.5, 0.0),
                (10.0, 1.25),
                (10.0, 5.0),
            ],
        );
        // A closed shape keeps every corner of a square, 4 per side.
        let square = path_geometry(
            vec![
                Vec2(0.0, 0.0),
                Vec2(4.0, 0.0),
                Vec2(4.0, 4.0),
                Vec2(0.0, 4.0),
            ],
            true,
        );
        let corners = xs(&resample(&square, 1.0, 0, true).unwrap());
        assert_eq!(corners.len(), 16);
        for corner in [(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)] {
            assert!(corners.contains(&Vec2(corner.0, corner.1)), "{corner:?}");
        }
    }

    #[test]
    fn resample_interpolates_what_can_be_and_picks_the_nearer_point_for_the_rest() {
        let mut line = path_geometry(vec![Vec2(0.0, 0.0), Vec2(8.0, 0.0)], false);
        line.points_mut()
            .insert("w", AttributeArray::F32(vec![0.0, 10.0]))
            .unwrap();
        line.points_mut()
            .insert(
                "c",
                AttributeArray::Color(vec![
                    Color::new(0.0, 0.0, 0.0, 1.0),
                    Color::new(1.0, 0.5, 0.0, 1.0),
                ]),
            )
            .unwrap();
        line.points_mut()
            .insert(names::ID, AttributeArray::I32(vec![7, 9]))
            .unwrap();
        line.points_mut()
            .insert("name", AttributeArray::Str(vec!["a".into(), "b".into()]))
            .unwrap();
        line.points_mut()
            .insert(names::IN_TAN, AttributeArray::Vec2(vec![Vec2(1.0, 1.0); 2]))
            .unwrap();
        line.primitive_attrs_mut()
            .insert("tag", AttributeArray::I32(vec![42]))
            .unwrap();
        let result = resample(&line, 0.0, 4, false).unwrap();
        let point = |name: &str| result.points().get(name).unwrap().clone();
        assert_eq!(point("w").as_f32("w").unwrap(), [0.0, 2.5, 5.0, 7.5, 10.0]);
        let AttributeArray::Color(colors) = point("c").as_ref().clone() else {
            panic!("c is a colour column");
        };
        assert!((colors[2].r - 0.5).abs() < 1e-6 && (colors[2].g - 0.25).abs() < 1e-6);
        assert_eq!(point(names::ID).as_i32(names::ID).unwrap(), [7, 7, 9, 9, 9]);
        assert_eq!(
            point(names::INDEX).as_i32(names::INDEX).unwrap(),
            [0, 1, 2, 3, 4]
        );
        let AttributeArray::Str(names) = point("name").as_ref().clone() else {
            panic!("name is a string column");
        };
        assert_eq!(names, ["a", "a", "b", "b", "b"]);
        assert!(result.points().get(names::IN_TAN).is_none());
        assert_eq!(
            result
                .primitive_attrs()
                .get("tag")
                .unwrap()
                .as_i32("tag")
                .unwrap(),
            [42],
            "primitive rows follow their primitive"
        );
    }

    #[test]
    fn resample_handles_degenerate_paths_without_erroring() {
        // One vertex, a path whose points coincide, and an open two-point
        // path of zero length all pass through.
        for points in [
            vec![Vec2(3.0, 4.0)],
            vec![Vec2(1.0, 1.0), Vec2(1.0, 1.0)],
            vec![Vec2(2.0, 2.0), Vec2(2.0, 2.0), Vec2(2.0, 2.0)],
        ] {
            for closed in [false, true] {
                for keep in [false, true] {
                    let geometry = path_geometry(points.clone(), closed);
                    let result = resample(&geometry, 0.5, 4, keep).unwrap();
                    assert_eq!(xs(&result), points, "{points:?} closed={closed}");
                    assert_eq!(result.validate(), Ok(()));
                }
            }
        }
        // No path primitive at all: nothing to do, nothing wrong.
        let cloud = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0)]);
        assert_eq!(resample(&cloud, 0.5, 4, false).unwrap().point_count(), 2);
        assert_eq!(
            resample(&Geometry::new(), 0.5, 4, false)
                .unwrap()
                .point_count(),
            0
        );
        // A tiny length is capped rather than allocating without bound.
        let long = path_geometry(vec![Vec2(0.0, 0.0), Vec2(1.0e6, 0.0)], false);
        assert!(resample(&long, 1.0e-6, 0, false).unwrap().point_count() <= MAX_PATH_SEGMENTS + 1);
    }

    /// The budget is per path, not per corner-to-corner span.
    #[test]
    fn many_corners_and_a_tiny_length_stay_within_the_path_budget() {
        let zigzag: Vec<Vec2> = (0..200)
            .map(|i| Vec2(i as f32 * 1000.0, if i % 2 == 0 { 0.0 } else { 1000.0 }))
            .collect();
        let geometry = path_geometry(zigzag, false);
        let result = resample(&geometry, 1.0e-4, 0, true).unwrap();
        assert!(result.point_count() <= MAX_PATH_SEGMENTS + 200);
        assert!(result.point_count() > 200, "still resampled, just coarser");
        assert_eq!(result.validate(), Ok(()));
    }

    #[test]
    fn resample_rebuilds_each_path_in_turn() {
        let mut geometry = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(4.0, 0.0),
            Vec2(0.0, 10.0),
            Vec2(0.0, 16.0),
        ]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        geometry.push_primitive(Primitive::Path {
            verts: 2..4,
            closed: false,
        });
        let result = resample(&geometry, 2.0, 0, false).unwrap();
        // 4 long -> 2 segments, 6 long -> 3 segments.
        assert_eq!(
            result.primitives(),
            &[
                Primitive::Path {
                    verts: 0..3,
                    closed: false
                },
                Primitive::Path {
                    verts: 3..7,
                    closed: false
                },
            ]
        );
        assert_points(
            &xs(&result)[3..],
            &[(0.0, 10.0), (0.0, 12.0), (0.0, 14.0), (0.0, 16.0)],
        );
    }

    #[test]
    fn resample_refuses_meshes() {
        let mut geometry =
            Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0), Vec2(0.0, 1.0)]);
        geometry.push_mesh(0..3, &[0, 1, 2]);
        assert!(resample(&geometry, 1.0, 4, false).is_err());
    }

    // ----- measure -------------------------------------------------------------

    fn f32_column(geometry: &Geometry, domain: Domain, name: &str) -> Vec<f32> {
        geometry
            .attribute_set(domain)
            .get(name)
            .unwrap_or_else(|| panic!("{domain:?} lacks {name}"))
            .as_f32(name)
            .unwrap()
            .to_vec()
    }

    fn close(actual: &[f32], expected: &[f32], tolerance: f32) {
        assert_eq!(actual.len(), expected.len(), "{actual:?}");
        for (a, e) in actual.iter().zip(expected) {
            assert!((a - e).abs() <= tolerance, "{actual:?} vs {expected:?}");
        }
    }

    fn rectangle(closed: bool) -> Geometry {
        path_geometry(
            vec![
                Vec2(0.0, 0.0),
                Vec2(4.0, 0.0),
                Vec2(4.0, 3.0),
                Vec2(0.0, 3.0),
            ],
            closed,
        )
    }

    /// A counter-clockwise regular polygon close enough to a circle that the
    /// analytic values hold to the tolerance used below.
    fn circle(radius: f32, sides: usize) -> Geometry {
        path_geometry(
            (0..sides)
                .map(|i| {
                    let angle = i as f32 / sides as f32 * std::f32::consts::TAU;
                    Vec2(radius * angle.cos(), radius * angle.sin())
                })
                .collect(),
            true,
        )
    }

    #[test]
    fn measure_matches_the_analytic_values_of_a_rectangle() {
        let rect = rectangle(true);
        let perimeter = measure(&rect, Measure::Perimeter, "").unwrap();
        close(
            &f32_column(&perimeter, Domain::Primitive, "perimeter"),
            &[14.0],
            1e-5,
        );
        let area = measure(&rect, Measure::Area, "").unwrap();
        close(&f32_column(&area, Domain::Primitive, "area"), &[12.0], 1e-5);
        let lengths = measure(&rect, Measure::SegmentLength, "").unwrap();
        close(
            &f32_column(&lengths, Domain::Point, "segment_length"),
            &[4.0, 3.0, 4.0, 3.0],
            1e-5,
        );
        let sized = measure(&rect, Measure::Size, "").unwrap();
        assert_eq!(
            sized
                .primitive_attrs()
                .get("size")
                .unwrap()
                .as_vec2("size")
                .unwrap(),
            [Vec2(4.0, 3.0)]
        );
        let bounds = measure(&rect, Measure::Bounds, "").unwrap();
        assert_eq!(
            bounds
                .detail()
                .get("bounds")
                .unwrap()
                .as_vec4("bounds")
                .unwrap(),
            [Vec4(0.0, 0.0, 4.0, 3.0)]
        );
        // The rectangle's own columns are untouched and shared.
        assert!(Arc::ptr_eq(
            perimeter.points().get(names::P).unwrap(),
            rect.points().get(names::P).unwrap()
        ));
    }

    #[test]
    fn measure_matches_the_analytic_values_of_a_circle() {
        let (radius, sides) = (5.0f32, 720);
        let round = circle(radius, sides);
        let tau = std::f32::consts::TAU;
        // A regular polygon's exact values, which the circle's are the limit of.
        let n = sides as f32;
        let perimeter = f32_column(
            &measure(&round, Measure::Perimeter, "").unwrap(),
            Domain::Primitive,
            "perimeter",
        );
        close(
            &perimeter,
            &[n * 2.0 * radius * (tau / n / 2.0).sin()],
            1e-3,
        );
        close(&perimeter, &[tau * radius], 0.01);
        let area = f32_column(
            &measure(&round, Measure::Area, "").unwrap(),
            Domain::Primitive,
            "area",
        );
        close(&area, &[std::f32::consts::PI * radius * radius], 0.01);
        // Curvature is 1 / R at every point, positive for a counter-clockwise
        // loop and negative for the same loop run backwards.
        let curvature = measure(&round, Measure::Curvature, "").unwrap();
        close(
            &f32_column(&curvature, Domain::Point, "curvature"),
            &vec![0.2; sides],
            1e-3,
        );
        let mut backwards = xs(&round);
        backwards.reverse();
        let backwards = path_geometry(backwards, true);
        let curvature = measure(&backwards, Measure::Curvature, "").unwrap();
        close(
            &f32_column(&curvature, Domain::Point, "curvature"),
            &vec![-0.2; sides],
            1e-3,
        );
        let area = f32_column(
            &measure(&backwards, Measure::Area, "").unwrap(),
            Domain::Primitive,
            "area",
        );
        assert!(area[0] < 0.0, "a clockwise loop is negative: {area:?}");
    }

    /// The area is the signed shoelace sum, so lobes of opposite winding
    /// cancel: this path runs one lobe clockwise and the other
    /// counter-clockwise, and reads -4 where the ink covers more.
    #[test]
    fn the_area_of_a_self_crossing_path_is_the_signed_winding_sum() {
        let crossing = path_geometry(
            vec![
                Vec2(0.0, 0.0),
                Vec2(4.0, 4.0),
                Vec2(4.0, 0.0),
                Vec2(0.0, 2.0),
            ],
            true,
        );
        let area = f32_column(
            &measure(&crossing, Measure::Area, "").unwrap(),
            Domain::Primitive,
            "area",
        );
        close(&area, &[-4.0], 1e-5);
        // A symmetric bowtie cancels exactly.
        let bowtie = path_geometry(
            vec![
                Vec2(0.0, 0.0),
                Vec2(2.0, 2.0),
                Vec2(2.0, 0.0),
                Vec2(0.0, 2.0),
            ],
            true,
        );
        let area = f32_column(
            &measure(&bowtie, Measure::Area, "").unwrap(),
            Domain::Primitive,
            "area",
        );
        close(&area, &[0.0], 1e-5);
    }

    /// An open path's area is that of the shape its chord closes, and its
    /// perimeter is only the way along it.
    #[test]
    fn an_open_path_is_measured_for_area_as_if_it_were_closed() {
        let open = path_geometry(vec![Vec2(0.0, 0.0), Vec2(4.0, 0.0), Vec2(4.0, 3.0)], false);
        let closed = path_geometry(vec![Vec2(0.0, 0.0), Vec2(4.0, 0.0), Vec2(4.0, 3.0)], true);
        let area = |geometry: &Geometry| {
            f32_column(
                &measure(geometry, Measure::Area, "").unwrap(),
                Domain::Primitive,
                "area",
            )
        };
        close(&area(&open), &[6.0], 1e-5);
        assert_eq!(area(&open), area(&closed));
        let perimeter = |geometry: &Geometry| {
            f32_column(
                &measure(geometry, Measure::Perimeter, "").unwrap(),
                Domain::Primitive,
                "perimeter",
            )
        };
        close(&perimeter(&open), &[7.0], 1e-5);
        close(&perimeter(&closed), &[12.0], 1e-5);
        // The open path's last point has no segment leaving it, its ends no
        // curvature.
        let lengths = measure(&open, Measure::SegmentLength, "").unwrap();
        close(
            &f32_column(&lengths, Domain::Point, "segment_length"),
            &[4.0, 3.0, 0.0],
            1e-5,
        );
        let curvature = measure(&open, Measure::Curvature, "").unwrap();
        let curvature = f32_column(&curvature, Domain::Point, "curvature");
        assert_eq!((curvature[0], curvature[2]), (0.0, 0.0));
        assert!(curvature[1] > 0.0, "a left turn: {curvature:?}");
    }

    #[test]
    fn measure_writes_each_quantity_on_its_domain_under_the_requested_name() {
        let rect = rectangle(true);
        for (what, domain) in [
            (Measure::Perimeter, Domain::Primitive),
            (Measure::Area, Domain::Primitive),
            (Measure::Size, Domain::Primitive),
            (Measure::Curvature, Domain::Point),
            (Measure::SegmentLength, Domain::Point),
            (Measure::Bounds, Domain::Detail),
        ] {
            assert_eq!(what.domain(), domain);
            let named = measure(&rect, what, "mine").unwrap();
            assert!(
                named.attribute_set(domain).get("mine").is_some(),
                "{what:?}"
            );
            assert!(
                named
                    .attribute_set(domain)
                    .get(what.default_name())
                    .is_none(),
                "{what:?}"
            );
            let default = measure(&rect, what, "").unwrap();
            assert!(
                default
                    .attribute_set(domain)
                    .get(what.default_name())
                    .is_some()
            );
        }
        // Two primitives get one value each, in primitive order.
        let mut two = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(4.0, 0.0),
            Vec2(4.0, 3.0),
            Vec2(0.0, 3.0),
            Vec2(10.0, 10.0),
            Vec2(11.0, 10.0),
            Vec2(11.0, 12.0),
        ]);
        two.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        two.push_primitive(Primitive::Path {
            verts: 4..7,
            closed: true,
        });
        let sized = measure(&two, Measure::Size, "").unwrap();
        assert_eq!(
            sized
                .primitive_attrs()
                .get("size")
                .unwrap()
                .as_vec2("size")
                .unwrap(),
            [Vec2(4.0, 3.0), Vec2(1.0, 2.0)]
        );
    }

    #[test]
    fn measure_copes_with_degenerate_and_empty_geometry() {
        let single = path_geometry(vec![Vec2(3.0, 4.0)], false);
        for what in [
            Measure::Perimeter,
            Measure::Area,
            Measure::Curvature,
            Measure::SegmentLength,
            Measure::Size,
            Measure::Bounds,
        ] {
            let measured = measure(&single, what, "").unwrap();
            assert_eq!(measured.validate(), Ok(()), "{what:?}");
            let empty = measure(&Geometry::new(), what, "").unwrap();
            assert_eq!(empty.validate(), Ok(()), "{what:?}");
        }
        let bounds = measure(&Geometry::new(), Measure::Bounds, "").unwrap();
        assert_eq!(
            bounds
                .detail()
                .get("bounds")
                .unwrap()
                .as_vec4("bounds")
                .unwrap(),
            [Vec4(0.0, 0.0, 0.0, 0.0)]
        );
        // Coincident points have no curvature rather than NaN.
        let stacked = path_geometry(vec![Vec2(1.0, 1.0); 3], true);
        let curvature = measure(&stacked, Measure::Curvature, "").unwrap();
        assert_eq!(f32_column(&curvature, Domain::Point, "curvature"), [0.0; 3]);
        let mut mesh = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0), Vec2(0.0, 1.0)]);
        mesh.push_mesh(0..3, &[0, 1, 2]);
        assert!(measure(&mesh, Measure::Area, "").is_err());
        // But a mesh has an extent.
        assert!(measure(&mesh, Measure::Size, "").is_ok());
    }

    /// A path piece and a mesh piece each keep their own points and runs.
    #[test]
    fn split_by_piece_rebases_paths_and_meshes_per_piece() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0); 7]);
        // Piece 1: a triangle mesh over points 0..3, then a path over 3..5;
        // piece 0: a path over 5..7.
        geometry.push_mesh(0..3, &[0, 1, 2]);
        geometry.push_primitive(Primitive::Path {
            verts: 3..5,
            closed: true,
        });
        geometry.push_primitive(Primitive::Path {
            verts: 5..7,
            closed: false,
        });
        geometry
            .primitive_attrs_mut()
            .insert(names::PIECE, AttributeArray::I32(vec![1, 1, 0]))
            .unwrap();
        geometry
            .points_mut()
            .insert("id", AttributeArray::I32((0..7).collect()))
            .unwrap();

        let pieces = split_by_piece(&geometry, names::PIECE).unwrap();
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].point_count(), 2);
        assert_eq!(pieces[1].point_count(), 5);
        assert_eq!(
            pieces[1].primitives(),
            [
                Primitive::Mesh {
                    verts: 0..3,
                    indices: 0..3
                },
                Primitive::Path {
                    verts: 3..5,
                    closed: true
                }
            ]
        );
        assert_eq!(
            pieces[0].points().get("id").unwrap().as_i32("id").unwrap(),
            [5, 6]
        );
        assert_eq!(pieces[1].indices(), [0, 1, 2]);
        for piece in &pieces {
            assert_eq!(piece.validate(), Ok(()));
        }
    }

    /// Detail reaches every piece, other primitive columns follow their
    /// primitives, and a point two pieces share is duplicated into each.
    #[test]
    fn split_by_piece_carries_detail_columns_and_duplicates_shared_points() {
        // Primitive 0 (piece 1) runs over points 0..3, primitive 1 (piece 0)
        // over 2..4: point 2 is shared across the two pieces.
        let mut geometry = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(1.0, 0.0),
            Vec2(2.0, 0.0),
            Vec2(3.0, 0.0),
        ]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: false,
        });
        geometry.push_primitive(Primitive::Path {
            verts: 2..4,
            closed: false,
        });
        let prims = geometry.primitive_attrs_mut();
        prims
            .insert(names::PIECE, AttributeArray::I32(vec![1, 0]))
            .unwrap();
        prims
            .insert("width", AttributeArray::F32(vec![10.0, 20.0]))
            .unwrap();
        prims
            .insert("name", AttributeArray::Str(vec!["a".into(), "b".into()]))
            .unwrap();
        geometry
            .detail_mut()
            .insert("title", AttributeArray::Str(vec!["scene".into()]))
            .unwrap();

        let pieces = split_by_piece(&geometry, names::PIECE).unwrap();
        assert_eq!(pieces.len(), 2);
        for piece in &pieces {
            assert_eq!(
                piece
                    .detail()
                    .get("title")
                    .unwrap()
                    .as_str("title")
                    .unwrap(),
                ["scene"],
                "detail must reach every piece"
            );
        }
        // Ascending: piece 0 holds primitive 1, piece 1 holds primitive 0.
        assert_eq!(
            pieces[0]
                .primitive_attrs()
                .get("width")
                .unwrap()
                .as_f32("width")
                .unwrap(),
            [20.0]
        );
        assert_eq!(
            pieces[1]
                .primitive_attrs()
                .get("width")
                .unwrap()
                .as_f32("width")
                .unwrap(),
            [10.0]
        );
        assert_eq!(
            pieces[0]
                .primitive_attrs()
                .get("name")
                .unwrap()
                .as_str("name")
                .unwrap(),
            ["b"]
        );
        assert_eq!(
            pieces[1]
                .primitive_attrs()
                .get("name")
                .unwrap()
                .as_str("name")
                .unwrap(),
            ["a"]
        );
        // The shared point (x = 2) is in both pieces.
        let xs = |piece: &Geometry| -> Vec<f32> {
            piece
                .points()
                .get(names::P)
                .unwrap()
                .as_vec2(names::P)
                .unwrap()
                .iter()
                .map(|p| p.0)
                .collect()
        };
        assert_eq!(xs(&pieces[0]), [2.0, 3.0]);
        assert_eq!(xs(&pieces[1]), [0.0, 1.0, 2.0]);
    }

    #[test]
    fn split_by_piece_refuses_instances_and_wrong_column_types() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0); 2]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        geometry
            .primitive_attrs_mut()
            .insert(names::PIECE, AttributeArray::F32(vec![0.0]))
            .unwrap();
        assert!(matches!(
            split_by_piece(&geometry, names::PIECE),
            Err(GeometryOpError::Geometry(
                GeometryError::TypeMismatch { .. }
            ))
        ));

        let mut with_instances = geometry.clone();
        with_instances
            .instances_mut()
            .insert(names::INDEX, AttributeArray::I32(vec![0]))
            .unwrap();
        assert!(matches!(
            split_by_piece(&with_instances, names::PIECE),
            Err(GeometryOpError::HasInstances { .. })
        ));
    }
}
