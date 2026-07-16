// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lazy, batch-evaluated scalar fields and geometry attribute modulation.

use std::fmt;
use std::sync::Arc;

use thiserror::Error;

use super::{AttributeArray, AttributeType, Domain, Geometry, GeometryError, names};
use crate::eval::EvalContext;
use crate::id::DataTypeId;
use crate::types::{Color, NodeData, Vec2, Vec3, Vec4};

/// A pure, batch-evaluated mapping from positions to attribute values.
pub trait Field: Send + Sync {
    fn sample(&self, positions: &[Vec2], ctx: &EvalContext) -> AttributeArray;
}

/// A lazily evaluated field that can flow through node graph ports.
#[derive(Clone)]
pub struct FieldValue(pub Arc<dyn Field>);

impl FieldValue {
    pub fn new(field: impl Field + 'static) -> Self {
        Self(Arc::new(field))
    }

    pub fn sample(&self, positions: &[Vec2], ctx: &EvalContext) -> AttributeArray {
        self.0.sample(positions, ctx)
    }
}

impl fmt::Debug for FieldValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("FieldValue")
            .field(&"<field>")
            .finish()
    }
}

impl NodeData for FieldValue {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::FIELD
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Deterministic two-dimensional simplex fractal noise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NoiseField {
    pub seed: u32,
    pub frequency: f32,
    pub octaves: u32,
}

impl Default for NoiseField {
    fn default() -> Self {
        Self {
            seed: 0,
            frequency: 1.0,
            octaves: 1,
        }
    }
}

impl Field for NoiseField {
    fn sample(&self, positions: &[Vec2], _ctx: &EvalContext) -> AttributeArray {
        let values = positions
            .iter()
            .map(|position| {
                let mut amplitude = 1.0;
                let mut frequency = self.frequency;
                let mut total = 0.0;
                let mut normalization = 0.0;
                for octave in 0..self.octaves.max(1) {
                    total += amplitude
                        * simplex_2d(
                            position.0 * frequency,
                            position.1 * frequency,
                            self.seed.wrapping_add(octave),
                        );
                    normalization += amplitude;
                    amplitude *= 0.5;
                    frequency *= 2.0;
                }
                total / normalization
            })
            .collect();
        AttributeArray::F32(values)
    }
}

/// Geometric distance used by a [`FalloffField`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FalloffShape {
    /// Euclidean distance from `center`.
    Sphere,
    /// Absolute signed distance along `direction` from `center`.
    Linear { direction: Vec2 },
}

/// Smooth falloff that is one through `inner_radius` and zero at `outer_radius`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FalloffField {
    pub center: Vec2,
    pub inner_radius: f32,
    pub outer_radius: f32,
    pub shape: FalloffShape,
}

impl Field for FalloffField {
    fn sample(&self, positions: &[Vec2], _ctx: &EvalContext) -> AttributeArray {
        let values = positions
            .iter()
            .map(|position| {
                let delta = Vec2(position.0 - self.center.0, position.1 - self.center.1);
                let distance = match self.shape {
                    FalloffShape::Sphere => delta.0.hypot(delta.1),
                    FalloffShape::Linear { direction } => {
                        let length = direction.0.hypot(direction.1);
                        if length <= f32::EPSILON {
                            0.0
                        } else {
                            (delta.0 * direction.0 + delta.1 * direction.1).abs() / length
                        }
                    }
                };
                smooth_falloff(distance, self.inner_radius, self.outer_radius)
            })
            .collect();
        AttributeArray::F32(values)
    }
}

/// Piecewise-linear scalar remapping of another field.
#[derive(Clone, Debug)]
pub struct CurveRemapField {
    pub source: FieldValue,
    /// Control points sorted by input value. Construction sorts a supplied curve.
    pub points: Arc<[(f32, f32)]>,
}

impl CurveRemapField {
    pub fn new(source: FieldValue, mut points: Vec<(f32, f32)>) -> Self {
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self {
            source,
            points: points.into(),
        }
    }
}

impl Field for CurveRemapField {
    fn sample(&self, positions: &[Vec2], ctx: &EvalContext) -> AttributeArray {
        let values = scalar_values(self.source.sample(positions, ctx), positions.len())
            .into_iter()
            .map(|value| remap_curve(value, &self.points))
            .collect();
        AttributeArray::F32(values)
    }
}

/// Placeholder for future Lua-backed field evaluation.
///
/// This mirrors the animation expression placeholder: it retains the expression
/// and a deterministic default until the scripting runtime is integrated.
#[derive(Clone, Debug, PartialEq)]
pub struct ExpressionField {
    pub expression: String,
    pub default: f32,
}

impl Field for ExpressionField {
    fn sample(&self, positions: &[Vec2], _ctx: &EvalContext) -> AttributeArray {
        AttributeArray::F32(vec![self.default; positions.len()])
    }
}

/// Image sampling fields are intentionally deferred until `FrameBuffer` has a
/// defined UV-coordinate input and sampling policy.

macro_rules! binary_field {
    ($name:ident, $operation:expr) => {
        #[derive(Clone, Debug)]
        pub struct $name {
            pub left: FieldValue,
            pub right: FieldValue,
        }

        impl Field for $name {
            fn sample(&self, positions: &[Vec2], ctx: &EvalContext) -> AttributeArray {
                let left = scalar_values(self.left.sample(positions, ctx), positions.len());
                let right = scalar_values(self.right.sample(positions, ctx), positions.len());
                AttributeArray::F32(left.into_iter().zip(right).map($operation).collect())
            }
        }
    };
}

binary_field!(AddField, |(left, right)| left + right);
binary_field!(MultiplyField, |(left, right)| left * right);
binary_field!(MaxField, |(left, right)| left.max(right));

/// Linear interpolation between two scalar fields.
#[derive(Clone, Debug)]
pub struct BlendField {
    pub left: FieldValue,
    pub right: FieldValue,
    pub amount: f32,
}

impl Field for BlendField {
    fn sample(&self, positions: &[Vec2], ctx: &EvalContext) -> AttributeArray {
        let left = scalar_values(self.left.sample(positions, ctx), positions.len());
        let right = scalar_values(self.right.sample(positions, ctx), positions.len());
        let amount = self.amount.clamp(0.0, 1.0);
        AttributeArray::F32(
            left.into_iter()
                .zip(right)
                .map(|(left, right)| left + (right - left) * amount)
                .collect(),
        )
    }
}

/// Errors produced by [`apply_field`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FieldError {
    #[error(transparent)]
    Geometry(#[from] GeometryError),
    #[error("field modulation does not support {0} attributes")]
    UnsupportedAttributeType(AttributeType),
}

/// Returns a geometry clone with a field blended into one numeric attribute.
///
/// Positions are read from the selected domain's `P` attribute. The original
/// geometry and its structurally shared columns are not mutated.
pub fn apply_field(
    geometry: &Geometry,
    domain: Domain,
    target: &str,
    field: &dyn Field,
    amount: f32,
    ctx: &EvalContext,
) -> Result<Geometry, FieldError> {
    let attributes = geometry.attribute_set(domain);
    let positions = attributes
        .get(names::P)
        .ok_or_else(|| GeometryError::AttributeNotFound {
            name: names::P.into(),
        })?
        .as_vec2(names::P)?;
    let existing = attributes
        .get(target)
        .ok_or_else(|| GeometryError::AttributeNotFound {
            name: target.into(),
        })?;
    let sampled = field.sample(positions, ctx);
    if sampled.len() != positions.len() {
        return Err(GeometryError::LengthMismatch {
            name: target.into(),
            expected: positions.len(),
            actual: sampled.len(),
        }
        .into());
    }
    if sampled.attr_type() != existing.attr_type() {
        return Err(GeometryError::TypeMismatch {
            name: target.into(),
            expected: existing.attr_type(),
            actual: sampled.attr_type(),
        }
        .into());
    }

    let blended = blend_arrays(existing, &sampled, amount.clamp(0.0, 1.0))?;
    let mut result = geometry.clone();
    result.attribute_set_mut(domain).insert(target, blended)?;
    Ok(result)
}

fn blend_arrays(
    left: &AttributeArray,
    right: &AttributeArray,
    amount: f32,
) -> Result<AttributeArray, FieldError> {
    macro_rules! blend_tuple {
        ($left:expr, $right:expr, $constructor:expr) => {
            $left
                .iter()
                .zip($right)
                .map(|(a, b)| $constructor(a, b, amount))
                .collect()
        };
    }

    Ok(match (left, right) {
        (AttributeArray::F32(a), AttributeArray::F32(b)) => {
            AttributeArray::F32(a.iter().zip(b).map(|(a, b)| a + (b - a) * amount).collect())
        }
        (AttributeArray::Vec2(a), AttributeArray::Vec2(b)) => AttributeArray::Vec2(blend_tuple!(
            a,
            b,
            |a: &Vec2, b: &Vec2, t| Vec2(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
        )),
        (AttributeArray::Vec3(a), AttributeArray::Vec3(b)) => {
            AttributeArray::Vec3(blend_tuple!(a, b, |a: &Vec3, b: &Vec3, t| Vec3(
                a.0 + (b.0 - a.0) * t,
                a.1 + (b.1 - a.1) * t,
                a.2 + (b.2 - a.2) * t,
            )))
        }
        (AttributeArray::Vec4(a), AttributeArray::Vec4(b)) => {
            AttributeArray::Vec4(blend_tuple!(a, b, |a: &Vec4, b: &Vec4, t| Vec4(
                a.0 + (b.0 - a.0) * t,
                a.1 + (b.1 - a.1) * t,
                a.2 + (b.2 - a.2) * t,
                a.3 + (b.3 - a.3) * t,
            )))
        }
        (AttributeArray::Color(a), AttributeArray::Color(b)) => {
            AttributeArray::Color(blend_tuple!(a, b, |a: &Color, b: &Color, t| Color {
                r: a.r + (b.r - a.r) * t,
                g: a.g + (b.g - a.g) * t,
                b: a.b + (b.b - a.b) * t,
                a: a.a + (b.a - a.a) * t,
            }))
        }
        _ => return Err(FieldError::UnsupportedAttributeType(left.attr_type())),
    })
}

fn scalar_values(array: AttributeArray, expected_len: usize) -> Vec<f32> {
    match array {
        AttributeArray::F32(values) if values.len() == expected_len => values,
        _ => vec![0.0; expected_len],
    }
}

fn smooth_falloff(distance: f32, inner: f32, outer: f32) -> f32 {
    if outer <= inner {
        return if distance <= inner { 1.0 } else { 0.0 };
    }
    let t = ((distance - inner) / (outer - inner)).clamp(0.0, 1.0);
    let smooth = t * t * (3.0 - 2.0 * t);
    1.0 - smooth
}

fn remap_curve(value: f32, points: &[(f32, f32)]) -> f32 {
    let Some(&(first_x, first_y)) = points.first() else {
        return value;
    };
    if value <= first_x {
        return first_y;
    }
    for pair in points.windows(2) {
        let [(x0, y0), (x1, y1)] = pair else {
            continue;
        };
        if value <= *x1 {
            let width = x1 - x0;
            return if width.abs() <= f32::EPSILON {
                *y1
            } else {
                y0 + (y1 - y0) * ((value - x0) / width)
            };
        }
    }
    points.last().map_or(value, |point| point.1)
}

// Small seeded 2D simplex implementation derived from Stefan Gustavson's
// public-domain simplex noise algorithm.
fn simplex_2d(x: f32, y: f32, seed: u32) -> f32 {
    const F2: f32 = 0.366_025_42;
    const G2: f32 = 0.211_324_87;
    const GRADIENTS: [(f32, f32); 8] = [
        (1.0, 1.0),
        (-1.0, 1.0),
        (1.0, -1.0),
        (-1.0, -1.0),
        (1.0, 0.0),
        (-1.0, 0.0),
        (0.0, 1.0),
        (0.0, -1.0),
    ];

    let skew = (x + y) * F2;
    let i = (x + skew).floor() as i32;
    let j = (y + skew).floor() as i32;
    let unskew = (i + j) as f32 * G2;
    let x0 = x - (i as f32 - unskew);
    let y0 = y - (j as f32 - unskew);
    let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };
    let x1 = x0 - i1 as f32 + G2;
    let y1 = y0 - j1 as f32 + G2;
    let x2 = x0 - 1.0 + 2.0 * G2;
    let y2 = y0 - 1.0 + 2.0 * G2;

    let corner = |dx: f32, dy: f32, lattice_x: i32, lattice_y: i32| {
        let attenuation = 0.5 - dx * dx - dy * dy;
        if attenuation <= 0.0 {
            0.0
        } else {
            let gradient = GRADIENTS[hash_lattice(lattice_x, lattice_y, seed) as usize & 7];
            let attenuation2 = attenuation * attenuation;
            attenuation2 * attenuation2 * (gradient.0 * dx + gradient.1 * dy)
        }
    };

    70.0 * (corner(x0, y0, i, j) + corner(x1, y1, i + i1, j + j1) + corner(x2, y2, i + 1, j + 1))
}

fn hash_lattice(x: i32, y: i32, seed: u32) -> u32 {
    let mut hash = seed ^ (x as u32).wrapping_mul(0x9e37_79b9);
    hash ^= (y as u32).wrapping_mul(0x85eb_ca6b);
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x7feb_352d);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x846c_a68b);
    hash ^ (hash >> 16)
}
