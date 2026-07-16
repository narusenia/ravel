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

Use targeted checks while iterating and broaden them in proportion to risk:

```bash
cargo fmt --all -- --check
cargo test -p <crate-name>
cargo test --workspace
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

GPU tests require an adapter. FFmpeg integration coverage depends on active
features and available shared libraries. Do not regenerate ignored assets or
snapshots unless the task requires it.
