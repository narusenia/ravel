// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::composition::DocumentValidationError;
use crate::composition::templates::TemplateError;
use crate::id::{CompId, LayerId};
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
