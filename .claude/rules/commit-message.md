# Git Rules

## Branch Naming
- Conventional prefix required: `feat/`, `fix/`, `refactor/`, `docs/`, `test/`, `chore/`, `perf/`, `ci/`
- Kebab-case after prefix (e.g. `feat/node-graph-evaluator`, `fix/timeline-crash`)
- Use concrete feature/fix names — no abstract labels (`feat/phase1`, `fix/review-feedback` are bad)
- Examples:
  - Good: `feat/dag-topological-sort`
  - Good: `fix/wgpu-shader-compilation`
  - Bad: `feat/step2`
  - Bad: `chore/cleanup`

## PR Title
- Same format as commit messages (Conventional prefix + concrete description)

## Commit Granularity
- Commit in logical units — one concept per commit
- Do NOT batch unrelated changes into a single commit
- Do NOT commit everything at the end — commit as each logical unit is complete
- Examples of logical units:
  - Adding a new type/trait definition
  - Implementing a single feature or function
  - Adding tests for a specific module
  - Fixing a specific bug
  - Updating configuration or CI

## Commit Message Format
- Single line only — no multi-line messages
- English only
- Prefix required: `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`, `perf:`, `ci:`
- Be specific about what changed — not why or how it was found
  - Good: `feat: add NodeData trait hierarchy and concrete types`
  - Good: `test: add unit tests for topological sort`
  - Bad: `feat: implement task-001`
  - Bad: `fix: codex review`
- Do NOT include task IDs (TASK-001), issue numbers, or ticket references
- Lowercase after prefix
