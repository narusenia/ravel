# Implementation documentation

Canonical sources of truth are:

- `docs/requirements/REQ-*.md` for product requirements;
- `docs/specifications/` for architecture, data-model, and UI specifications;
- per-feature `*-plan.md` documents for in-flight work.

Layout of this directory:

| Location | Contents |
|---|---|
| top level | live plans — in progress, planned, or reference |
| `done/` | completed plans, kept for provenance and as templates |
| `archive/` | the historical TASK-ID generation (`TASK-001`…`TASK-052` plus `task-016/017/019-plan.md`) — provenance only, **not** current design |

A plan moves into `done/` when every unit is merged. Inbound references from
source doc-comments move with it.

**`backlog.md` lists every implementation unit across all live plans in one
table** — start there to find something to pick up. This index is for finding
the design behind a unit.

## In progress

| File | Subject | Status | Related requirements |
|---|---|---|---|
| `audio-plan.md` | Audio layers, the sound bank, playback wiring, and analysis nodes | units 1–4 done — 2026-07-26 | REQ-MEDIA-002, REQ-MEDIA-003 |
| `media-import-plan.md` | Media import, asset references, MediaBin, and the unified media node | units 1–5 done — 2026-07-26 | REQ-UI-008, REQ-UI-010, REQ-PROJ-001 |
| `evaluation-scope-plan.md` | `PathSegment` scope axis, graph-internal iteration, group convention | unit 1 done — 2026-07-27 | REQ-CORE-013, REQ-CORE-002/011 |
| `motion-blur-plan.md` | Continuous-time channels, quality tiers, sampled motion blur | unit 1 done — 2026-07-27 | REQ-RENDER-004 |
| `per-instance-modulation-plan.md` | Field-driven per-instance attribute modulation, `attribute.delete` | units 1–2 done — 2026-07-28 | REQ-MOGRAPH-001, REQ-CORE-010, REQ-CORE-012 |
| `gpu-compositing-plan.md` | GPU shell compositing, readback, and the viewer image path (responsiveness stage 2) | plan written — 2026-07-28 | REQ-LAYER-001/010, REQ-GPU-001 |
| `viewer-inspection-plan.md` | Composition background wiring, checkerboard, channel isolation, pixel readout, playback/cache status | INSP-1 done — 2026-07-30 | REQ-UI-004, REQ-LAYER-001 |
| `viewer-preview-resolution-plan.md` | Preview resolution factor (`Full`/`1/2`/`1/4`) replacing the hidden `VIEWER_MAX_DIM` cap, plus input-driven adaptive resolution — **the only way to inspect output at full resolution today** | VRES-1 done — 2026-08-06 (no UI to change the factor until VRES-2) | REQ-UI-004 |
| `developer-docs-plan.md` | Implementer how-to pages (`docs/dev/`) and the documentation index | units 1–8 done — 2026-07-30 | — |
| `settings-screen-plan.md` | Settings dialog, the 4-layer apply path, theme/locale/keybinding reachability | SET-1–7 done — 2026-08-03; SET-8/SET-16 done — 2026-08-10; SET-9–SET-15 gated on their features | REQ-PROJ-004, REQ-UI-006/007 |

## Planned

Ordered by dependency. The REQ-MOGRAPH work now runs through
`per-instance-modulation-plan.md`, which is already in progress above —
several plans here wait on its later units rather than on each other.

| File | Subject | Depends on | Related requirements |
|---|---|---|---|
| `geometry-ops-plan.md` | Blast, sort, resample, measure, switch, null, line/grid, connect, curve parameter | `evaluation-scope-plan.md` | REQ-CORE-010, REQ-MOGRAPH-001 |
| `discrete-keyframes-plan.md` | Keyframes for Int and String parameters (`IntChannel`, `StringSteps`) — animation stops being f32-only | — | REQ-CORE-010 |
| `render-warning-channel-plan.md` | Machine-readable warnings for what a render silently gets wrong: offline or unreadable media, and identifier parameters that are not static — driven by a wire, keyframed, or hand-authored `StringSteps` (`HIGH-34`, `HIGH-35`) | — | — |
| `hands-on-findings-handoff.md` | Where the 2026-08-08 hands-on findings landed, and the order to pick the filed bugs up in | — | — |
| `parameter-groups-plan.md` | Parameter groups (Pages) declared by the node type; the shape `OFX-5` reads plugin Group/Page into | — | REQ-PLUGIN-001 |
| `refactor-plan-0808.md` | Workflow-penetration UX: the instrumentation that counts panel round-trips and re-searches, plus the known Timeline / import / search fixes — **the pre-release UX bucket** | — | REQ-UI-002–004, REQ-UI-013 |
| `node-graph-readability-plan.md` | Manual auto-layout on undo, the `node_editor` settings section that finally persists `EdgeStyle`, then top-down flow, reroute, edge insertion, and type-coloured edges | — | REQ-UI-002, REQ-UI-003 |
| `contextual-parameter-options-plan.md` | Contextual option lists and a parameter-driven output type, so `layer.ref` picks a sibling layer by name instead of scrubbing an id — **post-release; the mechanism `MED-APP-29` turned out to need** | — | REQ-LAYER-005, REQ-UI-002 |
| `wrangle-plan.md` | A multi-statement, multi-attribute CPU wrangle node and spare parameters on any node — **post-release, no open gates. `HIGH-30` was closed by #346 and the exposure-model branch settled on plan A (growing a parameter and exposing it stay two steps) on 2026-08-09** | — | REQ-CORE-010, REQ-CORE-015, REQ-PLUGIN-005 |
| `network-interface-editing-plan.md` | In/Out custom port editing, subnet pin sync, collapse/extract — **prerequisite for `done/exposed-parameters-plan.md`** | — | REQ-LAYER-002, REQ-LAYER-003, REQ-PROJ-006 |
| `scene-info-nodes-plan.md` | `layer.info` / `comp.info`, `InvalidationHint::Shell`, shell-binding cycles | `network-interface-editing-plan.md` (units 1–3) | REQ-LAYER-002, REQ-LAYER-005, REQ-CORE-007 |
| `viewer-tool-extensions-plan.md` | Hand/Zoom tools, box selection, path point editing, polygon/star drawing — takes over MED-APP-15 | `done/viewer-overlay-manipulator-plan.md` (unit 1, for the box frame) | REQ-UI-011 |
| `path-shading-plan.md` | The CPU per-pixel path evaluator, vertex-colour interpolation along a stroke, and `stroke_align` — the three things blocked on zeno returning coverage and nothing else | `style-attributes-plan.md` unit 6 (merged) | REQ-MOGRAPH-001, REQ-RENDER-001, REQ-CORE-012 |
| `properties-parameter-editors-plan.md` | Curve and colour-ramp parameter types and inline editors, `math.curve`, `color.ramp` | — (`style-attributes-plan.md` unit 6 for `field.ramp`) | REQ-UI-002, REQ-UI-012, REQ-CORE-012 |
| `cache-plan.md` | Cache identity, byte budget, the output-stage frame cache, the green cache bar, layer-scoped invalidation and idle read-ahead — **the cross-cutting cache charter**; what is left is the disk tier (`CACHE-11`, measurement-gated) and the f16 pixel loops (`CACHE-Y`) | `gpu-compositing-plan.md` (unit 5 only) | REQ-CORE-006, REQ-CORE-002/011 |
| `typography-plan.md` | Text layout, glyph geometry, path text, per-character modulation | `per-instance-modulation-plan.md` | REQ-MOGRAPH-004 |
| `stateful-eval-plan.md` | `StatefulProcessor` and the simulation cache | — | REQ-CORE-011 |
| `particle-plan.md` | Particle simulation as point geometry | `stateful-eval-plan.md`, `per-instance-modulation-plan.md` | REQ-MOGRAPH-002 |
| `effects-library-plan.md` | Colour, blur, distortion, generation, stylise, and time nodes | — | REQ-MOGRAPH-005 |
| `gpu-resident-geometry-plan.md` | `GpuGeometry`, GPU-side instance expansion, WGSL fields — **phase 0 measured; verdict: proceed** | — | REQ-CORE-009, REQ-GPU-001/003 |
| `style-attributes-plan.md` | Fill and stroke as per-element attributes | — | REQ-CORE-010, REQ-MOGRAPH-001 |
| `vector-field-plan.md` | Vector fields — look-at, curl noise, flow — **units 5 and 7a merged** | `per-instance-modulation-plan.md` | REQ-CORE-012 |
| `path-ops-plan.md` | Boolean, offset, round corners, simplify, trim — **phase 0 decides the boolean approach** | `evaluation-scope-plan.md` | REQ-CORE-010, REQ-MOGRAPH-005 |
| `layer-shell-wiring-plan.md` | Wire the declared-but-unused `track_matte` and `time_remap` | — | REQ-LAYER, REQ-CORE-001 |
| `ci-cache-plan.md` | Move the CI cache to sccache + Cloudflare R2 — **two platforms' `target/` archives cannot both fit the 10 GB limit, so every merge evicts the other** | — (needs an R2 bucket and repository secrets) | — |
| `color-management-plan.md` | Linear working space, per-asset input colour space, viewer/export transforms, then the OCIO backend — **`CM-1`–`CM-5` are implemented (the pipeline is linear). `CM-7` (the GPU display transform, OCIO-independent) is ready to start. `CM-6` (`ocio-rs`) was deferred on 2026-08-10 and now opens on demand — when a `.ocio` config or an ACES deliverable is actually asked for — and `CM-9` / `CM-8` sit behind it** | `done/render-export-plan.md` (EXPORT-1 for CM-4) | REQ-RENDER-003, REQ-CORE-009 |
| `align-panel-plan.md` | Layer align/distribute panel — low priority | `done/free-pane-docking-plan.md` (DOCK-8, merged) | REQ-UI-013 |
| `3d-scene-plan.md` | `Primitive::Mesh`, the `Scene` type, camera, triangle renderer, primitives, 3D cloning, lighting, extrusion, model import | — (extrusion alone waits on `typography-plan.md`) | REQ-3D-001–009 |
| `plugin-system-plan.md` | `ProcessorRegistry`, package manifests, WGSL shader plugins, WASM geometry nodes | `done/exposed-parameters-plan.md` (merged), `gpu-backend-plan.md` (GPUBK-1) | REQ-PLUGIN-002, REQ-PLUGIN-004 |
| `gpu-backend-plan.md` | Hide the backend behind an abstraction, then add Metal/D3D12/Vulkan — unblocks OFX and takes over MED-GPU-01 | — | REQ-INFRA-009, REQ-GPU-001 |
| `gpu-device-loss-recovery-plan.md` | Recover adopted and owned GPU devices across the evaluator, texture pools, viewer, export queue, and window lifecycle | `gpu-backend-plan.md` (GPUBK-9), `done/zero-copy-viewer-plan.md` (ZC-8) | REQ-GPU-001 |
| `ofx-host-plan.md` | The OpenFX host: an isolated C++ process, the suites, and the GPU interop — **OFX defines no D3D12 path, so Windows has no zero-copy route** | `gpu-backend-plan.md` (GPUBK-8, merged), `plugin-system-plan.md` (PLUG-1) | REQ-PLUGIN-001, REQ-PROJ-002 |
| `geometry-fracture-plan.md` | Voronoi cell fracture in 2D and 3D, polygon triangulation, selectable algorithms | `3d-scene-plan.md` (unit 1, for the 3D variant) | REQ-CORE-010, REQ-MOGRAPH-001, REQ-3D-003 |

`panel-placement-plan.md` (#181) is **superseded** by
`done/free-pane-docking-plan.md`: the view-toggle problem is solved by
default-slot insertion on the new layout model (DOCK-2) instead of on the
gpui-component dock. The old file stays at the top level with a superseded
banner for provenance; its PANEL-1〜3 units were withdrawn before any work
started.

Four Viewer plans land in roadmap phase E and all touch the same input and
paint paths, so **`done/viewer-overlay-manipulator-plan.md` unit 1 must go first**.
The snap guides, the box-selection frame, and the pixel-value readout are all
written against its screen-space painting API; without it each plan invents its
own paint path. That unit also fixes two things the earlier plan text got wrong:
there are five existing overlays, not four (the evaluation-error display was
missing), and the selection bbox's eight handles are decorative — no scale or
rotate gesture exists anywhere, which is what `OVL-7` adds.

Three plans all change `EvalRequest` / `EvalUpdate`
(`done/attribute-spreadsheet-plan.md` unit 1 makes them multi-target;
`stateful-eval-plan.md` unit 3 adds a provisional-result flag;
`done/viewer-overlay-manipulator-plan.md` unit 2 rides on the multi-target form to
pull overlay data). **The order settled with the multi-target form landing
first** (2026-08-06, #302), so the other two now extend a shape that exists
instead of each inventing one. The overlay plan adds no second evaluation path
of its own.

`properties-parameter-editors-plan.md` unit 1 owns `ParameterValue::Curve` and
`ParameterValue::Ramp`, and six nodes across three domains consume them: value
(`math.curve`, `color.ramp`), field (`field.curve_remap` and `field.ramp`), and
raster (the tone curve and the gradient generator in `effects-library-plan.md`
units 1 and 3). `Curve` is merged (`.ravprj` format v6 converts the control
points `field.curve_remap` used to store as text); FX-1, FX-3, and
`style-attributes-plan.md` unit 6 must use it rather than invent a second
representation per domain. `Ramp` still comes first for the gradient side.

`vector-field-plan.md` unit 5 (folding `_x` / `_y` parameters into `Channel2` /
`Channel3`, `.ravprj` format v5) is merged, which unblocks what it gated:
`done/viewer-overlay-manipulator-plan.md` unit 5 can now declare a `ParamRole` per
position parameter, and the Properties Vector row is reachable.
`attribute.set`'s `value` is folded too, at the arity its `type` selects;
changing `type` reshapes the value and re-types its parameter port in one
command (`Graph::set_params` plus
`registry::builtin::dependent_param_updates`). `Int` component pairs such as
`scatter.grid`'s `count_x` / `count_y` stay separate — a `Channel2` is a pair
of float channels.

`evaluation-scope-plan.md` unit 1 is merged (#186), so the axis simulation
caching, time remapping, and graph-internal iteration share is settled. The
evaluator caches one value per `(path, node)` — `frame` is a validity check,
not a key — and `PathSegment` is now the one place that splits it.
`stateful-eval-plan.md` keeps a dedicated `SimTrack` for the sequential
fill pattern but must key it on `NodeKey` rather than inventing a key type.

`settings-screen-plan.md` owns the settings dialog **and the apply path that
makes settings do anything at all**. That path now exists: `AppSettings` is
resolved at launch and the locale, the appearance, and the project's default
frame rate are applied from it, so `ja.toml` is reachable from the language
picker — the core of MED-APP-10, though the finding itself stays open while
auto-save, proxy, and colour are unwired. Its governing rule is that **an item appears in the
dialog only when the setting changes behaviour** — so auto-save, proxy, and
colour items (and the cache's disk tier) are gated on their features landing
rather than shown as dead controls, which is why the plan stays live with
SET-9–SET-15 open. Anything
that wants a user-facing preference goes through that plan instead of adding
its own dialog.

`cache-plan.md` owns everything else about caching: the validity conditions as
one `CacheIdentity`, the quantised time key, cache precision, the single byte
budget (the texture pool's `LruBudget` becomes subordinate to it), the
output-stage frame cache, and the hit-rate API. Three other plans must not
invent their own version of these — `motion-blur-plan.md` unit 2 (time-based
validity) is absorbed into it, `stateful-eval-plan.md` gets its simulation
reservation from it, and `done/render-export-plan.md` relies on its precision
requirement so a render never eats a preview-quality frame. It also takes over
seven cache issues (HIGH-03, HIGH-16, MED-CORE-02/03/06/07 and the
single-entry image cache in MED-MED-02) because they all rewrite the same functions.

The motion-graphics plans implement on the CPU and keep the GPU boundary open
rather than building for it. Each carries a "GPU 方針" section stating what is
and is not measured, its migration point, and its numeric trigger.

The old geometry number (`perf-baseline.md` scenario c, 0.007 ms) is a
**warm-cache** number and shows nothing about CPU geometry evaluation cost — no
plan may cite it as such. The uncached sweep is now on record
(`perf-baseline.md`, "ジオメトリ評価スケーリング baseline"): at 100k elements
the end-to-end chain costs 18.24 ms, and **77% of the CPU side is `rasterize`
expanding instances on the CPU every frame** — not field evaluation (1.17 ms)
and not the upload (1.20 ms). `gpu-resident-geometry-plan.md` proceeds, led by
`GpuGeometry` and the resident-rasterize unit. The same measurement shows a CPU
particle step at 100k costs about 0.2 ms and a per-frame vertex upload holds to a few
hundred thousand vertices, so neither particles nor 3D need GPU simulation or
WGSL fields to reach those counts — they need the resident draw path.

`done/image-instancing-plan.md` decides that a frame buffer is copied by riding the
geometry instance mechanism rather than by a second placement world, and its
first unit (`IMG-1`) retires `SceneContent::Image` while that is still cheap:
`scene.render` is unwritten, so nothing reads the image variant today. Two
other plans have to read it before they touch the same ground —
`gpu-resident-geometry-plan.md` (`GPU-5` assumes instance sources stay CPU-side
metadata, which a texture handle is not) and `cache-plan.md` (`CACHE-3` has to
charge an image-carrying geometry to VRAM). `3d-scene-plan.md` unit 4 keeps its
textured-rectangle behaviour; only the route to it changes.

## Reference

| File | Subject |
|---|---|
| `backlog.md` | **Every implementation unit in one table** — the entry point for picking up work |
| `roadmap.md` | **What order to do it in, and why** — phases, the four ordering criteria, and the open question about where export belongs |
| `plan.md` | Implementation overview by subsystem |
| `perf-baseline.md` | Evaluation and render measurements |
| `path-channel-design.md` | Path animation design memo (#150); implementation deferred |

## Done

| File | Subject | Merged | Related requirements |
|---|---|---|---|
| `done/asset-identity-plan.md` | Stable `AssetId` separate from the display name, so a re-import cannot silently rebind an existing reference; the exposed declaration's claim is recorded on the entry | #456, #460 — 2026-08-21 | REQ-PROJ-001, REQ-UI-008, REQ-UI-010 |
| `done/panel-visibility-plan.md` | Tell a panel whether its tab is in front, so a background pane stops rebuilding and catches up when it returns | #409, #461 — 2026-08-21 | REQ-UI-002, REQ-UI-013 |
| `done/responsiveness-stage3-plan.md` | Responsiveness stage 3 (roadmap phase C3): graph adjacency index, layer-level `ptr_eq`, path interning, panel revision gates, GPU rasterize and upload dedup | #395, #396, #397, #461 — 2026-08-21 | REQ-CORE-002/006/011, REQ-UI-002/003 |
| `done/attribute-spreadsheet-plan.md` | Geometry attribute inspection panel, multi-target evaluation | #302, #448, #450 — 2026-08-15 | REQ-CORE-010, REQ-UI-013 |
| `done/viewer-overlay-manipulator-plan.md` | Extensible Viewer overlay mechanism, Field/Geometry visualisation, parameter and layer-shell manipulators, motion path | #255, #429–#441 — 2026-08-15 | REQ-UI-011, REQ-UI-013, REQ-CORE-012 |
| `done/viewer-snap-guides-plan.md` | Snapping to existing geometry, rulers and user guides | #444, #446 — 2026-08-15 | REQ-UI-011, REQ-UI-004 |
| `done/image-instancing-plan.md` | `InstanceSource`, the `geometry.from_image` node, and the rasterize texture path — frame buffers copied through the existing instance mechanism; retires `SceneContent::Image` | #309, #418, #426, #430 — 2026-08-14 | REQ-3D-001, REQ-MOGRAPH-001, REQ-CORE-010 |
| `done/render-export-plan.md` | Render queue, headless CLI (`ravel-cli render`, `list`, `interactive`), audio mixdown, and the export dialog with its render queue panel | #299–#335 — 2026-08-08 | REQ-RENDER-001, REQ-RENDER-005 |
| `done/exposed-parameters-plan.md` | Declared named inputs — the one mechanism behind CLI `--param`, subgraph templates, network interfaces, and shader manifests | #321 — 2026-08-07 | REQ-PROJ-006, REQ-PLUGIN-005 |
| `done/expression-language-plan.md` | The loop-free expression language: parameter and field expressions, geometry attribute access, and the Properties expression editor | #320 — 2026-08-07 | REQ-CORE-014/015, REQ-CORE-007/010, REQ-MOGRAPH-001 |
| `done/pointer-feedback-plan.md` | Cursor feedback for canvas panels and Outliner layer reordering | #213 — 2026-07-30 | REQ-UI-002/003/011/012 |
| `done/ui-spec-restructure-plan.md` | Per-view UI specifications and pointer-feedback contract | #213 — 2026-07-30 | REQ-UI-001–013 |
| `done/audio-editing-readiness-plan.md` | Output-rate audio preparation cache, end pause forwarding, and preparation feedback | #212 — 2026-07-30 | REQ-MEDIA-002, REQ-UI-003/008 |
| `done/audio-correctness-plan.md` | Epoch-based playback queue, sample-accurate decode, resampling, encoding, and device negotiation | #207 — 2026-07-29 | REQ-MEDIA-002/003 |
| `done/curve-editor-plan.md` | Timeline curve editor | #146 — 2026-07-24 | REQ-UI-012 |
| `done/data-safety-plan.md` | Atomic project persistence, visible failures, gesture isolation, and crash guards | #205 — 2026-07-29 | REQ-PROJ-001/002, REQ-LAYER-009, REQ-GPU-001 |
| `done/eval-render-performance-plan.md` | Background evaluation and GPU-resident rendering | #65–#69 — 2026-07-17 | REQ-CORE-005, REQ-CORE-009, REQ-GPU |
| `done/free-pane-docking-plan.md` | Custom docking system (`ravel-dock`), multi-instance panels, homogeneous windows, AlwaysOnTop, layout persistence — supersedes `panel-placement-plan.md` (#181) | #228–#243 — 2026-08-01 | REQ-UI-005, REQ-UI-009, REQ-UI-013 |
| `done/geometry-pipeline-ui-plan.md` | Geometry nodes, shape compilation, and Viewer integration | #60–#63 — 2026-07-17 | REQ-MOGRAPH-001, REQ-CORE-010 |
| `done/gpui-command-focus-refactor-plan.md` | Command dispatch and focus ownership | #42–#48, #163 — 2026-07-26 | REQ-UI-001, REQ-UI-007 |
| `done/ui-responsiveness-plan.md` | Separating eval-result arrival from document change so panels stop rebuilding per frame | #191–#193 — 2026-07-28 | REQ-UI-001, REQ-CORE-005 |
| `done/layer-network-model-plan.md` | Composition/Layer networks and persistence | #72–#76 — 2026-07-18 | REQ-LAYER-001–011, REQ-CORE-001, REQ-UI-003 |
| `done/node-expansion-plan.md` | Scalar math, geometry transform/merge, and frame port | #87–#90 — 2026-07-18 | REQ-LAYER-002 |
| `done/outliner-comp-management-plan.md` | Outliner panel and composition management | #154–#159, #161 — 2026-07-26 | REQ-UI-013, REQ-UI-003 |
| `done/param-input-ports-plan.md` | Node-driven parameter input ports | #83–#86 — 2026-07-18 | REQ-CORE-002, REQ-LAYER-008 |
| `done/param-range-scrub-input-plan.md` | Parameter ranges and numeric scrub input | #64 — 2026-07-17 | REQ-UI-002 |
| `done/playback-foundation-plan.md` | Frame-accurate playback foundation | #70, #71 — 2026-07-17 | REQ-MEDIA-002, REQ-CORE-005 |
| `done/smoke-test-fixes-plan.md` | Viewer, node editor, timeline, and UI smoke-test fixes | #92–#135 — 2026-07-21 | REQ-CORE-001, REQ-CORE-002, REQ-UI-002–004 |
| `done/tool-system-plan.md` | Viewer selection, shape, and pen tools | #138–#151 — 2026-07-25 | REQ-UI-011 |
| `done/viewer-comp-coordinate-scale-plan.md` | Composition-space Viewer scaling | #129 — 2026-07-21 | REQ-CORE-001, REQ-CORE-009 |
