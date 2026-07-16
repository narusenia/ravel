// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node-graph adapters for pure geometry attribute operations.

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, NodeProcessor};
use ravel_core::geometry::{
    AggregateMode, AttributeArray, AttributeValue, Domain, Geometry, TransferMode, attribute_set,
    attribute_transfer, path_sample, promote_attribute,
};
use ravel_core::graph::{Node, Parameter, ParameterValue};
use ravel_core::types::{Color, NodeData, Vec2, Vec3, Vec4};

pub struct AttributeSetProcessor {
    domain: Domain,
    name: String,
    value: AttributeValue,
}

impl AttributeSetProcessor {
    pub fn from_node(node: &Node) -> Self {
        let parameters = &node.parameters;
        let value_type = string_param(parameters, "type", "f32");
        let x = float_param(parameters, "value", 0.0);
        let y = float_param(parameters, "value_y", 0.0);
        let z = float_param(parameters, "value_z", 0.0);
        let w = float_param(parameters, "value_w", 0.0);
        let value = match value_type {
            "vec2" => AttributeValue::Vec2(Vec2(x, y)),
            "vec3" => AttributeValue::Vec3(Vec3(x, y, z)),
            "vec4" => AttributeValue::Vec4(Vec4(x, y, z, w)),
            "color" => AttributeValue::Color(Color::new(x, y, z, w)),
            "i32" => AttributeValue::I32(int_param(parameters, "int_value", 0)),
            "bool" => AttributeValue::Bool(bool_param(parameters, "bool_value", false)),
            "string" => {
                AttributeValue::Str(string_param(parameters, "string_value", "").to_owned())
            }
            _ => AttributeValue::F32(x),
        };
        Self {
            domain: domain_param(parameters, "domain", Domain::Point),
            name: string_param(parameters, "name", "value").to_owned(),
            value,
        }
    }
}

impl NodeProcessor for AttributeSetProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "attribute.set")?;
        Ok(Box::new(attribute_set(
            geometry,
            self.domain,
            &self.name,
            self.value.clone(),
        )?))
    }
}

pub struct AttributePromoteProcessor {
    source: Domain,
    target: Domain,
    name: String,
    mode: AggregateMode,
}

impl AttributePromoteProcessor {
    pub fn from_node(node: &Node) -> Self {
        let parameters = &node.parameters;
        Self {
            source: domain_param(parameters, "source_domain", Domain::Point),
            target: domain_param(parameters, "target_domain", Domain::Detail),
            name: string_param(parameters, "name", "value").to_owned(),
            mode: match string_param(parameters, "aggregate", "average") {
                "max" => AggregateMode::Max,
                "first" => AggregateMode::First,
                _ => AggregateMode::Average,
            },
        }
    }
}

impl NodeProcessor for AttributePromoteProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "attribute.promote")?;
        Ok(Box::new(promote_attribute(
            geometry,
            self.source,
            self.target,
            &self.name,
            self.mode,
        )?))
    }
}

pub struct AttributeTransferProcessor {
    source_domain: Domain,
    target_domain: Domain,
    name: String,
    mode: TransferMode,
}

impl AttributeTransferProcessor {
    pub fn from_node(node: &Node) -> Self {
        let parameters = &node.parameters;
        Self {
            source_domain: domain_param(parameters, "source_domain", Domain::Point),
            target_domain: domain_param(parameters, "target_domain", Domain::Point),
            name: string_param(parameters, "name", "value").to_owned(),
            mode: match string_param(parameters, "mode", "nearest") {
                "distance_weighted" => TransferMode::DistanceWeighted,
                _ => TransferMode::Nearest,
            },
        }
    }
}

impl NodeProcessor for AttributeTransferProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let target = geometry_input(inputs, 0, "attribute.transfer")?;
        let source = geometry_input(inputs, 1, "attribute.transfer")?;
        Ok(Box::new(attribute_transfer(
            target,
            self.target_domain,
            source,
            self.source_domain,
            &self.name,
            self.mode,
        )?))
    }
}

pub struct PathSampleProcessor {
    distance: f32,
}

impl PathSampleProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            distance: float_param(&node.parameters, "distance", 0.0),
        }
    }
}

impl NodeProcessor for PathSampleProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        let path = geometry_input(inputs, 0, "attribute.path_sample")?;
        let sample = path_sample(path, self.distance)?;
        let mut result = Geometry::from_points(vec![sample.position]);
        result
            .points_mut()
            .insert("tangent", AttributeArray::Vec2(vec![sample.tangent]))?;
        result
            .points_mut()
            .insert("normal", AttributeArray::Vec2(vec![sample.normal]))?;
        Ok(Box::new(result))
    }
}

fn geometry_input<'a>(
    inputs: &'a [&dyn NodeData],
    index: usize,
    processor: &str,
) -> anyhow::Result<&'a Geometry> {
    inputs
        .get(index)
        .and_then(|input| input.downcast_ref::<Geometry>())
        .with_context(|| format!("{processor}: input {index} is not Geometry"))
}

fn domain_param(parameters: &[Parameter], key: &str, default: Domain) -> Domain {
    match string_param(parameters, key, "") {
        "instance" => Domain::Instance,
        "detail" => Domain::Detail,
        "point" => Domain::Point,
        _ => default,
    }
}

fn float_param(parameters: &[Parameter], key: &str, default: f32) -> f32 {
    parameters
        .iter()
        .find(|parameter| parameter.key == key)
        .and_then(|parameter| match parameter.value {
            ParameterValue::Float(value) => Some(value),
            _ => None,
        })
        .unwrap_or(default)
}

fn int_param(parameters: &[Parameter], key: &str, default: i32) -> i32 {
    parameters
        .iter()
        .find(|parameter| parameter.key == key)
        .and_then(|parameter| match parameter.value {
            ParameterValue::Int(value) => Some(value),
            _ => None,
        })
        .unwrap_or(default)
}

fn bool_param(parameters: &[Parameter], key: &str, default: bool) -> bool {
    parameters
        .iter()
        .find(|parameter| parameter.key == key)
        .and_then(|parameter| match parameter.value {
            ParameterValue::Bool(value) => Some(value),
            _ => None,
        })
        .unwrap_or(default)
}

fn string_param<'a>(parameters: &'a [Parameter], key: &str, default: &'a str) -> &'a str {
    parameters
        .iter()
        .find(|parameter| parameter.key == key)
        .and_then(|parameter| match &parameter.value {
            ParameterValue::String(value) => Some(value.as_str()),
            _ => None,
        })
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scatter::GridProcessor;
    use ravel_core::id::NodeId;
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (100, 100))
    }

    #[test]
    fn set_processor_writes_configured_constant() {
        let node = Node::new(NodeId::new(1), "attribute.set")
            .with_param("name", ParameterValue::String("weight".into()))
            .with_param("value", ParameterValue::Float(2.5));
        let geometry = Geometry::from_points(vec![Vec2(0.0, 0.0); 2]);
        let output = AttributeSetProcessor::from_node(&node)
            .process(&ctx(), &[&geometry])
            .unwrap();
        let output = output.downcast_ref::<Geometry>().unwrap();
        assert_eq!(
            output
                .points()
                .get("weight")
                .unwrap()
                .as_f32("weight")
                .unwrap(),
            &[2.5, 2.5]
        );
    }

    #[test]
    fn attribute_propagates_through_scatter_instance_source() {
        let set_node = Node::new(NodeId::new(1), "attribute.set")
            .with_param("name", ParameterValue::String("weight".into()))
            .with_param("value", ParameterValue::Float(2.5));
        let source = Geometry::from_points(vec![Vec2(0.0, 0.0)]);
        let attributed = AttributeSetProcessor::from_node(&set_node)
            .process(&ctx(), &[&source])
            .unwrap();
        let attributed = attributed.downcast_ref::<Geometry>().unwrap();

        let grid_node = Node::new(NodeId::new(2), "scatter.grid")
            .with_param("count_x", ParameterValue::Int(2))
            .with_param("count_y", ParameterValue::Int(1));
        let scattered = GridProcessor::from_node(&grid_node)
            .process(&ctx(), &[attributed])
            .unwrap();
        let scattered = scattered.downcast_ref::<Geometry>().unwrap();
        let propagated = scattered.instance_source().unwrap();
        assert_eq!(
            propagated
                .points()
                .get("weight")
                .unwrap()
                .as_f32("weight")
                .unwrap(),
            &[2.5]
        );
    }
}
