// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::document::NetworkPath;
use super::ops::PortRef;
use crate::composition::DocumentValidationError;
use crate::composition::templates::TemplateError;
use crate::graph::GraphError;
use crate::id::{CompId, EdgeId, LayerId, NodeId};
use thiserror::Error;

/// Why an edit operation was refused. A refused operation leaves the caller's
/// document untouched: [`super::apply`] never mutates its input.
#[derive(Debug, Error)]
pub enum EditError {
    #[error("composition {0:?} does not exist")]
    CompositionNotFound(CompId),
    #[error("layer {layer:?} does not exist in composition {comp:?}")]
    LayerNotFound { comp: CompId, layer: LayerId },
    #[error("layer {layer:?} in composition {comp:?} is locked")]
    LayerLocked { comp: CompId, layer: LayerId },
    #[error("unknown layer template {0:?}")]
    UnknownTemplate(String),
    #[error("layer template could not be instantiated: {0}")]
    Template(#[from] TemplateError),
    #[error("alias {0:?} is already defined in this batch")]
    DuplicateAlias(String),
    #[error("alias {0:?} is not defined by an earlier operation of this batch")]
    UnknownAlias(String),
    #[error("alias {0:?} names a different kind of object than the operation needs")]
    AliasKindMismatch(String),
    #[error("network {0:?} does not exist")]
    NetworkNotFound(NetworkPath),
    #[error("node {0:?} does not exist in the network")]
    NodeNotFound(NodeId),
    #[error("edge {0:?} does not exist in the network")]
    EdgeNotFound(EdgeId),
    #[error("node {0:?} is a network boundary node and cannot be removed")]
    BoundaryNode(NodeId),
    #[error("unknown node type {0:?}")]
    UnknownNodeType(String),
    #[error("node {node:?} has no parameter {key:?}")]
    UnknownParameter { node: NodeId, key: String },
    #[error("parameter {key:?} of node {node:?} does not take that kind of value")]
    ParameterTypeMismatch { node: NodeId, key: String },
    #[error("parameter {key:?} of node {node:?} is animated and no frame was given to key")]
    AnimatedParameter { node: NodeId, key: String },
    #[error("node {node:?} has no such port {port:?} (input side: {input})")]
    PortNotFound {
        node: NodeId,
        port: PortRef,
        input: bool,
    },
    #[error(
        "output {output_port} of node {output_node:?} is not accepted by input {input_port} of node {input_node:?}"
    )]
    PortTypeMismatch {
        output_node: NodeId,
        output_port: u32,
        input_node: NodeId,
        input_port: u32,
    },
    #[error("the graph refused the edit: {0}")]
    Graph(#[from] GraphError),
    #[error("the edit produced an invalid document: {0}")]
    InvalidDocument(#[from] DocumentValidationError),
}

impl EditError {
    /// A stable identifier that is never translated, for callers (the CLI, MCP)
    /// that report the error rather than display it.
    pub fn code(&self) -> &'static str {
        match self {
            Self::CompositionNotFound(_) => "composition-not-found",
            Self::LayerNotFound { .. } => "layer-not-found",
            Self::LayerLocked { .. } => "layer-locked",
            Self::UnknownTemplate(_) => "unknown-template",
            Self::Template(_) => "template-failed",
            Self::DuplicateAlias(_) => "duplicate-alias",
            Self::UnknownAlias(_) => "unknown-alias",
            Self::AliasKindMismatch(_) => "alias-kind-mismatch",
            Self::NetworkNotFound(_) => "network-not-found",
            Self::NodeNotFound(_) => "node-not-found",
            Self::EdgeNotFound(_) => "edge-not-found",
            Self::BoundaryNode(_) => "boundary-node",
            Self::UnknownNodeType(_) => "unknown-node-type",
            Self::UnknownParameter { .. } => "unknown-parameter",
            Self::ParameterTypeMismatch { .. } => "parameter-type-mismatch",
            Self::AnimatedParameter { .. } => "animated-parameter",
            Self::PortNotFound { .. } => "port-not-found",
            Self::PortTypeMismatch { .. } => "port-type-mismatch",
            Self::Graph(_) => "graph",
            Self::InvalidDocument(_) => "invalid-document",
        }
    }
}

/// A batch that stopped at operation `index` (zero-based). Nothing was applied.
#[derive(Debug, Error)]
#[error("operation {index} failed: {error}")]
pub struct BatchError {
    pub index: usize,
    #[source]
    pub error: EditError,
}
