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
