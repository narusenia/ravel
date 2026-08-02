// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Network interface node processors: `net.in` and `net.out`
//! (REQ-LAYER-002).
//!
//! `net.in` injects shell-provided values into the network: the layer's base
//! quad geometry, the layer-local time and frame index, the composited lower
//! stack (adjustment layers), and the layer's custom parameters. `net.out`
//! collects the network's results (`frame` plus custom ports) into a
//! [`PortRecord`] in input-port order.

use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams, ResolvedValue};
use ravel_core::geometry::{Geometry, Primitive};
use ravel_core::graph::Node;
use ravel_core::id::DataTypeId;
use ravel_core::network as net;
use ravel_core::types::{
    Color, FrameBuffer, NodeData, PlainText, PortRecord, Scalar, Vec2, Vec3, Vec4,
};
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
                net::PORT_BASE_GEOMETRY => Arc::new(base_quad(ctx.comp_resolution)),
                net::PORT_TIME => Arc::new(Scalar(ctx.time as f32)),
                // A legacy user-defined custom port that claims the builtin
                // name keeps its custom-parameter semantics (the load-time
                // normalization also leaves it untouched).
                net::PORT_FRAME_INDEX
                    if node
                        .parameters
                        .iter()
                        .all(|p| p.key != net::PORT_FRAME_INDEX) =>
                {
                    Arc::new(Scalar(ctx.frame as f32))
                }
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
                    .unwrap_or_else(|| custom_param_value(name, port.data_type, params, ctx)),
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

/// The layer's base quad: a closed path covering the composition coordinate space.
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
///
/// The typed zero is [`zero_value`] for the **port's** type, not `Scalar(0.0)`.
/// A custom port whose type has no `ParameterValue` counterpart — a subnet's
/// inner In declaring a `GEOMETRY` pin (REQ-LAYER-003) — has no parameter to
/// fall back on by construction, and answering it with a scalar sent a value
/// of the wrong type to a downstream node that had declared what it accepts.
pub(crate) fn custom_param_value(
    name: &str,
    data_type: DataTypeId,
    params: &ResolvedParams,
    ctx: &EvalContext,
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
        // No parameter of that name, or one whose kind carries no wire value
        // (`Str`, `PathPoints`, `Curve`): the port's own typed zero answers.
        _ => zero_value(Some(&data_type), ctx),
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
        Some(&DataTypeId::PLAIN_TEXT) => Arc::new(PlainText(String::new())),
        // `FIELD` has no zero: a field is a sampler, and this crate has no
        // constant one to hand out. It falls through to the scalar zero, which
        // is the wrong type — a custom `Field` port left unconnected still
        // misreports, and needs a `ConstantField` in `ravel-core` to fix.
        _ => Arc::new(Scalar(0.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::RectProcessor;
    use ravel_core::eval::Evaluator;
    use ravel_core::geometry::names;
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::FrameRate;

    /// `f` yields the layer-local frame index of the evaluation context.
    #[test]
    fn net_in_f_outputs_the_frame_index() {
        let in_node = Node::new(NodeId::new(1), net::NET_IN_TYPE_KEY)
            .with_output(net::PORT_FRAME_INDEX, DataTypeId::SCALAR);
        let g = Graph::new().add_node(in_node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(NetInProcessor));

        let fps = FrameRate::new(30, 1);
        for frame in [0u64, 30, 123] {
            let ctx = EvalContext::new(frame, fps, (64, 64));
            let out = ev.evaluate(&g, NodeId::new(1), &ctx).unwrap();
            let v = out.downcast_ref::<Scalar>().unwrap().0;
            assert_eq!(v, frame as f32, "frame {frame}");
        }
    }

    /// A legacy user-defined custom port named `f` (it carries a same-named
    /// parameter) keeps its custom-parameter semantics instead of being
    /// hijacked by the builtin frame index.
    #[test]
    fn legacy_custom_f_port_keeps_parameter_semantics() {
        let in_node = Node::new(NodeId::new(1), net::NET_IN_TYPE_KEY)
            .with_output(net::PORT_FRAME_INDEX, DataTypeId::SCALAR)
            .with_param(net::PORT_FRAME_INDEX, ParameterValue::Float(7.5));
        let g = Graph::new().add_node(in_node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(NetInProcessor));

        let ctx = EvalContext::new(30, FrameRate::new(30, 1), (64, 64));
        let out = ev.evaluate(&g, NodeId::new(1), &ctx).unwrap();
        let v = out.downcast_ref::<Scalar>().unwrap().0;
        assert_eq!(v, 7.5, "custom parameter wins over the frame index");
    }

    /// Regression: an unconnected custom port answers with **its own** typed
    /// zero. A `GEOMETRY` port has no `ParameterValue` counterpart to fall
    /// back on, and the scalar zero it used to return was a value of the wrong
    /// type on a wire that had declared what it carries.
    #[test]
    fn unconnected_custom_port_yields_the_ports_typed_zero() {
        let in_node = Node::new(NodeId::new(1), net::NET_IN_TYPE_KEY)
            .with_output("shape", DataTypeId::GEOMETRY);
        let g = Graph::new().add_node(in_node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(NetInProcessor));

        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (64, 64));
        let out = ev.evaluate(&g, NodeId::new(1), &ctx).unwrap();
        let geo = out
            .downcast_ref::<Geometry>()
            .expect("a GEOMETRY port must not answer with a Scalar");
        assert_eq!(geo.points().element_count(), 0, "an empty geometry");

        // The same holds for the other wire types the fallback now covers.
        let ports = [
            (DataTypeId::COLOR, "tint"),
            (DataTypeId::VEC2, "offset"),
            (DataTypeId::FRAME_BUFFER, "plate"),
        ];
        for (data_type, name) in ports {
            let node = Node::new(NodeId::new(2), net::NET_IN_TYPE_KEY).with_output(name, data_type);
            let g = Graph::new().add_node(node).unwrap();
            let mut ev = Evaluator::new();
            ev.register(NodeId::new(2), Arc::new(NetInProcessor));
            let out = ev.evaluate(&g, NodeId::new(2), &ctx).unwrap();
            assert_eq!(out.data_type_id(), data_type, "port {name}");
        }
    }

    /// A SCALAR custom port without a parameter is unchanged by the typed
    /// fallback: the scalar zero was already the right answer.
    #[test]
    fn unconnected_scalar_port_still_yields_zero() {
        let in_node = Node::new(NodeId::new(1), net::NET_IN_TYPE_KEY)
            .with_output("amount", DataTypeId::SCALAR);
        let g = Graph::new().add_node(in_node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(NetInProcessor));

        let ctx = EvalContext::new(0, FrameRate::new(30, 1), (64, 64));
        let out = ev.evaluate(&g, NodeId::new(1), &ctx).unwrap();
        assert_eq!(out.downcast_ref::<Scalar>().unwrap().0, 0.0);
    }

    /// Regression: `net.in`'s `t` output drives an exposed parameter and
    /// advances with the frame (time-dependent freshness must reach the
    /// consumer through the parameter-port edge).
    #[test]
    fn net_in_t_drives_exposed_param_across_frames() {
        let in_node = Node::new(NodeId::new(1), net::NET_IN_TYPE_KEY)
            .with_output(net::PORT_BASE_GEOMETRY, DataTypeId::GEOMETRY)
            .with_output(net::PORT_TIME, DataTypeId::SCALAR);
        let rect = Node::new(NodeId::new(2), "shape.rect")
            .with_output("output", DataTypeId::GEOMETRY)
            .with_param("center", ParameterValue::vec2(32.0, 32.0))
            .with_param("width", ParameterValue::Float(4.0))
            .with_param("height", ParameterValue::Float(4.0));
        let g = Graph::new()
            .add_node(in_node)
            .unwrap()
            .add_node(rect)
            .unwrap()
            .expose_param_port(NodeId::new(2), "width")
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(1),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();

        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(NetInProcessor));
        ev.register(NodeId::new(2), Arc::new(RectProcessor));

        let fps = FrameRate::new(30, 1);
        let width_at = |ev: &mut Evaluator, frame: u64| -> f32 {
            let ctx = EvalContext::new(frame, fps, (64, 64));
            let out = ev.evaluate(&g, NodeId::new(2), &ctx).unwrap();
            let geo = out.downcast_ref::<Geometry>().unwrap();
            let xs: Vec<f32> = geo
                .points()
                .get(names::P)
                .unwrap()
                .as_vec2(names::P)
                .unwrap()
                .iter()
                .map(|p| p.0)
                .collect();
            xs.iter().copied().fold(f32::NEG_INFINITY, f32::max)
                - xs.iter().copied().fold(f32::INFINITY, f32::min)
        };
        let w0 = width_at(&mut ev, 0);
        let w30 = width_at(&mut ev, 30);
        assert!(
            (w0 - 0.0).abs() < 1e-4,
            "t=0s at frame 0 → width 0, got {w0}"
        );
        assert!(
            (w30 - 1.0).abs() < 1e-4,
            "t=1s at frame 30 → width 1, got {w30}"
        );
    }
}
