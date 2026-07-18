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
    .with_subnet(Graph)     // subnet node: owns a nested graph (REQ-LAYER-003)
node.subnet: Option<Arc<Graph>>   // None for non-subnet nodes
ParameterValue::{Float, Int, Bool, String, ...}

Graph::new()
    .add_node(Node) -> Result<Graph, GraphError>      // consumes self
    .add_edge(..) / .remove_node(id) / .remove_edge(id)
    .expose_param_port(node_id, key)   // parameter → is_param InputPort (appended)
    .remove_param_port(node_id, key)   // atomic: drops edges + re-indexes later ports
graph.replace_node(Arc<Node>) -> Graph                // parameter edits
node.param_port_index(key) / node.supports_param_ports()
param_value.port_data_type()   // Float/Int/Bool/Channel→SCALAR, Channel2→VEC2, Channel4→COLOR
graph.node(id) / .nodes() / .edges() / .inputs_of(id) / .outputs_of(id)
graph.topological_sort() -> Result<Vec<NodeId>, GraphError>
// Graph is serde-capable: id-sorted {nodes, edges} lists, re-validated
// through Graph::from_parts on load (nested subnet graphs included).
```

### `eval` — Hybrid Pull + Dirty Notification (scoped, REQ-LAYER-007)

```rust
EvalContext::new(frame: u64, fps: FrameRate, resolution: (u32, u32))
    // fields: ctx.frame, ctx.fps, ctx.resolution

trait NodeProcessor: Send + Sync {
    fn process(
        &self,
        node: &Node,                              // ports/metadata/type_key
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],     // per-input-port slots; None = unconnected
        params: &ResolvedParams,                  // per-frame values (f32_or/i32_or/str_or/..)
        scope: &mut dyn EvalScope,                // nested evaluation / document access
    ) -> anyhow::Result<Arc<dyn NodeData>>;
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

Evaluator::new()
    .register(node_id, Arc<dyn NodeProcessor>)  // processors are stateless re params
    .evaluate(&graph, node_id, &ctx) -> Result<..>    // pulls upstream only
    .evaluate_at(&[segments], &graph, node_id, &ctx)  // seeded ownership path
    .mark_dirty(&graph, node_id) / .mark_dirty_at(&graph, &[segments], node_id)
    .is_dirty(id) / .invalidate_all() / .invalidate_scope(&[segments])
    .set_document(Arc<Document>)                // required by comp.network / Layer Ref
    .take_timings() -> Vec<(NodeId, Duration)>  // process() durations of the last pull
```

Cache/dirty are keyed by ownership path + NodeId; animated (keyframed or
node-output-bound) parameters make a node time-varying automatically.
Multi-output nodes yield a `PortRecord` indexed by the edge's `source_port`.

### `animation`

```rust
KeyframeCurve::new(); curve.insert(frame, value, Interpolation::Linear);
curve.sample(frame) -> f32
AnimationChannel::keyframes(curve) | ChannelSource::Constant(v)
channel.evaluate(frame, &ctx) -> f32   // frame is layer-local
// ChannelSource::{Expression, AudioReactive} are placeholders.
// ChannelSource::NodeOutput(node, port) resolves inside the evaluator
// (parameter bindings only, same graph/scope).
// ParameterValue::{Channel, Channel2, Channel3, Channel4} put channels on
// node parameters (REQ-LAYER-004).
```

### `composition` — Layer-network model (v3, REQ-LAYER-001)

```rust
Layer::new(id, name, network: Graph) .with_time(start, in, out)
    // shell: start_frame (i64, negative allowed), solo/muted/locked,
    // transform (rotation in DEGREES), opacity, blend_mode, adjustment,
    // parent; reserved v2: time_remap, track_matte
    // LayerSource is REMOVED — kinds are creation templates (REQ-LAYER-008)
layer.has_frame_output() -> bool   // false = null layer (REQ-LAYER-005)

Composition::new(id, name, (w, h), FrameRate, duration).add_layer(layer)
Document::{with_composition, get_composition, changed_network_paths(&old)}
Document::{with_media_asset(id, path), get_media_asset(&str)}
    // media_assets: im::HashMap<String, MediaAssetEntry { path }> — the
    // evaluation-time asset table indexed by the video node's asset_id
// Layer/Composition/Document are serde-capable (deterministic: id/key-sorted
// adapters; network graphs re-validate through Graph::from_parts on load).
// A deserialized Document must pass `doc.validate()` (structural invariants:
// root/comp-id/frame-rate/layer-ref integrity, DocumentValidationError),
// then `doc.advance_id_counters()` (REQ-LAYER-009) moves every
// NodeId/EdgeId/CompId/LayerId counter past `doc.id_watermarks()` so fresh
// ids never collide with loaded ones.

compile_composition(&comp, graph) -> CompilationResult  // shell chain only:
    // normal:     boundary(comp.network) → Transform → Opacity → Merge
    // adjustment: boundary(◂ bg) → Transform → Merge(adjustment)(◂ bg)
    // null layer: Transform only (for parenting)
deterministic_node_id(comp, layer, NodeRole) / decode_deterministic_node_id(id)

validate::{validate_precomp_cycles, validate_parenting_cycles,
    validate_layer_ref_cycles}   // layer.ref cycles incl. inside subnets

templates::LayerTemplate { key, display_name, nodes, edges }  // RON data
    .instantiate(&NodeRegistry) -> Result<Graph, TemplateError>
    // registry seeds ports/params; template extends/overrides; fresh
    // NodeId::next per instantiation
templates::{builtin_layer_templates(), builtin_layer_template(key)}
    // "solid" | "shape" | "video" | "null" from assets/layer-templates/
```

### `network` — In/Out interface conventions (REQ-LAYER-002)

```rust
NET_IN_TYPE_KEY = "net.in"   // outputs: base_geometry, t, f, [source], custom params
NET_OUT_TYPE_KEY = "net.out" // inputs: frame (+ custom ports for Layer Ref)
find_in_node(&graph) / find_out_node(&graph) / frame_port_index(node)
// net.in/net.out values are PortRecords in port order.
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
    .with_param_options(key, options)    // closed option set for a String
    // param → Properties renders an enum dropdown (merge `operation`,
    // math.scalar `op`)
registry.param_range(type_key, param_key) -> Option<&ParamRange>  // .clamp(v)
registry.param_options(type_key, param_key) -> Option<&[String]>
register_builtins(&mut NodeRegistry)   // registry/builtin.rs — update the
    // count/category tests there when adding a template
```

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

### `runtime::eval_service` — background evaluation (UI non-blocking)

```rust
InvalidationHint::{None, Params(Vec<NodeId>), Structural}
trait EvalWorkerHooks: Send {          // host-supplied, runs on the worker
    fn sync(&mut self, &mut Evaluator, &Graph, Option<&Document>, &InvalidationHint);
    fn finalize(&mut self, Arc<dyn NodeData>, &EvalContext) -> Arc<dyn NodeData>;
}
EvalRequest { graph, node, path: Vec<PathSegment>, ctx,
    document: Option<Arc<Document>>, hint }
    // document → Evaluator::set_document before sync (scoped invalidation);
    // non-empty path evaluates via evaluate_at
EvalService::spawn(hooks, on_update)   // dedicated thread "ravel-eval-service"
    .request(EvalRequest) -> u64              // generation; latest-wins queue
    .cancel_pending() / .latest_generation()
EvalUpdate { generation, node, result, timings }  // worker thread; timings
    // feed the node editor's per-node load readout
```

Consumers publish only updates whose `generation == latest_generation()`.
`ravel-app`'s `GpuEvalHooks` (`src/eval_hooks.rs`) owns `GpuContext` +
`ShaderManager`, maps hints to `register_all_processors` /
`processor_for_node` (searching the document's layer networks too), and
rasterizes `Geometry` outputs for the Viewer.

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
maps `Node::type_key` → processor and recurses into subnet inner graphs;
`processor_for_node(&Node, &GpuContext, &mut ShaderManager, &Arc<Mutex<TexturePool>>)`
builds one node's processor (processors never capture parameter values —
edits only require dirty marking, not a rebuild);
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
| `constant.color` | CPU | animatable `color` param (Channel4) → `Color` output |
| `math.scalar` | CPU | `op` enum (add/subtract/multiply/divide/min/max/mod/pow + unary abs/negate/floor/ceil/round/sqrt/sin/cos); `a`/`b` are Float params (drive via exposed param ports); div/mod-by-zero and sqrt(<0) → 0; mod is `rem_euclid`; radians |
| `math.remap` | CPU | linear fit `value`: `[in_min,in_max]` → `[out_min,out_max]`, optional `clamp`; degenerate in-range → `out_min` |
| `video` | CPU | decodes media via the document asset table (`asset_id`); layer-local seconds → media frame (`floor(t·fps)`, clamped); FFmpeg backend behind the `ffmpeg` feature, injectable `ReaderFactory` for tests |
| `layer.ref` | CPU | same-comp reference to another layer's `net.out` port (`layer` + `port` params); pre-transform output at the target's local time; typed zero outside its interval |
| `subnet` | CPU | evaluates `node.subnet` recursively (`PathSegment::Subnet`); connected pins bind the inner `net.in`, unconnected pins promote same-name node params |
| `blur`, `transform`, `merge`, `color_correct` | GPU (wgpu compute, WGSL in `src/shaders/`) | tests need an adapter |
| `rasterize` | GPU render pass | Geometry → resident FrameBuffer; non-zero-winding paths, point sprites, nested instances. Element color: `Cd`/`alpha` attrs > `color` pin > `color` param (REQ-LAYER-008). Synthetic Composition nodes remain on the CPU zeno reference path. |
| `field.noise` / `.falloff` / `.curve_remap` / `.expression` | CPU | emit `FieldValue` |
| `field.add` / `.multiply` / `.max` / `.blend` | CPU | combine two field inputs |
| `field.apply` | CPU | Geometry + Field → Geometry; modulate a named attribute |
| `geometry.transform` | CPU | scale→rotate→translate around a pivot (`use_centroid` default on = bbox center, else `pivot_x/y`); rotation in degrees; transforms point `P` and instance placement (`P` + `rot` offset + component-wise `scale`); CoW columns |
| `attribute.set` / `.promote` / `.transfer` | CPU | copy-on-write Geometry attribute operations |
| `attribute.path_sample` | CPU | absolute arc length → one-point Geometry with P/tangent/normal |
| `shape.rect` / `.ellipse` / `.polygon` / `.star` | CPU | emit `Geometry` (closed path + P column) |
| `shape.custom_path` | CPU | placeholder: returns empty `Geometry` until `ParameterValue::PathPoints` lands (pen-tool plan) |
| `scatter.grid` / `.circular` / `.path_array` / `.scatter` | CPU | emit `Geometry` with instance domain (index/P/rot/scale) |
| `comp.network` | CPU | layer network boundary: layer-local `EvalContext`, scoped evaluation of the layer's owned network |
| `comp.transform` | CPU | layer transform channels (degrees) + parent chain, inverse-mapped premultiplied bilinear resample; identity passes through |
| `comp.opacity` | CPU | alpha × layer opacity (layer-local frame); 1.0 passes through |
| `comp.merge.*` | CPU | straight-alpha Porter-Duff over with W3C blend modes; `.adjustment` mixes bg/adjusted by layer opacity (effect strength) and bypasses outside the interval |
| `net.in` / `net.out` | CPU | network interface nodes (REQ-LAYER-002); produce `PortRecord`s (a single-output `net.in` yields the value directly); custom In ports prefer scope bindings over own params |

`rasterize` selection is unchanged: synthetic-flagged nodes use
`RasterizeProcessor::from_node` (CPU zeno reference path) while normal graph
nodes use `RasterizeProcessor::new` and produce `GpuFrameBuffer` directly.
Viewer ad-hoc Geometry finalization also uses the CPU constructor until the
Viewer accepts GPU textures.

Unknown type keys are skipped silently (plugin space).

## ravel-ui — headless shell

- `CommandId` (command.rs): every user command; string ids like
  `panel.reattach`, menu label keys via `menu_label_key()`.
  `LayerAdd{Solid,Shape,Video,Null}` map to builtin layer templates via
  `layer_template_key()` (REQ-LAYER-008; a test ties the two sets together).
- `document` (document.rs): the app-wide document editing state.
  `DocumentStore { document(), apply(doc), commit(doc), undo(), redo() }` —
  the Document snapshot is the undo unit (REQ-LAYER-009); `apply` is the
  live mid-gesture update, `commit` records one step. `NetworkPath
  { comp, layer, subnets }` names a network by ownership path
  (`entered(subnet)` / `truncated(depth)` / `segments()`); free helpers:
  `default_document`, `root_composition`, `update_composition`,
  `update_layer`, `add_layer`, `remove_layer`, `reorder_layer`,
  `add_layer_from_template(doc, comp, template, &registry)`,
  `resolve_network(doc, &path)`, `replace_network(doc, &path, graph)`.
- `AppShell::handle_command(CommandId) -> CommandOutcome` (shell.rs):
  the single headless command entry.
  `CommandOutcome::{Handled, DetachPanel { panel, window_id },
  ReattachPanel { panel, window_id }, ...}` — hosts act on outcomes.
- `WindowManager` (window.rs): `detach(panel)?`, `reattach(window_id)?`,
  `window_of(panel)`, `is_detached(panel)`, placements for restore.
- `panels/` holds per-panel headless state (e.g. `TimelinePanel`: playhead,
  scroll, zoom, selection, expansion — property expansion is keyed by
  `keyframes::PropertyRowId` — solo/mute/lock toggles).
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
  (keys animated channels preserving interpolation/tangents,
  `set_curve_value` for the bare curve), `preview_keyframe_move`
  (baseline-derived drag preview), `row_channels`, `has_keyframe_at`.
- `properties/`: `PropertySection { title, fields }` where `title` is a
  locale key; `PropertyField::{Float, Int, Bool, String, Enum, Color,
  ReadOnly}` keyed by stable identifiers. Builders: `sections_for_node(node,
  &registry, frame)` (samples animated channels at the layer-local frame),
  `sections_for_layer(layer, &ctx)` (evaluates transform channels in
  layer-local time; includes the In node's custom parameters as
  `custom.<name>` fields, REQ-LAYER-002). Reverse mapping:
  `layer::apply_layer_field(&mut Layer, key, &PropertyValue, local_frame)`
  (shell attributes + `custom.*` In parameters; animated channels are keyed
  at `local_frame`, not flattened), `layer::toggle_layer_keyframe` /
  `layer::layer_field_keyframed` for the per-field key toggle,
  `layer::in_node_id`.

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
- Document state: `ProjectState` (`src/project_state.rs`) is the single
  owner of the live `Document`, the Document-level undo stack, and the
  background `EvalService`; the workspace creates it and registers the
  durable `ProjectStateHandle` global. All edits flow through
  `apply_document(doc, hint, cx)` (live) / `commit_document` (one undo
  step); `undo`/`redo` are routed here by the workspace when no panel
  intercepts `EditUndo`/`EditRedo`. The Viewer always evaluates the root
  composition output (`compile_composition` + Document-aware requests,
  REQ-LAYER-007); `request_viewer_eval(hint, cx)` posts one request at the
  shared `PlaybackPosition`. Eval results publish `ViewerFrame` and merge
  per-node durations into the `NodeEvalTimings` global (node editor load
  readout: muted < 8 ms, yellow < 33 ms, red beyond).
  `disable_background_eval_for_tests()` keeps gpui tests deterministic.
- Persistence: `.ravprj` format v3 (`src/project/`) — a zip of
  `manifest.json` (format_version drives the `migration` chain),
  `document/main.ron` (the full `Document`, deterministic RON),
  `assets/refs.json`, `settings.toml`; saving writes a `.bak` of the
  previous revision. `ProjectFile::{new, from_document, to_archive,
  from_archive, save, load}`; the layout is selected by the source version
  (v3 requires `document/main.ron`), and a v1/v2 archive (flat
  `graph/main.ron` only) wraps the graph in a fresh Document (root comp
  from the manifest's resolution/frame rate). Every load runs
  `Document::validate()` (structural invariants: root presence, comp id
  consistency, non-zero frame rate, unique layer ids, resolved
  parent/track-matte refs) and advances the id counters (REQ-LAYER-009).
  `project::timestamp::rfc3339_now()` supplies wall-clock stamps without a
  chrono dependency. `ProjectState` owns the open project:
  `project_path()`, `new_document`, `save_project_to(path, cx)`,
  `load_project_from(path, cx)` (file I/O on the background executor;
  loading replaces the document and undo history wholesale; generation /
  revision guards make an in-flight save or load harmless when the user
  edits or replaces the document meanwhile). The File menu commands
  (New/Open/Save/Save As) route through the workspace's
  `CommandOutcome::Delegate` arm with GPUI path prompts.
- Node editor: edits one network at a time, addressed by
  `ravel_ui::document::NetworkPath` (REQ-LAYER-011): the timeline opens a
  layer's network via `NodeEditorPanel::open_network` (double-click),
  double-clicking a subnet node dives deeper, the breadcrumb bar returns to
  ancestors, and `NodeMetadata.synthetic` nodes are filtered from painting
  and every hit test. Graph edits are spliced into the document with
  `replace_network` and committed to `ProjectState`.
  `toggle_param_keyframe(node, key, cx)` adds/removes a key at
  `current_local_frame()` (the playhead in the owning layer's local time);
  parameter scrubs keep channel parameters animated (a keyframed channel
  gets a key at the current frame instead of flattening to a constant).
- Timeline: mirrors the document's root composition; layer add (menu),
  delete (`EditDelete`, locked layers protected), reorder (header drag),
  move/trim (bar drag with in/out handles), solo/mute/lock all commit
  Document undo steps. Layer selection publishes the Properties target but
  never re-targets the node editor. The property tree lists the shell
  channels plus keyframed network parameters
  (`ravel_ui::keyframes::property_rows`); diamonds are moved by drag,
  added by double-clicking a channel row (`add_keyframe_at`), and
  `EditDelete` scopes to the selected diamond before the layer — all in
  layer-local frames converted with `comp_frame_for_key` (REQ-LAYER-004).
- Playback: `PlaybackController` (`src/playback.rs`) wraps the headless
  `Transport` (PlaybackClock + drop counting) and handles the delegated
  transport commands (`PlaybackToggle`/`PlaybackStop`/`FrameStep*`). While
  playing, a spawned task ticks once per frame interval, moves the Timeline
  playhead, records the shared `PlaybackPosition` global, and asks
  `ProjectState` to re-evaluate the root composition output at the new
  frame (`publish_position`). The Timeline ruler scrub calls
  `seek_from_timeline(frame, fps, duration, cx)`, which must never read or
  write the timeline entity (reentrancy).
