// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Typed message definitions and channel constructors for inter-pool
//! communication.

use crate::eval::{EvalContext, EvalError};
use crate::id::NodeId;
use crate::types::NodeData;
use crossbeam_channel::{bounded, Receiver, Sender};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// UI → eval pool
// ---------------------------------------------------------------------------

/// Request from the UI thread to evaluate a specific output node.
pub struct EvalRequest {
    pub output: NodeId,
    pub ctx: EvalContext,
    pub reply: Sender<EvalResponse>,
}

/// Evaluation result sent back to the requester.
pub struct EvalResponse {
    pub output: NodeId,
    pub frame: u64,
    pub result: Result<Arc<dyn NodeData>, EvalError>,
}

// ---------------------------------------------------------------------------
// Decode pool → eval pool
// ---------------------------------------------------------------------------

/// A decoded frame delivered from the decode pool.
pub struct DecodedFrame {
    /// Opaque source identifier (asset / clip id).
    pub source_id: u64,
    pub frame: u64,
    pub data: Box<dyn NodeData>,
}

// ---------------------------------------------------------------------------
// Channel bundle
// ---------------------------------------------------------------------------

/// Pre-built channel pair for eval requests.
pub fn eval_channel(capacity: usize) -> (Sender<EvalRequest>, Receiver<EvalRequest>) {
    bounded(capacity)
}

/// Pre-built channel pair for decoded frames flowing into the eval pool.
pub fn decode_channel(capacity: usize) -> (Sender<DecodedFrame>, Receiver<DecodedFrame>) {
    bounded(capacity)
}

/// One-shot style reply channel for a single eval request.
pub fn reply_channel() -> (Sender<EvalResponse>, Receiver<EvalResponse>) {
    bounded(1)
}
