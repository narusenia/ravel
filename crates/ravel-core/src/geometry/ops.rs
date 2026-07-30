// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure, copy-on-write operations over geometry attributes and paths.

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
    let column = match mode {
        TransferMode::Nearest => {
            let indices = target_positions
                .iter()
                .map(|target| nearest_index(source_positions, *target));
            select_values(source_values, indices)
        }
        TransferMode::DistanceWeighted => {
            transfer_weighted(source_values, source_positions, target_positions)?
        }
    };
    let mut result = target.clone();
    result
        .attribute_set_mut(target_domain)
        .insert(name, column)?;
    result.validate()?;
    Ok(result)
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
    let points = positions(geometry, Domain::Point)?.require_planar("attribute.path_sample")?;
    geometry.require_paths("attribute.path_sample")?;
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

fn transfer_weighted(
    source: &AttributeArray,
    source_positions: &[Vec3],
    target_positions: &[Vec3],
) -> Result<AttributeArray, GeometryOpError> {
    let weights = || {
        target_positions
            .iter()
            .map(|target| normalized_weights(source_positions, *target))
    };
    Ok(match source {
        AttributeArray::F32(values) => AttributeArray::F32(
            weights()
                .map(|weights| weights.iter().zip(values).map(|(w, v)| w * v).sum())
                .collect(),
        ),
        AttributeArray::Vec2(values) => AttributeArray::Vec2(
            weights()
                .map(|weights| {
                    weights
                        .iter()
                        .zip(values)
                        .fold(Vec2(0.0, 0.0), |sum, (w, value)| {
                            Vec2(sum.0 + w * value.0, sum.1 + w * value.1)
                        })
                })
                .collect(),
        ),
        AttributeArray::Vec3(values) => AttributeArray::Vec3(
            weights()
                .map(|weights| {
                    weights
                        .iter()
                        .zip(values)
                        .fold(Vec3(0.0, 0.0, 0.0), |sum, (w, value)| {
                            Vec3(
                                sum.0 + w * value.0,
                                sum.1 + w * value.1,
                                sum.2 + w * value.2,
                            )
                        })
                })
                .collect(),
        ),
        AttributeArray::Vec4(values) => AttributeArray::Vec4(
            weights()
                .map(|weights| {
                    weights
                        .iter()
                        .zip(values)
                        .fold(Vec4(0.0, 0.0, 0.0, 0.0), |sum, (w, value)| {
                            Vec4(
                                sum.0 + w * value.0,
                                sum.1 + w * value.1,
                                sum.2 + w * value.2,
                                sum.3 + w * value.3,
                            )
                        })
                })
                .collect(),
        ),
        AttributeArray::Color(values) => AttributeArray::Color(
            weights()
                .map(|weights| {
                    weights
                        .iter()
                        .zip(values)
                        .fold(Color::TRANSPARENT, |sum, (w, value)| Color {
                            r: sum.r + w * value.r,
                            g: sum.g + w * value.g,
                            b: sum.b + w * value.b,
                            a: sum.a + w * value.a,
                        })
                })
                .collect(),
        ),
        AttributeArray::I32(values) => AttributeArray::I32(
            weights()
                .map(|weights| {
                    weights
                        .iter()
                        .zip(values)
                        .map(|(weight, value)| weight * *value as f32)
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

fn nearest_index(points: &[Vec3], target: Vec3) -> usize {
    points
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            distance_squared(**left, target).total_cmp(&distance_squared(**right, target))
        })
        .map_or(0, |(index, _)| index)
}

fn normalized_weights(points: &[Vec3], target: Vec3) -> Vec<f32> {
    if let Some(index) = points
        .iter()
        .position(|point| distance_squared(*point, target) <= f32::EPSILON)
    {
        let mut weights = vec![0.0; points.len()];
        weights[index] = 1.0;
        return weights;
    }
    let mut weights = points
        .iter()
        .map(|point| 1.0 / distance_squared(*point, target).sqrt())
        .collect::<Vec<_>>();
    let total = weights.iter().sum::<f32>();
    for weight in &mut weights {
        *weight /= total;
    }
    weights
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

fn push_segment(segments: &mut Vec<(Vec2, Vec2, f32, f32)>, start: Vec2, end: Vec2) {
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
}
