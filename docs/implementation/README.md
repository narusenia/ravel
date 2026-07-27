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

## Planned

Ordered by dependency. `per-instance-modulation-plan.md` has no prerequisites
and is the entry point for the REQ-MOGRAPH work.

| File | Subject | Depends on | Related requirements |
|---|---|---|---|
| `evaluation-scope-plan.md` | `PathSegment` scope axis, graph-internal iteration, group convention | — | REQ-CORE-013, REQ-CORE-002/011 |
| `geometry-ops-plan.md` | Blast, sort, resample, measure, switch, null | `evaluation-scope-plan.md` | REQ-CORE-010, REQ-MOGRAPH-001 |
| `per-instance-modulation-plan.md` | Field-driven per-instance attribute modulation, `attribute.delete` | — | REQ-MOGRAPH-001, REQ-CORE-010, REQ-CORE-012 |
| `panel-placement-plan.md` | View toggles for panels the active preset does not lay out (#181) | — | REQ-UI-013, REQ-UI-001 |
| `attribute-spreadsheet-plan.md` | Geometry attribute inspection panel, multi-target evaluation | `panel-placement-plan.md` | REQ-CORE-010, REQ-UI-013 |
| `typography-plan.md` | Text layout, glyph geometry, path text, per-character modulation | `per-instance-modulation-plan.md` | REQ-MOGRAPH-004 |
| `stateful-eval-plan.md` | `StatefulProcessor` and the simulation cache | — | REQ-CORE-011 |
| `particle-plan.md` | Particle simulation as point geometry | `stateful-eval-plan.md`, `per-instance-modulation-plan.md` | REQ-MOGRAPH-002 |
| `effects-library-plan.md` | Colour, blur, distortion, generation, stylise, and time nodes | — | REQ-MOGRAPH-005 |
| `gpu-resident-geometry-plan.md` | `GpuGeometry`, WGSL fields — **phase 0 may cancel it** | `per-instance-modulation-plan.md` | REQ-CORE-009, REQ-GPU-001/003 |
| `3d-basics-sketch.md` | 3D text extrusion, primitives, camera, lighting — **sketch only** | `typography-plan.md`, `gpu-resident-geometry-plan.md` | REQ-MOGRAPH-003 |

Two plans both change `EvalRequest` / `EvalUpdate`
(`attribute-spreadsheet-plan.md` unit 1 makes them multi-target;
`stateful-eval-plan.md` unit 3 adds a provisional-result flag). Decide the
order before starting either.

**`evaluation-scope-plan.md` unit 1 comes before `stateful-eval-plan.md`.**
The evaluator caches one value per `(path, node)` — `frame` is a validity
check, not a key — so simulation caching, time remapping, and graph-internal
iteration all hit the same wall. Extending `PathSegment` once keeps them from
growing three separate mechanisms.

The motion-graphics plans implement on the CPU and keep the GPU boundary open
rather than building for it. Each carries a "GPU 方針" section stating what is
and is not measured, its migration point, and its numeric trigger.

The one geometry measurement on record (`perf-baseline.md` scenario c,
0.007 ms) is a **warm-cache** number and does not show that CPU geometry
evaluation is cheap — no plan may cite it as such.
`gpu-resident-geometry-plan.md` exists to measure the uncached path and may
cancel itself on the result. Effects nodes are GPU already; particles are the
one place GPU genuinely pays, and that unit is gated on a VRAM-cache decision.

## Reference

| File | Subject |
|---|---|
| `backlog.md` | **Every implementation unit in one table** — the entry point for picking up work |
| `plan.md` | Implementation overview by subsystem |
| `perf-baseline.md` | Evaluation and render measurements |
| `path-channel-design.md` | Path animation design memo (#150); implementation deferred |

## Done

| File | Subject | Merged | Related requirements |
|---|---|---|---|
| `done/curve-editor-plan.md` | Timeline curve editor | #146 — 2026-07-24 | REQ-UI-012 |
| `done/eval-render-performance-plan.md` | Background evaluation and GPU-resident rendering | #65–#69 — 2026-07-17 | REQ-CORE-005, REQ-CORE-009, REQ-GPU |
| `done/geometry-pipeline-ui-plan.md` | Geometry nodes, shape compilation, and Viewer integration | #60–#63 — 2026-07-17 | REQ-MOGRAPH-001, REQ-CORE-010 |
| `done/gpui-command-focus-refactor-plan.md` | Command dispatch and focus ownership | #42–#48, #163 — 2026-07-26 | REQ-UI-001, REQ-UI-007 |
| `done/layer-network-model-plan.md` | Composition/Layer networks and persistence | #72–#76 — 2026-07-18 | REQ-LAYER-001–011, REQ-CORE-001, REQ-UI-003 |
| `done/node-expansion-plan.md` | Scalar math, geometry transform/merge, and frame port | #87–#90 — 2026-07-18 | REQ-LAYER-002 |
| `done/outliner-comp-management-plan.md` | Outliner panel and composition management | #154–#159, #161 — 2026-07-26 | REQ-UI-013, REQ-UI-003 |
| `done/param-input-ports-plan.md` | Node-driven parameter input ports | #83–#86 — 2026-07-18 | REQ-CORE-002, REQ-LAYER-008 |
| `done/param-range-scrub-input-plan.md` | Parameter ranges and numeric scrub input | #64 — 2026-07-17 | REQ-UI-002 |
| `done/playback-foundation-plan.md` | Frame-accurate playback foundation | #70, #71 — 2026-07-17 | REQ-MEDIA-002, REQ-CORE-005 |
| `done/smoke-test-fixes-plan.md` | Viewer, node editor, timeline, and UI smoke-test fixes | #92–#135 — 2026-07-21 | REQ-CORE-001, REQ-CORE-002, REQ-UI-002–004 |
| `done/tool-system-plan.md` | Viewer selection, shape, and pen tools | #138–#151 — 2026-07-25 | REQ-UI-011 |
| `done/viewer-comp-coordinate-scale-plan.md` | Composition-space Viewer scaling | #129 — 2026-07-21 | REQ-CORE-001, REQ-CORE-009 |
