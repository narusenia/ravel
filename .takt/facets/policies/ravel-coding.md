# Ravel Coding Policy

- Follow CLAUDE.md conventions strictly
- Cargo workspace: `crates/ravel-core`, `crates/ravel-gpu`, `crates/ravel-media`, `crates/ravel-ui`, `crates/ravel-app`
- Internal processing: 32bit float, no artificial resolution/FPS limits
- Thread model: rayon (eval), crossbeam (IPC), tokio (I/O only), dedicated audio thread
- Immutable graph with `Arc` + `im` crate for undo
- All nodes evaluated via Hybrid Pull + dirty notification
- Tests: unit tests for core logic, integration tests for pipelines
- Run `cargo test` and `cargo clippy` before declaring implementation complete
