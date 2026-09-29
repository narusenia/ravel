// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `layer.info` — reads a layer's shell from the document (REQ-LAYER-002/005).
//!
//! The counterpart of [`layer_ref`](crate::layer_ref): that node reads a
//! layer's network *output*, this one reads the shell around it — the time
//! placement, the transform, the opacity, the layer's place in the stack.
//! `layer = -1` (the template default) is the layer whose network this node
//! sits in; anything else is the decimal `LayerId` of a layer in the same
//! composition.
//!
//! **This node evaluates nothing.** It reads `Document` fields, so it can
//! name its own layer without forming a cycle, and no graph-level cycle
//! detection applies to it. The price is that the change it reads carries no
//! edge: a shell edit reaches it through
//! [`InvalidationHint::Shell`](ravel_core::runtime::InvalidationHint::Shell)
//! and the `SHELL_READER_TYPE_KEYS` walk in `ravel_core::eval`, not through
//! the graph.
//!
//! Which ports a node carries is a user choice from the template's candidate
//! set (`registry::builtin`'s `layer_info_port_options`); the values here are
//! keyed by the same names, and an unknown one is an error rather than a
//! typed zero so the two lists cannot quietly drift apart.

use ravel_core::composition::transform::world_matrix;
use ravel_core::composition::{Composition, Layer};
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, PathSegment, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::id::LayerId;
use ravel_core::types::{NodeData, PlainText, PortRecord, Scalar, Vec2};
use std::sync::Arc;

use crate::net::zero_value;

pub struct LayerInfoProcessor;

impl LayerInfoProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for LayerInfoProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        // The enclosing layer scope is both "which composition" and, for the
        // `-1` default, "which layer".
        let Some(&PathSegment::Layer(comp_id, source_id)) = scope
            .path()
            .iter()
            .rev()
            .find(|s| matches!(s, PathSegment::Layer(..)))
        else {
            anyhow::bail!("layer.info: not evaluated inside a layer network");
        };

        let document = scope
            .document()
            .ok_or_else(|| anyhow::anyhow!("layer.info: no document set on the evaluator"))?;
        let comp = document
            .get_composition(comp_id)
            .ok_or_else(|| anyhow::anyhow!("layer.info: composition {comp_id:?} missing"))?;
        let source = comp
            .get_layer(source_id)
            .ok_or_else(|| anyhow::anyhow!("layer.info: layer {source_id:?} missing"))?;

        let target_id = target_layer(params.str_or("layer", SELF_TARGET), source_id)?;
        let target = comp.get_layer(target_id).ok_or_else(|| {
            anyhow::anyhow!("layer.info: target layer {target_id:?} not in composition {comp_id:?}")
        })?;

        // `ctx` is the **source** layer's local time, and two different
        // conversions come out of it.
        //
        // The target's own local frame is what every shell channel is
        // sampled at (REQ-LAYER-006), and `None` outside the target's
        // display interval — every port then answers with its typed zero,
        // exactly as `layer.ref` does.
        let local_frame = source.retimed_local_frame(target, ctx.frame);
        // The **composition** frame is what `world_matrix` wants: it reads
        // `ctx.sample_frame()` as composition time and derives each layer's
        // and each ancestor's own local frame from it. Handing it the
        // source's local time would sample the target and its whole parent
        // chain at the wrong moment whenever the source is placed off zero —
        // and the local ports, which pass the frame explicitly, would be
        // right while the world ones were not.
        let sub_frame = ctx.sample_frame() - ctx.frame as f64;
        let comp_sample = source.comp_frame(ctx.frame as i64) as f64 + sub_frame;
        let reader = ShellReader {
            comp,
            target,
            local_frame,
            ctx: EvalContext {
                // `frame` and `time` are one instant in two forms and
                // `sample_frame()` is what reads them back; the unsigned
                // frame index is clamped only because a composition frame
                // can be negative, and that subtraction cancels out.
                frame: comp_sample.max(0.0) as u64,
                time: comp_sample / comp.frame_rate.as_f64(),
                fps: comp.frame_rate,
                // Composition space, not canvas space: the local transform
                // ports read the channels raw, so the world ones have to be
                // in the same units or the two disagree under a proxy
                // resolution.
                resolution: comp.resolution,
                comp_resolution: comp.resolution,
                ..*ctx
            },
        };

        let mut record: Vec<Arc<dyn NodeData>> = Vec::with_capacity(node.outputs.len());
        for port in &node.outputs {
            record.push(match local_frame {
                Some(_) => reader.value(&port.name)?,
                None => zero_value(Some(&port.data_type), ctx),
            });
        }
        // Single-output convention: an edge extracts a lone output directly.
        if record.len() == 1 {
            return Ok(record.pop().expect("one entry"));
        }
        Ok(Arc::new(PortRecord(record)))
    }

    fn is_time_dependent(&self) -> bool {
        // `local_t` / `local_f` move with the frame, and so does every
        // transform port once the shell is keyframed.
        true
    }
}

/// The `layer` parameter value that means "the layer this network belongs
/// to". A negative number, so it can never collide with a `LayerId`.
const SELF_TARGET: &str = "-1";

/// The layer a `layer` parameter value names, given the layer the node sits
/// in.
///
/// `""` and any other unparsable spelling are refused rather than folded into
/// `-1`: the default is written into every node the template creates, so a
/// value that is not it is a value something wrote, and guessing "itself"
/// would read a different layer than the document says.
fn target_layer(value: &str, source: LayerId) -> anyhow::Result<LayerId> {
    match value.parse::<i64>() {
        Ok(-1) => Ok(source),
        // Zero is the reserved "no layer" id `identifier_overlay` writes over
        // a target that does not stand still (a wire, a keyframed target).
        Ok(raw) if raw > 0 => Ok(LayerId::new(raw as u64)),
        _ => anyhow::bail!("layer.info: no target layer set ({value:?})"),
    }
}

/// One shell, read port by port.
struct ShellReader<'a> {
    comp: &'a Composition,
    target: &'a Layer,
    /// The target's local frame, `None` outside its display interval.
    local_frame: Option<i64>,
    /// Composition-space context at **composition** time.
    ///
    /// Not the source network's context: this one is what `world_matrix`
    /// reads its frame from, and the channel reads below pass
    /// [`local_frame`](Self::local_frame) explicitly, so they are unaffected
    /// by the frame it carries.
    ctx: EvalContext,
}

impl ShellReader<'_> {
    fn value(&self, port: &str) -> anyhow::Result<Arc<dyn NodeData>> {
        let layer = self.target;
        let lf = self.local_frame.unwrap_or(0) as f64;
        let t = &layer.transform;
        let scalar = |v: f32| -> Arc<dyn NodeData> { Arc::new(Scalar(v)) };
        let channels = |c: &[ravel_core::animation::channel::AnimationChannel; 2]| {
            Arc::new(Vec2(
                c[0].evaluate(lf, &self.ctx),
                c[1].evaluate(lf, &self.ctx),
            )) as Arc<dyn NodeData>
        };

        Ok(match port {
            "name" => Arc::new(PlainText(layer.name.clone())),
            // The Timeline row, which is the only layer number the user ever
            // reads (`composition::timeline_row` owns the direction).
            "index" => scalar(self.comp.timeline_row_of(layer.id).unwrap_or(0) as f32),
            "start_frame" => scalar(layer.start_frame as f32),
            "in_frame" => scalar(layer.in_frame as f32),
            "out_frame" => scalar(layer.out_frame as f32),
            "duration" => scalar(layer.duration() as f32),
            // The layer's frame of reference is the composition's: a layer
            // has no size of its own until its network draws one.
            "size" => Arc::new(Vec2(
                self.comp.resolution.0 as f32,
                self.comp.resolution.1 as f32,
            )),
            "local_t" => scalar((lf / self.comp.frame_rate.as_f64()) as f32),
            "local_f" => scalar(lf as f32),
            "position" => channels(&t.position),
            "scale" => channels(&t.scale),
            "anchor" => channels(&t.anchor_point),
            // Radians, as the plan specifies, while the shell stores and
            // edits degrees: a network feeds these to trigonometry.
            "rotation" => scalar(t.rotation.evaluate(lf, &self.ctx).to_radians()),
            "opacity" => scalar(layer.opacity.evaluate(lf, &self.ctx)),
            "world_position" | "world_scale" | "world_rotation" => self.world(port),
            other => anyhow::bail!("layer.info: no such output port {other:?}"),
        })
    }

    /// The parent chain folded in, decomposed out of the **same**
    /// `world_matrix` the renderer and the viewer's overlay compose
    /// (`composition::transform`). Recomputing the chain here would give the
    /// network a third answer to a question that already has one.
    fn world(&self, port: &str) -> Arc<dyn NodeData> {
        let m = world_matrix(self.comp, self.target, &self.ctx).0;
        match port {
            // Where the layer's anchor lands. `world_matrix` maps the
            // layer's own space, and the anchor is the point its `position`
            // names, so this is `position` itself when nothing is parented.
            "world_position" => {
                let lf = self.local_frame.unwrap_or(0) as f64;
                let t = &self.target.transform;
                let (ax, ay) = (
                    t.anchor_point[0].evaluate(lf, &self.ctx),
                    t.anchor_point[1].evaluate(lf, &self.ctx),
                );
                let (x, y) = ravel_core::composition::transform::Affine(m).apply(ax, ay);
                Arc::new(Vec2(x, y))
            }
            // The linear part's column lengths and the first column's angle:
            // a layer matrix is `R(θ)·S`, so column 0 is `(cosθ·sx, sinθ·sx)`
            // and column 1 is `(-sinθ·sy, cosθ·sy)`.
            "world_scale" => Arc::new(Vec2(m[0].hypot(m[3]), m[1].hypot(m[4]))),
            _ => Arc::new(Scalar(m[3].atan2(m[0]))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::animation::channel::AnimationChannel;
    use ravel_core::composition::{Document, Layer};
    use ravel_core::eval::{Evaluator, PathSegment};
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::{CompId, DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::registry::{NodeRegistry, builtin};
    use ravel_core::runtime::ShellScope;
    use ravel_core::types::{Color, FrameRate};

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };
    const RES: (u32, u32) = (200, 100);

    fn comp_id() -> CompId {
        CompId::new(1)
    }

    fn info_node(id: u64, target: &str, ports: &[&str]) -> Node {
        let mut reg = NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        let options: Vec<_> = reg.output_options("layer.info").to_vec();
        let mut node = Node::new(NodeId::new(id), "layer.info")
            .with_param("layer", ParameterValue::String(target.into()));
        node.outputs = ports
            .iter()
            .map(|name| {
                options
                    .iter()
                    .find(|p| p.name == *name)
                    .unwrap_or_else(|| panic!("{name} is not a declared candidate"))
                    .clone()
            })
            .collect();
        node
    }

    fn layer(id: u64, name: &str) -> Layer {
        Layer::new(LayerId::new(id), name, Graph::new()).with_time(0, 0, 300)
    }

    fn comp_of(layers: Vec<Layer>) -> Composition {
        let mut comp = Composition::new(comp_id(), "Info", RES, FPS, 300);
        for l in layers {
            comp.layers.push_back(l);
        }
        comp
    }

    /// `comp` with `owner`'s network replaced by `graph` — the node has to be
    /// *in* the layer it claims to belong to, or the shell-reader walk
    /// (which scans layer networks) cannot find it.
    fn with_network(comp: &Composition, owner: LayerId, graph: &Graph) -> Composition {
        let mut comp = comp.clone();
        let index = comp.layers.iter().position(|l| l.id == owner).unwrap();
        let mut layer = comp.layers[index].clone();
        layer.network = graph.clone();
        comp.layers.set(index, layer);
        comp
    }

    /// Evaluate `node` (already carrying its ports) inside `owner`'s scope.
    fn eval_at(
        comp: &Composition,
        owner: LayerId,
        node: Node,
        frame: u64,
    ) -> Result<Arc<dyn NodeData>, ravel_core::eval::EvalError> {
        let id = node.id;
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(id, Arc::new(LayerInfoProcessor));
        ev.set_document(Arc::new(
            Document::default().with_composition(with_network(comp, owner, &graph)),
        ));
        ev.evaluate_at(
            &[PathSegment::Layer(comp_id(), owner)],
            &graph,
            id,
            &EvalContext::new(frame, FPS, RES),
        )
    }

    /// The whole error chain, since the evaluator wraps a processor's
    /// complaint in its own "processing failed for node …".
    fn error_chain(err: &ravel_core::eval::EvalError) -> String {
        let mut text = err.to_string();
        let mut source = std::error::Error::source(err);
        while let Some(cause) = source {
            text.push_str(&format!(": {cause}"));
            source = cause.source();
        }
        text
    }

    fn scalar_of(value: &Arc<dyn NodeData>) -> f32 {
        value.downcast_ref::<Scalar>().expect("a scalar port").0
    }

    fn vec2_of(value: &Arc<dyn NodeData>) -> (f32, f32) {
        let v = value.downcast_ref::<Vec2>().expect("a vec2 port");
        (v.0, v.1)
    }

    fn record(value: &Arc<dyn NodeData>) -> Vec<Arc<dyn NodeData>> {
        value
            .downcast_ref::<PortRecord>()
            .expect("a record")
            .0
            .clone()
    }

    /// Every candidate the template declares has a value here, of the type
    /// the candidate declares. The two lists are written separately — names
    /// and types in the registry, values in this file — so this is what stops
    /// them drifting: an unknown port is an error, not a silent zero.
    #[test]
    fn every_declared_candidate_port_answers_with_its_declared_type() {
        let mut reg = NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        let options = reg.output_options("layer.info").to_vec();
        assert!(!options.is_empty(), "layer.info declares candidates");

        let names: Vec<&str> = options.iter().map(|p| p.name.as_str()).collect();
        let comp = comp_of(vec![layer(1, "Only")]);
        let out = eval_at(&comp, LayerId::new(1), info_node(10, "-1", &names), 0).unwrap();
        for (value, port) in record(&out).iter().zip(&options) {
            assert_eq!(
                value.data_type_id(),
                port.data_type,
                "port {} answered with the wrong type",
                port.name
            );
        }
    }

    /// `-1` is the layer the network belongs to; a decimal id is any layer of
    /// the same composition.
    #[test]
    fn self_and_sibling_targets_resolve() {
        let comp = comp_of(vec![layer(1, "Bottom"), layer(2, "Top")]);
        let name = |target: &str, owner: u64| {
            let out = eval_at(
                &comp,
                LayerId::new(owner),
                info_node(10, target, &["name"]),
                0,
            )
            .unwrap();
            out.downcast_ref::<PlainText>().unwrap().0.clone()
        };
        assert_eq!(name("-1", 2), "Top", "-1 is the owning layer");
        assert_eq!(name("1", 2), "Bottom", "an id is that layer");
        assert_eq!(name("2", 1), "Top", "read in the other direction too");
    }

    /// The `index` port is the Timeline row, which runs top-down while
    /// `comp.layers` runs bottom-up.
    #[test]
    fn index_is_the_timeline_row() {
        let comp = comp_of(vec![
            layer(1, "Bottom"),
            layer(2, "Middle"),
            layer(3, "Top"),
        ]);
        let row = |owner: u64| {
            let out = eval_at(
                &comp,
                LayerId::new(owner),
                info_node(10, "-1", &["index"]),
                0,
            )
            .unwrap();
            scalar_of(&out)
        };
        assert_eq!(row(3), 1.0, "the topmost layer is row 1");
        assert_eq!(row(2), 2.0);
        assert_eq!(row(1), 3.0);
    }

    /// The target's shell is read at the target's own local time: the
    /// reader's local frame goes back out to composition time and into the
    /// target's (REQ-LAYER-006).
    ///
    /// **Both** layers are placed off zero, so neither half of the round trip
    /// can be dropped or sign-flipped without moving the answer — with the
    /// source at zero the outbound half contributes nothing and a reversed
    /// sign passes unnoticed.
    #[test]
    fn local_time_is_the_targets_own() {
        let comp = comp_of(vec![
            layer(1, "Source").with_time(5, 0, 300),
            layer(2, "Target").with_time(10, 0, 300),
        ]);
        // Source-local 20 → comp 25 → target-local 15.
        let out = eval_at(
            &comp,
            LayerId::new(1),
            info_node(10, "2", &["local_f", "local_t"]),
            20,
        )
        .unwrap();
        let values = record(&out);
        assert_eq!(
            scalar_of(&values[0]),
            15.0,
            "source-local 20 → target-local 15"
        );
        assert!((scalar_of(&values[1]) - 0.5).abs() < 1e-6);

        // Read the other way: the target's own local frame is its own time.
        let own = eval_at(
            &comp,
            LayerId::new(2),
            info_node(11, "-1", &["local_f"]),
            15,
        )
        .unwrap();
        assert_eq!(scalar_of(&own), 15.0, "-1 round-trips to itself");
    }

    /// Outside the target's display interval every port answers with its
    /// typed zero, the same contract `layer.ref` keeps.
    #[test]
    fn a_target_outside_its_interval_yields_typed_zeros() {
        let mut target = layer(2, "Target").with_time(100, 0, 300);
        target.transform.position[0] = AnimationChannel::constant(40.0);
        let comp = comp_of(vec![layer(1, "Source"), target]);
        let ports = ["name", "index", "position"];

        // Inside: the real values.
        let inside =
            record(&eval_at(&comp, LayerId::new(1), info_node(10, "2", &ports), 100).unwrap());
        assert_eq!(inside[0].downcast_ref::<PlainText>().unwrap().0, "Target");
        assert_eq!(scalar_of(&inside[1]), 1.0);
        assert_eq!(vec2_of(&inside[2]), (40.0, 0.0));

        // Before the target starts: zeros, still typed.
        let outside =
            record(&eval_at(&comp, LayerId::new(1), info_node(11, "2", &ports), 99).unwrap());
        assert_eq!(outside[0].downcast_ref::<PlainText>().unwrap().0, "");
        assert_eq!(scalar_of(&outside[1]), 0.0);
        assert_eq!(vec2_of(&outside[2]), (0.0, 0.0));
        let types = [DataTypeId::PLAIN_TEXT, DataTypeId::SCALAR, DataTypeId::VEC2];
        for ((value, port), data_type) in outside.iter().zip(ports).zip(types) {
            assert_eq!(value.data_type_id(), data_type, "{port} kept its type");
        }
    }

    /// Local and world differ under parenting, and world is the decomposition
    /// of the very matrix the renderer composes.
    #[test]
    fn world_ports_match_the_composed_matrix() {
        let mut parent = layer(1, "Parent");
        parent.transform.position = [
            AnimationChannel::constant(10.0),
            AnimationChannel::constant(0.0),
        ];
        parent.transform.rotation = AnimationChannel::constant(90.0);
        parent.transform.scale = [
            AnimationChannel::constant(2.0),
            AnimationChannel::constant(2.0),
        ];
        let mut child = layer(2, "Child").with_parent(LayerId::new(1));
        child.transform.position = [
            AnimationChannel::constant(5.0),
            AnimationChannel::constant(0.0),
        ];
        let comp = comp_of(vec![parent, child]);

        let out = eval_at(
            &comp,
            LayerId::new(2),
            info_node(
                10,
                "-1",
                &[
                    "position",
                    "scale",
                    "rotation",
                    "world_position",
                    "world_scale",
                    "world_rotation",
                ],
            ),
            0,
        )
        .unwrap();
        let v = record(&out);

        assert_eq!(
            vec2_of(&v[0]),
            (5.0, 0.0),
            "local position is the raw channel"
        );
        assert_eq!(vec2_of(&v[1]), (1.0, 1.0));
        assert_eq!(scalar_of(&v[2]), 0.0);

        // The authority: the same matrix the renderer composes.
        let target = comp.get_layer(LayerId::new(2)).unwrap();
        let m = world_matrix(&comp, target, &EvalContext::new(0, FPS, RES));
        let expected = m.apply(0.0, 0.0);
        let world = vec2_of(&v[3]);
        assert!(
            (world.0 - expected.0).abs() < 1e-4 && (world.1 - expected.1).abs() < 1e-4,
            "world_position {world:?} vs the composed matrix {expected:?}"
        );
        assert_ne!(world, vec2_of(&v[0]), "parenting must move it");

        let world_scale = vec2_of(&v[4]);
        assert!(
            (world_scale.0 - 2.0).abs() < 1e-4 && (world_scale.1 - 2.0).abs() < 1e-4,
            "the parent's scale reaches the child: {world_scale:?}"
        );
        assert!(
            (scalar_of(&v[5]) - std::f32::consts::FRAC_PI_2).abs() < 1e-4,
            "the parent's 90° reaches the child, in radians"
        );
    }

    /// The world ports are sampled at **composition** time, not at the
    /// reading network's local time.
    ///
    /// `world_matrix` takes `ctx.sample_frame()` as the composition frame and
    /// derives each layer's and each ancestor's own local frame from it, so a
    /// reader inside a layer network has to convert back out first. The
    /// source therefore carries a non-zero `start_frame` **and** `in_frame`
    /// (both terms of the conversion matter) and the parent is animated, so
    /// handing `world_matrix` the source's own frame samples the chain at the
    /// wrong moment and reads a different number.
    #[test]
    fn world_ports_are_sampled_at_composition_time() {
        // Parent x ramps 0 → 100 over comp frames 0..40.
        let mut parent = layer(1, "Parent");
        let mut curve = ravel_core::animation::curve::KeyframeCurve::new();
        curve.insert(
            0,
            0.0,
            ravel_core::animation::interpolation::Interpolation::Linear,
        );
        curve.insert(
            40,
            100.0,
            ravel_core::animation::interpolation::Interpolation::Linear,
        );
        parent.transform.position[0] = AnimationChannel::keyframes(curve);

        let target = layer(2, "Target").with_parent(LayerId::new(1));
        // Source-local 5 → comp 5 + 20 - 5 = 20 → parent-local 20 → x = 50.
        let source = layer(3, "Source").with_time(20, 5, 300);
        let comp = comp_of(vec![parent, target, source]);

        let out = eval_at(
            &comp,
            LayerId::new(3),
            info_node(10, "2", &["world_position"]),
            5,
        )
        .unwrap();
        let world = vec2_of(&out);

        // The authority: the composed matrix at the COMPOSITION frame.
        let at_comp = EvalContext::new(20, FPS, RES);
        let expected =
            world_matrix(&comp, comp.get_layer(LayerId::new(2)).unwrap(), &at_comp).apply(0.0, 0.0);
        assert!(
            (world.0 - expected.0).abs() < 1e-4 && (world.1 - expected.1).abs() < 1e-4,
            "world_position {world:?} vs the matrix at comp frame 20 {expected:?}"
        );
        assert!(
            (world.0 - 50.0).abs() < 1e-4,
            "comp frame 20 is half the parent's ramp, got {}",
            world.0
        );
    }

    /// World values stay in composition space when the canvas is smaller (a
    /// proxy render), so they can be compared with the local ports.
    #[test]
    fn world_position_is_composition_space() {
        let mut only = layer(1, "Only");
        only.transform.position = [
            AnimationChannel::constant(40.0),
            AnimationChannel::constant(20.0),
        ];
        let comp = comp_of(vec![only]);
        let node = info_node(10, "-1", &["world_position"]);
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(10), Arc::new(LayerInfoProcessor));
        ev.set_document(Arc::new(
            Document::default().with_composition(with_network(&comp, LayerId::new(1), &graph)),
        ));

        let half = EvalContext::new(0, FPS, (RES.0 / 2, RES.1 / 2)).with_comp_resolution(RES);
        let out = ev
            .evaluate_at(
                &[PathSegment::Layer(comp_id(), LayerId::new(1))],
                &graph,
                NodeId::new(10),
                &half,
            )
            .unwrap();
        assert_eq!(vec2_of(&out), (40.0, 20.0), "not scaled to the canvas");
    }

    /// The error names the layer that was asked for — the id is the only part
    /// of the message that tells the user which node to go and fix.
    #[test]
    fn a_missing_target_is_reported_with_its_id() {
        let comp = comp_of(vec![layer(1, "Only")]);
        let message = match eval_at(&comp, LayerId::new(1), info_node(10, "77", &["name"]), 0) {
            Err(err) => error_chain(&err),
            Ok(_) => panic!("a missing target must not evaluate"),
        };
        assert!(
            message.contains("LayerId(77)") && message.contains("CompId(1)"),
            "message was {message}"
        );
    }

    /// A target nothing set is refused rather than silently read as `-1`.
    #[test]
    fn an_unset_target_is_refused() {
        assert!(target_layer("-1", LayerId::new(9)).is_ok());
        for spelling in ["", "0", "nonsense", "-2"] {
            assert!(
                target_layer(spelling, LayerId::new(9)).is_err(),
                "{spelling:?} must not resolve"
            );
        }
    }

    /// The shell edit reaches the reader. Nothing in the graph changed and
    /// the reader's own shell did not move, so `set_document`'s diff leaves
    /// its cache alone: only the `InvalidationHint::Shell` walk
    /// (`SHELL_READER_TYPE_KEYS`) drops it. Take `"layer.info"` out of that
    /// list and this test reads the old position.
    #[test]
    fn a_shell_edit_reaches_the_reader_through_the_shell_hint() {
        let mut target = layer(2, "Target");
        target.transform.position[0] = AnimationChannel::constant(10.0);
        let comp = comp_of(vec![layer(1, "Source"), target]);

        let node = info_node(10, "2", &["position"]);
        let graph = Graph::new().add_node(node).unwrap();
        let comp = with_network(&comp, LayerId::new(1), &graph);
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(10), Arc::new(LayerInfoProcessor));
        ev.set_document(Arc::new(Document::default().with_composition(comp.clone())));
        let path = [PathSegment::Layer(comp_id(), LayerId::new(1))];
        let ctx = EvalContext::new(0, FPS, RES);
        let read = |ev: &mut Evaluator| {
            vec2_of(
                &ev.evaluate_at(&path, &graph, NodeId::new(10), &ctx)
                    .unwrap(),
            )
        };
        assert_eq!(read(&mut ev), (10.0, 0.0));

        let mut moved = comp;
        let index = moved
            .layers
            .iter()
            .position(|l| l.id == LayerId::new(2))
            .unwrap();
        let mut layer = moved.layers[index].clone();
        layer.transform.position[0] = AnimationChannel::constant(90.0);
        moved.layers.set(index, layer);
        ev.set_document(Arc::new(Document::default().with_composition(moved)));
        ev.invalidate_shell_readers(&[ShellScope {
            comp: comp_id(),
            layer: Some(LayerId::new(2)),
        }]);

        assert_eq!(
            read(&mut ev),
            (90.0, 0.0),
            "the shell hint must reach a node with no edge to the shell"
        );
    }

    /// `layer.info(index) → color.ramp` gives each layer its own colour: the
    /// index port drives a real downstream node through a parameter port.
    #[test]
    fn index_drives_a_colour_ramp_per_layer() {
        let comp = comp_of(vec![layer(1, "A"), layer(2, "B"), layer(3, "C")]);
        let colour_of = |owner: u64| -> Color {
            let info = info_node(10, "-1", &["index"]);
            let ramp = Node::new(NodeId::new(11), "color.ramp")
                .with_output("output", DataTypeId::COLOR)
                .with_param("value", ParameterValue::Float(0.0))
                .with_param("in_min", ParameterValue::Float(1.0))
                .with_param("in_max", ParameterValue::Float(3.0));
            let graph = Graph::new()
                .add_node(info)
                .unwrap()
                .add_node(ramp)
                .unwrap()
                .expose_param_port(NodeId::new(11), "value")
                .unwrap()
                .add_edge(
                    EdgeId::new(1),
                    NodeId::new(10),
                    OutputPortIndex(0),
                    NodeId::new(11),
                    InputPortIndex(0),
                )
                .unwrap();
            let mut ev = Evaluator::new();
            ev.register(NodeId::new(10), Arc::new(LayerInfoProcessor));
            ev.register(NodeId::new(11), Arc::new(crate::color::ColorRampProcessor));
            ev.set_document(Arc::new(
                Document::default().with_composition(with_network(
                    &comp,
                    LayerId::new(owner),
                    &graph,
                )),
            ));
            let out = ev
                .evaluate_at(
                    &[PathSegment::Layer(comp_id(), LayerId::new(owner))],
                    &graph,
                    NodeId::new(11),
                    &EvalContext::new(0, FPS, RES),
                )
                .unwrap();
            *out.downcast_ref::<Color>().unwrap()
        };

        let (top, middle, bottom) = (colour_of(3), colour_of(2), colour_of(1));
        assert_ne!(top, middle, "row 1 and row 2 must differ");
        assert_ne!(middle, bottom, "row 2 and row 3 must differ");
    }
}
