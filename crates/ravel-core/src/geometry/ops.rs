// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure, copy-on-write operations over geometry attributes and paths.

use std::ops::Range;

use thiserror::Error;

use super::{
    AttributeArray, AttributeType, Domain, Geometry, GeometryError, Positions, Primitive, names,
};
use crate::types::{Color, Vec2, Vec3, Vec4};

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

/// Samples the first path primitive at an absolute, clamped arc length.
///
/// Arc length along a 3D polyline has no agreed definition yet (the frame it
/// would return is ambiguous), so a geometry with `Vec3` positions is an
/// explicit error rather than a silent projection onto xy. A mesh has no arc
/// length at all, so it is rejected the same way instead of being skipped —
/// silently sampling the first path of a mixed geometry would answer a
/// question the caller did not ask.
pub fn path_sample(geometry: &Geometry, distance: f32) -> Result<PathSample, GeometryOpError> {
    let (path, closed) = first_path(geometry, "attribute.path_sample")?;
    let mut segments = Vec::with_capacity(path.len());
    for index in 1..path.len() {
        push_segment(&mut segments, path[index - 1], path[index]);
    }
    if closed {
        push_segment(&mut segments, *path.last().unwrap(), path[0]);
    }
    let total = segments.last().map_or(0.0, |segment| segment.2);
    if total <= f32::EPSILON {
        return Err(GeometryOpError::InvalidPath);
    }
    let target = distance.clamp(0.0, total);
    let &(start, end, cumulative, length) = segments
        .iter()
        .find(|segment| target <= segment.2)
        .unwrap_or_else(|| segments.last().unwrap());
    let t = ((target - (cumulative - length)) / length).clamp(0.0, 1.0);
    let tangent = normalize(Vec2(end.0 - start.0, end.1 - start.1));
    Ok(PathSample {
        position: Vec2(
            start.0 + (end.0 - start.0) * t,
            start.1 + (end.1 - start.1) * t,
        ),
        tangent,
        normal: Vec2(-tangent.1, tangent.0),
    })
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
    // Shares `push_segment` with `path_sample`: the same cumulative lengths,
    // and the same rule that a zero-length segment does not advance them (a
    // duplicated point therefore repeats its predecessor's `u`).
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
    if total <= f32::EPSILON {
        return vec![0.0; path.len()];
    }
    at_vertex.iter().map(|length| length / total).collect()
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

fn broadcast_value(value: &AttributeValue, count: usize) -> AttributeArray {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
}
