// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Subgraph templates — a reusable subnet plus the exposed parameters it
//! publishes (REQ-PLUGIN-005 (2), EXPO-6).
//!
//! This is Ravel's answer to Houdini's HDA, Nuke's Gizmo and Blender's Node
//! Group: a network someone authored once, stamped into any project, with a
//! handful of named inputs and everything else hidden. The evaluation side
//! already exists — a [`subnet`](crate::network::SUBNET_TYPE_KEY) node
//! recursively evaluates its inner graph — so a template is that inner graph
//! plus the contract over it.
//!
//! # The declarations are the project's declarations
//!
//! A template's public parameters are an [`ExposedParameters`], the same type
//! and the same invariants a project's are ([`crate::exposed`]). Not a parallel
//! "template parameter" type that happens to look similar: the whole point of
//! REQ-PROJ-006 is that one declaration mechanism serves the CLI's
//! `--param`, a template's public inputs, a network interface and a shader
//! manifest, and a second type would be the fourth reinvention that requirement
//! exists to prevent. Concretely, that means a template file's declarations go
//! through [`ExposedParameter::new`] on the way in exactly as a `.ravprj`'s do,
//! and an instantiated template's declarations are indistinguishable — to
//! [`ExposedListing`](crate::exposed::listing::ExposedListing), to
//! [`apply`](crate::exposed::apply::apply), and to the editing UI — from ones
//! the user made by exposing a parameter.
//!
//! # Instantiation mints ids, so the bindings have to move with them
//!
//! A binding is a [`NodeId`] plus a parameter key, and node ids are
//! document-globally unique (REQ-LAYER-009), so stamping a template twice into
//! one project has to give the second copy fresh ids
//! ([`Graph::duplicate_with_fresh_ids`]) — and rewrite its declarations'
//! bindings through the same map. A declaration left pointing at the first
//! copy would silently drive the wrong instance, which is worse than not
//! resolving: it would resolve, and to the wrong node.
//!
//! The same reasoning is why a declaration the inner graph does *not* hold
//! makes [`SubgraphTemplate::instantiate`] fail rather than travel along
//! unrewritten. A [`NodeId`] is a bare integer, and the document being stamped
//! into is free to hold that integer already — on a node the template has
//! never heard of. Carrying the id through would not produce an unresolvable
//! declaration; it would produce one that resolves to a stranger, and hands
//! the project's callers a `--param` that edits it.
//!
//! # Names collide; the instantiation layer renames
//!
//! Two copies of one template declare the same names, and a project cannot hold
//! two declarations of one name. [`add_declarations`] resolves that by
//! suffixing (`title`, `title_2`, `title_3`), and reports every rename it made.
//! The renaming lives here rather than in [`ExposedParameters::insert`] on
//! purpose: the core never invents a name behind the user's back for an edit
//! *they* made (a colliding rename is refused and reported, see
//! [`ExposedParameters::rename`]), but a second stamp of a template is not a
//! naming decision the user expressed, and failing it would make templates
//! single-use.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::composition::Document;
use crate::exposed::{ExposedParameter, ExposedParameters};
use crate::graph::{Graph, Node};
use crate::id::NodeId;
use crate::network::{SUBNET_TYPE_KEY, adopt_subnet_inner, is_subnet_node};

/// Why a subgraph template could not be captured or instantiated.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SubgraphTemplateError {
    #[error("a subgraph template must have a name")]
    EmptyName,

    #[error("node {0:?} is not a subnet node")]
    NotASubnet(NodeId),

    #[error("subnet node {0:?} has no inner graph")]
    NoInnerGraph(NodeId),

    /// A declaration binds a node the template's own graph does not hold, so
    /// instantiation has no fresh id to rewrite it to. Raised by
    /// [`SubgraphTemplate::instantiate`], never by
    /// [`SubgraphTemplate::capture`] — capture only keeps what is inside.
    #[error(
        "declaration {name:?} binds node {node:?}, which the template's own graph does not hold"
    )]
    UnboundDeclaration { name: String, node: NodeId },
}

/// A subnet's inner graph plus the parameters it publishes.
///
/// Fields are private: `declarations` may only bind to nodes inside `inner`,
/// which [`SubgraphTemplate::capture`] establishes and
/// [`SubgraphTemplate::instantiate`] preserves.
///
/// A hand-written or stale template file that names a node the inner graph
/// does not hold still **loads** — refusing the file would lose the rest of a
/// template over one entry, and a file the user can open is a file they can
/// repair. The refusal is at [`SubgraphTemplate::instantiate`] instead, which
/// is the point where such a binding stops being inert and starts naming a
/// node id in someone's document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubgraphTemplate {
    name: String,
    /// The subnet's inner graph, including its `net.in` / `net.out` pair.
    inner: Graph,
    /// The public inputs, bound to nodes inside `inner`.
    #[serde(default)]
    declarations: ExposedParameters,
}

/// What instantiating a template produced: the subnet node to place, and the
/// declarations to add to the document.
///
/// The two travel together and belong in **one** document commit. Adding the
/// node without the declarations gives the user a network with no controls;
/// adding the declarations without the node gives the project a contract
/// pointing at nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct Instantiated {
    /// A `subnet` node owning a fresh copy of the template's inner graph.
    pub node: Node,
    /// The template's declarations, rebound to the copy.
    pub declarations: ExposedParameters,
}

impl SubgraphTemplate {
    /// Capture the subnet node `subnet_id` of `graph`, together with every
    /// declaration in `declarations` bound inside it.
    ///
    /// Declarations bound *outside* the subnet are left behind: they are the
    /// containing project's contract, not this template's, and carrying them
    /// would publish a control that reaches nothing once the template is
    /// stamped somewhere else.
    pub fn capture(
        name: impl Into<String>,
        graph: &Graph,
        subnet_id: NodeId,
        declarations: &ExposedParameters,
    ) -> Result<Self, SubgraphTemplateError> {
        let name = name.into();
        let name = name.trim();
        if name.is_empty() {
            return Err(SubgraphTemplateError::EmptyName);
        }
        let node = graph
            .node(subnet_id)
            .filter(|node| is_subnet_node(node))
            .ok_or(SubgraphTemplateError::NotASubnet(subnet_id))?;
        let inner = node
            .subnet
            .as_deref()
            .cloned()
            .ok_or(SubgraphTemplateError::NoInnerGraph(subnet_id))?;

        let inside = node_ids_of(&inner);
        // `from_declarations` cannot fail here: the names were unique in the
        // set they came from, and a subset of unique names is unique.
        let declarations = ExposedParameters::from_declarations(
            declarations
                .iter()
                .filter(|declaration| inside.contains(&declaration.binding().node))
                .cloned(),
        )
        .expect("a subset of unique names is unique");

        Ok(Self {
            name: name.to_string(),
            inner,
            declarations,
        })
    }

    /// The template's display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The public inputs, as they are declared in the template.
    ///
    /// Their bindings name nodes of [`SubgraphTemplate::inner`], not of any
    /// document — [`SubgraphTemplate::instantiate`] is what makes them
    /// resolvable.
    pub fn declarations(&self) -> &ExposedParameters {
        &self.declarations
    }

    /// The subnet's inner graph.
    pub fn inner(&self) -> &Graph {
        &self.inner
    }

    /// Build a fresh copy: a `subnet` node owning a duplicate of the inner
    /// graph, and the declarations rebound to that duplicate.
    ///
    /// Every node id in the copy is new ([`Graph::duplicate_with_fresh_ids`],
    /// which also follows nested subnets and node-output parameter bindings),
    /// so the same template can be stamped into one project any number of
    /// times without two copies sharing an id — or a declaration.
    ///
    /// # Errors
    ///
    /// [`SubgraphTemplateError::UnboundDeclaration`] when a declaration binds a
    /// node the inner graph does not hold — a property a hand-edited or stale
    /// file can have and `capture` cannot produce, refused here rather than at
    /// load so the file stays openable and nothing that would corrupt a
    /// document reaches one.
    pub fn instantiate(&self) -> Result<Instantiated, SubgraphTemplateError> {
        let (inner, id_map) = self.inner.duplicate_with_fresh_ids();
        let mut node = Node::new(NodeId::next(), SUBNET_TYPE_KEY).with_label(self.name.clone());
        adopt_subnet_inner(&mut node, inner);

        let rebound = self
            .declarations
            .iter()
            .map(|declaration| {
                let binding = declaration.binding();
                // A binding the inner graph does not hold has no fresh id to
                // move to, and keeping the old one is not "unresolvable": node
                // ids are bare integers, so the id very plausibly names a live
                // node of the document being stamped into, and the declaration
                // would drive *that* node.
                let fresh = id_map.get(&binding.node).copied().ok_or_else(|| {
                    SubgraphTemplateError::UnboundDeclaration {
                        name: declaration.name().to_string(),
                        node: binding.node,
                    }
                })?;
                Ok(declaration
                    .clone()
                    .with_binding(crate::exposed::ExposedBinding::new(
                        fresh,
                        binding.key.clone(),
                    )))
            })
            .collect::<Result<Vec<_>, SubgraphTemplateError>>()?;

        let declarations = ExposedParameters::from_declarations(rebound)
            .expect("the template's names were already unique");

        Ok(Instantiated { node, declarations })
    }
}

/// Every node id in `graph`, including the ones inside nested subnets.
fn node_ids_of(graph: &Graph) -> HashSet<NodeId> {
    let mut ids = HashSet::new();
    collect_node_ids(graph, &mut ids);
    ids
}

fn collect_node_ids(graph: &Graph, ids: &mut HashSet<NodeId>) {
    for node in graph.nodes() {
        ids.insert(node.id);
        if let Some(inner) = node.subnet.as_deref() {
            collect_node_ids(inner, ids);
        }
    }
}

/// Add `declarations` to `document`'s, renaming any whose name is taken.
///
/// Returns the document and the renames performed, as `(wanted, given)`, in
/// declaration order. The caller shows them: a control the user will look for
/// under the template's name is now under another one, and finding that out by
/// reading the list is worse than being told.
///
/// The suffix is the first free `<name>_<n>` starting at `2`, so a second stamp
/// of a `title` template declares `title_2` and a third `title_3`. Appending to
/// the name rather than to the *type* or the binding keeps the collision
/// visible in the one place the contract is read.
pub fn add_declarations(
    document: Document,
    declarations: ExposedParameters,
) -> (Document, Vec<(String, String)>) {
    let mut set = document.exposed_parameters.clone();
    let mut renames = Vec::new();
    for declaration in declarations.iter() {
        let wanted = declaration.name().to_string();
        let given = free_name(&set, &wanted);
        if given != wanted {
            renames.push((wanted, given.clone()));
        }
        let declaration = rename_to(declaration, &given);
        set.insert(declaration)
            .expect("the name was chosen because it is free");
    }
    (document.with_exposed_parameters(set), renames)
}

/// `wanted` if it is free, else the first free `<wanted>_<n>` from `n = 2`.
fn free_name(set: &ExposedParameters, wanted: &str) -> String {
    if !set.contains(wanted) {
        return wanted.to_string();
    }
    (2u32..)
        .map(|n| format!("{wanted}_{n}"))
        .find(|candidate| !set.contains(candidate))
        .expect("the range is unbounded and the set is finite")
}

/// `declaration` under `name`, keeping everything else.
///
/// Goes through [`ExposedParameter::new`] rather than mutating, so a renamed
/// declaration is checked by the same constructor a hand-written one is.
fn rename_to(declaration: &ExposedParameter, name: &str) -> ExposedParameter {
    ExposedParameter::new(
        name,
        declaration.value_type(),
        declaration.default_value().clone(),
        declaration.binding().clone(),
    )
    .expect("the type, default and name all came from a valid declaration")
    .with_description(declaration.description())
}

/// Every node id an instantiation minted: the subnet node and everything
/// inside it, at any depth.
///
/// A caller placing the node needs this to say what it created — to select the
/// new nodes, or to scope an invalidation to them.
pub fn instantiated_ids(instantiated: &Instantiated) -> HashSet<NodeId> {
    let mut ids = match instantiated.node.subnet.as_deref() {
        Some(inner) => node_ids_of(inner),
        None => HashSet::new(),
    };
    ids.insert(instantiated.node.id);
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::{Composition, Layer};
    use crate::exposed::apply::{AssetContext, apply, resolve};
    use crate::exposed::listing::ExposedListing;
    use crate::exposed::{ExposedBinding, ExposedType, ExposedValue};
    use crate::graph::ParameterValue;
    use crate::id::{CompId, DataTypeId, LayerId};
    use crate::network::{self, NET_IN_TYPE_KEY, NET_OUT_TYPE_KEY, PORT_FRAME, PORT_TIME};
    use crate::types::FrameRate;

    /// The subnet's inner graph: the fixed In / Out pair plus one node whose
    /// `text` parameter the template publishes.
    fn inner_graph() -> (Graph, NodeId) {
        let title = NodeId::next();
        let graph = Graph::new()
            .add_node(
                Node::new(NodeId::next(), NET_IN_TYPE_KEY)
                    .with_output(PORT_TIME, DataTypeId::SCALAR),
            )
            .unwrap()
            .add_node(
                Node::new(title, "test")
                    .with_output("out", DataTypeId::FRAME_BUFFER)
                    .with_param("text", ParameterValue::String("Ravel".into()))
                    .with_param("scale", ParameterValue::Float(1.0)),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::next(), NET_OUT_TYPE_KEY)
                    .with_input(PORT_FRAME, &[DataTypeId::FRAME_BUFFER]),
            )
            .unwrap();
        (graph, title)
    }

    /// A graph holding one subnet node, plus the declaration over its inner
    /// `text` and one over a node outside it.
    fn authored() -> (Graph, NodeId, ExposedParameters) {
        let (inner, title) = inner_graph();
        let subnet_id = NodeId::next();
        let mut subnet = Node::new(subnet_id, SUBNET_TYPE_KEY);
        network::adopt_subnet_inner(&mut subnet, inner);
        let outside = NodeId::next();
        let graph = Graph::new()
            .add_node(subnet)
            .unwrap()
            .add_node(
                Node::new(outside, "test")
                    .with_output("out", DataTypeId::SCALAR)
                    .with_param("gain", ParameterValue::Float(2.0)),
            )
            .unwrap();
        let declarations = ExposedParameters::from_declarations([
            ExposedParameter::inferred(
                "headline",
                ExposedValue::String("Ravel".into()),
                ExposedBinding::new(title, "text"),
            )
            .unwrap()
            .with_description("The title card's text"),
            ExposedParameter::inferred(
                "gain",
                ExposedValue::Float(2.0),
                ExposedBinding::new(outside, "gain"),
            )
            .unwrap(),
        ])
        .unwrap();
        (graph, subnet_id, declarations)
    }

    fn template() -> SubgraphTemplate {
        let (graph, subnet_id, declarations) = authored();
        SubgraphTemplate::capture("Title Card", &graph, subnet_id, &declarations)
            .expect("the node is a subnet with an inner graph")
    }

    /// A document whose single layer network holds `graph`.
    fn document_with(graph: Graph) -> Document {
        let comp = Composition::new(CompId::new(1), "Main", (16, 16), FrameRate::new(30, 1), 100)
            .add_layer(Layer::new(LayerId::next(), "L", graph).with_time(0, 0, 100));
        Document::default().with_composition(comp)
    }

    // -----------------------------------------------------------------------
    // Capture
    // -----------------------------------------------------------------------

    /// The completion criterion: a template's public parameters *are* the
    /// declaration mechanism, not a look-alike.
    #[test]
    fn a_templates_public_parameters_are_exposed_parameters() {
        let template = template();
        let declarations: &ExposedParameters = template.declarations();
        assert_eq!(declarations.len(), 1);
        let declaration = declarations.get("headline").expect("captured");
        assert_eq!(declaration.value_type(), ExposedType::String);
        assert_eq!(
            declaration.default_value(),
            &ExposedValue::String("Ravel".into())
        );
        assert_eq!(declaration.description(), "The title card's text");
    }

    /// A declaration over a node outside the subnet belongs to the project
    /// that authored it, not to the template.
    #[test]
    fn capture_leaves_declarations_bound_outside_the_subnet_behind() {
        assert!(!template().declarations().contains("gain"));
    }

    #[test]
    fn capture_refuses_a_node_that_is_not_a_subnet() {
        let (graph, _subnet_id, declarations) = authored();
        let other = graph
            .nodes()
            .find(|node| node.type_key == "test")
            .expect("the outside node")
            .id;
        assert_eq!(
            SubgraphTemplate::capture("T", &graph, other, &declarations),
            Err(SubgraphTemplateError::NotASubnet(other))
        );
        let absent = NodeId::new(9_999_999);
        assert_eq!(
            SubgraphTemplate::capture("T", &graph, absent, &declarations),
            Err(SubgraphTemplateError::NotASubnet(absent))
        );
    }

    #[test]
    fn capture_refuses_a_blank_name() {
        let (graph, subnet_id, declarations) = authored();
        assert_eq!(
            SubgraphTemplate::capture("   ", &graph, subnet_id, &declarations),
            Err(SubgraphTemplateError::EmptyName)
        );
    }

    // -----------------------------------------------------------------------
    // Instantiation
    // -----------------------------------------------------------------------

    #[test]
    fn an_instance_is_a_subnet_node_that_declares_its_inner_interface() {
        let instance = template()
            .instantiate()
            .expect("the template binds only its own nodes");
        assert!(is_subnet_node(&instance.node));
        assert_eq!(instance.node.metadata.label.as_deref(), Some("Title Card"));
        assert!(
            instance.node.subnet.is_some(),
            "the instance owns its own inner graph"
        );
        // `net.out`'s `frame` input becomes the node's one output pin.
        assert_eq!(
            instance
                .node
                .outputs
                .iter()
                .map(|port| port.name.as_str())
                .collect::<Vec<_>>(),
            [PORT_FRAME]
        );
    }

    /// The property two stamps of one template depend on: nothing is shared.
    #[test]
    fn two_instances_share_no_node_id_and_no_binding() {
        let template = template();
        let first = template
            .instantiate()
            .expect("the template binds only its own nodes");
        let second = template
            .instantiate()
            .expect("the template binds only its own nodes");

        let first_ids = instantiated_ids(&first);
        let second_ids = instantiated_ids(&second);
        assert!(
            first_ids.is_disjoint(&second_ids),
            "two instances share no node id"
        );

        let binding_of = |instance: &Instantiated| {
            instance
                .declarations
                .get("headline")
                .expect("declared")
                .binding()
                .clone()
        };
        assert_ne!(binding_of(&first), binding_of(&second));
    }

    /// A binding must land on the node the copy owns, not on the template's
    /// own (which no document holds).
    #[test]
    fn an_instances_binding_names_a_node_of_its_own_copy() {
        let instance = template()
            .instantiate()
            .expect("the template binds only its own nodes");
        let binding = instance
            .declarations
            .get("headline")
            .expect("declared")
            .binding()
            .clone();
        let inner = instance.node.subnet.as_deref().expect("an inner graph");
        assert!(node_ids_of(inner).contains(&binding.node));
        assert!(
            !node_ids_of(template().inner()).contains(&binding.node),
            "the binding moved off the template's own graph"
        );
    }

    /// A template whose declaration names `stale`, a node its own graph does
    /// not hold.
    ///
    /// [`SubgraphTemplate::capture`] cannot produce one — it keeps only what is
    /// inside the subnet — but the file format can: a hand edit, a merge, or a
    /// graph trimmed after the template was written. Going through RON is the
    /// point: this is exactly the template a user's `.ravtpl` can be, and it
    /// loads.
    fn template_with_stale_binding(stale: NodeId) -> SubgraphTemplate {
        let template = template();
        let bound = template
            .declarations()
            .get("headline")
            .expect("declared")
            .binding()
            .node;
        let config = ron::ser::PrettyConfig::new().struct_names(true);
        let text = ron::ser::to_string_pretty(&template, config).expect("it serializes");
        // Only the declarations are rewritten: rewriting the graph's copy of
        // the id too would just rename the node and keep the binding sound.
        let (head, tail) = text
            .split_once("declarations:")
            .expect("the declarations follow the graph");
        let rewritten = tail.replace(
            &format!("NodeId({})", bound.raw()),
            &format!("NodeId({})", stale.raw()),
        );
        assert_ne!(rewritten, tail, "the binding was found");
        ron::from_str(&format!("{head}declarations:{rewritten}"))
            .expect("a template with a stale binding still loads")
    }

    #[test]
    fn instantiate_refuses_a_declaration_the_inner_graph_does_not_hold() {
        let stale = NodeId::new(4_242);
        assert_eq!(
            template_with_stale_binding(stale).instantiate(),
            Err(SubgraphTemplateError::UnboundDeclaration {
                name: "headline".to_string(),
                node: stale,
            })
        );
    }

    /// The destructive path the refusal exists for. A `NodeId` is a bare
    /// integer, so the document being stamped into is free to already hold the
    /// one a stale binding names — here it does, on a node the template has
    /// never heard of. Carrying the binding through would not leave the
    /// declaration unresolvable: it would resolve, onto that node, and publish
    /// a `--param` that edits it.
    #[test]
    fn a_stale_binding_does_not_grab_a_node_the_host_document_already_holds() {
        let stranger = NodeId::next();
        let template = template_with_stale_binding(stranger);
        let host = Graph::new()
            .add_node(
                Node::new(stranger, "test")
                    .with_output("out", DataTypeId::FRAME_BUFFER)
                    .with_param("text", ParameterValue::String("Not the template's".into())),
            )
            .unwrap();

        // What a caller does with an instantiation: place the node and add the
        // declarations to the document, together.
        let document = match template.instantiate() {
            Ok(instance) => {
                let graph = host.clone().add_node(instance.node).unwrap();
                add_declarations(document_with(graph), instance.declarations).0
            }
            Err(err) => {
                assert_eq!(
                    err,
                    SubgraphTemplateError::UnboundDeclaration {
                        name: "headline".to_string(),
                        node: stranger,
                    }
                );
                document_with(host)
            }
        };

        assert!(
            document
                .exposed_parameters
                .bound_to(stranger, "text")
                .is_none(),
            "no declaration reaches the host document's own node"
        );
        assert_eq!(
            parameter_of(&document, stranger, "text"),
            ParameterValue::String("Not the template's".into()),
            "and nothing an exposed parameter carries can reach it"
        );
    }

    // -----------------------------------------------------------------------
    // The completion criterion: same type, same validation
    // -----------------------------------------------------------------------

    /// A template's declarations, once instantiated, are read and applied by
    /// exactly the paths a project's own declarations are — the listing a CLI
    /// reads and the `apply` a render runs.
    #[test]
    fn an_instantiated_template_declares_what_the_headless_path_applies() {
        let instance = template()
            .instantiate()
            .expect("the template binds only its own nodes");
        let inner_title = instance
            .declarations
            .get("headline")
            .expect("declared")
            .binding()
            .node;
        let graph = Graph::new().add_node(instance.node).unwrap();
        let (document, renames) = add_declarations(document_with(graph), instance.declarations);
        assert!(renames.is_empty(), "the first stamp collides with nothing");

        assert_eq!(
            resolve(&document),
            Vec::new(),
            "an instantiated declaration reaches its parameter"
        );

        let listing = ExposedListing::of(&document);
        assert_eq!(listing.parameters.len(), 1);
        let entry = &listing.parameters[0];
        assert_eq!(entry.name, "headline");
        assert_eq!(entry.value_type, ExposedType::String);
        assert_eq!(entry.default, ExposedValue::String("Ravel".into()));
        assert_eq!(entry.description, "The title card's text");
        assert!(entry.resolved);

        let applied = apply(
            document,
            &[(
                "headline".to_string(),
                ExposedValue::String("Stamped".into()),
            )]
            .into_iter()
            .collect(),
            AssetContext::default(),
        )
        .expect("a string reaches a string parameter");
        assert!(applied.issues.is_empty());
        assert_eq!(
            parameter_of(&applied.document, inner_title, "text"),
            ParameterValue::String("Stamped".into()),
            "the value reached the node inside the instantiated subnet"
        );
    }

    /// The same refusal a project's declarations get: a value of the wrong
    /// type is rejected before anything is written.
    #[test]
    fn a_template_declaration_refuses_a_wrong_typed_value_like_any_other() {
        let instance = template()
            .instantiate()
            .expect("the template binds only its own nodes");
        let graph = Graph::new().add_node(instance.node).unwrap();
        let (document, _) = add_declarations(document_with(graph), instance.declarations);
        let err = apply(
            document,
            &[("headline".to_string(), ExposedValue::Float(1.0))]
                .into_iter()
                .collect(),
            AssetContext::default(),
        )
        .expect_err("a float is not a string");
        assert_eq!(
            err,
            crate::exposed::apply::ExposedApplyError::TypeMismatch {
                name: "headline".into(),
                declared: ExposedType::String,
                found: ExposedType::Float,
            }
        );
    }

    fn parameter_of(document: &Document, node: NodeId, key: &str) -> ParameterValue {
        fn find(graph: &Graph, node: NodeId) -> Option<&Node> {
            if let Some(found) = graph.node(node) {
                return Some(found);
            }
            graph
                .nodes()
                .filter_map(|n| n.subnet.as_deref())
                .find_map(|inner| find(inner, node))
        }
        document
            .compositions
            .values()
            .flat_map(|comp| comp.layers.iter())
            .find_map(|layer| find(&layer.network, node))
            .expect("the node is in the document")
            .parameters
            .iter()
            .find(|parameter| parameter.key == key)
            .expect("the parameter is on the node")
            .value
            .clone()
    }

    // -----------------------------------------------------------------------
    // Name collisions
    // -----------------------------------------------------------------------

    #[test]
    fn a_second_stamp_is_renamed_rather_than_refused() {
        let template = template();
        let first = template
            .instantiate()
            .expect("the template binds only its own nodes");
        let second = template
            .instantiate()
            .expect("the template binds only its own nodes");
        let third = template
            .instantiate()
            .expect("the template binds only its own nodes");

        let graph = Graph::new()
            .add_node(first.node)
            .unwrap()
            .add_node(second.node)
            .unwrap()
            .add_node(third.node)
            .unwrap();
        let document = document_with(graph);
        let (document, renames) = add_declarations(document, first.declarations);
        assert!(renames.is_empty());
        let (document, renames) = add_declarations(document, second.declarations);
        assert_eq!(
            renames,
            [("headline".to_string(), "headline_2".to_string())]
        );
        let (document, renames) = add_declarations(document, third.declarations);
        assert_eq!(
            renames,
            [("headline".to_string(), "headline_3".to_string())]
        );

        assert_eq!(
            document
                .exposed_parameters
                .iter()
                .map(ExposedParameter::name)
                .collect::<Vec<_>>(),
            ["headline", "headline_2", "headline_3"]
        );
        // Every one of them still reaches its own copy.
        assert_eq!(resolve(&document), Vec::new());
    }

    #[test]
    fn a_renamed_declaration_keeps_its_type_default_description_and_binding() {
        let template = template();
        let first = template
            .instantiate()
            .expect("the template binds only its own nodes");
        let second = template
            .instantiate()
            .expect("the template binds only its own nodes");
        let wanted = second
            .declarations
            .get("headline")
            .expect("declared")
            .clone();

        let graph = Graph::new()
            .add_node(first.node)
            .unwrap()
            .add_node(second.node)
            .unwrap();
        let (document, _) = add_declarations(document_with(graph), first.declarations);
        let (document, _) = add_declarations(document, second.declarations);

        let renamed = document
            .exposed_parameters
            .get("headline_2")
            .expect("the second stamp is there under its suffix");
        assert_eq!(renamed.value_type(), wanted.value_type());
        assert_eq!(renamed.default_value(), wanted.default_value());
        assert_eq!(renamed.description(), wanted.description());
        assert_eq!(renamed.binding(), wanted.binding());
    }

    // -----------------------------------------------------------------------
    // Robustness against the edits the plan is about (REQ-PROJ-006)
    // -----------------------------------------------------------------------

    /// `NETIF-6` (Collapse / Extract) is EXPO-6's dependency, and its known
    /// limitation is about `ChannelSource::NodeOutput` bindings, not about
    /// declarations. Collapse and extract both preserve node ids, and a
    /// declaration binds by node id — so a declared parameter survives being
    /// moved into and back out of a subnet. Asserting it here is what keeps
    /// that from regressing silently.
    #[test]
    fn a_declaration_survives_collapse_and_extract() {
        let title = NodeId::next();
        let graph = Graph::new()
            .add_node(
                Node::new(title, "test")
                    .with_output("out", DataTypeId::FRAME_BUFFER)
                    .with_param("text", ParameterValue::String("Ravel".into())),
            )
            .unwrap();
        let declarations = ExposedParameters::from_declarations([ExposedParameter::inferred(
            "headline",
            ExposedValue::String("Ravel".into()),
            ExposedBinding::new(title, "text"),
        )
        .unwrap()])
        .unwrap();

        let document = document_with(graph.clone()).with_exposed_parameters(declarations);
        assert_eq!(resolve(&document), Vec::new());

        let (collapsed, subnet_id) =
            network::collapse_to_subnet(graph, [title]).expect("one node collapses");
        let document = document_with(collapsed.clone())
            .with_exposed_parameters(document.exposed_parameters.clone());
        assert_eq!(
            resolve(&document),
            Vec::new(),
            "collapsing the bound node into a subnet keeps the declaration reaching it"
        );

        let extracted = network::extract_subnet(collapsed, subnet_id).expect("it extracts");
        let document =
            document_with(extracted).with_exposed_parameters(document.exposed_parameters);
        assert_eq!(
            resolve(&document),
            Vec::new(),
            "extracting it back keeps the declaration reaching it"
        );
    }
}
