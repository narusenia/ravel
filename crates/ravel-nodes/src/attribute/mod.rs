// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node-graph adapters for pure geometry attribute operations.

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{
    AggregateMode, AttributeArray, AttributeValue, Domain, Geometry, TransferMode, attribute_set,
    attribute_transfer, path_sample, promote_attribute,
};
use ravel_core::graph::Node;
use ravel_core::types::{Color, NodeData, Vec2, Vec3, Vec4};
use std::sync::Arc;

pub struct AttributeSetProcessor;

impl AttributeSetProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for AttributeSetProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "attribute.set")?;
        let x = params.f32_or("value", 0.0);
        let y = params.f32_or("value_y", 0.0);
        let z = params.f32_or("value_z", 0.0);
        let w = params.f32_or("value_w", 0.0);
        let value = match params.str_or("type", "f32") {
            "vec2" => AttributeValue::Vec2(Vec2(x, y)),
            "vec3" => AttributeValue::Vec3(Vec3(x, y, z)),
            "vec4" => AttributeValue::Vec4(Vec4(x, y, z, w)),
            "color" => AttributeValue::Color(Color::new(x, y, z, w)),
            "i32" => AttributeValue::I32(params.i32_or("int_value", 0)),
            "bool" => AttributeValue::Bool(params.bool_or("bool_value", false)),
            "string" => AttributeValue::Str(params.str_or("string_value", "").to_owned()),
            _ => AttributeValue::F32(x),
        };
        let domain = domain_param(params, "domain", Domain::Point);
        let name = params.str_or("name", "value");
        Ok(Arc::new(attribute_set(geometry, domain, name, value)?))
    }
}

pub struct AttributePromoteProcessor;

impl AttributePromoteProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for AttributePromoteProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "attribute.promote")?;
        let mode = match params.str_or("aggregate", "average") {
            "max" => AggregateMode::Max,
            "first" => AggregateMode::First,
            _ => AggregateMode::Average,
        };
        Ok(Arc::new(promote_attribute(
            geometry,
            domain_param(params, "source_domain", Domain::Point),
            domain_param(params, "target_domain", Domain::Detail),
            params.str_or("name", "value"),
            mode,
        )?))
    }
}

pub struct AttributeTransferProcessor;

impl AttributeTransferProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for AttributeTransferProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let target = geometry_input(inputs, 0, "attribute.transfer")?;
        let source = geometry_input(inputs, 1, "attribute.transfer")?;
        let mode = match params.str_or("mode", "nearest") {
            "distance_weighted" => TransferMode::DistanceWeighted,
            _ => TransferMode::Nearest,
        };
        Ok(Arc::new(attribute_transfer(
            target,
            domain_param(params, "target_domain", Domain::Point),
            source,
            domain_param(params, "source_domain", Domain::Point),
            params.str_or("name", "value"),
            mode,
        )?))
    }
}

pub struct PathSampleProcessor;

impl PathSampleProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for PathSampleProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let path = geometry_input(inputs, 0, "attribute.path_sample")?;
        let sample = path_sample(path, params.f32_or("distance", 0.0))?;
        let mut result = Geometry::from_points(vec![sample.position]);
        result
            .points_mut()
            .insert("tangent", AttributeArray::Vec2(vec![sample.tangent]))?;
        result
            .points_mut()
            .insert("normal", AttributeArray::Vec2(vec![sample.normal]))?;
        Ok(Arc::new(result))
    }
}

fn geometry_input<'a>(
    inputs: &'a [Option<Arc<dyn NodeData>>],
    index: usize,
    processor: &str,
) -> anyhow::Result<&'a Geometry> {
    inputs
        .get(index)
        .and_then(|input| input.as_ref())
        .and_then(|input| input.downcast_ref::<Geometry>())
        .with_context(|| format!("{processor}: input {index} is not Geometry"))
}

fn domain_param(params: &ResolvedParams, key: &str, default: Domain) -> Domain {
    match params.str_or(key, "") {
        "instance" => Domain::Instance,
        "detail" => Domain::Detail,
        "point" => Domain::Point,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scatter::GridProcessor;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (100, 100))
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

    #[test]
    fn set_processor_writes_configured_constant() {
        let node = Node::new(NodeId::new(1), "attribute.set")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_param("name", ParameterValue::String("weight".into()))
            .with_param("value", ParameterValue::Float(2.5));
        let source =
            Node::new(NodeId::new(2), "test.source").with_output("out", DataTypeId::GEOMETRY);
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(node.clone())
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(1),
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        let geometry: Arc<dyn NodeData> = Arc::new(Geometry::from_points(vec![Vec2(0.0, 0.0); 2]));
        ev.register(NodeId::new(2), Arc::new(StubSource(geometry)));
        ev.register(NodeId::new(1), Arc::new(AttributeSetProcessor));

        let output = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
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
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_output("geometry", DataTypeId::GEOMETRY)
            .with_param("name", ParameterValue::String("weight".into()))
            .with_param("value", ParameterValue::Float(2.5));
        let grid_node = Node::new(NodeId::new(2), "scatter.grid")
            .with_input("source", &[DataTypeId::GEOMETRY])
            .with_param("count_x", ParameterValue::Int(2))
            .with_param("count_y", ParameterValue::Int(1));
        let source =
            Node::new(NodeId::new(3), "test.source").with_output("out", DataTypeId::GEOMETRY);
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(set_node)
            .unwrap()
            .add_node(grid_node)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(3),
                OutputPortIndex(0),
                NodeId::new(1),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        let geometry: Arc<dyn NodeData> = Arc::new(Geometry::from_points(vec![Vec2(0.0, 0.0)]));
        ev.register(NodeId::new(3), Arc::new(StubSource(geometry)));
        ev.register(NodeId::new(1), Arc::new(AttributeSetProcessor));
        ev.register(NodeId::new(2), Arc::new(GridProcessor));

        let scattered = ev.evaluate(&graph, NodeId::new(2), &ctx()).unwrap();
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
