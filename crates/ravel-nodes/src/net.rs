// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Network interface node processors: `net.in` and `net.out`
//! (REQ-LAYER-002).
//!
//! `net.in` injects shell-provided values into the network: the layer's base
//! quad geometry, the layer-local time, the composited lower stack
//! (adjustment layers), and the layer's custom parameters. `net.out`
//! collects the network's results (`frame` plus custom ports) into a
//! [`PortRecord`] in input-port order.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams, ResolvedValue};
use ravel_core::geometry::{Geometry, Primitive};
use ravel_core::graph::Node;
use ravel_core::id::DataTypeId;
use ravel_core::network as net;
use ravel_core::types::{Color, FrameBuffer, NodeData, PortRecord, Scalar, Vec2, Vec3, Vec4};
use std::sync::Arc;

// ===========================================================================
// net.in
// ===========================================================================

/// Produces the In node's [`PortRecord`]: one value per declared output
/// port, in port order.
pub struct NetInProcessor;

impl NetInProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for NetInProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let mut record: Vec<Arc<dyn NodeData>> = Vec::with_capacity(node.outputs.len());
        for port in &node.outputs {
            let value: Arc<dyn NodeData> = match port.name.as_str() {
                net::PORT_BASE_GEOMETRY => Arc::new(base_quad(ctx.resolution)),
                net::PORT_TIME => Arc::new(Scalar(ctx.time as f32)),
                net::PORT_SOURCE => scope
                    .bindings()
                    .iter()
                    .find(|(name, _)| name == net::PORT_SOURCE)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| transparent(ctx)),
                // Custom ports: a caller-provided binding (a subnet's
                // connected outer pin, REQ-LAYER-003) wins over the In
                // node's own parameter default.
                name => scope
                    .bindings()
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| custom_param_value(name, port.data_type, params)),
            };
            record.push(value);
        }
        // Single-output convention: edges extract a lone output directly
        // (PortRecord::extract), so wrap only genuine multi-output nodes.
        if record.len() == 1 {
            return Ok(record.pop().expect("one entry"));
        }
        Ok(Arc::new(PortRecord(record)))
    }

    fn is_time_dependent(&self) -> bool {
        // `t` and keyframed custom parameters vary per frame.
        true
    }
}

// ===========================================================================
// net.out
// ===========================================================================

/// Collects the Out node's inputs into a [`PortRecord`] in input-port order.
/// Unconnected ports yield a typed zero placeholder.
pub struct NetOutProcessor;

impl NetOutProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for NetOutProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let record: Vec<Arc<dyn NodeData>> = node
            .inputs
            .iter()
            .enumerate()
            .map(|(i, port)| {
                inputs
                    .get(i)
                    .and_then(|v| v.clone())
                    .unwrap_or_else(|| zero_value(port.accepted_types.first(), ctx))
            })
            .collect();
        Ok(Arc::new(PortRecord(record)))
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

/// The layer's base quad: a closed path covering the evaluation resolution.
fn base_quad(resolution: (u32, u32)) -> Geometry {
    let (w, h) = (resolution.0 as f32, resolution.1 as f32);
    let mut geo =
        Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(w, 0.0), Vec2(w, h), Vec2(0.0, h)]);
    geo.push_primitive(Primitive::Path {
        verts: 0..4,
        closed: true,
    });
    geo
}

fn transparent(ctx: &EvalContext) -> Arc<dyn NodeData> {
    Arc::new(FrameBuffer::new_zeroed(ctx.resolution.0, ctx.resolution.1))
}

/// Value of a custom parameter port: the resolved parameter matching the
/// port name, converted to the port's data type. Unset parameters yield a
/// typed zero.
pub(crate) fn custom_param_value(
    name: &str,
    data_type: DataTypeId,
    params: &ResolvedParams,
) -> Arc<dyn NodeData> {
    match params.get(name) {
        Some(ResolvedValue::Float(v)) => Arc::new(Scalar(*v)),
        Some(ResolvedValue::Int(v)) => Arc::new(Scalar(*v as f32)),
        Some(ResolvedValue::Bool(v)) => Arc::new(Scalar(if *v { 1.0 } else { 0.0 })),
        Some(ResolvedValue::Vec2(v)) => Arc::new(Vec2(v[0], v[1])),
        Some(ResolvedValue::Vec3(v)) if data_type == DataTypeId::COLOR => {
            Arc::new(Color::new(v[0], v[1], v[2], 1.0))
        }
        Some(ResolvedValue::Vec3(v)) => Arc::new(Vec3(v[0], v[1], v[2])),
        Some(ResolvedValue::Vec4(v)) if data_type == DataTypeId::COLOR => {
            Arc::new(Color::new(v[0], v[1], v[2], v[3]))
        }
        Some(ResolvedValue::Vec4(v)) => Arc::new(Vec4(v[0], v[1], v[2], v[3])),
        // Custom parameters are scalar/vector/color only (REQ-LAYER-002).
        _ => Arc::new(Scalar(0.0)),
    }
}

/// Typed zero value for an unconnected port.
pub(crate) fn zero_value(data_type: Option<&DataTypeId>, ctx: &EvalContext) -> Arc<dyn NodeData> {
    match data_type {
        Some(&DataTypeId::FRAME_BUFFER) => transparent(ctx),
        Some(&DataTypeId::GEOMETRY) => Arc::new(Geometry::new()),
        Some(&DataTypeId::VEC2) => Arc::new(Vec2(0.0, 0.0)),
        Some(&DataTypeId::VEC3) => Arc::new(Vec3(0.0, 0.0, 0.0)),
        Some(&DataTypeId::VEC4) => Arc::new(Vec4(0.0, 0.0, 0.0, 0.0)),
        Some(&DataTypeId::COLOR) => Arc::new(Color::TRANSPARENT),
        _ => Arc::new(Scalar(0.0)),
    }
}
