// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lazy, batch-evaluated fields and geometry attribute modulation.

use std::fmt;
use std::sync::{Arc, OnceLock};

use thiserror::Error;

use super::{AttributeArray, AttributeSet, AttributeType, Domain, Geometry, GeometryError, names};
use crate::eval::EvalContext;
use crate::expression::{self, Component, ExpressionError, Program, Scope};
use crate::id::DataTypeId;
use crate::param_curve::CurveParam;
use crate::param_ramp::RampParam;
use crate::types::{Color, NodeData, Vec2, Vec3, Vec4, magnitude};

/// Everything a [`Field`] may read when it is evaluated.
///
/// Passed by reference so adding an input (simulation state, audio analysis,
/// three-dimensional positions) does not break every implementation. The
/// batch shape — one call per column, not per element — is also what lets a
/// field map onto a single WGSL function later.
#[derive(Clone, Copy)]
pub struct FieldSample<'a> {
    /// `P` of the domain being sampled. Defines the output length.
    ///
    /// **Planar, even when the geometry is not.** [`apply_field`] resolves
    /// this through `Positions::projected`, which borrows a `Vec2` column and
    /// materializes the `xy` of a `Vec3` one — so a field reading positions
    /// from here never sees the height of a 3D point cloud. That is why the
    /// accessor is documented as "planar-by-construction" rather than
    /// `require_planar`: dropping `z` is silent, and a field whose result
    /// would lose meaning must not take this route. A field that needs the
    /// real width of `P` reads it from [`FieldSample::attributes`], which
    /// carries the column unprojected.
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

    /// Approximate footprint of this field, in bytes, including what it owns
    /// on the heap and any field it wraps.
    ///
    /// Feeds [`FieldValue`]'s `NodeData::byte_size`, and through it the cache
    /// budget. **No default implementation**, for the same reason
    /// [`crate::types::NodeData::byte_size`] has none: a default of `0` is a
    /// silent under-count, and a field that carries a source expression or
    /// wraps another field is not free. Combinators recurse into their
    /// operands.
    fn byte_size(&self) -> u64;
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

    /// Approximate footprint of the field behind this handle.
    ///
    /// A combinator counts its operands through this, so a field tree reports
    /// the whole tree rather than one pointer.
    pub fn byte_size(&self) -> u64 {
        self.0.byte_size()
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
        size_of::<Self>() as u64 + self.0.byte_size()
    }
}

/// A field that answers the same value everywhere.
///
/// Its reason to exist in production is the **typed zero of a `FIELD` port**:
/// every other wire type has a natural empty value (an empty `Geometry`, a
/// transparent `FrameBuffer`, an empty string), but a field is a sampler, so
/// its zero has to be a sampler too. `FieldValue::new(ConstantField(0.0))` is
/// what an unconnected `FIELD` port evaluates to — see `zero_value` in
/// `ravel-nodes`. Answering with a `Scalar` instead would hand a value of the
/// wrong type to a node that had declared what it accepts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConstantField(pub f32);

impl Field for ConstantField {
    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        AttributeArray::F32(vec![self.0; input.len()])
    }

    fn byte_size(&self) -> u64 {
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
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64
    }

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

/// A unit vector from each sample position toward a fixed target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionToField {
    pub target: Vec2,
}

impl Field for DirectionToField {
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64
    }

    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        AttributeArray::Vec2(
            input
                .positions
                .iter()
                .map(|position| {
                    unit_vector(Vec2(self.target.0 - position.0, self.target.1 - position.1))
                })
                .collect(),
        )
    }
}

/// A divergence-free vector field made from the existing simplex noise field.
///
/// The scalar noise is treated as a stream function `N`; returning
/// `(∂N/∂y, -∂N/∂x)` makes the two terms cancel in the divergence. Both
/// derivatives use the same [`NoiseField`] rather than a second noise
/// implementation, so seed, octave and frequency semantics stay identical to
/// [`NoiseField`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurlNoiseField {
    pub noise: NoiseField,
    pub step: f32,
}

impl CurlNoiseField {
    pub fn new(noise: NoiseField, step: f32) -> Self {
        Self { noise, step }
    }
}

impl Field for CurlNoiseField {
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64
    }

    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let step = finite_difference_step(self.step);
        let values = input
            .positions
            .iter()
            .map(|position| {
                let samples = [
                    Vec2(position.0, position.1 + step),
                    Vec2(position.0, position.1 - step),
                    Vec2(position.0 + step, position.1),
                    Vec2(position.0 - step, position.1),
                ];
                let values = scalar_values(
                    self.noise
                        .sample(&FieldSample::new(&samples, input.attributes, input.ctx)),
                    samples.len(),
                );
                Vec2(
                    (values[0] - values[1]) / (2.0 * step),
                    -(values[2] - values[3]) / (2.0 * step),
                )
            })
            .collect();
        AttributeArray::Vec2(values)
    }
}

/// The finite-difference gradient of a scalar field.
#[derive(Clone, Debug)]
pub struct GradientField {
    pub source: FieldValue,
    pub step: f32,
}

impl GradientField {
    pub fn new(source: FieldValue, step: f32) -> Self {
        Self { source, step }
    }
}

impl Field for GradientField {
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64 + self.source.byte_size()
    }

    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let step = finite_difference_step(self.step);
        let x_plus: Vec<_> = input
            .positions
            .iter()
            .map(|position| Vec2(position.0 + step, position.1))
            .collect();
        let x_minus: Vec<_> = input
            .positions
            .iter()
            .map(|position| Vec2(position.0 - step, position.1))
            .collect();
        let y_plus: Vec<_> = input
            .positions
            .iter()
            .map(|position| Vec2(position.0, position.1 + step))
            .collect();
        let y_minus: Vec<_> = input
            .positions
            .iter()
            .map(|position| Vec2(position.0, position.1 - step))
            .collect();
        let x_plus_values = scalar_values(
            self.source
                .sample(&FieldSample::new(&x_plus, input.attributes, input.ctx)),
            input.len(),
        );
        let x_minus_values = scalar_values(
            self.source
                .sample(&FieldSample::new(&x_minus, input.attributes, input.ctx)),
            input.len(),
        );
        let y_plus_values = scalar_values(
            self.source
                .sample(&FieldSample::new(&y_plus, input.attributes, input.ctx)),
            input.len(),
        );
        let y_minus_values = scalar_values(
            self.source
                .sample(&FieldSample::new(&y_minus, input.attributes, input.ctx)),
            input.len(),
        );
        AttributeArray::Vec2(
            (0..input.len())
                .map(|index| {
                    Vec2(
                        (x_plus_values[index] - x_minus_values[index]) / (2.0 * step),
                        (y_plus_values[index] - y_minus_values[index]) / (2.0 * step),
                    )
                })
                .collect(),
        )
    }
}

/// A normalized vector from a center, either outward or tangent to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadialField {
    pub center: Vec2,
    pub tangent: bool,
}

impl Field for RadialField {
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64
    }

    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        AttributeArray::Vec2(
            input
                .positions
                .iter()
                .map(|position| {
                    let delta = Vec2(position.0 - self.center.0, position.1 - self.center.1);
                    let radial = unit_vector(delta);
                    if self.tangent {
                        Vec2(-radial.1, radial.0)
                    } else {
                        radial
                    }
                })
                .collect(),
        )
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
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64
    }

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
    fn byte_size(&self) -> u64 {
        // Recurses: a remap chain is as large as everything under it.
        size_of::<Self>() as u64
            + self.source.byte_size()
            + (size_of::<CurveParam>() + std::mem::size_of_val(self.curve.points())) as u64
    }

    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let positions = input.positions;
        let values = scalar_values(self.source.sample(input), positions.len())
            .into_iter()
            .map(|value| self.curve.evaluate(value))
            .collect();
        AttributeArray::F32(values)
    }
}

/// Colour lookup: another field's scalar, normalized, through a [`RampParam`].
///
/// The only field that produces a **colour from a number**. Everything else
/// that can answer `Color` (`field.attribute`) merely reads a colour column
/// that already existed, so without this a graph can darken `Cd` but cannot
/// change its hue.
///
/// `in_min` / `in_max` map the source field's own range onto the ramp's
/// `0..=1` domain before the lookup; the ramp itself clamps outside its end
/// stops.
#[derive(Clone, Debug)]
pub struct RampField {
    pub source: FieldValue,
    /// The colour ramp. `Arc`-shared so cloning the field stays cheap.
    pub ramp: Arc<RampParam>,
    /// Source value that maps to ramp position `0`.
    pub in_min: f32,
    /// Source value that maps to ramp position `1`.
    pub in_max: f32,
}

impl RampField {
    pub fn new(source: FieldValue, ramp: RampParam) -> Self {
        Self {
            source,
            ramp: Arc::new(ramp),
            in_min: 0.0,
            in_max: 1.0,
        }
    }

    /// Builder: set the input range normalized onto the ramp's domain.
    ///
    /// `in_max < in_min` is legal and reverses the ramp. `in_max == in_min`
    /// is the degenerate case: the normalization has no width, so the field
    /// becomes a hard step at that value — everything below reads the first
    /// stop, everything at or above the last. That mirrors what
    /// [`smooth_falloff`] does with a zero-width radius band rather than
    /// dividing by zero and sampling `NaN`.
    pub fn with_range(mut self, in_min: f32, in_max: f32) -> Self {
        self.in_min = in_min;
        self.in_max = in_max;
        self
    }

    /// Source value → ramp position.
    ///
    /// `in_min` / `in_max` are ordinary Float parameters, so a parameter port
    /// can drive them from a computed value and hand this a `NaN` or an
    /// infinity. A span that is not a finite positive width has no
    /// normalization to perform, so it degenerates to the same hard step a
    /// zero-width range takes rather than dividing into `NaN` and sampling the
    /// ramp's last colour everywhere.
    fn normalized(&self, value: f32) -> f32 {
        let span = self.in_max - self.in_min;
        if !span.is_finite() || span == 0.0 {
            return if value < self.in_min { 0.0 } else { 1.0 };
        }
        (value - self.in_min) / span
    }
}

impl Field for RampField {
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64
            + self.source.byte_size()
            + (size_of::<RampParam>() + std::mem::size_of_val(self.ramp.stops())) as u64
    }

    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let colors = scalar_values(self.source.sample(input), input.len())
            .into_iter()
            .map(|value| self.ramp.evaluate(self.normalized(value)))
            .collect();
        AttributeArray::Color(colors)
    }
}

/// Why a field expression cannot be evaluated.
/// Why a field expression is not evaluated at all.
///
/// Compilation is the only way a field expression can fail. Whether the
/// attributes it names actually exist is not decidable here — the geometry is
/// not known until the field is sampled — so an unbindable attribute is a
/// sample-time warning that reads `0.0`, not a reason to refuse the whole
/// expression. The enum keeps its shape so that a future binding failure which
/// *is* decidable at construction has somewhere to go.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FieldExpressionError {
    /// The source does not compile.
    #[error("{0}")]
    Compile(#[from] ExpressionError),
}

/// A field whose value is a scalar expression over position and the evaluation
/// context (REQ-CORE-015).
///
/// # What the expression may read
///
/// The vocabulary is [`Scope::field_context`] — `frame`, `time`, `fps`, the
/// two resolutions, `elem.count` — plus any attribute of the domain being
/// sampled: `@P.x`, `@index`, `@N.y`, `@Cd.r`, or a user column by name.
///
/// # Attributes are bound per batch, and only from the sampled domain
///
/// Which columns a geometry carries cannot be known while compiling, so every
/// `@name` is resolved once per [`Field::sample`] call and only the values
/// vary as the batch is walked.
///
/// **There is no promotion between domains, and none is expressible.**
/// [`FieldSample`] carries exactly one [`AttributeSet`] — the domain
/// [`apply_field`] is writing to — so a point expression has no route to a
/// primitive column. Naming one is naming an attribute that does not exist,
/// which is the case below.
///
/// A reference that cannot be bound — an unknown name, a `Str` column, a
/// column whose length does not match the batch, or a component the column
/// does not have — reads `0.0` and warns **once per sample**. Per element it
/// would be one line per point per frame; per sample it is one line an author
/// can act on.
///
/// # `@P`
///
/// Position is read from the domain's own `P` column, at its real width, so
/// `@P.z` is the height of a three-dimensional point cloud. Only when the
/// domain carries no usable `P` column does the planar
/// [`FieldSample::positions`] stand in for it.
///
/// A component the position column does not have reads `0.0` **without a
/// warning**, unlike every other attribute: on a two-dimensional domain zero
/// is the element's actual third coordinate, so `@P.z` there is an answer and
/// not a misconfiguration.
///
/// # Compiling once
///
/// The compiled program is built when the field is constructed, and sampling
/// only walks it. A field expression runs once per element per frame, so
/// parsing at sample time would be the entire cost.
#[derive(Clone, Debug)]
pub struct ExpressionField {
    source: String,
    /// Value used when there is no usable program: the source is empty, or it
    /// did not compile. A *result* that is not finite is passed through, since
    /// the language propagates IEEE and fields write into an `f32` column.
    default: f32,
    program: Result<Arc<CompiledFieldExpression>, Arc<FieldExpressionError>>,
}

/// A compiled field expression together with the component each of its
/// attribute references selects.
#[derive(Debug)]
struct CompiledFieldExpression {
    program: Program,
    /// The component each of `program.attribute_refs()` selects, in slot
    /// order, or `None` where the reference named none. Resolved once here so
    /// that binding a batch only has to look the columns up.
    ///
    /// `None` is kept distinct from `Some(Component::X)` because the two mean
    /// different things on a vector column: naming no component is what
    /// `check_components` rejects at compile time for a *declared* vector, and
    /// an undeclared one deserves the same warning at sample time.
    components: Vec<Option<Component>>,
}

impl ExpressionField {
    /// Compile `source` against the field vocabulary.
    pub fn new(source: impl Into<String>, default: f32) -> Self {
        let source = source.into();
        let program = compile_field_expression(&source)
            .map(Arc::new)
            .map_err(Arc::new);
        if let Err(error) = &program {
            // Once per field, not once per element or per frame: a broken
            // expression must be visible without flooding a playback log.
            tracing::warn!(%source, %error, "field expression is not evaluated");
        }
        Self {
            source,
            default,
            program,
        }
    }

    /// The source text, exactly as the author wrote it.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Value answered where the expression cannot be.
    pub fn default_value(&self) -> f32 {
        self.default
    }

    /// The compiled program, or `None` when the source cannot be evaluated.
    pub fn program(&self) -> Option<&Program> {
        self.program.as_ref().ok().map(|compiled| &compiled.program)
    }

    /// Why the source cannot be evaluated, if it cannot.
    pub fn error(&self) -> Option<&FieldExpressionError> {
        self.program.as_ref().err().map(|error| &**error)
    }

    /// Whether sampling this field answers differently as the frame moves
    /// (see [`Dependencies::references_time_axis`](crate::expression::Dependencies::references_time_axis)).
    ///
    /// **The node emitting this field must report the answer as its own time
    /// dependence.** A `FieldValue` is a lazy object: the same one is produced
    /// at every frame and only the *sample* varies, so nothing downstream can
    /// tell that `sin(time)` moves unless the emitting node says so and is
    /// therefore re-pulled per frame. Without it the evaluator caches the
    /// consumer under `TimeKey::TIMELESS` and the picture stops moving.
    ///
    /// A source that does not compile answers its constant default, so it is
    /// not time-varying.
    pub fn is_time_varying(&self) -> bool {
        self.program()
            .is_some_and(|program| program.dependencies().references_time_axis())
    }
}

impl PartialEq for ExpressionField {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source && self.default == other.default
    }
}

/// Compile, and record which component each attribute reference selects.
fn compile_field_expression(source: &str) -> Result<CompiledFieldExpression, FieldExpressionError> {
    let program = expression::compile(source, &Scope::field_context())?;
    let components = program
        .attribute_refs()
        .iter()
        // A suffix past the first selects from the scalar the first one
        // produced, so the first component is the whole binding.
        .map(|reference| reference.components.first().copied())
        .collect();
    Ok(CompiledFieldExpression {
        program,
        components,
    })
}

/// Where one `@attribute` reference reads its value for a batch.
///
/// Resolved once per [`Field::sample`] call rather than per element: the name
/// lookup, the shape checks and the warnings they raise all belong outside the
/// loop that runs once per point.
enum AttributeBinding<'a> {
    /// A component of the planar positions, for a domain that carries no
    /// usable `P` column of its own.
    Position(Component),
    /// A component of a resolved attribute column.
    Column {
        column: &'a AttributeArray,
        component: usize,
    },
    /// Nothing to read; every element sees `0.0`.
    Unbound,
}

impl AttributeBinding<'_> {
    fn read(&self, element: usize, positions: &[Vec2]) -> f64 {
        match self {
            Self::Position(component) => {
                let position = positions[element];
                match component {
                    Component::X => f64::from(position.0),
                    Component::Y => f64::from(position.1),
                    // A planar position has no third or fourth coordinate, and
                    // zero is what it would be if it did.
                    Component::Z | Component::W => 0.0,
                }
            }
            Self::Column { column, component } => column_component(column, element, *component),
            Self::Unbound => 0.0,
        }
    }
}

/// Resolve one attribute reference against the domain being sampled.
///
/// Every path that cannot answer the value the author asked for warns, except
/// the one where zero is the answer: a position component the domain does not
/// carry. Warning once here is what keeps a misconfigured expression visible
/// without putting a log line behind every element of every frame.
fn bind_attribute<'a>(
    name: &str,
    component: Option<Component>,
    input: &FieldSample<'a>,
) -> AttributeBinding<'a> {
    let is_position = name == names::P;
    let selected = component.unwrap_or(Component::X);
    // `P` always has an answer — the planar column the field is sampled
    // through. Any other name that cannot be bound reads zero.
    let fallback = || {
        if is_position {
            AttributeBinding::Position(selected)
        } else {
            AttributeBinding::Unbound
        }
    };

    let Some(column) = input.attributes.get(name) else {
        if !is_position {
            tracing::warn!(
                attribute = name,
                "field expression: unknown attribute; reading 0.0"
            );
        }
        return fallback();
    };
    if column.len() != input.positions.len() {
        tracing::warn!(
            attribute = name,
            expected = input.positions.len(),
            actual = column.len(),
            "field expression: attribute has the wrong length; reading 0.0"
        );
        return fallback();
    }
    let Some(arity) = readable_arity(column.attr_type()) else {
        tracing::warn!(
            attribute = name,
            attr_type = ?column.attr_type(),
            "field expression: attribute is not numeric; reading 0.0"
        );
        return fallback();
    };
    if component.is_none() && arity > 1 {
        // The compile-time counterpart of this is `MissingComponent`, which
        // only fires for an attribute the scope declares a width for. An
        // expression yields one number either way, so say which one it got.
        tracing::warn!(
            attribute = name,
            attr_type = ?column.attr_type(),
            "field expression: attribute is a vector; reading its `x` component"
        );
    }
    if selected.index() >= arity && !is_position {
        tracing::warn!(
            attribute = name,
            attr_type = ?column.attr_type(),
            component = selected.canonical_name(),
            "field expression: attribute has no such component; reading 0.0"
        );
        return AttributeBinding::Unbound;
    }
    AttributeBinding::Column {
        column,
        component: selected.index(),
    }
}

/// One component of an attribute column, as the `f64` the language evaluates
/// in.
///
/// Distinct from [`readable_component`], which answers `f32`: an `i32` column
/// such as `index` is exact in `f64` for every value it can hold, and would
/// start rounding past 2^24 on the way through `f32`.
fn column_component(column: &AttributeArray, index: usize, component: usize) -> f64 {
    match column {
        AttributeArray::F32(values) => f64::from(values[index]),
        AttributeArray::I32(values) => f64::from(values[index]),
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
        | AttributeArray::Color(_) => f64::from(sampled_component(column, index, component)),
        // Unreachable: `bind_attribute` refuses a `Str` column.
        AttributeArray::Str(_) => 0.0,
    }
}

impl Field for ExpressionField {
    fn byte_size(&self) -> u64 {
        let compiled = match &self.program {
            Ok(compiled) => {
                compiled.program.byte_size()
                    + (compiled.components.len() * size_of::<Option<Component>>()) as u64
            }
            Err(_) => size_of::<FieldExpressionError>() as u64,
        };
        size_of::<Self>() as u64 + self.source.len() as u64 + compiled
    }

    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let count = input.positions.len();
        let Ok(compiled) = &self.program else {
            return AttributeArray::F32(vec![self.default; count]);
        };
        let program = &compiled.program;
        if program.is_empty() {
            return AttributeArray::F32(vec![self.default; count]);
        }

        // Neither of these varies across the batch, so both are built once.
        let variables = expression::field_values(
            input.ctx.sample_frame(),
            input.ctx,
            expression::FieldContext {
                element_count: count,
            },
        );
        if compiled.components.is_empty() {
            // Nothing element-varying is read; one evaluation answers all of
            // them, and the result cannot depend on the order of the batch.
            let value = program.evaluate(&variables) as f32;
            return AttributeArray::F32(vec![value; count]);
        }

        // Once for the batch, before the per-element loop: this is where a
        // name is looked up and where an unbindable one is reported.
        let bindings: Vec<AttributeBinding<'_>> = program
            .attribute_refs()
            .iter()
            .zip(&compiled.components)
            .map(|(reference, component)| bind_attribute(&reference.name, *component, input))
            .collect();

        let mut attributes = vec![0.0f64; bindings.len()];
        let mut values = Vec::with_capacity(count);
        for element in 0..count {
            for (slot, binding) in bindings.iter().enumerate() {
                attributes[slot] = binding.read(element, input.positions);
            }
            values.push(program.evaluate_with(&variables, &attributes) as f32);
        }
        AttributeArray::F32(values)
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
        self.component = component_index(spec);
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
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64 + self.name.len() as u64
    }

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

/// Component index named by a selector such as `"x"` / `"z"` or `"r"` / `"b"`.
///
/// Positional, so both spellings of a slot agree with [`ComponentMask`].
/// Anything else selects the first component — a half-typed selector in the
/// node editor must not turn the graph red.
pub fn component_index(spec: &str) -> usize {
    match spec.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('y') | Some('g') => 1,
        Some('z') | Some('b') => 2,
        Some('w') | Some('a') => 3,
        _ => 0,
    }
}

/// How many components a field transform can read from a sampled column, or
/// `None` when there is nothing to transform.
///
/// A column whose length does not match the batch, or whose type is not
/// modulatable (`I32`, `Bool`, `Str`), has no vector in it. Both cases warn
/// **once per sample** and let the caller read zero, exactly as
/// [`combine_binary`] does: [`Field`] answers a column rather than a
/// `Result`, so a half-built graph must not take the evaluation down, and a
/// warning per element per frame is not something an author can act on.
fn transform_arity(sampled: &AttributeArray, length: usize, node: &str) -> Option<usize> {
    if sampled.len() != length {
        tracing::warn!(
            node,
            expected = length,
            actual = sampled.len(),
            "field transform: the source column has the wrong length; reading zero"
        );
        return None;
    }
    let arity = component_arity(sampled.attr_type());
    if arity.is_none() {
        tracing::warn!(
            node,
            attr_type = ?sampled.attr_type(),
            "field transform: the source is not a numeric field; reading zero"
        );
    }
    arity
}

/// The four components of element `index`, zero past the source's own arity.
///
/// The clamp is what keeps a scalar source from *broadcasting*:
/// [`sampled_component`] answers an `F32` column's value for every slot, which
/// is the promotion rule binary combination wants and the opposite of what a
/// transform wants — `field.angle` on a scalar field must read `y = 0`, not
/// `y = x`.
fn transform_components(sampled: &AttributeArray, arity: usize, index: usize) -> [f32; 4] {
    std::array::from_fn(|slot| {
        if slot < arity {
            sampled_component(sampled, index, slot)
        } else {
            0.0
        }
    })
}

/// `field.length`: the magnitude of a vector field, as a scalar field.
///
/// The zero vector answers `0`, which is its length and not a special case.
/// A scalar source answers `|value|`.
#[derive(Clone, Debug)]
pub struct LengthField {
    pub source: FieldValue,
}

impl LengthField {
    pub fn new(source: FieldValue) -> Self {
        Self { source }
    }
}

impl Field for LengthField {
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64 + self.source.byte_size()
    }

    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let length = input.len();
        let sampled = self.source.sample(input);
        let Some(arity) = transform_arity(&sampled, length, "field.length") else {
            return AttributeArray::F32(vec![0.0; length]);
        };
        AttributeArray::F32(
            (0..length)
                .map(|index| magnitude(arity, transform_components(&sampled, arity, index)))
                .collect(),
        )
    }
}

/// `field.component`: one component of a vector field, as a scalar field.
///
/// A component the source does not carry reads `0.0` and warns once per
/// sample, the way [`AttributeField`] reports a component its column lacks:
/// asking a Vec2 field for `z` is a misconfiguration, not a value.
#[derive(Clone, Debug)]
pub struct ComponentField {
    pub source: FieldValue,
    /// Component index (`x`/`r` is 0).
    pub component: usize,
}

impl ComponentField {
    pub fn new(source: FieldValue, component: usize) -> Self {
        Self { source, component }
    }
}

impl Field for ComponentField {
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64 + self.source.byte_size()
    }

    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let length = input.len();
        let sampled = self.source.sample(input);
        let zero = || AttributeArray::F32(vec![0.0; length]);
        let Some(arity) = transform_arity(&sampled, length, "field.component") else {
            return zero();
        };
        if self.component >= arity {
            tracing::warn!(
                component = self.component,
                attr_type = ?sampled.attr_type(),
                "field.component: the source field has no such component; reading zero"
            );
            return zero();
        }
        AttributeArray::F32(
            (0..length)
                .map(|index| sampled_component(&sampled, index, self.component))
                .collect(),
        )
    }
}

/// `field.compose`: two, three or four scalar fields into one vector field.
///
/// Each source contributes its own first component, so wiring a vector field
/// into a slot takes its `x` rather than failing — the same reading
/// [`AttributeField`] gives an unqualified column.
#[derive(Clone, Debug)]
pub struct ComposeField {
    /// One source per component, in `x`, `y`, `z`, `w` order.
    pub sources: Vec<FieldValue>,
}

impl ComposeField {
    pub fn new(sources: Vec<FieldValue>) -> Self {
        Self { sources }
    }
}

impl Field for ComposeField {
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64 + self.sources.iter().map(FieldValue::byte_size).sum::<u64>()
    }

    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let length = input.len();
        let columns: Vec<Vec<f32>> = self
            .sources
            .iter()
            .map(|source| {
                let sampled = source.sample(input);
                match transform_arity(&sampled, length, "field.compose") {
                    Some(_) => (0..length)
                        .map(|index| sampled_component(&sampled, index, 0))
                        .collect(),
                    None => vec![0.0; length],
                }
            })
            .collect();
        let value =
            |slot: usize, index: usize| columns.get(slot).map_or(0.0, |column| column[index]);
        // Only the three template arities are constructible; anything else
        // falls back to Vec2 so the field stays total. `Field::sample` cannot
        // report an error, so a caller that built this by hand with a length
        // outside 2..=4 silently loses components — the assert names that in
        // a debug build rather than leaving it to be discovered from a wrong
        // picture.
        debug_assert!(
            (2..=4).contains(&self.sources.len()),
            "field.compose takes 2, 3 or 4 sources, got {}",
            self.sources.len()
        );
        match self.sources.len() {
            3 => AttributeArray::Vec3(
                (0..length)
                    .map(|i| Vec3(value(0, i), value(1, i), value(2, i)))
                    .collect(),
            ),
            4 => AttributeArray::Vec4(
                (0..length)
                    .map(|i| Vec4(value(0, i), value(1, i), value(2, i), value(3, i)))
                    .collect(),
            ),
            _ => AttributeArray::Vec2(
                (0..length)
                    .map(|i| Vec2(value(0, i), value(1, i)))
                    .collect(),
            ),
        }
    }
}

/// `field.angle`: the direction of a vector field, as a scalar field of
/// radians in `-π..=π` (`atan2(y, x)`).
///
/// This is how a direction reaches `rot`, which is an F32 in radians and can
/// take no vector: `field.direction_to → field.angle → field.apply(rot)`.
///
/// **The zero vector answers `0`.** It has no direction, and `atan2(0, 0)` is
/// `0` in IEEE 754 — inventing an error for the point that happens to sit on
/// the centre of a radial field would fail the common case. A source with a
/// single component reads `y = 0`, so it answers `0` where the value is
/// positive and `π` where it is negative.
#[derive(Clone, Debug)]
pub struct AngleField {
    pub source: FieldValue,
}

impl AngleField {
    pub fn new(source: FieldValue) -> Self {
        Self { source }
    }
}

impl Field for AngleField {
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64 + self.source.byte_size()
    }

    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let length = input.len();
        let sampled = self.source.sample(input);
        let Some(arity) = transform_arity(&sampled, length, "field.angle") else {
            return AttributeArray::F32(vec![0.0; length]);
        };
        AttributeArray::F32(
            (0..length)
                .map(|index| {
                    let c = transform_components(&sampled, arity, index);
                    c[1].atan2(c[0])
                })
                .collect(),
        )
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
            fn byte_size(&self) -> u64 {
                size_of::<Self>() as u64 + self.left.byte_size() + self.right.byte_size()
            }

            fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
                combine_binary(&self.left, &self.right, input, $operation)
            }
        }
    };
}

binary_field!(AddField, |left, right| left + right);
binary_field!(MultiplyField, |left, right| left * right);
binary_field!(MaxField, |left, right| left.max(right));

/// Linear interpolation between two fields.
#[derive(Clone, Debug)]
pub struct BlendField {
    pub left: FieldValue,
    pub right: FieldValue,
    pub amount: f32,
}

impl Field for BlendField {
    fn byte_size(&self) -> u64 {
        size_of::<Self>() as u64 + self.left.byte_size() + self.right.byte_size()
    }

    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
        let amount = self.amount.clamp(0.0, 1.0);
        combine_binary(&self.left, &self.right, input, move |left, right| {
            left + (right - left) * amount
        })
    }
}

/// Sample both operands and combine them element-wise.
///
/// A pair with no resolvable type reads zero rather than failing: [`Field`]
/// answers a column, not a `Result`, and a half-built graph must not take the
/// evaluation down. The warning is raised once per sample, not once per
/// element, so a mistyped pair is visible without flooding a playback log.
fn combine_binary(
    left: &FieldValue,
    right: &FieldValue,
    input: &FieldSample<'_>,
    operation: impl Fn(f32, f32) -> f32,
) -> AttributeArray {
    let length = input.positions.len();
    let left = left.sample(input);
    let right = right.sample(input);
    combine_samples(&left, &right, length, operation).unwrap_or_else(|error| {
        tracing::warn!(%error, "field operands do not combine; reading zero");
        AttributeArray::F32(vec![0.0; length])
    })
}

/// Type a binary combination answers, or why the operands do not combine.
///
/// Two operands of one type combine component-wise, and a scalar broadcasts
/// into the other operand's type. Nothing else resolves: `Vec4` and `Color`
/// carry four components each but are not the same type, and pairing them
/// would be exactly the implicit conversion this model refuses (see
/// `vector-field-plan.md`).
fn binary_result_type(
    left: AttributeType,
    right: AttributeType,
) -> Result<AttributeType, FieldError> {
    let modulatable = |attr_type| component_arity(attr_type).is_some();
    match (left, right) {
        (left, right) if left == right && modulatable(left) => Ok(left),
        (AttributeType::F32, other) | (other, AttributeType::F32) if modulatable(other) => {
            Ok(other)
        }
        _ => Err(FieldError::IncompatibleOperands { left, right }),
    }
}

/// Combine two sampled columns element-wise and component-wise.
fn combine_samples(
    left: &AttributeArray,
    right: &AttributeArray,
    length: usize,
    operation: impl Fn(f32, f32) -> f32,
) -> Result<AttributeArray, FieldError> {
    let result_type = binary_result_type(left.attr_type(), right.attr_type())?;
    // An operand whose length does not match the batch reads zero, the way the
    // scalar-only path answered before a combination could carry a type.
    let component = |array: &AttributeArray, index: usize, slot: usize| {
        if array.len() == length {
            sampled_component(array, index, slot)
        } else {
            0.0
        }
    };
    // `sampled_component` broadcasts an `F32` operand across every slot, which
    // is the scalar-times-vector rule; a same-typed pair reads slot for slot.
    let value = |index: usize, slot: usize| {
        operation(component(left, index, slot), component(right, index, slot))
    };
    Ok(match result_type {
        AttributeType::F32 => AttributeArray::F32((0..length).map(|i| value(i, 0)).collect()),
        AttributeType::Vec2 => AttributeArray::Vec2(
            (0..length)
                .map(|i| Vec2(value(i, 0), value(i, 1)))
                .collect(),
        ),
        AttributeType::Vec3 => AttributeArray::Vec3(
            (0..length)
                .map(|i| Vec3(value(i, 0), value(i, 1), value(i, 2)))
                .collect(),
        ),
        AttributeType::Vec4 => AttributeArray::Vec4(
            (0..length)
                .map(|i| Vec4(value(i, 0), value(i, 1), value(i, 2), value(i, 3)))
                .collect(),
        ),
        AttributeType::Color => AttributeArray::Color(
            (0..length)
                .map(|i| Color {
                    r: value(i, 0),
                    g: value(i, 1),
                    b: value(i, 2),
                    a: value(i, 3),
                })
                .collect(),
        ),
        // `binary_result_type` only answers modulatable types.
        AttributeType::I32 | AttributeType::Bool | AttributeType::Str => {
            return Err(FieldError::IncompatibleOperands {
                left: left.attr_type(),
                right: right.attr_type(),
            });
        }
    })
}

/// Errors produced by [`apply_field`] and by combining two fields.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FieldError {
    #[error(transparent)]
    Geometry(#[from] GeometryError),
    #[error("field modulation does not support {0} attributes")]
    UnsupportedAttributeType(AttributeType),
    /// The two operands of a binary field are neither the same type nor a
    /// scalar to broadcast into the other.
    #[error("a {left} field does not combine with a {right} field")]
    IncompatibleOperands {
        left: AttributeType,
        right: AttributeType,
    },
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
        Self::UNSPECIFIED
    }
}

impl ComponentMask {
    /// Every component.
    pub const ALL: Self = Self(0b1111);

    /// "The target type decides": every component, except that a Color or
    /// Vec4 target keeps its alpha (`rgb`).
    ///
    /// A scalar field broadcasts into every selected component, so writing
    /// alpha too means darkening a color also makes it transparent — which is
    /// a side effect of brightness modulation, not a request for it. Say `a`
    /// (or `rgba`) to move alpha on purpose.
    pub const UNSPECIFIED: Self = Self(0);

    /// Parse a component specification such as `"xy"`, `"rgb"` or `"a"`.
    ///
    /// Unknown characters are ignored; a specification that selects nothing
    /// yields [`ComponentMask::UNSPECIFIED`] so a typo cannot silently turn
    /// the node into a no-op.
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
        Self(bits)
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
        if self == Self::UNSPECIFIED {
            // Arity 4 is Color or Vec4; see [`ComponentMask::UNSPECIFIED`].
            return if arity == 4 { Self(0b0111) } else { available };
        }
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
    /// Create the target column when the geometry does not have it, instead
    /// of failing with `AttributeNotFound`. On by default: `stroke_color` and
    /// `stroke_width` are attributes nothing writes until a user modulates
    /// them, so requiring an `attribute.set` in front of every `field.apply`
    /// would be a ceremony with no decision in it.
    pub create_if_missing: bool,
}

impl<'a> FieldApply<'a> {
    /// Modulate `target` on `domain`, replacing the value outright.
    pub fn new(domain: Domain, target: &'a str) -> Self {
        Self {
            domain,
            target,
            amount: 1.0,
            combine: CombineMode::Set,
            components: ComponentMask::UNSPECIFIED,
            group: "",
            create_if_missing: true,
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

    pub fn with_create_if_missing(mut self, create_if_missing: bool) -> Self {
        self.create_if_missing = create_if_missing;
        self
    }
}

/// The column [`apply_field`] invents for a target the geometry does not have.
///
/// A reserved standard attribute is created with the type and the semantic
/// default the geometry spec declares for it — an invented `Cd` has to be
/// white, not transparent black, or `combine = multiply` would blank the
/// geometry the first time anybody modulated it. Anything else takes the
/// field's own sampled type, zeroed.
fn created_column(target: &str, sampled: AttributeType, length: usize) -> AttributeArray {
    match target {
        names::CD | names::STROKE_COLOR => AttributeArray::Color(vec![Color::WHITE; length]),
        names::STROKE_WIDTH => AttributeArray::F32(vec![0.0; length]),
        // `fill` is declared Bool, and a reserved attribute takes its declared
        // type even when that makes the combine fail: `combine_arrays` then
        // reports the same unsupported-type error a geometry that already had
        // a `fill` column would produce. Inventing an F32 `fill` instead would
        // let `field.apply` report success while `rasterize`, which reads the
        // attribute as Bool, went on ignoring it.
        names::FILL => AttributeArray::Bool(vec![false; length]),
        _ => match sampled {
            AttributeType::Vec2 => AttributeArray::Vec2(vec![Vec2(0.0, 0.0); length]),
            AttributeType::Vec3 => AttributeArray::Vec3(vec![Vec3(0.0, 0.0, 0.0); length]),
            AttributeType::Vec4 => AttributeArray::Vec4(vec![Vec4(0.0, 0.0, 0.0, 0.0); length]),
            AttributeType::Color => AttributeArray::Color(vec![Color::TRANSPARENT; length]),
            // `I32` / `Bool` / `Str` are not modulatable: created here so
            // `combine_arrays` reports the type it always reported, rather
            // than this function inventing a second error for the same case.
            AttributeType::I32 => AttributeArray::I32(vec![0; length]),
            AttributeType::Bool => AttributeArray::Bool(vec![false; length]),
            AttributeType::Str => AttributeArray::Str(vec![String::new(); length]),
            AttributeType::F32 => AttributeArray::F32(vec![0.0; length]),
        },
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
    // The field sees the whole domain, not just `P`, so `field.attribute` can
    // drive modulation from `index` or any other column. Sampled before the
    // target column is resolved, because an invented column's type can come
    // from what the field returned.
    let sampled = field.sample(&FieldSample::new(positions, attributes, ctx));
    if sampled.len() != positions.len() {
        return Err(GeometryError::LengthMismatch {
            name: spec.target.into(),
            expected: positions.len(),
            actual: sampled.len(),
        }
        .into());
    }
    let created;
    let existing = match attributes.get(spec.target) {
        Some(column) => &**column,
        None if spec.create_if_missing => {
            created = created_column(spec.target, sampled.attr_type(), positions.len());
            &created
        }
        None => {
            return Err(GeometryError::AttributeNotFound {
                name: spec.target.into(),
            }
            .into());
        }
    };
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
///
/// Shared with the attribute writes in [`ops`](super::ops) so that every node
/// taking a `group` parameter resolves it the same way.
pub(super) fn group_selection(
    attributes: &AttributeSet,
    group: &str,
    length: usize,
) -> Option<Vec<bool>> {
    if group.is_empty() {
        return None;
    }
    let Some(column) = attributes.get(group) else {
        tracing::warn!(group, "group attribute not found; affecting every element");
        return None;
    };
    let AttributeArray::Bool(values) = column.as_ref() else {
        tracing::warn!(
            group,
            attr_type = ?column.attr_type(),
            "group attribute is not Bool; affecting every element"
        );
        return None;
    };
    if values.len() != length {
        tracing::warn!(
            group,
            expected = length,
            actual = values.len(),
            "group attribute has the wrong length; affecting every element"
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

fn finite_difference_step(step: f32) -> f32 {
    if step.is_finite() && step > 0.0 {
        step
    } else {
        0.01
    }
}

fn unit_vector(value: Vec2) -> Vec2 {
    let length = value.0.hypot(value.1);
    if length > 0.0 && length.is_finite() {
        Vec2(value.0 / length, value.1 / length)
    } else {
        Vec2(0.0, 0.0)
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

    struct XField;

    #[test]
    fn field_byte_size_counts_the_whole_tree() {
        // A combinator must not report one pointer: `byte_size` has no
        // default precisely so a wrapping field cannot forget its operands.
        let expression = ExpressionField::new("x".repeat(4096), 0.0);
        let leaf = FieldValue::new(expression);
        assert!(leaf.byte_size() >= 4096);

        let blended = FieldValue::new(BlendField {
            left: leaf.clone(),
            right: leaf.clone(),
            amount: 0.5,
        });
        assert!(
            blended.byte_size() >= 2 * 4096,
            "a blend of two expression fields reported {} bytes",
            blended.byte_size()
        );

        let remapped = FieldValue::new(CurveRemapField::new(
            blended,
            CurveParam::linear([(0.0, 0.0), (1.0, 1.0)]),
        ));
        assert!(remapped.byte_size() >= 2 * 4096);
    }

    impl Field for XField {
        fn byte_size(&self) -> u64 {
            size_of::<Self>() as u64
        }

        fn sample(&self, input: &FieldSample<'_>) -> AttributeArray {
            let positions = input.positions;
            AttributeArray::F32(positions.iter().map(|position| position.0).collect())
        }
    }

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    /// A field answering a fixed column, so a test can give a combinator an
    /// operand of any type.
    struct ConstantArrayField(AttributeArray);

    impl Field for ConstantArrayField {
        fn byte_size(&self) -> u64 {
            size_of::<Self>() as u64
        }

        fn sample(&self, _input: &FieldSample<'_>) -> AttributeArray {
            self.0.clone()
        }
    }

    fn constant_array(array: AttributeArray) -> FieldValue {
        FieldValue::new(ConstantArrayField(array))
    }

    fn typed_sample(field: &dyn Field, positions: &[Vec2]) -> AttributeArray {
        field.sample(&FieldSample::positions_only(positions, &ctx()))
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

    // ---- expression fields (EXPR-5) ---------------------------------------

    #[test]
    fn a_position_expression_answers_per_element() {
        let positions = [Vec2(0.0, 0.0), Vec2(1.0, 2.0), Vec2(-3.5, 0.25)];
        let field = ExpressionField::new("@P.x * 2 + @P.y", -1.0);

        assert!(field.error().is_none(), "{:?}", field.error());
        assert_eq!(scalar_sample(&field, &positions), vec![0.0, 4.0, -6.75]);
    }

    #[test]
    fn a_field_expression_reads_the_context_and_the_element_count() {
        let positions = [Vec2(0.0, 0.0); 4];
        assert_eq!(
            scalar_sample(&ExpressionField::new("elem.count", 0.0), &positions),
            vec![4.0; 4]
        );
        assert_eq!(
            scalar_sample(&ExpressionField::new("res.width / 2", 0.0), &positions),
            vec![960.0; 4]
        );
    }

    #[test]
    fn an_expression_field_composes_with_the_other_fields() {
        let positions = [Vec2(1.0, 0.0), Vec2(3.0, 0.0)];
        let x = FieldValue::new(ExpressionField::new("@P.x", 0.0));
        let ten = FieldValue::new(ConstantField(10.0));

        assert_eq!(
            scalar_sample(
                &AddField {
                    left: x.clone(),
                    right: ten.clone()
                },
                &positions
            ),
            vec![11.0, 13.0]
        );
        assert_eq!(
            scalar_sample(
                &MultiplyField {
                    left: x.clone(),
                    right: ten.clone()
                },
                &positions
            ),
            vec![10.0, 30.0]
        );
        assert_eq!(
            scalar_sample(
                &MaxField {
                    left: x,
                    right: ten
                },
                &positions
            ),
            vec![10.0, 10.0]
        );
    }

    #[test]
    fn an_expression_field_does_not_depend_on_element_order() {
        let field = ExpressionField::new("noise(@P.x * 0.7, @P.y * 0.7) * 3 + @P.y", 0.0);
        let positions = [
            Vec2(0.0, 0.0),
            Vec2(1.5, -2.0),
            Vec2(-4.25, 8.0),
            Vec2(0.125, 0.5),
        ];
        let reversed: Vec<Vec2> = positions.iter().rev().copied().collect();

        let straight = scalar_sample(&field, &positions);
        let mut flipped = scalar_sample(&field, &reversed);
        flipped.reverse();

        assert_eq!(
            straight, flipped,
            "an element's value is its own position's"
        );
        // …and evaluating twice gives the same numbers (REQ-CORE-006 keys on it).
        assert_eq!(straight, scalar_sample(&field, &positions));
    }

    /// Sampling must not parse — the completion criterion of EXPR-5, and the
    /// whole reason a compiled form is kept at all.
    ///
    /// Counting calls into the compiler is the only evidence that shows it.
    /// Comparing the held program's address does not: a `sample` that parsed
    /// afresh for every element and threw the result away would leave the
    /// stored program untouched and pass.
    #[test]
    fn sampling_never_parses() {
        use crate::expression::compile_calls;

        let before = compile_calls();
        let field = ExpressionField::new("@P.x + 1", 0.0);
        let after_construction = compile_calls();
        assert_eq!(
            after_construction - before,
            1,
            "construction is what compiles, exactly once"
        );

        // 512 elements sampled four times. A parse per element, or even one
        // per `sample` call, would show up here; nothing does.
        let positions: Vec<Vec2> = (0..512).map(|index| Vec2(index as f32, 0.0)).collect();
        for _ in 0..4 {
            scalar_sample(&field, &positions);
        }

        assert_eq!(
            compile_calls(),
            after_construction,
            "sampling compiled something: the point of holding a compiled \
             program is that evaluation never parses"
        );
    }

    /// The same guarantee on the other path: a broken source must not be
    /// retried on every sample either.
    #[test]
    fn sampling_a_broken_expression_never_parses_either() {
        use crate::expression::compile_calls;

        let field = ExpressionField::new("@P.x +", 1.0);
        assert!(field.error().is_some());

        let after_construction = compile_calls();
        scalar_sample(&field, &[Vec2(0.0, 0.0); 8]);
        assert_eq!(compile_calls(), after_construction);
    }

    /// `@P.z` is zero on a 2D domain, where zero is the element's actual third
    /// coordinate. Pinned because the spelling is persisted and §9 of the
    /// specification only permits growing the language in the invalid → valid
    /// direction — EXPR-6 may bind more attributes, but it may not quietly
    /// give `@P.z` a different meaning.
    #[test]
    fn the_third_position_component_is_zero_on_a_two_dimensional_domain() {
        let positions = [Vec2(3.0, 4.0), Vec2(-1.5, 0.0)];

        assert_eq!(
            scalar_sample(&ExpressionField::new("@P.z", -1.0), &positions),
            vec![0.0, 0.0]
        );
        // …and it composes as a plain zero rather than poisoning the result.
        assert_eq!(
            scalar_sample(&ExpressionField::new("@P.z + @P.x", -1.0), &positions),
            vec![3.0, -1.5]
        );
        // `b` is the same component under the colour spelling.
        assert_eq!(
            scalar_sample(&ExpressionField::new("@P.b + @P.y", -1.0), &positions),
            vec![4.0, 0.0]
        );
    }

    /// The counterpart to the 2D case, and a **deliberate** change of answer:
    /// a 3D point cloud now reads its real height. `apply_field` still
    /// projects `P` onto `xy` for [`FieldSample::positions`], but the
    /// expression binds `@P` from the domain's own column, which is
    /// unprojected. The specification has always defined `@P.z` as the
    /// position's third coordinate (§8); what changed is that the
    /// implementation reaches it.
    #[test]
    fn the_third_position_component_is_the_height_of_three_dimensional_geometry() {
        let mut geometry = Geometry::from_points3(vec![Vec3(1.0, 2.0, 30.0), Vec3(4.0, 5.0, 60.0)]);
        geometry
            .points_mut()
            .insert("weight", AttributeArray::F32(vec![0.0, 0.0]))
            .unwrap();

        let spec = FieldApply::new(Domain::Point, "weight");
        let applied = apply_field(
            &geometry,
            &spec,
            &ExpressionField::new("@P.z", -1.0),
            &ctx(),
        )
        .expect("apply");

        assert_eq!(
            applied
                .points()
                .get("weight")
                .unwrap()
                .as_f32("weight")
                .unwrap(),
            &[30.0, 60.0],
            "the unprojected `P` column carries the height a field expression reads"
        );
        assert_eq!(
            applied.points().get(names::P).unwrap().attr_type(),
            AttributeType::Vec3
        );
    }

    #[test]
    fn standard_attributes_bind_to_the_sampled_domain() {
        let mut set = scattered_attributes();
        set.insert(
            names::N,
            AttributeArray::Vec3(vec![
                Vec3(0.0, 1.0, 0.0),
                Vec3(0.0, 2.0, 0.0),
                Vec3(0.0, 3.0, 0.0),
                Vec3(0.0, 4.0, 0.0),
            ]),
        )
        .unwrap();
        set.insert(
            names::CD,
            AttributeArray::Color(vec![
                Color::new(0.25, 0.0, 0.0, 1.0),
                Color::new(0.5, 0.0, 0.0, 1.0),
                Color::new(0.75, 0.0, 0.0, 1.0),
                Color::new(1.0, 0.0, 0.0, 1.0),
            ]),
        )
        .unwrap();

        assert_eq!(
            sample_with(&ExpressionField::new("@index", -1.0), &set),
            vec![0.0, 1.0, 2.0, 3.0]
        );
        assert_eq!(
            sample_with(&ExpressionField::new("@N.y", -1.0), &set),
            vec![1.0, 2.0, 3.0, 4.0]
        );
        assert_eq!(
            sample_with(&ExpressionField::new("@Cd.r", -1.0), &set),
            vec![0.25, 0.5, 0.75, 1.0]
        );
        // Attributes compose with the context vocabulary and with each other.
        assert_eq!(
            sample_with(&ExpressionField::new("@P.x + @index * 10", -1.0), &set),
            vec![0.0, 11.0, 22.0, 33.0]
        );
    }

    #[test]
    fn a_user_named_attribute_binds_like_a_standard_one() {
        let mut set = scattered_attributes();
        set.insert("weight", AttributeArray::F32(vec![0.5, 1.5, 2.5, 3.5]))
            .unwrap();

        assert_eq!(
            sample_with(&ExpressionField::new("@weight * 2", -1.0), &set),
            vec![1.0, 3.0, 5.0, 7.0]
        );
    }

    /// An integer column reaches the expression through `f64`, which holds
    /// every `i32` exactly. Routed through `f32` this value would come back as
    /// 16777216.
    #[test]
    fn an_integer_attribute_converts_without_rounding() {
        let mut set = scattered_attributes();
        set.insert(
            "id",
            AttributeArray::I32(vec![16_777_217, 0, -16_777_217, 1]),
        )
        .unwrap();

        assert_eq!(
            sample_with(&ExpressionField::new("@id == 16777217", 0.0), &set),
            vec![1.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn a_boolean_attribute_reads_as_one_or_zero() {
        let mut set = scattered_attributes();
        set.insert(
            "selected",
            AttributeArray::Bool(vec![true, false, true, false]),
        )
        .unwrap();

        assert_eq!(
            sample_with(&ExpressionField::new("@selected", -1.0), &set),
            vec![1.0, 0.0, 1.0, 0.0]
        );
    }

    /// An attribute that cannot be bound reads `0.0` — the expression still
    /// evaluates, so the rest of it keeps working and the author sees the term
    /// drop out rather than the whole field falling to its default.
    #[test]
    fn an_unbindable_attribute_reads_zero_and_the_expression_still_evaluates() {
        let mut set = scattered_attributes();
        set.insert(
            "label",
            AttributeArray::Str(vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
            ]),
        )
        .unwrap();
        set.insert("weight", AttributeArray::F32(vec![0.0; 4]))
            .unwrap();

        // Unknown name, non-numeric column, and a component the column has
        // not got — all three read zero and leave `@P.x` intact. The third
        // only reaches sampling because `weight` is not a declared attribute:
        // `@index.y` is an `InvalidComponent` at compile time, since the scope
        // knows that width.
        for source in ["@missing + @P.x", "@label + @P.x", "@weight.y + @P.x"] {
            assert_eq!(
                sample_with(&ExpressionField::new(source, 99.0), &set),
                vec![0.0, 1.0, 2.0, 3.0],
                "`{source}`"
            );
        }
    }

    /// A column whose length does not match the batch is a shape error, not a
    /// value: reading it per element would index out of bounds.
    #[test]
    fn an_attribute_of_the_wrong_length_reads_zero() {
        let mut set = AttributeSet::new();
        set.insert(
            names::P,
            AttributeArray::Vec2(vec![Vec2(1.0, 0.0), Vec2(2.0, 0.0)]),
        )
        .unwrap();
        // `AttributeSet::insert` guards the length, so the mismatch has to be
        // built by sampling a set against a shorter batch.
        let positions = [Vec2(1.0, 0.0)];
        let sampled = ExpressionField::new("@P.y + @index", -1.0).sample(&FieldSample::new(
            &positions,
            &set,
            &ctx(),
        ));

        assert_eq!(sampled.as_f32("sample").unwrap(), &[0.0]);
    }

    /// Only the domain being sampled is visible. A point expression naming a
    /// primitive column is naming an attribute that does not exist, because
    /// [`FieldSample`] carries one [`AttributeSet`] and there is no promotion
    /// rule that could reach the other.
    #[test]
    fn an_attribute_of_another_domain_is_not_visible() {
        let mut geometry = Geometry::from_points(vec![Vec2(1.0, 0.0), Vec2(2.0, 0.0)]);
        geometry
            .points_mut()
            .insert("weight", AttributeArray::F32(vec![0.0, 0.0]))
            .unwrap();
        geometry
            .detail_mut()
            .insert("material", AttributeArray::F32(vec![7.0]))
            .unwrap();

        let spec = FieldApply::new(Domain::Point, "weight");
        let applied = apply_field(
            &geometry,
            &spec,
            &ExpressionField::new("@material", -1.0),
            &ctx(),
        )
        .expect("apply");

        assert_eq!(
            applied
                .points()
                .get("weight")
                .unwrap()
                .as_f32("weight")
                .unwrap(),
            &[0.0, 0.0]
        );
    }

    /// An undeclared vector named without a component cannot be caught while
    /// compiling — the scope knows no width for it — so sampling picks `x` and
    /// warns rather than refusing an expression that already compiled.
    #[test]
    fn a_vector_attribute_without_a_component_reads_its_first() {
        let mut set = scattered_attributes();
        set.insert(
            "uv",
            AttributeArray::Vec2(vec![
                Vec2(5.0, 0.0),
                Vec2(6.0, 0.0),
                Vec2(7.0, 0.0),
                Vec2(8.0, 0.0),
            ]),
        )
        .unwrap();

        assert_eq!(
            sample_with(&ExpressionField::new("@uv", -1.0), &set),
            vec![5.0, 6.0, 7.0, 8.0]
        );
    }

    /// Without a `P` column the planar positions stand in, which is what a
    /// field sampled through [`FieldSample::positions_only`] relies on.
    #[test]
    fn position_falls_back_to_the_planar_column() {
        let positions = [Vec2(3.0, 4.0), Vec2(-1.5, 2.0)];

        assert_eq!(
            scalar_sample(&ExpressionField::new("@P.x + @P.y", -1.0), &positions),
            vec![7.0, 0.5]
        );
    }

    #[test]
    fn a_field_expression_that_does_not_compile_answers_the_default() {
        for source in ["@P.x +", "unknown", "@P", "min(1)"] {
            let field = ExpressionField::new(source, 2.5);
            assert!(field.error().is_some(), "`{source}` must report why");
            assert_eq!(scalar_sample(&field, &[Vec2(0.0, 0.0); 2]), vec![2.5; 2]);
        }
    }

    #[test]
    fn an_empty_field_expression_answers_the_default() {
        let field = ExpressionField::new("  ", 3.25);
        assert!(field.error().is_none(), "a blank box is not an error");
        assert_eq!(scalar_sample(&field, &[Vec2(0.0, 0.0); 2]), vec![3.25; 2]);
    }

    #[test]
    fn an_empty_batch_yields_an_empty_column() {
        assert!(scalar_sample(&ExpressionField::new("@P.x", 0.0), &[]).is_empty());
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
    fn vector_operands_combine_component_wise() {
        let positions = [Vec2(0.0, 0.0); 2];
        let left = constant_array(AttributeArray::Vec2(vec![Vec2(1.0, 2.0), Vec2(3.0, 4.0)]));
        let right = constant_array(AttributeArray::Vec2(vec![
            Vec2(10.0, 20.0),
            Vec2(-30.0, 40.0),
        ]));

        assert_eq!(
            typed_sample(
                &AddField {
                    left: left.clone(),
                    right: right.clone(),
                },
                &positions,
            ),
            AttributeArray::Vec2(vec![Vec2(11.0, 22.0), Vec2(-27.0, 44.0)])
        );
        assert_eq!(
            typed_sample(
                &MultiplyField {
                    left: left.clone(),
                    right: right.clone(),
                },
                &positions,
            ),
            AttributeArray::Vec2(vec![Vec2(10.0, 40.0), Vec2(-90.0, 160.0)])
        );
        assert_eq!(
            typed_sample(
                &MaxField {
                    left: left.clone(),
                    right: right.clone(),
                },
                &positions,
            ),
            AttributeArray::Vec2(vec![Vec2(10.0, 20.0), Vec2(3.0, 40.0)])
        );
        assert_eq!(
            typed_sample(
                &BlendField {
                    left,
                    right,
                    amount: 0.5,
                },
                &positions,
            ),
            AttributeArray::Vec2(vec![Vec2(5.5, 11.0), Vec2(-13.5, 22.0)])
        );
    }

    #[test]
    fn vec4_operands_combine_component_wise() {
        let left = AttributeArray::Vec4(vec![Vec4(1.0, 2.0, 3.0, 4.0)]);
        let right = AttributeArray::Vec4(vec![Vec4(10.0, 20.0, 30.0, 40.0)]);

        assert_eq!(
            combine_samples(&left, &right, 1, |left, right| left + right),
            Ok(AttributeArray::Vec4(vec![Vec4(11.0, 22.0, 33.0, 44.0)]))
        );
    }

    #[test]
    fn a_scalar_operand_broadcasts_across_a_vector_one() {
        // Scaling a vector field by an intensity is the case this unit exists
        // for, and it has to read the same whichever side the scalar is on.
        let positions = [Vec2(0.0, 0.0); 2];
        let vectors = constant_array(AttributeArray::Vec2(vec![Vec2(1.0, 2.0), Vec2(3.0, 4.0)]));
        let half = FieldValue::new(ConstantField(0.5));
        let scaled = AttributeArray::Vec2(vec![Vec2(0.5, 1.0), Vec2(1.5, 2.0)]);

        assert_eq!(
            typed_sample(
                &MultiplyField {
                    left: vectors.clone(),
                    right: half.clone(),
                },
                &positions,
            ),
            scaled
        );
        assert_eq!(
            typed_sample(
                &MultiplyField {
                    left: half.clone(),
                    right: vectors.clone(),
                },
                &positions,
            ),
            scaled
        );
        assert_eq!(
            typed_sample(
                &AddField {
                    left: half,
                    right: vectors,
                },
                &positions,
            ),
            AttributeArray::Vec2(vec![Vec2(1.5, 2.5), Vec2(3.5, 4.5)])
        );
    }

    #[test]
    fn color_operands_combine_component_wise() {
        let positions = [Vec2(0.0, 0.0)];
        let left = constant_array(AttributeArray::Color(vec![Color {
            r: 0.25,
            g: 0.5,
            b: 0.75,
            a: 1.0,
        }]));
        let right = constant_array(AttributeArray::Color(vec![Color {
            r: 0.75,
            g: 0.25,
            b: 0.25,
            a: 0.5,
        }]));

        assert_eq!(
            typed_sample(
                &MaxField {
                    left: left.clone(),
                    right: right.clone(),
                },
                &positions,
            ),
            AttributeArray::Color(vec![Color {
                r: 0.75,
                g: 0.5,
                b: 0.75,
                a: 1.0,
            }])
        );
        assert_eq!(
            typed_sample(
                &BlendField {
                    left,
                    right,
                    amount: 0.5,
                },
                &positions,
            ),
            AttributeArray::Color(vec![Color {
                r: 0.5,
                g: 0.375,
                b: 0.5,
                a: 0.75,
            }])
        );
    }

    #[test]
    fn a_scalar_operand_broadcasts_across_a_color_one() {
        let positions = [Vec2(0.0, 0.0)];
        let color = constant_array(AttributeArray::Color(vec![Color {
            r: 0.2,
            g: 0.4,
            b: 0.6,
            a: 0.8,
        }]));

        assert_eq!(
            typed_sample(
                &MultiplyField {
                    left: FieldValue::new(ConstantField(0.5)),
                    right: color,
                },
                &positions,
            ),
            AttributeArray::Color(vec![Color {
                r: 0.1,
                g: 0.2,
                b: 0.3,
                a: 0.4,
            }])
        );
    }

    #[test]
    fn operands_of_unrelated_types_do_not_combine() {
        // `Vec4` and `Color` are four components each, and pairing them anyway
        // is the implicit conversion the model refuses.
        let pairs = [
            (
                AttributeArray::Vec2(vec![Vec2(1.0, 2.0)]),
                AttributeArray::Vec3(vec![Vec3(1.0, 2.0, 3.0)]),
            ),
            (
                AttributeArray::Vec4(vec![Vec4(1.0, 2.0, 3.0, 4.0)]),
                AttributeArray::Color(vec![Color {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                }]),
            ),
            (
                AttributeArray::F32(vec![1.0]),
                AttributeArray::Bool(vec![true]),
            ),
        ];

        for (left, right) in pairs {
            assert_eq!(
                combine_samples(&left, &right, 1, |left, right| left + right),
                Err(FieldError::IncompatibleOperands {
                    left: left.attr_type(),
                    right: right.attr_type(),
                })
            );
        }
    }

    #[test]
    fn a_field_over_unrelated_operands_reads_zero() {
        // The typed error cannot leave `Field::sample`, so the combinator
        // answers a zero scalar column instead of taking evaluation down.
        let positions = [Vec2(0.0, 0.0); 2];
        let field = AddField {
            left: constant_array(AttributeArray::Vec2(vec![Vec2(1.0, 2.0), Vec2(3.0, 4.0)])),
            right: constant_array(AttributeArray::Vec3(vec![
                Vec3(1.0, 2.0, 3.0),
                Vec3(4.0, 5.0, 6.0),
            ])),
        };

        assert_eq!(
            typed_sample(&field, &positions),
            AttributeArray::F32(vec![0.0, 0.0])
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

    // ---- RampField --------------------------------------------------------

    const RAMP_RED: Color = Color::new(1.0, 0.0, 0.0, 1.0);
    const RAMP_BLUE: Color = Color::new(0.0, 0.0, 1.0, 1.0);

    /// `XField` reads back the x of each position, so the sample positions
    /// below double as the ramp's input values.
    fn ramp_colors(field: &RampField, inputs: &[f32]) -> Vec<Color> {
        let positions: Vec<Vec2> = inputs.iter().map(|x| Vec2(*x, 0.0)).collect();
        match typed_sample(field, &positions) {
            AttributeArray::Color(colors) => colors,
            other => panic!(
                "a ramp field must answer Color, got {:?}",
                other.attr_type()
            ),
        }
    }

    #[test]
    fn ramp_reads_the_expected_colour_for_a_known_input() {
        let field = RampField::new(
            FieldValue::new(XField),
            RampParam::linear([(0.0, RAMP_RED), (1.0, RAMP_BLUE)]),
        );
        let colors = ramp_colors(&field, &[0.0, 0.5, 1.0]);
        assert_eq!(colors[0], RAMP_RED);
        assert!((colors[1].r - 0.5).abs() < 1e-6 && (colors[1].b - 0.5).abs() < 1e-6);
        assert_eq!(colors[2], RAMP_BLUE);
    }

    #[test]
    fn a_single_stop_ramp_is_one_colour_over_the_whole_field() {
        let field = RampField::new(
            FieldValue::new(XField),
            RampParam::linear([(0.5, RAMP_RED)]),
        );
        assert!(
            ramp_colors(&field, &[-5.0, 0.0, 0.5, 1.0, 9.0])
                .iter()
                .all(|color| *color == RAMP_RED)
        );
    }

    #[test]
    fn ramp_inputs_outside_the_range_clamp_to_the_end_colours() {
        let field = RampField::new(
            FieldValue::new(XField),
            RampParam::linear([(0.0, RAMP_RED), (1.0, RAMP_BLUE)]),
        );
        let colors = ramp_colors(&field, &[-4.0, 4.0]);
        assert_eq!(colors, vec![RAMP_RED, RAMP_BLUE]);
    }

    /// `in_min` / `in_max` rescale the source field before the lookup, which
    /// is what lets a ramp authored over `0..=1` read an attribute in pixels.
    #[test]
    fn the_input_range_rescales_before_the_lookup() {
        let field = RampField::new(
            FieldValue::new(XField),
            RampParam::linear([(0.0, RAMP_RED), (1.0, RAMP_BLUE)]),
        )
        .with_range(100.0, 200.0);
        let colors = ramp_colors(&field, &[100.0, 150.0, 200.0]);
        assert_eq!(colors[0], RAMP_RED);
        assert!((colors[1].b - 0.5).abs() < 1e-6);
        assert_eq!(colors[2], RAMP_BLUE);
    }

    /// A zero-width input range has no normalization to do. It becomes a hard
    /// step at that value rather than dividing by zero and sampling `NaN`.
    #[test]
    fn a_zero_width_input_range_is_a_step_not_a_division_by_zero() {
        let field = RampField::new(
            FieldValue::new(XField),
            RampParam::linear([(0.0, RAMP_RED), (1.0, RAMP_BLUE)]),
        )
        .with_range(2.0, 2.0);
        assert_eq!(
            ramp_colors(&field, &[1.9, 2.0, 2.1]),
            vec![RAMP_RED, RAMP_BLUE, RAMP_BLUE]
        );
    }

    /// `in_min` / `in_max` are Float parameters, so a parameter port can hand
    /// them a computed `NaN` or infinity. A span that is not a finite width
    /// takes the same hard step at `in_min` a zero-width range takes; without
    /// the guard the division yields a `NaN` position, which samples the last
    /// colour everywhere and looks like a working ramp stuck on one stop.
    #[test]
    fn a_non_finite_input_range_is_a_step_not_a_nan_position() {
        let ramp = || RampParam::linear([(0.0, RAMP_RED), (1.0, RAMP_BLUE)]);
        let samples = [-1.0, 0.0, 0.5, 1.0];
        // Everything below `in_min` reads the first stop, everything at or
        // above it the last. `NaN` compares false against both, so an
        // unorderable `in_min` puts every sample on the last stop.
        for (in_min, in_max, expected) in [
            // Guarded here but unchanged in answer: `value < NaN` is false.
            (f32::NAN, 1.0, [RAMP_BLUE; 4]),
            // Was `NaN` positions (all blue); now steps at 0.
            (0.0, f32::NAN, [RAMP_RED, RAMP_BLUE, RAMP_BLUE, RAMP_BLUE]),
            (f32::NEG_INFINITY, f32::INFINITY, [RAMP_BLUE; 4]),
            // Was a ratio collapsed to 0 (all red); now steps at `-f32::MAX`.
            (-f32::MAX, f32::MAX, [RAMP_BLUE; 4]),
        ] {
            let field = RampField::new(FieldValue::new(XField), ramp()).with_range(in_min, in_max);
            assert_eq!(
                ramp_colors(&field, &samples),
                expected.to_vec(),
                "range ({in_min}, {in_max})"
            );
        }
    }

    /// The point of the whole unit. A scalar field can only ever move `Cd`
    /// along the grey axis, because one number broadcasts to r, g and b; a
    /// ramp field writes three different numbers, so the hue moves.
    #[test]
    fn a_ramp_field_changes_the_hue_of_cd_where_a_scalar_field_cannot() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0)]);
        geometry
            .points_mut()
            .insert(names::CD, AttributeArray::Color(vec![Color::WHITE; 2]))
            .unwrap();
        let spec = FieldApply::new(Domain::Point, names::CD);

        let greyscale = apply_field(&geometry, &spec, &XField, &ctx()).unwrap();
        // `XField` samples the point's x, so the two points read 0 and 1, and
        // the scalar broadcasts that one number across r, g and b. Naming the
        // values rather than comparing the colour to itself is what makes the
        // assertion fail if the scalar path stops writing, writes white, or
        // reaches only one channel.
        assert_eq!(
            greyscale
                .points()
                .get(names::CD)
                .unwrap()
                .as_color(names::CD)
                .unwrap()
                .to_vec(),
            vec![
                Color::new(0.0, 0.0, 0.0, 1.0),
                Color::new(1.0, 1.0, 1.0, 1.0),
            ],
            "a scalar field broadcasts, so it can only produce grey"
        );

        let ramp = RampField::new(
            FieldValue::new(XField),
            RampParam::linear([(0.0, RAMP_RED), (1.0, RAMP_BLUE)]),
        );
        let colored = apply_field(&geometry, &spec, &ramp, &ctx()).unwrap();
        let colors = colored
            .points()
            .get(names::CD)
            .unwrap()
            .as_color(names::CD)
            .unwrap();
        assert_eq!(colors[0], RAMP_RED);
        assert_eq!(colors[1], RAMP_BLUE);
    }

    /// The Color target's default component mask is `rgb`, so a ramp carrying
    /// an alpha does not silently punch holes in the geometry.
    #[test]
    fn a_ramp_does_not_write_alpha_unless_the_mask_asks_for_it() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0)]);
        geometry
            .points_mut()
            .insert(names::CD, AttributeArray::Color(vec![Color::WHITE]))
            .unwrap();
        let ramp = RampField::new(
            FieldValue::new(XField),
            RampParam::linear([(0.0, Color::new(1.0, 0.0, 0.0, 0.0))]),
        );

        let spec = FieldApply::new(Domain::Point, names::CD);
        let masked = apply_field(&geometry, &spec, &ramp, &ctx()).unwrap();
        assert_eq!(
            masked
                .points()
                .get(names::CD)
                .unwrap()
                .as_color(names::CD)
                .unwrap()[0]
                .a,
            1.0
        );

        let spec = spec.with_components(ComponentMask::parse("rgba"));
        let full = apply_field(&geometry, &spec, &ramp, &ctx()).unwrap();
        assert_eq!(
            full.points()
                .get(names::CD)
                .unwrap()
                .as_color(names::CD)
                .unwrap()[0]
                .a,
            0.0
        );
    }

    /// A geometry with no `Cd` gets one made for it (`create_if_missing`), and
    /// the ramp's colours land in it — no `attribute.set` ceremony in front.
    #[test]
    fn a_ramp_creates_the_colour_column_it_writes() {
        let geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0)]);
        let ramp = RampField::new(
            FieldValue::new(XField),
            RampParam::linear([(0.0, RAMP_RED), (1.0, RAMP_BLUE)]),
        );
        let result = apply_field(
            &geometry,
            &FieldApply::new(Domain::Point, names::STROKE_COLOR),
            &ramp,
            &ctx(),
        )
        .unwrap();
        let colors = result
            .points()
            .get(names::STROKE_COLOR)
            .unwrap()
            .as_color(names::STROKE_COLOR)
            .unwrap();
        assert_eq!(colors[0], RAMP_RED);
        assert_eq!(colors[1], RAMP_BLUE);
        assert!(
            result.points().get(names::CD).is_none(),
            "modulating stroke_color must leave the fill colour alone"
        );
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
        assert!(ComponentMask::parse("rgba").contains(3));
        assert!(ComponentMask::parse("xy").contains(0));
        assert!(ComponentMask::parse("xy").contains(1));
        assert!(!ComponentMask::parse("xy").contains(2));
        // `r`/`g`/`b`/`a` address the same slots as `x`/`y`/`z`/`w`.
        assert_eq!(ComponentMask::parse("rgb"), ComponentMask::parse("xyz"));
        // A specification that names nothing does not select nothing — it
        // hands the choice to the target type, which is `rgb` for a Color and
        // every component for anything else.
        assert_eq!(ComponentMask::parse(""), ComponentMask::UNSPECIFIED);
        assert_eq!(ComponentMask::parse("!?"), ComponentMask::UNSPECIFIED);
    }

    /// `stroke_color` and `stroke_width` are attributes nothing writes until
    /// somebody modulates them, so the modulation node has to be able to
    /// start them.
    #[test]
    fn a_missing_target_is_created_before_it_is_modulated() {
        let geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(2.0, 0.0)]);
        let spec = FieldApply::new(Domain::Point, names::CD);

        let result = applied(&geometry, &spec);
        let colors = result
            .points()
            .get(names::CD)
            .unwrap()
            .as_color(names::CD)
            .unwrap();

        // Created opaque white, then `Set` writes the sample into rgb only.
        assert_eq!(
            colors,
            &[
                Color::new(0.0, 0.0, 0.0, 1.0),
                Color::new(2.0, 2.0, 2.0, 1.0)
            ]
        );
    }

    #[test]
    fn create_if_missing_disabled_still_reports_the_missing_attribute() {
        let geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(2.0, 0.0)]);
        let spec = FieldApply::new(Domain::Point, names::CD).with_create_if_missing(false);

        let error = apply_field(&geometry, &spec, &XField, &ctx()).unwrap_err();

        assert!(
            matches!(
                &error,
                FieldError::Geometry(GeometryError::AttributeNotFound { name }) if name == names::CD
            ),
            "unexpected error: {error}"
        );
    }

    /// A created column carries the type the geometry spec declares for the
    /// reserved name, not whatever the field happened to sample: a scalar
    /// field driving `Cd` still creates a Color.
    #[test]
    fn a_created_reserved_attribute_takes_its_declared_type() {
        let geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(2.0, 0.0)]);

        for (target, expected) in [
            (names::CD, AttributeType::Color),
            (names::STROKE_COLOR, AttributeType::Color),
            (names::STROKE_WIDTH, AttributeType::F32),
            // Not reserved: the scalar field's own type.
            ("heat", AttributeType::F32),
        ] {
            let result = applied(&geometry, &FieldApply::new(Domain::Point, target));
            assert_eq!(
                result.points().get(target).unwrap().attr_type(),
                expected,
                "{target}"
            );
        }
    }

    /// `fill` is declared Bool, and creating it must not quietly widen that to
    /// the field's own type. An invented F32 `fill` would let this call report
    /// success while `rasterize` — which reads the attribute as Bool — went on
    /// ignoring it: a modulation that does nothing and says nothing.
    #[test]
    fn creating_a_bool_reserved_target_reports_the_type_it_always_reported() {
        let geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(2.0, 0.0)]);
        let missing = apply_field(
            &geometry,
            &FieldApply::new(Domain::Point, names::FILL),
            &XField,
            &ctx(),
        );

        // The same error a geometry that already had a `fill` column gives.
        let present = apply_field(
            &two_points_with(names::FILL, AttributeArray::Bool(vec![false, false])),
            &FieldApply::new(Domain::Point, names::FILL),
            &XField,
            &ctx(),
        );
        assert!(
            missing.is_err() && present.is_err(),
            "a Bool target is not modulatable whether or not it had to be created"
        );
        assert_eq!(
            missing.unwrap_err().to_string(),
            present.unwrap_err().to_string(),
            "creating the column must not invent a second error for the same case"
        );
    }

    /// Darkening a color must not also make it transparent. Alpha moves only
    /// when the component specification says so.
    #[test]
    fn a_scalar_field_leaves_alpha_alone_unless_it_is_named() {
        let opaque = Color::new(0.2, 0.4, 0.6, 0.8);
        let geometry = two_points_with(names::CD, AttributeArray::Color(vec![opaque, opaque]));

        let pinned = applied(&geometry, &FieldApply::new(Domain::Point, names::CD));
        let colors = pinned
            .points()
            .get(names::CD)
            .unwrap()
            .as_color(names::CD)
            .unwrap();
        assert_eq!(
            colors,
            &[
                Color::new(0.0, 0.0, 0.0, 0.8),
                Color::new(2.0, 2.0, 2.0, 0.8)
            ]
        );

        let named = applied(
            &geometry,
            &FieldApply::new(Domain::Point, names::CD).with_components(ComponentMask::parse("a")),
        );
        let colors = named
            .points()
            .get(names::CD)
            .unwrap()
            .as_color(names::CD)
            .unwrap();
        assert_eq!(
            colors,
            &[
                Color::new(0.2, 0.4, 0.6, 0.0),
                Color::new(0.2, 0.4, 0.6, 2.0)
            ]
        );
    }

    #[test]
    fn a_non_scalar_field_must_match_the_target_type() {
        struct Vec2Field;
        impl Field for Vec2Field {
            fn byte_size(&self) -> u64 {
                size_of::<Self>() as u64
            }

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
