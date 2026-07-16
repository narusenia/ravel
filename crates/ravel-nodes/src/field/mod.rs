// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node-graph adapters for the headless field implementations in `ravel-core`.

use ravel_core::eval::{EvalContext, NodeProcessor};
use ravel_core::geometry::{
    AddField, BlendField, CurveRemapField, ExpressionField, FalloffField, FalloffShape, FieldValue,
    Geometry, MaxField, MultiplyField, NoiseField, apply_field,
};
use ravel_core::graph::{Node, Parameter, ParameterValue};
use ravel_core::types::{NodeData, Vec2};

pub struct NoiseFieldProcessor {
    field: NoiseField,
}

impl NoiseFieldProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            field: NoiseField {
                seed: int_param(&node.parameters, "seed", 0) as u32,
                frequency: float_param(&node.parameters, "frequency", 1.0),
                octaves: int_param(&node.parameters, "octaves", 1).max(1) as u32,
            },
        }
    }
}

impl NodeProcessor for NoiseFieldProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        _inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        Ok(Box::new(FieldValue::new(self.field)))
    }
}

pub struct FalloffFieldProcessor {
    field: FalloffField,
}

impl FalloffFieldProcessor {
    pub fn from_node(node: &Node) -> Self {
        let center = Vec2(
            float_param(&node.parameters, "center_x", 0.0),
            float_param(&node.parameters, "center_y", 0.0),
        );
        let shape = match string_param(&node.parameters, "shape", "sphere") {
            "linear" => FalloffShape::Linear {
                direction: Vec2(
                    float_param(&node.parameters, "direction_x", 1.0),
                    float_param(&node.parameters, "direction_y", 0.0),
                ),
            },
            _ => FalloffShape::Sphere,
        };
        Self {
            field: FalloffField {
                center,
                inner_radius: float_param(&node.parameters, "inner_radius", 0.0),
                outer_radius: float_param(&node.parameters, "outer_radius", 1.0),
                shape,
            },
        }
    }
}

impl NodeProcessor for FalloffFieldProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        _inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        Ok(Box::new(FieldValue::new(self.field)))
    }
}

pub struct CurveRemapFieldProcessor {
    points: Vec<(f32, f32)>,
}

impl CurveRemapFieldProcessor {
    pub fn from_node(node: &Node) -> Self {
        let points = parse_curve(string_param(&node.parameters, "points", "0:0,1:1"));
        Self { points }
    }
}

impl NodeProcessor for CurveRemapFieldProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let source = field_input(inputs, 0, "field.curve_remap")?;
        Ok(Box::new(FieldValue::new(CurveRemapField::new(
            source,
            self.points.clone(),
        ))))
    }
}

pub struct ExpressionFieldProcessor {
    field: ExpressionField,
}

impl ExpressionFieldProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            field: ExpressionField {
                expression: string_param(&node.parameters, "expression", "").to_owned(),
                default: float_param(&node.parameters, "default", 0.0),
            },
        }
    }
}

impl NodeProcessor for ExpressionFieldProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        _inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        Ok(Box::new(FieldValue::new(self.field.clone())))
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
                _ctx: &EvalContext,
                inputs: &[&dyn NodeData],
            ) -> anyhow::Result<Box<dyn NodeData>> {
                let left = field_input(inputs, 0, $name)?;
                let right = field_input(inputs, 1, $name)?;
                Ok(Box::new(FieldValue::new($field { left, right })))
            }
        }
    };
}

binary_processor!(AddFieldProcessor, AddField, "field.add");
binary_processor!(MultiplyFieldProcessor, MultiplyField, "field.multiply");
binary_processor!(MaxFieldProcessor, MaxField, "field.max");

pub struct BlendFieldProcessor {
    amount: f32,
}

pub struct ApplyFieldProcessor {
    domain: ravel_core::geometry::Domain,
    target: String,
    amount: f32,
}

impl ApplyFieldProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            domain: match string_param(&node.parameters, "domain", "point") {
                "instance" => ravel_core::geometry::Domain::Instance,
                "detail" => ravel_core::geometry::Domain::Detail,
                _ => ravel_core::geometry::Domain::Point,
            },
            target: string_param(&node.parameters, "target", "value").to_owned(),
            amount: float_param(&node.parameters, "amount", 1.0),
        }
    }
}

impl NodeProcessor for ApplyFieldProcessor {
    fn process(
        &self,
        ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let geometry = inputs
            .first()
            .and_then(|input| input.downcast_ref::<Geometry>())
            .ok_or_else(|| anyhow::anyhow!("field.apply: input 0 is not Geometry"))?;
        let field = field_input(inputs, 1, "field.apply")?;
        Ok(Box::new(apply_field(
            geometry,
            self.domain,
            &self.target,
            field.0.as_ref(),
            self.amount,
            ctx,
        )?))
    }
}

impl BlendFieldProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            amount: float_param(&node.parameters, "amount", 0.5),
        }
    }
}

impl NodeProcessor for BlendFieldProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let left = field_input(inputs, 0, "field.blend")?;
        let right = field_input(inputs, 1, "field.blend")?;
        Ok(Box::new(FieldValue::new(BlendField {
            left,
            right,
            amount: self.amount,
        })))
    }
}

fn field_input(
    inputs: &[&dyn NodeData],
    index: usize,
    processor: &str,
) -> anyhow::Result<FieldValue> {
    inputs
        .get(index)
        .and_then(|input| input.downcast_ref::<FieldValue>())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{processor}: input {index} is not a FieldValue"))
}

fn float_param(params: &[Parameter], key: &str, default: f32) -> f32 {
    params
        .iter()
        .find(|parameter| parameter.key == key)
        .and_then(|parameter| match parameter.value {
            ParameterValue::Float(value) => Some(value),
            _ => None,
        })
        .unwrap_or(default)
}

fn int_param(params: &[Parameter], key: &str, default: i32) -> i32 {
    params
        .iter()
        .find(|parameter| parameter.key == key)
        .and_then(|parameter| match parameter.value {
            ParameterValue::Int(value) => Some(value),
            _ => None,
        })
        .unwrap_or(default)
}

fn string_param<'a>(params: &'a [Parameter], key: &str, default: &'a str) -> &'a str {
    params
        .iter()
        .find(|parameter| parameter.key == key)
        .and_then(|parameter| match &parameter.value {
            ParameterValue::String(value) => Some(value.as_str()),
            _ => None,
        })
        .unwrap_or(default)
}

fn parse_curve(value: &str) -> Vec<(f32, f32)> {
    let points = value
        .split(',')
        .filter_map(|point| {
            let (input, output) = point.split_once(':')?;
            Some((input.trim().parse().ok()?, output.trim().parse().ok()?))
        })
        .collect::<Vec<_>>();
    if points.is_empty() {
        vec![(0.0, 0.0), (1.0, 1.0)]
    } else {
        points
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::geometry::{AttributeArray, Field};
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::types::FrameRate;

    #[derive(Clone, Copy)]
    struct ConstantField(f32);

    impl Field for ConstantField {
        fn sample(&self, positions: &[Vec2], _ctx: &EvalContext) -> AttributeArray {
            AttributeArray::F32(vec![self.0; positions.len()])
        }
    }

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    fn sample(value: &dyn NodeData) -> Vec<f32> {
        value
            .downcast_ref::<FieldValue>()
            .unwrap()
            .sample(&[Vec2(0.25, 0.75)], &ctx())
            .as_f32("sample")
            .unwrap()
            .to_vec()
    }

    #[test]
    fn noise_processor_reads_node_parameters() {
        let node = Node::new(NodeId::new(1), "field.noise")
            .with_output("field", DataTypeId::FIELD)
            .with_param("seed", ParameterValue::Int(19))
            .with_param("frequency", ParameterValue::Float(2.5))
            .with_param("octaves", ParameterValue::Int(3));
        let processor = NoiseFieldProcessor::from_node(&node);

        let first = processor.process(&ctx(), &[]).unwrap();
        let second = processor.process(&ctx(), &[]).unwrap();
        assert_eq!(sample(first.as_ref()), sample(second.as_ref()));
    }

    #[test]
    fn curve_processor_wraps_its_field_input() {
        let node = Node::new(NodeId::new(1), "field.curve_remap")
            .with_input("field", &[DataTypeId::FIELD])
            .with_output("field", DataTypeId::FIELD)
            .with_param("points", ParameterValue::String("0:0,1:10".into()));
        let processor = CurveRemapFieldProcessor::from_node(&node);
        let source = FieldValue::new(ConstantField(0.25));

        let output = processor.process(&ctx(), &[&source]).unwrap();
        assert_eq!(sample(output.as_ref()), vec![2.5]);
    }

    #[test]
    fn blend_processor_composes_two_field_inputs() {
        let node = Node::new(NodeId::new(1), "field.blend")
            .with_input("left", &[DataTypeId::FIELD])
            .with_input("right", &[DataTypeId::FIELD])
            .with_output("field", DataTypeId::FIELD)
            .with_param("amount", ParameterValue::Float(0.25));
        let processor = BlendFieldProcessor::from_node(&node);
        let left = FieldValue::new(ConstantField(2.0));
        let right = FieldValue::new(ConstantField(6.0));

        let output = processor.process(&ctx(), &[&left, &right]).unwrap();
        assert_eq!(sample(output.as_ref()), vec![3.0]);
    }

    #[test]
    fn expression_processor_returns_configured_placeholder_default() {
        let node = Node::new(NodeId::new(1), "field.expression")
            .with_output("field", DataTypeId::FIELD)
            .with_param("expression", ParameterValue::String("P.x * 2".into()))
            .with_param("default", ParameterValue::Float(7.0));
        let processor = ExpressionFieldProcessor::from_node(&node);

        let output = processor.process(&ctx(), &[]).unwrap();
        assert_eq!(sample(output.as_ref()), vec![7.0]);
    }

    #[test]
    fn apply_processor_modulates_geometry_attribute() {
        let node = Node::new(NodeId::new(1), "field.apply")
            .with_param("target", ParameterValue::String("weight".into()))
            .with_param("amount", ParameterValue::Float(0.5));
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0)]);
        geometry
            .points_mut()
            .insert("weight", AttributeArray::F32(vec![2.0]))
            .unwrap();
        let field = FieldValue::new(ConstantField(6.0));
        let output = ApplyFieldProcessor::from_node(&node)
            .process(&ctx(), &[&geometry, &field])
            .unwrap();
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
