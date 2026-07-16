#!/usr/bin/env bash
# Claude Code PostToolUse hook: run the anti-pattern lint after Edit/Write
# touches Rust code under crates/. Exit 2 feeds violations back to the agent.

set -uo pipefail
input=$(cat)

case "$input" in
*crates/*.rs*) ;;
*) exit 0 ;;
esac

cd "$(git rev-parse --show-toplevel)" || exit 0
out=$(bash scripts/lint-patterns.sh 2>&1)
status=$?
if [ "$status" -ne 0 ]; then
    echo "$out" >&2
    exit 2
fi
exit 0
