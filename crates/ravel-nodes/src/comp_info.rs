// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.info` — reads a composition's own fields from the document
//! (REQ-LAYER-002/005).
//!
//! The composition-scale counterpart of [`layer_info`](crate::layer_info):
//! that node reads the shell around one layer, this one reads the frame the
//! whole stack sits in — resolution, frame rate, duration, background, layer
//! count. `comp = -1` (the template default) is the composition whose layer
//! network this node sits in; anything else is the decimal [`CompId`] of
//! another composition in the same document.
//!
//! **This node evaluates nothing**, so naming another composition costs none
//! of `precomp`'s machinery: no `PathSegment::Comp`, no cycle detection, no
//! recursion. The price is the same one `layer.info` pays — the change it
//! reads carries no edge, so it reaches the reader through
//! [`InvalidationHint::Shell`](ravel_core::runtime::InvalidationHint::Shell)
//! and the `SHELL_READER_TYPE_KEYS` walk in `ravel_core::eval` rather than
//! through the graph.
//!
//! ## Which clock `comp_t` and `comp_f` report
//!
//! `ctx` inside a layer network is that **layer's local** time, and the
//! layer's own local clock already has ports of its own on `layer.info`
//! (`local_t` / `local_f`). A port called `comp_t` that answered with the
//! same number would be a second spelling of one of those, so these two
//! convert back out to **composition** time first
//! ([`Layer::comp_frame`](ravel_core::composition::Layer::comp_frame)) — the
//! clock the Timeline's playhead shows, which is what someone asking "where
//! in the comp am I" means. That conversion is exactly what `layer.info`'s
//! `world_*` ports needed, and for the same reason: a reader placed off zero
//! otherwise reports a time that is nobody's.
//!
//! Across compositions the **seconds are the authority**, as the plan
//! specifies. `comp_t` is that instant in seconds on the current
//! composition's clock, whatever the target is, and `comp_f` is the same
//! instant divided by the **target's** frame rate. A `FrameRate` is rational
//! — 30000/1001 is not 30 — so converting a frame number into another
//! composition's frame number and back would not land where it started.

use ravel_core::composition::{Composition, Layer};
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, PathSegment, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::id::CompId;
use ravel_core::types::{NodeData, PlainText, PortRecord, Scalar, Vec2};
use std::sync::Arc;

pub struct CompInfoProcessor;

impl CompInfoProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for CompInfoProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        // The enclosing layer scope is both "which composition the `-1`
        // default means" and, through the owning layer's placement, the
        // conversion from this network's local time to composition time.
        let Some(&PathSegment::Layer(comp_id, source_id)) = scope
            .path()
            .iter()
            .rev()
            .find(|s| matches!(s, PathSegment::Layer(..)))
        else {
            anyhow::bail!("comp.info: not evaluated inside a layer network");
        };

        let document = scope
            .document()
            .ok_or_else(|| anyhow::anyhow!("comp.info: no document set on the evaluator"))?;
        let current = document
            .get_composition(comp_id)
            .ok_or_else(|| anyhow::anyhow!("comp.info: composition {comp_id:?} missing"))?;
        let source = current
            .get_layer(source_id)
            .ok_or_else(|| anyhow::anyhow!("comp.info: layer {source_id:?} missing"))?;

        let target_id = target_comp(params.str_or("comp", SELF_TARGET), comp_id)?;
        let target = document.get_composition(target_id).ok_or_else(|| {
            anyhow::anyhow!("comp.info: target composition {target_id:?} not in the document")
        })?;

        let reader = CompReader {
            target,
            comp_t: comp_seconds(source, current, ctx),
        };

        let mut record: Vec<Arc<dyn NodeData>> = Vec::with_capacity(node.outputs.len());
        for port in &node.outputs {
            record.push(reader.value(&port.name)?);
        }
        // Single-output convention: an edge extracts a lone output directly.
        if record.len() == 1 {
            return Ok(record.pop().expect("one entry"));
        }
        Ok(Arc::new(PortRecord(record)))
    }

    fn is_time_dependent(&self) -> bool {
        // `comp_t` / `comp_f` move with the frame. Nothing else here does,
        // but a node carrying either would serve a stale instant.
        true
    }
}

/// The `comp` parameter value that means "the composition this network
/// belongs to". A negative number, so it can never collide with a `CompId`.
const SELF_TARGET: &str = "-1";

/// The composition a `comp` parameter value names, given the composition the
/// node sits in.
///
/// `""` and any other unparsable spelling are refused rather than folded into
/// `-1`, exactly as `layer.info`'s target is: the default is written into
/// every node the template creates, so a value that is not it is a value
/// something wrote, and guessing "the current one" would read a different
/// composition than the document says.
fn target_comp(value: &str, current: CompId) -> anyhow::Result<CompId> {
    match value.parse::<i64>() {
        Ok(-1) => Ok(current),
        // Zero is the reserved "no composition" id, the spelling
        // `ParameterValue::identifier` already reads as `Identifier::Unset`.
        Ok(raw) if raw > 0 => Ok(CompId::new(raw as u64)),
        _ => anyhow::bail!("comp.info: no target composition set ({value:?})"),
    }
}

/// The instant being evaluated, in **seconds on `current`'s clock**.
///
/// `ctx.frame` is `source`'s local frame, so it goes back out to composition
/// time before it becomes a time at all. `sample_frame` carries the
/// sub-frame part (motion blur, time remapping) and the subtraction cancels
/// it back in, so a fractional instant survives the trip.
///
/// Seconds, not frames, because this is the value every port derives from:
/// `comp_f` re-divides it by whichever composition is being read, and a
/// rational frame rate makes the frame-number route lossy.
fn comp_seconds(source: &Layer, current: &Composition, ctx: &EvalContext) -> f64 {
    let sub_frame = ctx.sample_frame() - ctx.frame as f64;
    let comp_frame = source.comp_frame(ctx.frame as i64) as f64 + sub_frame;
    comp_frame / current.frame_rate.as_f64()
}

/// One composition, read port by port.
struct CompReader<'a> {
    target: &'a Composition,
    /// The instant being evaluated, in seconds on the **current**
    /// composition's clock — the same number whichever composition
    /// [`target`](Self::target) is.
    comp_t: f64,
}

impl CompReader<'_> {
    fn value(&self, port: &str) -> anyhow::Result<Arc<dyn NodeData>> {
        let comp = self.target;
        let scalar = |v: f32| -> Arc<dyn NodeData> { Arc::new(Scalar(v)) };

        Ok(match port {
            "name" => Arc::new(PlainText(comp.name.clone())),
            "resolution" => Arc::new(Vec2(comp.resolution.0 as f32, comp.resolution.1 as f32)),
            "frame_rate" => scalar(comp.frame_rate.as_f64() as f32),
            "duration_frames" => scalar(comp.duration_frames as f32),
            "comp_t" => scalar(self.comp_t as f32),
            // The instant in the **target's** frames. When the target is the
            // current composition this is the composition frame itself; when
            // it is not, this is the whole point of the port.
            "comp_f" => scalar((self.comp_t * comp.frame_rate.as_f64()) as f32),
            "background" => Arc::new(comp.background_color),
            "layer_count" => scalar(comp.layers.len() as f32),
            other => anyhow::bail!("comp.info: no such output port {other:?}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::composition::{Document, Layer};
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::{DataTypeId, LayerId, NodeId};
    use ravel_core::registry::{NodeRegistry, builtin};
    use ravel_core::types::{Color, FrameRate};

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };
    /// 30000/1001 — the rational rate the plan names, and the reason the
    /// seconds rather than the frame number are the authority.
    const NTSC: FrameRate = FrameRate {
        num: 30000,
        den: 1001,
    };
    const RES: (u32, u32) = (200, 100);

    fn here() -> CompId {
        CompId::new(1)
    }

    fn info_node(id: u64, target: &str, ports: &[&str]) -> Node {
        let mut reg = NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        let options: Vec<_> = reg.output_options("comp.info").to_vec();
        let mut node = Node::new(NodeId::new(id), "comp.info")
            .with_param("comp", ParameterValue::String(target.into()));
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

    fn comp_of(id: CompId, name: &str, fps: FrameRate, layers: Vec<Layer>) -> Composition {
        let mut comp = Composition::new(id, name, RES, fps, 300);
        for l in layers {
            comp.layers.push_back(l);
        }
        comp
    }

    /// `comp` with `owner`'s network replaced by `graph` — the node has to be
    /// *in* the layer it claims to belong to.
    fn with_network(comp: &Composition, owner: LayerId, graph: &Graph) -> Composition {
        let mut comp = comp.clone();
        let index = comp.layers.iter().position(|l| l.id == owner).unwrap();
        let mut layer = comp.layers[index].clone();
        layer.network = graph.clone();
        comp.layers.set(index, layer);
        comp
    }

    /// Evaluate `node` inside `owner`'s scope in the first composition, with
    /// every composition in `comps` present in the document.
    fn eval_in(
        comps: &[Composition],
        owner: LayerId,
        node: Node,
        frame: u64,
    ) -> Result<Arc<dyn NodeData>, ravel_core::eval::EvalError> {
        let ctx = EvalContext::new(frame, comps[0].frame_rate, RES);
        eval_in_ctx(comps, owner, node, &ctx)
    }

    /// [`eval_in`] at an arbitrary context, for the instants that do not sit
    /// on the frame grid.
    fn eval_in_ctx(
        comps: &[Composition],
        owner: LayerId,
        node: Node,
        ctx: &EvalContext,
    ) -> Result<Arc<dyn NodeData>, ravel_core::eval::EvalError> {
        let id = node.id;
        let graph = Graph::new().add_node(node).unwrap();
        let mut document = Document::default();
        for (index, comp) in comps.iter().enumerate() {
            document = document.with_composition(if index == 0 {
                with_network(comp, owner, &graph)
            } else {
                comp.clone()
            });
        }
        let host = comps[0].id;
        let mut ev = Evaluator::new();
        ev.register(id, Arc::new(CompInfoProcessor));
        ev.set_document(Arc::new(document));
        ev.evaluate_at(&[PathSegment::Layer(host, owner)], &graph, id, ctx)
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
        let options = reg.output_options("comp.info").to_vec();
        assert!(!options.is_empty(), "comp.info declares candidates");

        let names: Vec<&str> = options.iter().map(|p| p.name.as_str()).collect();
        let comp = comp_of(here(), "Here", FPS, vec![layer(1, "Only")]);
        let out = eval_in(&[comp], LayerId::new(1), info_node(10, "-1", &names), 0).unwrap();
        for (value, port) in record(&out).iter().zip(&options) {
            assert_eq!(
                value.data_type_id(),
                port.data_type,
                "port {} answered with the wrong type",
                port.name
            );
        }
    }

    /// `-1` is the composition the network belongs to; a decimal id is any
    /// other composition in the document.
    #[test]
    fn the_current_and_another_composition_both_read() {
        let mut here = comp_of(here(), "Here", FPS, vec![layer(1, "Only")]);
        here.resolution = (200, 100);
        let mut there = comp_of(CompId::new(7), "There", NTSC, vec![layer(2, "A")]);
        there.resolution = (640, 480);
        there.duration_frames = 90;
        there.background_color = Color::new(0.25, 0.5, 0.75, 1.0);
        let comps = [here, there];

        let read = |target: &str| {
            let out = eval_in(
                &comps,
                LayerId::new(1),
                info_node(
                    10,
                    target,
                    &[
                        "name",
                        "resolution",
                        "frame_rate",
                        "duration_frames",
                        "background",
                        "layer_count",
                    ],
                ),
                0,
            )
            .unwrap();
            record(&out)
        };

        let mine = read("-1");
        assert_eq!(mine[0].downcast_ref::<PlainText>().unwrap().0, "Here");
        let res = mine[1].downcast_ref::<Vec2>().unwrap();
        assert_eq!((res.0, res.1), (200.0, 100.0));
        assert_eq!(scalar_of(&mine[2]), 30.0);
        assert_eq!(scalar_of(&mine[3]), 300.0);
        assert_eq!(*mine[4].downcast_ref::<Color>().unwrap(), Color::BLACK);
        assert_eq!(scalar_of(&mine[5]), 1.0);

        let other = read("7");
        assert_eq!(other[0].downcast_ref::<PlainText>().unwrap().0, "There");
        let res = other[1].downcast_ref::<Vec2>().unwrap();
        assert_eq!((res.0, res.1), (640.0, 480.0));
        assert!((scalar_of(&other[2]) - 30000.0 / 1001.0).abs() < 1e-3);
        assert_eq!(scalar_of(&other[3]), 90.0);
        assert_eq!(
            *other[4].downcast_ref::<Color>().unwrap(),
            Color::new(0.25, 0.5, 0.75, 1.0)
        );
        assert_eq!(scalar_of(&other[5]), 1.0);
    }

    /// The plan's rule, in the case it was written for: the seconds are the
    /// authority and `comp_f` is that instant divided by the **target's**
    /// frame rate.
    ///
    /// Reading the current composition at 30/1 gives back the composition
    /// frame itself; reading a 30000/1001 composition at the same instant
    /// gives a *different* number, and the difference is the whole point —
    /// dividing by the reader's own fps would make both answers 60.
    #[test]
    fn comp_f_is_divided_by_the_targets_frame_rate() {
        let here = comp_of(here(), "Here", FPS, vec![layer(1, "Only")]);
        let there = comp_of(CompId::new(7), "There", NTSC, vec![]);
        let comps = [here, there];

        // Composition frame 60 at 30/1 is 2.0 s.
        let read = |target: &str| {
            let out = eval_in(
                &comps,
                LayerId::new(1),
                info_node(10, target, &["comp_t", "comp_f"]),
                60,
            )
            .unwrap();
            let v = record(&out);
            (scalar_of(&v[0]), scalar_of(&v[1]))
        };

        let (t, f) = read("-1");
        assert!((t - 2.0).abs() < 1e-6, "comp_t was {t}");
        assert!((f - 60.0).abs() < 1e-4, "the current comp's own frame: {f}");

        let (t, f) = read("7");
        assert!(
            (t - 2.0).abs() < 1e-6,
            "the seconds do not depend on the target: {t}"
        );
        let expected = 2.0 * 30000.0 / 1001.0;
        assert!(
            (f - expected as f32).abs() < 1e-2,
            "comp_f must use 30000/1001, expected {expected}, got {f}"
        );
        assert!(
            (f - 60.0).abs() > 0.05,
            "dividing by the reader's own fps would give 60"
        );
    }

    /// `comp_t` / `comp_f` are the **composition's** clock, not the reading
    /// layer's local one.
    ///
    /// The owning layer carries a non-zero `start_frame` **and** a non-zero
    /// `in_frame`, because both terms of `Layer::comp_frame` matter: with
    /// either at zero the conversion could be dropped or sign-flipped and
    /// still land on the right number. `local_f` for this layer is 20, the
    /// composition frame is 20 + 30 - 5 = 45, and only one of the two is a
    /// port on this node.
    #[test]
    fn comp_time_is_the_compositions_clock_not_the_layers() {
        let comp = comp_of(
            here(),
            "Here",
            FPS,
            vec![layer(1, "Off zero").with_time(30, 5, 300)],
        );
        let out = eval_in(
            &[comp],
            LayerId::new(1),
            info_node(10, "-1", &["comp_t", "comp_f"]),
            20,
        )
        .unwrap();
        let v = record(&out);
        assert!(
            (scalar_of(&v[1]) - 45.0).abs() < 1e-4,
            "local 20 → comp 45, got {}",
            scalar_of(&v[1])
        );
        assert!(
            (scalar_of(&v[0]) - 1.5).abs() < 1e-6,
            "comp frame 45 at 30 fps is 1.5 s, got {}",
            scalar_of(&v[0])
        );
    }

    /// An instant **between** two frames keeps its fraction all the way
    /// through the conversion.
    ///
    /// The test above places the layer off zero but still lands on the frame
    /// grid, where dropping the sub-frame term changes nothing. Here the
    /// context sits half a frame past local 20 — composition frame 45.5 —
    /// and both ports have to say so. Time remapping and motion blur sample
    /// exactly like this, and a `comp_f` that had silently snapped to 45
    /// would drive whatever reads it one full frame per sample.
    #[test]
    fn a_sub_frame_instant_keeps_its_fraction() {
        let comp = comp_of(
            here(),
            "Here",
            FPS,
            vec![layer(1, "Off zero").with_time(30, 5, 300)],
        );
        let mut ctx = EvalContext::new(20, FPS, RES);
        ctx.time += 0.5 / FPS.as_f64();
        let out = eval_in_ctx(
            &[comp],
            LayerId::new(1),
            info_node(10, "-1", &["comp_t", "comp_f"]),
            &ctx,
        )
        .unwrap();
        let v = record(&out);
        assert!(
            (scalar_of(&v[1]) - 45.5).abs() < 1e-4,
            "local 20.5 → comp 45.5, got {}",
            scalar_of(&v[1])
        );
        assert!(
            (scalar_of(&v[0]) - 45.5 / 30.0).abs() < 1e-5,
            "comp frame 45.5 at 30 fps is 1.51666…s, got {}",
            scalar_of(&v[0])
        );
    }

    /// The error names the composition that was asked for — the id is the
    /// only part of the message that tells the user which node to go and fix.
    #[test]
    fn a_missing_target_is_reported_with_its_id() {
        let comp = comp_of(here(), "Here", FPS, vec![layer(1, "Only")]);
        let message = match eval_in(&[comp], LayerId::new(1), info_node(10, "77", &["name"]), 0) {
            Err(err) => error_chain(&err),
            Ok(_) => panic!("a missing target must not evaluate"),
        };
        assert!(message.contains("CompId(77)"), "message was {message}");
    }

    /// A target nothing set is refused rather than silently read as `-1`.
    #[test]
    fn an_unset_target_is_refused() {
        assert!(target_comp("-1", here()).is_ok());
        for spelling in ["", "0", "nonsense", "-2"] {
            assert!(
                target_comp(spelling, here()).is_err(),
                "{spelling:?} must not resolve"
            );
        }
    }

    /// A `comp.info` target keeps its id reserved even after the composition
    /// it names is gone, so the next `CompId` cannot land on it and quietly
    /// point the node at an unrelated composition (the `HIGH-35` shape).
    ///
    /// Take `comp.info` out of `validate::comp_target_ids` and the watermark
    /// drops back to the surviving composition's own id.
    #[test]
    fn a_comp_info_target_holds_the_composition_watermark() {
        let graph = Graph::new()
            .add_node(info_node(10, "42", &["name"]))
            .unwrap();
        let comp = with_network(
            &comp_of(here(), "Here", FPS, vec![layer(1, "Only")]),
            LayerId::new(1),
            &graph,
        );
        let document = Document::default().with_composition(comp);
        assert_eq!(
            document.id_watermarks().comp,
            42,
            "a deleted composition's id stays reserved while a node names it"
        );
    }

    /// The same node with the default target reserves nothing: `-1` names no
    /// composition, so it must not push the watermark anywhere.
    #[test]
    fn the_self_target_reserves_no_composition_id() {
        let graph = Graph::new()
            .add_node(info_node(10, "-1", &["name"]))
            .unwrap();
        let comp = with_network(
            &comp_of(here(), "Here", FPS, vec![layer(1, "Only")]),
            LayerId::new(1),
            &graph,
        );
        let document = Document::default().with_composition(comp);
        assert_eq!(document.id_watermarks().comp, here().raw());
    }

    /// A node outside a layer network has no composition to call its own, so
    /// `-1` would have to guess. It refuses instead.
    #[test]
    fn outside_a_layer_network_it_refuses() {
        let node = info_node(10, "-1", &["name"]);
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(10), Arc::new(CompInfoProcessor));
        ev.set_document(Arc::new(Document::default().with_composition(comp_of(
            here(),
            "Here",
            FPS,
            vec![layer(1, "Only")],
        ))));
        let message = match ev.evaluate(&graph, NodeId::new(10), &EvalContext::new(0, FPS, RES)) {
            Err(err) => error_chain(&err),
            Ok(_) => panic!("no layer scope, no composition"),
        };
        assert!(
            message.contains("not evaluated inside a layer network"),
            "message was {message}"
        );
    }

    /// A single picked port is handed out on its own, not wrapped in a
    /// record — the convention every multi-output node keeps.
    #[test]
    fn a_lone_port_is_not_wrapped_in_a_record() {
        let comp = comp_of(here(), "Here", FPS, vec![layer(1, "Only")]);
        let out = eval_in(
            &[comp],
            LayerId::new(1),
            info_node(10, "-1", &["frame_rate"]),
            0,
        )
        .unwrap();
        assert_eq!(out.data_type_id(), DataTypeId::SCALAR);
        assert_eq!(scalar_of(&out), 30.0);
    }
}
