// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `comp.time_remap`, `comp.freeze_frame` and `comp.frame_blend`: the time
//! nodes of the effects library. They read their input at a frame of their
//! own choosing instead of the one being rendered.
//!
//! **How the input is pulled.** [`NodeProcessor::pulls_inputs_itself`] is
//! `true`, so the evaluator does not evaluate the input at the current frame
//! first (a freeze would never use it); the node asks the scope for the input
//! at the frame it wants ([`EvalScope::evaluate_input_at`]), which caches the
//! shifted pull under `TimeShift(node, frame)` next to the unshifted entries.
//!
//! **Source position.** Each node reduces to one number, the (possibly
//! fractional) source frame `p`:
//!
//! - `comp.time_remap`: `p = curve(frame)`. The curve maps the output frame
//!   (x) to the source frame (y), both in this graph's frames; an empty curve,
//!   the default, is the identity for every frame. A curve clamps outside its
//!   end points, so a curve ending at x = 100 holds its last value after
//!   frame 100.
//! - `comp.freeze_frame`: `p = frame` parameter. It does not depend on the
//!   rendered frame, so the node is time-independent and its output is cached
//!   across frames.
//! - `comp.frame_blend`: `p = rendered frame + offset`.
//!
//! **Fractional `p`.** `TimeShift` is keyed by integer frames, so a node never
//! pulls a fractional frame. `interpolation = nearest` (`time_remap`'s
//! default) rounds `p`; `blend` (always, for `frame_blend`) pulls
//! `floor(p)` and `floor(p) + 1` and mixes them by the fractional part, in
//! premultiplied alpha, so `p = n + 0.5` is the mean of two frames. An integral
//! `p` pulls one frame and returns it untouched. `p` below 0 or not finite
//! reads frame 0 (frames are unsigned).
//!
//! Frames of different sizes (a resolution change between two pulls cannot
//! happen within one evaluation) are not mixed: the earlier one is returned.
//!
//! **Not shell nodes**: ordinary user-placed nodes; no `Document` access. Not
//! motion blur, and not a layer shell's `time_remap`.

use bytemuck::Zeroable;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::graph::Node;
use ravel_core::types::NodeData;
use ravel_gpu::{
    ComputeDispatch, ComputePipeline, GpuContext, GpuFrameBuffer, ShaderManager, TexturePool,
};
use std::sync::{Arc, Mutex};

use super::transparent;
use crate::gpu_util;

const SHADER_SRC: &str = include_str!("../shaders/comp_time_mix.wgsl");

pub use ravel_core::registry::builtin::COMP_TIME_REMAP_INTERPOLATIONS as INTERPOLATIONS;

/// Whether `interpolation` is a `comp.time_remap` value the processor knows.
pub fn comp_time_remap_interpolation_is_known(interpolation: &str) -> bool {
    INTERPOLATIONS.contains(&interpolation)
}

/// Which node a [`CompTimeProcessor`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeKind {
    Remap,
    Freeze,
    FrameBlend,
}

/// The source frames one output frame needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Pull {
    One(u64),
    /// `frame` and `frame + 1`, mixed by `t` in `(0, 1)`.
    Blend {
        frame: u64,
        t: f32,
    },
}

impl Pull {
    /// Reduce the source position `p` to the frames to pull.
    pub(crate) fn plan(p: f64, blend: bool) -> Self {
        let p = if p.is_finite() { p.max(0.0) } else { 0.0 };
        let floor = p.floor();
        let t = (p - floor) as f32;
        if t == 0.0 {
            Self::One(floor as u64)
        } else if blend {
            Self::Blend {
                frame: floor as u64,
                t,
            }
        } else {
            Self::One(p.round() as u64)
        }
    }
}

impl TimeKind {
    fn label(self) -> &'static str {
        match self {
            Self::Remap => "comp.time_remap",
            Self::Freeze => "comp.freeze_frame",
            Self::FrameBlend => "comp.frame_blend",
        }
    }

    /// What to pull for the rendered `frame`.
    pub(crate) fn pull(self, frame: u64, p: &ResolvedParams) -> Pull {
        match self {
            Self::Remap => {
                let source = match p.curve("curve") {
                    Some(curve) => f64::from(curve.evaluate(frame as f32)),
                    None => frame as f64,
                };
                Pull::plan(source, p.str_or("interpolation", "nearest") == "blend")
            }
            Self::Freeze => Pull::plan(f64::from(p.f32_or("frame", 0.0)).round(), false),
            Self::FrameBlend => Pull::plan(frame as f64 + f64::from(p.f32_or("offset", 0.5)), true),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    t: f32,
    _pad: [f32; 3],
}

pub struct CompTimeProcessor {
    kind: TimeKind,
    ctx: GpuContext,
    pipeline: Arc<ComputePipeline>,
    pool: Arc<Mutex<TexturePool>>,
}

impl CompTimeProcessor {
    pub fn new(
        kind: TimeKind,
        ctx: GpuContext,
        shaders: &mut ShaderManager,
        pool: Arc<Mutex<TexturePool>>,
    ) -> Self {
        let layout = [
            gpu_util::input_texture_layout_entry(0),
            gpu_util::input_texture_layout_entry(1),
            gpu_util::output_storage_layout_entry(2),
            gpu_util::uniform_layout_entry(3),
        ];
        let source = gpu_util::with_premultiplied_helpers(SHADER_SRC);
        let pipeline = shaders
            .compute_pipeline(
                "comp_time_mix",
                &source,
                "main",
                &layout,
                gpu_util::WORKGROUP_SIZE,
            )
            .expect("comp_time_mix.wgsl compilation failed");
        Self {
            kind,
            ctx,
            pipeline,
            pool,
        }
    }

    fn mix(
        &self,
        a: Arc<dyn NodeData>,
        b: Arc<dyn NodeData>,
        t: f32,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let label = self.kind.label();
        if gpu_util::frame_size(a.as_ref()) != gpu_util::frame_size(b.as_ref()) {
            return Ok(a);
        }
        let ga = gpu_util::ensure_gpu(&self.ctx, &self.pool, a.as_ref())
            .map_err(|e| anyhow::anyhow!("{label}: {e}"))?;
        let gb = gpu_util::ensure_gpu(&self.ctx, &self.pool, b.as_ref())
            .map_err(|e| anyhow::anyhow!("{label}: {e}"))?;
        let (width, height) = ga.size();
        let output_tex = self
            .pool
            .lock()
            .unwrap()
            .acquire(gpu_util::tex_key_rw(width, height));
        let bindings = [ga.binding(), gb.binding()];
        let output_binding = output_tex.binding();
        let uniform = Params {
            t,
            ..Params::zeroed()
        };
        self.ctx.dispatch_compute(&ComputeDispatch {
            label,
            pipeline: &self.pipeline,
            inputs: &bindings,
            output: &output_binding,
            uniform: bytemuck::bytes_of(&uniform),
            width,
            height,
        });
        ga.release(&self.pool);
        gb.release(&self.pool);
        Ok(Arc::new(GpuFrameBuffer::new(
            self.ctx.clone(),
            &self.pool,
            output_tex,
            width,
            height,
        )))
    }
}

impl NodeProcessor for CompTimeProcessor {
    /// Nothing is captured from the node; every value is read from `params`.
    fn rebuild_on_node_change(&self) -> bool {
        false
    }

    /// The input is read at the node's own frames, never at the rendered one.
    fn pulls_inputs_itself(&self) -> bool {
        true
    }

    /// A freeze reads one fixed frame; the other two follow the rendered one.
    fn is_time_dependent(&self) -> bool {
        self.kind != TimeKind::Freeze
    }

    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let label = self.kind.label();
        let mut at = |frame| {
            scope
                .evaluate_input_at(node, 0, frame, ctx)
                .map_err(|e| anyhow::anyhow!("{label}: {e}"))
        };
        match self.kind.pull(ctx.frame, params) {
            Pull::One(frame) => Ok(at(frame)?.unwrap_or_else(|| transparent(ctx))),
            Pull::Blend { frame, t } => {
                let (Some(a), Some(b)) = (at(frame)?, at(frame + 1)?) else {
                    return Ok(transparent(ctx));
                };
                self.mix(a, b, t)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fx_test_util::*;
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::param_curve::CurveParam;
    use ravel_core::types::FrameBuffer;

    const SIZE: u32 = 4;

    /// The source's pixels at frame `f`: dyadic, so every mix below is exact.
    fn source_frame(f: u64) -> FrameBuffer {
        let v = f as f32 / 8.0;
        frame(
            SIZE,
            SIZE,
            &[[v, 0.5, 1.0 - v, 1.0]; (SIZE * SIZE) as usize],
        )
    }

    /// A time-dependent source emitting `source_frame(ctx.frame)`, recording
    /// every frame it is evaluated at.
    struct Source {
        frames: Arc<Mutex<Vec<u64>>>,
    }
    impl NodeProcessor for Source {
        fn process(
            &self,
            _node: &Node,
            ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.frames.lock().unwrap().push(ctx.frame);
            Ok(Arc::new(source_frame(ctx.frame)))
        }
        fn is_time_dependent(&self) -> bool {
            true
        }
    }

    struct Rig {
        ev: Evaluator,
        graph: Graph,
        time: NodeId,
        frames: Arc<Mutex<Vec<u64>>>,
    }

    impl Rig {
        /// `Source` -> the time node `kind` with `params`.
        fn new(gpu: &GpuContext, kind: TimeKind, params: Vec<(&str, ParameterValue)>) -> Self {
            let (src, time) = (NodeId::new(1), NodeId::new(2));
            let mut node = Node::new(time, "time")
                .with_input("image", &[DataTypeId::FRAME_BUFFER])
                .with_output("out", DataTypeId::FRAME_BUFFER);
            for (key, value) in params {
                node = node.with_param(key, value);
            }
            let graph = Graph::new()
                .add_node(
                    Node::new(src, "test.source").with_output("out", DataTypeId::FRAME_BUFFER),
                )
                .unwrap()
                .add_node(node)
                .unwrap()
                .add_edge(
                    EdgeId::new(1),
                    src,
                    OutputPortIndex(0),
                    time,
                    InputPortIndex(0),
                )
                .unwrap();
            let frames = Arc::new(Mutex::new(Vec::new()));
            let mut ev = Evaluator::new();
            ev.register(
                src,
                Arc::new(Source {
                    frames: frames.clone(),
                }),
            );
            let mut shaders = ShaderManager::new(gpu.clone());
            ev.register(
                time,
                Arc::new(CompTimeProcessor::new(
                    kind,
                    gpu.clone(),
                    &mut shaders,
                    pool(gpu),
                )),
            );
            Self {
                ev,
                graph,
                time,
                frames,
            }
        }

        /// The time node's output at `frame`.
        fn at(&mut self, frame: u64) -> FrameBuffer {
            let c = ctx((SIZE, SIZE)).at_frame(frame);
            let out = self
                .ev
                .evaluate(&self.graph, self.time, &c)
                .expect("time node");
            match out.downcast_ref::<FrameBuffer>() {
                Some(fb) => fb.clone(),
                None => readback(out.as_ref()),
            }
        }

        fn source_frames(&self) -> Vec<u64> {
            self.frames.lock().unwrap().clone()
        }
    }

    fn assert_same(a: &FrameBuffer, b: &FrameBuffer, what: &str) {
        assert_eq!(a.as_f32(), b.as_f32(), "{what}");
    }

    fn average(a: &FrameBuffer, b: &FrameBuffer) -> FrameBuffer {
        FrameBuffer::from_f32(
            a.width,
            a.height,
            a.as_f32()
                .iter()
                .zip(b.as_f32().iter())
                .map(|(x, y)| (x + y) / 2.0)
                .collect(),
        )
    }

    fn curve(points: &[(f32, f32)]) -> ParameterValue {
        ParameterValue::Curve(CurveParam::linear(points.iter().copied()))
    }

    #[test]
    fn plan_rounds_or_splits_the_source_position() {
        assert_eq!(Pull::plan(4.0, true), Pull::One(4));
        assert_eq!(Pull::plan(4.4, false), Pull::One(4));
        assert_eq!(Pull::plan(4.5, false), Pull::One(5));
        assert_eq!(Pull::plan(4.25, true), Pull::Blend { frame: 4, t: 0.25 });
        assert_eq!(Pull::plan(-3.0, true), Pull::One(0), "frames are unsigned");
        assert_eq!(Pull::plan(f64::NAN, true), Pull::One(0));
    }

    #[test]
    fn an_identity_curve_gives_the_input_back() {
        let Some(gpu) = gpu_or_skip() else { return };
        // No curve parameter, and the template's empty curve.
        for params in [
            vec![],
            vec![("curve", ParameterValue::Curve(CurveParam::from_points([])))],
        ] {
            let mut r = Rig::new(&gpu, TimeKind::Remap, params);
            for f in [0, 3, 9] {
                assert_same(&r.at(f), &source_frame(f), "identity");
            }
            assert_eq!(r.source_frames(), vec![0, 3, 9]);
        }
    }

    #[test]
    fn a_curve_maps_the_output_frame_to_the_source_frame() {
        let Some(gpu) = gpu_or_skip() else { return };
        // Frame x reads source frame 10 - x (a reversal).
        let mut r = Rig::new(
            &gpu,
            TimeKind::Remap,
            vec![("curve", curve(&[(0.0, 10.0), (10.0, 0.0)]))],
        );
        assert_same(&r.at(3), &source_frame(7), "reversed");
        // The unshifted frame 3 is never evaluated.
        assert_eq!(r.source_frames(), vec![7]);
    }

    #[test]
    fn remap_rounds_by_default_and_blends_on_request() {
        let Some(gpu) = gpu_or_skip() else { return };
        // Frame 3 reads source position 1.5.
        let half = || curve(&[(0.0, 0.0), (8.0, 4.0)]);
        let mut nearest = Rig::new(&gpu, TimeKind::Remap, vec![("curve", half())]);
        assert_same(&nearest.at(3), &source_frame(2), "nearest");
        let blend = vec![
            ("curve", half()),
            ("interpolation", ParameterValue::String("blend".into())),
        ];
        let mut blended = Rig::new(&gpu, TimeKind::Remap, blend);
        assert_same(
            &blended.at(3),
            &average(&source_frame(1), &source_frame(2)),
            "blend",
        );
    }

    #[test]
    fn a_freeze_is_the_same_output_on_every_frame() {
        let Some(gpu) = gpu_or_skip() else { return };
        let mut r = Rig::new(
            &gpu,
            TimeKind::Freeze,
            vec![("frame", ParameterValue::Float(5.0))],
        );
        for f in 0..6 {
            assert_same(&r.at(f), &source_frame(5), "frozen frame");
        }
        // One pull at the frozen frame serves every rendered frame.
        assert_eq!(r.source_frames(), vec![5]);
    }

    #[test]
    fn a_frame_blend_between_two_frames_is_their_average() {
        let Some(gpu) = gpu_or_skip() else { return };
        // The default offset is 0.5: frame 4 mixes frames 4 and 5.
        let mut r = Rig::new(&gpu, TimeKind::FrameBlend, vec![]);
        assert_same(
            &r.at(4),
            &average(&source_frame(4), &source_frame(5)),
            "midpoint",
        );
        let mut frames = r.source_frames();
        frames.sort_unstable();
        assert_eq!(frames, vec![4, 5]);

        // A whole offset is one untouched pull.
        let mut r = Rig::new(
            &gpu,
            TimeKind::FrameBlend,
            vec![("offset", ParameterValue::Float(0.0))],
        );
        assert_same(&r.at(4), &source_frame(4), "offset 0");
        assert_eq!(r.source_frames(), vec![4]);

        // A quarter offset weights the later frame by 0.25.
        let mut r = Rig::new(
            &gpu,
            TimeKind::FrameBlend,
            vec![("offset", ParameterValue::Float(0.25))],
        );
        let out = r.at(4);
        let (a, b) = (source_frame(4), source_frame(5));
        for ((o, x), y) in out
            .as_f32()
            .iter()
            .zip(a.as_f32().iter())
            .zip(b.as_f32().iter())
        {
            assert!((o - (0.75 * x + 0.25 * y)).abs() < 1e-6);
        }
    }

    /// Premultiplied mix: a transparent frame contributes no colour.
    #[test]
    fn the_mix_is_premultiplied() {
        let Some(gpu) = gpu_or_skip() else { return };
        let mut shaders = ShaderManager::new(gpu.clone());
        let p = CompTimeProcessor::new(TimeKind::FrameBlend, gpu.clone(), &mut shaders, pool(&gpu));
        let a: Arc<dyn NodeData> = Arc::new(frame(1, 1, &[[1.0, 0.0, 0.0, 1.0]]));
        let b: Arc<dyn NodeData> = Arc::new(frame(1, 1, &[[0.0, 1.0, 0.0, 0.0]]));
        let out = readback(p.mix(a, b, 0.5).unwrap().as_ref());
        // Half-covered red: the straight colour stays red, alpha halves.
        assert_eq!(&*out.as_f32(), &[1.0, 0.0, 0.0, 0.5]);
    }

    /// The downstream-cache regression on the real node: consecutive frames
    /// through a time node each get their own source frame, and revisiting one
    /// is a cache hit rather than the neighbour's value.
    #[test]
    fn frames_downstream_of_a_time_node_are_never_served_stale() {
        let Some(gpu) = gpu_or_skip() else { return };
        let mut r = Rig::new(
            &gpu,
            TimeKind::Remap,
            vec![("curve", curve(&[(0.0, 10.0), (100.0, 110.0)]))],
        );
        for f in [1, 2, 1, 3] {
            assert_same(&r.at(f), &source_frame(f + 10), &format!("frame {f}"));
        }
        assert_eq!(r.source_frames(), vec![11, 12, 13]);
    }

    #[test]
    fn without_an_input_the_node_is_transparent() {
        let Some(gpu) = gpu_or_skip() else { return };
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = Node::new(NodeId::new(9), "comp.freeze_frame")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_output("out", DataTypeId::FRAME_BUFFER);
        let graph = Graph::new().add_node(node.clone()).unwrap();
        let mut ev = Evaluator::new();
        let p = CompTimeProcessor::new(TimeKind::Freeze, gpu.clone(), &mut shaders, pool(&gpu));
        ev.register(node.id, Arc::new(p));
        let out = ev.evaluate(&graph, node.id, &ctx((2, 2))).unwrap();
        let fb = out.downcast_ref::<FrameBuffer>().expect("a CPU frame");
        assert!(fb.as_f32().iter().all(|v| *v == 0.0));
    }
}
