// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node-graph adapters for the headless field implementations in `ravel-core`.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{
    AddField, AttributeField, BlendField, CombineMode, ComponentMask, CurveRemapField, Domain,
    ExpressionField, FalloffField, FalloffShape, FieldApply, FieldValue, Geometry, MaxField,
    MultiplyField, NoiseField, apply_field,
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
        let shape = match params.str_or("shape", "sphere") {
            "linear" => {
                let [dx, dy, _dz] = params.vec3_or("direction", [1.0, 0.0, 0.0]);
                FalloffShape::Linear {
                    direction: Vec2(dx, dy),
                }
            }
            _ => FalloffShape::Sphere,
        };
        Ok(Arc::new(FieldValue::new(FalloffField {
            center,
            inner_radius: params.f32_or("inner_radius", 0.0),
            outer_radius: params.f32_or("outer_radius", 1.0),
            shape,
        })))
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
            .with_group(params.str_or("group", ""));
        Ok(Arc::new(apply_field(
            geometry,
            &spec,
            field.0.as_ref(),
            ctx,
        )?))
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
    use ravel_core::geometry::{AttributeArray, AttributeSet, ConstantField, FieldSample};
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::param_curve::CurveParam;
    use ravel_core::types::FrameRate;

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
        // Refused and non-compiling sources answer a constant default.
        assert!(!reads_clock("@Cd.r * frame"));
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
}
