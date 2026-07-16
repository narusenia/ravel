// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node-graph adapters for the headless field implementations in `ravel-core`.

use ravel_core::eval::{EvalContext, NodeProcessor};
use ravel_core::geometry::{
    AddField, BlendField, CurveRemapField, ExpressionField, FalloffField, FalloffShape, FieldValue,
    MaxField, MultiplyField, NoiseField,
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
