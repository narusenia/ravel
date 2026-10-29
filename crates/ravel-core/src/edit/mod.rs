// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed, serializable edits of a [`Document`], applied without a UI.
//!
//! [`apply`] and [`apply_batch`] take a `&Document` and return a new one, so a
//! refused operation cannot have changed anything. Every result has passed
//! [`Document::validate`]. This layer does not commit: the caller owns undo
//! and evaluation, and one [`apply_batch`] result committed once is one undo
//! step.

mod animation;
pub mod document;
mod error;
pub mod network;
mod ops;
pub mod param;
#[cfg(test)]
mod tests;

use crate::composition::templates::builtin_layer_template;
use crate::composition::{Document, Layer};
use crate::graph::Graph;
use crate::id::{CompId, EdgeId, LayerId, NodeId};
use crate::registry::NodeRegistry;
use animation::{
    ChannelWrite, LayerEdit, apply_layer_edit, build_curve, compiled_expression, param_clamp,
    shell_clamp, write_param_channel, write_shell_channel,
};
pub use document::{CompositionSettings, NetworkPath};
use document::{
    add_composition, add_layer_from_template, remove_composition, remove_layer, replace_network,
    resolve_network, update_layer,
};
pub use error::{BatchError, EditError};
use network::{
    connect_edge_and_update_variadic_inputs, remove_edge_and_compact, remove_node_and_compact,
    resolve_port,
};
pub use ops::{
    ChannelTarget, CompRef, EditOp, Endpoint, KeyInput, KeyInterpolation, LayerRef, NetworkRef,
    NodeRef, ParamInput, PortRef,
};
use param::set_parameter;
use std::collections::BTreeMap;

/// What an operation needs besides the document.
pub struct EditContext<'a> {
    pub registry: &'a NodeRegistry,
    /// The layer-local frame animated parameters are keyed at. `None` makes a
    /// write to an animated parameter an error.
    pub local_frame: Option<u64>,
    /// Whether `SetParameter` may replace a channel driven by an expression,
    /// a node output or a blend (the write collapses it to a constant). Off by
    /// default: only the Properties scrub, which has always done that and
    /// undoes it, turns it on. Everything else gets `driven-parameter`.
    pub allow_driven_overwrite: bool,
}

/// Something an operation created and gave an id to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Created {
    Composition(CompId),
    Layer(LayerId),
    /// The node itself; the inner nodes a subnet or iterate brings are not
    /// listed.
    Node(NodeId),
    Edge(EdgeId),
}

/// The result of an accepted edit.
#[derive(Clone, Debug)]
pub struct Applied {
    pub document: Document,
    /// Everything created, in operation order.
    pub created: Vec<Created>,
    /// The `as` aliases the batch defined and what they resolved to.
    pub aliases: BTreeMap<String, Created>,
}

type Aliases = BTreeMap<String, Created>;

/// Apply one operation. On `Err` the caller's `doc` is unchanged.
pub fn apply(doc: &Document, op: &EditOp, cx: &EditContext) -> Result<Applied, EditError> {
    apply_batch(doc, std::slice::from_ref(op), cx).map_err(|e| e.error)
}

/// Apply `ops` in order to a working copy: all of them, or none. On `Err` the
/// index of the failing operation is reported and `doc` is unchanged.
pub fn apply_batch(
    doc: &Document,
    ops: &[EditOp],
    cx: &EditContext,
) -> Result<Applied, BatchError> {
    let mut document = doc.clone();
    let mut created = Vec::new();
    let mut aliases = Aliases::new();
    for (index, op) in ops.iter().enumerate() {
        let fail = |error| BatchError { index, error };
        let made = step(&mut document, op, cx, &mut aliases).map_err(fail)?;
        document.validate().map_err(|e| fail(e.into()))?;
        created.extend(made);
    }
    Ok(Applied {
        document,
        created,
        aliases,
    })
}

/// Refuse a taken alias before the operation does any work.
fn check_alias(aliases: &Aliases, alias: &Option<String>) -> Result<(), EditError> {
    match alias {
        Some(name) if aliases.contains_key(name) => Err(EditError::DuplicateAlias(name.clone())),
        _ => Ok(()),
    }
}

fn define_alias(aliases: &mut Aliases, alias: &Option<String>, made: Created) {
    if let Some(name) = alias {
        aliases.insert(name.clone(), made);
    }
}

fn resolve_comp(aliases: &Aliases, r: &CompRef) -> Result<CompId, EditError> {
    match r {
        CompRef::Id(id) => Ok(*id),
        CompRef::Alias(name) => match aliases.get(name) {
            Some(Created::Composition(id)) => Ok(*id),
            Some(_) => Err(EditError::AliasKindMismatch(name.clone())),
            None => Err(EditError::UnknownAlias(name.clone())),
        },
    }
}

fn resolve_layer(aliases: &Aliases, r: &LayerRef) -> Result<LayerId, EditError> {
    match r {
        LayerRef::Id(id) => Ok(*id),
        LayerRef::Alias(name) => match aliases.get(name) {
            Some(Created::Layer(id)) => Ok(*id),
            Some(_) => Err(EditError::AliasKindMismatch(name.clone())),
            None => Err(EditError::UnknownAlias(name.clone())),
        },
    }
}

fn resolve_node(aliases: &Aliases, r: &NodeRef) -> Result<NodeId, EditError> {
    match r {
        NodeRef::Id(id) => Ok(*id),
        NodeRef::Alias(name) => match aliases.get(name) {
            Some(Created::Node(id)) => Ok(*id),
            Some(_) => Err(EditError::AliasKindMismatch(name.clone())),
            None => Err(EditError::UnknownAlias(name.clone())),
        },
    }
}

fn resolve_path(aliases: &Aliases, r: &NetworkRef) -> Result<NetworkPath, EditError> {
    Ok(NetworkPath {
        comp: resolve_comp(aliases, &r.comp)?,
        layer: resolve_layer(aliases, &r.layer)?,
        subnets: r
            .subnets
            .iter()
            .map(|n| resolve_node(aliases, n))
            .collect::<Result<_, _>>()?,
    })
}

/// Rebuild `doc` with the network `path` addresses replaced by `f(network)`.
fn edit_network<T>(
    doc: &mut Document,
    path: &NetworkPath,
    f: impl FnOnce(Graph) -> Result<(Graph, T), EditError>,
) -> Result<T, EditError> {
    let composition = doc
        .get_composition(path.comp)
        .ok_or(EditError::CompositionNotFound(path.comp))?;
    if composition.get_layer(path.layer).is_none() {
        return Err(EditError::LayerNotFound {
            comp: path.comp,
            layer: path.layer,
        });
    }
    let graph = resolve_network(doc, path)
        .ok_or_else(|| EditError::NetworkNotFound(path.clone()))?
        .clone();
    let (graph, out) = f(graph)?;
    *doc = replace_network(doc, path, graph)
        .ok_or_else(|| EditError::NetworkNotFound(path.clone()))?;
    Ok(out)
}

/// A copy of layer `layer` of `comp`, for an edit that needs it before it
/// rewrites anything.
fn layer_of(doc: &Document, comp: CompId, layer: LayerId) -> Result<Layer, EditError> {
    doc.get_composition(comp)
        .ok_or(EditError::CompositionNotFound(comp))?
        .get_layer(layer)
        .cloned()
        .ok_or(EditError::LayerNotFound { comp, layer })
}

/// Rewrite one layer through `f`; the layer is left as it was on `Err`.
fn edit_layer<T>(
    doc: &mut Document,
    comp: CompId,
    layer: LayerId,
    f: impl FnOnce(&mut Layer) -> Result<T, EditError>,
) -> Result<T, EditError> {
    let mut edited = layer_of(doc, comp, layer)?;
    let out = f(&mut edited)?;
    *doc = update_layer(doc, comp, layer, |l| *l = edited)
        .ok_or(EditError::LayerNotFound { comp, layer })?;
    Ok(out)
}

/// What [`write_channel`] puts on a channel.
enum ChannelInput<'a> {
    /// `SetKeyframes`
    Keys(&'a [KeyInput]),
    /// `ClearAnimation`
    Constant(f32),
    /// `SetExpression`
    Expression(&'a str),
}

/// Write one channel of `target`: the keys of `SetKeyframes`, the constant of
/// `ClearAnimation` or the expression of `SetExpression`.
fn write_channel(
    doc: &mut Document,
    target: &ChannelTarget,
    component: u32,
    input: ChannelInput<'_>,
    cx: &EditContext,
    aliases: &Aliases,
) -> Result<(), EditError> {
    match target {
        ChannelTarget::Parameter { path, node, key } => {
            let path = &resolve_path(aliases, path)?;
            let node = resolve_node(aliases, node)?;
            let layer = layer_of(doc, path.comp, path.layer)?;
            edit_network(doc, path, |graph| {
                let write = match &input {
                    ChannelInput::Keys(keys) => ChannelWrite::Keys(build_curve(
                        keys,
                        &layer,
                        param_clamp(&graph, node, key, cx)?,
                    )?),
                    ChannelInput::Constant(value) => {
                        ChannelWrite::Constant(param_clamp(&graph, node, key, cx)?(*value))
                    }
                    ChannelInput::Expression(source) => {
                        ChannelWrite::Expression(compiled_expression(source)?)
                    }
                };
                Ok((write_param_channel(graph, node, key, component, write)?, ()))
            })
        }
        ChannelTarget::Shell {
            comp,
            layer,
            property,
        } => {
            let comp = resolve_comp(aliases, comp)?;
            let layer = resolve_layer(aliases, layer)?;
            edit_layer(doc, comp, layer, |layer| {
                let clamp = shell_clamp(*property);
                let write = match &input {
                    ChannelInput::Keys(keys) => {
                        ChannelWrite::Keys(build_curve(keys, layer, clamp)?)
                    }
                    ChannelInput::Constant(value) => ChannelWrite::Constant(clamp(*value)),
                    ChannelInput::Expression(source) => {
                        ChannelWrite::Expression(compiled_expression(source)?)
                    }
                };
                write_shell_channel(layer, *property, component, write)
            })
        }
    }
}

/// Run one operation against the working `doc`. May leave `doc` half-edited on
/// `Err`; [`apply_batch`] discards it.
fn step(
    doc: &mut Document,
    op: &EditOp,
    cx: &EditContext,
    aliases: &mut Aliases,
) -> Result<Vec<Created>, EditError> {
    match op {
        EditOp::AddComposition { settings, alias } => {
            check_alias(aliases, alias)?;
            let (next, id) = add_composition(doc, settings.clone());
            *doc = next;
            let made = Created::Composition(id);
            define_alias(aliases, alias, made);
            Ok(vec![made])
        }
        EditOp::RemoveComposition { comp } => {
            let comp = resolve_comp(aliases, comp)?;
            *doc = remove_composition(doc, comp).ok_or(EditError::CompositionNotFound(comp))?;
            Ok(Vec::new())
        }
        EditOp::AddLayer {
            comp,
            template,
            alias,
        } => {
            check_alias(aliases, alias)?;
            let comp = resolve_comp(aliases, comp)?;
            if doc.get_composition(comp).is_none() {
                return Err(EditError::CompositionNotFound(comp));
            }
            let template = builtin_layer_template(template)
                .ok_or_else(|| EditError::UnknownTemplate(template.clone()))?;
            let (next, id) = add_layer_from_template(doc, comp, template, cx.registry)?
                .ok_or(EditError::CompositionNotFound(comp))?;
            *doc = next;
            let made = Created::Layer(id);
            define_alias(aliases, alias, made);
            Ok(vec![made])
        }
        EditOp::RemoveLayer { comp, layers } => {
            let comp = resolve_comp(aliases, comp)?;
            for layer in layers {
                let layer = resolve_layer(aliases, layer)?;
                let composition = doc
                    .get_composition(comp)
                    .ok_or(EditError::CompositionNotFound(comp))?;
                let found = composition
                    .get_layer(layer)
                    .ok_or(EditError::LayerNotFound { comp, layer })?;
                if found.locked {
                    return Err(EditError::LayerLocked { comp, layer });
                }
                *doc =
                    remove_layer(doc, comp, layer).ok_or(EditError::CompositionNotFound(comp))?;
            }
            Ok(Vec::new())
        }
        EditOp::AddNode {
            path,
            type_key,
            position,
            params,
            alias,
        } => {
            check_alias(aliases, alias)?;
            let path = &resolve_path(aliases, path)?;
            let comp = doc.get_composition(path.comp).cloned();
            let id = edit_network(doc, path, |graph| {
                let mut node = cx
                    .registry
                    .create_node(type_key, NodeId::next())
                    .ok_or_else(|| EditError::UnknownNodeType(type_key.clone()))?;
                if let Some(position) = position {
                    node.metadata.position = *position;
                }
                node.metadata.z = graph
                    .nodes()
                    .filter(|n| !n.metadata.synthetic)
                    .map(|n| n.metadata.z)
                    .max()
                    .map_or(0, |z| z + 1);
                let id = node.id;
                let mut graph = graph.add_node(node)?;
                for (key, value) in params {
                    graph = set_parameter(graph, id, key, value, cx, comp.as_deref())?;
                }
                Ok((graph, id))
            })?;
            let made = Created::Node(id);
            define_alias(aliases, alias, made);
            Ok(vec![made])
        }
        EditOp::RemoveNode { path, node } => {
            let path = &resolve_path(aliases, path)?;
            let node = resolve_node(aliases, node)?;
            edit_network(doc, path, |graph| {
                let found = graph.node(node).ok_or(EditError::NodeNotFound(node))?;
                if crate::network::is_in_node(found) || crate::network::is_out_node(found) {
                    return Err(EditError::BoundaryNode(node));
                }
                Ok((remove_node_and_compact(graph, node)?, ()))
            })?;
            Ok(Vec::new())
        }
        EditOp::Connect { path, from, to } => {
            let path = &resolve_path(aliases, path)?;
            let from_node = resolve_node(aliases, &from.node)?;
            let to_node = resolve_node(aliases, &to.node)?;
            let edge = EdgeId::next();
            edit_network(doc, path, |graph| {
                let output = graph
                    .node(from_node)
                    .ok_or(EditError::NodeNotFound(from_node))?;
                let output = resolve_port(output, &from.port, false)?;
                let input = graph
                    .node(to_node)
                    .ok_or(EditError::NodeNotFound(to_node))?;
                let input = resolve_port(input, &to.port, true)?;
                let graph = connect_edge_and_update_variadic_inputs(
                    graph,
                    edge,
                    from_node,
                    crate::id::OutputPortIndex(output),
                    to_node,
                    crate::id::InputPortIndex(input),
                )?;
                Ok((graph, ()))
            })?;
            Ok(vec![Created::Edge(edge)])
        }
        EditOp::SetParameter {
            path,
            node,
            key,
            value,
        } => {
            let path = &resolve_path(aliases, path)?;
            let node = resolve_node(aliases, node)?;
            let comp = doc.get_composition(path.comp).cloned();
            edit_network(doc, path, |graph| {
                let graph = set_parameter(graph, node, key, value, cx, comp.as_deref())?;
                Ok((graph, ()))
            })?;
            Ok(Vec::new())
        }
        EditOp::SetKeyframes {
            target,
            component,
            keys,
        } => {
            write_channel(
                doc,
                target,
                *component,
                ChannelInput::Keys(keys),
                cx,
                aliases,
            )?;
            Ok(Vec::new())
        }
        EditOp::ClearAnimation {
            target,
            component,
            value,
        } => {
            if !value.is_finite() {
                return Err(EditError::InvalidKeyframes(
                    "the value is not a finite number".into(),
                ));
            }
            write_channel(
                doc,
                target,
                *component,
                ChannelInput::Constant(*value),
                cx,
                aliases,
            )?;
            Ok(Vec::new())
        }
        EditOp::SetExpression {
            target,
            component,
            expression,
        } => {
            let input = ChannelInput::Expression(expression);
            write_channel(doc, target, *component, input, cx, aliases)?;
            Ok(Vec::new())
        }
        EditOp::SetLayer {
            comp,
            layer,
            name,
            start_frame,
            in_frame,
            out_frame,
            anchor_point,
            position,
            scale,
            rotation,
            opacity,
            blend_mode,
            parent,
            solo,
            muted,
            locked,
        } => {
            let comp = resolve_comp(aliases, comp)?;
            let layer = resolve_layer(aliases, layer)?;
            let parent = parent
                .as_ref()
                .map(|p| p.as_ref().map(|p| resolve_layer(aliases, p)).transpose())
                .transpose()?;
            // `Document::validate` does not look at parenting, so the cycle
            // rule is enforced here (the Properties picker does the same by
            // not offering a cycle).
            if let Some(Some(parent)) = parent {
                let composition = doc
                    .get_composition(comp)
                    .ok_or(EditError::CompositionNotFound(comp))?;
                let candidate = composition
                    .get_layer(parent)
                    .ok_or(EditError::LayerNotFound {
                        comp,
                        layer: parent,
                    })?;
                if parent == layer || composition.descends_from(candidate, layer) {
                    return Err(EditError::InvalidLayerEdit(format!(
                        "layer {parent:?} cannot be the parent of {layer:?}: it would be its own ancestor"
                    )));
                }
            }
            let edit = LayerEdit {
                name: name.clone(),
                start_frame: *start_frame,
                in_frame: *in_frame,
                out_frame: *out_frame,
                anchor_point: *anchor_point,
                position: *position,
                scale: *scale,
                rotation: *rotation,
                opacity: *opacity,
                blend_mode: *blend_mode,
                parent,
                solo: *solo,
                muted: *muted,
                locked: *locked,
            };
            edit_layer(doc, comp, layer, |l| apply_layer_edit(l, edit))?;
            Ok(Vec::new())
        }
        EditOp::Disconnect { path, edge } => {
            let path = &resolve_path(aliases, path)?;
            edit_network(doc, path, |graph| {
                Ok((remove_edge_and_compact(graph, *edge)?, ()))
            })?;
            Ok(Vec::new())
        }
    }
}
