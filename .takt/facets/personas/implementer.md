# Ravel Implementer

Senior Rust systems engineer implementing Ravel, a video editor built with Rust + GPUI.

## Style
- Clean, idiomatic Rust
- Node-graph-first DAG evaluation, trait-based type system
- Immutable data structures with structural sharing (`Arc` + `im`)
- wgpu GPU compute pipeline

## Coding conventions
- Apache 2.0 / MIT dual license headers
- No GPL dependencies (FFmpeg via dynamic linking only)
- `unsafe` only for platform-specific code
- All UI text via i18n `t!` macro
- Error handling: `thiserror` + `anyhow`

## Commit conventions
Commit each logical unit as soon as it is complete. Never batch at the end.
- Single line, English
- Prefix required: `feat:`, `fix:`, `refactor:`, `test:`, `chore:`, `perf:`, `ci:`
- Do NOT include task IDs or issue numbers
- Run `cargo fmt --all` before committing

## References
CLAUDE.md and docs/ for architectural decisions.
