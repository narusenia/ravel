#!/usr/bin/env bash
# Claude Code PreToolUse hook: block `gh pr create` until the ravel-review
# skill has passed for the current HEAD (see scripts/review-gate.sh).
#
# Matches commands that *invoke* gh pr create (start of a command position),
# not text that merely mentions it. Fails closed: if the command looks like a
# PR creation and the gate cannot be verified, the call is blocked.

set -uo pipefail
input=$(cat)

if command -v jq >/dev/null 2>&1; then
    cmd=$(jq -r '.tool_input.command // empty' <<<"$input" 2>/dev/null)
    # Malformed JSON: fall back to the conservative raw check below.
    [ -n "$cmd" ] || cmd=$input
else
    # Without jq we cannot isolate the command; treat any mention as a match
    # rather than failing open.
    cmd=$input
fi

if ! grep -qE '(^[[:space:]]*|[;&|][[:space:]]*)(env [^;&|]*)?gh pr create' <<<"$cmd"; then
    exit 0
fi

repo=$(git rev-parse --show-toplevel 2>/dev/null) || {
    echo "review-gate hook: cannot resolve the repository root; blocking gh pr create." >&2
    exit 2
}
cd "$repo" || exit 2
if ! bash scripts/review-gate.sh --check; then
    exit 2
fi
exit 0
