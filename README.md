# Ravel

A next-generation video editor combining timeline-based editing with procedural node graph workflows.

## Vision

Ravel brings the power of procedural content creation (inspired by Houdini and Cavalry) to the world of video editing. It features a node-graph-first architecture where everything — effects, compositing, transitions, and even timelines — is represented as a directed acyclic graph (DAG). A familiar timeline interface sits on top as a constrained view, giving users the best of both worlds.

### Key Features

- **Node Graph First** — All processing is expressed as a DAG. The timeline is syntactic sugar over sequence nodes.
- **Procedural Motion Graphics** — Shape generators, repeaters, particle systems, fields/forces, and expression-driven parameters.
- **Procedural Typography** — Per-character animation, text-on-path, text-to-geometry conversion, 3D text extrusion. Built for lyric videos and motion design.
- **Audio Reactive** — FFT spectrum analysis, beat detection, BPM sync, and beat markers on the timeline. Connect audio analysis to any parameter.
- **OpenFX Compatible** — Run industry-standard plugins (Sapphire, BorisFX, etc.) via a C/C++ shim layer with process isolation.
- **OpenColorIO** — Industry-standard color management with GPU-accelerated LUT application.
- **WGSL Custom Shaders** — Write custom GPU effects with live preview.
- **Lua Scripting** — Expression language for parameter control and automation.

## Tech Stack

| Component | Technology |
|-----------|------------|
| Language | Rust |
| UI Framework | GPUI-CE (community edition of Zed's GPUI) |
| GPU Compute | wgpu (unified with UI backend) |
| Media I/O | FFmpeg (LGPL, dynamic link) + native HW decoders |
| Color | OpenColorIO |
| Audio | CPAL + dasp + rubato |
| Scripting | Lua (mlua) |

## Architecture

```
UI Layer (GPUI-CE)  →  Application Layer  →  Core Engine (DAG Eval)
                                              ↓
                                        Media Layer (FFmpeg, CPAL, OCIO)
                                              ↓
                                        GPU Layer (wgpu)
                                              ↓
                                        Platform Layer (Metal / D3D11 / Vulkan)
```

See [docs/specifications/architecture.md](docs/specifications/architecture.md) for the full architecture specification.

## Project File

Ravel uses `.ravprj` files — zip containers with human-readable internals (RON for graphs, JSON for metadata, TOML for settings). Git-friendly by design.

## Platform Support

| Platform | Status | GPU Backend |
|----------|--------|-------------|
| macOS | Primary | Metal |
| Windows | Co-primary | Direct3D 11 / wgpu |
| Linux | Planned | Vulkan / wgpu |
| Web | Experimental | WebGPU |

## License

Open Core — the core engine, built-in nodes, and plugin APIs are open source (Apache 2.0 / MIT). Premium features, templates, and enterprise support are offered commercially.

## Documentation

- [Requirements](docs/requirements/overview.md)
- [Architecture](docs/specifications/architecture.md)
- [Data Model](docs/specifications/data-model.md)
- [UI Specification](docs/specifications/ui-spec.md)
- [Implementation Plan](docs/implementation/plan.md)
