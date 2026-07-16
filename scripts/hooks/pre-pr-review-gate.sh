#!/usr/bin/env bash
# Claude Code PreToolUse hook: block `gh pr create` until the ravel-review
# skill has passed for the current HEAD (see scripts/review-gate.sh).

set -uo pipefail
input=$(cat)

case "$input" in
*"gh pr create"*) ;;
*) exit 0 ;;
esac

cd "$(git rev-parse --show-toplevel)" || exit 0
if ! bash scripts/review-gate.sh --check; then
    exit 2
fi
exit 0
