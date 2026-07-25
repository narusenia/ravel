# Implementation documentation

Canonical sources of truth are:

- `docs/requirements/REQ-*.md` for product requirements;
- `docs/specifications/` for architecture, data-model, and UI specifications;
- per-feature `docs/implementation/*-plan.md` documents for in-flight work.

`docs/implementation/archive/` contains the historical TASK-ID generation
(`TASK-001`…`TASK-052` and `task-016-plan.md`, `task-017-plan.md`, and
`task-019-plan.md`). It is retained for provenance only and must not be treated
as current design.

## Live documents

| File | Subject | Status | Related requirements |
|---|---|---|---|
| `curve-editor-plan.md` | Timeline curve editor | Done (#146) — 2026-07-24 | REQ-UI-012 |
| `eval-render-performance-plan.md` | Background evaluation and GPU-resident rendering | Done (#65, #66, #67, #68, #69) — 2026-07-17 | REQ-CORE-005, REQ-CORE-009, REQ-GPU |
| `geometry-pipeline-ui-plan.md` | Geometry nodes, shape compilation, and Viewer integration | Done (#60, #61, #62, #63) — 2026-07-17 | REQ-MOGRAPH-001, REQ-CORE-010 |
| `gpui-command-focus-refactor-plan.md` | Command dispatch and focus ownership | In progress (phases 0–4 and 6 done: #42, #43, #44, #45, #46, #47, #48) — next: phase 5 | REQ-UI-001, REQ-UI-007 |
| `layer-network-model-plan.md` | Composition/Layer networks and persistence | Done (#72, #73, #74, #75, #76) — 2026-07-18 | REQ-LAYER-001–011, REQ-CORE-001, REQ-UI-003 |
| `node-expansion-plan.md` | Scalar math, geometry transform/merge, and frame port | Done (#87, #88, #89, #90) — 2026-07-18 | REQ-LAYER-002 |
| `outliner-comp-management-plan.md` | Outliner panel and composition management | Done (#154, #155, #156, #157, #158, #159, #160) — 2026-07-26 | REQ-UI-013, REQ-UI-003 |
| `param-input-ports-plan.md` | Node-driven parameter input ports | Done (#83, #84, #85, #86) — 2026-07-18 | REQ-CORE-002, REQ-LAYER-008 |
| `param-range-scrub-input-plan.md` | Parameter ranges and numeric scrub input | Done (#64) — 2026-07-17 | REQ-UI-002 |
| `path-channel-design.md` | Path animation design memo | Reference (#150) — design memo; implementation deferred | REQ-UI-011 |
| `playback-foundation-plan.md` | Frame-accurate playback foundation | Done (#70, #71) — 2026-07-17 | REQ-MEDIA-002, REQ-CORE-005 |
| `smoke-test-fixes-plan.md` | Viewer, node editor, timeline, and UI smoke-test fixes | Done (#92–#100, #102–#109, #111–#115, #117–#120, #122–#127, #131–#135) — 2026-07-21 | REQ-CORE-001, REQ-CORE-002, REQ-UI-002–004 |
| `tool-system-plan.md` | Viewer selection, shape, and pen tools | Done (#138, #139, #140, #142, #143, #144, #145, #147, #149, #150, #151) — 2026-07-25 | REQ-UI-011 |
| `viewer-comp-coordinate-scale-plan.md` | Composition-space Viewer scaling | Done (#129) — 2026-07-21 | REQ-CORE-001, REQ-CORE-009 |
| `perf-baseline.md` | Evaluation and render measurements | Reference — measurement record | REQ-CORE-005, REQ-CORE-009 |
