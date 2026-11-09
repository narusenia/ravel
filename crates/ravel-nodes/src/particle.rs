// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `particle.simulate`: frame-sequential point particles (REQ-MOGRAPH-002,
//! `docs/implementation/particle-plan.md`).
//!
//! The node is a [`StatefulProcessor`]: the particles of frame `t` follow from
//! those of frame `t - 1`, the parameters and inputs of frame `t`, and nothing
//! else. The state is a `Point`-domain [`Geometry`] — one column per
//! attribute — so what the node outputs is the state itself, and a GPU step
//! can later work on the same columns one element per invocation.
//!
//! # Step order
//!
//! One frame is: integrate the particles alive, remove those past their life,
//! emit the new ones. A particle is born at the frame it is emitted (`age`
//! 0) and is not moved until the next. Integration is explicit (forward)
//! Euler — the position moves with the velocity of the frame before, and the
//! velocity then takes the acceleration:
//!
//! ```text
//! P' = P + v * dt          v' = v + (a - drag * v) * dt          age' = age + 1
//! ```
//!
//! A particle with `age' >= life` is removed, so one with `life = L` is drawn
//! for `L` frames.
//!
//! # Determinism
//!
//! Randomness is a hash of the `seed` parameter and the particle's `id`
//! ([`element_hash`]); `id` is the emission order, assigned once. Nothing
//! reads a clock, `rand` or the output resolution, so two evaluations of one
//! frame — or one frame reached by scrubbing back — are bit-identical.

use std::f32::consts::TAU;
use std::sync::Arc;

use ravel_core::eval::{EvalContext, ResolvedParams, SimInputs, StatefulProcessor};
use ravel_core::geometry::{AttributeArray, Geometry, element_hash, names};
use ravel_core::registry::builtin::PARTICLE_MAX_POINTS_DEFAULT;
use ravel_core::types::{NodeData, Vec2};

/// The state of one frame: the live particles and what emission carries over.
pub struct ParticleState {
    /// `P`, `velocity`, `id`, `age`, `life` and `index` of every live
    /// particle, `Point` domain only.
    points: Geometry,
    /// The `id` the next emitted particle takes.
    next_id: i32,
    /// The fraction of a particle `rate` has promised but not yet emitted.
    /// `f64` so a long run of small fractions does not drift.
    carry: f64,
    /// Whether the last emission was cut short by `max_points`, so the warning
    /// is raised when the cap is *reached* rather than on every frame spent
    /// at it.
    capped: bool,
}

impl ParticleState {
    /// The live particles.
    pub fn points(&self) -> &Geometry {
        &self.points
    }

    /// Whether the last step's emission was cut short by `max_points`.
    pub fn capped(&self) -> bool {
        self.capped
    }
}

/// The processor behind `particle.simulate`.
pub struct ParticleSimulate;

/// The parameters of one frame, read once per step.
struct Config {
    rate: f64,
    life: f32,
    life_variance: f32,
    max_points: usize,
    seed: u32,
    initial_velocity: Vec2,
    initial_spread: f32,
    drag: f32,
}

impl Config {
    fn read(params: &ResolvedParams) -> Self {
        let [vx, vy] = params.vec2_or("initial_velocity", [0.0, 0.0]);
        Self {
            rate: f64::from(params.f32_or("rate", 60.0).max(0.0)),
            life: params.f32_or("life", 60.0),
            life_variance: params.f32_or("life_variance", 0.0).max(0.0),
            max_points: params
                .i32_or("max_points", PARTICLE_MAX_POINTS_DEFAULT)
                .max(0) as usize,
            seed: params.i32_or("seed", 0) as u32,
            initial_velocity: Vec2(vx, vy),
            initial_spread: params.f32_or("initial_spread", 0.0).max(0.0),
            drag: params.f32_or("drag", 0.0).max(0.0),
        }
    }
}

/// The particle columns while a step builds them.
#[derive(Default)]
struct Columns {
    p: Vec<Vec2>,
    v: Vec<Vec2>,
    id: Vec<i32>,
    age: Vec<f32>,
    life: Vec<f32>,
}

impl Columns {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            p: Vec::with_capacity(capacity),
            v: Vec::with_capacity(capacity),
            id: Vec::with_capacity(capacity),
            age: Vec::with_capacity(capacity),
            life: Vec::with_capacity(capacity),
        }
    }

    fn len(&self) -> usize {
        self.p.len()
    }

    fn into_geometry(self) -> Geometry {
        let mut points = Geometry::from_points(self.p);
        let set = points.points_mut();
        // The columns share `P`'s length by construction.
        set.insert(names::VELOCITY, AttributeArray::Vec2(self.v))
            .expect("velocity matches P");
        set.insert(names::ID, AttributeArray::I32(self.id))
            .expect("id matches P");
        set.insert(names::AGE, AttributeArray::F32(self.age))
            .expect("age matches P");
        set.insert(names::LIFE, AttributeArray::F32(self.life))
            .expect("life matches P");
        points
    }
}

/// The five simulation columns of `points`, or `None` when any is missing or
/// the wrong type (a state this node did not make), which reads as no
/// particles.
struct Live<'a> {
    p: &'a [Vec2],
    v: &'a [Vec2],
    id: &'a [i32],
    age: &'a [f32],
    life: &'a [f32],
}

impl<'a> Live<'a> {
    fn of(points: &'a Geometry) -> Option<Self> {
        let set = points.points();
        let column = |name: &str| set.get(name).map(|column| &**column);
        Some(Self {
            p: column(names::P)?.as_vec2(names::P).ok()?,
            v: column(names::VELOCITY)?.as_vec2(names::VELOCITY).ok()?,
            id: column(names::ID)?.as_i32(names::ID).ok()?,
            age: column(names::AGE)?.as_f32(names::AGE).ok()?,
            life: column(names::LIFE)?.as_f32(names::LIFE).ok()?,
        })
    }
}

/// A uniform `[0, 1)` from a hash, 24 bits of it.
fn unit(hash: u32) -> f32 {
    (hash & 0x00FF_FFFF) as f32 / 0x0100_0000 as f32
}

/// The `k`-th independent random number of the particle `id`, in `[0, 1)`.
fn random(seed: u32, id: i32, k: u32) -> f32 {
    unit(element_hash(element_hash(seed, id as u32), k))
}

impl ParticleSimulate {
    /// Emits this frame's particles after the `alive` already there, appending
    /// to `out`. Returns the new `(carry, next_id, capped)`.
    fn emit(
        config: &Config,
        ctx: &EvalContext,
        alive: usize,
        carry: f64,
        next_id: i32,
        out: &mut Columns,
    ) -> (f64, i32, bool) {
        let wanted = carry + config.rate / ctx.fps.as_f64();
        let whole = wanted.floor();
        let room = config.max_points.saturating_sub(alive);
        // `as usize` saturates, and `room` bounds the result.
        let count = (whole as usize).min(room);
        let capped = (whole as usize) > room;
        let mut next_id = next_id;
        for _ in 0..count {
            let id = next_id;
            next_id = next_id.wrapping_add(1);
            let life = (config.life
                + config.life_variance * (2.0 * random(config.seed, id, 0) - 1.0))
                .max(1.0);
            let angle = random(config.seed, id, 1) * TAU;
            let jitter = config.initial_spread * random(config.seed, id, 2).sqrt();
            out.p.push(Vec2(0.0, 0.0));
            out.v.push(Vec2(
                config.initial_velocity.0 + jitter * angle.cos(),
                config.initial_velocity.1 + jitter * angle.sin(),
            ));
            out.id.push(id);
            out.age.push(0.0);
            out.life.push(life);
        }
        (wanted - whole, next_id, capped)
    }

    fn finish(columns: Columns, carry: f64, next_id: i32, capped: bool) -> ParticleState {
        ParticleState {
            points: columns.into_geometry(),
            next_id,
            carry,
            capped,
        }
    }
}

/// Raises the cap warning on the frame the cap is reached, not on each frame
/// spent there.
fn warn_on_reaching_cap(was_capped: bool, capped: bool, config: &Config) {
    if capped && !was_capped {
        tracing::warn!(
            max_points = config.max_points,
            "particle.simulate reached max_points; emission is suppressed until particles expire"
        );
    }
}

impl StatefulProcessor for ParticleSimulate {
    type State = ParticleState;

    fn initial(&self, ctx: &EvalContext, inputs: &SimInputs<'_>) -> ParticleState {
        let config = Config::read(inputs.params);
        let mut columns = Columns::default();
        let (carry, next_id, capped) = Self::emit(&config, ctx, 0, 0.0, 0, &mut columns);
        warn_on_reaching_cap(false, capped, &config);
        Self::finish(columns, carry, next_id, capped)
    }

    fn step(
        &self,
        prev: &ParticleState,
        ctx: &EvalContext,
        inputs: &SimInputs<'_>,
    ) -> ParticleState {
        let config = Config::read(inputs.params);
        let dt = (1.0 / ctx.fps.as_f64()) as f32;
        let damping = (config.drag * dt).min(1.0);

        let mut next = Columns::default();
        if let Some(live) = Live::of(&prev.points) {
            next = Columns::with_capacity(live.p.len());
            for i in 0..live.p.len() {
                let age = live.age[i] + 1.0;
                if age >= live.life[i] {
                    continue;
                }
                let (p, v) = (live.p[i], live.v[i]);
                next.p.push(Vec2(p.0 + v.0 * dt, p.1 + v.1 * dt));
                next.v.push(Vec2(v.0 - v.0 * damping, v.1 - v.1 * damping));
                next.id.push(live.id[i]);
                next.age.push(age);
                next.life.push(live.life[i]);
            }
        }
        let alive = next.len();
        let (carry, next_id, capped) =
            Self::emit(&config, ctx, alive, prev.carry, prev.next_id, &mut next);
        warn_on_reaching_cap(prev.capped, capped, &config);
        Self::finish(next, carry, next_id, capped)
    }

    fn output(&self, state: &ParticleState) -> Arc<dyn NodeData> {
        Arc::new(state.points.clone())
    }

    fn byte_size(&self, state: &ParticleState) -> u64 {
        std::mem::size_of::<ParticleState>() as u64 + state.points.byte_size()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::NodeId;
    use ravel_core::registry::NodeRegistry;
    use ravel_core::registry::builtin::register_builtins;
    use ravel_core::types::FrameRate;
    use std::sync::Mutex;
    use tracing_subscriber::layer::SubscriberExt as _;

    pub(crate) const FPS: u32 = 30;
    pub(crate) const SIM: u64 = 1;

    pub(crate) fn ctx(frame: u64) -> EvalContext {
        EvalContext::new(frame, FrameRate::new(FPS, 1), (200, 200))
    }

    /// A real `particle.simulate` node from the registry template, with
    /// `params` set over the defaults.
    pub(crate) fn sim_node(params: &[(&str, ParameterValue)]) -> Node {
        let mut registry = NodeRegistry::new();
        register_builtins(&mut registry);
        let mut node = registry
            .create_node("particle.simulate", NodeId::new(SIM))
            .expect("particle.simulate is registered");
        for (key, value) in params {
            node.parameters
                .iter_mut()
                .find(|parameter| parameter.key == *key)
                .unwrap_or_else(|| panic!("no {key} parameter"))
                .value = value.clone();
        }
        node
    }

    pub(crate) fn evaluator() -> Evaluator {
        let mut evaluator = Evaluator::new();
        evaluator.register(
            NodeId::new(SIM),
            ravel_core::eval::stateful_processor(ParticleSimulate),
        );
        evaluator
    }

    pub(crate) fn geometry_at(
        evaluator: &mut Evaluator,
        graph: &Graph,
        frame: u64,
    ) -> Arc<dyn NodeData> {
        evaluator
            .evaluate(graph, NodeId::new(SIM), &ctx(frame))
            .expect("the simulation evaluates")
    }

    pub(crate) fn column<'a>(geometry: &'a dyn NodeData, name: &str) -> &'a AttributeArray {
        geometry
            .downcast_ref::<Geometry>()
            .expect("a geometry")
            .points()
            .get(name)
            .unwrap_or_else(|| panic!("no {name} column"))
    }

    fn ids(geometry: &dyn NodeData) -> Vec<i32> {
        column(geometry, names::ID)
            .as_i32(names::ID)
            .unwrap()
            .to_vec()
    }

    fn count(geometry: &dyn NodeData) -> usize {
        geometry.downcast_ref::<Geometry>().unwrap().point_count()
    }

    fn one_node(params: &[(&str, ParameterValue)]) -> Graph {
        Graph::new().add_node(sim_node(params)).unwrap()
    }

    fn float(value: f32) -> ParameterValue {
        ParameterValue::Float(value)
    }

    #[test]
    fn emission_follows_rate_times_frame_time() {
        // 60 per second at 30 fps is two per frame, and frame 0 emits too.
        let graph = one_node(&[("rate", float(60.0)), ("life", float(1000.0))]);
        let mut ev = evaluator();
        for (frame, expected) in [(0, 2), (1, 4), (4, 10), (9, 20)] {
            assert_eq!(count(&*geometry_at(&mut ev, &graph, frame)), expected);
        }
    }

    #[test]
    fn a_fractional_rate_carries_over_between_frames() {
        // Half a particle per frame: one every other frame, none lost.
        let graph = one_node(&[("rate", float(15.0)), ("life", float(1000.0))]);
        let mut ev = evaluator();
        let counts: Vec<usize> = (0..6)
            .map(|frame| count(&*geometry_at(&mut ev, &graph, frame)))
            .collect();
        assert_eq!(counts, [0, 1, 1, 2, 2, 3]);
    }

    #[test]
    fn a_particle_past_its_life_is_removed() {
        // One particle per frame, each living three frames.
        let graph = one_node(&[("rate", float(30.0)), ("life", float(3.0))]);
        let mut ev = evaluator();
        let counts: Vec<usize> = (0..8)
            .map(|frame| count(&*geometry_at(&mut ev, &graph, frame)))
            .collect();
        assert_eq!(counts, [1, 2, 3, 3, 3, 3, 3, 3]);
        let out = geometry_at(&mut ev, &graph, 7);
        let age = column(&*out, names::AGE).as_f32(names::AGE).unwrap();
        assert!(age.iter().all(|age| *age < 3.0), "{age:?}");
    }

    #[test]
    fn id_is_stable_through_a_life_and_index_is_repacked() {
        let graph = one_node(&[("rate", float(30.0)), ("life", float(3.0))]);
        let mut ev = evaluator();
        let before = geometry_at(&mut ev, &graph, 4);
        let after = geometry_at(&mut ev, &graph, 5);

        // Particles 2, 3, 4 are alive at frame 4; 2 expires and 5 is born.
        assert_eq!(ids(&*before), [2, 3, 4]);
        assert_eq!(ids(&*after), [3, 4, 5]);
        // The survivors keep their id and have moved down the array.
        for geometry in [&before, &after] {
            assert_eq!(
                column(&**geometry, names::INDEX)
                    .as_i32(names::INDEX)
                    .unwrap(),
                [0, 1, 2]
            );
        }
    }

    #[test]
    fn the_same_seed_gives_bit_identical_particles() {
        let params = [
            ("rate", float(90.0)),
            ("life", float(20.0)),
            ("life_variance", float(8.0)),
            ("initial_velocity", ParameterValue::vec2(10.0, -4.0)),
            ("initial_spread", float(50.0)),
            ("seed", ParameterValue::Int(7)),
        ];
        let graph = one_node(&params);
        let run = |graph: &Graph| {
            let mut ev = evaluator();
            geometry_at(&mut ev, graph, 25)
        };
        let (first, second) = (run(&graph), run(&graph));
        for name in [
            names::P,
            names::VELOCITY,
            names::LIFE,
            names::AGE,
            names::ID,
        ] {
            assert_eq!(column(&*first, name), column(&*second, name), "{name}");
        }

        // A different seed changes the random draws, or the test proves nothing.
        let mut other = params.to_vec();
        other[5] = ("seed", ParameterValue::Int(8));
        let other = run(&one_node(&other));
        assert_ne!(
            column(&*first, names::VELOCITY),
            column(&*other, names::VELOCITY)
        );
    }

    /// Counts the warnings raised while `f` runs.
    fn warnings_during(f: impl FnOnce()) -> usize {
        #[derive(Default)]
        struct Count(Arc<Mutex<usize>>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Count {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if *event.metadata().level() == tracing::Level::WARN {
                    *self.0.lock().unwrap() += 1;
                }
            }
        }
        let count = Arc::new(Mutex::new(0));
        let subscriber = tracing_subscriber::registry().with(Count(count.clone()));
        tracing::subscriber::with_default(subscriber, f);
        let guard = count.lock().unwrap();
        *guard
    }

    #[test]
    fn reaching_max_points_stops_emission_and_warns_once() {
        // Ten per frame against a ceiling of 25.
        let graph = one_node(&[
            ("rate", float(300.0)),
            ("life", float(1000.0)),
            ("max_points", ParameterValue::Int(25)),
        ]);
        let mut ev = evaluator();
        let mut counts = Vec::new();
        let warnings = warnings_during(|| {
            for frame in 0..6 {
                counts.push(count(&*geometry_at(&mut ev, &graph, frame)));
            }
        });
        assert_eq!(
            counts,
            [10, 20, 25, 25, 25, 25],
            "emission stops at the cap"
        );
        // The warning marks reaching the cap, not every frame spent at it.
        assert_eq!(warnings, 1);
    }

    #[test]
    fn without_forces_a_particle_moves_in_a_straight_line() {
        let graph = one_node(&[
            ("rate", float(30.0)),
            ("life", float(1000.0)),
            ("initial_velocity", ParameterValue::vec2(60.0, -30.0)),
        ]);
        let mut ev = evaluator();
        let out = geometry_at(&mut ev, &graph, 10);
        let p = column(&*out, names::P).as_vec2(names::P).unwrap();
        // Particle `id` was born at frame `id` and has moved `10 - id` frames
        // at 60 and -30 px/s, which is 2 and -1 px per frame.
        for (id, position) in ids(&*out).into_iter().zip(p) {
            let frames = (10 - id) as f32;
            assert!(
                (position.0 - 2.0 * frames).abs() < 1e-4,
                "{id}: {position:?}"
            );
            assert!((position.1 + frames).abs() < 1e-4, "{id}: {position:?}");
        }
    }

    #[test]
    fn drag_decays_the_speed_geometrically() {
        let graph = one_node(&[
            ("rate", float(30.0)),
            ("life", float(1000.0)),
            ("initial_velocity", ParameterValue::vec2(90.0, 0.0)),
            ("drag", float(3.0)),
        ]);
        let mut ev = evaluator();
        let out = geometry_at(&mut ev, &graph, 6);
        let velocity = column(&*out, names::VELOCITY)
            .as_vec2(names::VELOCITY)
            .unwrap();
        // Particle 0 has had six steps of `v *= 1 - drag * dt`, dt = 1/30.
        let expected = 90.0 * (1.0f32 - 3.0 / 30.0).powi(6);
        assert!((velocity[0].0 - expected).abs() < 1e-3, "{}", velocity[0].0);
        assert!(velocity[0].0 < 90.0 && velocity[0].0 > 0.0);
    }

    /// Wall time of one steady-state step at 10k and 100k live particles, for
    /// the `max_points` default (`perf-baseline.md`). Run with
    /// `cargo test -p ravel-nodes --release particle_step_time -- --ignored --nocapture`
    /// and read `uptime` first: a loaded machine measures the load.
    #[test]
    #[ignore = "benchmark; prints, asserts nothing"]
    fn particle_step_time() {
        for live in [10_000usize, 100_000] {
            // `life` frames of `live / life` births each hold `live` alive.
            let life = 100.0;
            let per_frame = live as f32 / life;
            let graph = one_node(&[
                ("rate", float(per_frame * FPS as f32)),
                ("life", float(life)),
                ("max_points", ParameterValue::Int(10_000_000)),
                ("initial_velocity", ParameterValue::vec2(30.0, 10.0)),
                ("initial_spread", float(20.0)),
            ]);
            let mut ev = evaluator();
            geometry_at(&mut ev, &graph, 150);
            let frames = 50u64;
            let start = std::time::Instant::now();
            for frame in 151..151 + frames {
                geometry_at(&mut ev, &graph, frame);
            }
            let ms = start.elapsed().as_secs_f64() * 1e3 / frames as f64;
            let out = geometry_at(&mut ev, &graph, 150 + frames);
            println!("live {} -> {ms:.3} ms/step", count(&*out));
        }
    }
}
