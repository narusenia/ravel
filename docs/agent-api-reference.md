# Ravel API reference for coding agents

A compact map of the public API surface an agent needs when extending Ravel.
Code is authoritative; when this document and the code disagree, trust the
code and fix this file in the same change. Paths are workspace-relative.

## Cross-cutting conventions

- **Immutability**: `Graph` mutations return a new `Graph` (`im` + `Arc`
  structural sharing). Never mutate a graph in place.
- **Undo**: `UndoStack<T>` snapshots whole states. Graph/composition edits
  must stay atomic per snapshot.
- **Data flow**: everything crossing a node port implements `NodeData` and is
  matched by `DataTypeId`.
- **Compositing**: FrameBuffers are straight (unpremultiplied) alpha, RGBA
  f32. Porter-Duff over divides by out-alpha (see `merge.wgsl`, rasterize).
- **Time**: animation keyframes live in layer-local frames; the network
  boundary node (`comp.network`) rewrites the `EvalContext` to
  `comp_frame - start_frame + in_frame`. UI or shell processors that
  evaluate channels directly must convert comp frame → layer-local first.
- **i18n**: user-visible text goes through `t!` / `ravel_i18n::translate`
  with entries in `assets/locales/{en,ja}.toml`. Headless layers emit locale
  keys (e.g. `properties.section.*`), the GPUI layer translates at render.
- **Verification**: `mise run check` = fmt + pattern lint + clippy
  (`-D warnings`) + workspace tests. `scripts/review-gate.sh --mark` records
  the pre-PR review marker (required by the `gh pr create` hook).

## ravel-core

### `id` — typed identifiers

```rust
NodeId / EdgeId / CompId / LayerId   // u64 newtypes; ::new(raw), ::next(), .raw()
DataTypeId(u32)                       // port type tag; ::new(raw), .raw()
InputPortIndex(pub u32) / OutputPortIndex(pub u32)
```

Well-known `DataTypeId` constants: `FRAME_BUFFER=1`, `SCALAR=10`, `VEC2=11`,
`VEC3=12`, `VEC4=13`, `COLOR=14`, `TIME_CODE=20`, `AUDIO_BUFFER=30`,
`PLAIN_TEXT=40`, `GEOMETRY=50`, `FIELD=51`, `SCENE=52`.

### `types` — data types and category traits

```rust
trait NodeData: Send + Sync + 'static {
    fn data_type_id(&self) -> DataTypeId;
    fn as_any(&self) -> &dyn Any;
    fn is_gpu_resident(&self) -> bool { false }  // true for ravel-gpu's GpuFrameBuffer
    fn byte_size(&self) -> u64;                  // NO default — see below
}
// dyn NodeData::downcast_ref::<T>() for concrete access.

trait BufferData: NodeData    { width/height/pixel_format }
trait TemporalData: NodeData  { duration/frame_rate }
trait GeometricData: NodeData { bounds() -> Rect; transform() -> Transform2D }
trait NumericData: NodeData   { components() -> usize }
```

Concrete types: `FrameBuffer { width, height, format: PixelFormat, data:
Arc<[u8]> }` (row-major RGBA bytes; `PixelFormat::{RgbaF32, RgbaF16, Rgba8,
MonoF32}`. Read pixels through `fb.as_f32() -> Cow<[f32]>` — borrowed for
float formats, expanded for reduced precision; direct `.data[...]` indexing
is lint-banned. **Code that indexes four channels per pixel (compositing,
GPU/encoder upload) uses `fb.as_rgba_f32() -> Result<Cow<[f32]>,
FrameFormatError>`**, which refuses a single-channel buffer and a length that
disagrees with `width * height * 4`. Constructors: `FrameBuffer::new_zeroed(w, h)` (RgbaF32),
`FrameBuffer::from_f32(w, h, Vec<f32>)`, `FrameBuffer::with_format(w, h,
fmt)`), `Scalar(f32)`, `Vec2(f32, f32)`,
`Vec3`, `Vec4`, `Color { r, g, b, a }` (`Color::new`, `Color::WHITE`),
`Rect { x, y, width, height }`, `Transform2D { m: [f32; 6] }`
(`Transform2D::IDENTITY`), `FrameRate::new(num, den)`.

`byte_size()` is the cache budget's accounting unit and deliberately has
**no default implementation**: a default `0` would silently under-account a
new type and the symptom (a budget that never evicts) is invisible, so a
missing implementation is a compile error. It counts the heap (or VRAM)
behind the handle — `FrameBuffer` returns `data.len()`, `PortRecord` sums its
children, `Geometry` sums its attribute columns and instance sources,
`GpuFrameBuffer` reads its texture key. Approximate is fine; the order of
magnitude is not.

### `cache_budget` — the single memory authority (`CACHE-3`)

```rust
enum Tier { Vram, Ram, Disk }                     // Tier::ALL is the array order
enum CacheKind { NodeResult(Tier), Frame(Tier), MediaFrame, Sim }
CacheBudgetConfig { vram_bytes, ram_bytes, disk_bytes, sim_reserve_ratio }
    // Default = 1 GiB VRAM / 2 GiB RAM / 4 GiB disk / 25% sim reserve.
    // CacheBudgetConfig::DEFAULT_* are the canonical constants; the
    // settings layer resolves onto them instead of restating numbers.

SharedCacheBudget::new(config)                    // the ONLY public constructor
    .reserve(kind, bytes)             -> (Reservation, Vec<Evicted>)
    .reserve_speculative(kind, bytes) -> (Reservation, Vec<Evicted>)  // CACHE-9 read-ahead
    .headroom(Tier) -> u64            // tier limit minus what is held
    .touch(ReservationId)             // a hit: keeps eviction least-recently-*used*
    .stats() -> CacheStats            // limits / used / sim_used / sim_reserved / entries
    .reconfigure(config)              // a settings change; live claims are untouched
```

A `Reservation` releases its bytes on drop, so a cache entry that owns one
cannot leak accounting through `remove` / `retain` / `clear`. Over the limit
`reserve` returns the entries to drop, ordered `speculative → ordinary
(least recently used)`, and **never a `CacheKind::Sim` reservation under
ordinary pressure** — a share of each tier is held back for simulation state,
whose re-computation is `O(frames)` rather than one node. Protection is not
exemption: once sim alone exceeds the tier total, sim is trimmed by sim,
least recently used first.

**Acting on the returned list is mandatory.** The budget releases an evicted
entry's bytes before returning it, so a consumer that does not drop the value
leaves the budget counting *fewer* bytes than the process holds — the limit
stops being a limit. Unreachable today (the evaluator's cache is the only
`reserve` caller; `TexturePool` only reads `headroom`), and first reachable
in `CACHE-5` / `CACHE-8`.

There is no `Default` and no public `CacheBudget::new`: a second budget is a
second authority. The application builds one in `ProjectState::new` and hands
it to both `GpuEvalHooks::with_budget` and `EvalService::spawn_with_budget`.
A structural resync must go through `Evaluator::reset()`, never
`*evaluator = Evaluator::new()` — the latter would drop the budget, and
`ProcessorSync` exists so it cannot be written.

**Lock order is pool → budget.** `TexturePool::release` reads the budget while
the pool is locked; nothing may call into the pool while holding the budget.

### `graph` — immutable DAG

```rust
Node::new(id, type_key)
    .with_input(name, &[DataTypeId]) .with_output(name, DataTypeId)
    .with_param(key, ParameterValue) .with_label(..) .with_position(x, y)
    .with_subnet(Graph)     // subnet node: owns a nested graph (REQ-LAYER-003)
node.subnet: Option<Arc<Graph>>   // None for non-subnet nodes
ParameterValue::{Float, Int, Bool, String, Channel..Channel4,
    PathPoints(Vec<PathPoint>),   // PathPoint { p, in_tan, out_tan } (pen, REQ-UI-011)
    Curve(CurveParam)}            // scalar transfer curve (see `param_curve`)
    // PathPoints and Curve are appended LAST on purpose: bincode indexes
    // variants positionally, so a new one may only go at the end, and the
    // layout change is covered by a JOURNAL_FORMAT_VERSION bump.
ParameterValue::vec2(x, y) / ::vec3(x, y, z)   // constant vector parameters
    // Geometric vectors are ONE Channel2/Channel3, never a `_x` / `_y` pair of
    // Floats: `shape.*` `center`, `shape.ellipse` `radius`, `scatter.grid`
    // `spacing`, `geometry.transform` `translate` / `rotation` (Euler degrees,
    // Z is the 2D angle) / `scale` / `pivot`, `transform` `translate`,
    // `field.falloff` `center` / `direction`, `scatter.scatter` `area`. Read
    // with `params.vec2_or(key, ..)` / `vec3_or`. `attribute.set`'s `value`
    // is one parameter whose arity follows its `type`. Int pairs
    // (`scatter.grid` `count_x` / `count_y`) stay separate.
ParameterValue::channels() -> Option<Vec<AnimationChannel>>   // 1..=4 components
ParameterValue::from_channels(Vec<AnimationChannel>)          // None outside 1..=4

Graph::new()
    .add_node(Node) -> Result<Graph, GraphError>      // consumes self
    .add_edge(..) / .remove_node(id) / .remove_edge(id)
    .expose_param_port(node_id, key)   // parameter → is_param InputPort (appended)
    .remove_param_port(node_id, key)   // atomic: drops edges + re-indexes later ports
    .remove_output_port(node_id, OutputPortIndex)     // the output-side mirror
    .insert_output_port(node_id, index, OutputPort)   // append = index == outputs.len()
    .remove_input_port(node_id, InputPortIndex)       // index-keyed sibling of
    .insert_input_port(node_id, index, InputPort)     // remove_param_port
    // For input ports that are neither exposed parameters nor variadic slots —
    // `net.out`'s custom ports. Raw with respect to the input-list conventions
    // (variadic contiguity, parameter ports last), like `reorder_ports`.
    // Output ports are addressed from THREE places: `Edge::source_port`, the
    // `ChannelSource::NodeOutput(node, port)` bindings of node PARAMETERS, and
    // the same bindings on LAYER SHELL channels (transform / opacity / audio
    // gain — see `composition::remap_layer_channel_node_outputs`). These calls
    // follow the first two; a removed port's edge is deleted and a binding to
    // it collapses to `ChannelSource::Constant` (an irreversible loss, undone
    // by the caller's Document commit). Shell channels they do NOT follow —
    // `Graph` cannot see `Composition`, so that is the Document layer's job.
    // Harmless today (`AnimationChannel::evaluate` treats `NodeOutput` as a
    // placeholder), real once shell channels resolve against a graph.
    // Never edit `node.outputs` through `replace_node` — that leaves every one
    // of the three silently pointing one slot off.
    .rename_port(node_id, PortSide, old_name, new_name)
    // Edges are index-addressed, so a rename moves nothing; the point is the
    // NAME pairing, and only TWO mechanisms create one: an `is_param` input
    // port (`expose_param_port`) and a `net.in` output port's same-named
    // parameter fallback (REQ-LAYER-002). Those rename the parameter too.
    // A coincidental match is NOT a pairing and is left alone — `constant`
    // has an output `value` and a parameter `value` that its processor reads
    // by literal key.
    .reorder_ports(node_id, PortSide, &[String])   // the new order, by port name
    // Must be a permutation of the side's current names. Raw on inputs: the
    // variadic group's contiguity is the caller's to preserve.
    .set_params(node_id, &[Parameter]) // set values + follow their port types
    // A parameter whose ACCEPTANCE SET changes cannot keep its exposed port:
    // the port is re-created with the new set and its incoming edges are
    // dropped (a Scalar source cannot drive a VEC3 port). An unchanged set
    // keeps the port and its edges, so `vec4` <-> `color` costs nothing.
    // One call = one consistent graph, so the caller's
    // Document commit stays one undo step. Pair it with
    // `registry::builtin::dependent_param_updates(node, &changed)`, which
    // returns the updates a change forces — today only `attribute.set`'s
    // `value`, reshaped when its `type` changes.
graph.replace_node(Arc<Node>) -> Graph                // parameter edits
node.param_port_index(key) / node.supports_param_ports()
PortSide::{Input, Output}   // which port list a name-keyed edit means
node.is_bypassable()   // EVERY output port has a type-matching non-param input
    // NodeMetadata.bypassed (serde(default), persisted): evaluator pass-through
param_value.port_data_type()       // PRINCIPAL type: port colour, nominal type
    // Float/Int/Bool/Channel→SCALAR, Channel2→VEC2, Channel3→VEC3,
    // Channel4→COLOR; String/PathPoints/Curve→None
param_value.port_accepted_types()  // ACCEPTANCE set, principal type first
    // Same as above except Channel4→[COLOR, VEC4]: the two are readings
    // of the same four floats, so `vector.construct.vec4` can drive a
    // 4-component parameter. Use this to decide whether a connection is
    // legal and whether a value change invalidates a port; use
    // `port_data_type()` only where one type has to stand for the value.
    // `expose_param_port` writes this set into `InputPort.accepted_types`,
    // and load-time `normalize_param_ports` re-derives it so an older
    // project accepts what an identical new one does.
graph.node(id) / .nodes() / .edges() / .inputs_of(id) / .outputs_of(id)
graph.topological_sort() -> Result<Vec<NodeId>, GraphError>
node.parameter_sources() -> Vec<(NodeId, OutputPortIndex)>
    // The `ChannelSource::NodeOutput` pulls a node's parameters make. Real
    // dependencies that the edge list does NOT carry — include them in any
    // "what does a change here reach" walk.
graph.downstream_adjacency() -> HashMap<NodeId, Vec<NodeId>>
    // One pass, spanning edges AND parameter_sources. For flooding from
    // several seeds without re-walking the graph per seed.
graph.ptr_eq(&other) -> bool          // O(1) structural-sharing check
    // true PROVES identical content (same persistent-map roots), so a
    // derived index may be reused; false is inconclusive. Lets a caller
    // cache a graph-derived index across frames without hashing.
// Graph is serde-capable: id-sorted {nodes, edges} lists, re-validated
// through Graph::from_parts on load (nested subnet graphs included).
```

### `eval` — Hybrid Pull + Dirty Notification (scoped, REQ-LAYER-007)

```rust
EvalContext::new(frame: u64, fps: FrameRate, resolution: (u32, u32))
    // fields: ctx.frame, ctx.time, ctx.fps, ctx.resolution,
    //         ctx.comp_resolution, ctx.min_precision
    .with_comp_resolution((u32, u32))   // geometry coordinate basis
    .with_min_precision(Precision)      // lowest storage precision accepted
ctx.sample_frame() -> f64               // continuous comp frame position

TimeKey::from_frame_position(frames: f64) -> TimeKey  // the only rounding site
TimeKey::SUBFRAME_SCALE: f64 = 4096.0   // ticks per frame
TimeKey::TIMELESS                       // key of a time-independent value
enum Precision { U8, F16, F32 }         // ordered; F32 is the default

trait NodeProcessor: Send + Sync {
    fn process(
        &self,
        node: &Node,                              // ports/metadata/type_key
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],     // per-input-port slots; None = unconnected
        params: &ResolvedParams,                  // per-frame values
            // f32_or / i32_or / bool_or / str_or / vec2_or / vec3_or / vec4_or,
            // plus path_points(key) and curve(key) for the structural kinds
            // (ResolvedValue::{PathPoints, Curve} pass through unresolved)
        scope: &mut dyn EvalScope,                // nested evaluation / document access
    ) -> anyhow::Result<Arc<dyn NodeData>>;

    fn is_time_dependent(&self) -> bool { false }
    fn rebuild_on_node_change(&self) -> bool { true }  // false = construction
        // captured nothing off the node, so a node change only needs
        // Evaluator::invalidate_node (the GPU processors override it)
}

trait EvalScope {                                 // implemented by Evaluator
    fn evaluate_sub(&mut self, segment: PathSegment, graph: &Graph,
        output: NodeId, ctx: &EvalContext,
        bindings: Vec<(String, Arc<dyn NodeData>)>) -> Result<Arc<dyn NodeData>, EvalError>;
    fn bindings(&self) -> &[(String, Arc<dyn NodeData>)];
    fn document(&self) -> Option<Arc<Document>>;
    fn path(&self) -> &[PathSegment];   // current ownership path (layer.ref
                                        // finds its enclosing layer here)
}

enum PathSegment { Layer(CompId, LayerId), Subnet(NodeId), Comp(CompId) }

Evaluator::new()                                // unbounded cache (tests, examples)
Evaluator::with_budget(SharedCacheBudget)       // cache bounded + LRU-evicted
    .reset()                                    // drop all state, KEEP the budget
                                                // (what a structural resync uses)
    .cache_stats() -> EvalCacheStats            // hits, misses_by_reason, entries,
                                                // bytes_by_tier; readable in release
    .reset_cache_stats()                        // "measure from here"
trait ProcessorRegistry { register / processor / invalidate_node }
    // implemented by Evaluator and by runtime::ProcessorSync, so
    // registration helpers take `&mut impl ProcessorRegistry`
    .register(node_id, Arc<dyn NodeProcessor>)  // also invalidates the node
    .processor(node_id) -> Option<&Arc<dyn NodeProcessor>>
    .invalidate_node(node_id)                   // register's invalidation alone,
                                                // keeping the registration
    .evaluate(&graph, node_id, &ctx) -> Result<..>    // pulls upstream only
    .evaluate_at(&[segments], &graph, node_id, &ctx)  // seeded ownership path
    .mark_dirty(&graph, node_id) / .mark_dirty_at(&graph, &[segments], node_id)
    .is_dirty(id) / .invalidate_all() / .invalidate_scope(&[segments])
    .set_document(Arc<Document>)                // required by comp.network / Layer Ref
    .take_timings() -> Vec<(NodeId, Duration)>  // process() durations of the last pull
```

Evaluation rejects a pull branch deeper than `MAX_EVALUATION_DEPTH` (256) with
`EvalError::DepthLimitExceeded`; persisted documents reject subnet nesting
deeper than `MAX_SUBNET_DEPTH` (64) before recursive load normalization.

Cache/dirty are keyed by ownership path + NodeId; animated (keyframed or
node-output-bound) parameters make a node time-varying automatically.
Multi-output nodes yield a `PortRecord` indexed by the edge's `source_port`.

Cached values, dirty flags, per-tier byte totals and a `NodeId → paths`
reverse index live behind one private module, so no code path can update one
and forget another. The index is what makes `register()` / `invalidate_node`
cost the node's own paths instead of a walk over the whole cache
(MED-CORE-07). With a budget attached, each entry holds a `Reservation`;
exceeding a tier evicts least-recently-used first, GPU-resident values being
charged to `Tier::Vram` and everything else to `Tier::Ram`. Without one the
cache is unbounded, exactly as before `CACHE-3`.

`cache_stats()` counts every node pull exactly once, hits and misses alike,
with the miss classified by `CacheMiss` (`dirty`, `input_fresh`,
`params_fresh`, `bypass_toggled`, `resolution_changed`, `fps_changed`,
`frame_advanced`, `precision_insufficient`, `bindings_changed`, `no_entry`).
It is compiled into release builds so a "the cache stopped working"
regression can be asserted in CI rather than timed.

A cached value additionally carries the identity it is specific to: the
quantised position (`TimeKey`, `TimeKey::TIMELESS` for a time-independent
node), `resolution`, `comp_resolution`, `fps`, `min_precision` and the bypass
flag. Every axis is matched by equality except precision, which is matched by
order — a stored value at or above the requested floor is served verbatim,
never converted, and a lower one misses with `precision_insufficient`. Two
positions inside one 1/4096-frame tick are the same request, so sub-frame
pulls within a frame (motion blur, time remapping) re-evaluate while integer
frame stepping behaves exactly as before. Constant parameters are cloned only
when the node is actually processed; a cache hit resolves nothing but the
channel-backed parameters it needs to detect a fresh source.

Scope bindings are *not* part of that identity — they are values, not
context. A scope re-entered with a different `Arc` bound to a name drops only
the cached values that name's interface output port feeds (the reach is
computed once per scope and reused while the network's `Graph` is the same
object), and the interface node recomputes with `bindings_changed`, reporting
freshness **per output port**. An adjustment layer's re-composited `source`
therefore no longer invalidates the layer's static generators or the
consumers of `t` / `base_geometry`. A bound name that matches no interface
output port cannot be traced and conservatively drops the whole scope.

Bypass (`NodeMetadata.bypassed`): the evaluator skips `process` and yields,
per output port, the value of the first connected non-param input port that
accepts the port's data type — single-output nodes yield it directly,
multi-output nodes a `PortRecord` in output-port order. Only the used inputs
are pulled (their freshness drives cache validity); unused inputs, parameter
resolution, and the processor are never touched, so a failing unused input
or NodeOutput-bound parameter source cannot fail the bypass. A node with no
type-matching connected input for some output port is processed normally —
bypass is ignored, never an error (`is_bypassable` therefore requires every
output port to match). The flag is part of cache validity, so toggling it
(a `Graph::replace_node` metadata edit) recomputes the node even without
explicit invalidation.

### `animation`

```rust
KeyframeCurve::new(); curve.insert(frame, value, Interpolation::Linear);
curve.sample(frame: f64) -> f32   // keyframes sit on integer frames,
    // sampling is continuous: sub-frame contexts (motion blur, time
    // remapping) interpolate instead of repeating the frame's value.
    // Step segments are half-open: [left.frame, right.frame).
AnimationChannel::keyframes(curve) | ChannelSource::Constant(v)
channel.evaluate(frame: f64, &ctx) -> f32   // frame is layer-local
    // Derive it with ctx.sample_frame() (continuous comp frame) and
    // Layer::local_frame_continuous; the u64 Layer::local_frame stays for
    // keyframe display and keyframe writes.
// ChannelSource::{Expression, AudioReactive} are placeholders.
// ChannelSource::NodeOutput(node, port) resolves inside the evaluator
// (parameter bindings only, same graph/scope).
// ParameterValue::{Channel, Channel2, Channel3, Channel4} put channels on
// node parameters (REQ-LAYER-004).
```

### `param_curve` — scalar transfer curves

```rust
CurveParam::identity()                      // 0→0, 1→1, linear (the default)
CurveParam::linear([(x, y), ..])            // linear points, sorted on the way in
CurveParam::from_points([CurvePoint, ..])   // full control, sorted on the way in
CurvePoint::new(x, y, Interpolation).with_tangents(in, out)
curve.evaluate(x: f32) -> f32
curve.points() / .len() / .is_empty()
curve.insert_point(CurvePoint) / .remove_point(x) / .move_point(from_x, to_x, y)
```

Same interpolation modes, tangent convention and segment rules as
`KeyframeCurve` (both go through `animation::interpolation::{linear_at,
bezier_at}`); the axis is an arbitrary `f32` input rather than an integer
frame. **Out of `[first.x, last.x]` the curve clamps** to the end outputs, and
an **empty curve is the identity** (`evaluate(x) == x`) so a remap with no
points cannot erase its input. Repeat / extrapolate belong to the node that
reads the curve, not to the type.

Points are **sorted by input, unique, and finite** — `evaluate` and the CRUD
methods binary-search them. Every entry point enforces that, **including
`Deserialize`**, which is hand-written for exactly this reason: it drops
non-finite points (`CurvePoint::is_finite`), sorts the rest, and collapses a
repeated input to the **last** point, so a hand-edited `.ravprj` yields a
defined curve instead of silently wrong samples. `insert_point` and the v5 → v6
upgrade collapse a repeat the same way.

Consumed through `ParameterValue::Curve` (`field.curve_remap` today) and read
in a processor with `params.curve(key)`. Properties renders it as a
`PropertyField::Curve` row — a thumbnail that expands
`widgets::param_curve_editor` inline.

### `exposed` — Exposed parameter declarations (REQ-PROJ-006)

`ravel_core::exposed`. A project's **external parameter contract**: what a CLI
render, a template instantiation or a network interface may set by name,
without knowing the network behind it. Persisted on `Document`
(`.ravprj` v7).

```rust
ExposedParameter::new(name, ExposedType, ExposedValue, ExposedBinding)
    -> Result<_, ExposedParameterError>          // checks the default's type
ExposedParameter::inferred(name, ExposedValue, ExposedBinding)  // type from value
    .with_description(text)
declaration.name() / .value_type() / .default_value() / .description() / .binding()

ExposedBinding::new(NodeId, key)                 // node id + parameter key
ExposedType::{Float, Int, Bool, String, Vec2, Vec3, Vec4, Color, Media}
ExposedValue::{Float, Int, Bool, String, Vec2, Vec3, Vec4, Color, Media(AssetPath)}

ExposedParameters::{new, from_declarations([..]), insert(decl), get(name),
                    contains(name), iter, len, is_empty}
```

Three invariants, enforced by the constructors **and** by `Deserialize`: names
are unique (and stored trimmed, so whitespace cannot split one contract in
two), a declaration's default is a value of its declared type, and the default
is finite (`ExposedValue::is_finite` — a `NaN` default is not equal to itself,
so it would break the round trip the contract depends on).
`ExposedParameter`'s `Deserialize` is strict (a violation is a serde error);
`ExposedParameters`' is lenient in the way the rest of `.ravprj` loading is —
it drops a blank-named, contradictory, non-finite or duplicate declaration
(first occurrence of a name wins) with a `tracing::warn!` and keeps the
project loadable. That leniency is **semantic only**: an entry that fails to
parse at all (missing field, unknown variant, truncation) still fails the whole
document load, because `Vec<StoredParameter>` cannot skip an element and
resynchronize. Declaration order is data (the presentation order), so the
persisted form is the sequence itself and is never sorted.

`ExposedValue` is deliberately **not** `ParameterValue`: only constant-shaped
values belong in an external contract, so animation channels, `PathPoints` and
`Curve` have no counterpart, and `Media` — which `ParameterValue` has no
variant for — carries an `AssetPath` that binds to a media node's `asset_id`.
Resolving and applying a binding is EXPO-2 / EXPO-4 in
`docs/implementation/exposed-parameters-plan.md`; this module only declares.

### `composition` — Layer-network model (v3, REQ-LAYER-001)

```rust
Layer::new(id, name, network: Graph) .with_time(start, in, out)
    // shell: start_frame (i64, negative allowed), solo/muted/locked,
    // transform (rotation in DEGREES), opacity, blend_mode, adjustment,
    // parent, audio: Option<AudioSource>; reserved v2: time_remap, track_matte
    // LayerSource is REMOVED — kinds are creation templates (REQ-LAYER-008)
AudioSource::new(asset_id, stream_index)
    // gain defaults to a constant 1.0; fade frames default to 0;
    // audio_muted defaults to false. gain is sampled in layer-local frames.
    // stream_index is the CONTAINER stream index (what the decoder seeks by),
    // not the ordinal among the audio streams: see
    // AssetMetadata::first_audio_stream_index().
layer.has_frame_output() -> bool   // false = frameless layer (Null or Audio)

Composition::new(id, name, (w, h), FrameRate, duration).add_layer(layer)
Document::{with_composition, get_composition, changed_network_paths(&old)}
Document::{with_media_asset(id, path), get_media_asset(&str)}
    // media_assets: im::HashMap<String, MediaAssetEntry> — the
    // evaluation-time asset table indexed by the media node's asset_id
Document::with_exposed_parameters(ExposedParameters)
    // exposed_parameters: the project's external contract (`exposed` above);
    // #[serde(default)], so a pre-v7 document reads as zero declarations
// Layer/Composition/Document are serde-capable (deterministic: id/key-sorted
// adapters; network graphs re-validate through Graph::from_parts on load).
// A deserialized Document must pass `doc.validate()` (structural invariants:
// root/comp-id/frame-rate/layer-ref integrity, DocumentValidationError),
// then `doc.advance_id_counters()` (REQ-LAYER-009) moves every
// NodeId/EdgeId/CompId/LayerId counter past `doc.id_watermarks()` so fresh
// ids never collide with loaded ones.
Document::sync_subnet_pins()   // load-time DRIFT REPAIR, not a format upgrade
    // Re-derives every subnet node's pins from its own inner In/Out
    // (`network::sync_subnet_pins`) in the flat graph, each layer network and
    // nested subnets, inner-most first via `graph_walk::map_subnets`. Runs
    // LAST in the load chain: the port normalizations above move the very
    // ports the derivation reads. Idempotent. MINTS NO IDS, so it cannot
    // repair a subnet whose `subnet` field is `None` (that needs two fresh
    // node ids, and the chain runs before `advance_id_counters`).
Document::fold_component_params()   // .ravprj v4 → v5, run AFTER the counters
    // Folds `_x` / `_y` component parameters (the scalar
    // `geometry.transform` `rotation`, and `attribute.set`'s `value` family
    // at the arity its `type` reads) into channel values in every graph:
    // the flat graph, each layer network, and nested subnets. A missing
    // component takes the template default. Exposed component ports collapse
    // into one vector port; separately driven ones are preserved by an
    // inserted `vector.construct.vec2` / `.vec3` / `.vec4` (so the pass mints
    // node and edge ids). Idempotent.

compile_composition(&comp, graph) -> CompilationResult  // background + shell chain:
    // base:       comp.background(Composition.background_color)
    // normal:     boundary(comp.network) → Transform → Opacity → Merge(◂ bg)
    // adjustment: boundary(◂ bg) → Transform → Merge(adjustment)(◂ bg)
    // frameless layer: Transform only (Null can still parent; Audio is not composited)
deterministic_node_id(comp, layer, NodeRole) / decode_deterministic_node_id(id)
    // the compiled `parent_transform` edge is a dependency edge only (value
    // unread) and exists only for active parents — see transform::world_matrix

transform::{Affine, layer_matrix, world_matrix}   // the ONLY shell transform
    // math: `comp.transform` (pixels) and the viewer (bbox / hit test / path
    // overlay) both call world_matrix, so overlays cannot drift from pixels.
    // world_matrix walks the whole parent chain regardless of the ancestors'
    // solo/mute (parenting is independent of visibility, REQ-LAYER-001) and
    // samples each ancestor at *its own* local frame (REQ-LAYER-006).
    // Affine is row-major 2x3: mul (self ∘ other) / apply / inverse /
    // is_identity. Translation is scaled by ctx.comp_to_canvas_scale()
    // (1.0 for UI-side contexts).
Layer::local_frame(comp_frame: u64) -> u64          // keyframe addressing
Layer::local_frame_continuous(comp_frame: f64) -> f64   // channel sampling
    // Same formula; layer_matrix / world_matrix and the shell's opacity and
    // merge nodes take the continuous form so sub-frame contexts animate.
Composition::{ancestors(&layer) -> Vec<&Layer>, descends_from(&layer, id)}

validate::{validate_precomp_cycles, validate_parenting_cycles,
    validate_layer_ref_cycles}   // layer.ref cycles incl. inside subnets

templates::LayerTemplate { key, display_name, nodes, edges }  // RON data
    .instantiate(&NodeRegistry) -> Result<Graph, TemplateError>
    // registry seeds ports/params; template extends/overrides; fresh
    // NodeId::next per instantiation
templates::{builtin_layer_templates(), builtin_layer_template(key)}
    // "solid" | "shape" | "media" | "audio" | "null" from assets/layer-templates/
```

### `network` — In/Out interface conventions (REQ-LAYER-002)

```rust
NET_IN_TYPE_KEY = "net.in"   // outputs: base_geometry, t, f, [source], custom params
NET_OUT_TYPE_KEY = "net.out" // inputs: frame (+ custom ports for Layer Ref)
find_in_node(&graph) / find_out_node(&graph) / frame_port_index(node)
// net.in/net.out values are PortRecords in port order.

// --- custom port editing (the only supported way to grow an interface node)
NetworkContext::{LayerRoot, Subnet}   // `NetworkPath::subnets.is_empty()`,
    // collapsed by the caller: `NetworkPath` lives in `ravel-ui`.
CustomPortType::{Float, Int, Bool, Vec2, Vec3, Color,
                 Geometry, Field, FrameBuffer, Text}
    // NOT a DataTypeId: Float/Int/Bool all wire as SCALAR but are three
    // parameter kinds with three Properties widgets.
    ::allowed_for_in(context)  // LayerRoot = the six value types (the shell
                               // supplies values only); Subnet = all ten
    ::allowed_for_out()        // eight, context-independent: all ten MINUS
                               // Int/Bool. Those two are a parameter KIND, and
                               // an Out custom port is a bare input with no
                               // parameter to keep one in — all three scalar
                               // kinds become `[SCALAR]` and read back as
                               // Float. The set to OFFER; add/set_type do not
                               // reject them, they just cannot tell them apart.
    .data_type() / .accepted_types()   // Color also accepts VEC4
    .default_parameter()  // channel-backed for Float/Vec2/Vec3/Color (custom
                          // In params are keyframable), Int/Bool constant,
                          // None for the four wire-only types
add_custom_port(graph, node_id, name, CustomPortType, NetworkContext)
    // In: output port + same-named parameter, together. Out: input port only.
    // Refuses a name an existing PARAMETER already holds, for every type: an
    // In output falls back to the parameter of its own name, so a wire-only
    // port landing on an occupied key would answer with the wrong type.
remove_custom_port(graph, node_id, name, NetworkContext)  // drops the parameter
rename_custom_port(graph, node_id, old, new, NetworkContext)  // with the port
    // Both take the context because editing a legacy custom `f` away from a
    // LAYER-ROOT In re-appends the BUILTIN `f` in the same call — otherwise
    // the layer cannot read its frame index until `append_missing_in_ports`
    // repairs it on the next load. Inside a Subnet nothing is auto-added: the
    // inner In's ports are the enclosing node's pin interface.
is_fixed_port(node, PortSide, name)   // net.in base_geometry/t/f/source,
    // net.out frame. NOT removable, NOT renamable — the guard lives here, not
    // inside `Graph::rename_port`, which stays the general operation.
is_builtin_port_name(node, PortSide, name)   // the RESERVED-name question
    // The two differ for exactly one port: an `f` output carrying a same-named
    // parameter is a legacy custom port (the evaluator honours the same
    // exception), so it is editable — while `f` still cannot name a NEW port.
set_custom_port_type(graph, node_id, name, CustomPortType, NetworkContext)
    // The port KEEPS ITS SLOT: only its wire type (In) or acceptance set (Out)
    // and its paired parameter change, so nothing is re-indexed. Edges the new
    // type cannot carry are dropped, one by one — an edge whose other end
    // still accepts the new wire type survives, so `Float` -> `Int` (both
    // SCALAR) costs nothing, the same way `vec4` <-> `color` does in
    // `Graph::set_params`. The parameter takes the new type's
    // `default_parameter()`; the old VALUE is not carried over (no meaning-
    // preserving map between the kinds). Retyping to the current type is a
    // no-op that keeps the value. Refuses a fixed port, and refuses turning a
    // legacy custom `f` into a parameterless type (without its parameter the
    // port would become the built-in `f` and stop being editable).
move_custom_port(graph, node_id, name, offset)   // -1 = one slot earlier
    // Fixed ports neither move nor are crossed: a step onto one stops the
    // move (as a step past either end does) and the call still succeeds, so
    // `net.in`'s base_geometry/t/f/source stay the prologue of every network.
    // `Graph::reorder_ports` is the raw permutation and knows none of this.
custom_port_type(node, PortSide, name) -> Option<CustomPortType>
    // Reads a port's type back. On an In OUTPUT the parameter disambiguates
    // Float/Int/Bool behind the one SCALAR; on an Out INPUT there is no
    // parameter, so a SCALAR port always reads back as `Float`. Fixed ports
    // answer too (the Ports UI lists them read-only and needs a type to show).

// --- subnet pins: DERIVED, never edited directly (REQ-LAYER-003)
SUBNET_TYPE_KEY = "subnet"   // is_subnet_node(node)
subnet_pins(&inner) -> Option<(Vec<InputPort>, Vec<OutputPort>)>
    // The interface `inner` implies. ASYMMETRIC on purpose:
    //   inputs  = the inner In's CUSTOM outputs only. Its fixed ports
    //             (base_geometry / t / f / source) are answered from the
    //             EvalContext or the enclosing scope's bindings, so a pin for
    //             one would be a socket nothing reads.
    //   outputs = EVERY inner Out input, `frame` included. Nothing on that
    //             side has a source of its own — NetOutProcessor collects its
    //             inputs, SubnetProcessor maps them onto the pins BY NAME —
    //             so `frame` is a pin like any other. `is_fixed_port` calling
    //             both "fixed" is the EDIT-PROTECTION question, not this one.
    // Pin acceptance goes through CustomPortType, so a COLOR pin takes VEC4
    // exactly as a hand-made colour port does. `None` when `inner` has no In
    // or no Out: half a network is not a declaration.
sync_subnet_pins(graph, subnet_id) -> Result<Graph, NetworkError>
    // Re-derives the pins and remaps the outer wiring onto them, matching
    // pins to the inner declaration BY NAME: a surviving name keeps its edges
    // wherever its slot moved; a vanished one takes its edges (and its
    // `ChannelSource::NodeOutput` bindings) with it; a retyped one keeps its
    // slot and drops only the outer edges the new wire type cannot carry.
    // Promotion parameters follow the pins: the subnet node's parameter list
    // IS the promotion set (`supports_param_ports` excludes subnets), a new
    // one is seeded from the inner In's same-named parameter so promotion
    // does not change what the subnet evaluates to, and one whose kind no
    // longer fits the pin resets to the type default.
    // Call after EVERY inner-graph commit and on load. Unchanged input comes
    // back untouched, so it is idempotent and cheap. MINTS NO IDS — which is
    // why a subnet with `subnet: None` is left broken rather than repaired.
    // Errs on a missing node or a node that is not a subnet.
sync_subnet_pins_or_log(graph, subnet_id) -> Graph   // for callers that have
    // already established the node is a subnet; logs a refusal, returns the
    // graph unchanged. Used by `sync_subnet_pins_in` and `replace_network`.
sync_subnet_pins_in(&graph) -> Graph   // every subnet directly in `graph`,
    // ONE level. Nesting comes from composing it with `graph_walk::map_subnets`
    // (see `Document::sync_subnet_pins`), which rewrites inner graphs first.
new_subnet_inner_graph(in_id, out_id) -> Graph   // ids MUST differ
    // The pair a fresh subnet starts with. In carries `t` ONLY: base_geometry
    // and source are shell concepts a subnet is not, and `f` is auto-added at
    // the layer root only. Out carries `frame` — that is what makes a
    // just-created subnet evaluate (with no output pin at all the processor
    // has no name to resolve) and it answers with a transparent frame.
seed_subnet_node(&mut node)   // inner graph + the pins it implies.
    // MINTS TWO GLOBAL NodeIds, and SKIPS one equal to `node.id`: node ids are
    // unique globally, not per graph — `Evaluator`'s processor table is a flat
    // `HashMap<NodeId, _>` with no ownership path in the key, so a subnet and
    // its own inner net.in sharing an id would lose one processor.

// --- collapse / extract: a selection <-> a subnet node (REQ-LAYER-003)
collapsible_targets(&graph, selection) -> Vec<NodeId>   // deduped, in id order
    // Drops the In and Out nodes — a network has exactly one of each, so a
    // collapse taking one would leave the network without it and give the
    // subnet a second — and `NodeMetadata.synthetic` nodes, which the next
    // shell compile puts back where they were. Id order, not selection order,
    // so the same selection always yields the same inner graph.
can_collapse(&graph, selection) -> bool   // what a menu asks before offering it
    // The same two rules the transform applies (something survives
    // `collapsible_targets`, and no path leaves the selection and comes back),
    // answered without minting ids or building the graph a probe would.
collapse_to_subnet(graph, selection) -> Result<(Graph, NodeId), NetworkError>
    // ONE PIN PER OUTER ENDPOINT, not per crossing edge: input pins are keyed
    // by the `(node, output port)` the crossing edges LEAVE, output pins by
    // the same pair inside the selection. One outer wire stays one wire — a
    // source feeding three moved nodes arrives on a single pin and fans out
    // again inside. Keying by the input end would multiply one source into as
    // many pins as it had readers, which is a different graph, not a nested one.
    // A PIN IS NAMED AFTER THE INPUT PORT the crossing edge lands on (the moved
    // node's port for an input pin, the outer reader's for an output pin) —
    // the end that says what the value is FOR, and the reason a selection
    // feeding `net.out` collapses to a `frame` output pin, the same shape a
    // hand-built subnet has. A name already taken on its side (the In node's
    // built-in names included) gets a `_2`, `_3`, … suffix.
    // A selection NOTHING LEAVES still gets one `frame` output pin, for
    // `new_subnet_inner_graph`'s reason: with no output pin at all the subnet
    // cannot be asked for a value. When edges do cross, they are the pins and
    // no extra one is invented.
    // NODE IDS DO NOT CHANGE (they are document-unique, not per graph) and the
    // edges between moved nodes keep theirs; only the crossings are re-created,
    // one becoming two. So a collapse/extract round trip restores CONNECTIVITY
    // (node id + port name), never edge ids — compare graphs that way.
    // Errs `NothingToCollapse`, and `CollapseWouldCycle` when a path leaves the
    // selection and returns: the subnet node would feed itself and
    // `Graph::add_edge` refuses it.
extract_subnet(graph, subnet_id) -> Result<Graph, NetworkError>   // the inverse
    // Pin wiring is reconnected end to end — an edge into an input pin runs
    // from its outer source straight to whatever the inner In drove, an edge
    // out of an output pin from whatever fed the inner Out — and the In/Out
    // pair is dropped, because it WAS the boundary. The moved nodes are
    // translated so their centroid lands where the subnet node stood (collapse
    // moves it the other way, so the round trip keeps positions too).
    // An inner edge from a FIXED In port is re-pointed at the ENCLOSING In
    // node: `t` (and `f` / `base_geometry` / `source`, where the inner In has
    // them) are EvalContext values rather than pins, so the same port one level
    // up is the same value, and not reconnecting would cost an extracted node
    // its time. A CUSTOM pin with nothing wired outside is not treated that
    // way — its value came from the promotion parameter, which has no home in
    // the parent, so the input becomes unconnected and falls back to the moved
    // node's own parameter.
    // A moved node whose id the parent already uses is renumbered, and the
    // edges and `ChannelSource::NodeOutput` bindings AMONG THE MOVED NODES
    // follow it (`graph::remap_parameter_node_outputs`); the renumbering does
    // not reach inside a moved subnet node's own inner graph. Document-built
    // graphs never hit this — hand-assembled ones do.
    // Errs on a missing node, a node that is not a subnet, and `NoInnerGraph`.
// KNOWN GAP, both directions: a `ChannelSource::NodeOutput` binding that
// CROSSES the boundary is not followed (collapse splits it across the
// boundary, extract leaves it pointing at a dropped inner In). Same shape as
// the trap `remove_output_port` names. Unreachable today: nothing in
// `ravel-app` / `ravel-ui` / `ravel-project` constructs a `NodeOutput` source.
NetworkError::NotSubnetNode(NodeId)   // beside NotInterfaceNode / NoInnerGraph /
    // NothingToCollapse / CollapseWouldCycle / PortTypeNotAllowed /
    // ReservedPortName / FixedPort / Graph
```

### `geometry` — attributes, container, fields (procedural geometry spec)

```rust
type AttrName = SmolStr;
AttributeArray::{F32, Vec2, Vec3, Vec4, Color, I32, Bool, Str}(Vec<..>)
    .len() / .attr_type() / .as_f32(name)? / .as_vec2_mut(name)? / ...
AttributeSet    // HashMap<AttrName, Arc<AttributeArray>>, uniform length
    .insert(name, column)?      // validates length against existing columns
    .make_mut(name)?            // CoW via Arc::make_mut; must not change len
    .get(name) / .element_count() / .iter() / .describe()

Geometry        // domains: points / primitives+attrs / instances / detail
    ::new() / ::from_points(Vec<Vec2>) / ::from_points3(Vec<Vec3>) // P + index
    .validate()?           // P:Vec2|Vec3, prim + index ranges, detail len 1
    .positions(Domain) -> Option<Result<Positions<'_>>>   // the P column
    .points()/.points_mut() (+ primitive_attrs, instances, detail variants)
    .push_primitive(Primitive::Path { verts: Range<usize>, closed })
    .push_mesh(verts: Range<usize>, triangles: &[u32])  // the only mesh builder
    .indices() -> &[u32]                  // shared triangle buffer (Arc CoW)
    .extend_indices(&[u32]) -> usize      // append, returns the start offset
    .mesh_indices(&Primitive) -> Option<&[u32]>
    .has_mesh() / .require_paths(operation)?  // => RequiresPathPrimitives
    .set_instance_source(Option<Arc<Geometry>>)
    .summary() -> GeometrySummary         // counts + attribute listings
    // implements NodeData (GEOMETRY) + GeometricData; bounds() is the xy extent

Primitive::Path { verts: Range<usize>, closed }
Primitive::Mesh { verts: Range<usize>, indices: Range<usize> }
    // `indices` ranges into Geometry::indices(), 3 per triangle; each value is
    // an offset RELATIVE to verts.start, so merge shifts ranges and appends
    // the blob instead of remapping every triangle.
    .verts() -> &Range<usize>            // both variants; kind-agnostic walks
    .is_mesh() / .shifted(points, indices) -> Primitive   // relocate for concat

Positions::{D2(&[Vec2]), D3(&[Vec3])}   // P at the dimension a domain carries
    ::from_column(&AttributeArray)?      // rejects non-position columns
    .len() / .is_empty() / .attr_type() / .dimension()
    .planar() -> Option<&[Vec2]>         // zero-copy 2D fast path
    .require_planar(operation)?          // 3D => GeometryError::RequiresPlanarP
    .projected() -> Cow<[Vec2]>          // xy; documented planar consumers only
    .get3(i) / .iter3()                  // Vec3 with z = 0 for a 2D column

geometry::names // reserved attribute names: P (Vec2|Vec3), INDEX, ID, ROT,
                // SCALE, CD, ALPHA, PSCALE, AGE, LIFE, VELOCITY, IN_TAN,
                // OUT_TAN, ANCHOR, SOURCE_INDEX, and 3D-only ORIENT (Vec4
                // quaternion), SCALE3 (Vec3), N (Vec3 normal)

geometry::rotation   // owns rotation math: radians, ZYX euler (Rx * Ry * Rz,
                     // so Z applies first), right-handed, no 4x4 type here
Quat(x, y, z, w)     // from_vec4/to_vec4 match the `orient` column component
                     // for component; from_euler_zyx/to_euler_zyx,
                     // from_axis_angle, mul_quat (`*`, rhs first), dot, length,
                     // normalized, conjugate, inverse, negated, rotate,
                     // slerp (short arc), to_mat3
Mat3([f32; 9])       // row-major, column vectors (mul_vec3 = m * v); from_rows,
                     // rows, as_array, get, from_euler_zyx, to_euler_zyx (at
                     // |Y| = 90° X is 0 and the coupling lands on Z), to_quat,
                     // mul_mat3, transposed

trait Field: Send + Sync {
    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray;
    fn byte_size(&self) -> u64;   // NO default; combinators recurse into
                                  // their operands, same rule as NodeData
}
FieldSample { positions, attributes, ctx }   // whole domain, not just P
    ::new(positions, &AttributeSet, &ctx) / ::positions_only(positions, &ctx)
FieldValue(Arc<dyn Field>)   // NodeData (FIELD), lazy — consumers sample
ConstantField(f32)           // the same value everywhere; `ConstantField(0.0)`
    // is the typed zero of an unconnected FIELD port (a field's zero has to be
    // a field — consumers call `sample`, and a `Scalar` cannot answer that)
NoiseField { seed, frequency, octaves }      // deterministic simplex/fBm
FalloffField { center, inner_radius, outer_radius, shape }
CurveRemapField::new(source, points)         // piecewise-linear
AttributeField::new(name).with_component("y").with_normalize(true)
    // reads a column of the sampled domain (index/id/user), so modulation can
    // be driven by something other than position. F32/I32/Bool/vector columns;
    // normalize maps the column's own [min, max] onto [0, 1]. A missing,
    // non-numeric or wrong-length column warns and yields `default`.
AddField/MultiplyField/MaxField { left, right }, BlendField { .., amount }
FieldApply::new(Domain, target)              // + with_amount/combine/components/group
CombineMode::{Set, Add, Multiply, Min, Max}  // result = lerp(existing, op, amount)
ComponentMask::parse("xy" | "rgb" | "a")     // empty or unusable => every component
apply_field(&geo, &FieldApply, &field, &ctx) -> Result<Geometry>
    // dimension-agnostic; a 3D geometry samples the planar built-in fields at
    // the xy projection of P and P itself is only rewritten when it is target

geometry::ops
attribute_set / promote_attribute / attribute_transfer -> Result<Geometry>
bounds_center(&geo) -> Option<Vec3>          // points, else instances; z = 0 in 2D
path_sample(&geo, distance) -> Result<PathSample>   // planar only

geometry::triangulate
Triangulator     // `earcut` (MIT OR Apache-2.0) behind a buffer-owning type;
    ::new()      // hold ONE per evaluation path — a fresh instance per frame
                 // throws away the scratch buffers that make it allocation-free
    .triangulate(&[Vec2], hole_starts: &[u32]) -> Result<&[u32], GeometryError>
    // Vertices are the outer ring followed by every hole ring in one slice,
    // each ring given once and never repeating its first vertex; hole_starts
    // is where each hole ring begins. Input is an ALREADY flattened polyline
    // (ravel_nodes::flatten::FLATTEN_TOLERANCE); this module has no tolerance
    // of its own. Output is 3 indices per triangle addressing the slice
    // directly, which is exactly push_mesh(verts, triangles)'s relative
    // encoding — no usize round trip. Degenerate rings (< 3 vertices, zero
    // area, duplicate vertices, self-intersection) are NOT errors: they yield
    // whatever triangles exist, possibly none.
    // OUTPUT IS ALWAYS COUNTER-CLOCKWISE, whatever the input's winding:
    // `earcut` normalises the outer ring, so index order carries no facing
    // from the input. Derive a facing yourself (FRAC-3's cut-plane N, 3D-8's
    // caps); do not read it back out.
    // Rejections: the two hole-index preconditions `earcut` documents as
    // panics, caught at the entry so they never reach it =>
    // GeometryError::HoleRingsOutOfOrder / HoleRingOutOfRange, plus
    // TooManyVertices past the u32 index bound. (`earcut` documents a third
    // panic, an internal capacity limit needing >1e8 vertices — above the
    // u32 bound already refused here.)
```

### `scene` — 3D scenes and cameras (REQ-3D-001 / REQ-3D-002)

Scene space is composition space extended with depth: `+X` right, `+Y` **down**
(the 2D pixel convention; the composition origin is its top-left corner), `+Z`
away from the viewer. Right-handed. `Mat4` is **column-major**
(`cols[col * 4 + row]`) and acts on column vectors from the left, so a
hierarchy composes as `parent * child`.

A `Scene` is **not persisted** — no `Serialize`/`Deserialize`. It is an
evaluated value on a port; `.ravprj` stores nodes, edges and parameters, which
is why the object model can grow without a file migration.

```rust
Mat4 { cols: [f32; 16] }               // column-major
    ::IDENTITY / ::from_rows([[f32;4];4]) / ::from_translation / ::from_scale
    ::from_euler_zyx_degrees([f32;3])   // extrinsic ZYX: fixed Z, then Y, then X
                                        // => Rx * Ry * Rz. Only lifts
                                        // geometry::rotation::Mat3 into affine
                                        // 4x4 — that module owns the order and
                                        // the trigonometry, here and for `orient`
    .element(row, col) / .mul(&rhs) / .transform_vec4 / .transform_point3

Transform3D { translate, rotate, scale, pivot }   // rotate = Euler degrees
    ::IDENTITY / ::from_translation([f32;3])
    .to_matrix()   // T(translate) · T(pivot) · R · S · T(-pivot)

SceneImage::new(Arc<dyn NodeData>, width, height) -> Result<_, SceneError>
    // the value must be tagged FRAME_BUFFER; either the CPU `FrameBuffer` or
    // ravel-gpu's resident frame, so a texture needs no readback
    .frame() / .width() / .height() / .aspect_ratio()
    .rect() -> Rect        // pixel-sized, centred on the object origin, so the
                           // rectangle carries the source aspect ratio
SceneContent::{Geometry(Arc<Geometry>), Image(SceneImage), Scene(Arc<Scene>)}
SceneObject { content, transform }
    ::new / ::geometry / ::image / ::scene

Scene   // NodeData (SCENE); im::Vector lists, so appending shares the spine
    ::new() / ::from_camera(Camera)
    .with_object(SceneObject) -> Scene   // returns a new value
    .with_camera(Camera) -> Scene
    .merged(&Scene) -> Scene             // self's objects+cameras, then other's
    .objects() / .cameras() / .object_count() / .camera_count()
    .primary_camera() -> Option<&Camera> // the first camera merged in
    .flatten() -> Vec<FlatObject>        // drawable leaves, nesting composed
    // is_gpu_resident() is true when ANY frame it holds, at any depth, is a
    // texture: the scene is then not wholly CPU-readable. byte_size() recurses
    // into content and nested scenes, like Geometry's instance_sources, and
    // saturates (an arbitrary NodeData supplies the estimate).
    // All three walks assume ACYCLIC nesting, which the API guarantees: a
    // Scene is immutable, so SceneContent::Scene can only wrap an
    // already-constructed value. There is no depth limit.
FlatObject { content: FlatContent, world_transform: Mat4 }
FlatContent::{Geometry(Arc<Geometry>), Image(SceneImage)}

Camera { position, target, up, projection, near, far }
    // `up` is the scene direction mapping to the TOP of the image; scene space
    // is Y-down so the default is -Y
    ::default() / ::looking_at(position, target)
    .with_projection(Projection) / .with_clip_range(near, far)
    .view_matrix()                       // left-handed basis on the right-handed
                                         // world: view-space z is the distance
                                         // ahead. Degenerate inputs are
                                         // resolved, never NaN
    .clip_range() -> (near, far)         // `near` as authored, `far` forced at
                                         // least MIN_CLIP_SPAN beyond it:
                                         // `near`/`far` are independent params
                                         // so `near >= far` is reachable and
                                         // would invert the depth mapping
    .projection_matrix(aspect)           // wgpu clip space: xy in [-1,1] y-up,
                                         // depth in [0,1], near→0 far→1.
                                         // Total: every degenerate input
                                         // (aspect <= 0 or NaN, fov at 180°,
                                         // empty/inverted clip range) is
                                         // clamped, never propagated
    .projection_matrix_for(&EvalContext) / .view_projection_matrix(aspect)
    .view_projection_matrix_for(&EvalContext)
Projection::Perspective { fov_y_degrees } | Orthographic { height }
camera::aspect_ratio(&EvalContext) -> f32
    // from `comp_resolution`, NOT `resolution`: scene coordinates are
    // composition coordinates, so a half-size preview is a render scale
    // (`comp_to_canvas_scale`) rather than a reprojection
camera::PROJECTION_KINDS / PROJECTION_PERSPECTIVE / PROJECTION_ORTHOGRAPHIC
camera::DEFAULT_{DISTANCE, FOV_Y_DEGREES, ORTHOGRAPHIC_HEIGHT, NEAR, FAR}
camera::MIN_CLIP_SPAN
SceneError::{NotAFrameBuffer { data_type }, EmptyImage { width, height }}
```

### `registry` — node templates for the editor

```rust
NodeTemplate::new(type_key, display_name, NodeCategory)
    .with_input(InputPort { name, accepted_types })
    .with_output(OutputPort { name, data_type })
    .with_param(Parameter { key, value })
    .with_param_range(key, hard, ui)     // ParamRange: hard = clamp bound,
    // ui = default editing span (slider/scrub); ui must be within hard.
    // Every numeric default param MUST declare one (builtin test enforces).
    .with_param_options(key, options)    // closed option set for a String
    // param → Properties renders an enum dropdown (merge `operation`,
    // math.scalar `op`)
registry.param_range(type_key, param_key) -> Option<&ParamRange>  // .clamp(v)
registry.param_options(type_key, param_key) -> Option<&[String]>
template.create_node(id) / registry.create_node(type_key, id) -> Node
    // NOT a pure function of the template for one type key: a `subnet`
    // template declares no ports, so this builds the inner net.in/net.out pair
    // (`network::seed_subnet_node`) and takes the pins from THAT — which MINTS
    // TWO GLOBAL NodeIds. Every other template is pure. Callers that probe
    // templates speculatively (the palette's connectability filter) therefore
    // burn ids; harmless, but do not assume creation is free.
register_builtins(&mut NodeRegistry)   // registry/builtin.rs — update the
    // count/category tests there when adding a template
registry.categories() -> Vec<NodeCategory>   // present categories, in
    // declaration order (the discriminant), which the add-node menu order in
    // `panels/node_editor.rs::node_category_order` mirrors
NodeCategory::{Geometry, Scene, Field, Image, Color, Time, Utility}
    // data-domain groupings, each 1:1 with the port colour of its domain's
    // data type (Scene => SCENE). Adding a variant touches the enum,
    // `port_colors::category_color`, `assets::RavelIcon::for_category`,
    // `node_category_order` / `node_category_label`, and the
    // `panel.node_graph_menu.category.*` locale keys
```

`NodeTemplate.label` is an English literal that seeds the created node's
`metadata.label` (and thereby detects user renames); the UI does not display
it directly — display labels come from the locale entry
`node.<type_key>.label` (see `ravel-ui` `node_locale` below), falling back
to the `type_key`.

### `undo`

```rust
UndoStack::<T: Clone>::new(initial).with_max_history(n)
    .push(state) / .undo() / .redo() / .current() / .can_undo() / .can_redo()

// Journal (crash recovery): length-prefixed entries behind an 8-byte header
// (magic "RVLJ" + u32 JOURNAL_FORMAT_VERSION). Legacy headerless files and
// mismatched versions are discarded (writer truncates on open, reader skips
// with UnsupportedVersion) — the bincode layout has no cross-version
// guarantees, and `Node` field additions must never use
// `skip_serializing_if` (it desyncs the journal's field layout).
```

`.ravprj` saves publish a fully written, synced same-directory temporary file
through an atomic replacement and retain the previous revision as `.bak`.
`ProjectFile::load_with_backup` validates and opens that backup when the main
archive is unreadable, except for `MigrationError::TooNew` (never silently
roll a newer project back to an older revision).

### `runtime::eval_service` — background evaluation (UI non-blocking)

```rust
InvalidationHint::{None, Params(Vec<NodeId>), Structural}
trait EvalWorkerHooks: Send {          // host-supplied, runs on the worker
    fn sync(&mut self, &mut Evaluator, &Graph, Option<&Document>, &InvalidationHint);
    fn finalize(&mut self, Arc<dyn NodeData>, &EvalContext) -> Arc<dyn NodeData>;
}
EvalRequest { graph, nodes: Vec<NodeId>, path: Vec<PathSegment>, ctx,
    document: Option<Arc<Document>>, hint }
    // document → Evaluator::set_document before sync (scoped invalidation);
    // non-empty path evaluates via evaluate_at
    // nodes are pulled in order through one Evaluator, so a target upstream
    // of an earlier one is a cache hit; a failing target does not stop the
    // rest. Coalescing is per request, so all targets survive or none do
EvalService::spawn(hooks, on_update)   // dedicated thread "ravel-eval-service"
EvalService::spawn_with_budget(hooks, SharedCacheBudget, on_update)
    // the application's form: the worker builds its Evaluator on its own
    // thread, so the budget has to be handed in at spawn
ProcessorSync<'a>            // what `sync` gets: register / processor /
    ::new(&mut Evaluator)    // invalidate_node, and nothing else
    .request(EvalRequest) -> u64              // generation; latest-wins queue
    .cancel_pending() / .latest_generation()
EvalOutput = Result<Arc<dyn NodeData>, EvalError>  // one target's outcome
EvalUpdate { generation, frame, timings,          // worker thread; timings
    results: Vec<(NodeId, EvalOutput)> }
    // one entry per requested node, in EvalRequest::nodes order; timings are
    // aggregated over every target and feed the node editor's load readout
```

Consumers publish updates monotonically: any update newer than the last
published generation is shown (requiring `generation == latest_generation()`
starved the viewer whenever one evaluation outlived one playback tick);
`cancel_pending()` returns a fence generation that blocks in-flight results.
`ravel-app`'s `GpuEvalHooks` (`src/eval_hooks.rs`) owns `GpuContext` +
`ShaderManager`, maps hints to `register_all_processors` /
`processor_for_node` (searching the document's layer networks too), and
rasterizes `Geometry` outputs for the Viewer. A `Params` hint whose node
already has a processor reporting `rebuild_on_node_change() == false` is
served by `Evaluator::invalidate_node` instead of a rebuild — a GPU
processor's construction compiles a shader and creates a pipeline.

### `runtime::playback` — frame-accurate transport clock

```rust
PlaybackClock::new(fps: FrameRate, duration_frames: u64)   // stopped at 0
    .play(now: Instant) / .pause(now) / .toggle(now) / .stop()
    .seek(frame, now) / .step(±delta, now) -> u64          // step pauses
    .current_frame(now) -> u64   // closed-form from play origin: jitter
                                 // drops frames but never drifts the clock
PlaybackState::{Stopped, Playing, Paused}
```

The time source is an argument; `ravel-app`'s `Transport` wraps it with
`ClockSource::Wall(Instant)` / `ClockSource::Audio(&SyncClock)` (audio-plan
unit 3): the audio device clock is the master while the active composition
has audio tracks and an engine runs, otherwise playback falls back to the
wall clock. Reaching the end pauses on the last frame. See
`docs/implementation/done/playback-foundation-plan.md` and
`docs/implementation/audio-plan.md`.

## ravel-nodes — built-in processors

`register_all_processors(&mut Evaluator, &Graph, &GpuContext, &mut ShaderManager, &Arc<Mutex<TexturePool>>)`
maps `Node::type_key` → processor and recurses into subnet inner graphs;
`processor_for_node(&Node, &GpuContext, &mut ShaderManager, &Arc<Mutex<TexturePool>>)`
builds one node's processor (processors never capture parameter values —
edits only require dirty marking, not a rebuild; the GPU ones say so via
`NodeProcessor::rebuild_on_node_change() == false`);
`shared_texture_pool(&GpuContext)` makes a standalone per-eval-worker pool
with a fixed 512 MiB idle budget (tests, examples).
`shared_texture_pool_with_budget(&GpuContext, SharedCacheBudget)` is the
application's form: the pool then holds no limit of its own and its idle
allowance is the VRAM the budget has left after the resident textures,
re-read on release (an approximation that follows, not a hard instant cap).

A GPU processor gets its pipeline from
`ShaderManager::compute_pipeline(name, source, entry_point, layout, workgroup_size)
-> GpuResult<Arc<ComputePipeline>>`, which compiles (cached by source hash,
validated once per distinct source) and builds the pipeline (cached by shader
hash + entry point + layout + workgroup size) in one call — N nodes of a type
share one pipeline. `created_pipeline_count()` / `cached_module_count()` /
`validated_count()` expose the counters for tests.

The same WGSL reaches a non-wgpu backend through `ravel_gpu::translate`:
`translate_wgsl(name, source, ShaderTarget) -> GpuResult<TranslatedShader>`,
plus `ShaderManager::translate(name, target)` for a source the manager already
holds (a built-in embedded with `include_str!` or one registered at runtime).
`ShaderTarget::{Msl, Hlsl, SpirV}` (`ShaderTarget::ALL` is the array tests
iterate) names the language; `TranslatedShader` carries `as_text()` for MSL /
HLSL, `spirv_words() -> Option<&[u32]>` for SPIR-V, `to_bytes()` for either, and
`entry_points()` — **the names in the artifact**, which a writer renames when the
WGSL name is a keyword of the target (MSL rewrites `main`; the replacement
spelling is naga's choice, not a contract), so a backend must read them here
rather than reuse the WGSL name. No naga or wgpu type
appears in the signatures. Translation needs no device, parses and validates
through the same code as `validate_wgsl`, so invalid WGSL fails as
`GpuError::ShaderCompile` exactly as compilation would; valid WGSL that has no
expression in the target fails as `GpuError::ShaderTranslate { name, target,
message }`. One module may hold several pipelines whose `@binding` numbers
overlap (`rasterize.wgsl`) — for different reasons per target: MSL slots are
assigned **per entry point**, from the globals that entry point reaches, while
HLSL keeps one module-wide identity map because a register class per resource
kind (`b` / `t` / `u` / `s`) already separates them. A writer that cannot place
a binding drops the entry point instead of failing, so translation checks that
every entry point survived.

**No `wgpu` type appears in `ravel-gpu`'s public API** outside `interop`
(GPUBK-4), and the `gpu-facade-wgpu` lint keeps it that way. `GpuContext` has
no `device()` / `queue()` / `adapter()`; `ComputePipeline` and `RasterPipeline`
expose neither their raw pipeline nor their bind-group layout;
`PooledTexture` and `GpuFrameBuffer` hand out a `binding()`, not a texture;
`CompiledShader` keeps its module. Work is described (`ComputeDispatch`,
`QuadDraw`, `BindingDesc`, `TextureKey`) and the crate converts it once.
`GpuContext::adapter_info() -> &AdapterInfo { name, vendor, device,
device_type: DeviceType, driver, driver_info, backend: GpuBackend }` is the
crate's own descriptor — `GpuBackend` is "what is executing" (`Vulkan` /
`Metal` / `Dx12` / `Gl` / `BrowserWebGpu` / `Noop`) and is not
`interop::NativeApi`, which is the narrower "whose objects can be handed out".

`ravel_gpu::interop` is the **only** place a backend-native handle leaves the
crate, and exists for the OpenFX host (REQ-PLUGIN-001) and hardware decode
(REQ-GPU-001) alone: `native_api(&GpuContext) -> Option<NativeApi>` (safe,
`Metal` / `Direct3D12`, `None` on any other backend) says whether the two
`unsafe` accessors will answer, `native_device(&GpuContext) ->
Option<NativeDevice<'_>>` yields `id<MTLDevice>` / `ID3D12Device*` and
`native_texture(&GpuFrameBuffer) -> Option<NativeTexture<'_>>` yields
`id<MTLTexture>` / `ID3D12Resource*`, both as a borrowed non-null
`NativeHandle<'a>` (`api()` / `as_ptr()`) that owns nothing and must not be
released — under D3D12 it carries no `AddRef`. The types are deliberately not
re-exported from the crate root so every use site spells `ravel_gpu::interop`;
the `gpu-native-handle-escape` lint greps for the handle symbols themselves, so
an alias does not launder them: only `ravel-gpu`, `ravel-media` and the future
OFX crate may name them (`native_api` is exempt — it reads the adapter
description and hands out no pointer). A node processor must not — it would pin
the node to one backend and bypass dispatch batching and the pool's lifetime
bookkeeping. Work submitted straight to a native handle runs on a timeline the
dispatch batch knows nothing about, and `flush()` only *submits* the batch —
ordering the two needs `GpuContext::wait()` or a fence shared with the native
queue, not a flush.
The same module holds the device-sharing pair, which trades in wgpu objects
rather than platform ones and is judged by its own rule (`GPUBK-9`):
`interop::context_from_wgpu(instance, adapter, device, queue) -> GpuContext`
builds a context on a device someone else created (REQ-GPU-001's "UI and
compute share one device"), and `interop::wgpu_instance(&GpuContext) ->
&wgpu::Instance` is its counterpart — a toolkit that shares its device creates
its surfaces on that same instance. Receiving a device is not the escape handing
a pointer out is: it happens once at startup and bypasses neither dispatch
batching nor the pool. What it does decide is which device the whole pipeline
runs on, so the `gpu-device-sharing` lint limits the callers to `ravel-gpu` and
`ravel-app`. `crates/ravel-gpu/tests/device_sharing.rs` pins the behaviour.

GPU nodes exchange `ravel_gpu::GpuFrameBuffer` (VRAM-resident, shares
`DataTypeId::FRAME_BUFFER`; `.to_frame_buffer()` reads back, `Drop` returns
the texture to the pool). Helpers re-exported from `ravel_nodes`:
`ensure_gpu` / `ensure_cpu` / `clone_frame_value` (pass-throughs).
A GPU node's per-frame work is one
`GpuContext::dispatch_compute(&ComputeDispatch { pipeline, inputs, output,
uniform, width, height, .. })` call: inputs are `TextureBinding`s from
`PooledTexture::binding()` / `GpuFrameBuffer::binding()` (N sampled inputs
bind at `@binding(0..N)`, the output storage texture at `@binding(N)`, the
uniform block at `@binding(N+1)`), uniform buffers are reused by content
key, bind groups by (pipeline, textures, uniform) identity, and all
dispatches record into one frame-shared encoder. The batch submits only at
a flush point — a readback of a texture the batch writes, an upload into a
texture the batch uses, `GpuContext::wait`, an explicit `flush()`, or the
64-dispatch cap. In the app the viewer readback is the per-frame flush:
one submit per frame, never one per node.
`GpuContext::transfer_stats()` counts per-context uploads/readbacks and
readback staging buffers created; `GpuContext::dispatch_stats()` counts batched
submits, recorded passes (`dispatches`, one per `ComputeDispatch` or `QuadDraw`
— a different quantity from `submits`, which batching drove below one per
evaluation), and uniform-buffer / bind-group creations.
Transfers take the pool lease itself, never a texture handle:
`upload_texture(&GpuContext, &PooledTexture, &[u8])` reads the layout from the
lease's own `key`, so the copy and the allocation cannot disagree.
Readback (`ravel_gpu::transfer`) borrows its mappable buffer from a size-keyed
staging pool, waits for its own submission index rather than the whole device,
and lands the bytes in whichever container the caller keeps:
`read_texture(&GpuContext, &PooledTexture) -> Vec<u8>`,
`read_texture_shared -> Arc<[u8]>` (what
`GpuFrameBuffer::to_frame_buffer` builds the `FrameBuffer` from, with no second
copy). `begin_read_texture` / `GpuFrameBuffer::begin_readback` return a
`PendingReadback` — the backend-agnostic stand-in for an in-flight copy (no
`map_async`, no `MapMode`): `wait_timeout(d)` waits up to `d` and reports
whether the copy landed, `is_complete()` is the zero-timeout form (a real device
query, so ask for the bytes rather than spinning on it), and
`wait_into_vec()` / `wait_shared()` block until they can return them. Every
variant waits only on this readback's own submission.
A `ComputeDispatch` with an empty `uniform` declares no slot at
`@binding(N + 1)` — the parameterless case, used by the rasterizer's
unpremultiply pass.
`ravel_gpu::RasterPipeline` is the one render pipeline, built with a
`ColorTarget { format: TextureFormat, blend: BlendMode }` (no wgpu type in the
signature). Drawing is declarative like compute:
`GpuContext::draw_quads(&QuadDraw { pipeline, uniform, storage, target,
instance_count, .. })` — the uniform binds at `@binding(0)`, `storage[0..N]`
(read-only storage buffers, each non-empty) at `@binding(1..N + 1)`, and the
pass clears `target` before drawing six vertices per instance. It records into
the same frame-shared encoder as compute, and `target` joins the batch's
pending-use set, so the attachment may be released to the pool immediately
after recording. The draw's storage buffers and bind group are rebuilt every
draw (the geometry differs frame to frame) and counted in
`bind_groups_created`. Rasterize draws analytic-AA path/point quads into a
premultiplied RGBA16Float attachment, then converts to straight-alpha
RGBA32Float without a CPU transfer.
Current keys:

| type_key | processor | notes |
|----------|-----------|-------|
| `constant` | CPU | Scalar output |
| `constant.color` | CPU | animatable `color` param (Channel4) → `Color` output |
| `math.scalar` | CPU | `op` enum (add/subtract/multiply/divide/min/max/mod/pow + unary abs/negate/floor/ceil/round/sqrt/sin/cos); `a`/`b` are Float params (drive via exposed param ports); div/mod-by-zero and sqrt(<0) → 0; mod is `rem_euclid`; radians |
| `math.remap` | CPU | linear fit `value`: `[in_min,in_max]` → `[out_min,out_max]`, optional `clamp`; degenerate in-range → `out_min` |
| `vector.construct.vec2` / `.vec3` / `.vec4` | CPU | Scalar components → `Vec2` / `Vec3` / `Vec4` output. `x`/`y`/`z`/`w` are Float params (drive via exposed param ports, like `math.scalar`); unset components are 0. Arity is a separate `type_key`, not a `type` param, because port types live on the node instance (`VECTOR_CONSTRUCT_VEC2` and friends in `registry::builtin`) |
| `media` | CPU | decodes media via the document asset table (`asset_id`), branching on `AssetKind`: containers via `MediaReader` (layer-local seconds → media frame `floor(t·fps)`, clamped), stills via an injectable `ImageReaderFactory` with the decoded frame `Arc`-cached, sequences by rebuilding the frame file name (`start + floor(t·seq_fps)` clamped to `start..=end`; seq_fps = `metadata.frame_rate` else comp fps); offline / decode failure → transparent frame at ctx resolution (warned once per asset); FFmpeg backend behind the `ffmpeg` feature; `video` is a load-time alias normalized by `Document::normalize_node_type_aliases` |
| `layer.ref` | CPU | same-comp reference to another layer's `net.out` port (`layer` + `port` params); pre-transform output at the target's local time; typed zero outside its interval |
| `subnet` | CPU | evaluates `node.subnet` recursively (`PathSegment::Subnet`); connected pins bind the inner `net.in`, unconnected pins promote same-name node params |
| `blur`, `transform`, `merge`, `color_correct` | GPU (wgpu compute, WGSL in `src/shaders/`) | tests need an adapter |
| `rasterize` | GPU render pass | Geometry → resident FrameBuffer; non-zero-winding paths, point sprites, nested instances. Paths with `in_tan`/`out_tan` point attributes are bezier-flattened first (shared `flatten::flatten_path`, CPU and GPU consume the same polyline). Element color: `Cd`/`alpha` attrs > `color` pin > `color` param (REQ-LAYER-008). Synthetic Composition nodes remain on the CPU zeno reference path. Planar paths only: a `Vec3` `P` or a `Primitive::Mesh` anywhere in the geometry or its instance sources is an explicit error (`RequiresPlanarP` / `RequiresPathPrimitives`), since 3D and triangles are drawn through `scene.render`. |
| `field.noise` / `.falloff` / `.curve_remap` / `.expression` | CPU | emit `FieldValue`. `field.curve_remap`'s `points` is a `Curve` parameter (a `"0:0,1:1"` string before `.ravprj` v6) |
| `field.attribute` | CPU | emit `FieldValue` reading a column of the sampled domain (`name` / `component` / `normalize` / `default`) |
| `field.add` / `.multiply` / `.max` / `.blend` | CPU | combine two field inputs |
| `field.apply` | CPU | Geometry + Field → Geometry; modulate a named attribute |
| `geometry.transform` | CPU | scale→rotate→translate around a pivot (`use_centroid` default on = bbox center, else the `pivot` Channel3); `translate` / `scale` / `pivot` are Channel3 and `rotation` is a Channel3 of Euler degrees; a `Vec2` `P` uses only the xy/Z components (the rest are inert, identity fast path included), a `Vec3` `P` uses all three with the fixed ZYX Euler order; transforms point `P` and instance placement (`P` + `rot` offset + component-wise `scale`); CoW columns |
| `geometry.merge` | CPU | concatenates A then B: points, primitives (vertex ranges re-based; meshes also re-base their index ranges and the index buffers are concatenated), instances; attribute union + typed-zero fill; same-name type conflict and distinct instance sources are errors; empty/unconnected side passes the other through |
| `scene.add` | CPU | places one value in a `Scene` with a `Transform3D`. `object` accepts GEOMETRY / FRAME_BUFFER / SCENE — a scene on that port is the **nesting** case, which is how a transform hierarchy is built; `scene` is the scene to add into and may be unconnected, so a chain accumulates. A frame buffer becomes a `SceneImage` sized from its own resolution (aspect preserved, and the CPU/GPU representation passes through untouched). `translate` / `rotation` / `scale` / `pivot` are Channel3, same keys as `geometry.transform`; `rotation` is Euler degrees, extrinsic ZYX. Never touches `P` — projection is `scene.render`'s |
| `scene.merge` | CPU | union of two scenes, objects and cameras alike; a missing side passes the other through |
| `scene.camera` | CPU | source node emitting a `Scene` with exactly one `Camera` and no objects, so `scene.merge` combines cameras and objects with one node. `position` / `target` are Channel3, `projection` is a `perspective`/`orthographic` enum, `fov` / `ortho_height` / `near` / `far` are Floats (an unknown `projection` falls back to perspective rather than failing). The aspect ratio is **not** baked in here — it is read from `comp_resolution` when a projection matrix is asked for |
| `attribute.set` / `.promote` / `.transfer` | CPU | copy-on-write Geometry attribute operations, dimension-agnostic (`.transfer` measures distance in three components, so the two sides may differ in dimension). `attribute.set`'s `value` arity follows its `type` (`f32`→Channel … `vec4`/`color`→Channel4); `i32`/`bool`/`string` read `int_value`/`bool_value`/`string_value` |
| `attribute.path_sample` | CPU | absolute arc length → one-point Geometry with P/tangent/normal; a `Vec3` `P` or a `Primitive::Mesh` is an explicit error (`GeometryError::RequiresPlanarP` / `RequiresPathPrimitives`) |
| `shape.rect` / `.ellipse` / `.polygon` / `.star` | CPU | emit `Geometry` (closed path + P column) |
| `shape.custom_path` | CPU | pen-tool path: `points` (`PathPoints`) + `closed` params → Geometry with P + `in_tan`/`out_tan` point attributes; curves are flattened by rasterize (`ravel_nodes::flatten`, 0.25px tolerance), shared by the CPU/GPU paths |
| `scatter.grid` / `.circular` / `.path_array` / `.scatter` | CPU | emit `Geometry` with instance domain (index/P/rot/scale). A 3D source is stamped as-is, but `center_input` and `scatter.path_array` are planar-only and error explicitly on a `Vec3` `P`; `path_array` also rejects a `Primitive::Mesh` since it walks arc length |
| `comp.network` | CPU | layer network boundary: layer-local `EvalContext`, scoped evaluation of the layer's owned network |
| `comp.background` | CPU | fills the composition-sized RGBA f32 buffer from `Composition.background_color`; bottom of every compiled shell chain |
| `comp.transform` | CPU | layer transform channels (degrees) + parent chain, inverse-mapped premultiplied bilinear resample; identity passes through |
| `comp.opacity` | CPU | alpha × layer opacity (layer-local frame); 1.0 passes through |
| `comp.merge.*` | CPU | straight-alpha Porter-Duff over with W3C blend modes; `.adjustment` mixes bg/adjusted by layer opacity (effect strength) and bypasses outside the interval |
| `net.in` / `net.out` | CPU | network interface nodes (REQ-LAYER-002); produce `PortRecord`s (a single-output `net.in` yields the value directly); custom In ports prefer scope bindings over own params, then fall back to the **port's** typed zero |

`rasterize` selection is unchanged: synthetic-flagged nodes use
`RasterizeProcessor::from_node` (CPU zeno reference path) while normal graph
nodes use `RasterizeProcessor::new` and produce `GpuFrameBuffer` directly.
Viewer ad-hoc Geometry finalization also uses the CPU constructor until the
Viewer accepts GPU textures.

Unknown type keys are skipped silently (plugin space).

## ravel-ui — headless shell

- `node_locale` (node_locale.rs): locale *keys* for node types —
  `label_key(type_key)` (`node.<type_key>.label`), `description_key`,
  `param_key`; `user_label(node, &registry)` (a stored label that differs
  from the template default) and `label_or_key(node, &registry)` (rename →
  key → bare type key). This crate only builds keys; `ravel-app`'s
  `node_locale` translates them (`type_label`, `display_label`,
  `description`, `param_description`). Strings live in
  `assets/locales/{en,ja}.toml` under `[node."<type_key>"]`; a registry-scan
  test fails when a builtin template lacks a `label` in either locale.
- `node_search` (node_search.rs): pure filtering/ranking for the node search
  palette — `SearchCandidate { type_key, label, description, category }`
  (strings already locale-resolved by the host) and
  `filter_candidates(candidates, query, category, recents) -> Vec<usize>`,
  which matches the query case-insensitively against label and description
  and ranks label matches above description-only matches, recently used
  types first.
- `CommandId` (command.rs): every user command; string ids like
  `panel.reattach`, menu label keys via `menu_label_key()`.
  `LayerAdd{Solid,Shape,Video,Audio,Null}` map to builtin layer templates via
  `layer_template_key()` (REQ-LAYER-008; a test ties the two sets together).
- Keybindings (keybindings/): `KeyChord { modifiers, key }` (`FromStr` /
  `Display` round-trip `"Cmd+Shift+Z"`) and `KeyBindings { bind ->
  Result<(), ConflictError>, force_bind, resolve, iter, len }`, a chord → command
  map where one chord names one command. Definition files are sectioned
  `<section>.<action> = "<chord>"` documents (`keybindings/parser.rs`) read two
  ways: `parse_toml` / `parse_json` are strict, one bad entry failing the
  document, which is what `default_bindings()` (the embedded
  `assets/keybindings/default.toml`) needs; `overlay_user_toml(base, input,
  reserved) -> Result<UserOverlay { bindings, from_user, skipped },
  KeybindError>` is tolerant, laying a user file over `base` entry by entry — a
  rebound command loses its base chord, a claimed chord is taken from the base
  command that held it, entries are applied in ascending `<section>.<action>`
  order so the first to claim a contested chord keeps it, a command in
  `reserved` is refused with `KeybindError::PanelScoped`, and `Err` is reserved
  for a document that is not TOML at all. `KeyBindings::force_bind` is
  crate-private: outside callers use `bind` and handle `ConflictError`.
- `document` (document.rs): the app-wide document editing state.
  `DocumentStore { document(), apply(doc), commit(doc), undo(), redo() }` —
  the Document snapshot is the undo unit (REQ-LAYER-009); `apply` is the
  live mid-gesture update, `commit` records one step. `NetworkPath
  { comp, layer, subnets }` names a network by ownership path
  (`entered(subnet)` / `truncated(depth)` / `segments()`); free helpers:
  `default_document(frame_rate)` (one empty root composition; the rate is a
  parameter because it is the one field a setting decides —
  `ravel_app::app_settings::default_frame_rate`), `root_composition`,
  `update_composition`,
  `update_layer`, `add_layer`, `remove_layer`, `reorder_layer`,
  `add_layer_from_template(doc, comp, template, &registry)`,
  `add_media_layer(doc, comp, template, &registry, MediaLayerSpec {
  name_base, asset_id, start_frame, out_frame, audio_stream_index })` (the
  media template with `asset_id` bound, placed at the playhead — REQ-UI-010;
  `audio_stream_index: Some(i)` also gives the shell an `AudioSource` for the
  same asset id, which is how a video layer's sound is wired — audio-plan
  unit 4),
  `resolve_network(doc, &path)`, `replace_network(doc, &path, graph)`.
- `AppShell::handle_command(CommandId) -> CommandOutcome` (shell.rs):
  the single headless command entry.
  `CommandOutcome::{Handled, OpenPanel { instance },
  DetachPanel { instance, window_id },
  ReattachPanel { window_id, instances }, ...}` — hosts act on outcomes.
  The shell owns the effective `WorkspaceLayout` (`layout()` /
  `layout_mut()`); panel visibility (`visibility()`) is derived from the
  main window's tree, and focus is tracked per `PanelInstanceId`
  (`set_focused_instance` is the only writer — the host calls it from the
  real focus global before every command; there is no by-kind setter
  outside tests). View toggles (`toggle_panel`) insert absent panels at their
  `PanelKind::default_slot() -> DockSlot` and remove present ones from
  their area — placement no longer depends on the active preset. An insert
  returns `OpenPanel`, and the host moves real GPUI focus to that pane
  (`window_host::focus_pane`): the shell never elects a focused panel
  itself, it only mirrors the host's focus through
  `set_focused_instance`.
- `WorkspaceLayout` (layout.rs): N windows, each one split/area tree;
  `windows[0]` is the main window. Operations: `split`, `close_area`,
  `move_tab`, `detach_to_window`, `close_window`, `duplicate_instance`,
  `insert_instance` (new instance at the kind's default slot),
  `activate_tab`, `remove_instance` (single tab, folding empty areas),
  `replace_main_tree` (preset switch; renumbers instance ids around
  detached windows), `absorb_window` (every instance of a detached window
  back to its default slot in main, ids preserved),
  `adopt(&incoming) -> Vec<WindowLayout>` (installs a layout from outside the
  session — restored or project-embedded — keeping the main window's id,
  renumbering everything else, and returning the windows the host must open),
  `absorb_window` also covers a window the platform refused to open.
  Invariants are enforced on construction and deserialization.
- `ViewStates<T>` (view_state.rs): per-instance view state (zoom, pan,
  display target) keyed by `PanelInstanceId`; `retain_instances(&layout)`
  drops state for destroyed instances.
- `WindowId` / `WindowPlacement` (window.rs): logical window ids and
  on-desktop placement records shared by the layout model and the host.
  `WindowPlacement::is_usable()` gates restoring a hand-editable record onto
  real window bounds.
- `layout_doc.rs`: the persisted layout wire format and the rule that picks a
  session's layout. `LayoutDocument { layout_version, embed_in_projects,
  custom_presets, layout }` (`to_toml` / `from_toml`, `LAYOUT_VERSION`) is
  shared by `<config>/ravel/layout.toml` and the `.ravprj` entry; every read
  failure is a `LayoutDocError` the caller turns into the default layout.
  `LayoutStore::{capture, layout_for_project, embed_in_projects}` holds the
  application default and refuses to overwrite it while a project's embedded
  layout owns the session.
- Named layouts (REQ-UI-005): `AppShell::{save_layout_as, apply_custom_layout,
  remove_custom_layout, restore_layout}` over
  `PresetLibrary::{save_custom, remove_custom, custom_presets}`.
- `panels/` holds per-panel headless state (e.g. `TimelinePanel`: playhead,
  scroll, zoom, expansion — property expansion is keyed by
  `keyframes::PropertyRowId` — solo/mute/lock toggles). `TimelinePanel`
  mirrors the active composition as `Option<Composition>` (`composition()`,
  `comp_id()`, `layer(id)`, `layers()`, `frame_rate()`, `duration_frames()`);
  the layer selection is NOT here — it lives in the host's `LayerSelection`
  global (REQ-UI-013).
- Composition management (REQ-UI-013) lives in `document.rs`:
  `CompositionSettings { name, resolution, frame_rate, duration_frames,
  background_color }` is the settings value (`from_composition`, `fallback`,
  `sanitized` — clamps to a constructible composition, `into_composition`,
  `apply_to` which keeps the layers), plus `add_composition` (adopts the model
  root when the document has none), `duplicate_composition` (fresh comp, layer,
  and node ids), `remove_composition` (moves a dangling `root_comp` to the
  neighbour), `compositions_in_order` / `neighbour_composition` (display order
  is by `CompId`), `unique_composition_name`, `next_composition_name`.
  Bulk layer editing (REQ-UI-013 unit 6) composes into ONE snapshot, so a whole
  selection is one undo step: `update_layers(doc, comp, &[LayerId], f)`,
  `remove_layers` (skips locked layers), `duplicate_layers` (returns the new
  document plus the copies in source order).
  `properties::composition` turns the same settings into `PropertyField`s
  (`sections_for_composition`, `composition_fields`, `apply_composition_field`,
  `frame_rate_from_fps` — keeps 29.97 as `30000/1001`).
- `panels::outliner` (panels/outliner.rs) flattens a whole `Document` into
  `Vec<OutlinerRow>` for the Outliner tree (REQ-UI-013):
  `OutlinerPanel::rows(document)`, with `OutlinerRow { depth, kind, label,
  expandable, expanded }` and `OutlinerRowKind::{Comp, Layer, Node { subnet,
  reference }, UnusedGroup { count }}`. Node rows walk upstream from
  `net.out` in input-port order; an already-emitted node becomes a
  `reference` leaf and unreachable nodes land in `UnusedGroup`. Expansion is
  keyed by `OutlinerKey::{Comp, Layer, Node, Unused}` and stored as the
  difference from per-kind defaults (comps and node chains open, layers and
  the unused bucket closed), so rows that do not exist yet already have the
  right state. The panel holds no selection — that is the host's
  `LayerSelection` / `CanvasSelection`.
- `panels::media_bin` (panels/media_bin.rs) flattens `Document::media_assets`
  into `Vec<MediaBinRow>` for the MediaBin list (REQ-UI-008, media-import plan
  unit 4): `MediaBinPanel::rows(document)` applies the kind filter
  (`MediaBinFilter::{All, Video, Still, Audio}`) and a case-insensitive
  substring name search, sorted by name. `MediaBinRow { asset_id, name, kind,
  duration, offline }` carries everything the row paints. `classify(entry)`
  decides the `MediaBinRowKind::{Video, Still, Audio}` category (a container
  is audio only with audio streams and no probed video stream; a sequence is
  video). `asset_references(document, asset_id)` lists every layer still
  using an asset (media node binding or shell audio source) for the delete
  confirmation. The panel holds no selection — that is the host's
  `MediaSelection`.
- `keyframes` (keyframes.rs): the timeline property-tree model and keyframe
  editing (REQ-LAYER-004). `PropertyRowId::{Shell(PropertyGroup), Network
  { node, key }}` identifies a channel group; `property_rows(layer)` lists
  the shell groups plus every keyframed parameter of the layer's
  **top-level** network (In custom params and subnet-promoted params
  included; nodes inside subnets are keyed via the node editor's subnet
  context and are not listed — v1). All edit frames are layer-local:
  `layer_local_frame(layer, comp_frame)` /
  `comp_frame_for_key(layer, local)`. Edits rebuild the layer immutably:
  `insert_keyframe` (converts a constant channel), `remove_keyframe` (the
  last key reverts to a constant), `move_keyframe`, `set_channel_value`
  (keys animated channels preserving interpolation/tangents),
  `set_keyframe_tangent`, `set_keyframe_interpolation`,
  (Bezier conversion seeds zero-length segment handles at one third while
  preserving the linear shape and any saved non-zero tangents),
  `set_curve_value` for the bare curve, `preview_keyframe_move` /
  `preview_keyframe_moves` / `preview_keyframe_moves_with_value_delta` /
  `preview_keyframe_tangent` (baseline-derived drag previews),
  `row_channels`, `has_keyframe_at`. `document::duplicate_layer`
  deep-copies a layer above its source with fresh ids
  (`Graph::duplicate_with_fresh_ids` / `Layer::duplicate_with_fresh_ids`
  remap edges and `ChannelSource::NodeOutput` bindings — NodeIds are
  globally unique across the document).
- `properties/`: `PropertySection { title, fields }` where `title` is a
  locale key; `PropertyField::{Float, Int, Bool, String, Enum, Color, Vector,
  Curve, ReadOnly, PortList}` keyed by stable identifiers (`Curve` carries a
  whole `CurveParam`; the panel renders it as a thumbnail row that expands
  `widgets::param_curve_editor` inline, and which rows are open is panel view
  state that never enters the Document). `PortList { key, side, rows:
  Vec<PortRow { name, port_type, fixed }>, options }` is the odd one out: it
  describes a network interface node's SHAPE, not a value, so it never travels
  through `PropertyValue` — the host routes its edits to
  `network::{add,remove,rename,move}_custom_port` and
  `network::set_custom_port_type` instead of to a parameter write. Fixed ports are rows too (`fixed` marks them read-only), so
  the list matches the node on the canvas. Builders: `sections_for_node(node,
  &registry, frame, driven, NetworkContext)` (samples animated channels at the
  layer-local frame; the context reaches only `node_ports_section`, which
  returns `None` for anything but `net.in` / `net.out`. Collapse a
  `NetworkPath` with `NetworkPath::context()`),
  `sections_for_layer(layer, &ctx, audio_asset: Option<&AssetMetadata>)`
  (evaluates transform channels in layer-local time; includes the In node's
  custom parameters as `custom.<name>` fields, REQ-LAYER-002; `audio_asset`
  is the metadata of the asset the layer's `AudioSource` points at, resolved
  by the caller — it only feeds the Audio section's stream picker options,
  `layer::parse_stream_index` reads the container index back out of the
  selected option, and nothing here ever probes a file),
  `sections_for_layers(&[&Layer], &ctx)` for a multi-layer selection (count plus
  the shell fields, all `ReadOnly`, differing values shown as `MIXED_VALUE`, a
  merged boolean as the locale key `VALUE_ON` / `VALUE_OFF` which the panel
  translates — this crate has no i18n dependency; a one-element slice still
  renders the read-only multi view). Reverse mapping:
  `layer::apply_layer_field(&mut Layer, key, &PropertyValue, local_frame)`
  (shell attributes + `custom.*` In parameters; animated channels are keyed
  at `local_frame`, not flattened), `layer::toggle_layer_keyframe` /
  `layer::layer_field_keyframed` for the per-field key toggle,
  `layer::in_node_id`.

## ravel-dock — docking UI

- `DockRoot` (dock.rs): GPUI entity rendering one window's `LayoutNode`
  tree — split containers with draggable separators, a `TabBar` per area
  with an overflow menu, an empty-area placeholder. Construct with
  `DockRoot::new(root, Rc<dyn PaneContent>, cx)`; replace the tree with
  `set_layout(root, cx)` after applying events. Owns no focus; drags cancel
  on Escape through a keystroke observer.
- `PaneContent` (content.rs): host-supplied pane contents —
  `tab_title(instance)`, `view(instance)` (must return a stable view per
  instance id), optional `tab_icon(instance)` (goes in the tab's prefix slot,
  since `Tab::icon` would replace the label) and `empty_state()`. ravel-dock
  never branches on `PanelKind` itself. `panels::PanelViews` is the
  implementation.
- `DockEvent` (dock.rs): `SplitRatioChanged { path, ratio }` (emitted once
  when a splitter drag ends), `TabActivated { instance }`,
  `TabDropped { instance, anchor, zone }`,
  `TabDetachRequested { instance, screen_position }` (a tab released outside
  the window; the host hit-tests `screen_position` against its open windows and
  either moves the tab into the window it lands on or opens a new one), and
  `AreaActionRequested { instance, action }`. The host applies them to its
  model and pushes the tree back.
- Appliers (path.rs): `set_ratio_at(&mut node, &path, ratio)` /
  `activate_tab(&mut node, id)` on one tree;
  `apply_tab_drop(&mut WorkspaceLayout, window, instance, anchor, zone)` and
  `apply_area_action(&mut WorkspaceLayout, window, instance, action)` on the
  workspace (all-or-nothing, `Result<_, LayoutError>`).
  `tab_drop_changes_layout` is the no-op predicate, `lead_split_child` the
  reordering a left/top drop needs.
- `AreaAction` (dock.rs): `SplitRight`, `SplitDown`, `DuplicateRight`
  (duplicates the instance first, so a lone tab can still split), `Close`.
  Labels come from `dock.area_menu.*` in the locale assets.
- `NodePath` / `SplitSide` (path.rs): split addressing from the root
  (`NodePath::root().child(SplitSide::First)`), `node_at` for lookups.
- `layout_math`: px conversion between ratios and container spans
  (`split_sizes`, `ratio_from_position`, `SPLITTER_PX`,
  `splitter_thickness`) plus drop-zone geometry (`DropZone`, `drop_zone`,
  `drop_highlight`, `DROP_EDGE_FRACTION`, `DEFAULT_SPLIT_RATIO`).
- `examples/gallery`: validation binary — four built-in presets over a real
  `WorkspaceLayout`, dummy panes, theme toggle. Run with
  `cargo run -p ravel-dock --example gallery`.

## ravel-app — GPUI host rules (see `.agents/rules/gpui.md`)

- One command path: KeyBinding/menu/button → GPUI Action → nearest
  `on_action` → unhandled falls through to App-level handlers →
  `RavelWorkspace::dispatch_command()`. Add commands ONLY by extending
  `CommandId` + the `for_each_command!` table in `workspace.rs`.
- Panels: constructors take `(instance: PanelInstanceId, window, cx)`; focus via
  `track_panel_focus(instance, &focus_handle, window, cx)` (panels/mod.rs) which
  syncs `FocusedPanelGlobal`. Never grab focus in mouse handlers or render.
  `panels::build_panel_view(&PanelInstance, window, cx) -> AnyView`
  (module-private) is the only place a pane view is created; `PanelViews`
  caches per `PanelInstanceId`, `view_id` exposes the entity id for tests, and
  `retain(&live)` drops the views of destroyed instances.
- Windows: `window_host::WindowHost` (window_host.rs) is the uniform host —
  title bar + `ravel_dock::DockRoot` for one logical `WindowId` + the dialog
  and notification layers. `window_host::{open, close, close_all_detached,
  set_detached_minimized, open_restored}` drive window lifecycle (`open` takes
  the window's `&WindowLayout`, so its `always_on_top` and its restored
  `placement` apply from the first frame; `open_restored` opens the windows a
  restored layout brought and absorbs any the platform refuses);
  `window_host::focus_pane(instance, cx)` gives one pane the real keyboard
  focus wherever it is docked (deferred, so it is safe to call from inside a
  window's update — this is how an opened panel gets the focus);
  `WindowRegistry` (Global)
  maps `WindowId` → `AnyWindowHandle` for every window, main included
  (`handle`, `window_id_of`, `main`, `detached`, `window_bounds`). Every host
  observes its own window's bounds and records them into the layout
  (`layout_persist::record_placement`) without any I/O.
- Layout persistence: `layout_persist` (layout_persist.rs) owns the
  `LayoutPersistence` global around `ravel_ui::layout_doc::LayoutStore`.
  `install(cx)` reads `<config>/ravel/layout.toml` once during bootstrap and
  `restore_into(shell, doc)` installs it; `save` writes on the background
  executor after every command, `save_blocking` on teardown (a spawned task
  would never be polled). `layout_for_project` / `document_for_embedding` are
  the two ends of the `.ravprj` opt-in. Anything unreadable degrades to the
  default arrangement — a layout must never cost a launch.
  `workspace_layouts::WorkspaceLayoutsForm` is the Manage Layouts dialog body
  (named layouts + the embed toggle; the platform's own Save dialog cannot host
  the control).
- Window chrome: `title_bar::RavelTitleBar` (title_bar.rs) is the one title bar
  every window draws — `new(center_label)` plus `leading()` / `trailing()`
  slots over `gpui_component::TitleBar`. It owns the centering correction
  (`WINDOW_CONTROLS_INSET` and the trailing controls' width), so nothing else
  pads a bar by hand. The main window fills the leading slot
  (`title_bar::render_main_title_bar`), a detached window the trailing slot with
  the always-on-top pin, which writes `WindowLayout::always_on_top` through
  `AppShell` and mirrors it with `Window::set_always_on_top`.
- Durable globals only (`SelectedPropertiesTarget`, `FocusedPanelGlobal`,
  `WindowRegistry`, `ActiveComposition`, `LayerSelection`,
  `CanvasSelection`, `ToolState`, `MediaSelection`); component events use `EventEmitter` +
  retained `Subscription`s. Do not add one-shot event globals. Node parameter
  edits are the single-receiver exception: Properties defers a direct call to
  `NodeEditorPanel::apply_property_change` through the durable
  `NodeEditorHandle`, keeping cross-window Entity updates outside the source
  window's update.
- `ActiveComposition(Option<CompId>)` (panels/mod.rs) is what the UI shows —
  Timeline, viewer evaluation, the playback clock, and Properties all resolve
  through it, never through `Document::root_comp` (which stays the model root
  a reopened document starts on). `ProjectState` is its only writer:
  `set_active_composition(comp, cx)` switches, drops the compiled chain, and
  re-evaluates; `active_composition(&self, cx)` resolves it in the live
  document. `None` (composition 0) is a real state — every consumer draws an
  empty state (REQ-UI-013).
- `LayerSelection` (panels/mod.rs) holds the selected layers in selection order
  (the anchor first); `panels::{layer_selection, selected_layer,
  set_layer_selection, clear_layer_selection}` are the accessors. Invariant:
  `LayerSelection.comp == ActiveComposition` — the writers stamp the active
  composition and a switch resets the selection. A `PropertiesTarget::Layer` /
  `Layers` no longer matching the selection is dropped by the same writers; a
  `Nodes` target belongs to the node editor and is never stolen.
- `ProjectState::document_changed` prunes the layer selection after EVERY
  document change (`panels::prune_layer_selection`): selected layers the document
  has lost leave the selection, and a Properties target that was showing the
  selection is republished. No panel has to exist for that to hold — the `motion`
  and `node` workspaces have no Timeline.
- `MediaSelection` (panels/mod.rs) holds the selected media assets
  (`media_assets` keys, click order) for the MediaBin (REQ-UI-008);
  `panels::{media_selection, set_media_selection}` are the accessors, and
  `set_media_selection` is the only writer — it also publishes the Properties
  subject (`PropertiesTarget::MediaAsset { id }` for one asset, `Empty`
  otherwise). `PropertiesTarget::MediaAsset` only identifies the subject; the
  Properties panel shows a placeholder until media-import plan unit 6.
  `ProjectState::document_changed` also calls `panels::prune_media_selection`
  after every document change, dropping selected assets (and a stale
  `MediaAsset` target) the document has lost. The MediaBin panel
  (panels/media_bin.rs) rebuilds rows from `ravel_ui::panels::media_bin`
  outside `render()`, kicks `ThumbnailCache::get_or_request` on rebuild and
  decodes ready PNGs on cache notification (kind-icon fallback otherwise),
  and routes row operations through free functions:
  `add_asset_as_layer` / `new_composition_from_asset` (both reuse the unit-3
  `ProjectState::import_media` path) and `request_delete_asset` /
  `delete_confirmation` (in-use assets confirm with the reference count).
- Multi-selection (REQ-UI-013 unit 6): a modified click's meaning is headless in
  `ravel_ui::panels::layer_selection` —
  `LayerClickMode::from_modifiers(shift, platform)` (Shift ranges, the platform
  modifier toggles, Shift wins) and `layer_selection_after_click(current, order,
  clicked, mode)`, where `order` is the composition's stack order. Both writers
  (`TimelineGpuiPanel::select_layer_with_mode`,
  `OutlinerGpuiPanel::select_layer_with_mode`) compute through it and then call
  `panels::publish_layer_properties_target(cx)`, which publishes `Layer` for one
  and `Layers` for several. `LayerClickMode::is_additive()` suppresses the
  gestures a modified click must not start (bar move/trim, row reorder), and a
  right click keeps a selection that already holds the row
  (`select_layer_for_menu`). Bulk edits go through `operation_targets(row)` in
  both panels — the whole selection when the row is part of it, else that row —
  and the clicked row decides a flag's new value; the Timeline's S/M/L and
  disclosure controls `stop_propagation()` so a bulk toggle does not collapse the
  selection.
- Viewer selection overlay: a node selection draws bboxes with transform handles;
  a selection of two or more layers draws one handle-less bbox per layer
  (`layer_comp_rect` = union of the layer network's shape-node bounds, shell
  transformed — `None` for a layer with no measurable geometry). Dragging inside
  one of those bboxes moves every selected layer: `MoveDrag { targets }` holds one
  `MoveTarget` per network (each with its own layer-local frame), every target's
  edit lands in one document, and the gesture commits once (REQ-UI-013 unit 6).
  Layers whose shell transform is not identity keep their bbox but do not move —
  the drag writes comp-space deltas into the layer-local `center` vector.
- The node editor's open network FOLLOWS `LayerSelection` and opens only for a
  selection of exactly one layer: nothing selected and several layers selected
  are the same closed state (with different center messages), and closing clears
  `CanvasSelection` so no stale node — or Viewer bbox reading it — points into
  the abandoned network. It observes the global; no panel pushes at it.
  `open_network(path)` keeps a
  `CanvasSelection` that already names `path`, so a writer can select nodes of
  a not-yet-open network (the Outliner node rows) and the selection survives
  the switch; `center_on_node(path, node)`, `open_and_fit(path)`, and
  `enter_subnet_at(path, node)` are the view-movement entry points. With no
  node selected the editor only withdraws its own `Nodes` target — it never
  blanks a `Layer` target the selection writers own.
- Composition commands (`CompositionNew` / `CompositionSettings` /
  `CompositionDuplicate` / `CompositionDelete`) run in `RavelWorkspace`;
  `ProjectState::{create_composition, apply_composition_settings,
  duplicate_composition, delete_composition}` are the one-undo-step document
  operations (create and duplicate also switch the active composition; delete
  hands over to the neighbour). `panels::command_target_composition(cx)` is the
  single rule for *which* composition they act on: the Properties composition
  target (an Outliner composition row) else the active composition — so the
  menu, the Outliner header buttons, and the row context menu all dispatch the
  same Action. `ProjectState::new_composition_defaults(cx)` is the one place the
  initial values of `Composition ▸ New…` are decided, and therefore the one place
  the precedence lives: the active composition's format, else the resolved
  default frame rate over `CompositionSettings::fallback`. An active composition
  *hides* the setting — it decides `File ▸ New`'s root composition and the case
  with nothing active.
- Outliner layer operations (REQ-UI-013 unit 5, `panels/outliner.rs`): row
  `on_mouse_move` decides the reorder target (no coordinate math; a node or
  Unused row lands on its owning layer), the drag applies live and commits once
  on mouse-up, and the row context menu calls the panel's
  `begin_rename`/`duplicate_layer`/`delete_layer` directly — `EditDelete` and
  friends mean "the focused thing", which is not the row under the cursor.
  Operations are limited to the active composition's rows, and locked layers
  offer no Rename/Delete.
- Dialogs: `window.open_dialog` / `open_alert_dialog` need the host to render
  `Root::render_dialog_layer(window, cx)` (see `RavelWorkspace::render`) —
  without it a dialog is open and invisible. A plain `Dialog` also renders no
  buttons of its own: build the footer with `DialogFooter` + `Button`
  (`button_props` only reaches the footer `AlertDialog` builds).
  `composition_form::CompositionForm` is the shared New/Settings form; it
  returns edited settings on demand so nothing touches the document until the
  dialog is confirmed.
- `SelectedPropertiesTarget` only IDENTIFIES the Properties panel target
  (`PropertiesTarget::Layer { comp_id, layer_id }`,
  `PropertiesTarget::Layers { comp_id, layer_ids }` — read-only in v1,
  `PropertiesTarget::Nodes { network: NetworkPath, ids }`, or
  `PropertiesTarget::Composition { comp_id }` — plain fields, no keyframes) — it never
  carries value snapshots. The panel resolves live values from the
  `ProjectState` document (`resolve_network` for nodes, composition layer
  lookup + `PlaybackPosition` for the frame) and observes the `ProjectState`
  entity and `PlaybackPosition` directly, so any document change or
  playhead move refreshes displayed values in place without a republish.
  A layer with `audio.is_some()` additionally exposes the Audio section:
  keyframe-capable gain plus fade-in/out frames, audio mute, and stream index.
  Gain uses the same shell-channel preview/commit and local-frame path as opacity.
  Publishers: timeline layer selection, node editor selection
  (`notify_properties_selection`).
- Never `update()` another window from within a window update — defer with
  `cx.defer` (see `window_host::close`).
- Category tints: `port_colors::category_color` maps `NodeCategory` onto the
  port colour of its domain's data type, so a node's header matches the port
  dots of the data it deals with. A pair table in that file's tests pins the
  1:1 mapping.
- Port colors and silhouettes: `node_editor/port_colors.rs` maps `DataTypeId`
  → Hsla and → `PortShape` (`Circle`, `RoundedSquare` = FRAME_BUFFER,
  `Diamond` = GEOMETRY, `Triangle` = FIELD, `Hexagon` = SCENE); add an arm for
  a new data type or it falls back to a gray circle. A new `PortShape` variant
  also needs an arm in `painting.rs::paint_port_marker`.
- Hover popover: `node_editor/hover_popover.rs` holds the node info popover
  (DISC-2) — `HoverPopover` (dwell state machine, `HOVER_DWELL` = 500 ms,
  generation-countered so stale timers and gestures cannot open it),
  `hover_info(node, &registry, frame)` (document-derived content model;
  samples animated channels at the frame, never issues evaluation), and
  `hover_popover_element(...)` (gpui-component `Popover` in controlled mode,
  wrapped in a zero-size, absolutely positioned div — the popover resolves
  its anchor from the wrapper's prepaint bounds, so the wrapper sits at the
  node's screen position and canvas hit testing is untouched).
  `data_type_name` localizes port types via `node_graph.popover.port_type.*`;
  add a locale key for a new data type or it renders `#<raw>`.
- GPUI integration tests live in `crates/ravel-app/tests/` using
  `#[gpui::test]` + `TestAppContext` (see `command_dispatch_repro.rs` for
  the workspace harness and app-level action routing).
- Document state: `ProjectState` (`src/project_state.rs`) is the single
  owner of the live `Document`, the Document-level undo stack, and the
  background `EvalService`; the workspace creates it and registers the
  durable `ProjectStateHandle` global. All edits flow through
  `apply_document(doc, hint, cx)` (live) / `commit_document` (one undo
  step); `undo`/`redo` are routed here by the workspace when no panel
  intercepts `EditUndo`/`EditRedo`. The Viewer always evaluates the root
  composition output (`compile_composition` + Document-aware requests,
  REQ-LAYER-007); `request_viewer_eval(hint, cx)` posts one request at the
  shared `PlaybackPosition`. Eval results publish `ViewerFrame::{Blank {
  composition_resolution }, Frame { image, composition_resolution }, Error
  { message, composition_resolution }}`; the full composition resolution is
  deliberately separate from the reduced evaluation buffer so Viewer viewport
  geometry remains exact. The evaluation buffer size is
  `ViewerResolution::apply(comp.resolution)`
  (`ravel_ui::panels::viewer::ViewerResolution::{Full, Half, Quarter}`,
  default `Half`, `div_ceil` per axis) — a factor of the composition
  resolution rather than an absolute cap (REQ-UI-004). `ProjectState::{
  viewer_resolution, set_viewer_resolution}` read and choose it; the setter
  re-evaluates with `InvalidationHint::None` because the evaluator's cache
  identity already carries the target resolution
  (`CacheMiss::ResolutionChanged`). It is view state and never reaches the
  `.ravprj`. The `image` is a `panels::ViewerImage` — a
  straight-alpha BGRA `RenderImage` plus the evaluation buffer's dimensions —
  converted from the evaluated `FrameBuffer` by
  `ViewerImage::from_frame_buffer` **on the evaluation worker thread**
  (`EvalService` invokes its result callback there), so publishing a frame
  costs the UI thread an `Arc` move rather than a per-pixel conversion. Results also merge per-node durations into the
  `NodeEvalTimings` global (node editor load readout: muted < 8 ms, yellow <
  33 ms, red beyond; hidden while a node
  is bypassed — the pass-through records no timings). Only nodes the document
  still holds are kept: synthetic compositing nodes are never stored, a
  structural change drops what it deleted, and a document replacement clears
  the global (node ids are reused across documents), so the global never
  outgrows the document and no readout is inherited by a reused id.
  `disable_background_eval_for_tests()` keeps gpui tests deterministic.
- Settings apply path (`src/app_settings.rs`, `SET-1`): the one route from a
  settings file to running behaviour. `read_global_settings()` →
  `GlobalSettingsFile::resolved()` (before the GPUI app exists, so the locale is
  active before the first `t!`), `apply_startup_locale(locale_dir, requested) ->
  LocaleOutcome::{Applied, FellBack, Unavailable}` (`init` on `DEFAULT_LOCALE`
  = `"en"` then `set_locale`, so an unknown locale warns and falls back instead
  of leaving the app with no catalogs), then `install(file, cx)` publishes the
  durable `AppSettings` global. Read with `resolved(cx) -> ResolvedSettings` and
  `layer(scope, cx) -> SettingsLayer` (the per-layer overrides a dialog shows as
  "customized"), and `default_frame_rate(cx) -> FrameRate` (the resolved
  `playback.frame_rate` as a rate a composition can be built with;
  `parse_frame_rate(text)` is the notation — an fps number or an exact rational,
  `None` for anything else — exposed so a control never offers a value the reader
  would ignore). Write with `update(SettingsScope::{Global, Project}, |layer|
  …, cx)`: one field per call, `None` removes the override so the value falls
  back to the layers below, and only the edited layer is persisted — `Global`
  is written atomically off the UI thread to `<config>/ravel/settings.toml`
  (failure emits `ProjectEvent::SettingsSaveFailed`), `Project` travels in the
  `.ravprj` and marks the project dirty (`ProjectState::mark_settings_changed`).
  `set_project_layer(layer, cx)` is called only from the document replacement
  path, so a project's overrides start applying when it opens and stop when it
  is replaced. Resolution is `default → global → project`; the `user` layer has
  no store yet. Applying is per-subsystem and only for values that moved:
  `ravel_i18n::set_locale` plus one `cx.refresh_windows()` for the locale, and
  `apply_resolved_appearance(cx)` for the theme — the latter also run whenever
  `ThemeRegistry` changes (`install` observes it), because the themes directory is
  read asynchronously and re-read on every file change, so a theme the settings
  name may not be in the registry yet; `watch_dir`'s `on_load` fires only once and
  cannot stand in for that.
  It hands `Theme::{light_theme, dark_theme}` the registry entries named by
  `ResolvedSettings::{light_theme, dark_theme}` (never copied colours: the
  registry re-resolves those slots *by name* on reload, which is what makes theme
  files hot-reload) and then `Theme::change` / `sync_system_appearance` per
  `theme_mode`. A theme name nothing carries — or one whose `mode` does not match
  the slot — falls back to the bundled theme for that mode, while the resolved
  settings keep the requested name so a theme file arriving later is still worn.
- Settings dialogs (`src/settings_dialog.rs`): `SettingsScope::{Preferences,
  Project}` is the *screen* (and therefore the layer written), not
  `app_settings::SettingsScope`. `SettingsPageKind::{Appearance, Language,
  Keybindings, Project}`; a page carries fields only once its settings apply, and
  `Settings` hides a page with no item. `fields_for(kind, cx) -> Vec<PageField>`
  is the rows a page shows (`title_key`, `description_key`, `field`) and the seam
  the dialog builds its groups from — a test can ask a `PageField` for
  `is_resettable(cx)` / `reset(window, cx)`, which a finished `SettingItem` no
  longer answers. `label_keys(kind)` is every i18n key a page's fields render.
  Fields bind `SettingField::on_reset(is_dirty, reset)` —
  `is_dirty` = this layer holds a value, `reset` = remove it — and never
  `default_value()`, which would write the default as an explicit override.
  Language options come from `ravel_i18n::available_locales()` (arbitrary order,
  so sort) labelled with `ravel_i18n::locale_display_name(code)`, each locale's
  own `language.name`. The Project page's default frame rate is a closed
  dropdown of the common rates plus whatever the file already holds *if it
  parses* (`app_settings::parse_frame_rate`), so no unreadable value can be
  written; it shows the rate **in force** (which may come from the global layer)
  while its reset follows the project layer alone — that difference is how
  "the project overrides the preference" is visible.
- `workspace::PANEL_BINDINGS` / `panel_bound_commands()`: the single table of
  bindings that exist only in code because the asset format cannot express a key
  context. `PanelBinding { command, chord, panel, context }`, in registration
  order. Two readers — `build_keybindings` registers it, `keybindings::rows`
  lists it — and it is also the reserved set a user file is refused, so adding a
  panel shortcut makes it un-overridable in the same edit. Never call
  `KeyBinding::new` for a panel shortcut outside this table.
- Keybinding load path (`src/keybindings.rs`, `SET-5`): the only reader of
  `<config>/ravel/keybindings.toml` (`paths::global_keybindings_path()`).
  `read_keybindings()` / `read_keybindings_at(Option<PathBuf>)` lay the user file
  over the embedded defaults and return `LoadedKeybindings { bindings(),
  user_file() }`; a missing file is silent, an unreadable one warns and yields
  the defaults, and a single bad entry warns and is skipped. `main` puts
  `bindings()` on the `AppShell` — never straight into `cx.bind_keys` — so
  `workspace::build_keybindings` gives user chords the same `!Input` context
  every asset chord gets (`MED-APP-16`), then `install(loaded, cx)` publishes the
  durable global. `rows(&loaded)` / `current_row(command, cx)` produce
  `KeybindingRow { command, chord, panel_chords, origin }` with
  `KeybindingOrigin::{Default, User, Panel, Unassigned}` for the read-only
  Preferences list — `Panel` and `PanelChord { chord, panel }` come from
  `workspace::PANEL_BINDINGS`, so a Viewer-only shortcut reads as bound rather
  than unassigned. Editing is `SET-12`, so the global is written once at
  startup.
- Persistence: `.ravprj` format v7 — the format itself lives in the GUI-free
  `ravel-project` crate (`crates/ravel-project/`), and `ravel-app` only drives
  it from `ProjectState`. A zip of
  `manifest.json` (format_version drives the `migration` chain),
  `document/main.ron` (the full `Document`, deterministic RON),
  `settings.toml`, `ui_state.json`, `workspace_layout.toml`; saving writes a
  `.bak` of the previous revision. `ProjectFile::{new, from_document, to_archive,
  to_archive_for_root, from_archive, save, load}`; the layout is selected by the
  source version (v3+ requires `document/main.ron`), and a v1/v2 archive (flat
  `graph/main.ron` only) wraps the graph in a fresh Document (root comp
  from the manifest's resolution/frame rate). Every load runs
  `Document::validate()` (structural invariants: root presence, comp id
  consistency, non-zero frame rate, unique layer ids, resolved
  parent/track-matte refs) and advances the id counters (REQ-LAYER-009).
  An archive older than v5 additionally runs
  `Document::fold_component_params()` after the counters are advanced: v5
  folded the `_x` / `_y` component parameters into Channel2/Channel3, and that
  change lives in `document/main.ron`, which the untyped `migration` chain
  never sees. An archive older than v6 likewise runs
  `Document::upgrade_curve_params()`: v6 replaced the `"0:0,1:1"` string that
  held `field.curve_remap`'s control points with a `ParameterValue::Curve`.
  It reproduces the v5 reader — unreadable entries are dropped one at a time
  (only a string with *no* readable point becomes `CurveParam::identity()`),
  order does not matter, and a repeated input keeps its last point — logging
  whatever it drops.
  **v7 has no such typed pass**: it only adds `Document.exposed_parameters`
  (the exposed parameter declarations, REQ-PROJ-006), which `#[serde(default)]`
  reads as an empty set from any older document, so `migrate_v6_to_v7` and
  `from_archive` merely advance the stamp. The version was still raised because
  an older build would drop the declarations and write the document back
  without them — a contract other tools read by name, lost invisibly; the bump
  turns that into `MigrationError::TooNew`.
  Both walk every graph of the document (flat graph, layer networks, nested
  subnets) through the shared `composition::graph_walk` traversal.
  `Layer.audio: Option<AudioSource>` is an additive format-v4 field: it does
  not introduce a migration step. Missing `audio` reads as `None`, and
  all `AudioSource` fields have serde defaults. With `struct_names(true)`,
  the present form is `Some(AudioSource(...))`.
  `ui_state::UiState` (`ui_state.json`) holds UI state that must stay out of
  the undo history and the document diff — currently `active_comp`
  (REQ-UI-013). The entry is optional in both directions (missing entry →
  defaults, unknown fields ignored, unreadable content → defaults + a
  warning, never a failed load), which is why it does NOT bump
  `format_version`; add future UI state as `#[serde(default)]` fields.
  `UiState::initial_active_comp(&document)` is the single fallback rule
  (persisted id while it resolves, else `root_comp`).
  `ProjectFile.workspace_layout: Option<LayoutDocument>`
  (`workspace_layout.toml`) is the same kind of optional entry, and additionally
  **opt-in**: it is written only while the user turned the toggle on, so an
  ordinary save produces the archive it always did. Unreadable or
  future-versioned content reads as `None`.
  Asset references (REQ-PROJ-001): `Document.media_assets` holds
  `ravel_core::composition::MediaAssetEntry { path: AssetPath, kind:
  AssetKind, metadata: AssetMetadata, #[serde(skip)] resolved:
  Option<PathBuf> }`. `AssetPath` (`Absolute`/`Relative`/`Variable`)
  persists as one string, so a v3 entry (`{ path: PathBuf }`) reads back as
  `Absolute` with its kind inferred from the extension — v4 needs no
  document rewrite, and it dropped `assets/refs.json` (always written
  empty; a leftover entry is ignored). `save` narrows each reference
  against the directory holding the `.ravprj`
  (`Document::with_relativized_assets`, driven by `resolved`) and `load`
  reverses it (`Document::with_resolved_assets`); `project_root_of(path)`
  is the shared anchor rule. Evaluation reads **only** `resolved` —
  `resolved == None` is offline. `from_archive` alone resolves just the
  absolute references, since it does not know where the archive lives.
  `ravel_project::timestamp::rfc3339_now()` supplies wall-clock stamps without a
  chrono dependency. `ProjectState` owns the open project:
  `project_path()`, `is_dirty()`, `new_document`, `save_project_to(path, cx)`,
  `save_project_to_then(path, completion, cx)`,
  `load_project_from(path, cx)` (file I/O on the background executor;
  loading replaces the document and undo history wholesale; generation /
  revision guards make an in-flight save or load harmless when the user
  edits or replaces the document meanwhile). Dirty state compares the live
  revision with the revision of the last completed save; the completion hook
  reports `Saved` only when no later edit remains, and remains attached to its
  own request through the FIFO save queue. New/load establish a clean baseline,
  including the startup document. The File menu commands (New/Open/Save/Save
  As) route through the workspace's `CommandOutcome::Delegate` arm with GPUI
  path prompts. Dirty New/Open/Quit/window-close actions use a Save / Discard /
  Cancel dialog; Save resumes the action only after `SaveOutcome::Saved`, while
  failure, supersession, or an edit made during the save preserves the document.
- Media import (REQ-UI-010): `CommandId::FileImport` (File ▸ Import…, Cmd+I,
  multi-select dialog) and OS file drag-and-drop both funnel into
  `media::import::import_paths(paths, cx)` (`src/media/import.rs`). The
  workspace root accepts gpui's `ExternalPaths` drops (`can_drop` + `on_drop`),
  which the platform file-drop events are translated into. Probing runs on
  the background executor through the injectable `MediaProber`
  (`ravel-media` `probe` + `detect_sequence` behind the `ffmpeg` feature);
  `probe_path` classifies each file into `AssetKind` (multi-frame sequence →
  `Sequence` with the composition frame rate as its metadata default, still
  extension → `Still`, otherwise a container that must probe or be skipped).
  A container's audio streams are recorded in
  `AssetMetadata.audio_streams` (container stream index + codec + rate +
  channels), which is what the Properties stream picker lists.
  `ProjectState::import_media(probed, skipped, cx)` then applies the whole
  batch as ONE `commit_document` (one undo step): assets are relativized
  against the project root, an already-registered absolute path reuses its
  asset id, and each asset gets a media layer at the playhead
  (`start_frame = PlaybackPosition.frame`,
  `out_frame = ceil(duration_secs × comp_fps)`, falling back to the
  composition length). Composition settings are never touched (decision 5).
  Audio (audio-plan unit 4): a file with audio also gets an `AudioSource` on
  the shell, bound to the same asset id with
  `metadata.first_audio_stream_index()` — silent media leaves `Layer::audio`
  as `None` (nothing ever scans a network for "an audible media node"), and a
  container with sound but no picture uses the frameless `audio` template
  instead of a `media` node with no video stream to decode. The audio source
  is part of the same commit, so the import stays one undo step.
- Node editor: edits one network at a time, addressed by
  `ravel_ui::document::NetworkPath` (REQ-LAYER-011): Timeline layer selection
  opens that layer's network via `NodeEditorPanel::open_network`,
  double-clicking a subnet node dives deeper, the breadcrumb bar returns to
  ancestors, and `NodeMetadata.synthetic` nodes are filtered from painting
  and every hit test. Graph edits are spliced into the document with
  `replace_network` and committed to `ProjectState`.
  `toggle_param_keyframe(node, key, cx)` adds/removes a key at
  `current_local_frame()` (the playhead in the owning layer's local time);
  parameter scrubs keep channel parameters animated (a keyframed channel
  gets a key at the current frame instead of flattening to a constant).
- Timeline: mirrors the document's root composition; layer add (menu or
  context-menu Add Layer submenu), duplicate (context menu,
  `document::duplicate_layer`), delete (`EditDelete` / context menu,
  locked layers protected), reorder (header drag), move/trim (bar drag
  with in/out handles), solo/mute/lock all commit Document undo steps.
  Layer selection publishes the Properties target and makes that layer's
  network active in the Node Editor; deselection closes the network. The
  property tree lists the shell channels (including Audio Gain when present) plus
  keyframed network parameters (`ravel_ui::keyframes::property_rows`);
  each property row carries a keyframe navigator (prev/toggle/next at the
  playhead). Keyframe selection is a multi-set (`Shift`-click toggles,
  rubber-band over channel rows selects, group drag moves all selected
  from per-channel baselines); diamonds are added by double-clicking a
  channel row (`add_keyframe_at`) or via the context menu, and
  `EditDelete` deletes the whole keyframe selection before falling back
  to the layer — all in layer-local frames converted with
  `comp_frame_for_key` (REQ-LAYER-004). Graph view reuses that channel and
  keyframe selection, paints a toggleable time/value grid with a value
  ruler, and exposes fit, add/select/delete, and Bezier/Linear/Step
  interpolation controls through both its toolbar and context menu. Graph
  points drag in time/value space (Shift constrains the dominant axis), and
  dragging one Bezier handle applies the same delta to the corresponding
  handle of every selected key (Shift snaps its screen angle to 45-degree
  increments; Alt separates the opposite handles); every gesture previews
  from immutable per-channel baselines and commits as one undo step. Dragging
  graph background rubber-band selects keyframe anchors, with Shift adding to
  the existing selection. The
  transport row above the ruler hosts an editable `HH:MM:SS:FF` timecode,
  transport buttons dispatching the playback Actions, and a logarithmic
  ppf zoom slider with fit.
- Playback: `PlaybackController` (`src/playback.rs`) wraps the headless
  `Transport` (PlaybackClock + drop counting) and handles the delegated
  transport commands (`PlaybackToggle`/`PlaybackStop`/`FrameStep*`). While
  playing, a spawned task ticks once per frame interval, moves the Timeline
  playhead, records the shared `PlaybackPosition` global, and asks
  `ProjectState` to re-evaluate the root composition output at the new
  frame (`publish_position`). The Timeline ruler scrub calls
  `seek_from_timeline(frame, fps, duration, cx)`, which must never read or
  write the timeline entity (reentrancy).
- Audio playback (`src/audio/`, audio-plan units 3–4): `AudioService` (entity,
  registered as the `AudioServiceHandle` global, strongly owned by
  `RavelWorkspace`) owns the optional `AudioEngine` — started lazily on the
  first audio layer; a missing device is a fallback, not an error. Every
  document change reaches `AudioService::sync` through `ProjectState`'s
  document observer; `AudioMixdown::desired_tracks(comp, output_rate)`
  (`src/audio/mixdown.rs`) maps audio-carrying layers to `TrackSpec`s in
  output-rate sample frames (start/gain-curve/fades are converted
  `frame / comp_fps × output_rate`; mute/solo follow the compositor's
  `active_layers` rule), and only diffs go out as `SetTrack`/`RemoveTrack`.
  The engine adopts the default device's supported sample rate, channel count,
  and sample format; `AudioService` rebuilds the first desired-track diff if
  that rate differs from its startup placeholder. Decode and output-rate SRC
  run together on the background executor, and the completed buffer enters a
  per-asset+stream cache. `SetTrack` therefore always carries output-rate
  samples and the engine has no SRC worker; placement, trim, mute, solo, and
  fade edits reuse the cached asset instead of starting another full-track job.
  Decoding is full-length (`MAX_DECODE_BYTES` = 128 MiB cap → warn-and-skip); FFmpeg
  builds decode via `MediaReader::decode_audio_chunk`, non-FFmpeg builds
  skip tracks with a visible warning. `TrackSpec::shares_build_with` keys the
  expensive rebuild on asset + stream + trim + gain, so changing a layer's
  `stream_index` re-decodes and re-sends the track (the other stream is what
  then plays) while a timeline drag only patches placement. Picture and sound
  read the same layer-local axis: the shell's
  `start_frame`/`in_frame`/`out_frame` drive both the `media` node's
  `media_frame_for(local_secs, stream)` and the track's
  `start_frame`/`source_in_frames`/`source_out_frames`.
  `AudioService` notifies Timeline and MediaBin while a requested asset is
  preparing; both render a localized preparation label and preparation
  failures become non-auto-hiding workspace notifications.
- Playback clock: `Transport::tick_with/toggle_with(&ClockSource)` where
  `ClockSource::Wall(Instant)` (the historical path, used by all existing
  tests) or `ClockSource::Audio(&SyncClock)`. The single switch decision is
  `audio::playback_clock(cx)`: audio clock iff the active composition has
  audio tracks AND an engine runs; zero tracks or no device ⇒ wall. The
  controller forwards play/pause/seek to the engine on every transport
  command so the `SyncClock` stays aligned for the switch.
  Prepared chunks carry a transport epoch: pause/seek and mixer-state changes
  invalidate old queued audio. An atomic transport gate couples epoch changes
  to clock writes; the callback tries it without waiting and advances the
  clock only for current-epoch frames actually copied (never underrun silence).
