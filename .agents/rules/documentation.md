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
- When a change alters a public API, a registration path (node processors,
  panels, commands), or an asset format (locales, keybindings, workspaces,
  themes, `.ravprj`), update the matching how-to page in `docs/dev/` in the
  same change. Those pages carry the checklists contributors follow, so a
  stale one silently costs the next person a failed `mise run check`.
- Separate the roles: `.agents/rules/` states what must hold, `docs/dev/`
  states how to do it, `docs/agent-api-reference.md` maps types and functions,
  `docs/specifications/` states intended behaviour, and
  `docs/ui-impl-status.md` states what actually works today. Do not duplicate
  the same content across two of them.
- Keep `CLAUDE.md` as the thin `@AGENTS.md` import so repository guidance has a
  single canonical entry point.
- Do not describe planned features as implemented behavior.
