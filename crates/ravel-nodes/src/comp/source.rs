// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Source node processor for composition layers.
//!
//! Generates or retrieves the source content for a layer based on its
//! [`LayerSource`] type. Currently produces a solid-color placeholder
//! FrameBuffer; real implementations will integrate with the media
//! pipeline, shape renderer, text engine, or PreComp evaluator.

use ravel_core::eval::{EvalContext, NodeProcessor};
use ravel_core::geometry::Geometry;
use ravel_core::graph::{Node, ParameterValue};
use ravel_core::types::{FrameBuffer, NodeData};

pub struct CompSourceProcessor {
    source_type: String,
    width: u32,
    height: u32,
    color: [f32; 4],
}

impl CompSourceProcessor {
    pub fn from_node(node: &Node) -> Self {
        let source_type = node
            .type_key
            .strip_prefix("comp.source.")
            .unwrap_or("null")
            .to_string();

        let width = get_param_int(&node.parameters, "width", 1920) as u32;
        let height = get_param_int(&node.parameters, "height", 1080) as u32;

        let r = get_param_float(&node.parameters, "r", 0.0);
        let g = get_param_float(&node.parameters, "g", 0.0);
        let b = get_param_float(&node.parameters, "b", 0.0);
        let a = get_param_float(
            &node.parameters,
            "a",
            match source_type.as_str() {
                "null" => 0.0,
                _ => 1.0,
            },
        );

        Self {
            source_type,
            width,
            height,
            color: [r, g, b, a],
        }
    }
}

impl NodeProcessor for CompSourceProcessor {
    fn process(
        &self,
        _ctx: &EvalContext,
        inputs: &[&dyn NodeData],
    ) -> anyhow::Result<Box<dyn NodeData>> {
        // Shape sources pass through the Geometry from the referenced shape node.
        if self.source_type == "shape" {
            if let Some(geo) = inputs.first().and_then(|d| d.downcast_ref::<Geometry>()) {
                return Ok(Box::new(geo.clone()));
            }
            return Ok(Box::new(Geometry::new()));
        }

        let n = (self.width * self.height) as usize;
        let mut data = Vec::with_capacity(n * 4);
        for _ in 0..n {
            data.extend_from_slice(&self.color);
        }
        Ok(Box::new(FrameBuffer {
            width: self.width,
            height: self.height,
            data: data.into(),
        }))
    }

    fn is_time_dependent(&self) -> bool {
        matches!(self.source_type.as_str(), "media" | "precomp" | "generator")
    }
}

fn get_param_float(params: &[ravel_core::graph::Parameter], key: &str, default: f32) -> f32 {
    params
        .iter()
        .find(|p| p.key == key)
        .and_then(|p| match &p.value {
            ParameterValue::Float(v) => Some(*v),
            _ => None,
        })
        .unwrap_or(default)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::graph::Node;
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (100, 100))
    }

    #[test]
    fn solid_source_produces_colored_fb() {
        let node = Node::new(NodeId::new(1), "comp.source.solid")
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("width", ParameterValue::Int(4))
            .with_param("height", ParameterValue::Int(4))
            .with_param("r", ParameterValue::Float(1.0))
            .with_param("g", ParameterValue::Float(0.5))
            .with_param("b", ParameterValue::Float(0.0))
            .with_param("a", ParameterValue::Float(1.0));

        let proc = CompSourceProcessor::from_node(&node);
        let out = proc.process(&ctx(), &[]).unwrap();
        let fb = out.downcast_ref::<FrameBuffer>().unwrap();

        assert_eq!(fb.width, 4);
        assert_eq!(fb.height, 4);
        assert!((fb.data[0] - 1.0).abs() < f32::EPSILON);
        assert!((fb.data[1] - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn null_source_is_transparent() {
        let node = Node::new(NodeId::new(1), "comp.source.null")
            .with_output("output", DataTypeId::FRAME_BUFFER);
        let proc = CompSourceProcessor::from_node(&node);
        let out = proc.process(&ctx(), &[]).unwrap();
        let fb = out.downcast_ref::<FrameBuffer>().unwrap();
        assert!((fb.data[3] - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn media_source_is_time_dependent() {
        let node = Node::new(NodeId::new(1), "comp.source.media")
            .with_output("output", DataTypeId::FRAME_BUFFER);
        let proc = CompSourceProcessor::from_node(&node);
        assert!(proc.is_time_dependent());
    }

    #[test]
    fn solid_source_is_not_time_dependent() {
        let node = Node::new(NodeId::new(1), "comp.source.solid")
            .with_output("output", DataTypeId::FRAME_BUFFER);
        let proc = CompSourceProcessor::from_node(&node);
        assert!(!proc.is_time_dependent());
    }

    #[test]
    fn shape_source_passes_through_geometry() {
        use ravel_core::geometry::{Geometry, Primitive};
        use ravel_core::types::Vec2;

        let node = Node::new(NodeId::new(1), "comp.source.shape")
            .with_output("output", DataTypeId::GEOMETRY);
        let proc = CompSourceProcessor::from_node(&node);

        let mut geo = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 10.0)]);
        geo.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });

        let refs: Vec<&dyn NodeData> = vec![&geo];
        let out = proc.process(&ctx(), &refs).unwrap();
        let result = out.downcast_ref::<Geometry>().unwrap();
        assert_eq!(result.point_count(), 2);
    }

    #[test]
    fn shape_source_without_input_returns_empty() {
        let node = Node::new(NodeId::new(1), "comp.source.shape")
            .with_output("output", DataTypeId::GEOMETRY);
        let proc = CompSourceProcessor::from_node(&node);
        let out = proc.process(&ctx(), &[]).unwrap();
        let result = out.downcast_ref::<Geometry>().unwrap();
        assert_eq!(result.point_count(), 0);
    }
}
