// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! TimeOffset processor for composition layers.
//!
//! Handles the time mapping between a Layer's position on the Composition
//! timeline and its source-local frame range:
//!
//! ```text
//! source_frame = (comp_frame - start_frame).clamp(in_frame, out_frame - 1)
//! ```
//!
//! For PreComp layers, additionally converts between parent and child fps:
//! ```text
//! child_frame = source_frame * (child_fps / parent_fps)
//! ```
//!
//! Currently passes through the input unchanged. The actual time remapping
//! requires evaluator support for per-node EvalContext modification, which
//! is a planned enhancement. The frame parameters are stored and validated
//! so the processor is ready for integration.

use ravel_core::eval::{EvalContext, NodeProcessor};
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::types::NodeData;

pub struct TimeOffsetProcessor {
    pub start_frame: i64,
    pub in_frame: u64,
    pub out_frame: u64,
    pub child_fps_num: Option<u32>,
    pub child_fps_den: Option<u32>,
}

impl TimeOffsetProcessor {
    pub fn from_node(node: &Node) -> Self {
        Self {
            start_frame: get_param_int(&node.parameters, "start_frame", 0) as i64,
            in_frame: get_param_int(&node.parameters, "in_frame", 0).max(0) as u64,
            out_frame: get_param_int(&node.parameters, "out_frame", 0).max(0) as u64,
            child_fps_num: get_param_opt_int(&node.parameters, "child_fps_num").map(|v| v as u32),
            child_fps_den: get_param_opt_int(&node.parameters, "child_fps_den").map(|v| v as u32),
        }
    }

    /// Calculate the source-local frame for a given composition frame.
    pub fn map_frame(&self, comp_frame: u64) -> u64 {
        let offset = comp_frame as i64 - self.start_frame;
        let source = offset.max(0) as u64 + self.in_frame;

        let clamped = if self.out_frame > self.in_frame {
            source.min(self.out_frame - 1)
        } else {
            source
        };

        if let (Some(child_num), Some(child_den)) = (self.child_fps_num, self.child_fps_den)
            && child_den > 0
        {
            return (clamped as f64 * child_num as f64 / child_den as f64) as u64;
        }

        clamped
    }
}

impl NodeProcessor for TimeOffsetProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        // Pass through: actual time remapping requires evaluator context
        // modification support. The map_frame() method is ready for integration.
        if let Some(input) = inputs.first()
            && let Some(fb) = crate::gpu_util::clone_frame_value(*input)
        {
            return Ok(fb);
        }
        anyhow::bail!("comp.time_offset: no valid FrameBuffer input")
    }

    fn is_time_dependent(&self) -> bool {
        true
    }
}

fn get_param_int(params: &[ravel_core::graph::Parameter], key: &str, default: i32) -> i32 {
    params
        .iter()
        .find(|p| p.key == key)
        .and_then(|p| match &p.value {
            ParameterValue::Int(v) => Some(*v),
            _ => None,
        })
        .unwrap_or(default)
}

fn get_param_opt_int(params: &[ravel_core::graph::Parameter], key: &str) -> Option<i32> {
    params
        .iter()
        .find(|p| p.key == key)
        .and_then(|p| match &p.value {
            ParameterValue::Int(v) => Some(*v),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::graph::Node;
    use ravel_core::id::{DataTypeId, NodeId};

    fn time_offset_node(start: i32, in_f: i32, out_f: i32) -> Node {
        Node::new(NodeId::new(1), "comp.time_offset")
            .with_input("input", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("start_frame", ParameterValue::Int(start))
            .with_param("in_frame", ParameterValue::Int(in_f))
            .with_param("out_frame", ParameterValue::Int(out_f))
    }

    #[test]
    fn map_frame_basic() {
        let proc = TimeOffsetProcessor::from_node(&time_offset_node(0, 0, 100));
        assert_eq!(proc.map_frame(0), 0);
        assert_eq!(proc.map_frame(50), 50);
        assert_eq!(proc.map_frame(99), 99);
    }

    #[test]
    fn map_frame_clamps_to_out() {
        let proc = TimeOffsetProcessor::from_node(&time_offset_node(0, 0, 100));
        assert_eq!(proc.map_frame(150), 99);
    }

    #[test]
    fn map_frame_with_start_offset() {
        let proc = TimeOffsetProcessor::from_node(&time_offset_node(10, 0, 100));
        // comp_frame 10 → source 0, comp_frame 20 → source 10
        assert_eq!(proc.map_frame(10), 0);
        assert_eq!(proc.map_frame(20), 10);
    }

    #[test]
    fn map_frame_before_start_clamps_to_in() {
        let proc = TimeOffsetProcessor::from_node(&time_offset_node(10, 5, 100));
        // comp_frame 0 → offset = -10 → clamped to 0 → + in_frame(5) = 5
        assert_eq!(proc.map_frame(0), 5);
    }

    #[test]
    fn map_frame_with_in_trim() {
        let proc = TimeOffsetProcessor::from_node(&time_offset_node(0, 30, 90));
        // comp_frame 0 → 0 + in(30) = 30
        // comp_frame 50 → 50 + 30 = 80
        assert_eq!(proc.map_frame(0), 30);
        assert_eq!(proc.map_frame(50), 80);
    }

    #[test]
    fn map_frame_with_fps_conversion() {
        let node = time_offset_node(0, 0, 100)
            .with_param("child_fps_num", ParameterValue::Int(60))
            .with_param("child_fps_den", ParameterValue::Int(30));
        let proc = TimeOffsetProcessor::from_node(&node);
        // Frame 10 at parent 30fps → child 60fps = 20
        assert_eq!(proc.map_frame(10), 20);
    }

    #[test]
    fn is_time_dependent() {
        let proc = TimeOffsetProcessor::from_node(&time_offset_node(0, 0, 100));
        assert!(proc.is_time_dependent());
    }

    #[test]
    fn negative_start_frame() {
        let proc = TimeOffsetProcessor::from_node(&time_offset_node(-30, 0, 100));
        // comp_frame 0 → offset = 0 - (-30) = 30 → source 30
        assert_eq!(proc.map_frame(0), 30);
    }
}
