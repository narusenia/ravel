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
Video node connects decoded media to layer-network evaluation.

The application does not yet provide a complete render queue/export workflow
or a finished media-bin workflow.

### Audio

`crates/ravel-audio` contains CPAL device/output support, mixing, resampling,
synchronization helpers, effects, and waveform generation. Application
playback currently uses the wall-clock playback controller; audio-master
synchronization and full timeline media/audio playback are not integrated.

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
attribute and field operations, scatter/clone nodes, and CPU/GPU rasterization.
The registry in `crates/ravel-core/src/registry/builtin.rs` is the source of
built-in templates and parameter metadata.

The broader effects library, typography, particles/simulation, 3D, custom WGSL
nodes, scripting, and external plugin hosting remain planned requirements, not
implemented features.

### UI panels and interaction

The GPUI application has concrete Node Editor, Timeline, Properties, and Viewer
panels. Implemented interaction includes graph editing, Composition/Layer
timeline editing, keyframe editing and curve view, frame playback controls,
Viewer zoom/pan and overlays, Viewer selection/move, rectangle/ellipse drawing,
and pen-path editing. Command dispatch and focus handling use the centralized
action/command route for the completed refactor phases.

The remaining command/focus refactor work is cross-panel Global signal cleanup;
see `gpui-command-focus-refactor-plan.md`. Other panel kinds that still render
placeholders must not be inferred to be complete from their enum or workspace
presence.

### Persistence

`crates/ravel-app/src/project/` and `project_state.rs` implement `.ravprj`
project save/load with a manifest, deterministic document data, migration and
validation, queued asynchronous I/O handling, and document-wide ID advancement.
Undo recovery journals are separately versioned in `ravel-core`.

Unsaved-change guards and autosave are not complete project workflows.

### Geometry and motion graphics

`crates/ravel-core/src/geometry/` implements typed copy-on-write attributes,
point/instance/detail domains, geometry containers, standard attribute names,
operations, and lazy fields. Shape generators produce geometry; transform,
merge, attribute, field, scatter, and rasterize processors form an evaluable
motion-graphics pipeline. Scatter supports multiple instance sources through
variadic ports, deterministic source selection, and anchor-aware placement.

Stateful particles, simulation caching, advanced per-instance modulation,
attribute-spreadsheet UI, procedural typography, and 3D remain unimplemented.

## How to plan new work

Start from the applicable requirement and specification, then consult the live
index in `docs/implementation/README.md`. Add or update a per-feature plan when
the design gate requires one. Do not derive current architecture or status from
the archived TASK-ID documents.
