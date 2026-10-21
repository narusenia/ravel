// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::document::CompositionSettings;
use crate::id::{CompId, LayerId};
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
}

impl EditOp {
    /// Whether the operation deletes something. Only a classification: asking
    /// the user is the caller's job.
    pub fn is_destructive(&self) -> bool {
        match self {
            Self::AddComposition { .. } | Self::AddLayer { .. } => false,
            Self::RemoveComposition { .. } | Self::RemoveLayer { .. } => true,
        }
    }
}
