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

Stateful simulation evaluation and the multi-tier frame cache described by
REQ-CORE-006/011 are not implemented. The node-level cache holds one value per
`(path, node)` with no byte limit, its validity check ignores sub-frame time,
and nothing reports hit rates. `cache-plan.md` owns the cache identity, the
single byte budget, the output-stage frame cache, and the measurement API.

The network interface is evaluable but not editable: `net.in` custom output
ports, `net.out` custom input ports, and recursive `subnet` graphs all evaluate,
yet no API or UI adds, removes, renames, or reorders a custom port — the
output-side reindex counterpart of `remove_input_port_and_reindex` does not
exist, and `NodeTemplate::create_node` never populates `Node::subnet`, so a
Subnet added from the menu fails evaluation. See
`network-interface-editing-plan.md`. Networks also cannot read their own
context: no node exposes layer or composition metadata
(`scene-info-nodes-plan.md`), and `precomp` is reserved with cycle detection
only, with no processor.

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

View toggles only reach panels the active workspace preset lays out (#181,
now owned by `free-pane-docking-plan.md` DOCK-2). Other panel kinds that still render placeholders
must not be inferred to be complete from their enum or workspace presence.

Viewer overlays (grid, safe areas, selection bounding boxes, pen-path handles)
are painted inline in one canvas closure, and the selection bounding box is
reconstructed from parameters through a hardcoded `type_key` match rather than
from evaluated geometry — a node type absent from that match gets no bounding
box. Overlays therefore cannot visualise anything that has to be evaluated,
including fields, and no manipulator exists for ordinary position parameters.
See `viewer-overlay-manipulator-plan.md`. Curves are editable in place:
`field.curve_remap`'s control points are a `ParameterValue::Curve` with an
inline editor in Properties. Ramps are not — `ParameterValue::Ramp` and its
editor are still open (`properties-parameter-editors-plan.md`).

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

Every built-in field returns `AttributeArray::F32`, and binary composition
coerces through `scalar_values()`, so fields are scalar-valued in practice —
but this is an implementation limit, not an interface one. `apply_field` already
promotes a scalar field into any numeric target by broadcasting to every
selected component, so `scale`, `Cd`, and `P` do modulate; the consequence is
that a scalar field can only move a colour along the grey axis (and drags alpha
with it, since the default component mask covers all four). Fields also see the
whole sampled domain, not position alone, so `field.attribute` can drive
modulation from `index` or any other column. `apply_field` does require the
target attribute to already exist. `vector-field-plan.md` lifts the scalar
limit; `style-attributes-plan.md` covers the default component mask, attribute
auto-creation, and the missing colour-ramp field.

Raster generation is absent: `crates/ravel-nodes/src/comp/` holds only merge,
opacity, and transform, and `rasterize` requires geometry input, so a plain
solid colour cannot be produced at all (`effects-library-plan.md`). Geometry
generation lacks a line, a grid, an element-connecting operator, and a path
parameter attribute (`geometry-ops-plan.md`). Value-domain vectors have no
constant or construct/split nodes, and built-in nodes still declare vectors as
separate `_x` / `_y` `Float` parameters (`vector-field-plan.md`).

Stateful particles, simulation caching, per-instance modulation, attribute
deletion, the attribute-spreadsheet UI, procedural typography, and 3D remain
unimplemented. Every one of these has a planned document — see the index.

## How to plan new work

Start from the applicable requirement and specification, then consult the live
index in `docs/implementation/README.md`. Add or update a per-feature plan when
the design gate requires one. Do not derive current architecture or status from
the archived TASK-ID documents.
