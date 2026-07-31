// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lazy, batch-evaluated scalar fields and geometry attribute modulation.

use std::fmt;
use std::sync::{Arc, OnceLock};

use thiserror::Error;

use super::{AttributeArray, AttributeSet, AttributeType, Domain, Geometry, GeometryError, names};
use crate::eval::EvalContext;
use crate::id::DataTypeId;
use crate::param_curve::CurveParam;
use crate::types::{Color, NodeData, Vec2, Vec3, Vec4};

/// Everything a [`Field`] may read when it is evaluated.
///
/// Passed by reference so adding an input (simulation state, audio analysis,
/// three-dimensional positions) does not break every implementation. The
/// batch shape — one call per column, not per element — is also what lets a
/// field map onto a single WGSL function later.
#[derive(Clone, Copy)]
pub struct FieldSample<'a> {
    /// `P` of the domain being sampled. Defines the output length.
    pub positions: &'a [Vec2],
    /// Every attribute of that domain, so a field can read `index`, `id` or
    /// any user column instead of only geometry.
    pub attributes: &'a AttributeSet,
    pub ctx: &'a EvalContext,
}

impl<'a> FieldSample<'a> {
    pub fn new(positions: &'a [Vec2], attributes: &'a AttributeSet, ctx: &'a EvalContext) -> Self {
        Self {
            positions,
            attributes,
            ctx,
        }
    }

    /// A sample with no attributes, for fields known to read positions only.
    pub fn positions_only(positions: &'a [Vec2], ctx: &'a EvalContext) -> Self {
        static EMPTY: OnceLock<AttributeSet> = OnceLock::new();
        Self::new(positions, EMPTY.get_or_init(AttributeSet::new), ctx)
    }

    /// Number of elements the field must produce.
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }
}

/// A pure, batch-evaluated mapping from a geometry domain to attribute values.
pub trait Field: Send + Sync {
    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray;
}

/// A lazily evaluated field that can flow through node graph ports.
#[derive(Clone)]
pub struct FieldValue(pub Arc<dyn Field>);

impl FieldValue {
    pub fn new(field: impl Field + 'static) -> Self {
        Self(Arc::new(field))
    }

    pub fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        self.0.sample(input)
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

    fn byte_size(&self) -> u64 {
        // A field is a closure over its parameters, not sampled storage: the
        // implementations are small `Copy` structs (noise coefficients,
        // gradient stops) and [`Field`] exposes no size. The handle is the
        // honest answer; a field that ever grows to hold a sampled table has
        // to widen [`Field`] with its own accounting.
        size_of::<Self>() as u64
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
    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let positions = input.positions;
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
    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let positions = input.positions;
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

/// Scalar remapping of another field through a [`CurveParam`].
#[derive(Clone, Debug)]
pub struct CurveRemapField {
    pub source: FieldValue,
    /// The transfer curve. `Arc`-shared so cloning the field stays cheap.
    pub curve: Arc<CurveParam>,
}

impl CurveRemapField {
    pub fn new(source: FieldValue, curve: CurveParam) -> Self {
        Self {
            source,
            curve: Arc::new(curve),
        }
    }
}

impl Field for CurveRemapField {
    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let positions = input.positions;
        let values = scalar_values(self.source.sample(input), positions.len())
            .into_iter()
            .map(|value| self.curve.evaluate(value))
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
    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let positions = input.positions;
        AttributeArray::F32(vec![self.default; positions.len()])
    }
}

/// Reads one component of an attribute on the domain being sampled.
///
/// This is what lets modulation be driven by something other than position —
/// `index` for stagger, `id` for stable per-element randomness, or any column
/// an upstream node wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct AttributeField {
    /// Attribute to read from the sampled domain.
    pub name: String,
    /// Component index for multi-component attributes (`x`/`r` is 0).
    pub component: usize,
    /// Rescale the column's own `[min, max]` onto `[0, 1]`.
    pub normalize: bool,
    /// Value used when the attribute is missing, unreadable, or the wrong length.
    pub default: f32,
}

impl AttributeField {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            component: 0,
            normalize: false,
            default: 0.0,
        }
    }

    /// Select a component by name (`"x"`, `"y"`, `"z"`, `"w"` or `"r"`, `"g"`,
    /// `"b"`, `"a"`). Anything else selects the first component.
    pub fn with_component(mut self, spec: &str) -> Self {
        self.component = match spec.chars().next().map(|c| c.to_ascii_lowercase()) {
            Some('y') | Some('g') => 1,
            Some('z') | Some('b') => 2,
            Some('w') | Some('a') => 3,
            _ => 0,
        };
        self
    }

    pub fn with_normalize(mut self, normalize: bool) -> Self {
        self.normalize = normalize;
        self
    }

    pub fn with_default(mut self, default: f32) -> Self {
        self.default = default;
        self
    }
}

impl Field for AttributeField {
    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let length = input.len();
        let fallback = || AttributeArray::F32(vec![self.default; length]);

        // A name that does not resolve is a warning, not a failure: the node
        // editor must not go red while the name is half-typed.
        let Some(column) = input.attributes.get(self.name.as_str()) else {
            tracing::warn!(
                attribute = self.name,
                "field.attribute: unknown attribute; using the default value"
            );
            return fallback();
        };
        if column.len() != length {
            tracing::warn!(
                attribute = self.name,
                expected = length,
                actual = column.len(),
                "field.attribute: attribute has the wrong length; using the default value"
            );
            return fallback();
        }

        // A `Str` column, or a component the column does not have, is the same
        // kind of misconfiguration as an unknown name: warn once and fall back
        // rather than quietly reading zero.
        let Some(arity) = readable_arity(column.attr_type()) else {
            tracing::warn!(
                attribute = self.name,
                attr_type = ?column.attr_type(),
                "field.attribute: attribute is not numeric; using the default value"
            );
            return fallback();
        };
        if self.component >= arity {
            tracing::warn!(
                attribute = self.name,
                attr_type = ?column.attr_type(),
                component = self.component,
                "field.attribute: attribute has no such component; using the default value"
            );
            return fallback();
        }

        let mut values: Vec<f32> = (0..length)
            .map(|index| readable_component(column.as_ref(), index, self.component))
            .collect();

        if self.normalize && !normalize_in_place(&mut values) {
            tracing::warn!(
                attribute = self.name,
                "field.attribute: cannot normalize a column holding NaN or infinity; \
                 using the default value"
            );
            return fallback();
        }
        AttributeArray::F32(values)
    }
}

/// Number of components [`AttributeField`] can read from a column, or `None`
/// when the column is not numeric.
///
/// Wider than [`component_arity`]: field modulation cannot write `I32` or
/// `Bool` targets, but it can perfectly well be *driven* by them — `index` is
/// an integer column and a group flag is a Bool one.
fn readable_arity(attr_type: AttributeType) -> Option<usize> {
    match attr_type {
        AttributeType::F32 | AttributeType::I32 | AttributeType::Bool => Some(1),
        AttributeType::Vec2 => Some(2),
        AttributeType::Vec3 => Some(3),
        AttributeType::Vec4 | AttributeType::Color => Some(4),
        AttributeType::Str => None,
    }
}

/// One component of an attribute as a scalar. The caller has already checked
/// the column against [`readable_arity`], so `component` is in range.
///
/// Matched exhaustively on purpose: a new [`AttributeArray`] variant must fail
/// to compile here rather than silently fall into a catch-all.
fn readable_component(column: &AttributeArray, index: usize, component: usize) -> f32 {
    match column {
        AttributeArray::F32(values) => values[index],
        AttributeArray::I32(values) => values[index] as f32,
        AttributeArray::Bool(values) => {
            if values[index] {
                1.0
            } else {
                0.0
            }
        }
        AttributeArray::Vec2(_)
        | AttributeArray::Vec3(_)
        | AttributeArray::Vec4(_)
        | AttributeArray::Color(_) => sampled_component(column, index, component),
        // Unreachable: `readable_arity` rejects `Str` before we get here.
        AttributeArray::Str(_) => 0.0,
    }
}

/// Rescale `values` from their own range onto `[0, 1]`, reporting whether it
/// was possible.
///
/// A column with no spread (one element, or all values equal) maps to `0.0`:
/// there is no meaningful position within a range of zero width, and `0.0`
/// keeps a single-element geometry from producing NaN.
///
/// Returns `false` for a column holding NaN or an infinity. `f32::min`/`max`
/// step over NaN, so such a column would otherwise produce a finite span and
/// carry the NaN straight through the `[0, 1]` contract; an infinity makes the
/// span non-finite and would silently flatten every element to zero. Neither
/// is a rescaling, so the caller falls back instead.
fn normalize_in_place(values: &mut [f32]) -> bool {
    if !values.iter().all(|value| value.is_finite()) {
        return false;
    }
    let (min, max) = values
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), value| {
            (min.min(*value), max.max(*value))
        });
    let span = max - min;
    if span <= 0.0 {
        values.fill(0.0);
        return true;
    }
    for value in values {
        *value = (*value - min) / span;
    }
    true
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
            fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
                let positions = input.positions;
                let left = scalar_values(self.left.sample(input), positions.len());
                let right = scalar_values(self.right.sample(input), positions.len());
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
    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let positions = input.positions;
        let left = scalar_values(self.left.sample(input), positions.len());
        let right = scalar_values(self.right.sample(input), positions.len());
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

/// Number of scalar components a modulatable attribute type carries.
///
/// `None` for the types field modulation does not support (`I32`, `Bool`,
/// `Str`), which doubles as the check that rejects them.
fn component_arity(attr_type: AttributeType) -> Option<usize> {
    match attr_type {
        AttributeType::F32 => Some(1),
        AttributeType::Vec2 => Some(2),
        AttributeType::Vec3 => Some(3),
        AttributeType::Vec4 | AttributeType::Color => Some(4),
        AttributeType::I32 | AttributeType::Bool | AttributeType::Str => None,
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
///
/// Modulation itself is dimension-agnostic — `P` is never rewritten unless it
/// is the target column, and a `Vec3` target combines component-wise like any
/// other. The built-in fields are planar (2D simplex, 2D falloff), so a 3D
/// geometry samples them at the xy projection of `P`; three-dimensional field
/// domains are a later unit, and [`FieldSample`] takes its input by reference
/// so growing it will not break existing implementations.
pub fn apply_field(
    geometry: &Geometry,
    spec: &FieldApply<'_>,
    field: &dyn Field,
    ctx: &EvalContext,
) -> Result<Geometry, FieldError> {
    let attributes = geometry.attribute_set(spec.domain);
    let sample_positions = geometry
        .positions(spec.domain)
        .ok_or_else(|| GeometryError::AttributeNotFound {
            name: names::P.into(),
        })??
        .projected();
    let positions = sample_positions.as_ref();
    let existing = attributes
        .get(spec.target)
        .ok_or_else(|| GeometryError::AttributeNotFound {
            name: spec.target.into(),
        })?;
    // The field sees the whole domain, not just `P`, so `field.attribute` can
    // drive modulation from `index` or any other column.
    let sampled = field.sample(&FieldSample::new(positions, attributes, ctx));
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
    // Reject unmodulatable columns before anything else, so a misconfigured
    // graph reports the same error whatever `amount` happens to be.
    let arity = component_arity(existing.attr_type())
        .ok_or_else(|| FieldError::UnsupportedAttributeType(existing.attr_type()))?;

    // "No modulation" has to be exact, not merely arithmetically neutral:
    // interpolating by zero would still evaluate the combine op first, and
    // that turns `-0.0` into `+0.0` and an overflowing intermediate into
    // `inf * 0 = NaN`. Return the column untouched instead.
    if amount == 0.0 {
        return Ok(existing.clone());
    }
    let mask = spec.components.resolved_for(arity, spec.target);

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
        fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
            let positions = input.positions;
            AttributeArray::F32(vec![self.0; positions.len()])
        }
    }

    struct XField;

    impl Field for XField {
        fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
            let positions = input.positions;
            AttributeArray::F32(positions.iter().map(|position| position.0).collect())
        }
    }

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    fn scalar_sample(field: &dyn Field, positions: &[Vec2]) -> Vec<f32> {
        field
            .sample(&FieldSample::positions_only(positions, &ctx()))
            .as_f32("sample")
            .unwrap()
            .to_vec()
    }

    /// Sample a field against a whole attribute set, the way `apply_field` does.
    fn sample_with(field: &dyn Field, attributes: &AttributeSet) -> Vec<f32> {
        let positions = attributes.get(names::P).unwrap().as_vec2(names::P).unwrap();
        field
            .sample(&FieldSample::new(positions, attributes, &ctx()))
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
            CurveParam::linear([(1.0, 10.0), (0.0, 0.0), (0.5, 2.0)]),
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

    /// `field.apply` writes a column and never touches topology, so a mesh
    /// passes through with its triangles intact.
    #[test]
    fn apply_field_passes_meshes_through_untouched() {
        let mut geometry = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(2.0, 0.0),
            Vec2(2.0, 2.0),
            Vec2(0.0, 2.0),
        ]);
        geometry
            .points_mut()
            .insert("weight", AttributeArray::F32(vec![2.0; 4]))
            .unwrap();
        geometry.push_mesh(0..4, &[0, 1, 2, 0, 2, 3]);

        let spec = FieldApply::new(Domain::Point, "weight");
        let result = apply_field(&geometry, &spec, &ConstantField(1.0), &ctx()).unwrap();

        assert_eq!(result.validate(), Ok(()));
        assert_eq!(result.primitives(), geometry.primitives());
        assert_eq!(result.indices(), &[0, 1, 2, 0, 2, 3]);
        assert_eq!(
            result.points().get("weight").unwrap().as_f32("weight"),
            Ok(&[1.0; 4][..])
        );
    }

    // ---- field.attribute ---------------------------------------------------

    /// Four points carrying the columns `scatter` would have written.
    fn scattered_attributes() -> AttributeSet {
        let mut set = AttributeSet::new();
        set.insert(names::INDEX, AttributeArray::I32(vec![0, 1, 2, 3]))
            .unwrap();
        set.insert(
            names::P,
            AttributeArray::Vec2(vec![
                Vec2(0.0, 10.0),
                Vec2(1.0, 20.0),
                Vec2(2.0, 30.0),
                Vec2(3.0, 40.0),
            ]),
        )
        .unwrap();
        set
    }

    #[test]
    fn attribute_field_reads_an_integer_column() {
        let attributes = scattered_attributes();
        let field = AttributeField::new(names::INDEX);

        assert_eq!(sample_with(&field, &attributes), vec![0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn attribute_field_normalizes_onto_zero_to_one() {
        let attributes = scattered_attributes();
        let field = AttributeField::new(names::INDEX).with_normalize(true);

        // The first element sits at 0 and the last at 1; this is the ramp
        // stagger drives from.
        assert_eq!(
            sample_with(&field, &attributes),
            vec![0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0]
        );
    }

    #[test]
    fn attribute_field_selects_a_component() {
        let attributes = scattered_attributes();
        let field = AttributeField::new(names::P).with_component("y");

        assert_eq!(
            sample_with(&field, &attributes),
            vec![10.0, 20.0, 30.0, 40.0]
        );
        // Without a component it reads x.
        let field = AttributeField::new(names::P);
        assert_eq!(sample_with(&field, &attributes), vec![0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn attribute_field_falls_back_instead_of_failing() {
        let attributes = scattered_attributes();

        // A half-typed name must not turn the graph red.
        let field = AttributeField::new("ind").with_default(7.0);
        assert_eq!(sample_with(&field, &attributes), vec![7.0; 4]);

        // Neither must a non-numeric column.
        let mut attributes = scattered_attributes();
        attributes
            .insert(
                "label",
                AttributeArray::Str(vec![
                    "a".to_owned(),
                    "b".to_owned(),
                    "c".to_owned(),
                    "d".to_owned(),
                ]),
            )
            .unwrap();
        let field = AttributeField::new("label").with_default(-1.0);
        assert_eq!(sample_with(&field, &attributes), vec![-1.0; 4]);
    }

    #[test]
    fn attribute_field_reads_bool_columns_as_zero_or_one() {
        let mut attributes = scattered_attributes();
        attributes
            .insert("mask", AttributeArray::Bool(vec![true, false, true, false]))
            .unwrap();
        let field = AttributeField::new("mask");

        assert_eq!(sample_with(&field, &attributes), vec![1.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn attribute_field_rejects_a_component_the_column_lacks() {
        // `Vec2` has no `z`. Reading zero here would be a silent wrong answer,
        // so it falls back the same way an unknown name does.
        let attributes = scattered_attributes();
        let field = AttributeField::new(names::P)
            .with_component("z")
            .with_default(9.0);

        assert_eq!(sample_with(&field, &attributes), vec![9.0; 4]);

        // A component the column does have still reads normally.
        let field = AttributeField::new(names::P)
            .with_component("y")
            .with_default(9.0);
        assert_eq!(
            sample_with(&field, &attributes),
            vec![10.0, 20.0, 30.0, 40.0]
        );

        // A scalar column has only `x`, so asking for `y` falls back too
        // rather than silently ignoring the component.
        let field = AttributeField::new(names::INDEX)
            .with_component("y")
            .with_default(9.0);
        assert_eq!(sample_with(&field, &attributes), vec![9.0; 4]);
        let field = AttributeField::new(names::INDEX).with_default(9.0);
        assert_eq!(sample_with(&field, &attributes), vec![0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn normalizing_a_non_finite_column_falls_back() {
        // `f32::min`/`max` step over NaN, so a NaN column would otherwise
        // produce a finite span and carry the NaN through; an infinity would
        // flatten everything to zero. Both fall back instead.
        for poison in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut attributes = scattered_attributes();
            attributes
                .insert("weird", AttributeArray::F32(vec![0.0, poison, 1.0, 2.0]))
                .unwrap();
            let field = AttributeField::new("weird")
                .with_normalize(true)
                .with_default(-3.0);

            assert_eq!(
                sample_with(&field, &attributes),
                vec![-3.0; 4],
                "{poison} must not survive normalization"
            );
        }

        // Without `normalize` the raw column passes through untouched.
        let mut attributes = scattered_attributes();
        attributes
            .insert(
                "weird",
                AttributeArray::F32(vec![0.0, f32::INFINITY, 1.0, 2.0]),
            )
            .unwrap();
        let field = AttributeField::new("weird");
        assert_eq!(
            sample_with(&field, &attributes),
            vec![0.0, f32::INFINITY, 1.0, 2.0]
        );
    }

    #[test]
    fn normalizing_a_flat_column_yields_zero() {
        // No spread means no meaningful position within the range; dividing by
        // the zero-width span would produce NaN.
        let mut attributes = scattered_attributes();
        attributes
            .insert("flat", AttributeArray::F32(vec![5.0; 4]))
            .unwrap();
        let field = AttributeField::new("flat").with_normalize(true);

        assert_eq!(sample_with(&field, &attributes), vec![0.0; 4]);
    }

    #[test]
    fn attribute_field_drives_modulation_through_apply_field() {
        // The point of the unit: modulation driven by a column rather than by
        // position. A quantising or position-only field would give one value.
        let mut geometry = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(0.0, 0.0),
            Vec2(0.0, 0.0),
            Vec2(0.0, 0.0),
        ]);
        geometry
            .points_mut()
            .insert(names::INDEX, AttributeArray::I32(vec![0, 1, 2, 3]))
            .unwrap();
        geometry
            .points_mut()
            .insert(names::ROT, AttributeArray::F32(vec![0.0; 4]))
            .unwrap();

        let field = AttributeField::new(names::INDEX).with_normalize(true);
        let spec = FieldApply::new(Domain::Point, names::ROT).with_combine(CombineMode::Add);
        let result = apply_field(&geometry, &spec, &field, &ctx()).unwrap();

        // Every point shares a position, so only the index can separate them.
        assert_eq!(
            result.points().get(names::ROT).unwrap().as_f32(names::ROT),
            Ok(&[0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0][..])
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

    /// Modulation is dimension-agnostic: a 3D geometry keeps its `Vec3` `P`
    /// untouched, and the field still sees the xy of every point.
    #[test]
    fn modulation_passes_three_dimensional_positions_through() {
        let mut geometry = Geometry::from_points3(vec![Vec3(0.0, 5.0, 7.0), Vec3(2.0, 5.0, -7.0)]);
        geometry
            .points_mut()
            .insert("scale", AttributeArray::Vec2(vec![Vec2(3.0, 4.0); 2]))
            .unwrap();
        let spec = FieldApply::new(Domain::Point, "scale").with_combine(CombineMode::Multiply);

        let result = applied(&geometry, &spec);

        assert_eq!(
            result.points().get("scale").unwrap().as_vec2("scale"),
            Ok(&[Vec2(0.0, 0.0), Vec2(6.0, 8.0)][..]),
            "the field reads the same x it reads in 2D"
        );
        assert_eq!(
            result.points().get(names::P).unwrap().as_vec3(names::P),
            Ok(&[Vec3(0.0, 5.0, 7.0), Vec3(2.0, 5.0, -7.0)][..]),
            "P is not rewritten and keeps its dimension"
        );
    }

    /// A `Vec3` target combines component-wise like any other column, which is
    /// what lets a field move 3D positions.
    #[test]
    fn a_scalar_field_modulates_a_three_dimensional_position_column() {
        let geometry = Geometry::from_points3(vec![Vec3(0.0, 5.0, 7.0), Vec3(2.0, 5.0, 7.0)]);
        let spec = FieldApply::new(Domain::Point, names::P).with_combine(CombineMode::Add);

        let result = applied(&geometry, &spec);

        assert_eq!(
            result.points().get(names::P).unwrap().as_vec3(names::P),
            // The field is `x`: 0 adds nothing, 2 adds two to every component.
            Ok(&[Vec3(0.0, 5.0, 7.0), Vec3(4.0, 7.0, 9.0)][..])
        );
        assert_eq!(result.validate(), Ok(()));
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
    fn unmodulatable_columns_are_rejected_whatever_the_amount() {
        // The zero-amount short circuit must not turn a misconfigured graph
        // into a silent success.
        let columns = [
            ("flag", AttributeArray::Bool(vec![true, false])),
            ("count", AttributeArray::I32(vec![1, 2])),
            (
                "label",
                AttributeArray::Str(vec!["a".to_owned(), "b".to_owned()]),
            ),
        ];
        for (name, column) in columns {
            let geometry = two_points_with(name, column);
            for amount in [1.0, 0.5, 0.0, -1.0] {
                let spec = FieldApply::new(Domain::Point, name).with_amount(amount);
                assert!(
                    apply_field(&geometry, &spec, &XField, &ctx()).is_err(),
                    "{name} at amount {amount} must stay an error"
                );
            }
        }
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
            fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
                let positions = input.positions;
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
