#!/usr/bin/env bash
# Review gate marker for the ravel-review skill.
#
#   --mark  [ref]   record that ravel-review passed for `ref`'s tree (default HEAD)
#   --check [ref]   succeed only if `ref`'s tree has been marked (default HEAD)
#
# The PreToolUse hook in .claude/settings.json runs `--check` before
# `gh pr create`; the ravel-review skill runs `--mark` after a PASS verdict.
#
# Markers are keyed by tree hash and stored in the **common** git directory,
# so a review recorded in one `git worktree` is visible from every other one.
# Reviewing a branch in a scratch worktree and opening its PR from the main
# checkout is the normal flow; a per-worktree marker would break it.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# Trees stay marked until they age out; a tree hash names an exact content
# state, so a stale entry can only ever match the diff that was reviewed.
readonly KEEP=50

marker="$(git rev-parse --git-common-dir)/ravel-review-ok"
mode="${1:-}"
ref="${2:-HEAD}"

resolve_tree() {
    git rev-parse --verify --quiet "$1^{tree}" || {
        echo "review-gate: cannot resolve '$1' to a tree." >&2
        exit 2
    }
}

case "$mode" in
--mark)
    tree=$(resolve_tree "$ref")
    if [ -f "$marker" ] && grep -qxF "$tree" "$marker"; then
        echo "review-gate: already marked $tree"
        exit 0
    fi
    printf '%s\n' "$tree" >>"$marker"
    # Keep the file bounded; the newest entries are at the end.
    if [ "$(wc -l <"$marker")" -gt "$KEEP" ]; then
        tail -n "$KEEP" "$marker" >"$marker.tmp" && mv "$marker.tmp" "$marker"
    fi
    echo "review-gate: marked $tree"
    ;;
--check)
    tree=$(resolve_tree "$ref")
    if [ ! -f "$marker" ] || ! grep -qxF "$tree" "$marker"; then
        echo "review-gate: ravel-review has not passed for $ref ($tree)." >&2
        echo "Run the ravel-review skill on this diff; it records the marker on PASS." >&2
        exit 1
    fi
    ;;
*)
    echo "usage: review-gate.sh --mark|--check [ref]" >&2
    exit 2
    ;;
esac
