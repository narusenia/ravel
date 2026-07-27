// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lazy, batch-evaluated scalar fields and geometry attribute modulation.

use std::fmt;
use std::sync::Arc;

use thiserror::Error;

use super::{AttributeArray, AttributeSet, AttributeType, Domain, Geometry, GeometryError, names};
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

/// Deferred image-sampling field marker.
///
/// This deliberately does not implement [`Field`] until `FrameBuffer` has a
/// defined UV-coordinate input and sampling policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageSamplerField;

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

/// How a sampled value is combined with the attribute's existing value.
///
/// The combined value is then interpolated back toward the existing one by
/// `amount` (see [`FieldApply::amount`]), so `amount = 0` means "no
/// modulation" in every mode and `Set` at `amount = 1` replaces the value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CombineMode {
    /// Replace the existing value with the sampled one.
    #[default]
    Set,
    /// Add the sampled value to the existing one.
    Add,
    /// Scale the existing value by the sampled one.
    Multiply,
    /// Keep whichever is smaller.
    Min,
    /// Keep whichever is larger.
    Max,
}

impl CombineMode {
    /// Parse a parameter string; anything unrecognised falls back to `Set`.
    pub fn parse(value: &str) -> Self {
        match value {
            "add" => Self::Add,
            "multiply" => Self::Multiply,
            "min" => Self::Min,
            "max" => Self::Max,
            _ => Self::Set,
        }
    }

    fn apply(self, existing: f32, sampled: f32) -> f32 {
        match self {
            Self::Set => sampled,
            Self::Add => existing + sampled,
            Self::Multiply => existing * sampled,
            Self::Min => existing.min(sampled),
            Self::Max => existing.max(sampled),
        }
    }
}

/// Which components of a multi-component attribute a field writes to.
///
/// Components are addressed positionally, so both spellings of the same slot
/// work: `x`/`r` is 0, `y`/`g` is 1, `z`/`b` is 2, `w`/`a` is 3. An empty or
/// unrecognised specification selects every component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComponentMask(u8);

impl Default for ComponentMask {
    fn default() -> Self {
        Self::ALL
    }
}

impl ComponentMask {
    /// Every component.
    pub const ALL: Self = Self(0b1111);

    /// Parse a component specification such as `"xy"`, `"rgb"` or `"a"`.
    ///
    /// Unknown characters are ignored; a specification that selects nothing
    /// falls back to [`ComponentMask::ALL`] so a typo cannot silently turn the
    /// node into a no-op.
    pub fn parse(spec: &str) -> Self {
        let mut bits = 0u8;
        for character in spec.chars() {
            let slot = match character.to_ascii_lowercase() {
                'x' | 'r' => 0,
                'y' | 'g' => 1,
                'z' | 'b' => 2,
                'w' | 'a' => 3,
                _ => continue,
            };
            bits |= 1 << slot;
        }
        if bits == 0 { Self::ALL } else { Self(bits) }
    }

    /// Whether component `index` is selected.
    pub fn contains(self, index: usize) -> bool {
        index < 4 && self.0 & (1 << index) != 0
    }

    /// Narrow the mask to an attribute with `arity` components.
    ///
    /// A specification that names only components the target does not have —
    /// `"z"` on a `Vec2`, say — would otherwise select nothing and silently
    /// turn the node into a no-op. Treat it the same way as an unusable group
    /// name: warn and fall back to every component.
    fn resolved_for(self, arity: usize, target: &str) -> Self {
        let available = Self((1u8 << arity.min(4)) - 1);
        let narrowed = Self(self.0 & available.0);
        if narrowed.0 == 0 {
            tracing::warn!(
                target_attribute = target,
                arity,
                "field component mask selects no component of the target; writing every component"
            );
            return available;
        }
        narrowed
    }
}

/// Number of scalar components a numeric attribute type carries.
fn component_arity(attr_type: AttributeType) -> usize {
    match attr_type {
        AttributeType::Vec2 => 2,
        AttributeType::Vec3 => 3,
        AttributeType::Vec4 | AttributeType::Color => 4,
        _ => 1,
    }
}

/// What [`apply_field`] should do with the values it samples.
#[derive(Clone, Copy, Debug)]
pub struct FieldApply<'a> {
    /// Geometry domain whose `P` drives the sampling and whose column is written.
    pub domain: Domain,
    /// Name of the attribute to modulate.
    pub target: &'a str,
    /// How far to move the existing value toward the combined one, `0..=1`.
    pub amount: f32,
    /// How the sampled value combines with the existing one.
    pub combine: CombineMode,
    /// Which components of a multi-component target to write.
    pub components: ComponentMask,
    /// Name of a Bool attribute restricting the affected elements. Empty
    /// selects every element (REQ-CORE-013 element-scope convention).
    pub group: &'a str,
}

impl<'a> FieldApply<'a> {
    /// Modulate `target` on `domain`, replacing the value outright.
    pub fn new(domain: Domain, target: &'a str) -> Self {
        Self {
            domain,
            target,
            amount: 1.0,
            combine: CombineMode::Set,
            components: ComponentMask::ALL,
            group: "",
        }
    }

    pub fn with_amount(mut self, amount: f32) -> Self {
        self.amount = amount;
        self
    }

    pub fn with_combine(mut self, combine: CombineMode) -> Self {
        self.combine = combine;
        self
    }

    pub fn with_components(mut self, components: ComponentMask) -> Self {
        self.components = components;
        self
    }

    pub fn with_group(mut self, group: &'a str) -> Self {
        self.group = group;
        self
    }
}

/// Returns a geometry clone with a field combined into one numeric attribute.
///
/// Positions are read from the selected domain's `P` attribute. The original
/// geometry and its structurally shared columns are not mutated.
///
/// A scalar field promotes to a multi-component target by broadcasting to
/// every selected component; a field whose type already matches the target is
/// combined component-wise. Any other pairing is a type error.
pub fn apply_field(
    geometry: &Geometry,
    spec: &FieldApply<'_>,
    field: &dyn Field,
    ctx: &EvalContext,
) -> Result<Geometry, FieldError> {
    let attributes = geometry.attribute_set(spec.domain);
    let positions = attributes
        .get(names::P)
        .ok_or_else(|| GeometryError::AttributeNotFound {
            name: names::P.into(),
        })?
        .as_vec2(names::P)?;
    let existing = attributes
        .get(spec.target)
        .ok_or_else(|| GeometryError::AttributeNotFound {
            name: spec.target.into(),
        })?;
    let sampled = field.sample(positions, ctx);
    if sampled.len() != positions.len() {
        return Err(GeometryError::LengthMismatch {
            name: spec.target.into(),
            expected: positions.len(),
            actual: sampled.len(),
        }
        .into());
    }
    // A scalar field promotes into any numeric target; otherwise the sampled
    // type has to match the column exactly.
    if sampled.attr_type() != AttributeType::F32 && sampled.attr_type() != existing.attr_type() {
        return Err(GeometryError::TypeMismatch {
            name: spec.target.into(),
            expected: existing.attr_type(),
            actual: sampled.attr_type(),
        }
        .into());
    }

    let selection = group_selection(attributes, spec.group, positions.len());
    let combined = combine_arrays(existing, &sampled, spec, selection.as_deref())?;
    let mut result = geometry.clone();
    result
        .attribute_set_mut(spec.domain)
        .insert(spec.target, combined)?;
    Ok(result)
}

/// Resolve the `group` parameter to a per-element selection.
///
/// `None` means "every element". A named group that is missing, not Bool, or
/// the wrong length falls back to every element with a warning rather than
/// failing the evaluation: a half-typed name in the node editor must not turn
/// the graph red (REQ-CORE-013 element-scope convention).
fn group_selection(attributes: &AttributeSet, group: &str, length: usize) -> Option<Vec<bool>> {
    if group.is_empty() {
        return None;
    }
    let Some(column) = attributes.get(group) else {
        tracing::warn!(
            group,
            "field group attribute not found; affecting every element"
        );
        return None;
    };
    let AttributeArray::Bool(values) = column.as_ref() else {
        tracing::warn!(
            group,
            attr_type = ?column.attr_type(),
            "field group attribute is not Bool; affecting every element"
        );
        return None;
    };
    if values.len() != length {
        tracing::warn!(
            group,
            expected = length,
            actual = values.len(),
            "field group attribute has the wrong length; affecting every element"
        );
        return None;
    }
    Some(values.clone())
}

/// One scalar component of a sampled column.
///
/// An `F32` column broadcasts: every component of the target reads the same
/// scalar, which is what promotes a scalar field to a vector attribute.
fn sampled_component(sampled: &AttributeArray, index: usize, component: usize) -> f32 {
    match sampled {
        AttributeArray::F32(values) => values[index],
        AttributeArray::Vec2(values) => match component {
            0 => values[index].0,
            1 => values[index].1,
            _ => 0.0,
        },
        AttributeArray::Vec3(values) => match component {
            0 => values[index].0,
            1 => values[index].1,
            2 => values[index].2,
            _ => 0.0,
        },
        AttributeArray::Vec4(values) => match component {
            0 => values[index].0,
            1 => values[index].1,
            2 => values[index].2,
            3 => values[index].3,
            _ => 0.0,
        },
        AttributeArray::Color(values) => match component {
            0 => values[index].r,
            1 => values[index].g,
            2 => values[index].b,
            3 => values[index].a,
            _ => 0.0,
        },
        _ => 0.0,
    }
}

fn combine_arrays(
    existing: &AttributeArray,
    sampled: &AttributeArray,
    spec: &FieldApply<'_>,
    selection: Option<&[bool]>,
) -> Result<AttributeArray, FieldError> {
    let amount = spec.amount.clamp(0.0, 1.0);
    let combine = spec.combine;
    let mask = spec
        .components
        .resolved_for(component_arity(existing.attr_type()), spec.target);

    // "No modulation" has to be exact, not merely arithmetically neutral:
    // interpolating by zero would still evaluate the combine op first, and
    // that turns `-0.0` into `+0.0` and an overflowing intermediate into
    // `inf * 0 = NaN`. Return the column untouched instead.
    if amount == 0.0 {
        return Ok(existing.clone());
    }

    // Existing value for an unselected component or element, the combined one
    // interpolated by `amount` otherwise. `Set` reproduces the plain blend it
    // replaced, term for term.
    let resolve = |index: usize, component: usize, existing: f32| -> f32 {
        if !mask.contains(component) {
            return existing;
        }
        let combined = combine.apply(existing, sampled_component(sampled, index, component));
        existing + (combined - existing) * amount
    };
    let affected = |index: usize| selection.is_none_or(|flags| flags[index]);

    macro_rules! combine_elements {
        ($values:expr, $element:expr) => {
            $values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    if affected(index) {
                        $element(index, value)
                    } else {
                        *value
                    }
                })
                .collect()
        };
    }

    Ok(match existing {
        AttributeArray::F32(values) => AttributeArray::F32(combine_elements!(
            values,
            |index, value: &f32| resolve(index, 0, *value)
        )),
        AttributeArray::Vec2(values) => AttributeArray::Vec2(combine_elements!(
            values,
            |index, value: &Vec2| Vec2(resolve(index, 0, value.0), resolve(index, 1, value.1))
        )),
        AttributeArray::Vec3(values) => {
            AttributeArray::Vec3(combine_elements!(values, |index, value: &Vec3| Vec3(
                resolve(index, 0, value.0),
                resolve(index, 1, value.1),
                resolve(index, 2, value.2),
            )))
        }
        AttributeArray::Vec4(values) => {
            AttributeArray::Vec4(combine_elements!(values, |index, value: &Vec4| Vec4(
                resolve(index, 0, value.0),
                resolve(index, 1, value.1),
                resolve(index, 2, value.2),
                resolve(index, 3, value.3),
            )))
        }
        AttributeArray::Color(values) => {
            AttributeArray::Color(combine_elements!(values, |index, value: &Color| Color {
                r: resolve(index, 0, value.r),
                g: resolve(index, 1, value.g),
                b: resolve(index, 2, value.b),
                a: resolve(index, 3, value.a),
            }))
        }
        _ => return Err(FieldError::UnsupportedAttributeType(existing.attr_type())),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FrameRate;

    #[derive(Clone, Copy)]
    struct ConstantField(f32);

    impl Field for ConstantField {
        fn sample(&self, positions: &[Vec2], _ctx: &EvalContext) -> AttributeArray {
            AttributeArray::F32(vec![self.0; positions.len()])
        }
    }

    struct XField;

    impl Field for XField {
        fn sample(&self, positions: &[Vec2], _ctx: &EvalContext) -> AttributeArray {
            AttributeArray::F32(positions.iter().map(|position| position.0).collect())
        }
    }

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    fn scalar_sample(field: &dyn Field, positions: &[Vec2]) -> Vec<f32> {
        field
            .sample(positions, &ctx())
            .as_f32("sample")
            .unwrap()
            .to_vec()
    }

    #[test]
    fn field_value_has_field_data_type() {
        let value = FieldValue::new(ConstantField(1.0));
        assert_eq!(value.data_type_id(), DataTypeId::FIELD);
    }

    #[test]
    fn noise_is_deterministic_for_the_same_seed() {
        let positions = [Vec2(0.13, 0.71), Vec2(-2.4, 8.1), Vec2(31.0, -0.25)];
        let field = NoiseField {
            seed: 42,
            frequency: 1.7,
            octaves: 4,
        };

        assert_eq!(
            scalar_sample(&field, &positions),
            scalar_sample(&field, &positions)
        );
    }

    #[test]
    fn fbm_octave_count_changes_output() {
        let positions = [Vec2(0.37, 0.91), Vec2(-1.2, 2.7)];
        let one_octave = NoiseField {
            seed: 7,
            frequency: 1.0,
            octaves: 1,
        };
        let four_octaves = NoiseField {
            octaves: 4,
            ..one_octave
        };

        assert_ne!(
            scalar_sample(&one_octave, &positions),
            scalar_sample(&four_octaves, &positions)
        );
    }

    #[test]
    fn sphere_falloff_holds_inner_and_outer_boundaries() {
        let field = FalloffField {
            center: Vec2(0.0, 0.0),
            inner_radius: 1.0,
            outer_radius: 3.0,
            shape: FalloffShape::Sphere,
        };
        let values = scalar_sample(
            &field,
            &[
                Vec2(0.0, 0.0),
                Vec2(1.0, 0.0),
                Vec2(2.0, 0.0),
                Vec2(3.0, 0.0),
            ],
        );

        assert_eq!(values, vec![1.0, 1.0, 0.5, 0.0]);
    }

    #[test]
    fn combinators_follow_scalar_algebra() {
        let positions = [Vec2(0.0, 0.0), Vec2(1.0, 1.0)];
        let two = FieldValue::new(ConstantField(2.0));
        let four = FieldValue::new(ConstantField(4.0));

        assert_eq!(
            scalar_sample(
                &AddField {
                    left: two.clone(),
                    right: four.clone(),
                },
                &positions,
            ),
            vec![6.0, 6.0]
        );
        assert_eq!(
            scalar_sample(
                &MultiplyField {
                    left: two.clone(),
                    right: four.clone(),
                },
                &positions,
            ),
            vec![8.0, 8.0]
        );
        assert_eq!(
            scalar_sample(
                &MaxField {
                    left: two.clone(),
                    right: four.clone(),
                },
                &positions,
            ),
            vec![4.0, 4.0]
        );
        assert_eq!(
            scalar_sample(
                &BlendField {
                    left: two,
                    right: four,
                    amount: 0.25,
                },
                &positions,
            ),
            vec![2.5, 2.5]
        );
    }

    #[test]
    fn curve_remap_interpolates_and_clamps() {
        let field = CurveRemapField::new(
            FieldValue::new(XField),
            vec![(1.0, 10.0), (0.0, 0.0), (0.5, 2.0)],
        );
        let values = scalar_sample(&field, &[Vec2(-1.0, 0.0), Vec2(0.25, 0.0), Vec2(2.0, 0.0)]);

        assert_eq!(values, vec![0.0, 1.0, 10.0]);
    }

    #[test]
    fn apply_field_modulates_point_attribute_without_mutating_input() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(2.0, 0.0)]);
        geometry
            .points_mut()
            .insert("weight", AttributeArray::F32(vec![2.0, 2.0]))
            .unwrap();

        let spec = FieldApply::new(Domain::Point, "weight").with_amount(0.5);
        let result = apply_field(&geometry, &spec, &XField, &ctx()).unwrap();

        assert_eq!(
            result.points().get("weight").unwrap().as_f32("weight"),
            Ok(&[1.0, 2.0][..])
        );
        assert_eq!(
            geometry.points().get("weight").unwrap().as_f32("weight"),
            Ok(&[2.0, 2.0][..])
        );
    }

    // ---- combine modes, component masks and groups -------------------------

    /// Two points at x = 0 and x = 2, so `XField` samples 0.0 and 2.0.
    fn two_points_with(name: &str, column: AttributeArray) -> Geometry {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(2.0, 0.0)]);
        geometry.points_mut().insert(name, column).unwrap();
        geometry
    }

    fn applied(geometry: &Geometry, spec: &FieldApply<'_>) -> Geometry {
        apply_field(geometry, spec, &XField, &ctx()).unwrap()
    }

    #[test]
    fn scalar_field_promotes_to_a_vec2_target_by_multiplying() {
        // The plan's headline case: a Vec2 `scale` modulated by a scalar field.
        let geometry = two_points_with(
            "scale",
            AttributeArray::Vec2(vec![Vec2(3.0, 4.0), Vec2(3.0, 4.0)]),
        );
        let spec = FieldApply::new(Domain::Point, "scale").with_combine(CombineMode::Multiply);

        let result = applied(&geometry, &spec);

        assert_eq!(
            result.points().get("scale").unwrap().as_vec2("scale"),
            // x = 0 → scaled to zero; x = 2 → doubled.
            Ok(&[Vec2(0.0, 0.0), Vec2(6.0, 8.0)][..])
        );
    }

    #[test]
    fn component_mask_leaves_unselected_components_untouched() {
        let geometry = two_points_with(
            "Cd",
            AttributeArray::Color(vec![
                Color {
                    r: 0.2,
                    g: 0.4,
                    b: 0.6,
                    a: 0.8,
                },
                Color {
                    r: 0.2,
                    g: 0.4,
                    b: 0.6,
                    a: 0.8,
                },
            ]),
        );
        let spec =
            FieldApply::new(Domain::Point, "Cd").with_components(ComponentMask::parse("rgb"));

        let result = applied(&geometry, &spec);
        let colors = result.points().get("Cd").unwrap().as_color("Cd").unwrap();

        // rgb replaced by the sample, alpha untouched in both elements.
        assert_eq!(
            colors[0],
            Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.8
            }
        );
        assert_eq!(
            colors[1],
            Color {
                r: 2.0,
                g: 2.0,
                b: 2.0,
                a: 0.8
            }
        );
    }

    #[test]
    fn zero_amount_is_no_modulation_in_every_mode() {
        let geometry = two_points_with("weight", AttributeArray::F32(vec![2.0, 5.0]));
        for combine in [
            CombineMode::Set,
            CombineMode::Add,
            CombineMode::Multiply,
            CombineMode::Min,
            CombineMode::Max,
        ] {
            let spec = FieldApply::new(Domain::Point, "weight")
                .with_combine(combine)
                .with_amount(0.0);
            let result = applied(&geometry, &spec);
            assert_eq!(
                result.points().get("weight").unwrap().as_f32("weight"),
                Ok(&[2.0, 5.0][..]),
                "{combine:?} at amount 0 must not modulate"
            );
        }
    }

    #[test]
    fn combine_modes_operate_on_the_existing_value() {
        let geometry = two_points_with("weight", AttributeArray::F32(vec![3.0, 3.0]));
        // Samples are 0.0 and 2.0 against an existing 3.0.
        let cases = [
            (CombineMode::Set, [0.0, 2.0]),
            (CombineMode::Add, [3.0, 5.0]),
            (CombineMode::Multiply, [0.0, 6.0]),
            (CombineMode::Min, [0.0, 2.0]),
            (CombineMode::Max, [3.0, 3.0]),
        ];
        for (combine, expected) in cases {
            let spec = FieldApply::new(Domain::Point, "weight").with_combine(combine);
            let result = applied(&geometry, &spec);
            assert_eq!(
                result.points().get("weight").unwrap().as_f32("weight"),
                Ok(&expected[..]),
                "{combine:?}"
            );
        }
    }

    #[test]
    fn group_restricts_the_affected_elements() {
        let mut geometry = two_points_with("weight", AttributeArray::F32(vec![7.0, 7.0]));
        geometry
            .points_mut()
            .insert("mask", AttributeArray::Bool(vec![false, true]))
            .unwrap();

        let spec = FieldApply::new(Domain::Point, "weight").with_group("mask");
        let result = applied(&geometry, &spec);

        assert_eq!(
            result.points().get("weight").unwrap().as_f32("weight"),
            // Element 0 is outside the group and keeps its input value exactly.
            Ok(&[7.0, 2.0][..])
        );
    }

    #[test]
    fn unusable_group_names_fall_back_to_every_element() {
        let mut geometry = two_points_with("weight", AttributeArray::F32(vec![7.0, 7.0]));
        geometry
            .points_mut()
            .insert("not_bool", AttributeArray::F32(vec![1.0, 1.0]))
            .unwrap();

        // A missing name and a non-Bool column are warnings, not errors: a
        // half-typed group in the node editor must not fail the evaluation.
        for group in ["typo", "not_bool"] {
            let spec = FieldApply::new(Domain::Point, "weight").with_group(group);
            let result = applied(&geometry, &spec);
            assert_eq!(
                result.points().get("weight").unwrap().as_f32("weight"),
                Ok(&[0.0, 2.0][..]),
                "group {group:?} should affect every element"
            );
        }
    }

    /// Bit patterns, not numeric equality: `-0.0 == 0.0` and `NaN != NaN`
    /// both hide the exact regressions these tests exist to catch.
    fn bits(values: &[f32]) -> Vec<u32> {
        values.iter().map(|value| value.to_bits()).collect()
    }

    #[test]
    fn zero_amount_preserves_the_exact_bit_pattern() {
        // Interpolating by zero would still evaluate the combine op:
        // `-0.0 + 0.0` is `+0.0`, and an overflowing sum times zero is NaN.
        let signed_zero = -0.0f32;
        let huge = f32::MAX;
        let quiet_nan = f32::from_bits(0x7fc0_1234);
        let geometry = two_points_with("weight", AttributeArray::F32(vec![signed_zero, huge]));

        for combine in [
            CombineMode::Set,
            CombineMode::Add,
            CombineMode::Multiply,
            CombineMode::Min,
            CombineMode::Max,
        ] {
            let spec = FieldApply::new(Domain::Point, "weight")
                .with_combine(combine)
                .with_amount(0.0);
            let result = applied(&geometry, &spec);
            assert_eq!(
                bits(
                    result
                        .points()
                        .get("weight")
                        .unwrap()
                        .as_f32("weight")
                        .unwrap()
                ),
                bits(&[signed_zero, huge]),
                "{combine:?} at amount 0 must leave the bits alone"
            );
        }

        // A NaN column survives untouched too.
        let geometry = two_points_with("weight", AttributeArray::F32(vec![quiet_nan, 1.0]));
        let spec = FieldApply::new(Domain::Point, "weight").with_amount(0.0);
        let result = applied(&geometry, &spec);
        assert_eq!(
            bits(
                result
                    .points()
                    .get("weight")
                    .unwrap()
                    .as_f32("weight")
                    .unwrap()
            ),
            bits(&[quiet_nan, 1.0])
        );
    }

    #[test]
    fn elements_outside_the_group_keep_their_exact_bits() {
        let signed_zero = -0.0f32;
        let quiet_nan = f32::from_bits(0x7fc0_1234);
        let mut geometry = two_points_with("weight", AttributeArray::F32(vec![signed_zero, 1.0]));
        geometry
            .points_mut()
            .insert("mask", AttributeArray::Bool(vec![false, true]))
            .unwrap();

        let spec = FieldApply::new(Domain::Point, "weight")
            .with_combine(CombineMode::Add)
            .with_group("mask");
        let result = applied(&geometry, &spec);
        let values = result
            .points()
            .get("weight")
            .unwrap()
            .as_f32("weight")
            .unwrap();

        // Element 0 is copied, not recomputed: `-0.0` stays negative.
        assert_eq!(values[0].to_bits(), signed_zero.to_bits());
        assert_eq!(values[1], 3.0);

        // The same holds for a NaN payload outside the group.
        let mut geometry = two_points_with("weight", AttributeArray::F32(vec![quiet_nan, 1.0]));
        geometry
            .points_mut()
            .insert("mask", AttributeArray::Bool(vec![false, true]))
            .unwrap();
        let result = applied(&geometry, &spec);
        let values = result
            .points()
            .get("weight")
            .unwrap()
            .as_f32("weight")
            .unwrap();
        assert_eq!(values[0].to_bits(), quiet_nan.to_bits());
    }

    #[test]
    fn unselected_components_keep_their_exact_bits() {
        let signed_zero = -0.0f32;
        let geometry = two_points_with(
            "Cd",
            AttributeArray::Color(vec![
                Color {
                    r: 0.2,
                    g: 0.4,
                    b: 0.6,
                    a: signed_zero,
                },
                Color {
                    r: 0.2,
                    g: 0.4,
                    b: 0.6,
                    a: signed_zero,
                },
            ]),
        );
        let spec = FieldApply::new(Domain::Point, "Cd")
            .with_combine(CombineMode::Add)
            .with_components(ComponentMask::parse("rgb"));

        let result = applied(&geometry, &spec);
        let colors = result.points().get("Cd").unwrap().as_color("Cd").unwrap();

        for color in colors {
            assert_eq!(color.a.to_bits(), signed_zero.to_bits());
        }
    }

    #[test]
    fn a_mask_naming_only_absent_components_writes_every_component() {
        // "z" does not exist on a Vec2. Selecting nothing would be a silent
        // no-op, so the mask widens rather than doing nothing.
        let geometry = two_points_with(
            "scale",
            AttributeArray::Vec2(vec![Vec2(1.0, 1.0), Vec2(1.0, 1.0)]),
        );
        let spec =
            FieldApply::new(Domain::Point, "scale").with_components(ComponentMask::parse("z"));

        let result = applied(&geometry, &spec);

        assert_eq!(
            result.points().get("scale").unwrap().as_vec2("scale"),
            Ok(&[Vec2(0.0, 0.0), Vec2(2.0, 2.0)][..])
        );
    }

    #[test]
    fn a_mask_naming_present_components_still_narrows() {
        // Guard against the widening above swallowing legitimate masks.
        let geometry = two_points_with(
            "scale",
            AttributeArray::Vec2(vec![Vec2(1.0, 1.0), Vec2(1.0, 1.0)]),
        );
        let spec =
            FieldApply::new(Domain::Point, "scale").with_components(ComponentMask::parse("x"));

        let result = applied(&geometry, &spec);

        assert_eq!(
            result.points().get("scale").unwrap().as_vec2("scale"),
            Ok(&[Vec2(0.0, 1.0), Vec2(2.0, 1.0)][..])
        );
    }

    #[test]
    fn component_mask_parsing_accepts_both_spellings() {
        assert!(ComponentMask::parse("").contains(3));
        assert!(ComponentMask::parse("xy").contains(0));
        assert!(ComponentMask::parse("xy").contains(1));
        assert!(!ComponentMask::parse("xy").contains(2));
        // `r`/`g`/`b`/`a` address the same slots as `x`/`y`/`z`/`w`.
        assert_eq!(ComponentMask::parse("rgb"), ComponentMask::parse("xyz"));
        // A specification that selects nothing falls back to every component.
        assert_eq!(ComponentMask::parse("!?"), ComponentMask::ALL);
    }

    #[test]
    fn a_non_scalar_field_must_match_the_target_type() {
        struct Vec2Field;
        impl Field for Vec2Field {
            fn sample(&self, positions: &[Vec2], _ctx: &EvalContext) -> AttributeArray {
                AttributeArray::Vec2(vec![Vec2(1.0, 1.0); positions.len()])
            }
        }

        let geometry = two_points_with("weight", AttributeArray::F32(vec![0.0, 0.0]));
        let spec = FieldApply::new(Domain::Point, "weight");
        assert!(apply_field(&geometry, &spec, &Vec2Field, &ctx()).is_err());

        // The same field lands fine on a Vec2 column, component-wise.
        let geometry = two_points_with(
            "offset",
            AttributeArray::Vec2(vec![Vec2(5.0, 5.0), Vec2(5.0, 5.0)]),
        );
        let spec = FieldApply::new(Domain::Point, "offset").with_combine(CombineMode::Add);
        let result = apply_field(&geometry, &spec, &Vec2Field, &ctx()).unwrap();
        assert_eq!(
            result.points().get("offset").unwrap().as_vec2("offset"),
            Ok(&[Vec2(6.0, 6.0), Vec2(6.0, 6.0)][..])
        );
    }
}
