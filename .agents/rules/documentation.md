---
paths:
  - "AGENTS.md"
  - "CLAUDE.md"
  - "README.md"
  - "README_ja.md"
  - "docs/**/*.md"
---

# Documentation rules

- Treat current implementation as authoritative when older planning documents
  disagree with code, and update stale documentation encountered in task scope.
- Check whether behavior or architecture changes affect requirements,
  specifications, implementation plans, locale assets, or keybinding assets.
- Update affected documentation in the same change.
- Keep `AGENTS.md` concise and durable. Put task-specific plans in
  `docs/implementation/`.
- Keep `CLAUDE.md` as the thin `@AGENTS.md` import so repository guidance has a
  single canonical entry point.
- Do not describe planned features as implemented behavior.
