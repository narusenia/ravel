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
- **Time**: animation keyframes live in layer-local frames; the compiled DAG
  applies `Layer::start_frame` via the TimeOffset node. UI that evaluates
  channels directly must convert comp frame → layer-local first.
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
`PLAIN_TEXT=40`, `GEOMETRY=50`, `FIELD=51`.

### `types` — data types and category traits

```rust
trait NodeData: Send + Sync + 'static {
    fn data_type_id(&self) -> DataTypeId;
    fn as_any(&self) -> &dyn Any;
    fn is_gpu_resident(&self) -> bool { false }  // true for ravel-gpu's GpuFrameBuffer
}
// dyn NodeData::downcast_ref::<T>() for concrete access.

trait BufferData: NodeData    { width/height/pixel_format }
trait TemporalData: NodeData  { duration/frame_rate }
trait GeometricData: NodeData { bounds() -> Rect; transform() -> Transform2D }
trait NumericData: NodeData   { components() -> usize }
```

Concrete types: `FrameBuffer { width, height, data: Arc<[f32]> }` (row-major
RGBA, `FrameBuffer::new_zeroed(w, h)`), `Scalar(f32)`, `Vec2(f32, f32)`,
`Vec3`, `Vec4`, `Color { r, g, b, a }` (`Color::new`, `Color::WHITE`),
`Rect { x, y, width, height }`, `Transform2D { m: [f32; 6] }`
(`Transform2D::IDENTITY`), `FrameRate::new(num, den)`.

### `graph` — immutable DAG

```rust
Node::new(id, type_key)
    .with_input(name, &[DataTypeId]) .with_output(name, DataTypeId)
    .with_param(key, ParameterValue) .with_label(..) .with_position(x, y)
ParameterValue::{Float, Int, Bool, String, ...}

Graph::new()
    .add_node(Node) -> Result<Graph, GraphError>      // consumes self
    .add_edge(..) / .remove_node(id) / .remove_edge(id)
graph.replace_node(Arc<Node>) -> Graph                // parameter edits
graph.node(id) / .nodes() / .edges() / .inputs_of(id) / .outputs_of(id)
graph.topological_sort() -> Result<Vec<NodeId>, GraphError>
```

### `eval` — Hybrid Pull + Dirty Notification

```rust
EvalContext::new(frame: u64, fps: FrameRate, resolution: (u32, u32))
    // fields: ctx.frame, ctx.fps, ctx.resolution

trait NodeProcessor: Send + Sync {
    fn process(&self, ctx: &EvalContext, inputs: &[&dyn NodeData])
        -> anyhow::Result<Box<dyn NodeData>>;
}

Evaluator::new()
    .register(node_id, Arc<dyn NodeProcessor>)
    .evaluate(&graph, node_id, &ctx) -> Result<..>    // pulls upstream only
    .mark_dirty(&graph, node_id) / .is_dirty(id) / .invalidate_all()
```

### `animation`

```rust
KeyframeCurve::new(); curve.insert(frame, value, Interpolation::Linear);
curve.sample(frame) -> f32
AnimationChannel::keyframes(curve) | ChannelSource::Constant(v)
channel.evaluate(frame, &ctx) -> f32   // frame is layer-local
// ChannelSource::{Expression, NodeOutput, AudioReactive} are placeholders.
```

### `composition` — AE-style Composition/Layer model

```rust
Layer::new(id, name, LayerSource) .with_time(start, in, out)
    // start_frame: i64 (negative allowed), solo/muted/locked, transform,
    // opacity, blend_mode
LayerSource::{Media, Solid, Shape, Text, PreComp, Generator, Null}

Composition::new(id, name, (w, h), FrameRate, duration).add_layer(layer)
compile_composition(..)   // composition/compile.rs → synthetic DAG nodes:
    // Source → TimeOffset → [Effects] → Transform → Opacity → Merge
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
    ::new() / ::from_points(Vec<Vec2>)   // seeds P + index
    .validate()?                          // P:Vec2, prim ranges, detail len 1
    .points()/.points_mut() (+ primitive_attrs, instances, detail variants)
    .push_primitive(Primitive::Path { verts: Range<usize>, closed })
    .set_instance_source(Option<Arc<Geometry>>)
    .summary() -> GeometrySummary         // counts + attribute listings
    // implements NodeData (GEOMETRY) + GeometricData

geometry::names // reserved attribute names: P, INDEX, ID, ROT, SCALE, CD,
                // ALPHA, PSCALE, AGE, LIFE, VELOCITY

trait Field: Send + Sync {
    fn sample(&self, positions: &[Vec2], ctx: &EvalContext) -> AttributeArray;
}
FieldValue(Arc<dyn Field>)   // NodeData (FIELD), lazy — consumers sample
NoiseField { seed, frequency, octaves }      // deterministic simplex/fBm
FalloffField { center, inner_radius, outer_radius, shape }
CurveRemapField::new(source, points)         // piecewise-linear
AddField/MultiplyField/MaxField { left, right }, BlendField { .., amount }
apply_field(&geo, Domain, target, &field, amount, &ctx) -> Result<Geometry>
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
registry.param_range(type_key, param_key) -> Option<&ParamRange>  // .clamp(v)
register_builtins(&mut NodeRegistry)   // registry/builtin.rs — update the
    // count/category tests there when adding a template
```

### `undo`

```rust
UndoStack::<T: Clone>::new(initial).with_max_history(n)
    .push(state) / .undo() / .redo() / .current() / .can_undo() / .can_redo()
```

### `runtime::eval_service` — background evaluation (UI non-blocking)

```rust
InvalidationHint::{None, Params(Vec<NodeId>), Structural}
trait EvalWorkerHooks: Send {          // host-supplied, runs on the worker
    fn sync(&mut self, &mut Evaluator, &Graph, &InvalidationHint);
    fn finalize(&mut self, Arc<dyn NodeData>, &EvalContext) -> Arc<dyn NodeData>;
}
EvalService::spawn(hooks, on_update)   // dedicated thread "ravel-eval-service"
    .request(graph, node, ctx, hint) -> u64   // generation; latest-wins queue
    .cancel_pending() / .latest_generation()
EvalUpdate { generation, node, result }       // delivered on the worker thread
```

Consumers publish only updates whose `generation == latest_generation()`.
`ravel-app`'s `GpuEvalHooks` (`src/eval_hooks.rs`) owns `GpuContext` +
`ShaderManager`, maps hints to `register_all_processors` /
`processor_for_node`, and rasterizes `Geometry` outputs for the Viewer.

### `runtime::playback` — frame-accurate transport clock

```rust
PlaybackClock::new(fps: FrameRate, duration_frames: u64)   // stopped at 0
    .play(now: Instant) / .pause(now) / .toggle(now) / .stop()
    .seek(frame, now) / .step(±delta, now) -> u64          // step pauses
    .current_frame(now) -> u64   // closed-form from play origin: jitter
                                 // drops frames but never drifts the clock
PlaybackState::{Stopped, Playing, Paused}
```

The time source is an argument (today `Instant::now()`); the audio master
clock (TASK-013 step 2, deferred) swaps in at these call sites. Reaching the
end pauses on the last frame. See
`docs/implementation/playback-foundation-plan.md`.

## ravel-nodes — built-in processors

`register_all_processors(&mut Evaluator, &Graph, &GpuContext, &mut ShaderManager, &Arc<Mutex<TexturePool>>)`
maps `Node::type_key` → processor;
`processor_for_node(&Node, &GpuContext, &mut ShaderManager, &Arc<Mutex<TexturePool>>)`
builds one node's processor (parameter edits rebuild just that node);
`shared_texture_pool(&GpuContext)` makes the per-eval-worker pool (512 MiB LRU).

GPU nodes exchange `ravel_gpu::GpuFrameBuffer` (VRAM-resident, shares
`DataTypeId::FRAME_BUFFER`; `.to_frame_buffer()` reads back, `Drop` returns
the texture to the pool). Helpers re-exported from `ravel_nodes`:
`ensure_gpu` / `ensure_cpu` / `clone_frame_value` (pass-throughs).
`GpuContext::transfer_stats()` counts per-context uploads/readbacks.
`ravel_gpu::RasterPipeline` wraps an instanced render pass; rasterize draws
analytic-AA path/point quads into a premultiplied RGBA16Float attachment, then
converts to straight-alpha RGBA32Float without a CPU transfer.
Current keys:

| type_key | processor | notes |
|----------|-----------|-------|
| `constant` | CPU | Scalar output |
| `blur`, `transform`, `merge`, `color_correct` | GPU (wgpu compute, WGSL in `src/shaders/`) | tests need an adapter |
| `rasterize` | GPU render pass | Geometry → resident FrameBuffer; non-zero-winding paths, point sprites, nested instances. Synthetic Composition nodes remain on the CPU zeno reference path. |
| `field.noise` / `.falloff` / `.curve_remap` / `.expression` | CPU | emit `FieldValue` |
| `field.add` / `.multiply` / `.max` / `.blend` | CPU | combine two field inputs |
| `field.apply` | CPU | Geometry + Field → Geometry; modulate a named attribute |
| `attribute.set` / `.promote` / `.transfer` | CPU | copy-on-write Geometry attribute operations |
| `attribute.path_sample` | CPU | absolute arc length → one-point Geometry with P/tangent/normal |
| `shape.rect` / `.ellipse` / `.polygon` / `.star` | CPU | emit `Geometry` (closed path + P column) |
| `shape.custom_path` | CPU | placeholder: returns empty `Geometry` until `ParameterValue::PathPoints` lands (pen-tool plan) |
| `scatter.grid` / `.circular` / `.path_array` / `.scatter` | CPU | emit `Geometry` with instance domain (index/P/rot/scale) |
| `comp.source.*`, `comp.time_offset`, `comp.transform`, `comp.opacity`, `comp.merge.*`, `comp.effects` | CPU | synthetic nodes from composition compile |

`comp.source.shape` passes through input Geometry; compilation inserts a
synthetic `rasterize` node between the shape source and the rest of the layer
chain (`ShapeRasterize` role, `NodeRole::ShapeRasterize = 6`). These synthetic
nodes intentionally use `RasterizeProcessor::from_node` so the CPU shape-layer
golden remains stable; normal graph nodes use `RasterizeProcessor::new` and
produce `GpuFrameBuffer` directly. Viewer ad-hoc Geometry finalization also
uses the CPU constructor until the Viewer accepts GPU textures.

Unknown type keys are skipped silently (plugin space).

## ravel-ui — headless shell

- `CommandId` (command.rs): every user command; string ids like
  `panel.reattach`, menu label keys via `menu_label_key()`.
- `AppShell::handle_command(CommandId) -> CommandOutcome` (shell.rs):
  the single headless command entry.
  `CommandOutcome::{Handled, DetachPanel { panel, window_id },
  ReattachPanel { panel, window_id }, ...}` — hosts act on outcomes.
- `WindowManager` (window.rs): `detach(panel)?`, `reattach(window_id)?`,
  `window_of(panel)`, `is_detached(panel)`, placements for restore.
- `panels/` holds per-panel headless state (e.g. `TimelinePanel`: playhead,
  scroll, zoom, selection, expansion, solo/mute/lock toggles).
- `properties/`: `PropertySection { title, fields }` where `title` is a
  locale key; `PropertyField::{Float, Int, Bool, String, Enum, Color,
  ReadOnly}` keyed by stable identifiers. Builders: `sections_for_node`,
  `sections_for_layer(layer, &ctx)` (evaluates transform channels in
  layer-local time).

## ravel-app — GPUI host rules (see `.agents/rules/gpui.md`)

- One command path: KeyBinding/menu/button → GPUI Action → nearest
  `on_action` → unhandled falls through to App-level handlers →
  `RavelWorkspace::dispatch_command()`. Add commands ONLY by extending
  `CommandId` + the `for_each_command!` table in `workspace.rs`.
- Panels: constructors take `(window, cx)`; focus via
  `track_panel_focus(kind, &focus_handle, window, cx)` (panels/mod.rs) which
  syncs `FocusedPanelGlobal`. Never grab focus in mouse handlers or render.
- Durable globals only (`SelectedPropertiesTarget`, `FocusedPanelGlobal`,
  `DetachedWindowHandles`); component events use `EventEmitter` +
  retained `Subscription`s. (`PropertyChanged` is legacy — Phase 5 will
  convert it; do not add new one-shot event globals.)
- Never `update()` another window from within a window update — defer with
  `cx.defer` (see `close_detached` in workspace.rs).
- Port colors: `node_editor/port_colors.rs` maps `DataTypeId` → Hsla; add an
  arm for a new data type or it falls back to gray.
- GPUI integration tests live in `crates/ravel-app/tests/` using
  `#[gpui::test]` + `TestAppContext` (see `command_dispatch_repro.rs` for
  the workspace harness and app-level action routing).
- Playback: `PlaybackController` (`src/playback.rs`) wraps the headless
  `Transport` (PlaybackClock + drop counting) and handles the delegated
  transport commands (`PlaybackToggle`/`PlaybackStop`/`FrameStep*`). While
  playing, a spawned task ticks once per frame interval, moves the Timeline
  playhead, records the shared `PlaybackPosition` global, and posts the one
  playback `eval.request` (`publish_position` — layer-network-model Phase 3
  rewrites this call site). The Timeline ruler scrub calls
  `seek_from_timeline(frame, fps, duration, cx)`, which must never read or
  write the timeline entity (reentrancy).
