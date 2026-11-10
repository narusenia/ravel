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
//! # Emission
//!
//! The emitter input is any geometry; where a particle is born is sampled from
//! it ([`Emitter`]): along the first path by arc length, or at a point or
//! instance position. Every attribute the emitter row carries (`Cd`,
//! `pscale`, …) is inherited, except the ones the simulation owns
//! ([`SIMULATED`]). Nothing wired emits from the origin; a wired emitter with
//! nothing to emit from emits nothing and is not an error.
//!
//! # Forces
//!
//! Every field wired to the `force` group is sampled **once per frame, as a
//! batch, at the positions of the live particles** ([`Forces`]), with the
//! particles' own attributes on offer (`field.attribute("age")` works). What
//! the field answers is an acceleration in pixels per second squared: a scalar
//! field is a magnitude along `force_direction`, a `Vec2` field is the
//! acceleration itself (`field.direction_to` times a falloff is an attractor).
//! Accelerations add. Drag is a parameter, not a field: it slows a particle in
//! proportion to its speed.
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
use ravel_core::geometry::absent::absent_column;
use ravel_core::geometry::ops::PathArcTable;
use ravel_core::geometry::{
    AttrName, AttributeArray, AttributeSet, Domain, FieldSample, FieldValue, Geometry, Primitive,
    bounds_center, element_hash, names,
};
use ravel_core::registry::builtin::PARTICLE_MAX_POINTS_DEFAULT;
use ravel_core::types::{NodeData, Vec2};

/// Attributes the simulation writes itself. An emitter row that carries one
/// does not pass it on: `P` is where the particle is, `index` is the array
/// slot, and the rest are the particle's own state.
const SIMULATED: [&str; 6] = [
    names::P,
    names::INDEX,
    names::ID,
    names::AGE,
    names::LIFE,
    names::VELOCITY,
];

/// The state of one frame: the live particles and what emission carries over.
pub struct ParticleState {
    /// `P`, `velocity`, `id`, `age`, `life` and `index` of every live
    /// particle, plus the attributes they inherited. `Point` domain only.
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
    /// Whether the emitter could not be read (3D positions, a path that is
    /// not one), for the same once-per-episode warning.
    emitter_unusable: bool,
    /// Whether a force answered a type that is not an acceleration.
    force_unusable: bool,
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

/// Which part of the emitter geometry particles are born at.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EmitFrom {
    /// The first path when the geometry has one, else its points, else its
    /// instances.
    Auto,
    Points,
    Path,
    Instances,
}

impl EmitFrom {
    fn parse(value: &str) -> Self {
        match value {
            "points" => Self::Points,
            "path" => Self::Path,
            "instances" => Self::Instances,
            _ => Self::Auto,
        }
    }
}

/// How the launch direction is chosen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LaunchMode {
    /// `initial_velocity` as given.
    Fixed,
    /// A random direction at the speed of `initial_velocity`.
    Random,
    /// Away from the emitter: the path normal, or away from the emitter's
    /// centre for points and instances, at the speed of `initial_velocity`.
    Normal,
}

impl LaunchMode {
    fn parse(value: &str) -> Self {
        match value {
            "random" => Self::Random,
            "normal" => Self::Normal,
            _ => Self::Fixed,
        }
    }
}

/// The parameters of one frame, read once per step.
struct Config {
    rate: f64,
    life: f32,
    life_variance: f32,
    max_points: usize,
    seed: u32,
    emit_from: EmitFrom,
    jitter: f32,
    launch: LaunchMode,
    initial_velocity: Vec2,
    initial_spread: f32,
    force_direction: Vec2,
    drag: f32,
}

impl Config {
    fn read(params: &ResolvedParams) -> Self {
        let [vx, vy] = params.vec2_or("initial_velocity", [0.0, 0.0]);
        let [fx, fy] = params.vec2_or("force_direction", [0.0, 1.0]);
        Self {
            rate: f64::from(params.f32_or("rate", 60.0).max(0.0)),
            life: params.f32_or("life", 60.0),
            life_variance: params.f32_or("life_variance", 0.0).max(0.0),
            max_points: params
                .i32_or("max_points", PARTICLE_MAX_POINTS_DEFAULT)
                .max(0) as usize,
            seed: params.i32_or("seed", 0) as u32,
            emit_from: EmitFrom::parse(params.str_or("emit_from", "auto")),
            jitter: params.f32_or("jitter", 0.0).max(0.0),
            launch: LaunchMode::parse(params.str_or("velocity_mode", "fixed")),
            initial_velocity: Vec2(vx, vy),
            initial_spread: params.f32_or("initial_spread", 0.0).max(0.0),
            force_direction: Vec2(fx, fy),
            drag: params.f32_or("drag", 0.0).max(0.0),
        }
    }
}

// ===========================================================================
// Emitter
// ===========================================================================

/// Where one particle is born.
struct Sample {
    position: Vec2,
    /// Outward unit direction, or zero when the spot has none.
    normal: Vec2,
    /// The emitter row whose attributes the particle inherits.
    row: Option<usize>,
}

/// The emitter input, read once per step.
enum Emitter<'a> {
    /// Nothing is wired: particles are born at the origin.
    Origin,
    /// Something is wired but there is nothing to be born at.
    Empty,
    /// Born at one of `positions` (a point or an instance), chosen at random.
    Positions {
        positions: &'a [Vec2],
        attributes: &'a AttributeSet,
        center: Vec2,
    },
    /// Born at a random arc length along the first path.
    Path {
        table: PathArcTable,
        /// The path's own vertices, which the particle inherits from the
        /// nearest of.
        vertices: &'a [Vec2],
        first_vertex: usize,
        attributes: &'a AttributeSet,
    },
}

fn unit_vector(v: Vec2) -> Vec2 {
    let length = v.0.hypot(v.1);
    if length <= f32::EPSILON {
        Vec2(0.0, 0.0)
    } else {
        Vec2(v.0 / length, v.1 / length)
    }
}

impl<'a> Emitter<'a> {
    /// Reads the emitter slot. The second answer is `true` when the geometry
    /// is of a kind this node cannot sample (not a geometry, 3D positions, a
    /// mesh) — as opposed to merely having nothing in it.
    fn read(input: Option<&'a Arc<dyn NodeData>>, from: EmitFrom) -> (Self, bool) {
        let Some(input) = input else {
            return (Self::Origin, false);
        };
        let Some(geometry) = input.downcast_ref::<Geometry>() else {
            return (Self::Empty, true);
        };
        let has_path = geometry
            .primitives()
            .iter()
            .any(|primitive| matches!(primitive, Primitive::Path { .. }));
        let from = match from {
            EmitFrom::Auto if has_path => EmitFrom::Path,
            EmitFrom::Auto if geometry.point_count() > 0 => EmitFrom::Points,
            EmitFrom::Auto => EmitFrom::Instances,
            explicit => explicit,
        };
        match from {
            EmitFrom::Path => Self::path(geometry),
            EmitFrom::Points => Self::positions(geometry, Domain::Point),
            _ => Self::positions(geometry, Domain::Instance),
        }
    }

    fn positions(geometry: &'a Geometry, domain: Domain) -> (Self, bool) {
        let Some(positions) = geometry.positions(domain) else {
            return (Self::Empty, false);
        };
        let Ok(positions) = positions else {
            return (Self::Empty, true);
        };
        let Some(planar) = positions.planar() else {
            return (Self::Empty, true);
        };
        if planar.is_empty() {
            return (Self::Empty, false);
        }
        let center = bounds_center(geometry).map_or(Vec2(0.0, 0.0), |c| Vec2(c.0, c.1));
        (
            Self::Positions {
                positions: planar,
                attributes: geometry.attribute_set(domain),
                center,
            },
            false,
        )
    }

    fn path(geometry: &'a Geometry) -> (Self, bool) {
        let Some(Ok(positions)) = geometry.positions(Domain::Point) else {
            return (Self::Empty, geometry.point_count() > 0);
        };
        let Some(points) = positions.planar() else {
            return (Self::Empty, true);
        };
        let Ok(table) = PathArcTable::build(geometry, "particle.simulate") else {
            // No path, a mesh, or a path with no length.
            return (Self::Empty, !geometry.primitives().is_empty());
        };
        // `PathArcTable` samples the first primitive, which it has just
        // checked is a path; the vertices are the run it walked.
        let Some(Primitive::Path { verts, .. }) = geometry.primitives().first() else {
            return (Self::Empty, true);
        };
        let Some(vertices) = points.get(verts.clone()) else {
            return (Self::Empty, true);
        };
        (
            Self::Path {
                table,
                vertices,
                first_vertex: verts.start,
                attributes: geometry.points(),
            },
            false,
        )
    }

    /// Whether anything can be born.
    fn can_emit(&self) -> bool {
        !matches!(self, Self::Empty)
    }

    /// The attributes a particle inherits, indexed by [`Sample::row`].
    fn attributes(&self) -> Option<&AttributeSet> {
        match self {
            Self::Positions { attributes, .. } | Self::Path { attributes, .. } => Some(attributes),
            Self::Origin | Self::Empty => None,
        }
    }

    /// Where particle `id` is born, by the draws `k = 3` (the spot) of its
    /// own hash.
    fn sample(&self, seed: u32, id: i32) -> Sample {
        let draw = random(seed, id, 3);
        match self {
            Self::Origin | Self::Empty => Sample {
                position: Vec2(0.0, 0.0),
                normal: Vec2(0.0, 0.0),
                row: None,
            },
            Self::Positions {
                positions, center, ..
            } => {
                let row = ((draw * positions.len() as f32) as usize).min(positions.len() - 1);
                let position = positions[row];
                Sample {
                    position,
                    normal: unit_vector(Vec2(position.0 - center.0, position.1 - center.1)),
                    row: Some(row),
                }
            }
            Self::Path {
                table,
                vertices,
                first_vertex,
                ..
            } => {
                let spot = table.sample(draw * table.length());
                let nearest = vertices
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        let distance = |v: &Vec2| {
                            (v.0 - spot.position.0).powi(2) + (v.1 - spot.position.1).powi(2)
                        };
                        distance(a).total_cmp(&distance(b))
                    })
                    .map_or(0, |(index, _)| index);
                Sample {
                    position: spot.position,
                    normal: spot.normal,
                    row: Some(first_vertex + nearest),
                }
            }
        }
    }
}

// ===========================================================================
// Columns
// ===========================================================================

/// An attribute inherited from an emitter, carried beside the simulation
/// columns.
type Extra = (AttrName, AttributeArray);

/// The particle columns while a step builds them.
#[derive(Default)]
struct Columns {
    p: Vec<Vec2>,
    v: Vec<Vec2>,
    id: Vec<i32>,
    age: Vec<f32>,
    life: Vec<f32>,
    extra: Vec<Extra>,
}

impl Columns {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            p: Vec::with_capacity(capacity),
            v: Vec::with_capacity(capacity),
            id: Vec::with_capacity(capacity),
            age: Vec::with_capacity(capacity),
            life: Vec::with_capacity(capacity),
            extra: Vec::new(),
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
        for (name, column) in self.extra {
            set.insert(name, column).expect("extras match P");
        }
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

/// The rows `rows` of `column`, in that order.
fn gather(column: &AttributeArray, rows: &[usize]) -> AttributeArray {
    macro_rules! pick {
        ($variant:ident, $values:expr) => {
            AttributeArray::$variant(rows.iter().map(|&row| $values[row].clone()).collect())
        };
    }
    match column {
        AttributeArray::F32(values) => pick!(F32, values),
        AttributeArray::Vec2(values) => pick!(Vec2, values),
        AttributeArray::Vec3(values) => pick!(Vec3, values),
        AttributeArray::Vec4(values) => pick!(Vec4, values),
        AttributeArray::Color(values) => pick!(Color, values),
        AttributeArray::I32(values) => pick!(I32, values),
        AttributeArray::Bool(values) => pick!(Bool, values),
        AttributeArray::Str(values) => pick!(Str, values),
    }
}

/// `tail` after `column`. The two are of one type — the caller made them so.
fn append(column: &mut AttributeArray, tail: AttributeArray) {
    match (column, tail) {
        (AttributeArray::F32(a), AttributeArray::F32(b)) => a.extend(b),
        (AttributeArray::Vec2(a), AttributeArray::Vec2(b)) => a.extend(b),
        (AttributeArray::Vec3(a), AttributeArray::Vec3(b)) => a.extend(b),
        (AttributeArray::Vec4(a), AttributeArray::Vec4(b)) => a.extend(b),
        (AttributeArray::Color(a), AttributeArray::Color(b)) => a.extend(b),
        (AttributeArray::I32(a), AttributeArray::I32(b)) => a.extend(b),
        (AttributeArray::Bool(a), AttributeArray::Bool(b)) => a.extend(b),
        (AttributeArray::Str(a), AttributeArray::Str(b)) => a.extend(b),
        _ => debug_assert!(false, "append of mismatched attribute types"),
    }
}

/// `count` rows of what an absent `name` reads as on a point.
fn absent_rows(name: &str, like: &AttributeArray, count: usize) -> AttributeArray {
    absent_column(
        &Geometry::new(),
        Domain::Point,
        name,
        like.attr_type(),
        count,
    )
}

/// Adds `emitted` newborn rows to `extra`, after the `alive` that are there:
/// the emitter's own row where it has this attribute in this type, what an
/// absent attribute reads as everywhere else — so a column stays as long as
/// the particles of the frames that made it.
fn inherit(
    extra: &mut Vec<Extra>,
    alive: usize,
    emitted: usize,
    attributes: Option<&AttributeSet>,
    rows: &[usize],
) {
    let from_emitter = |name: &str, like: &AttributeArray| -> AttributeArray {
        attributes
            .and_then(|set| set.get(name))
            .filter(|column| column.attr_type() == like.attr_type())
            .map_or_else(
                || absent_rows(name, like, emitted),
                |column| gather(column, rows),
            )
    };
    for (name, column) in extra.iter_mut() {
        let tail = from_emitter(name, column);
        append(column, tail);
    }
    let Some(attributes) = attributes else {
        return;
    };
    for (name, column) in attributes.iter() {
        if SIMULATED.contains(&name.as_str()) || extra.iter().any(|(have, _)| have == name) {
            continue;
        }
        let mut born = absent_rows(name, column, alive);
        append(&mut born, gather(column, rows));
        extra.push((name.clone(), born));
    }
}

// ===========================================================================
// Random draws
// ===========================================================================

/// A uniform `[0, 1)` from a hash, 24 bits of it.
fn unit(hash: u32) -> f32 {
    (hash & 0x00FF_FFFF) as f32 / 0x0100_0000 as f32
}

/// The `k`-th independent random number of the particle `id`, in `[0, 1)`.
///
/// `k`: 0 life, 1 launch direction, 2 + 6 launch spread (radius, angle),
/// 3 where on the emitter, 4 + 5 position jitter (radius, angle).
fn random(seed: u32, id: i32, k: u32) -> f32 {
    unit(element_hash(element_hash(seed, id as u32), k))
}

/// A uniform point of the disc of `radius`, from the draws `radius_k` and
/// `angle_k`.
fn disc(seed: u32, id: i32, radius: f32, radius_k: u32, angle_k: u32) -> Vec2 {
    if radius <= 0.0 {
        return Vec2(0.0, 0.0);
    }
    let r = radius * random(seed, id, radius_k).sqrt();
    let angle = random(seed, id, angle_k) * TAU;
    Vec2(r * angle.cos(), r * angle.sin())
}

// ===========================================================================
// Forces
// ===========================================================================

/// The fields wired to the `force` group.
struct Forces<'a>(Vec<&'a FieldValue>);

impl<'a> Forces<'a> {
    /// The fields in the slots after the emitter's.
    fn read(inputs: &'a [Option<Arc<dyn NodeData>>]) -> Self {
        Self(
            inputs
                .iter()
                .skip(1)
                .filter_map(|slot| slot.as_ref()?.downcast_ref::<FieldValue>())
                .collect(),
        )
    }

    /// The acceleration of each of `live` (pixels per second squared), or
    /// `None` when nothing pushes. The second answer is `true` when a field
    /// answered something that is not an acceleration, which then counts as
    /// zero.
    fn acceleration(
        &self,
        live: &Live<'_>,
        attributes: &AttributeSet,
        direction: Vec2,
        ctx: &EvalContext,
    ) -> (Option<Vec<Vec2>>, bool) {
        if self.0.is_empty() {
            return (None, false);
        }
        let sample = FieldSample::new(live.p, attributes, ctx);
        let mut total = vec![Vec2(0.0, 0.0); live.p.len()];
        let mut unusable = false;
        for field in &self.0 {
            match field.sample(&sample) {
                AttributeArray::F32(values) if values.len() == total.len() => {
                    for (sum, magnitude) in total.iter_mut().zip(values) {
                        sum.0 += direction.0 * magnitude;
                        sum.1 += direction.1 * magnitude;
                    }
                }
                AttributeArray::Vec2(values) if values.len() == total.len() => {
                    for (sum, push) in total.iter_mut().zip(values) {
                        sum.0 += push.0;
                        sum.1 += push.1;
                    }
                }
                _ => unusable = true,
            }
        }
        (Some(total), unusable)
    }
}

// ===========================================================================
// The processor
// ===========================================================================

/// How many fresh ids remain after `next_id` (ids are `0..i32::MAX`).
fn ids_left(next_id: i32) -> usize {
    usize::try_from(i32::MAX - next_id.max(0)).unwrap_or(0)
}

/// What one emission pass did.
struct Emission {
    carry: f64,
    next_id: i32,
    capped: bool,
}

impl ParticleSimulate {
    /// The launch velocity of particle `id`.
    fn launch(config: &Config, id: i32, sample: &Sample) -> Vec2 {
        let speed = config.initial_velocity.0.hypot(config.initial_velocity.1);
        let random_direction = || {
            let angle = random(config.seed, id, 1) * TAU;
            Vec2(angle.cos(), angle.sin())
        };
        let direction = match config.launch {
            LaunchMode::Fixed => None,
            LaunchMode::Random => Some(random_direction()),
            LaunchMode::Normal if sample.normal == Vec2(0.0, 0.0) => Some(random_direction()),
            LaunchMode::Normal => Some(sample.normal),
        };
        let base = direction.map_or(config.initial_velocity, |d| Vec2(d.0 * speed, d.1 * speed));
        let spread = disc(config.seed, id, config.initial_spread, 2, 6);
        Vec2(base.0 + spread.0, base.1 + spread.1)
    }

    /// Emits this frame's particles after the `alive` already there, appending
    /// to `out`.
    fn emit(
        config: &Config,
        ctx: &EvalContext,
        emitter: &Emitter<'_>,
        alive: usize,
        carry: f64,
        next_id: i32,
        out: &mut Columns,
    ) -> Emission {
        if !emitter.can_emit() {
            // Nothing to be born at: the promise lapses rather than piling up
            // into a burst when the emitter returns.
            return Emission {
                carry: 0.0,
                next_id,
                capped: false,
            };
        }
        let wanted = carry + config.rate / ctx.fps.as_f64();
        let whole = wanted.floor();
        // Ids are never reused: a reused id would replay another particle's
        // random draws, so emission stops once they run out, like at `max_points`.
        let room = config
            .max_points
            .saturating_sub(alive)
            .min(ids_left(next_id));
        // `as usize` saturates, and `room` bounds the result.
        let count = (whole as usize).min(room);
        let capped = (whole as usize) > room;
        let mut next_id = next_id;
        let mut rows = Vec::new();
        for _ in 0..count {
            let id = next_id;
            next_id += 1;
            let sample = emitter.sample(config.seed, id);
            let life = (config.life
                + config.life_variance * (2.0 * random(config.seed, id, 0) - 1.0))
                .max(1.0);
            let jitter = disc(config.seed, id, config.jitter, 4, 5);
            out.p.push(Vec2(
                sample.position.0 + jitter.0,
                sample.position.1 + jitter.1,
            ));
            out.v.push(Self::launch(config, id, &sample));
            out.id.push(id);
            out.age.push(0.0);
            out.life.push(life);
            rows.extend(sample.row);
        }
        inherit(&mut out.extra, alive, count, emitter.attributes(), &rows);
        Emission {
            carry: wanted - whole,
            next_id,
            capped,
        }
    }

    fn finish(
        columns: Columns,
        emission: Emission,
        emitter_unusable: bool,
        force_unusable: bool,
    ) -> ParticleState {
        ParticleState {
            points: columns.into_geometry(),
            next_id: emission.next_id,
            carry: emission.carry,
            capped: emission.capped,
            emitter_unusable,
            force_unusable,
        }
    }
}

/// Raises the warnings of a step on the frame their condition begins, not on
/// each frame spent in it.
fn warn_on_entering(prev: Option<&ParticleState>, state: &ParticleState, config: &Config) {
    if state.capped && !prev.is_some_and(|prev| prev.capped) {
        tracing::warn!(
            max_points = config.max_points,
            "particle.simulate reached max_points (or used every particle id); emission is suppressed"
        );
    }
    if state.emitter_unusable && !prev.is_some_and(|prev| prev.emitter_unusable) {
        tracing::warn!(
            "particle.simulate cannot emit from this geometry (3D positions, a mesh, or not a geometry); nothing is emitted"
        );
    }
    if state.force_unusable && !prev.is_some_and(|prev| prev.force_unusable) {
        tracing::warn!(
            "particle.simulate: a force answers neither a scalar nor a Vec2 per particle; it is ignored"
        );
    }
}

impl StatefulProcessor for ParticleSimulate {
    type State = ParticleState;

    fn initial(&self, ctx: &EvalContext, inputs: &SimInputs<'_>) -> ParticleState {
        let config = Config::read(inputs.params);
        let (emitter, unusable) = Emitter::read(
            inputs.inputs.first().and_then(|slot| slot.as_ref()),
            config.emit_from,
        );
        let mut columns = Columns::default();
        let emission = Self::emit(&config, ctx, &emitter, 0, 0.0, 0, &mut columns);
        let state = Self::finish(columns, emission, unusable, false);
        warn_on_entering(None, &state, &config);
        state
    }

    fn step(
        &self,
        prev: &ParticleState,
        ctx: &EvalContext,
        inputs: &SimInputs<'_>,
    ) -> ParticleState {
        let config = Config::read(inputs.params);
        let (emitter, unusable) = Emitter::read(
            inputs.inputs.first().and_then(|slot| slot.as_ref()),
            config.emit_from,
        );
        let dt = (1.0 / ctx.fps.as_f64()) as f32;
        let damping = (config.drag * dt).min(1.0);

        let mut next = Columns::default();
        let mut force_unusable = false;
        if let Some(live) = Live::of(&prev.points) {
            let (acceleration, unusable) = Forces::read(inputs.inputs).acceleration(
                &live,
                prev.points.points(),
                config.force_direction,
                ctx,
            );
            force_unusable = unusable;
            next = Columns::with_capacity(live.p.len());
            let mut kept = Vec::with_capacity(live.p.len());
            for i in 0..live.p.len() {
                let age = live.age[i] + 1.0;
                if age >= live.life[i] {
                    continue;
                }
                kept.push(i);
                let (p, v) = (live.p[i], live.v[i]);
                let a = acceleration.as_ref().map_or(Vec2(0.0, 0.0), |a| a[i]);
                next.p.push(Vec2(p.0 + v.0 * dt, p.1 + v.1 * dt));
                next.v.push(Vec2(
                    v.0 + a.0 * dt - v.0 * damping,
                    v.1 + a.1 * dt - v.1 * damping,
                ));
                next.id.push(live.id[i]);
                next.age.push(age);
                next.life.push(live.life[i]);
            }
            next.extra = prev
                .points
                .points()
                .iter()
                .filter(|(name, _)| !SIMULATED.contains(&name.as_str()))
                .map(|(name, column)| (name.clone(), gather(column, &kept)))
                .collect();
        }
        let alive = next.len();
        let emission = Self::emit(
            &config,
            ctx,
            &emitter,
            alive,
            prev.carry,
            prev.next_id,
            &mut next,
        );
        let state = Self::finish(next, emission, unusable, force_unusable);
        warn_on_entering(Some(prev), &state, &config);
        state
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
    use ravel_core::geometry::{
        AttributeField, ComposeField, ConstantField, DirectionToField, FalloffField, FalloffShape,
        MultiplyField, NoiseField, Primitive,
    };
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::NodeId;
    use ravel_core::registry::NodeRegistry;
    use ravel_core::registry::builtin::register_builtins;
    use ravel_core::types::{Color, FrameRate, Vec3};
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
    fn ids_run_out_instead_of_wrapping() {
        assert_eq!(ids_left(0), i32::MAX as usize);
        assert_eq!(ids_left(i32::MAX - 3), 3);
        assert_eq!(ids_left(i32::MAX), 0);
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

    /// Emits a fixed value; stands in for the upstream nodes of a test.
    struct Stub(Arc<dyn NodeData>);

    impl ravel_core::eval::NodeProcessor for Stub {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn ravel_core::eval::EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(self.0.clone())
        }
    }

    /// The simulation with `emitter` wired into its first input.
    fn with_emitter(params: &[(&str, ParameterValue)], emitter: Geometry) -> (Graph, Evaluator) {
        use ravel_core::id::{EdgeId, InputPortIndex, OutputPortIndex};
        let source = NodeId::new(100);
        let graph = Graph::new()
            .add_node(sim_node(params))
            .unwrap()
            .add_node(
                Node::new(source, "test.source")
                    .with_output("out", ravel_core::id::DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                source,
                OutputPortIndex(0),
                NodeId::new(SIM),
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = evaluator();
        ev.register(source, Arc::new(Stub(Arc::new(emitter))));
        (graph, ev)
    }

    fn positions(geometry: &dyn NodeData) -> Vec<Vec2> {
        column(geometry, names::P)
            .as_vec2(names::P)
            .unwrap()
            .to_vec()
    }

    fn near(a: Vec2, b: Vec2) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    const STILL: [(&str, ParameterValue); 0] = [];

    fn emitting(extra: &[(&'static str, ParameterValue)]) -> Vec<(&'static str, ParameterValue)> {
        let mut params = vec![("rate", float(300.0)), ("life", float(1000.0))];
        params.extend_from_slice(extra);
        params
    }

    /// A closed square path `(0,0)..(100,100)`.
    fn square() -> Geometry {
        let mut geometry = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(100.0, 0.0),
            Vec2(100.0, 100.0),
            Vec2(0.0, 100.0),
        ]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        geometry
    }

    #[test]
    fn point_emitters_birth_particles_at_their_points() {
        let marks = vec![Vec2(10.0, 20.0), Vec2(-30.0, 5.0), Vec2(70.0, 70.0)];
        let (graph, mut ev) = with_emitter(&emitting(&STILL), Geometry::from_points(marks.clone()));
        let out = geometry_at(&mut ev, &graph, 3);

        let born = positions(&*out);
        assert_eq!(born.len(), 40);
        for position in &born {
            assert!(
                marks.iter().any(|mark| near(*mark, *position)),
                "{position:?}"
            );
        }
        // All three are used, or the sampling is not sampling.
        for mark in &marks {
            assert!(
                born.iter().any(|position| near(*mark, *position)),
                "{mark:?}"
            );
        }
    }

    #[test]
    fn path_emitters_birth_particles_on_the_outline() {
        let (graph, mut ev) = with_emitter(&emitting(&STILL), square());
        let out = geometry_at(&mut ev, &graph, 3);
        let on_outline = |p: &Vec2| {
            let (x, y) = (p.0, p.1);
            let inside = (-1e-3..=100.001).contains(&x) && (-1e-3..=100.001).contains(&y);
            let on_edge = [x, y, 100.0 - x, 100.0 - y].iter().any(|d| d.abs() < 1e-3);
            inside && on_edge
        };
        let born = positions(&*out);
        assert!(!born.is_empty());
        assert!(born.iter().all(on_outline), "{born:?}");
        // Spread over the outline, not stuck on one corner: all four edges.
        for edge in [
            |p: &Vec2| p.1.abs() < 1e-3,
            |p: &Vec2| (p.0 - 100.0).abs() < 1e-3,
            |p: &Vec2| (p.1 - 100.0).abs() < 1e-3,
            |p: &Vec2| p.0.abs() < 1e-3,
        ] {
            assert!(born.iter().any(edge));
        }
    }

    #[test]
    fn emit_from_points_uses_the_vertices_of_a_path_instead() {
        let params = emitting(&[("emit_from", ParameterValue::String("points".into()))]);
        let (graph, mut ev) = with_emitter(&params, square());
        let born = positions(&*geometry_at(&mut ev, &graph, 2));
        let corners = [
            Vec2(0.0, 0.0),
            Vec2(100.0, 0.0),
            Vec2(100.0, 100.0),
            Vec2(0.0, 100.0),
        ];
        assert!(
            born.iter().all(|p| corners.iter().any(|c| near(*c, *p))),
            "{born:?}"
        );
    }

    #[test]
    fn instance_emitters_birth_particles_at_their_positions() {
        let marks = vec![Vec2(15.0, 15.0), Vec2(60.0, -20.0)];
        let mut emitter = Geometry::new();
        emitter
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(marks.clone()))
            .unwrap();
        let (graph, mut ev) = with_emitter(&emitting(&STILL), emitter);
        let born = positions(&*geometry_at(&mut ev, &graph, 2));
        assert_eq!(born.len(), 30);
        for position in &born {
            assert!(
                marks.iter().any(|mark| near(*mark, *position)),
                "{position:?}"
            );
        }
    }

    #[test]
    fn emitter_attributes_are_inherited_for_the_whole_life() {
        let marks = vec![Vec2(0.0, 0.0), Vec2(50.0, 0.0), Vec2(100.0, 0.0)];
        let colors = [
            Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            Color {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 1.0,
            },
            Color {
                r: 0.0,
                g: 0.0,
                b: 1.0,
                a: 1.0,
            },
        ];
        let mut emitter = Geometry::from_points(marks.clone());
        emitter
            .points_mut()
            .insert(names::CD, AttributeArray::Color(colors.to_vec()))
            .unwrap();
        emitter
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![1.0, 2.0, 3.0]))
            .unwrap();
        // Short lives, so the columns are compacted many times over.
        let params = [("rate", float(90.0)), ("life", float(4.0))];
        let (graph, mut ev) = with_emitter(&params, emitter);
        for frame in 0..12 {
            let out = geometry_at(&mut ev, &graph, frame);
            let at = positions(&*out);
            let cd = column(&*out, names::CD)
                .as_color(names::CD)
                .unwrap()
                .to_vec();
            let pscale = column(&*out, names::PSCALE)
                .as_f32(names::PSCALE)
                .unwrap()
                .to_vec();
            assert_eq!(cd.len(), at.len());
            for ((position, color), size) in at.iter().zip(&cd).zip(&pscale) {
                let mark = marks
                    .iter()
                    .position(|mark| near(*mark, *position))
                    .unwrap();
                assert_eq!(*color, colors[mark], "frame {frame}");
                assert_eq!(*size, (mark + 1) as f32, "frame {frame}");
            }
        }
    }

    #[test]
    fn a_path_particle_inherits_from_the_nearest_vertex() {
        let corners = [
            Vec2(0.0, 0.0),
            Vec2(100.0, 0.0),
            Vec2(100.0, 100.0),
            Vec2(0.0, 100.0),
        ];
        let mut emitter = square();
        emitter
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![1.0, 2.0, 3.0, 4.0]))
            .unwrap();
        let (graph, mut ev) = with_emitter(&emitting(&STILL), emitter);
        let out = geometry_at(&mut ev, &graph, 0);
        let pscale = column(&*out, names::PSCALE)
            .as_f32(names::PSCALE)
            .unwrap()
            .to_vec();
        let mut seen = [false; 4];
        for (p, size) in positions(&*out).iter().zip(&pscale) {
            let nearest = (0..4)
                .min_by(|&a, &b| {
                    let d = |c: usize| (corners[c].0 - p.0).hypot(corners[c].1 - p.1);
                    d(a).total_cmp(&d(b))
                })
                .unwrap();
            assert_eq!(*size, (nearest + 1) as f32, "{p:?}");
            seen[nearest] = true;
        }
        assert!(
            seen.iter().all(|s| *s),
            "every corner should own some particles"
        );
    }

    #[test]
    fn an_empty_emitter_is_not_an_error_and_emits_nothing() {
        for emitter in [Geometry::new(), Geometry::from_points(Vec::new())] {
            let (graph, mut ev) = with_emitter(&emitting(&STILL), emitter);
            for frame in 0..4 {
                assert_eq!(count(&*geometry_at(&mut ev, &graph, frame)), 0);
            }
        }
    }

    #[test]
    fn a_three_dimensional_emitter_emits_nothing_and_warns_once() {
        let emitter = Geometry::from_points3(vec![Vec3(1.0, 2.0, 3.0)]);
        let (graph, mut ev) = with_emitter(&emitting(&STILL), emitter);
        let mut counts = Vec::new();
        let warnings = warnings_during(|| {
            for frame in 0..4 {
                counts.push(count(&*geometry_at(&mut ev, &graph, frame)));
            }
        });
        assert_eq!(counts, [0, 0, 0, 0]);
        assert_eq!(warnings, 1);
    }

    #[test]
    fn jitter_scatters_the_birth_position_within_its_radius() {
        let params = emitting(&[("jitter", float(10.0))]);
        let (graph, mut ev) = with_emitter(&params, Geometry::from_points(vec![Vec2(50.0, 50.0)]));
        let born = positions(&*geometry_at(&mut ev, &graph, 3));
        let distance = |p: &Vec2| (p.0 - 50.0).hypot(p.1 - 50.0);
        assert!(born.iter().all(|p| distance(p) <= 10.0 + 1e-3));
        assert!(
            born.iter().any(|p| distance(p) > 1.0),
            "no particle moved off the point"
        );
    }

    #[test]
    fn launch_modes_choose_the_direction() {
        let speed = 40.0;
        // Frame 0: the particles are where they were born, not yet moved.
        let launched = |mode: &str| {
            let params = emitting(&[
                ("velocity_mode", ParameterValue::String(mode.into())),
                ("initial_velocity", ParameterValue::vec2(speed, 0.0)),
            ]);
            let (graph, mut ev) = with_emitter(&params, square());
            let out = geometry_at(&mut ev, &graph, 0);
            let velocity = column(&*out, names::VELOCITY)
                .as_vec2(names::VELOCITY)
                .unwrap()
                .to_vec();
            positions(&*out)
                .into_iter()
                .zip(velocity)
                .collect::<Vec<_>>()
        };

        let fixed = launched("fixed");
        assert!(fixed.iter().all(|(_, v)| near(*v, Vec2(speed, 0.0))));

        let random = launched("random");
        assert!(
            random
                .iter()
                .all(|(_, v)| (v.0.hypot(v.1) - speed).abs() < 1e-3)
        );
        assert!(
            random.iter().any(|(_, v)| v.1.abs() > 1.0),
            "directions do not vary"
        );

        // Perpendicular to the edge each particle was born on: along y on the
        // top and bottom edges, along x on the left and right ones. Corners
        // belong to two edges and are skipped.
        let normal = launched("normal");
        let mut checked = 0;
        for (p, v) in &normal {
            assert!((v.0.hypot(v.1) - speed).abs() < 1e-3);
            let horizontal_edge = p.1.abs() < 1e-3 || (p.1 - 100.0).abs() < 1e-3;
            let vertical_edge = p.0.abs() < 1e-3 || (p.0 - 100.0).abs() < 1e-3;
            match (horizontal_edge, vertical_edge) {
                (true, false) => assert!(v.0.abs() < 1e-3 && v.1.abs() > 1.0, "{p:?} {v:?}"),
                (false, true) => assert!(v.1.abs() < 1e-3 && v.0.abs() > 1.0, "{p:?} {v:?}"),
                _ => continue,
            }
            checked += 1;
        }
        assert!(checked >= 5, "too few particles off the corners: {checked}");
    }

    /// The simulation with `forces` wired into its `force` group, and
    /// `emitter` into the first input when there is one.
    fn with_forces(
        params: &[(&str, ParameterValue)],
        emitter: Option<Geometry>,
        forces: Vec<FieldValue>,
    ) -> (Graph, Evaluator) {
        wired(evaluator(), params, emitter, forces)
    }

    /// [`with_forces`] on the evaluator `ev`, which has the simulation
    /// registered.
    fn wired(
        mut ev: Evaluator,
        params: &[(&str, ParameterValue)],
        emitter: Option<Geometry>,
        forces: Vec<FieldValue>,
    ) -> (Graph, Evaluator) {
        use ravel_core::id::{EdgeId, InputPortIndex, OutputPortIndex};
        let mut graph = Graph::new().add_node(sim_node(params)).unwrap();
        let sim = NodeId::new(SIM);
        let wire =
            |graph: Graph, ev: &mut Evaluator, id: u64, slot: u32, value: Arc<dyn NodeData>| {
                let source = NodeId::new(id);
                let graph = graph
                    .add_node(
                        Node::new(source, "test.source").with_output("out", value.data_type_id()),
                    )
                    .unwrap()
                    .add_edge(
                        EdgeId::new(id),
                        source,
                        OutputPortIndex(0),
                        sim,
                        InputPortIndex(slot),
                    )
                    .unwrap();
                ev.register(source, Arc::new(Stub(value)));
                graph
            };
        if let Some(emitter) = emitter {
            graph = wire(graph, &mut ev, 100, 0, Arc::new(emitter));
        }
        for (slot, field) in forces.into_iter().enumerate() {
            graph = wire(
                graph,
                &mut ev,
                200 + slot as u64,
                1 + slot as u32,
                Arc::new(field),
            );
            graph = graph.grow_variadic_input_group(sim).unwrap();
        }
        (graph, ev)
    }

    fn constant(value: f32) -> FieldValue {
        FieldValue::new(ConstantField(value))
    }

    fn velocities(geometry: &dyn NodeData) -> Vec<Vec2> {
        column(geometry, names::VELOCITY)
            .as_vec2(names::VELOCITY)
            .unwrap()
            .to_vec()
    }

    /// One particle per frame, born at the origin at rest, never expiring.
    fn one_per_frame(
        extra: &[(&'static str, ParameterValue)],
    ) -> Vec<(&'static str, ParameterValue)> {
        let mut params = vec![("rate", float(FPS as f32)), ("life", float(1e6))];
        params.extend_from_slice(extra);
        params
    }

    #[test]
    fn a_constant_field_down_is_free_fall() {
        let gravity = 980.0;
        let (graph, mut ev) = with_forces(&one_per_frame(&STILL), None, vec![constant(gravity)]);
        let frames = 30u64;
        let out = geometry_at(&mut ev, &graph, frames);
        // Particle 0 is first in the array and has had `frames` steps.
        let (y, vy) = (positions(&*out)[0].1, velocities(&*out)[0].1);
        let (n, dt) = (frames as f32, 1.0 / FPS as f32);
        let t = n * dt;

        // Exactly what forward Euler gives: v_n = g n dt, y_n = g dt^2 n(n-1)/2.
        assert!((vy - gravity * t).abs() < 1e-2, "{vy}");
        let discrete = gravity * dt * dt * n * (n - 1.0) / 2.0;
        assert!((y - discrete).abs() < 1e-2 * discrete, "{y} vs {discrete}");
        // And the analytic solution y = g t^2 / 2 within the Euler error bound
        // g t dt / 2, so an integrator that is wrong by more than the scheme
        // itself is caught.
        let analytic = 0.5 * gravity * t * t;
        assert!(
            (y - analytic).abs() <= gravity * t * dt / 2.0 * 1.01,
            "{y} vs {analytic}"
        );
        // Nothing pushed sideways.
        assert!(positions(&*out)[0].0.abs() < 1e-4);
    }

    #[test]
    fn force_direction_chooses_where_a_scalar_force_pushes() {
        let params = one_per_frame(&[("force_direction", ParameterValue::vec2(1.0, 0.0))]);
        let (graph, mut ev) = with_forces(&params, None, vec![constant(300.0)]);
        let out = geometry_at(&mut ev, &graph, 10);
        let v = velocities(&*out)[0];
        assert!((v.0 - 100.0).abs() < 1e-2 && v.1.abs() < 1e-4, "{v:?}");
    }

    #[test]
    fn an_attractor_gathers_particles_at_its_center() {
        // A ring of eight points at radius 80, a falloff centred on the origin
        // that reaches past it, and the unit direction toward the origin scaled
        // by the falloff and a strength. Drag takes the energy out so the
        // particles settle.
        let center = Vec2(0.0, 0.0);
        let ring: Vec<Vec2> = (0..8)
            .map(|k| {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                Vec2(80.0 * a.cos(), 80.0 * a.sin())
            })
            .collect();
        let pull = FieldValue::new(MultiplyField {
            left: FieldValue::new(MultiplyField {
                left: FieldValue::new(DirectionToField { target: center }),
                right: FieldValue::new(FalloffField {
                    center,
                    inner_radius: 0.0,
                    outer_radius: 200.0,
                    shape: FalloffShape::Sphere,
                }),
            }),
            right: constant(300.0),
        });
        // Eight particles at frame 0 (240 per second at 30 fps), then none.
        let params = [
            ("rate", float(240.0)),
            ("life", float(1e6)),
            ("drag", float(6.0)),
            ("max_points", ParameterValue::Int(8)),
        ];
        let (graph, mut ev) = with_forces(&params, Some(Geometry::from_points(ring)), vec![pull]);

        let distance = |frame: u64, ev: &mut Evaluator| -> Vec<f32> {
            positions(&*geometry_at(ev, &graph, frame))
                .iter()
                .map(|p| p.0.hypot(p.1))
                .collect()
        };
        let start = distance(0, &mut ev);
        let early = distance(6, &mut ev);
        let late = distance(150, &mut ev);

        assert_eq!(start.len(), 8);
        assert!(start.iter().all(|d| (d - 80.0).abs() < 1e-3));
        // Drawn in from the start, and settled around the center at the end.
        assert!(early.iter().zip(&start).all(|(e, s)| e < s), "{early:?}");
        assert!(late.iter().all(|d| *d < 5.0), "{late:?}");
    }

    #[test]
    fn a_noise_force_gives_the_same_trajectory_every_time() {
        let noise = |seed| {
            FieldValue::new(MultiplyField {
                left: FieldValue::new(NoiseField {
                    seed,
                    frequency: 0.05,
                    octaves: 2,
                }),
                right: constant(400.0),
            })
        };
        // Born at the origin, where noise is zero, so they are launched off it.
        let params = one_per_frame(&[
            ("force_direction", ParameterValue::vec2(1.0, 1.0)),
            ("initial_velocity", ParameterValue::vec2(20.0, 0.0)),
            ("initial_spread", float(30.0)),
        ]);
        let run = |seed: u32, frame: u64| {
            let (graph, mut ev) = with_forces(&params, None, vec![noise(seed)]);
            positions(&*geometry_at(&mut ev, &graph, frame))
        };
        let first = run(5, 40);
        assert_eq!(first, run(5, 40), "the same noise is the same path");
        assert_ne!(first, run(6, 40), "another noise seed is another path");

        // Scrubbing back over the track reads the same states.
        let (graph, mut ev) = with_forces(&params, None, vec![noise(5)]);
        geometry_at(&mut ev, &graph, 40);
        geometry_at(&mut ev, &graph, 7);
        assert_eq!(first, positions(&*geometry_at(&mut ev, &graph, 40)));

        // The force acts: the same launch without it is a straight line.
        let (graph, mut ev) = with_forces(&params, None, Vec::new());
        let straight = positions(&*geometry_at(&mut ev, &graph, 40));
        assert_ne!(first, straight);
    }

    #[test]
    fn a_force_sees_the_attributes_of_the_particles_it_pushes() {
        // `age` as the magnitude: before step k+1 the particle is k frames old,
        // so v_n = dt * (0 + 1 + ... + (n - 1)).
        let (graph, mut ev) = with_forces(
            &one_per_frame(&STILL),
            None,
            vec![FieldValue::new(AttributeField::new("age"))],
        );
        let n = 6u64;
        let v = velocities(&*geometry_at(&mut ev, &graph, n))[0];
        let expected = (n * (n - 1) / 2) as f32 / FPS as f32;
        assert!((v.1 - expected).abs() < 1e-4, "{v:?} vs {expected}");
    }

    #[test]
    fn forces_add_up() {
        let (graph, mut ev) = with_forces(
            &one_per_frame(&STILL),
            None,
            vec![constant(100.0), constant(50.0)],
        );
        let v = velocities(&*geometry_at(&mut ev, &graph, 6))[0];
        assert!((v.1 - 150.0 * 6.0 / FPS as f32).abs() < 1e-3, "{v:?}");
    }

    #[test]
    fn a_force_that_is_not_an_acceleration_is_ignored_with_one_warning() {
        let vec3 = FieldValue::new(ComposeField::new(vec![
            constant(1.0),
            constant(1.0),
            constant(1.0),
        ]));
        let (graph, mut ev) = with_forces(&one_per_frame(&STILL), None, vec![vec3]);
        let mut last = Vec::new();
        let warnings = warnings_during(|| {
            for frame in 0..5 {
                last = velocities(&*geometry_at(&mut ev, &graph, frame));
            }
        });
        assert_eq!(warnings, 1);
        assert!(last.iter().all(|v| *v == Vec2(0.0, 0.0)));
    }

    #[test]
    fn size_and_color_follow_the_life_of_a_particle_through_field_apply() {
        use ravel_core::geometry::{ExpressionField, FieldApply, apply_field};
        let params = [("rate", float(90.0)), ("life", float(10.0))];
        let (graph, mut ev) = with_forces(&params, None, Vec::new());
        let out = geometry_at(&mut ev, &graph, 12);
        let particles = out.downcast_ref::<Geometry>().unwrap();

        // `pscale = 4 * (1 - age / life)`: the particle shrinks as it ages.
        let shrink = ExpressionField::new("4 * (1 - @age / @life)", 0.0);
        let sized = apply_field(
            particles,
            &FieldApply::new(Domain::Point, names::PSCALE),
            &shrink,
            &ctx(12),
        )
        .unwrap();

        let age = column(&*out, names::AGE).as_f32(names::AGE).unwrap();
        let life = column(&*out, names::LIFE).as_f32(names::LIFE).unwrap();
        let pscale = sized
            .points()
            .get(names::PSCALE)
            .unwrap()
            .as_f32(names::PSCALE)
            .unwrap();
        assert!(
            age.len() > 10 && age.iter().any(|a| *a > 5.0),
            "ages must vary"
        );
        for ((age, life), size) in age.iter().zip(life).zip(pscale) {
            assert!(
                (size - 4.0 * (1.0 - age / life)).abs() < 1e-4,
                "{age} {life} {size}"
            );
        }
    }

    // ---- the simulation cache ------------------------------------------------

    /// A run that exercises everything the state carries: a path emitter with
    /// an inherited attribute, a noise force, drag, and lives short enough
    /// that columns are compacted all the while.
    fn busy(ev: Evaluator) -> (Graph, Evaluator) {
        let mut emitter = square();
        emitter
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![1.0, 2.0, 3.0, 4.0]))
            .unwrap();
        let params = [
            ("rate", float(150.0)),
            ("life", float(14.0)),
            ("life_variance", float(5.0)),
            ("drag", float(0.5)),
            ("seed", ParameterValue::Int(3)),
            ("velocity_mode", ParameterValue::String("normal".into())),
            ("initial_velocity", ParameterValue::vec2(25.0, 0.0)),
            ("initial_spread", float(10.0)),
            ("jitter", float(2.0)),
        ];
        let noise = FieldValue::new(MultiplyField {
            left: FieldValue::new(NoiseField {
                seed: 9,
                frequency: 0.03,
                octaves: 2,
            }),
            right: constant(300.0),
        });
        wired(ev, &params, Some(emitter), vec![noise])
    }

    /// Every column of a frame, so "identical" means all of it.
    fn snapshot(geometry: &dyn NodeData) -> Vec<(String, AttributeArray)> {
        let set = geometry.downcast_ref::<Geometry>().unwrap().points();
        let mut columns: Vec<_> = set
            .iter()
            .map(|(name, column)| (name.to_string(), (**column).clone()))
            .collect();
        columns.sort_by(|a, b| a.0.cmp(&b.0));
        columns
    }

    const FRAMES: u64 = 46;

    /// The states of frames `0..FRAMES`, read forward one frame at a time.
    fn played() -> Vec<Vec<(String, AttributeArray)>> {
        let (graph, mut ev) = busy(evaluator());
        (0..FRAMES)
            .map(|frame| snapshot(&*geometry_at(&mut ev, &graph, frame)))
            .collect()
    }

    #[test]
    fn rewinding_and_replaying_gives_the_same_states() {
        let forward = played();
        assert!(
            forward
                .iter()
                .any(|columns| columns.iter().any(|(_, column)| column.len() > 10)),
            "the run is too quiet to prove anything"
        );

        let (graph, mut ev) = busy(evaluator());
        let at = |ev: &mut Evaluator, frame: u64| snapshot(&*geometry_at(ev, &graph, frame));
        for frame in 0..FRAMES {
            at(&mut ev, frame);
        }
        // Back to the middle and over the same frames again.
        for frame in (12..FRAMES).rev().step_by(5) {
            assert_eq!(
                at(&mut ev, frame),
                forward[frame as usize],
                "rewind to {frame}"
            );
        }
        for frame in 12..FRAMES {
            assert_eq!(
                at(&mut ev, frame),
                forward[frame as usize],
                "replay of {frame}"
            );
        }

        // And by another route altogether: a fresh evaluator jumping straight
        // to the last frame, then back to an early one.
        let (graph, mut fresh) = busy(evaluator());
        let at = |ev: &mut Evaluator, frame: u64| snapshot(&*geometry_at(ev, &graph, frame));
        assert_eq!(at(&mut fresh, FRAMES - 1), forward[FRAMES as usize - 1]);
        assert_eq!(at(&mut fresh, 3), forward[3]);
    }

    #[test]
    fn a_thinned_track_replays_the_frames_it_dropped() {
        use ravel_core::cache_budget::{CacheBudgetConfig, SharedCacheBudget, Tier};
        let forward = played();
        // A sim share that holds a handful of states, not forty-six.
        let budget = SharedCacheBudget::new(CacheBudgetConfig {
            vram_bytes: 1 << 20,
            ram_bytes: 200_000,
            disk_bytes: 0,
            sim_reserve_ratio: 0.25,
        });
        let mut budgeted = Evaluator::with_budget(budget.clone());
        budgeted.register(
            NodeId::new(SIM),
            ravel_core::eval::stateful_processor(ParticleSimulate),
        );
        let (graph, mut ev) = busy(budgeted);
        let counts: Vec<u64> = (0..FRAMES)
            .map(|frame| snapshot(&*geometry_at(&mut ev, &graph, frame))[0].1.len() as u64)
            .collect();

        let stats = budget.stats();
        assert!(
            stats.sim_used[Tier::Ram as usize] <= stats.sim_reserved[Tier::Ram as usize],
            "the track stays inside its share: {stats:?}"
        );
        // Thinned: well under the cost of keeping every frame.
        let everything: u64 = counts.iter().map(|count| count * 32).sum();
        assert!(
            stats.sim_used[Tier::Ram as usize] < everything / 2,
            "the budget was never tight enough to thin the track: {} vs {everything}",
            stats.sim_used[Tier::Ram as usize]
        );
        // Charged for what it holds, so a state is never free: at least the
        // columns of the newest frame.
        assert!(stats.sim_used[Tier::Ram as usize] >= counts[FRAMES as usize - 1] * 32);

        // Every frame, including the ones thinning dropped, replays to the
        // state it had.
        for frame in (0..FRAMES).rev() {
            assert_eq!(
                snapshot(&*geometry_at(&mut ev, &graph, frame)),
                forward[frame as usize],
                "frame {frame} after thinning"
            );
        }
    }

    #[test]
    fn a_forward_jump_is_provisional_and_then_final() {
        let forward = played();
        let (graph, mut ev) = busy(evaluator());
        ev.set_sim_provisional(Some(5));

        // The first pull fills frame 0, and is not provisional.
        let first = snapshot(&*geometry_at(&mut ev, &graph, 0));
        assert_eq!(first, forward[0]);
        assert!(!ev.has_provisional_sim());

        // A long jump answers with the last state held, without stepping.
        let shown = snapshot(&*geometry_at(&mut ev, &graph, FRAMES - 1));
        assert!(ev.has_provisional_sim());
        assert_eq!(shown, forward[0]);
        assert_ne!(shown, forward[FRAMES as usize - 1]);

        // Settled, the same pull fills the track and gives the final state.
        ev.settle_provisional();
        ev.set_sim_provisional(None);
        let settled = snapshot(&*geometry_at(&mut ev, &graph, FRAMES - 1));
        assert!(!ev.has_provisional_sim());
        assert_eq!(settled, forward[FRAMES as usize - 1]);
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
