// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Node editor preferences that outlive the panel that shows them.
//!
//! The drawing itself lives in `ravel-app`, but the *choice* is a persisted
//! setting: `ravel-project` reads it into the `node_editor` settings section
//! and only depends on `ravel-core` and this crate, so the type has to be
//! nameable without a GUI (`node-graph-readability-plan.md`, `NGR-3`).

use serde::{Deserialize, Serialize};

/// How an edge is drawn between two ports.
///
/// The serialized spellings are the `settings.toml` values, so renaming a
/// variant changes a file format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeStyle {
    #[default]
    Bezier,
    Straight,
    Step,
}

impl EdgeStyle {
    /// The next style in the order the menu lists them, wrapping around — what
    /// the cycle command steps through.
    pub fn cycled(self) -> Self {
        match self {
            Self::Bezier => Self::Straight,
            Self::Straight => Self::Step,
            Self::Step => Self::Bezier,
        }
    }
}

/// Which way the graph flows across a node: ports on the left and right
/// edges, or inputs on the top edge and outputs on the bottom.
///
/// Persisted like [`EdgeStyle`] (`settings.toml`, `node_editor.flow_direction`),
/// so the spellings are a file format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowDirection {
    #[default]
    Horizontal,
    Vertical,
}
