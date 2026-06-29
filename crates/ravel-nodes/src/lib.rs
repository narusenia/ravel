// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Built-in node processors for the Ravel DAG evaluation pipeline.
//!
//! Each module implements [`ravel_core::eval::NodeProcessor`] for one of the
//! registered built-in node types. GPU-accelerated processors use
//! [`ravel_gpu`] for shader compilation and texture management.

pub mod blur;
pub mod color_correct;
pub mod constant;
pub mod merge;
pub mod transform;
