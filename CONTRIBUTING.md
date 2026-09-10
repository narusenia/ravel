# Contributing to Ravel

> 日本語版: [`CONTRIBUTING_ja.md`](CONTRIBUTING_ja.md)

Thanks for looking at Ravel. This page is a map, not a rulebook: the rules
themselves live next to the code they govern, and duplicating them here would
guarantee that one copy goes stale.

By participating you agree to the
[Code of Conduct](CODE_OF_CONDUCT.md).

## Where the rules are

| Read this | For |
|---|---|
| [`AGENTS.md`](AGENTS.md) | **The canonical repository guide** — crate map, design gate, verification, git hygiene, definition of done |
| [`.agents/rules/`](.agents/rules) | The rules a change must hold: Rust and architecture, GPUI, the twelve UX invariants, documentation consistency |
| [`docs/dev/`](docs/dev) | How to do a specific thing: add a node, a panel, a widget, a command, a locale string; change persistence; write a theme; test |
| [`docs/README.md`](docs/README.md) | Which document plays which role |

`AGENTS.md` is written for coding agents, but nothing in it is
agent-specific: it is simply where the repository's conventions are kept, and
`CLAUDE.md` is a one-line import of it.

## Getting set up

Ravel builds with stable Rust. Task running is [mise](https://mise.jdx.dev/):

```bash
mise trust          # once per clone or git worktree, before any `mise run`
mise run hooks:install   # optional: pre-commit fmt / lint / clippy / docs
mise run check      # fmt + anti-pattern lint + clippy -D warnings + tests
```

`mise run check` is what CI runs. Run it before opening a pull request.

Two build notes that cost people time:

- **`cargo build -p ravel-cli`, never `cargo build --workspace`,** when you
  want the headless binary. Cargo unifies features across one build, so a
  workspace build links an audio device library into `ravel-cli` — the exact
  thing its feature split exists to avoid.
- A fresh `git worktree` has a cold target directory, so the pre-commit
  clippy hook can take several minutes on the first commit.

## Making a change

- **Branch names**: a semantic prefix plus a concrete kebab-case
  description — `fix/node-editor-shortcuts`, not `fix/phase2`.
- **Commits**: one logical concept each, and an English one-line
  [Conventional Commit](https://www.conventionalcommits.org/) subject
  (`feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`, `perf:`, `ci:`).
  No issue numbers or ticket references in the subject.
- **Tests**: a bug fix carries a regression test, and the test should fail if
  the fix is reverted. Prefer headless tests (`ravel-core`, `ravel-ui`) when
  the behaviour does not need a window.
- **Documentation**: [`docs/dev/doc-checklist.md`](docs/dev/doc-checklist.md)
  maps each kind of change to the documents it obliges. `mise run docs:check`
  verifies links, index coverage and issue counts.
- **Dependencies**: ask before adding a production dependency or moving a
  pinned git dependency. Ravel keeps FFmpeg dynamically linked and takes no
  dependency that would impose GPL terms on distributed binaries.
- **A change that spans crates or panels, or reworks a subsystem** (command
  dispatch, focus, evaluation, persistence) wants an implementation plan in
  [`docs/implementation/`](docs/implementation) before the code. Small fixes
  and single-panel features do not.

## Pull requests

The template asks for the things a reviewer cannot reconstruct from the diff:
why this change now, **the design decisions and their reasons**, the grounds
for believing existing behaviour is unchanged, and what verification you
actually ran. Filling those in is the review.

## Where issues live

- **GitHub Issues** is the place to report a bug or ask for a feature. Use the
  templates; both are offered in English and Japanese.
- The [`issues/`](issues) directory in this repository is something else: an
  internal ledger of audit findings (`MED-APP-05`, `LOW-CORE-03`, …) with
  their own severity files and index. **Please do not add entries there in a
  pull request** unless a maintainer asks — the numbering is assigned
  centrally, and `mise run docs:check` verifies the counts.

## Language

Code, comments, commit subjects and public-facing documents are English.
Much of `docs/` is Japanese, and both languages are welcome in issues and
pull requests — write in whichever you think more clearly.

## Licence

Ravel is dual licensed under [Apache 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT). Contributions are accepted under the same terms; new
source files carry the `Apache-2.0 OR MIT` SPDX header the existing ones use.
