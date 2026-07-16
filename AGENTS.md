# Ravel repository guide

## Project overview

Ravel is a Rust desktop application for video editing and procedural motion
graphics. Its core model is an immutable node DAG. The current timeline model
is an After Effects-style `Composition` containing ordered `Layer`s; a
composition is compiled into synthetic DAG nodes for evaluation.

Treat the implementation as authoritative when older planning documents differ
from the code. In particular, do not assume the earlier Track/Clip timeline
model is still current.

## Repository map

- `crates/ravel-core`: immutable graph, evaluator, data types, animation,
  Composition/Layer model, undo, recovery, and runtimes
- `crates/ravel-nodes`: built-in CPU/GPU node processors and WGSL shaders
- `crates/ravel-gpu`: shared wgpu device, compute pipelines, shader management,
  texture pooling, and transfers
- `crates/ravel-media`: FFmpeg-backed media decode/encode, hardware acceleration,
  format detection, and image sequences
- `crates/ravel-audio`: CPAL output, mixing, resampling, synchronization, effects,
  and waveform generation
- `crates/ravel-i18n`: locale loading and the `t!` translation macro
- `crates/ravel-ui`: headless shell state, commands, keybindings, panels,
  properties, menus, and workspace presets
- `crates/ravel-app`: GPUI host, windows and docking, concrete panels, widgets,
  project persistence, and the application entry point
- `assets`: locales, keybindings, and workspace preset data
- `docs/requirements`: product requirements
- `docs/specifications`: architecture, data model, and UI specifications
- `docs/implementation`: implementation plans and task notes

Important references:

- `docs/specifications/architecture.md`
- `docs/specifications/data-model.md`
- `docs/specifications/ui-spec.md`
- `docs/gpui-ui-guide.md`
- `docs/implementation/gpui-command-focus-refactor-plan.md`

## Architecture constraints

- Keep UI concerns out of `ravel-core`. Core graph, composition, evaluation, and
  persistence logic must remain usable without a live UI.
- Preserve the immutable graph model. Graph mutations return a new `Graph` and
  use `im` plus `Arc` for structural sharing; do not introduce ad-hoc mutable
  graph state.
- Preserve the Hybrid Pull + Dirty Notification evaluation model. Parameter or
  wiring changes mark affected nodes downstream; output evaluation pulls only
  the required upstream graph.
- The `Document` snapshot is the undo unit for graph and composition changes.
  Mutations that affect both must remain atomic from undo/redo's perspective.
- Internal image and numeric processing uses 32-bit float without artificial
  resolution or frame-rate limits.
- Keep blocking media I/O, decoding, graph evaluation, and other expensive work
  off the GPUI thread.
- Reuse the workspace-pinned `wgpu` revision. Do not add a second incompatible
  wgpu version to application-facing GPU paths.

## GPUI conventions

- Read `docs/gpui-ui-guide.md` before adding a panel or custom GPUI component.
- Bootstrap through `gpui_platform::application()` and wrap window roots with
  `gpui_component::Root`.
- Use GPUI Actions for commands and shortcuts. Do not add new raw modifier/key
  checks for operations that belong in the command system.
- Do not dispatch commands, change focus, or mutate application state as a side
  effect of `Render::render()`.
- A child editor or input owns focus until an explicit user action moves it.
  Do not unconditionally return focus to a workspace or panel during render.
- Do not introduce new `Global<Option<Event>>` values for one-shot events.
  Prefer Actions for commands and `EventEmitter`/subscriptions for component
  events. Globals are for durable shared state.
- Call `cx.notify()` after state changes that affect rendering, and retain
  `Subscription`s for as long as their observers must remain active.
- Avoid updating an Entity from inside another update of the same Entity.
- Use the inner context passed into Entity update closures.

## Coding conventions

- New Rust source files must start with the existing dual-license header:

  ```rust
  // Copyright 2026 Ravel Contributors
  // SPDX-License-Identifier: Apache-2.0 OR MIT
  ```

- UI-facing text must use the `t!` macro and locale assets. Do not hard-code new
  user-visible English or Japanese strings in Rust UI code.
- Use `thiserror` for typed library errors and `anyhow` at application or
  orchestration boundaries.
- Avoid `unwrap()`, `expect()`, and discarded fallible results in production
  paths. Handle, propagate, or log errors with useful context.
- Keep `unsafe` limited to reviewed platform/FFI boundaries where a safe
  alternative is not available. Document the safety invariant beside it.
- Do not add GPL dependencies when they would impose GPL terms on distributed
  Ravel binaries. FFmpeg must remain dynamically linked.
- Ask before adding a new production dependency or changing a pinned git
  dependency.
- Follow existing module organization and naming in the crate being changed.
  Avoid unrelated cleanup in a focused change.

## Build and verification

Use targeted checks while iterating, then broaden verification in proportion to
the change.

```bash
# Formatting
cargo fmt --all -- --check

# Affected crate
cargo test -p <crate-name>

# Normal workspace verification
cargo test --workspace

# Full verification, including integration tests and benches-as-targets
cargo test --workspace --all-targets

# Lints for a broad Rust change
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Notes:

- GPU tests require an available graphics adapter.
- FFmpeg integration tests require the appropriate shared libraries and may be
  compiled or skipped according to active features and the host environment.
- Do not regenerate ignored assets or snapshots unless the task requires it.
- Add regression tests for bug fixes. Prefer headless tests in `ravel-ui` or
  `ravel-core`; use GPUI integration tests only for behavior that depends on
  actual focus, action propagation, or rendering.

## Documentation

- Check whether a behavior or architecture change invalidates requirements,
  specifications, implementation plans, locale assets, or keybinding assets.
- Update the relevant documentation in the same change when behavior changes.
- Keep durable agent instructions here concise. Put task-specific plans under
  `docs/implementation/` and link them from here only when they remain useful.

## Git and change hygiene

- Preserve user changes and unrelated worktree modifications.
- Do not commit, push, or open a pull request unless the user asks.
- When commits are requested, use one logical concept per commit and an English
  one-line Conventional Commit message:
  `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`, `perf:`, or `ci:`.
- Branch names use the same semantic prefix followed by a concrete kebab-case
  description, for example `fix/node-editor-shortcuts`.
- Do not put task IDs, issue numbers, review provenance, or agent names in commit
  messages unless the user explicitly requests them.

## Definition of done

A change is complete when:

- the requested behavior is implemented without unrelated changes;
- relevant tests cover the behavior or the lack of an automated test is stated;
- formatting and appropriate targeted checks pass;
- broader tests are run when the risk warrants them;
- errors and platform constraints are handled explicitly;
- affected documentation, locale data, and assets are updated; and
- the final handoff reports changed files, verification performed, and any
  remaining limitations.
