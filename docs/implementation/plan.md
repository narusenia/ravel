# Ravel — current implementation overview

This document is a high-level snapshot of the implementation. Requirements in
`docs/requirements/REQ-*.md` and the architecture, data-model, and UI documents
in `docs/specifications/` remain authoritative. See
`docs/implementation/README.md` for the live per-feature plan index.

The former TASK-ID plan is archived under `docs/implementation/archive/`. It is
retained for provenance and is not a statement of current design or progress.

## Current timeline and graph model

The Track/Clip timeline model described by early planning documents is
obsolete. The current model is the REQ-LAYER Composition/Layer model: a
`Document` contains compositions, each composition owns an ordered list of
layer shells, and each layer owns one node network. The shell holds time
placement, transform, opacity, and blend state. `compile_composition` expands
that shell into synthetic DAG nodes and evaluation crosses layer-network
boundaries recursively.

The implementation is in `crates/ravel-core/src/composition/`, with the
boundary processors and shell-compositing processors in
`crates/ravel-nodes/src/`. The Timeline, Node Editor, Properties, Viewer, and
project state consume this model rather than a Track/Clip compatibility path.

## Subsystem status

### Core and evaluation

Implemented:

- immutable graphs, typed ports, validation, parameter input ports, variadic
  inputs, animation channels, undo, and versioned recovery journals in
  `crates/ravel-core`;
- pull evaluation with cache invalidation and recursive layer, layer-reference,
  and subnet boundaries;
- `EvalService` background evaluation with latest-wins request coalescing and
  generation-filtered results;
- frame-accurate wall-clock playback primitives.

Stateful simulation evaluation and the complete multi-tier cache described by
REQ-CORE-006/011 are not implemented.

### Media

`crates/ravel-media` provides FFmpeg-backed decode and encode, format probing,
image-sequence support, and hardware-acceleration device/transfer support. A
single `media` node connects decoded video, stills, and image sequences to
layer-network evaluation (`video` remains a load-time alias). Import runs
through File ▸ Import and OS file drops, assets persist as runtime-resolved
relative paths, and the MediaBin panel browses them with cached thumbnails.

Media properties and relinking, offline-asset display, and a complete render
queue/export workflow are not implemented.

### Audio

`crates/ravel-audio` contains CPAL device/output support, mixing, resampling,
synchronization helpers, effects, and waveform generation. `ravel-app` wires it
up: audio-carrying layers become mixer tracks through `AudioMixdown`, the
engine starts lazily (a missing device falls back to the wall clock), and the
playback clock follows the device's `SyncClock` while audio tracks exist.
Importing media with sound binds the layer shell's `AudioSource`, and the
Properties panel picks which container stream plays
(`docs/implementation/audio-plan.md`, units 1–4).

Waveform display, audio analysis nodes, the tagged sound bank, and audio
export are not implemented.

### GPU and rendering

`crates/ravel-gpu` implements shared wgpu device management, compute and raster
pipelines, shader management, texture pooling, transfers, and GPU frame
buffers. GPU node chains can remain resident across intermediate operations,
and geometry has GPU rasterization with a CPU reference path. The Viewer
receives evaluated images through the background evaluation path.

Zero-copy Viewer presentation, a render queue, Write node, and end-to-end
export are not implemented.

### Built-in nodes

`crates/ravel-nodes` includes constants, scalar math/remap, image blur/color
correction/transform/merge, Composition shell processors, network boundaries,
layer references, subnets, Video, shape generators, geometry transform/merge,
attribute and field operations, scatter/clone nodes, the unified media node,
and CPU/GPU rasterization.
The registry in `crates/ravel-core/src/registry/builtin.rs` is the source of
built-in templates and parameter metadata.

The broader effects library, typography, particles/simulation, 3D, custom WGSL
nodes, scripting, and external plugin hosting remain planned requirements, not
implemented features.

### UI panels and interaction

The GPUI application has concrete Node Editor, Timeline, Properties, Viewer,
Outliner, and MediaBin panels. Implemented interaction includes graph editing,
Composition/Layer timeline editing, composition management and multi-layer
selection from the Outliner, keyframe editing and curve view, frame playback
controls, Viewer zoom/pan and overlays, Viewer selection/move,
rectangle/ellipse drawing, and pen-path editing. Command dispatch and focus
handling use the centralized action/command route
(`done/gpui-command-focus-refactor-plan.md`, complete).

View toggles only reach panels the active workspace preset lays out (#181, see
`panel-placement-plan.md`). Other panel kinds that still render placeholders
must not be inferred to be complete from their enum or workspace presence.

### Persistence

`crates/ravel-app/src/project/` and `project_state.rs` implement `.ravprj`
project save/load with a manifest, deterministic document data, migration and
validation, queued asynchronous I/O handling, and document-wide ID advancement.
Undo recovery journals are separately versioned in `ravel-core`.

Destructive actions (New / Open / Quit / main-window close) are guarded by an
unsaved-changes confirmation keyed on the last completed save's revision.
Autosave and journal-replay recovery are not complete project workflows.

### Geometry and motion graphics

`crates/ravel-core/src/geometry/` implements typed copy-on-write attributes,
point/instance/detail domains, geometry containers, standard attribute names,
operations, and lazy fields. Shape generators produce geometry; transform,
merge, attribute, field, scatter, and rasterize processors form an evaluable
motion-graphics pipeline. Scatter supports multiple instance sources through
variadic ports, deterministic source selection, and anchor-aware placement.

Fields are scalar-only and `apply_field` requires an exact type match, so a
scalar field can currently modulate `rot` and `alpha` but not `scale`, `Cd`, or
`P`; fields also sample position alone, with no access to `index` or other
attributes. Stateful particles, simulation caching, per-instance modulation,
attribute deletion, the attribute-spreadsheet UI, procedural typography, and 3D
remain unimplemented. Every one of these has a planned document — see the
index.

## How to plan new work

Start from the applicable requirement and specification, then consult the live
index in `docs/implementation/README.md`. Add or update a per-feature plan when
the design gate requires one. Do not derive current architecture or status from
the archived TASK-ID documents.
