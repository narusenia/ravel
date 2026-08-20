---
paths:
  - "Cargo.toml"
  - "crates/**/Cargo.toml"
  - "crates/**/*.rs"
  - "crates/**/*.wgsl"
---

# Rust and architecture rules

- Keep UI concerns out of `ravel-core`. Core graph, composition, evaluation,
  and persistence logic must work without a live UI.
- Preserve the immutable graph model. Graph mutations return a new `Graph` and
  use `im` plus `Arc` for structural sharing.
- Preserve Hybrid Pull + Dirty Notification evaluation. Mark affected nodes
  downstream and pull only the upstream graph required by the requested output.
- Treat the `Document` snapshot as the undo unit for graph and composition
  changes. Cross-cutting mutations must remain atomic for undo/redo.
- Keep blocking I/O, decoding, graph evaluation, and expensive work off the UI
  thread.
- Reuse the workspace-pinned `wgpu` revision. Do not introduce a second
  incompatible wgpu version into application-facing GPU paths.
- Keep backend-native GPU handles inside `ravel_gpu::interop`. Handing a
  backend pointer out (`native_device`, `native_texture`, `NativeHandle`,
  `NativeDevice`, `NativeTexture`) serves the OpenFX host and hardware decode
  only, so the crates allowed to name those symbols are `ravel-gpu` itself,
  `ravel-media` and the future OFX host crate. Everything else — node
  processors, the core layer, and the UI and application crates alike — uses
  the abstract API: a processor that reaches a handle is pinned to one backend
  and silently opts out of dispatch batching and the texture pool's lifetime
  bookkeeping. The `gpu-native-handle-escape` lint enforces exactly that set.
  `interop::native_api` is not part of it — it reads the adapter description,
  names no pointer, and any crate may ask. **Both lints match symbol names, not
  the module path**, so adding a new item to `interop.rs` obliges you to add it
  to whichever of the two lists it belongs to; a new handle accessor is
  unguarded until you do.
- Keep the device-sharing entry points a contract, not a hole.
  `interop::context_from_wgpu` and `interop::wgpu_instance` are the direction
  where Ravel *receives* the graphics objects, because REQ-GPU-001 requires the
  UI framework and the compute pipeline to run on one device and a shared
  device is by definition one the host creates and Ravel accepts. Only
  `ravel-gpu` and `ravel-app` (the GPUI host) may name them: the call happens
  once at startup and decides which device the whole evaluation pipeline runs
  on, which is the application host's decision alone. The `gpu-device-sharing`
  lint enforces that pair. Do not widen it to make a library crate build its
  own context.
- Keep `wgpu` types out of `ravel-gpu`'s public API — signatures, public
  fields, public constants — with `interop` as the only exception. Describe
  the work in the crate's own vocabulary (`BindingDesc`, `TextureKey`,
  `ComputeDispatch`, `PooledTexture`, `AdapterInfo`) and convert to the
  backend inside the crate, at one site per type. The `gpu-facade-wgpu` lint
  enforces it. The exception is not negotiable away for the device-sharing
  entry points in particular: naming the toolkit's device type is what they are
  for, so replacing the backend changes those signatures too. That is the
  definition of the interop boundary, not a leak through it.
- New Rust files must use the existing Apache-2.0 OR MIT license header.
- Route user-visible text through `t!` and locale assets.
- Use `thiserror` for typed library errors and `anyhow` at orchestration
  boundaries. Handle, propagate, or log production errors with useful context.
- Limit `unsafe` to reviewed platform or FFI boundaries and document the safety
  invariant.
- Keep FFmpeg dynamically linked. Do not add dependencies that impose GPL terms
  on distributed Ravel binaries.
- Ask before adding a production dependency or changing a pinned git dependency.
- Add regression tests for bug fixes. Prefer headless tests in `ravel-core` or
  `ravel-ui` when the behavior does not require an actual window.
- **Do not match serializer output against a pattern containing `\n`.** RON's
  `PrettyConfig` defaults `new_line` to the platform's newline, so a fixture
  that rewrites or searches emitted text with an `\n` pattern silently matches
  nothing on Windows — and a local run can never show it. The project writers
  pin LF (`document_to_ron`, `GraphDoc::to_ron`, `subgraph_template::to_ron`),
  so text from **those** is safe; anything else must normalize
  (`.replace("\r\n", "\n")`) before it looks for a line break. This is not a
  lint: `\n` patterns are correct once the text is normalized, and a grep that
  cannot see the normalization would only earn an allow entry.

Use targeted checks while iterating and broaden them in proportion to risk.
`mise run check` is the full pre-PR verification (fmt + pattern lint + clippy
+ workspace tests); individual tasks and targeted equivalents:

```bash
mise run lint:patterns
cargo fmt --all -- --check
cargo test -p <crate-name>
cargo test --workspace
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

GPU tests require an adapter. FFmpeg integration coverage depends on active
features and available shared libraries. Do not regenerate ignored assets or
snapshots unless the task requires it.
