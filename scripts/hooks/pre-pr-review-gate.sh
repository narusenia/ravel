#!/usr/bin/env bash
# Claude Code PreToolUse hook: block `gh pr create` until the ravel-review
# skill has passed for the branch being proposed (see scripts/review-gate.sh).
#
# Matches commands that *invoke* gh pr create (start of a command position),
# not text that merely mentions it. Fails closed: if the command looks like a
# PR creation and the gate cannot be verified, the call is blocked.
#
# The hook runs in the session's working directory, which is not necessarily
# the checkout the PR is opened from — reviewing a branch in a scratch
# `git worktree` is the normal flow. So the branch is taken from the command
# itself (`--head`, or a leading `cd`) rather than from the session's HEAD.

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

# `--head branch`, `--head=branch` or `-H branch`. A fork PR spells it
# `owner:branch`; the local ref is the part after the colon.
head_ref=$(sed -nE 's/.*(^|[[:space:]])(--head[= ]|-H )[[:space:]]*"?'"'"'?([^[:space:]"'"'"']+).*/\3/p' <<<"$cmd" | head -n 1)
head_ref=${head_ref#*:}

# Otherwise gh infers the branch from the directory the command runs in, so a
# leading `cd <dir>` decides which checkout is meant.
cd_dir=$(sed -nE 's/^[[:space:]]*cd[[:space:]]+"?'"'"'?([^"'"'"'&;|]+).*/\1/p' <<<"$cmd" | head -n 1)
cd_dir=${cd_dir%"${cd_dir##*[![:space:]]}"}

repo_dir=${cd_dir:-.}
repo=$(git -C "$repo_dir" rev-parse --show-toplevel 2>/dev/null) || {
    echo "review-gate hook: cannot resolve the repository root; blocking gh pr create." >&2
    exit 2
}

# Run the checker that ships with *this* hook, not the copy in the target
# checkout: a scratch worktree sits on the branch under review and may predate
# the gate itself. `cd` only sets the git context the check runs against.
gate="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../review-gate.sh"
cd "$repo" || exit 2

if ! bash "$gate" --check "${head_ref:-HEAD}"; then
    exit 2
fi
exit 0
