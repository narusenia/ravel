#!/usr/bin/env bash
# Review gate marker for the ravel-review skill.
#
#   --mark   record that ravel-review passed for the current HEAD tree
#   --check  succeed only if the marker matches the current HEAD tree
#
# The PreToolUse hook in .claude/settings.json runs `--check` before
# `gh pr create`; the ravel-review skill runs `--mark` after a PASS verdict.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

marker=".git/ravel-review-ok"
tree=$(git rev-parse 'HEAD^{tree}')

case "${1:-}" in
--mark)
    echo "$tree" >"$marker"
    echo "review-gate: marked $tree"
    ;;
--check)
    if [ ! -f "$marker" ] || [ "$(cat "$marker")" != "$tree" ]; then
        echo "review-gate: ravel-review has not passed for the current HEAD." >&2
        echo "Run the ravel-review skill on this diff; it records the marker on PASS." >&2
        exit 1
    fi
    ;;
*)
    echo "usage: review-gate.sh --mark|--check" >&2
    exit 2
    ;;
esac
