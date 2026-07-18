// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Data-driven layer creation templates (REQ-LAYER-008).
//!
//! "Add a Solid" and friends are not structural layer kinds — they stamp an
//! **initial network** into a fresh layer, after which the network is freely
//! editable. Template definitions are pure data ([`LayerTemplate`],
//! RON-serializable) so user-defined templates (saved networks,
//! REQ-PLUGIN-005) can reuse the same pipeline later; the built-in Solid /
//! Shape / Video / Null definitions live in `assets/layer-templates/` and
//! are embedded at compile time.
//!
//! Node definitions are seeded from the [`NodeRegistry`] when the type key
//! is registered (ports and default parameters), with the template's own
//! `inputs` / `outputs` / `params` extending or overriding the seed — this
//! keeps interface nodes (`net.in` / `net.out`, whose ports are dynamic)
//! expressible without duplicating every built-in node definition.

use crate::graph::{Graph, GraphError, InputPort, Node, OutputPort, Parameter};
use crate::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use crate::registry::NodeRegistry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TemplateError {
    #[error("template edge references unknown node key {0:?}")]
    UnknownNodeKey(String),

    #[error("template node {node:?} has no port named {port:?}")]
    UnknownPort { node: String, port: String },

    #[error("template produced an invalid graph: {0}")]
    Graph(#[from] GraphError),
}

/// One node of a template, addressed by a template-local symbolic `key`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemplateNode {
    pub key: String,
    pub type_key: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub position: (f32, f32),
    /// Input ports appended to (not replacing) the registry seed.
    #[serde(default)]
    pub inputs: Vec<InputPort>,
    /// Output ports appended to (not replacing) the registry seed.
    #[serde(default)]
    pub outputs: Vec<OutputPort>,
    /// Parameters overriding the registry seed by key (appended when new).
    #[serde(default)]
    pub params: Vec<Parameter>,
}

/// One edge of a template: `(node key, port name)` pairs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TemplateEdge {
    pub from: (String, String),
    pub to: (String, String),
}

/// A layer creation template: the initial network stamped into a new layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerTemplate {
    pub key: String,
    pub display_name: String,
    pub nodes: Vec<TemplateNode>,
    pub edges: Vec<TemplateEdge>,
}

impl LayerTemplate {
    /// Build a fresh network from this template.
    ///
    /// Every instantiation allocates new globally-unique node ids
    /// (`NodeId::next`, REQ-LAYER-009), so a template can be stamped any
    /// number of times.
    pub fn instantiate(&self, registry: &NodeRegistry) -> Result<Graph, TemplateError> {
        let mut ids: HashMap<&str, NodeId> = HashMap::new();
        let mut graph = Graph::new();

        for spec in &self.nodes {
            let id = NodeId::next();
            let mut node = registry
                .create_node(&spec.type_key, id)
                .unwrap_or_else(|| Node::new(id, &spec.type_key));
            if let Some(label) = &spec.label {
                node.metadata.label = Some(label.clone());
            }
            node.metadata.position = spec.position;
            for port in &spec.inputs {
                if !node.inputs.iter().any(|p| p.name == port.name) {
                    node.inputs.push(port.clone());
                }
            }
            for port in &spec.outputs {
                if !node.outputs.iter().any(|p| p.name == port.name) {
                    node.outputs.push(port.clone());
                }
            }
            for param in &spec.params {
                match node.parameters.iter_mut().find(|p| p.key == param.key) {
                    Some(existing) => existing.value = param.value.clone(),
                    None => node.parameters.push(param.clone()),
                }
            }
            ids.insert(&spec.key, id);
            graph = graph.add_node(node)?;
        }

        for edge in &self.edges {
            let (source, source_port) = resolve_output(&graph, &ids, &edge.from)?;
            let (target, target_port) = resolve_input(&graph, &ids, &edge.to)?;
            graph = graph.add_edge(EdgeId::next(), source, source_port, target, target_port)?;
        }
        Ok(graph)
    }
}

fn node_for<'g>(
    graph: &'g Graph,
    ids: &HashMap<&str, NodeId>,
    key: &str,
) -> Result<&'g Node, TemplateError> {
    let id = ids
        .get(key)
        .ok_or_else(|| TemplateError::UnknownNodeKey(key.to_string()))?;
    // SAFETY of expect: every id in `ids` was just inserted into the graph.
    Ok(graph.node(*id).expect("template node present"))
}

fn resolve_output(
    graph: &Graph,
    ids: &HashMap<&str, NodeId>,
    (key, port): &(String, String),
) -> Result<(NodeId, OutputPortIndex), TemplateError> {
    let node = node_for(graph, ids, key)?;
    let index = node
        .outputs
        .iter()
        .position(|p| &p.name == port)
        .ok_or_else(|| TemplateError::UnknownPort {
            node: key.clone(),
            port: port.clone(),
        })?;
    Ok((node.id, OutputPortIndex(index as u32)))
}

fn resolve_input(
    graph: &Graph,
    ids: &HashMap<&str, NodeId>,
    (key, port): &(String, String),
) -> Result<(NodeId, InputPortIndex), TemplateError> {
    let node = node_for(graph, ids, key)?;
    let index = node
        .inputs
        .iter()
        .position(|p| &p.name == port)
        .ok_or_else(|| TemplateError::UnknownPort {
            node: key.clone(),
            port: port.clone(),
        })?;
    Ok((node.id, InputPortIndex(index as u32)))
}

/// The built-in Solid / Shape / Video / Null templates (REQ-LAYER-008),
/// parsed once from the embedded `assets/layer-templates/` definitions.
pub fn builtin_layer_templates() -> &'static [LayerTemplate] {
    static TEMPLATES: OnceLock<Vec<LayerTemplate>> = OnceLock::new();
    TEMPLATES.get_or_init(|| {
        [
            include_str!("../../../../assets/layer-templates/solid.ron"),
            include_str!("../../../../assets/layer-templates/shape.ron"),
            include_str!("../../../../assets/layer-templates/video.ron"),
            include_str!("../../../../assets/layer-templates/null.ron"),
        ]
        .iter()
        .map(|source| ron::from_str(source).expect("built-in layer template must parse"))
        .collect()
    })
}

/// Look up a built-in template by key (`"solid"`, `"shape"`, `"video"`,
/// `"null"`).
pub fn builtin_layer_template(key: &str) -> Option<&'static LayerTemplate> {
    builtin_layer_templates().iter().find(|t| t.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::Layer;
    use crate::id::LayerId;
    use crate::network as net;
    use crate::registry::NodeRegistry;
    use crate::registry::builtin::register_builtins;

    fn registry() -> NodeRegistry {
        let mut reg = NodeRegistry::new();
        register_builtins(&mut reg);
        reg
    }

    #[test]
    fn builtin_templates_parse() {
        let keys: Vec<&str> = builtin_layer_templates()
            .iter()
            .map(|t| t.key.as_str())
            .collect();
        assert_eq!(keys, ["solid", "shape", "video", "null"]);
    }

    #[test]
    fn every_builtin_template_instantiates() {
        let reg = registry();
        for template in builtin_layer_templates() {
            let network = template.instantiate(&reg).expect("instantiates");
            let in_node = net::find_in_node(&network)
                .unwrap_or_else(|| panic!("{}: has net.in", template.key));
            assert!(
                in_node
                    .outputs
                    .iter()
                    .any(|p| p.name == net::PORT_FRAME_INDEX),
                "{}: net.in has the frame index port",
                template.key
            );
            assert!(
                net::find_out_node(&network).is_some(),
                "{}: has net.out",
                template.key
            );
        }
    }

    #[test]
    fn solid_template_wires_color_into_rasterize() {
        let network = builtin_layer_template("solid")
            .unwrap()
            .instantiate(&registry())
            .unwrap();
        let rasterize = network
            .nodes()
            .find(|n| n.type_key == "rasterize")
            .expect("rasterize present");
        // Registry seed provides geometry+color inputs; both are connected.
        let connected: Vec<u32> = network
            .edges()
            .filter(|e| e.target == rasterize.id)
            .map(|e| e.target_port.0)
            .collect();
        assert!(connected.contains(&0), "geometry pin connected");
        assert!(connected.contains(&1), "color pin connected");

        let layer = Layer::new(LayerId::new(1), "Solid", network);
        assert!(layer.has_frame_output());
    }

    #[test]
    fn null_template_has_no_frame_output() {
        let network = builtin_layer_template("null")
            .unwrap()
            .instantiate(&registry())
            .unwrap();
        let layer = Layer::new(LayerId::new(1), "Null", network);
        assert!(!layer.has_frame_output());
    }

    #[test]
    fn instantiations_allocate_fresh_node_ids() {
        let reg = registry();
        let template = builtin_layer_template("solid").unwrap();
        let a = template.instantiate(&reg).unwrap();
        let b = template.instantiate(&reg).unwrap();
        let ids_a: std::collections::HashSet<_> = a.node_ids().collect();
        assert!(
            b.node_ids().all(|id| !ids_a.contains(&id)),
            "node ids must be globally unique per instantiation"
        );
    }

    #[test]
    fn unknown_edge_key_is_rejected() {
        let template = LayerTemplate {
            key: "broken".into(),
            display_name: "Broken".into(),
            nodes: vec![],
            edges: vec![TemplateEdge {
                from: ("ghost".into(), "out".into()),
                to: ("ghost".into(), "in".into()),
            }],
        };
        assert!(matches!(
            template.instantiate(&registry()),
            Err(TemplateError::UnknownNodeKey(_))
        ));
    }

    #[test]
    fn unknown_port_is_rejected() {
        let template = LayerTemplate {
            key: "broken".into(),
            display_name: "Broken".into(),
            nodes: vec![
                TemplateNode {
                    key: "a".into(),
                    type_key: "constant".into(),
                    label: None,
                    position: (0.0, 0.0),
                    inputs: vec![],
                    outputs: vec![],
                    params: vec![],
                },
                TemplateNode {
                    key: "b".into(),
                    type_key: "rasterize".into(),
                    label: None,
                    position: (0.0, 0.0),
                    inputs: vec![],
                    outputs: vec![],
                    params: vec![],
                },
            ],
            edges: vec![TemplateEdge {
                from: ("a".into(), "nope".into()),
                to: ("b".into(), "geometry".into()),
            }],
        };
        assert!(matches!(
            template.instantiate(&registry()),
            Err(TemplateError::UnknownPort { .. })
        ));
    }

    #[test]
    fn template_params_override_registry_defaults() {
        use crate::graph::ParameterValue;
        let template = LayerTemplate {
            key: "custom".into(),
            display_name: "Custom".into(),
            nodes: vec![TemplateNode {
                key: "c".into(),
                type_key: "constant".into(),
                label: None,
                position: (0.0, 0.0),
                inputs: vec![],
                outputs: vec![],
                params: vec![Parameter {
                    key: "value".into(),
                    value: ParameterValue::Float(42.0),
                }],
            }],
            edges: vec![],
        };
        let network = template.instantiate(&registry()).unwrap();
        let node = network.nodes().next().unwrap();
        let value = node
            .parameters
            .iter()
            .find(|p| p.key == "value")
            .and_then(|p| p.value.as_float());
        assert_eq!(value, Some(42.0));
        // Registry defaults are overridden in place, not duplicated.
        assert_eq!(
            node.parameters.iter().filter(|p| p.key == "value").count(),
            1
        );
    }
}
