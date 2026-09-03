// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node-graph adapters for pure geometry attribute operations.

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams, ResolvedValue};
use ravel_core::geometry::{
    AggregateMode, AttributeArray, AttributeValue, CurveUMode, Domain, Geometry, TransferMode,
    attribute_delete, attribute_set, attribute_transfer, curve_u, path_sample, promote_attribute,
};
use ravel_core::graph::Node;
use ravel_core::registry::builtin::{ATTRIBUTE_SET_DEFAULT_TYPE, attribute_set_value_defaults};
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
        // `value` is one parameter whose arity follows `type`: a Channel for
        // `f32`, a Channel2 for `vec2`, and so on (the arity is kept in step
        // by `ravel_core::registry::builtin::dependent_param_updates`).
        let type_name = params.str_or("type", ATTRIBUTE_SET_DEFAULT_TYPE);
        let default = |name: &str, index: usize| {
            attribute_set_value_defaults(name)
                .get(index)
                .copied()
                .unwrap_or(0.0)
        };
        let value = match type_name {
            "vec2" => {
                let [x, y] = params.vec2_or("value", [0.0, 0.0]);
                AttributeValue::Vec2(Vec2(x, y))
            }
            "vec3" => {
                let [x, y, z] = params.vec3_or("value", [0.0, 0.0, 0.0]);
                AttributeValue::Vec3(Vec3(x, y, z))
            }
            "vec4" => {
                let [x, y, z, w] = params.vec4_or("value", [0.0, 0.0, 0.0, 0.0]);
                AttributeValue::Vec4(Vec4(x, y, z, w))
            }
            "color" => {
                let [r, g, b, a] = params.vec4_or("value", [0.0, 0.0, 0.0, default("color", 3)]);
                AttributeValue::Color(Color::new(r, g, b, a))
            }
            "i32" => AttributeValue::I32(params.i32_or("int_value", 0)),
            "bool" => AttributeValue::Bool(params.bool_or("bool_value", false)),
            "string" => AttributeValue::Str(params.str_or("string_value", "").to_owned()),
            _ => AttributeValue::F32(params.f32_or("value", 0.0)),
        };
        let domain = domain_param(params, "domain", Domain::Point);
        let name = params.str_or("name", "value");
        Ok(Arc::new(attribute_set(geometry, domain, name, value)?))
    }
}

/// `attribute.delete`: drops one column from the chosen domain.
///
/// A name the domain does not carry is a no-op; the position column of a
/// position-carrying domain is an error, both decided by
/// [`attribute_delete`].
pub struct AttributeDeleteProcessor;

impl AttributeDeleteProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for AttributeDeleteProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "attribute.delete")?;
        Ok(Arc::new(attribute_delete(
            geometry,
            domain_param(params, "domain", Domain::Point),
            params.str_or("name", "value"),
        )?))
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
        let mode = aggregate_param(params);
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

pub struct CurveUProcessor;

impl CurveUProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for CurveUProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let path = geometry_input(inputs, 0, "attribute.curveu")?;
        let mode = match params.str_or("mode", "by_arc_length") {
            "by_vertex_order" => CurveUMode::VertexOrder,
            _ => CurveUMode::ArcLength,
        };
        Ok(Arc::new(curve_u(path, mode)?))
    }
}

pub(crate) fn geometry_input<'a>(
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

pub(crate) fn domain_param(params: &ResolvedParams, key: &str, default: Domain) -> Domain {
    let Some(value) = params.get(key) else {
        return default;
    };
    let ResolvedValue::Str(value) = value else {
        tracing::warn!(
            parameter = key,
            "attribute parameter is not a string; using the default value"
        );
        return default;
    };
    match value.as_str() {
        "instance" => Domain::Instance,
        "primitive" => Domain::Primitive,
        "detail" => Domain::Detail,
        "point" => Domain::Point,
        _ => {
            tracing::warn!(
                parameter = key,
                value = %value,
                "attribute parameter has an unknown domain; using the default value"
            );
            default
        }
    }
}

fn aggregate_param(params: &ResolvedParams) -> AggregateMode {
    let Some(value) = params.get("aggregate") else {
        return AggregateMode::Average;
    };
    let ResolvedValue::Str(value) = value else {
        tracing::warn!(
            parameter = "aggregate",
            "attribute parameter is not a string; using the default value"
        );
        return AggregateMode::Average;
    };
    match value.as_str() {
        "max" => AggregateMode::Max,
        "first" => AggregateMode::First,
        "average" => AggregateMode::Average,
        _ => {
            tracing::warn!(
                parameter = "aggregate",
                value = %value,
                "attribute parameter has an unknown aggregate; using the default value"
            );
            AggregateMode::Average
        }
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

    fn run_attribute_node(node: &Node, inputs: &[Arc<dyn NodeData>]) -> Arc<dyn NodeData> {
        let mut graph = Graph::new().add_node(node.clone()).unwrap();
        let mut ev = Evaluator::new();
        let processor: Arc<dyn NodeProcessor> = match node.type_key.as_str() {
            "attribute.set" => Arc::new(AttributeSetProcessor::from_node(node)),
            "attribute.delete" => Arc::new(AttributeDeleteProcessor::from_node(node)),
            "attribute.promote" => Arc::new(AttributePromoteProcessor::from_node(node)),
            "attribute.transfer" => Arc::new(AttributeTransferProcessor::from_node(node)),
            _ => panic!("unsupported test processor {}", node.type_key),
        };
        ev.register(node.id, processor);
        for (index, value) in inputs.iter().enumerate() {
            let source_id = NodeId::new(100 + index as u64);
            graph = graph
                .add_node(
                    Node::new(source_id, "test.source").with_output("out", value.data_type_id()),
                )
                .unwrap()
                .add_edge(
                    EdgeId::new(index as u64 + 1),
                    source_id,
                    OutputPortIndex(0),
                    node.id,
                    InputPortIndex(index as u32),
                )
                .unwrap();
            ev.register(source_id, Arc::new(StubSource(value.clone())));
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

    fn geometry_with_domains() -> Geometry {
        let mut geometry =
            Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0), Vec2(0.0, 1.0)]);
        geometry.push_primitive(ravel_core::geometry::Primitive::Path {
            verts: 0..3,
            closed: false,
        });
        geometry
            .instances_mut()
            .insert(
                "P",
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(1.0, 1.0)]),
            )
            .unwrap();
        geometry
    }

    fn transfer_geometry() -> Geometry {
        let mut geometry =
            Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0), Vec2(20.0, 0.0)]);
        geometry.push_primitive(ravel_core::geometry::Primitive::Path {
            verts: 0..3,
            closed: false,
        });
        geometry
            .points_mut()
            .insert("value", AttributeArray::F32(vec![1.0, 5.0, 3.0]))
            .unwrap();
        geometry
            .instances_mut()
            .insert(
                "P",
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]),
            )
            .unwrap();
        geometry
            .instances_mut()
            .insert("value", AttributeArray::F32(vec![7.0, 9.0]))
            .unwrap();
        geometry
    }

    fn transfer_geometry_with_domain(domain: &str) -> Geometry {
        let mut geometry = transfer_geometry();
        match domain {
            "point" => {}
            "primitive" => {
                geometry
                    .primitive_attrs_mut()
                    .insert("P", AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))
                    .unwrap();
                geometry
                    .primitive_attrs_mut()
                    .insert("value", AttributeArray::F32(vec![11.0]))
                    .unwrap();
            }
            "instance" => {}
            "detail" => {
                geometry
                    .detail_mut()
                    .insert("P", AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))
                    .unwrap();
                geometry
                    .detail_mut()
                    .insert("value", AttributeArray::F32(vec![13.0]))
                    .unwrap();
            }
            other => panic!("unexpected transfer domain {other}"),
        }
        geometry
    }

    fn transfer_target_geometry() -> Geometry {
        let mut geometry =
            Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0), Vec2(20.0, 0.0)]);
        geometry.push_primitive(ravel_core::geometry::Primitive::Path {
            verts: 0..3,
            closed: false,
        });
        geometry
            .instances_mut()
            .insert(
                "P",
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]),
            )
            .unwrap();
        geometry
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

    /// Every domain name the `domain` parameter accepts reaches the domain it
    /// names — **through the string parameter**, which is the only way a user
    /// can set it.
    ///
    /// `domain` is free text, not a closed set, and `domain_param` falls back
    /// to the node's default for anything it does not recognise. `"primitive"`
    /// was missing from that match, so a user who typed it silently got
    /// `Point` instead: `attribute.set(name = "Cd", domain = "primitive")`
    /// wrote a column `rasterize` does not read for paths, and the shape
    /// stayed its default colour with no error anywhere. Every existing test
    /// built the processor's domain through the Rust API, so none of them
    /// crossed the parameter that was broken.
    #[test]
    fn every_domain_name_reaches_the_domain_it_names() {
        for (name, expect_points, expect_primitives) in
            [("point", true, false), ("primitive", false, true)]
        {
            let node = Node::new(NodeId::new(1), "attribute.set")
                .with_input("geometry", &[DataTypeId::GEOMETRY])
                .with_param("domain", ParameterValue::String(name.into()))
                .with_param("name", ParameterValue::String("weight".into()))
                .with_param("value", ParameterValue::Float(2.5));
            let source =
                Node::new(NodeId::new(2), "test.source").with_output("out", DataTypeId::GEOMETRY);
            let graph = Graph::new()
                .add_node(source)
                .unwrap()
                .add_node(node)
                .unwrap()
                .add_edge(
                    EdgeId::new(1),
                    NodeId::new(2),
                    OutputPortIndex(0),
                    NodeId::new(1),
                    InputPortIndex(0),
                )
                .unwrap();
            let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0)]);
            geometry.push_primitive(ravel_core::geometry::Primitive::Path {
                verts: 0..2,
                closed: false,
            });
            let mut ev = Evaluator::new();
            ev.register(NodeId::new(2), Arc::new(StubSource(Arc::new(geometry))));
            ev.register(NodeId::new(1), Arc::new(AttributeSetProcessor));

            let output = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
            let output = output.downcast_ref::<Geometry>().unwrap();
            assert_eq!(
                output.points().get("weight").is_some(),
                expect_points,
                "domain = {name:?} on the point domain"
            );
            assert_eq!(
                output.primitive_attrs().get("weight").is_some(),
                expect_primitives,
                "domain = {name:?} on the primitive domain"
            );
        }
    }

    /// The registry is the source of the strings Properties presents. Drive
    /// every one of those strings through `attribute.set` and check the
    /// resulting column, so a declaration cannot drift away from
    /// `domain_param` without this test failing.
    #[test]
    fn declared_domain_options_reach_their_attribute_set_domains() {
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        let options = registry
            .param_options("attribute.set", "domain")
            .unwrap()
            .to_vec();
        assert_eq!(options, ravel_core::registry::builtin::ATTRIBUTE_DOMAINS);

        for domain in options {
            let mut node = registered_node("attribute.set", 1);
            set_string_param(&mut node, "domain", &domain);
            set_string_param(&mut node, "name", "marker");
            let output = run_attribute_node(&node, &[Arc::new(geometry_with_domains())]);
            let output = output.downcast_ref::<Geometry>().unwrap();
            let present = match domain.as_str() {
                "point" => output.points().get("marker").is_some(),
                "primitive" => output.primitive_attrs().get("marker").is_some(),
                "instance" => output.instances().get("marker").is_some(),
                "detail" => output.detail().get("marker").is_some(),
                other => panic!("unexpected domain option {other}"),
            };
            assert!(present, "domain = {domain:?} did not receive the attribute");
        }
    }

    /// `attribute.delete` reads the same four domain strings as
    /// `attribute.set`, and touches only the one it was given. Every domain
    /// carries the column, so a processor that ignored `domain` and always
    /// used the default would leave one of the other three behind.
    #[test]
    fn declared_domain_options_reach_their_attribute_delete_domains() {
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        let options = registry
            .param_options("attribute.delete", "domain")
            .unwrap()
            .to_vec();
        assert_eq!(options, ravel_core::registry::builtin::ATTRIBUTE_DOMAINS);

        let mut geometry = geometry_with_domains();
        geometry
            .points_mut()
            .insert("stagger_t", AttributeArray::F32(vec![3.5, -7.25, 11.75]))
            .unwrap();
        geometry
            .primitive_attrs_mut()
            .insert("stagger_t", AttributeArray::F32(vec![-2.75]))
            .unwrap();
        geometry
            .instances_mut()
            .insert("stagger_t", AttributeArray::F32(vec![6.25, -13.5]))
            .unwrap();
        geometry
            .detail_mut()
            .insert("stagger_t", AttributeArray::F32(vec![21.5]))
            .unwrap();

        for domain in options {
            let mut node = registered_node("attribute.delete", 1);
            set_string_param(&mut node, "domain", &domain);
            set_string_param(&mut node, "name", "stagger_t");
            let output = run_attribute_node(&node, &[Arc::new(geometry.clone())]);
            let output = output.downcast_ref::<Geometry>().unwrap();
            let present = [
                ("point", output.points().get("stagger_t").is_some()),
                (
                    "primitive",
                    output.primitive_attrs().get("stagger_t").is_some(),
                ),
                ("instance", output.instances().get("stagger_t").is_some()),
                ("detail", output.detail().get("stagger_t").is_some()),
            ];
            for (name, still_there) in present {
                assert_eq!(
                    still_there,
                    name != domain,
                    "domain = {domain:?} left the {name} domain in the wrong state"
                );
            }
        }
    }

    /// A name nothing wrote evaluates to the input geometry rather than an
    /// error, so a graph whose upstream stopped writing a scratch column keeps
    /// rendering.
    #[test]
    fn attribute_delete_of_a_missing_name_evaluates_to_the_input() {
        let mut node = registered_node("attribute.delete", 1);
        set_string_param(&mut node, "domain", "point");
        set_string_param(&mut node, "name", "never_written");
        let mut geometry = geometry_with_domains();
        geometry
            .points_mut()
            .insert("weight", AttributeArray::F32(vec![3.5, -7.25, 11.75]))
            .unwrap();

        let output = run_attribute_node(&node, &[Arc::new(geometry.clone())]);
        let output = output.downcast_ref::<Geometry>().unwrap();
        assert_eq!(output.summary().points, geometry.summary().points);
        assert_eq!(
            output
                .points()
                .get("weight")
                .unwrap()
                .as_f32("weight")
                .unwrap(),
            &[3.5, -7.25, 11.75]
        );
    }

    /// Every aggregate option is sent through the string parameter. The
    /// values are deliberately distinct, so accidentally treating all values
    /// as the default average cannot satisfy this test.
    #[test]
    fn declared_aggregate_options_reach_their_promotion_modes() {
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        let options = registry
            .param_options("attribute.promote", "aggregate")
            .unwrap()
            .to_vec();
        assert_eq!(options, ravel_core::registry::builtin::ATTRIBUTE_AGGREGATES);

        let mut geometry = geometry_with_domains();
        geometry
            .points_mut()
            .insert("value", AttributeArray::F32(vec![1.0, 5.0, 3.0]))
            .unwrap();
        for aggregate in options {
            let mut node = registered_node("attribute.promote", 1);
            set_string_param(&mut node, "aggregate", &aggregate);
            set_string_param(&mut node, "name", "value");
            let output = run_attribute_node(&node, &[Arc::new(geometry.clone())]);
            let output = output.downcast_ref::<Geometry>().unwrap();
            let actual = output
                .detail()
                .get("value")
                .unwrap()
                .as_f32("value")
                .unwrap()[0];
            let expected = match aggregate.as_str() {
                "average" => 3.0,
                "max" => 5.0,
                "first" => 1.0,
                other => panic!("unexpected aggregate option {other}"),
            };
            assert_eq!(actual, expected, "aggregate = {aggregate:?}");
        }
    }

    /// Both transfer domain parameters use the same four strings as
    /// `domain_param`; the geometry below gives every selected domain a
    /// position and value column so each branch is observable.
    /// Each value is sent through the evaluator rather than converted to a
    /// `Domain` in the test.
    #[test]
    fn declared_transfer_domain_options_reach_their_string_parameter_branches() {
        let mut registry = ravel_core::registry::NodeRegistry::new();
        ravel_core::registry::builtin::register_builtins(&mut registry);
        let options = registry
            .param_options("attribute.transfer", "source_domain")
            .unwrap()
            .to_vec();
        assert_eq!(options, ravel_core::registry::builtin::ATTRIBUTE_DOMAINS);
        assert_eq!(
            registry.param_options("attribute.transfer", "target_domain"),
            Some(options.as_slice())
        );

        for domain in options {
            let mut source_domain_node = registered_node("attribute.transfer", 1);
            set_string_param(&mut source_domain_node, "source_domain", &domain);
            let source_output = run_attribute_node(
                &source_domain_node,
                &[
                    Arc::new(transfer_target_geometry()),
                    Arc::new(transfer_geometry_with_domain(&domain)),
                ],
            );
            let source = source_output.downcast_ref::<Geometry>().unwrap();
            let source_values = source
                .points()
                .get("value")
                .unwrap()
                .as_f32("value")
                .unwrap();
            let expected = match domain.as_str() {
                "point" => [1.0, 5.0, 3.0],
                "primitive" => [11.0, 11.0, 11.0],
                "instance" => [7.0, 9.0, 9.0],
                "detail" => [13.0, 13.0, 13.0],
                other => panic!("unexpected domain option {other}"),
            };
            assert_eq!(source_values, &expected, "source_domain = {domain:?}");

            let mut target_domain_node = registered_node("attribute.transfer", 2);
            set_string_param(&mut target_domain_node, "target_domain", &domain);
            let target_output = run_attribute_node(
                &target_domain_node,
                &[
                    Arc::new(transfer_geometry_with_domain(&domain)),
                    Arc::new(transfer_geometry()),
                ],
            );
            let target = target_output.downcast_ref::<Geometry>().unwrap();
            let (target_values, expected) = match domain.as_str() {
                "point" => (
                    target
                        .points()
                        .get("value")
                        .unwrap()
                        .as_f32("value")
                        .unwrap(),
                    &[1.0, 5.0, 3.0][..],
                ),
                "primitive" => (
                    target
                        .primitive_attrs()
                        .get("value")
                        .unwrap()
                        .as_f32("value")
                        .unwrap(),
                    &[1.0][..],
                ),
                "instance" => (
                    target
                        .instances()
                        .get("value")
                        .unwrap()
                        .as_f32("value")
                        .unwrap(),
                    &[1.0, 5.0][..],
                ),
                "detail" => (
                    target
                        .detail()
                        .get("value")
                        .unwrap()
                        .as_f32("value")
                        .unwrap(),
                    &[1.0][..],
                ),
                other => panic!("unexpected domain option {other}"),
            };
            assert_eq!(target_values, expected, "target_domain = {domain:?}");
        }
    }

    #[test]
    fn unknown_attribute_set_values_warn_and_use_the_default_domain() {
        let mut node = registered_node("attribute.set", 1);
        set_string_param(&mut node, "domain", "future_domain");
        set_string_param(&mut node, "name", "marker");
        let logged = warnings_from(|| {
            let output = run_attribute_node(&node, &[Arc::new(geometry_with_domains())]);
            let output = output.downcast_ref::<Geometry>().unwrap();
            assert!(output.points().get("marker").is_some());
            assert!(output.primitive_attrs().get("marker").is_none());
        });
        assert!(
            logged.contains("unknown domain"),
            "missing warning: {logged:?}"
        );
    }

    #[test]
    fn unknown_aggregate_values_warn_and_use_average() {
        let mut node = registered_node("attribute.promote", 1);
        set_string_param(&mut node, "aggregate", "future_aggregate");
        set_string_param(&mut node, "name", "value");
        let mut geometry = geometry_with_domains();
        geometry
            .points_mut()
            .insert("value", AttributeArray::F32(vec![1.0, 5.0, 3.0]))
            .unwrap();
        let logged = warnings_from(|| {
            let output = run_attribute_node(&node, &[Arc::new(geometry)]);
            let output = output.downcast_ref::<Geometry>().unwrap();
            assert_eq!(
                output
                    .detail()
                    .get("value")
                    .unwrap()
                    .as_f32("value")
                    .unwrap(),
                &[3.0]
            );
        });
        assert!(
            logged.contains("unknown aggregate"),
            "missing warning: {logged:?}"
        );
    }

    #[test]
    fn unknown_transfer_domain_warns_and_uses_the_point_default() {
        let mut node = registered_node("attribute.transfer", 1);
        set_string_param(&mut node, "source_domain", "future_domain");
        let logged = warnings_from(|| {
            let output = run_attribute_node(
                &node,
                &[
                    Arc::new(transfer_target_geometry()),
                    Arc::new(transfer_geometry()),
                ],
            );
            assert!(
                output
                    .downcast_ref::<Geometry>()
                    .unwrap()
                    .points()
                    .get("value")
                    .is_some()
            );
        });
        assert!(
            logged.contains("unknown domain"),
            "missing warning: {logged:?}"
        );
    }

    /// The attribute operations touch columns, not coordinates: a 3D geometry
    /// passes through them with its `P` dimension intact.
    #[test]
    fn attribute_operations_pass_three_dimensional_positions_through() {
        let geometry = Geometry::from_points3(vec![
            Vec3(0.0, 0.0, 1.0),
            Vec3(1.0, 0.0, 2.0),
            Vec3(2.0, 0.0, 3.0),
        ]);

        let with_value =
            attribute_set(&geometry, Domain::Point, "weight", AttributeValue::F32(0.5)).unwrap();
        let promoted = promote_attribute(
            &with_value,
            Domain::Point,
            Domain::Detail,
            "weight",
            AggregateMode::Average,
        )
        .unwrap();
        let transferred = attribute_transfer(
            &Geometry::from_points3(vec![Vec3(0.0, 0.0, 1.5)]),
            Domain::Point,
            &with_value,
            Domain::Point,
            "weight",
            TransferMode::Nearest,
        )
        .unwrap();

        for (label, result) in [
            ("set", &with_value),
            ("promote", &promoted),
            ("transfer", &transferred),
        ] {
            assert_eq!(result.validate(), Ok(()), "{label} produced valid geometry");
            assert_eq!(
                result.points().get("P").unwrap().attr_type(),
                ravel_core::geometry::AttributeType::Vec3,
                "{label} kept the position dimension"
            );
        }
        assert_eq!(
            with_value.points().get("P").unwrap().as_vec3("P").unwrap(),
            geometry.points().get("P").unwrap().as_vec3("P").unwrap(),
            "the positions themselves are untouched"
        );
    }

    /// `value` is one parameter whose arity follows `type`: the processor
    /// reads the matching number of components from it.
    #[test]
    fn set_processor_reads_value_at_the_arity_its_type_selects() {
        let cases: Vec<(&str, ParameterValue, AttributeArray)> = vec![
            (
                "f32",
                ParameterValue::Channel(
                    ravel_core::animation::channel::AnimationChannel::constant(2.5),
                ),
                AttributeArray::F32(vec![2.5]),
            ),
            (
                "vec2",
                ParameterValue::vec2(1.0, -2.0),
                AttributeArray::Vec2(vec![Vec2(1.0, -2.0)]),
            ),
            (
                "vec3",
                ParameterValue::vec3(1.0, 2.0, 3.0),
                AttributeArray::Vec3(vec![Vec3(1.0, 2.0, 3.0)]),
            ),
            (
                "vec4",
                ParameterValue::from_channels(
                    None,
                    [4.0, 3.0, 2.0, 1.0]
                        .into_iter()
                        .map(ravel_core::animation::channel::AnimationChannel::constant)
                        .collect(),
                )
                .unwrap(),
                AttributeArray::Vec4(vec![Vec4(4.0, 3.0, 2.0, 1.0)]),
            ),
            (
                "color",
                ParameterValue::from_channels(
                    None,
                    [0.25, 0.5, 0.75, 1.0]
                        .into_iter()
                        .map(ravel_core::animation::channel::AnimationChannel::constant)
                        .collect(),
                )
                .unwrap(),
                AttributeArray::Color(vec![Color::new(0.25, 0.5, 0.75, 1.0)]),
            ),
        ];
        for (type_name, value, expected) in cases {
            let node = Node::new(NodeId::new(1), "attribute.set")
                .with_input("geometry", &[DataTypeId::GEOMETRY])
                .with_param("name", ParameterValue::String("v".into()))
                .with_param("type", ParameterValue::String(type_name.into()))
                .with_param("value", value);
            let source =
                Node::new(NodeId::new(2), "test.source").with_output("out", DataTypeId::GEOMETRY);
            let graph = Graph::new()
                .add_node(source)
                .unwrap()
                .add_node(node)
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
            let geometry: Arc<dyn NodeData> = Arc::new(Geometry::from_points(vec![Vec2(0.0, 0.0)]));
            ev.register(NodeId::new(2), Arc::new(StubSource(geometry)));
            ev.register(NodeId::new(1), Arc::new(AttributeSetProcessor));
            let output = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
            let output = output.downcast_ref::<Geometry>().unwrap();
            assert_eq!(
                output.points().get("v").map(AsRef::as_ref),
                Some(&expected),
                "{type_name}"
            );
        }
    }

    /// The first half of the "gradient along a line" chain
    /// (`shape.line` → `attribute.curveu` → `field.attribute("u")`): the node
    /// has to be reachable through the evaluator and label the generated
    /// points, not just the hand-built geometry the core test uses.
    #[test]
    fn curveu_processor_labels_the_points_of_a_generated_line() {
        let line = Node::new(NodeId::new(1), "shape.line")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("start", ParameterValue::vec2(0.0, 0.0))
            .with_param("end", ParameterValue::vec2(20.0, 0.0))
            .with_param("segments", ParameterValue::Int(2));
        let curveu = Node::new(NodeId::new(2), "attribute.curveu")
            .with_input("path", &[DataTypeId::GEOMETRY])
            .with_output("geometry", DataTypeId::GEOMETRY);
        let graph = Graph::new()
            .add_node(line)
            .unwrap()
            .add_node(curveu)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(crate::shape::LineProcessor));
        ev.register(NodeId::new(2), Arc::new(CurveUProcessor));

        let output = ev.evaluate(&graph, NodeId::new(2), &ctx()).unwrap();
        let output = output.downcast_ref::<Geometry>().unwrap();
        assert_eq!(
            output.points().get("u").unwrap().as_f32("u").unwrap(),
            &[0.0, 0.5, 1.0]
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
