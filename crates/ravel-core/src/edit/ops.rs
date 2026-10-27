// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::document::CompositionSettings;
use super::document::NetworkPath;
use crate::id::{CompId, EdgeId, LayerId, NodeId};
use serde::{Deserialize, Serialize};

/// A composition named by id or by an alias an earlier operation of the same
/// batch defined with `as`. In JSON a number is an id and a string an alias.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompRef {
    Id(CompId),
    Alias(String),
}

/// A layer named by id or by alias; see [`CompRef`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LayerRef {
    Id(LayerId),
    Alias(String),
}

/// A node named by id or by alias; see [`CompRef`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NodeRef {
    Id(NodeId),
    Alias(String),
}

/// A port named by its zero-based number or by its name. In JSON a number is
/// an index and a string a name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PortRef {
    Index(u32),
    Name(String),
}

/// One end of a connection: a node and one of its ports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub node: NodeRef,
    pub port: PortRef,
}

/// One typed edit of a [`Document`](crate::composition::Document).
///
/// The set is closed on purpose: `match` exhaustiveness makes a new operation
/// without an implementation a compile error. The operations that create
/// something take an optional `as` alias (`alias` in Rust) that later
/// operations of the same batch can use as a reference; aliases are not kept
/// beyond the batch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    AddComposition {
        settings: CompositionSettings,
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
    },
    RemoveComposition {
        comp: CompRef,
    },
    /// Stack a layer built from the builtin template `template` on top of
    /// `comp`.
    AddLayer {
        comp: CompRef,
        template: String,
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
    },
    /// Remove layers. A locked layer is an error, not a skip.
    RemoveLayer {
        comp: CompRef,
        layers: Vec<LayerRef>,
    },
    /// Create a node of the registered type `type_key` in the network `path`
    /// addresses, above everything already there.
    AddNode {
        path: NetworkPath,
        type_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<(f32, f32)>,
        #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
    },
    /// Remove a node and the edges touching it. A network's own `net.in` /
    /// `net.out` cannot be removed.
    RemoveNode {
        path: NetworkPath,
        node: NodeRef,
    },
    /// Connect an output port to an input port. An edge already on that input
    /// is replaced, and a variadic group keeps exactly one free slot.
    Connect {
        path: NetworkPath,
        from: Endpoint,
        to: Endpoint,
    },
    /// Remove one edge; its target's variadic group is compacted.
    Disconnect {
        path: NetworkPath,
        edge: EdgeId,
    },
}

impl EditOp {
    /// Whether the operation deletes something. Only a classification: asking
    /// the user is the caller's job.
    pub fn is_destructive(&self) -> bool {
        match self {
            Self::AddComposition { .. }
            | Self::AddLayer { .. }
            | Self::AddNode { .. }
            | Self::Connect { .. } => false,
            Self::RemoveComposition { .. }
            | Self::RemoveLayer { .. }
            | Self::RemoveNode { .. }
            | Self::Disconnect { .. } => true,
        }
    }
}
