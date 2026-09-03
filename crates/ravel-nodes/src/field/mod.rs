// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node-graph adapters for the headless field implementations in `ravel-core`.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams, ResolvedValue};
use ravel_core::geometry::{
    AddField, AngleField, AttributeField, BlendField, CombineMode, ComponentField, ComponentMask,
    ComposeField, ConstantField, CurlNoiseField, CurveRemapField, DirectionToField, Domain,
    ExpressionField, FalloffField, FalloffShape, FieldApply, FieldValue, Geometry, GradientField,
    LengthField, MaxField, MultiplyField, NoiseField, RadialField, RampField, TimeField, TimeMode,
    apply_field, component_index,
};
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::types::{NodeData, Vec2};
use std::sync::Arc;

pub struct NoiseFieldProcessor;

impl NoiseFieldProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for NoiseFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(FieldValue::new(NoiseField {
            seed: params.i32_or("seed", 0) as u32,
            frequency: params.f32_or("frequency", 1.0),
            octaves: params.i32_or("octaves", 1).max(1) as u32,
        })))
    }
}

pub struct DirectionToFieldProcessor;

impl NodeProcessor for DirectionToFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let parameter_target = || {
            let [x, y] = params.vec2_or("target", [0.0, 0.0]);
            Vec2(x, y)
        };
        let target = match inputs.first().and_then(|input| input.as_ref()) {
            None => parameter_target(),
            Some(input) => {
                let geometry = input
                    .downcast_ref::<Geometry>()
                    .ok_or_else(|| anyhow::anyhow!("field.direction_to: target is not Geometry"))?;
                ravel_core::geometry::bounds_center(geometry)
                    .map(|center| Vec2(center.0, center.1))
                    .unwrap_or_else(parameter_target)
            }
        };
        Ok(Arc::new(FieldValue::new(DirectionToField { target })))
    }
}

pub struct CurlNoiseFieldProcessor;

impl NodeProcessor for CurlNoiseFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(FieldValue::new(CurlNoiseField::new(
            NoiseField {
                seed: params.i32_or("seed", 0) as u32,
                frequency: params.f32_or("frequency", 1.0),
                octaves: params.i32_or("octaves", 1).max(1) as u32,
            },
            params.f32_or("step", 0.01),
        ))))
    }
}

pub struct GradientFieldProcessor;

impl NodeProcessor for GradientFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let source = field_input(inputs, 0, "field.gradient")?;
        Ok(Arc::new(FieldValue::new(GradientField::new(
            source,
            params.f32_or("step", 0.01),
        ))))
    }
}

pub struct RadialFieldProcessor;

impl NodeProcessor for RadialFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let [x, y] = params.vec2_or("center", [0.0, 0.0]);
        Ok(Arc::new(FieldValue::new(RadialField {
            center: Vec2(x, y),
            tangent: params.str_or("mode", "radial") == "tangent",
        })))
    }
}

pub struct FalloffFieldProcessor;

impl FalloffFieldProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for FalloffFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        // Declared as Channel3 so a 3D falloff needs no second migration;
        // the 2D sampler consumes X and Y.
        let [center_x, center_y, _center_z] = params.vec3_or("center", [0.0, 0.0, 0.0]);
        let center = Vec2(center_x, center_y);
        let shape = falloff_shape(params);
        Ok(Arc::new(FieldValue::new(FalloffField {
            center,
            inner_radius: params.f32_or("inner_radius", 0.0),
            outer_radius: params.f32_or("outer_radius", 1.0),
            shape,
        })))
    }
}

fn falloff_shape(params: &ResolvedParams) -> FalloffShape {
    let Some(value) = params.get("shape") else {
        return FalloffShape::Sphere;
    };
    let ResolvedValue::Str(value) = value else {
        tracing::warn!(
            parameter = "shape",
            "field.falloff parameter is not a string; using sphere"
        );
        return FalloffShape::Sphere;
    };
    match value.as_str() {
        "linear" => {
            let [dx, dy, _dz] = params.vec3_or("direction", [1.0, 0.0, 0.0]);
            FalloffShape::Linear {
                direction: Vec2(dx, dy),
            }
        }
        "sphere" => FalloffShape::Sphere,
        _ => {
            tracing::warn!(
                parameter = "shape",
                value = %value,
                "field.falloff has an unknown shape; using sphere"
            );
            FalloffShape::Sphere
        }
    }
}

pub struct CurveRemapFieldProcessor;

impl CurveRemapFieldProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for CurveRemapFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let source = field_input(inputs, 0, "field.curve_remap")?;
        // A missing or wrongly typed `points` falls back to the identity
        // curve, which is what the template declares.
        let curve = params.curve("points").cloned().unwrap_or_default();
        Ok(Arc::new(FieldValue::new(CurveRemapField::new(
            source, curve,
        ))))
    }
}

pub struct RampFieldProcessor;

impl RampFieldProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for RampFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let source = field_input(inputs, 0, "field.ramp")?;
        // A missing or wrongly typed `stops` falls back to the default ramp,
        // which is what the template declares.
        let ramp = params.ramp("stops").cloned().unwrap_or_default();
        Ok(Arc::new(FieldValue::new(
            RampField::new(source, ramp)
                .with_range(params.f32_or("in_min", 0.0), params.f32_or("in_max", 1.0)),
        )))
    }
}

/// Emits an [`ExpressionField`], and reports whether that field reads the clock.
///
/// The time dependence is decided here rather than downstream because a
/// `FieldValue` is lazy: the object this node produces is the same at every
/// frame and only sampling it varies. If this node is not time-dependent, the
/// evaluator caches it — and its consumers, which see no fresh input — under
/// `TimeKey::TIMELESS`, and `sin(time)` renders one frozen frame for the whole
/// timeline. Saying `true` here re-pulls the node each frame, which yields a
/// new `FieldValue` and cascades as `CacheMiss::InputFresh` through however
/// many combinators (`field.add`, `field.multiply`, `field.max`, …) sit
/// between here and `field.apply`.
///
/// The flag is captured at construction, so [`NodeProcessor::rebuild_on_node_change`]
/// must stay at its conservative `true`: an edit to `expression` has to rebuild
/// this processor, not merely drop its cached values.
pub struct ExpressionFieldProcessor {
    time_dependent: bool,
}

impl ExpressionFieldProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            time_dependent: expression_field_reads_the_clock(node),
        }
    }
}

/// Whether the expression stored on `node` moves with the frame position.
///
/// The stored parameter is the whole answer: `expression` is a `String`, and a
/// string parameter cannot be driven by a connected parameter port
/// (`GraphError::UnsupportedParamType`), so no overlay can substitute a source
/// this cannot see. `a_string_parameter_cannot_be_driven_by_a_port` pins that
/// — if it ever stops holding, this has to fall back to `true` rather than
/// guess, because being wrong here freezes the picture.
fn expression_field_reads_the_clock(node: &Node) -> bool {
    match node
        .parameters
        .iter()
        .find(|parameter| parameter.key == "expression")
        .map(|parameter| &parameter.value)
    {
        // Compiling here duplicates the work `process` does, once per
        // registration rather than per frame. `ExpressionField` is the only
        // place that knows how a source maps onto the field vocabulary, so
        // asking it is what keeps this answer and the evaluated one the same.
        Some(ParameterValue::String(source)) => {
            ExpressionField::new(source.clone(), 0.0).is_time_varying()
        }
        // No source, or one `ResolvedParams::str_or` will not read as a
        // string: `process` falls back to `""`, which is a constant.
        _ => false,
    }
}

impl NodeProcessor for ExpressionFieldProcessor {
    fn is_time_dependent(&self) -> bool {
        self.time_dependent
    }

    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(FieldValue::new(ExpressionField::new(
            params.str_or("expression", ""),
            params.f32_or("default", 0.0),
        ))))
    }
}

pub struct AddFieldProcessor;
pub struct MultiplyFieldProcessor;
pub struct MaxFieldProcessor;

macro_rules! binary_processor {
    ($processor:ident, $field:ident, $name:literal) => {
        impl NodeProcessor for $processor {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                inputs: &[Option<Arc<dyn NodeData>>],
                _params: &ResolvedParams,
                _scope: &mut dyn EvalScope,
            ) -> anyhow::Result<Arc<dyn NodeData>> {
                let left = field_input(inputs, 0, $name)?;
                let right = field_input(inputs, 1, $name)?;
                Ok(Arc::new(FieldValue::new($field { left, right })))
            }
        }
    };
}

binary_processor!(AddFieldProcessor, AddField, "field.add");
binary_processor!(MultiplyFieldProcessor, MultiplyField, "field.multiply");
binary_processor!(MaxFieldProcessor, MaxField, "field.max");

pub struct BlendFieldProcessor;

impl BlendFieldProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for BlendFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let left = field_input(inputs, 0, "field.blend")?;
        let right = field_input(inputs, 1, "field.blend")?;
        Ok(Arc::new(FieldValue::new(BlendField {
            left,
            right,
            amount: params.f32_or("amount", 0.5),
        })))
    }
}

/// `field.length`: wraps its input in a [`LengthField`].
pub struct LengthFieldProcessor;

impl NodeProcessor for LengthFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let source = field_input(inputs, 0, "field.length")?;
        Ok(Arc::new(FieldValue::new(LengthField::new(source))))
    }
}

/// `field.angle`: wraps its input in an [`AngleField`].
pub struct AngleFieldProcessor;

impl NodeProcessor for AngleFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let source = field_input(inputs, 0, "field.angle")?;
        Ok(Arc::new(FieldValue::new(AngleField::new(source))))
    }
}

/// `field.component`: wraps its input in a [`ComponentField`] selecting the
/// component the `component` parameter names.
pub struct ComponentFieldProcessor;

impl NodeProcessor for ComponentFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let source = field_input(inputs, 0, "field.component")?;
        Ok(Arc::new(FieldValue::new(ComponentField::new(
            source,
            component_index(params.str_or("component", "x")),
        ))))
    }
}

/// `field.compose.vec2` / `vec3` / `vec4`: one scalar field per component in,
/// a vector field out.
///
/// The arity is the processor's, not the wiring's: an unconnected `FIELD`
/// port evaluates to the typed zero (`ConstantField(0.0)`), so a half-wired
/// compose answers zeros in the slots nothing drives instead of failing.
pub struct ComposeFieldProcessor {
    components: usize,
}

impl ComposeFieldProcessor {
    pub const fn new(components: usize) -> Self {
        Self { components }
    }
}

impl NodeProcessor for ComposeFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let sources = (0..self.components)
            .map(|slot| optional_field_input(inputs, slot, "field.compose"))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Arc::new(FieldValue::new(ComposeField::new(sources))))
    }
}

pub struct AttributeFieldProcessor;

impl AttributeFieldProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for AttributeFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(FieldValue::new(
            AttributeField::new(params.str_or("name", "index"))
                .with_component(params.str_or("component", "x"))
                .with_normalize(params.bool_or("normalize", false))
                .with_default(params.f32_or("default", 0.0)),
        )))
    }
}

/// Emits a [`TimeField`]: the evaluation clock as a field.
///
/// **Always time-dependent.** The reasoning is `ExpressionFieldProcessor`'s,
/// without the conditional: a `FieldValue` is lazy, so nothing downstream can
/// tell that sampling it varies with the frame. Re-pulling this node every frame is
/// what marks `field.apply` as `CacheMiss::InputFresh` through however many
/// combinators sit between them; without it the whole chain caches as
/// `TimeKey::TIMELESS` and the modulation freezes.
pub struct TimeFieldProcessor;

impl NodeProcessor for TimeFieldProcessor {
    fn is_time_dependent(&self) -> bool {
        true
    }

    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let default = TimeField::default();
        Ok(Arc::new(FieldValue::new(
            TimeField::new(TimeMode::parse(params.str_or("mode", "seconds")))
                .with_duration(params.f32_or("duration", default.duration))
                .with_scale(params.f32_or("scale", default.scale))
                .with_offset(params.f32_or("offset", default.offset)),
        )))
    }
}

/// Emits a [`ConstantField`]: the same scalar everywhere.
pub struct ConstantFieldProcessor;

impl NodeProcessor for ConstantFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(FieldValue::new(ConstantField(
            params.f32_or("value", 1.0),
        ))))
    }
}

pub struct ApplyFieldProcessor;

impl ApplyFieldProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for ApplyFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = inputs
            .first()
            .and_then(|input| input.as_ref())
            .and_then(|input| input.downcast_ref::<Geometry>())
            .ok_or_else(|| anyhow::anyhow!("field.apply: input 0 is not Geometry"))?;
        let field = field_input(inputs, 1, "field.apply")?;
        let domain = match params.str_or("domain", "point") {
            "instance" => Domain::Instance,
            "detail" => Domain::Detail,
            _ => Domain::Point,
        };
        let spec = FieldApply::new(domain, params.str_or("target", "value"))
            .with_amount(params.f32_or("amount", 1.0))
            .with_combine(CombineMode::parse(params.str_or("combine", "set")))
            .with_components(ComponentMask::parse(params.str_or("components", "")))
            .with_group(params.str_or("group", ""))
            .with_create_if_missing(params.bool_or("create_if_missing", true));
        Ok(Arc::new(apply_field(
            geometry,
            &spec,
            field.0.as_ref(),
            ctx,
        )?))
    }
}

/// Reads field input `index`, treating an unconnected port as the constant
/// zero field.
///
/// A `FIELD` port carries a sampler, so its typed zero has to be a sampler
/// too — `ConstantField(0.0)`, exactly as that type's own documentation says.
/// Erroring instead would make a half-wired `field.compose` fail rather than
/// answer zero on the slot the user has not filled in yet, which is not how
/// any other multi-input node in either domain behaves (`vector.construct`
/// reads its component parameter, `vector.dot` reads the zero vector).
///
/// A port that *is* connected but carries something other than a field is
/// still an error: that is a wiring mistake, not an empty slot.
fn optional_field_input(
    inputs: &[Option<Arc<dyn NodeData>>],
    index: usize,
    processor: &str,
) -> anyhow::Result<FieldValue> {
    match inputs.get(index).and_then(|input| input.as_ref()) {
        None => Ok(FieldValue::new(ConstantField(0.0))),
        Some(input) => input
            .downcast_ref::<FieldValue>()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{processor}: input {index} is not a FieldValue")),
    }
}

fn field_input(
    inputs: &[Option<Arc<dyn NodeData>>],
    index: usize,
    processor: &str,
) -> anyhow::Result<FieldValue> {
    inputs
        .get(index)
        .and_then(|input| input.as_ref())
        .and_then(|input| input.downcast_ref::<FieldValue>())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{processor}: input {index} is not a FieldValue"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::geometry::{
        AttributeArray, AttributeSet, AttributeType, ConstantField, FieldSample, names,
    };
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::param_curve::CurveParam;
    use ravel_core::param_ramp::RampParam;
    use ravel_core::types::{Color, FrameRate};

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    fn sample(value: &dyn NodeData) -> Vec<f32> {
        value
            .downcast_ref::<FieldValue>()
            .unwrap()
            .sample(&FieldSample::positions_only(&[Vec2(0.25, 0.75)], &ctx()))
            .as_f32("sample")
            .unwrap()
            .to_vec()
    }

    /// [`sample`] at a caller-chosen context, for the fields whose answer *is*
    /// the context.
    fn sample_at(value: &dyn NodeData, ctx: &EvalContext) -> Vec<f32> {
        value
            .downcast_ref::<FieldValue>()
            .unwrap()
            .sample(&FieldSample::positions_only(&[Vec2(0.25, 0.75)], ctx))
            .as_f32("sample")
            .unwrap()
            .to_vec()
    }

    /// Emits a fixed value; stands in for upstream nodes in evaluator tests.
    struct StubSource(Arc<dyn NodeData>);

    impl NodeProcessor for StubSource {
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

    /// Evaluate `node` with `proc` in a fresh evaluator, wiring each value in
    /// `inputs` to the input slot of the same index via a stub source.
    /// Like [`run`], but a `None` slot is left **unwired** rather than fed a
    /// zero — the only way to exercise the unconnected-port path — and the
    /// evaluation result is returned instead of unwrapped.
    fn run_opt(
        node: &Node,
        proc: Arc<dyn NodeProcessor>,
        inputs: &[Option<Arc<dyn NodeData>>],
    ) -> Result<Arc<dyn NodeData>, ravel_core::eval::EvalError> {
        let mut graph = Graph::new().add_node(node.clone()).unwrap();
        let mut ev = Evaluator::new();
        ev.register(node.id, proc);
        for (i, value) in inputs.iter().enumerate() {
            let Some(value) = value else { continue };
            let src_id = NodeId::new(100 + i as u64);
            graph = graph
                .add_node(Node::new(src_id, "test.source").with_output("out", value.data_type_id()))
                .unwrap()
                .add_edge(
                    EdgeId::new(i as u64 + 1),
                    src_id,
                    OutputPortIndex(0),
                    node.id,
                    InputPortIndex(i as u32),
                )
                .unwrap();
            ev.register(src_id, Arc::new(StubSource(value.clone())));
        }
        ev.evaluate(&graph, node.id, &ctx())
    }

    fn run(
        node: &Node,
        proc: Arc<dyn NodeProcessor>,
        inputs: &[Arc<dyn NodeData>],
    ) -> Arc<dyn NodeData> {
        let mut graph = Graph::new().add_node(node.clone()).unwrap();
        let mut ev = Evaluator::new();
        ev.register(node.id, proc);
        for (i, value) in inputs.iter().enumerate() {
            let src_id = NodeId::new(100 + i as u64);
            graph = graph
                .add_node(Node::new(src_id, "test.source").with_output("out", value.data_type_id()))
                .unwrap()
                .add_edge(
                    EdgeId::new(i as u64 + 1),
                    src_id,
                    OutputPortIndex(0),
                    node.id,
                    InputPortIndex(i as u32),
                )
                .unwrap();
            ev.register(src_id, Arc::new(StubSource(value.clone())));
        }
        ev.evaluate(&graph, node.id, &ctx()).unwrap()
    }

    fn registered_node(type_key: &str, id: u64) -> Node {
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        registry
            .create_node(type_key, NodeId::new(id))
            .unwrap_or_else(|| panic!("{type_key} is not registered"))
    }

    fn set_string_param(node: &mut Node, key: &str, value: &str) {
        node.parameters
            .iter_mut()
            .find(|parameter| parameter.key == key)
            .unwrap_or_else(|| panic!("{} has no {key}", node.type_key))
            .value = ParameterValue::String(value.into());
    }

    fn warnings_from(f: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Sink(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
            type Writer = Self;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let sink = Sink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        String::from_utf8(sink.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn noise_processor_reads_node_parameters() {
        let node = Node::new(NodeId::new(1), "field.noise")
            .with_output("field", DataTypeId::FIELD)
            .with_param("seed", ParameterValue::Int(19))
            .with_param("frequency", ParameterValue::Float(2.5))
            .with_param("octaves", ParameterValue::Int(3));

        let first = run(&node, Arc::new(NoiseFieldProcessor::from_node(&node)), &[]);
        let second = run(&node, Arc::new(NoiseFieldProcessor::from_node(&node)), &[]);
        assert_eq!(sample(first.as_ref()), sample(second.as_ref()));
    }

    /// The falloff shape is a string parameter in the real template. The
    /// sample positions make the two implementations produce different
    /// values, so a missing match arm or a default-only implementation fails.
    #[test]
    fn declared_falloff_shapes_reach_their_string_parameter_branches() {
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        let options = registry
            .param_options("field.falloff", "shape")
            .unwrap()
            .to_vec();
        assert_eq!(options, ravel_core::registry::builtin::FALLOFF_SHAPES);

        let mut values = Vec::new();
        for shape in options {
            let mut node = registered_node("field.falloff", 1);
            set_string_param(&mut node, "shape", &shape);
            let output = run(
                &node,
                Arc::new(FalloffFieldProcessor::from_node(&node)),
                &[],
            );
            values.push((shape, sample(output.as_ref())[0]));
        }
        assert_ne!(values[0].1, values[1].1, "falloff branches must differ");
    }

    #[test]
    fn unknown_falloff_shape_warns_and_uses_sphere() {
        let mut unknown = registered_node("field.falloff", 1);
        set_string_param(&mut unknown, "shape", "future_shape");
        let mut sphere = registered_node("field.falloff", 2);
        set_string_param(&mut sphere, "shape", "sphere");

        let unknown_output = {
            let output = run(
                &unknown,
                Arc::new(FalloffFieldProcessor::from_node(&unknown)),
                &[],
            );
            sample(output.as_ref())
        };
        let sphere_output = {
            let output = run(
                &sphere,
                Arc::new(FalloffFieldProcessor::from_node(&sphere)),
                &[],
            );
            sample(output.as_ref())
        };
        let logged = warnings_from(|| {
            let output = run(
                &unknown,
                Arc::new(FalloffFieldProcessor::from_node(&unknown)),
                &[],
            );
            let _ = sample(output.as_ref());
        });
        assert_eq!(unknown_output, sphere_output);
        assert!(
            logged.contains("unknown shape"),
            "missing warning: {logged:?}"
        );
    }

    #[test]
    fn attribute_processor_reads_node_parameters() {
        let node = Node::new(NodeId::new(1), "field.attribute")
            .with_output("field", DataTypeId::FIELD)
            .with_param("name", ParameterValue::String("weight".into()))
            .with_param("component", ParameterValue::String("y".into()))
            .with_param("normalize", ParameterValue::Bool(false))
            .with_param("default", ParameterValue::Float(4.0));

        let value = run(
            &node,
            Arc::new(AttributeFieldProcessor::from_node(&node)),
            &[],
        );

        // `sample` supplies no attributes, so the configured default surfaces —
        // which is also the "unknown attribute does not fail" path.
        assert_eq!(sample(value.as_ref()), vec![4.0]);
    }

    #[test]
    fn attribute_processor_reads_a_column_from_the_sampled_domain() {
        let node = Node::new(NodeId::new(1), "field.attribute")
            .with_output("field", DataTypeId::FIELD)
            .with_param("name", ParameterValue::String("weight".into()))
            .with_param("default", ParameterValue::Float(-1.0));

        let value = run(
            &node,
            Arc::new(AttributeFieldProcessor::from_node(&node)),
            &[],
        );

        let mut attributes = AttributeSet::new();
        attributes
            .insert(
                "P",
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0)]),
            )
            .unwrap();
        attributes
            .insert("weight", AttributeArray::F32(vec![2.0, 8.0]))
            .unwrap();
        let positions = attributes.get("P").unwrap().as_vec2("P").unwrap();

        let sampled = value
            .downcast_ref::<FieldValue>()
            .unwrap()
            .sample(&FieldSample::new(positions, &attributes, &ctx()));

        assert_eq!(sampled.as_f32("weight"), Ok(&[2.0, 8.0][..]));
    }

    #[test]
    fn curve_processor_wraps_its_field_input() {
        let node = Node::new(NodeId::new(1), "field.curve_remap")
            .with_input("field", &[DataTypeId::FIELD])
            .with_output("field", DataTypeId::FIELD)
            .with_param(
                "points",
                ParameterValue::Curve(CurveParam::linear([(0.0, 0.0), (1.0, 10.0)])),
            );
        let source: Arc<dyn NodeData> = Arc::new(FieldValue::new(ConstantField(0.25)));

        let output = run(
            &node,
            Arc::new(CurveRemapFieldProcessor::from_node(&node)),
            &[source],
        );
        assert_eq!(sample(output.as_ref()), vec![2.5]);
    }

    #[test]
    fn curve_processor_matches_math_curve_for_the_same_control_points() {
        let curve = CurveParam::linear([(0.0, 0.0), (0.25, 0.8), (1.0, 0.2)]);
        let input = 0.5;
        let field_node = Node::new(NodeId::new(1), "field.curve_remap")
            .with_input("field", &[DataTypeId::FIELD])
            .with_output("field", DataTypeId::FIELD)
            .with_param("points", ParameterValue::Curve(curve.clone()));
        let field_source: Arc<dyn NodeData> = Arc::new(FieldValue::new(ConstantField(input)));
        let field_output = run(
            &field_node,
            Arc::new(CurveRemapFieldProcessor::from_node(&field_node)),
            &[field_source],
        );
        let field_value = sample(field_output.as_ref())[0];

        let math_node = Node::new(NodeId::new(2), "math.curve")
            .with_output("output", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(input))
            .with_param("curve", ParameterValue::Curve(curve));
        let graph = Graph::new().add_node(math_node).unwrap();
        let mut evaluator = Evaluator::new();
        evaluator.register(NodeId::new(2), Arc::new(crate::math::MathCurveProcessor));
        let math_output = evaluator.evaluate(&graph, NodeId::new(2), &ctx()).unwrap();
        let math_value = math_output
            .downcast_ref::<ravel_core::types::Scalar>()
            .unwrap()
            .0;

        assert_eq!(field_value.to_bits(), math_value.to_bits());
    }

    /// A node whose `points` is missing entirely (or is left over as some
    /// other kind) falls back to the identity curve rather than failing.
    #[test]
    fn curve_processor_without_points_is_the_identity() {
        let node = Node::new(NodeId::new(1), "field.curve_remap")
            .with_input("field", &[DataTypeId::FIELD])
            .with_output("field", DataTypeId::FIELD);
        let source: Arc<dyn NodeData> = Arc::new(FieldValue::new(ConstantField(0.25)));

        let output = run(
            &node,
            Arc::new(CurveRemapFieldProcessor::from_node(&node)),
            &[source],
        );
        assert_eq!(sample(output.as_ref()), vec![0.25]);
    }

    // ---- field.ramp -------------------------------------------------------

    const RED: Color = Color::new(1.0, 0.0, 0.0, 1.0);
    const BLUE: Color = Color::new(0.0, 0.0, 1.0, 1.0);

    fn ramp_node(stops: RampParam, in_min: f32, in_max: f32) -> Node {
        Node::new(NodeId::new(1), "field.ramp")
            .with_input("field", &[DataTypeId::FIELD])
            .with_output("field", DataTypeId::FIELD)
            .with_param("stops", ParameterValue::Ramp(stops))
            .with_param("in_min", ParameterValue::Float(in_min))
            .with_param("in_max", ParameterValue::Float(in_max))
    }

    fn sampled_color(value: &dyn NodeData) -> Color {
        match value
            .downcast_ref::<FieldValue>()
            .unwrap()
            .sample(&FieldSample::positions_only(&[Vec2(0.25, 0.75)], &ctx()))
        {
            AttributeArray::Color(colors) => colors[0],
            other => panic!("field.ramp must answer Color, got {:?}", other.attr_type()),
        }
    }

    /// The processor has to read all three parameters: the stops, and the
    /// input range that maps the source field onto them.
    #[test]
    fn ramp_processor_reads_its_parameters() {
        let node = ramp_node(RampParam::linear([(0.0, RED), (1.0, BLUE)]), 0.0, 8.0);
        let source: Arc<dyn NodeData> = Arc::new(FieldValue::new(ConstantField(4.0)));

        let output = run(
            &node,
            Arc::new(RampFieldProcessor::from_node(&node)),
            &[source],
        );
        let color = sampled_color(output.as_ref());
        assert!(
            (color.r - 0.5).abs() < 1e-6 && (color.b - 0.5).abs() < 1e-6,
            "{color:?}"
        );
    }

    /// A node whose `stops` is missing entirely (or is left over as some
    /// other kind) falls back to the default ramp rather than failing.
    #[test]
    fn ramp_processor_without_stops_is_the_default_ramp() {
        let node = Node::new(NodeId::new(1), "field.ramp")
            .with_input("field", &[DataTypeId::FIELD])
            .with_output("field", DataTypeId::FIELD);
        let source: Arc<dyn NodeData> = Arc::new(FieldValue::new(ConstantField(1.0)));

        let output = run(
            &node,
            Arc::new(RampFieldProcessor::from_node(&node)),
            &[source],
        );
        assert_eq!(sampled_color(output.as_ref()), Color::WHITE);
    }

    // ---- the gradient-along-a-path chain ----------------------------------

    /// One node of each type in the chain, wired
    /// `shape.line → attribute.curveu → field.apply(geometry)` and
    /// `field.attribute("u") → field.ramp → field.apply(field)`.
    ///
    /// Nodes come from the real templates so the ports and parameter defaults
    /// under test are the ones the application would place.
    fn path_gradient_geometry(target: &str) -> Geometry {
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        let node = |type_key: &str, id: u64, params: &[(&str, ParameterValue)]| {
            let mut node = registry
                .create_node(type_key, NodeId::new(id))
                .unwrap_or_else(|| panic!("{type_key} is not registered"));
            for (key, value) in params {
                let parameter = node
                    .parameters
                    .iter_mut()
                    .find(|p| p.key == *key)
                    .unwrap_or_else(|| panic!("{type_key} has no {key} parameter"));
                parameter.value = value.clone();
            }
            node
        };

        let line = node(
            "shape.line",
            1,
            &[
                ("start", ParameterValue::vec2(0.0, 0.0)),
                ("end", ParameterValue::vec2(100.0, 0.0)),
                ("segments", ParameterValue::Int(4)),
            ],
        );
        let curveu = node("attribute.curveu", 2, &[]);
        let attribute = node(
            "field.attribute",
            3,
            &[("name", ParameterValue::String("u".into()))],
        );
        let ramp = node(
            "field.ramp",
            4,
            &[(
                "stops",
                ParameterValue::Ramp(RampParam::linear([(0.0, RED), (1.0, BLUE)])),
            )],
        );
        let apply = node(
            "field.apply",
            5,
            &[("target", ParameterValue::String(target.into()))],
        );

        let mut graph = Graph::new();
        for n in [&line, &curveu, &attribute, &ramp, &apply] {
            graph = graph.add_node(n.clone()).unwrap();
        }
        for (edge, from, to, port) in [
            (1u64, 1u64, 2u64, 0u32),
            (2, 3, 4, 0),
            (3, 2, 5, 0),
            (4, 4, 5, 1),
        ] {
            graph = graph
                .add_edge(
                    EdgeId::new(edge),
                    NodeId::new(from),
                    OutputPortIndex(0),
                    NodeId::new(to),
                    InputPortIndex(port),
                )
                .unwrap();
        }

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(crate::shape::LineProcessor));
        ev.register(
            NodeId::new(2),
            Arc::new(crate::attribute::CurveUProcessor::from_node(&curveu)),
        );
        ev.register(
            NodeId::new(3),
            Arc::new(AttributeFieldProcessor::from_node(&attribute)),
        );
        ev.register(
            NodeId::new(4),
            Arc::new(RampFieldProcessor::from_node(&ramp)),
        );
        ev.register(
            NodeId::new(5),
            Arc::new(ApplyFieldProcessor::from_node(&apply)),
        );

        ev.evaluate(&graph, NodeId::new(5), &ctx())
            .unwrap()
            .downcast_ref::<Geometry>()
            .expect("Geometry")
            .clone()
    }

    fn color_column(geometry: &Geometry, name: &str) -> Vec<Color> {
        geometry
            .points()
            .get(name)
            .unwrap_or_else(|| panic!("{name} column"))
            .as_color(name)
            .unwrap_or_else(|_| panic!("{name} is a Color column"))
            .to_vec()
    }

    /// The purpose of the unit, end to end: the path's own parameter drives a
    /// colour ramp, so the two ends of the line come out different hues.
    #[test]
    fn a_ramp_paints_a_gradient_along_a_path() {
        let geometry = path_gradient_geometry(names::CD);
        let colors = color_column(&geometry, names::CD);

        assert_eq!(colors.len(), 5, "shape.line(segments = 4) has five points");
        assert_eq!(colors[0], RED, "u = 0 takes the first stop");
        assert_eq!(colors[4], BLUE, "u = 1 takes the last stop");
        assert!(
            colors[0].r > colors[2].r && colors[2].r > colors[4].r,
            "red falls monotonically along the path: {colors:?}"
        );
        assert!(
            colors[0].b < colors[2].b && colors[2].b < colors[4].b,
            "blue rises monotonically along the path: {colors:?}"
        );
    }

    /// The same chain aimed at `stroke_color` touches the stroke only: the
    /// fill colour is not written, not even created.
    #[test]
    fn the_same_ramp_aimed_at_stroke_color_leaves_the_fill_alone() {
        let geometry = path_gradient_geometry(names::STROKE_COLOR);
        let colors = color_column(&geometry, names::STROKE_COLOR);

        assert_eq!(colors[0], RED);
        assert_eq!(colors[4], BLUE);
        assert!(
            geometry.points().get(names::CD).is_none(),
            "the fill colour must stay whatever it was"
        );
    }

    #[test]
    fn blend_processor_composes_two_field_inputs() {
        let node = Node::new(NodeId::new(1), "field.blend")
            .with_input("left", &[DataTypeId::FIELD])
            .with_input("right", &[DataTypeId::FIELD])
            .with_output("field", DataTypeId::FIELD)
            .with_param("amount", ParameterValue::Float(0.25));
        let left: Arc<dyn NodeData> = Arc::new(FieldValue::new(ConstantField(2.0)));
        let right: Arc<dyn NodeData> = Arc::new(FieldValue::new(ConstantField(6.0)));

        let output = run(
            &node,
            Arc::new(BlendFieldProcessor::from_node(&node)),
            &[left, right],
        );
        assert_eq!(sample(output.as_ref()), vec![3.0]);
    }

    #[test]
    fn expression_processor_evaluates_the_configured_source() {
        let node = Node::new(NodeId::new(1), "field.expression")
            .with_output("field", DataTypeId::FIELD)
            .with_param("expression", ParameterValue::String("@P.x * 2".into()))
            .with_param("default", ParameterValue::Float(7.0));

        let output = run(
            &node,
            Arc::new(ExpressionFieldProcessor::from_node(&node)),
            &[],
        );
        // `sample` probes one element at (0.25, 0.75), so the expression — not
        // the default — has to produce 0.5.
        assert_eq!(sample(output.as_ref()), vec![0.5]);
    }

    /// A source without the `@` sigil is not a position reference, it is an
    /// unknown variable. The node keeps working and answers its default.
    #[test]
    fn expression_processor_falls_back_when_the_source_does_not_compile() {
        let node = Node::new(NodeId::new(1), "field.expression")
            .with_output("field", DataTypeId::FIELD)
            .with_param("expression", ParameterValue::String("P.x * 2".into()))
            .with_param("default", ParameterValue::Float(7.0));

        let output = run(
            &node,
            Arc::new(ExpressionFieldProcessor::from_node(&node)),
            &[],
        );
        assert_eq!(sample(output.as_ref()), vec![7.0]);
    }

    // ---- time dependence of an expression field ---------------------------

    fn expression_field_node(id: u64, source: &str) -> Node {
        Node::new(NodeId::new(id), "field.expression")
            .with_output("field", DataTypeId::FIELD)
            .with_param("expression", ParameterValue::String(source.into()))
            .with_param("default", ParameterValue::Float(0.0))
    }

    #[test]
    fn an_expression_field_node_is_time_dependent_only_when_it_reads_the_clock() {
        let reads_clock = |source: &str| {
            ExpressionFieldProcessor::from_node(&expression_field_node(1, source))
                .is_time_dependent()
        };

        assert!(reads_clock("frame * 2"));
        assert!(reads_clock("sin(time) + @P.x"));
        assert!(!reads_clock("@P.x * 2"));
        assert!(!reads_clock("res.width / 2"));
        assert!(!reads_clock(""));
        // An attribute other than position no longer refuses the source, so
        // the clock in it counts like any other.
        assert!(reads_clock("@Cd.r * frame"));
        assert!(!reads_clock("@index * 2"));
        // A source that does not compile answers a constant default.
        assert!(!reads_clock("frame *"));
    }

    /// Why reading the stored parameter is the whole answer above: nothing can
    /// substitute a source at process time, because a `String` parameter has no
    /// port form. If this ever starts succeeding, the time-dependence check has
    /// to stop trusting the stored value.
    #[test]
    fn a_string_parameter_cannot_be_driven_by_a_port() {
        let exposed = Graph::new()
            .add_node(expression_field_node(1, "@P.x"))
            .unwrap()
            .expose_param_port(NodeId::new(1), "expression");
        assert!(
            exposed.is_err(),
            "a port over `expression` would let an unseen source reach `process`"
        );
    }

    /// One geometry point carrying a `weight` column the field overwrites.
    fn weighted_point() -> Arc<dyn NodeData> {
        let mut geometry = Geometry::from_points(vec![Vec2(1.0, 0.0)]);
        geometry
            .points_mut()
            .insert("weight", AttributeArray::F32(vec![0.0]))
            .unwrap();
        Arc::new(geometry)
    }

    fn apply_node(id: u64) -> Node {
        Node::new(NodeId::new(id), "field.apply")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_input("field", &[DataTypeId::FIELD])
            .with_output("geometry", DataTypeId::GEOMETRY)
            .with_param("target", ParameterValue::String("weight".into()))
            .with_param("amount", ParameterValue::Float(1.0))
    }

    fn weight_of(value: &Arc<dyn NodeData>) -> f32 {
        value
            .downcast_ref::<Geometry>()
            .expect("Geometry")
            .points()
            .get("weight")
            .expect("weight column")
            .as_f32("weight")
            .expect("f32 column")[0]
    }

    fn frame_ctx(frame: u64) -> EvalContext {
        EvalContext::new(frame, FrameRate::new(30, 1), (1920, 1080))
    }

    /// The failure this guards against is a cache one, so it needs the
    /// evaluator and **one** evaluator across both frames: a fresh one per
    /// frame cannot serve a stale entry and would pass with the bug present.
    #[test]
    fn a_field_expression_reading_the_clock_re_evaluates_when_the_frame_advances() {
        let expression = expression_field_node(2, "frame * 2");
        let apply = apply_node(3);
        let graph = Graph::new()
            .add_node(
                Node::new(NodeId::new(1), "test.source").with_output("out", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_node(expression.clone())
            .unwrap()
            .add_node(apply.clone())
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(1),
            )
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(StubSource(weighted_point())));
        ev.register(
            NodeId::new(2),
            Arc::new(ExpressionFieldProcessor::from_node(&expression)),
        );
        ev.register(
            NodeId::new(3),
            Arc::new(ApplyFieldProcessor::from_node(&apply)),
        );

        let at_zero = ev.evaluate(&graph, NodeId::new(3), &frame_ctx(0)).unwrap();
        let at_one = ev.evaluate(&graph, NodeId::new(3), &frame_ctx(1)).unwrap();

        assert_eq!(weight_of(&at_zero), 0.0);
        assert_eq!(
            weight_of(&at_one),
            2.0,
            "the picture froze: the expression field was served from the timeless cache"
        );
    }

    /// Same thing with a combinator in between: the expression field is one
    /// arm of a `field.add`, so the time dependence has to travel as input
    /// freshness through a node that is itself time-independent.
    #[test]
    fn time_dependence_travels_through_a_field_combinator() {
        let expression = expression_field_node(2, "frame * 2");
        let apply = apply_node(4);
        let graph = Graph::new()
            .add_node(
                Node::new(NodeId::new(1), "test.source").with_output("out", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_node(expression.clone())
            .unwrap()
            .add_node(
                Node::new(NodeId::new(5), "test.source").with_output("out", DataTypeId::FIELD),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(3), "field.add")
                    .with_input("left", &[DataTypeId::FIELD])
                    .with_input("right", &[DataTypeId::FIELD])
                    .with_output("field", DataTypeId::FIELD),
            )
            .unwrap()
            .add_node(apply.clone())
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(5),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(1),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(3),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(4),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(4),
                NodeId::new(3),
                OutputPortIndex(0),
                NodeId::new(4),
                InputPortIndex(1),
            )
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(StubSource(weighted_point())));
        ev.register(
            NodeId::new(2),
            Arc::new(ExpressionFieldProcessor::from_node(&expression)),
        );
        ev.register(
            NodeId::new(5),
            Arc::new(StubSource(Arc::new(FieldValue::new(ConstantField(10.0))))),
        );
        ev.register(NodeId::new(3), Arc::new(AddFieldProcessor));
        ev.register(
            NodeId::new(4),
            Arc::new(ApplyFieldProcessor::from_node(&apply)),
        );

        let at_zero = ev.evaluate(&graph, NodeId::new(4), &frame_ctx(0)).unwrap();
        let at_one = ev.evaluate(&graph, NodeId::new(4), &frame_ctx(1)).unwrap();

        assert_eq!(weight_of(&at_zero), 10.0);
        assert_eq!(
            weight_of(&at_one),
            12.0,
            "the combinator served a stale sum: time dependence did not propagate"
        );
    }

    /// The other half of the fix: a field expression that reads only position
    /// must **not** become time-dependent, or every frame re-evaluates a value
    /// that cannot have changed.
    #[test]
    fn a_position_only_field_expression_stays_cached_across_frames() {
        let expression = expression_field_node(2, "@P.x * 3");
        let apply = apply_node(3);
        let graph = Graph::new()
            .add_node(
                Node::new(NodeId::new(1), "test.source").with_output("out", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_node(expression.clone())
            .unwrap()
            .add_node(apply.clone())
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(1),
            )
            .unwrap();

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(StubSource(weighted_point())));
        ev.register(
            NodeId::new(2),
            Arc::new(CountingExpressionField {
                inner: ExpressionFieldProcessor::from_node(&expression),
                calls: calls.clone(),
            }),
        );
        ev.register(
            NodeId::new(3),
            Arc::new(ApplyFieldProcessor::from_node(&apply)),
        );

        let at_zero = ev.evaluate(&graph, NodeId::new(3), &frame_ctx(0)).unwrap();
        let at_one = ev.evaluate(&graph, NodeId::new(3), &frame_ctx(1)).unwrap();

        assert_eq!(weight_of(&at_zero), 3.0);
        assert_eq!(weight_of(&at_one), 3.0);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "a position-only expression must not be re-pulled per frame"
        );
    }

    struct CountingExpressionField {
        inner: ExpressionFieldProcessor,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl NodeProcessor for CountingExpressionField {
        fn is_time_dependent(&self) -> bool {
            self.inner.is_time_dependent()
        }

        fn process(
            &self,
            node: &Node,
            ctx: &EvalContext,
            inputs: &[Option<Arc<dyn NodeData>>],
            params: &ResolvedParams,
            scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.process(node, ctx, inputs, params, scope)
        }
    }

    // ---- field.time / field.constant ---------------------------------------

    /// Half a frame past frame 18 at 24 fps: no mode can match another by
    /// accident, and a quantised clock cannot match at all.
    fn subframe_ctx() -> EvalContext {
        let mut ctx = EvalContext::new(18, FrameRate::new(24, 1), (1920, 1080));
        ctx.time = 18.5 / 24.0;
        ctx
    }

    #[test]
    fn time_processor_reads_node_parameters() {
        let node = Node::new(NodeId::new(1), "field.time")
            .with_output("field", DataTypeId::FIELD)
            .with_param("mode", ParameterValue::String("normalized".into()))
            .with_param("duration", ParameterValue::Float(74.0))
            .with_param("scale", ParameterValue::Float(3.0))
            .with_param("offset", ParameterValue::Float(-0.5));

        let value = run(&node, Arc::new(TimeFieldProcessor), &[]);

        // 18.5 / 74 = 0.25, then × 3 − 0.5. The default duration (300) would
        // give −0.315, and the frame mode 55.0.
        assert_eq!(sample_at(value.as_ref(), &subframe_ctx()), vec![0.25]);
    }

    #[test]
    fn time_processor_defaults_to_seconds() {
        let node = Node::new(NodeId::new(1), "field.time").with_output("field", DataTypeId::FIELD);

        let value = run(&node, Arc::new(TimeFieldProcessor), &[]);

        // Seconds, unscaled: the frame mode would answer 18.5.
        assert_eq!(
            sample_at(value.as_ref(), &subframe_ctx()),
            vec![(18.5 / 24.0) as f32]
        );
    }

    #[test]
    fn constant_processor_reads_its_value() {
        let node = Node::new(NodeId::new(1), "field.constant")
            .with_output("field", DataTypeId::FIELD)
            .with_param("value", ParameterValue::Float(-2.75));

        let value = run(&node, Arc::new(ConstantFieldProcessor), &[]);

        assert_eq!(sample(value.as_ref()), vec![-2.75]);
    }

    #[test]
    fn constant_processor_defaults_to_the_multiplicative_identity() {
        // Zero is what an *unconnected* field port already answers, so a
        // constant node has to default to something a `multiply` leaves alone.
        let node =
            Node::new(NodeId::new(1), "field.constant").with_output("field", DataTypeId::FIELD);

        let value = run(&node, Arc::new(ConstantFieldProcessor), &[]);

        assert_eq!(sample(value.as_ref()), vec![1.0]);
    }

    #[test]
    fn a_constant_field_node_is_not_time_dependent() {
        assert!(!ConstantFieldProcessor.is_time_dependent());
    }

    /// A cache failure, so it needs the evaluator and **one** evaluator across
    /// both frames: a fresh one per frame cannot serve a stale entry and would
    /// pass with the bug present.
    ///
    /// The `FieldValue` this node emits is lazy and identical every frame, so
    /// nothing downstream can see that sampling it moves. Only
    /// `is_time_dependent` re-pulls it, and only that re-pull marks
    /// `field.apply` as having a fresh input.
    #[test]
    fn a_time_field_re_evaluates_when_the_frame_advances() {
        let time = Node::new(NodeId::new(2), "field.time")
            .with_output("field", DataTypeId::FIELD)
            .with_param("mode", ParameterValue::String("frame".into()));
        let apply = apply_node(3);
        let graph = Graph::new()
            .add_node(
                Node::new(NodeId::new(1), "test.source").with_output("out", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_node(time.clone())
            .unwrap()
            .add_node(apply.clone())
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(1),
            )
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(StubSource(weighted_point())));
        ev.register(NodeId::new(2), Arc::new(TimeFieldProcessor));
        ev.register(
            NodeId::new(3),
            Arc::new(ApplyFieldProcessor::from_node(&apply)),
        );

        let at_zero = ev.evaluate(&graph, NodeId::new(3), &frame_ctx(0)).unwrap();
        let at_seven = ev.evaluate(&graph, NodeId::new(3), &frame_ctx(7)).unwrap();

        assert_eq!(weight_of(&at_zero), 0.0);
        assert_eq!(
            weight_of(&at_seven),
            7.0,
            "the picture froze: the time field was served from the timeless cache"
        );
    }

    #[test]
    fn apply_processor_modulates_geometry_attribute() {
        let node = Node::new(NodeId::new(1), "field.apply")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_input("field", &[DataTypeId::FIELD])
            .with_param("target", ParameterValue::String("weight".into()))
            .with_param("amount", ParameterValue::Float(0.5));
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0)]);
        geometry
            .points_mut()
            .insert("weight", AttributeArray::F32(vec![2.0]))
            .unwrap();
        let geometry: Arc<dyn NodeData> = Arc::new(geometry);
        let field: Arc<dyn NodeData> = Arc::new(FieldValue::new(ConstantField(6.0)));
        let output = run(
            &node,
            Arc::new(ApplyFieldProcessor::from_node(&node)),
            &[geometry, field],
        );
        assert_eq!(
            output
                .downcast_ref::<Geometry>()
                .unwrap()
                .points()
                .get("weight")
                .unwrap()
                .as_f32("weight")
                .unwrap(),
            &[4.0]
        );
    }

    // ---- field transforms: length / component / compose / angle -----------

    use ravel_core::geometry::Field;
    use ravel_core::registry::builtin::{
        FIELD_COMPONENTS, FIELD_COMPOSE_VEC2, FIELD_COMPOSE_VEC3, FIELD_COMPOSE_VEC4,
        VECTOR_CONSTRUCT_VEC3, VECTOR_LENGTH, VECTOR_SPLIT_VEC2, VECTOR_SPLIT_VEC3,
        VECTOR_SPLIT_VEC4,
    };
    use ravel_core::types::{PortRecord, Scalar, Vec3, Vec4};

    /// A field answering a fixed column, so a test can hand a transform a
    /// source of any attribute type.
    struct ConstantArrayField(AttributeArray);

    impl Field for ConstantArrayField {
        fn byte_size(&self) -> u64 {
            size_of::<Self>() as u64
        }

        fn sample(&self, _input: &FieldSample<'_>) -> AttributeArray {
            self.0.clone()
        }
    }

    fn vector_field(column: AttributeArray) -> Arc<dyn NodeData> {
        Arc::new(FieldValue::new(ConstantArrayField(column)))
    }

    fn scalar_field(value: f32) -> Arc<dyn NodeData> {
        Arc::new(FieldValue::new(ConstantField(value)))
    }

    /// One of the single-input transforms, with its parameters set.
    fn transform_node(type_key: &str, params: &[(&str, ParameterValue)]) -> Node {
        let mut node = Node::new(NodeId::new(1), type_key)
            .with_input("field", &[DataTypeId::FIELD])
            .with_output("field", DataTypeId::FIELD);
        for (key, value) in params {
            node = node.with_param(*key, value.clone());
        }
        node
    }

    /// A `field.compose` node of `components` arity, one `FIELD` input per
    /// component in `x`, `y`, `z`, `w` order.
    fn compose_node(type_key: &str, components: usize) -> Node {
        let mut node = Node::new(NodeId::new(1), type_key).with_output("field", DataTypeId::FIELD);
        for key in &FIELD_COMPONENTS[..components] {
            node = node.with_input(*key, &[DataTypeId::FIELD]);
        }
        node
    }

    /// The whole column a field answers, not just its scalar reading.
    fn typed_sample(value: &dyn NodeData) -> AttributeArray {
        value
            .downcast_ref::<FieldValue>()
            .unwrap()
            .sample(&FieldSample::positions_only(&[Vec2(0.25, 0.75)], &ctx()))
    }

    fn sample_vec2(value: &dyn NodeData, positions: &[Vec2]) -> Vec<Vec2> {
        match value
            .downcast_ref::<FieldValue>()
            .expect("a FieldValue")
            .sample(&FieldSample::positions_only(positions, &ctx()))
        {
            AttributeArray::Vec2(values) => values,
            other => panic!("expected a Vec2 field, got {:?}", other.attr_type()),
        }
    }

    #[test]
    fn direction_to_returns_unit_vectors_and_reads_the_target_parameter() {
        let node = Node::new(NodeId::new(1), "field.direction_to")
            .with_input("target_geometry", &[DataTypeId::GEOMETRY])
            .with_output("field", DataTypeId::FIELD)
            .with_param("target", ParameterValue::vec2(1.25, 0.75));
        let output = run(&node, Arc::new(DirectionToFieldProcessor), &[]);
        let vector = sample_vec2(output.as_ref(), &[Vec2(0.25, 0.75)])[0];
        assert_eq!(vector, Vec2(1.0, 0.0));
        assert!((vector.0.hypot(vector.1) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn direction_to_prefers_the_center_of_connected_target_geometry() {
        let node = Node::new(NodeId::new(1), "field.direction_to")
            .with_input("target_geometry", &[DataTypeId::GEOMETRY])
            .with_output("field", DataTypeId::FIELD)
            .with_param("target", ParameterValue::vec2(-100.0, -100.0));
        let geometry: Arc<dyn NodeData> = Arc::new(Geometry::from_points(vec![
            Vec2(2.25, 0.75),
            Vec2(4.25, 0.75),
        ]));
        let output = run(&node, Arc::new(DirectionToFieldProcessor), &[geometry]);
        assert_eq!(
            sample_vec2(output.as_ref(), &[Vec2(0.25, 0.75)])[0],
            Vec2(1.0, 0.0)
        );
    }

    #[test]
    fn curl_noise_is_reproducible_and_has_near_zero_divergence() {
        let node = Node::new(NodeId::new(1), "field.curl_noise")
            .with_output("field", DataTypeId::FIELD)
            .with_param("seed", ParameterValue::Int(23))
            .with_param("frequency", ParameterValue::Float(1.7))
            .with_param("octaves", ParameterValue::Int(3))
            .with_param("step", ParameterValue::Float(0.02));
        let first = run(&node, Arc::new(CurlNoiseFieldProcessor), &[]);
        let second = run(&node, Arc::new(CurlNoiseFieldProcessor), &[]);
        let positions = [Vec2(0.13, 0.71), Vec2(-2.4, 8.1), Vec2(31.0, -0.25)];
        assert_eq!(
            sample_vec2(first.as_ref(), &positions),
            sample_vec2(second.as_ref(), &positions)
        );

        let step = 0.02;
        for position in positions {
            let x_plus = sample_vec2(first.as_ref(), &[Vec2(position.0 + step, position.1)])[0];
            let x_minus = sample_vec2(first.as_ref(), &[Vec2(position.0 - step, position.1)])[0];
            let y_plus = sample_vec2(first.as_ref(), &[Vec2(position.0, position.1 + step)])[0];
            let y_minus = sample_vec2(first.as_ref(), &[Vec2(position.0, position.1 - step)])[0];
            let divergence = (x_plus.0 - x_minus.0 + y_plus.1 - y_minus.1) / (2.0 * step);
            assert!(
                divergence.abs() < 1e-3,
                "divergence at {position:?}: {divergence}"
            );
        }
    }

    #[test]
    fn gradient_matches_the_analytic_gradient_of_a_scalar_field() {
        let source: Arc<dyn NodeData> = Arc::new(FieldValue::new(
            ravel_core::geometry::ExpressionField::new("@P.x * 2 + @P.y * 3", 0.0),
        ));
        let node = Node::new(NodeId::new(1), "field.gradient")
            .with_input("field", &[DataTypeId::FIELD])
            .with_output("field", DataTypeId::FIELD)
            .with_param("step", ParameterValue::Float(0.01));
        let output = run(&node, Arc::new(GradientFieldProcessor), &[source]);
        let values = sample_vec2(output.as_ref(), &[Vec2(0.25, 0.75), Vec2(-4.0, 2.0)]);
        for value in values {
            assert!((value.0 - 2.0).abs() < 1e-4, "x gradient: {value:?}");
            assert!((value.1 - 3.0).abs() < 1e-4, "y gradient: {value:?}");
        }
    }

    #[test]
    fn radial_field_can_follow_the_tangent_direction() {
        let node = Node::new(NodeId::new(1), "field.radial")
            .with_output("field", DataTypeId::FIELD)
            .with_param("center", ParameterValue::vec2(0.0, 0.0))
            .with_param("mode", ParameterValue::String("tangent".into()));
        let output = run(&node, Arc::new(RadialFieldProcessor), &[]);
        assert_eq!(
            sample_vec2(output.as_ref(), &[Vec2(1.0, 0.0)])[0],
            Vec2(-0.0, 1.0)
        );
    }

    /// `field.length` of a one-element column.
    fn length_of(column: AttributeArray) -> f32 {
        sample(
            run(
                &transform_node("field.length", &[]),
                Arc::new(LengthFieldProcessor),
                &[vector_field(column)],
            )
            .as_ref(),
        )[0]
    }

    /// `field.angle` of a one-element column.
    fn angle_of(column: AttributeArray) -> f32 {
        sample(
            run(
                &transform_node("field.angle", &[]),
                Arc::new(AngleFieldProcessor),
                &[vector_field(column)],
            )
            .as_ref(),
        )[0]
    }

    /// `field.component` of a field value, selecting the component `spec`
    /// names.
    fn component_of(source: Arc<dyn NodeData>, spec: &str) -> f32 {
        sample(
            run(
                &transform_node(
                    "field.component",
                    &[("component", ParameterValue::String(spec.into()))],
                ),
                Arc::new(ComponentFieldProcessor),
                &[source],
            )
            .as_ref(),
        )[0]
    }

    #[test]
    fn length_is_the_magnitude_of_a_vector_field() {
        for (column, expected) in [
            (AttributeArray::Vec2(vec![Vec2(3.0, 4.0)]), 5.0),
            (AttributeArray::Vec3(vec![Vec3(2.0, -3.0, 6.0)]), 7.0),
            (AttributeArray::Vec4(vec![Vec4(1.0, 1.0, 1.0, 1.0)]), 2.0),
            // A scalar field is one component wide, so its length is |value|.
            (AttributeArray::F32(vec![-4.0]), 4.0),
        ] {
            let got = length_of(column);
            assert!((got - expected).abs() < 1e-6, "{got} is not {expected}");
        }
    }

    /// Summing the raw squares overflows at `f32::MAX` and flushes to zero at
    /// `f32::MIN_POSITIVE`, even though both lengths are exactly
    /// representable. The value-domain `vector.length` scales by the largest
    /// component to avoid it; the field version has to do the same, or the
    /// two nodes disagree on the inputs where it matters most.
    #[test]
    fn length_survives_components_at_the_edges_of_the_range() {
        for (column, expected) in [
            (AttributeArray::Vec2(vec![Vec2(f32::MAX, 0.0)]), f32::MAX),
            (
                AttributeArray::Vec2(vec![Vec2(f32::MIN_POSITIVE, 0.0)]),
                f32::MIN_POSITIVE,
            ),
            (
                AttributeArray::Vec3(vec![Vec3(0.0, -f32::MAX, 0.0)]),
                f32::MAX,
            ),
        ] {
            assert_eq!(length_of(column), expected);
        }
    }

    /// The zero vector has no length and no direction. Both answer `0` rather
    /// than a NaN — a NaN here travels silently into every attribute the
    /// field is applied to.
    #[test]
    fn the_zero_vector_has_length_zero_and_angle_zero() {
        let zero = || AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]);
        assert_eq!(length_of(zero()), 0.0);
        assert_eq!(angle_of(zero()), 0.0);
        assert!(angle_of(zero()).is_finite(), "atan2(0, 0) must not be NaN");
    }

    #[test]
    fn component_reads_the_component_its_parameter_names() {
        let source = || vector_field(AttributeArray::Vec3(vec![Vec3(1.0, 2.0, 3.0)]));
        for (spec, expected) in [("x", 1.0), ("y", 2.0), ("z", 3.0)] {
            assert_eq!(component_of(source(), spec), expected, "{spec}");
        }
    }

    /// A component the source does not carry reads zero. The guard matters
    /// because the shared component reader *broadcasts* an `F32` column
    /// across every slot — that is the promotion rule binary combination
    /// wants, and here it would make `y` answer the `x` value.
    #[test]
    fn a_component_the_source_lacks_is_zero() {
        assert_eq!(
            component_of(
                vector_field(AttributeArray::Vec2(vec![Vec2(1.0, 2.0)])),
                "z"
            ),
            0.0
        );
        assert_eq!(
            component_of(vector_field(AttributeArray::F32(vec![5.0])), "y"),
            0.0
        );
    }

    /// `compose` then `component` returns each scalar unchanged, at every
    /// arity — and the composed column really is the vector type its
    /// `type_key` promises.
    #[test]
    fn compose_and_component_round_trip() {
        for (type_key, components, expected_type) in [
            (FIELD_COMPOSE_VEC2, 2, AttributeType::Vec2),
            (FIELD_COMPOSE_VEC3, 3, AttributeType::Vec3),
            (FIELD_COMPOSE_VEC4, 4, AttributeType::Vec4),
        ] {
            let values = [1.5f32, -2.5, 0.25, -7.0];
            let sources: Vec<Arc<dyn NodeData>> = values[..components]
                .iter()
                .map(|value| scalar_field(*value))
                .collect();
            let composed = run(
                &compose_node(type_key, components),
                Arc::new(ComposeFieldProcessor::new(components)),
                &sources,
            );
            assert_eq!(
                typed_sample(composed.as_ref()).attr_type(),
                expected_type,
                "{type_key}"
            );
            for (slot, expected) in values[..components].iter().enumerate() {
                assert_eq!(
                    component_of(composed.clone(), FIELD_COMPONENTS[slot]),
                    *expected,
                    "{type_key} component {slot}"
                );
            }
        }
    }

    /// An unconnected slot is the typed zero of a `FIELD` port, so a
    /// half-wired compose answers zeros there rather than failing.
    ///
    /// The slot is left as `None` — wiring a `ConstantField(0.0)` into it
    /// instead would exercise the connected path and pass whether or not the
    /// unconnected one works at all.
    #[test]
    fn an_unwired_compose_slot_is_zero() {
        let composed = run_opt(
            &compose_node(FIELD_COMPOSE_VEC2, 2),
            Arc::new(ComposeFieldProcessor::new(2)),
            &[Some(scalar_field(4.0)), None],
        )
        .unwrap_or_else(|e| panic!("a half-wired compose evaluates: {e}"));
        assert_eq!(component_of(composed.clone(), "x"), 4.0);
        assert_eq!(component_of(composed, "y"), 0.0);
    }

    /// A slot that *is* wired but carries something other than a field is a
    /// wiring mistake, not an empty slot: it still errors.
    #[test]
    fn a_compose_slot_wired_to_a_non_field_is_an_error() {
        let err = run_opt(
            &compose_node(FIELD_COMPOSE_VEC2, 2),
            Arc::new(ComposeFieldProcessor::new(2)),
            &[
                Some(scalar_field(4.0)),
                Some(Arc::new(Scalar(1.0)) as Arc<dyn NodeData>),
            ],
        )
        .err()
        .expect("a non-field on a field port is a wiring mistake");
        // The evaluator wraps the processor's message, so the contract this
        // pins is "it fails" — not the wording. Zero-filling here instead
        // would turn a mis-wired port into a silent zero component.
        assert!(err.to_string().contains("node:1"), "{err}");
    }

    /// `field.angle` is `atan2(y, x)`: the quarter turns land exactly, the
    /// sign follows `y` across the negative x axis, and every answer is
    /// inside `-π..=π`.
    #[test]
    fn angle_answers_atan2_within_minus_pi_to_pi() {
        use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};
        for (vector, expected) in [
            (Vec2(1.0, 0.0), 0.0),
            (Vec2(1.0, 1.0), FRAC_PI_4),
            (Vec2(0.0, 1.0), FRAC_PI_2),
            (Vec2(-1.0, 1.0), 3.0 * FRAC_PI_4),
            (Vec2(-1.0, 0.0), PI),
            (Vec2(-1.0, -0.0), -PI),
            (Vec2(-1.0, -1.0), -3.0 * FRAC_PI_4),
            (Vec2(0.0, -1.0), -FRAC_PI_2),
        ] {
            let got = angle_of(AttributeArray::Vec2(vec![vector]));
            assert!(
                (got - expected).abs() < 1e-6,
                "{vector:?}: {got} is not {expected}"
            );
            assert!(
                (-PI..=PI).contains(&got),
                "{vector:?}: {got} is outside atan2's range"
            );
        }
    }

    /// A scalar source has no `y`, and the shared component reader
    /// *broadcasts* an `F32` column into every slot. Unclamped, `atan2(x, x)`
    /// would answer π/4 for every positive value — a plausible-looking wrong
    /// answer rather than an obvious one — so the arity clamp is pinned here.
    #[test]
    fn angle_of_a_scalar_field_reads_y_as_zero() {
        use std::f32::consts::PI;
        assert_eq!(angle_of(AttributeArray::F32(vec![2.0])), 0.0);
        let negative = angle_of(AttributeArray::F32(vec![-2.0]));
        assert!((negative - PI).abs() < 1e-6, "{negative} is not pi");
    }

    // ---- agreement with the value-domain `vector.*` nodes -----------------
    //
    // `vector-field-plan.md` unit 8 could not pin these when it landed,
    // because the field side did not exist yet. The two implementations are
    // separate — one transforms a column in `ravel-core`, the other a wire
    // value in `ravel-nodes` — so nothing but a test keeps them answering the
    // same thing.

    /// Runs a value-domain processor with no parameters.
    fn value_domain(
        processor: &dyn NodeProcessor,
        node: &Node,
        inputs: Vec<Option<Arc<dyn NodeData>>>,
    ) -> Arc<dyn NodeData> {
        processor
            .process(
                node,
                &ctx(),
                &inputs,
                &ResolvedParams::default(),
                &mut Evaluator::new(),
            )
            .unwrap()
    }

    /// `field.length` and `vector.length` agree bit for bit, including the
    /// edge-of-range inputs that separate the scaled magnitude from a naive
    /// sum of squares.
    #[test]
    fn field_length_answers_what_vector_length_answers() {
        for (column, value) in [
            (
                AttributeArray::Vec2(vec![Vec2(3.0, 4.0)]),
                Arc::new(Vec2(3.0, 4.0)) as Arc<dyn NodeData>,
            ),
            (
                AttributeArray::Vec3(vec![Vec3(2.0, -3.0, 6.0)]),
                Arc::new(Vec3(2.0, -3.0, 6.0)),
            ),
            (
                AttributeArray::Vec4(vec![Vec4(1.0, -2.0, 3.0, -4.0)]),
                Arc::new(Vec4(1.0, -2.0, 3.0, -4.0)),
            ),
            (
                AttributeArray::Vec2(vec![Vec2(f32::MAX, 0.0)]),
                Arc::new(Vec2(f32::MAX, 0.0)),
            ),
            (
                AttributeArray::Vec2(vec![Vec2(0.0, f32::MIN_POSITIVE)]),
                Arc::new(Vec2(0.0, f32::MIN_POSITIVE)),
            ),
        ] {
            let from_field = length_of(column);
            let from_value = value_domain(
                &crate::vector::VectorLengthProcessor,
                &Node::new(NodeId::new(1), VECTOR_LENGTH),
                vec![Some(value)],
            )
            .downcast_ref::<Scalar>()
            .expect("vector.length answers a Scalar")
            .0;
            assert_eq!(
                from_field.to_bits(),
                from_value.to_bits(),
                "field.length said {from_field}, vector.length said {from_value}"
            );
        }
    }

    /// `field.component` and `vector.split` take the same components out of
    /// the same vector.
    #[test]
    fn field_component_answers_what_vector_split_answers() {
        use crate::vector::{VectorArity, VectorSplitProcessor};
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);

        // Every arity, because `vector.split` is three separate `type_key`s
        // on the value side and three separate column types on the field
        // side: pinning one of them leaves the other two free to drift. The
        // non-finite rows pin the "IEEE passes straight through" claim, which
        // is where two independent implementations part company first.
        let cases: Vec<(VectorArity, &str, AttributeArray, Arc<dyn NodeData>)> = vec![
            (
                VectorArity::Vec2,
                VECTOR_SPLIT_VEC2,
                AttributeArray::Vec2(vec![Vec2(1.5, -2.5)]),
                Arc::new(Vec2(1.5, -2.5)),
            ),
            (
                VectorArity::Vec2,
                VECTOR_SPLIT_VEC2,
                AttributeArray::Vec2(vec![Vec2(f32::INFINITY, f32::NAN)]),
                Arc::new(Vec2(f32::INFINITY, f32::NAN)),
            ),
            (
                VectorArity::Vec3,
                VECTOR_SPLIT_VEC3,
                AttributeArray::Vec3(vec![Vec3(1.5, -2.5, 0.25)]),
                Arc::new(Vec3(1.5, -2.5, 0.25)),
            ),
            (
                VectorArity::Vec4,
                VECTOR_SPLIT_VEC4,
                AttributeArray::Vec4(vec![Vec4(1.5, -2.5, 0.25, -0.75)]),
                Arc::new(Vec4(1.5, -2.5, 0.25, -0.75)),
            ),
        ];

        for (arity, type_key, column, value) in cases {
            let node = registry
                .create_node(type_key, NodeId::new(1))
                .unwrap_or_else(|| panic!("{type_key} is registered"));
            let record = value_domain(&VectorSplitProcessor::new(arity), &node, vec![Some(value)]);
            let record = record
                .downcast_ref::<PortRecord>()
                .expect("vector.split answers a PortRecord");

            for (slot, name) in FIELD_COMPONENTS[..arity.components()].iter().enumerate() {
                let from_field = component_of(vector_field(column.clone()), name);
                let from_value = record.0[slot]
                    .downcast_ref::<Scalar>()
                    .expect("a split output is a Scalar")
                    .0;
                assert_eq!(
                    from_field.to_bits(),
                    from_value.to_bits(),
                    "{type_key} component {name}"
                );
            }
        }
    }

    /// `field.compose` and `vector.construct` build the same vector out of
    /// the same components.
    #[test]
    fn field_compose_answers_what_vector_construct_answers() {
        let values = [1.5f32, -2.5, 0.25];
        let sources: Vec<Arc<dyn NodeData>> =
            values.iter().map(|value| scalar_field(*value)).collect();
        let composed = run(
            &compose_node(FIELD_COMPOSE_VEC3, 3),
            Arc::new(ComposeFieldProcessor::new(3)),
            &sources,
        );
        let AttributeArray::Vec3(column) = typed_sample(composed.as_ref()) else {
            panic!("field.compose.vec3 answers a Vec3 column");
        };

        // Through the evaluator, so the component parameters are resolved the
        // way `vector.construct` reads them.
        let mut construct = Node::new(NodeId::new(1), VECTOR_CONSTRUCT_VEC3)
            .with_output("vector", DataTypeId::VEC3);
        for (key, value) in FIELD_COMPONENTS[..3].iter().zip(values) {
            construct = construct.with_param(*key, ParameterValue::Float(value));
        }
        let graph = Graph::new().add_node(construct).unwrap();
        let mut evaluator = Evaluator::new();
        evaluator.register(
            NodeId::new(1),
            Arc::new(crate::vector::VectorConstructProcessor::new(
                crate::vector::VectorArity::Vec3,
            )),
        );
        let constructed = *evaluator
            .evaluate(&graph, NodeId::new(1), &ctx())
            .unwrap()
            .downcast_ref::<Vec3>()
            .expect("vector.construct.vec3 answers a Vec3");

        assert_eq!(column[0], constructed);
    }

    #[test]
    fn the_field_transforms_are_not_time_dependent() {
        assert!(!LengthFieldProcessor.is_time_dependent());
        assert!(!AngleFieldProcessor.is_time_dependent());
        assert!(!ComponentFieldProcessor.is_time_dependent());
        assert!(!ComposeFieldProcessor::new(2).is_time_dependent());
    }
}
