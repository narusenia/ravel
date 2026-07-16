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
  Composition/Layer model, geometry (attributes, container, fields), undo,
  recovery, and runtimes
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

- `docs/agent-api-reference.md` (compact public-API map for coding agents)
- `docs/specifications/architecture.md`
- `docs/specifications/data-model.md`
- `docs/specifications/ui-spec.md`
- `docs/gpui-ui-guide.md`
- `docs/implementation/gpui-command-focus-refactor-plan.md`

## Path-specific rules

Shared rules live under `.agents/rules/`. Before editing files, read every rule
whose `paths` frontmatter matches the files in scope. Claude Code discovers the
same rules through `.claude/rules`; Codex and other agents should follow this
instruction explicitly.

- `.agents/rules/rust.md`: Rust, Cargo, architecture, and verification rules
- `.agents/rules/gpui.md`: GPUI and gpui-component UI rules
- `.agents/rules/documentation.md`: documentation consistency rules

Repository-specific reusable workflows and framework references live under
`.agents/skills/`. Invoke or load a matching skill when the task falls within
its description.

## Design gate

A change that spans multiple crates, multiple panels, or reworks a subsystem
(command dispatch, focus, evaluation, persistence) requires an implementation
plan in `docs/implementation/` before code is written. The plan states the
problem, the target architecture, reviewable implementation units, and
per-phase completion criteria — use
`docs/implementation/gpui-command-focus-refactor-plan.md` as the template.
Small fixes and single-panel features do not need a plan.

## Verification and review

- `mise run check` is the canonical verification entry point (fmt, pattern
  lint, clippy with denied warnings, workspace tests). CI runs the same tasks;
  `mise run hooks:install` enables the pre-commit hooks.
- `scripts/lint-patterns.sh` mechanically enforces the grep-detectable
  anti-patterns from `.agents/rules/`. Never weaken it to pass; add a
  justified entry to `scripts/lint-patterns.allow` only when the rule file
  documents the exception.
- Run the `ravel-review` skill on the diff before opening a pull request. It
  walks the context-dependent invariants (render purity, focus ownership,
  command-path singularity) that the lint cannot see.

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
