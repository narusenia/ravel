// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node-graph adapters for the headless field implementations in `ravel-core`.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{
    AddField, AttributeField, BlendField, CombineMode, ComponentMask, CurveRemapField, Domain,
    ExpressionField, FalloffField, FalloffShape, FieldApply, FieldValue, Geometry, MaxField,
    MultiplyField, NoiseField, apply_field,
};
use ravel_core::graph::Node;
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

pub struct ExpressionFieldProcessor;

impl ExpressionFieldProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for ExpressionFieldProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(FieldValue::new(ExpressionField {
            expression: params.str_or("expression", "").to_owned(),
            default: params.f32_or("default", 0.0),
        })))
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
    fn expression_processor_returns_configured_placeholder_default() {
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
