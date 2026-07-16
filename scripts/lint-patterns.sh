#!/usr/bin/env bash
# Anti-pattern lint for Ravel.
#
# Mechanically enforces the grep-detectable subset of .agents/rules/gpui.md.
# Context-dependent rules (no focus changes or command dispatch inside
# render(), etc.) are covered by the ravel-review skill instead.
#
# Exceptions live in scripts/lint-patterns.allow as lines of:
#   <rule> <file> <detail>
# Add an entry only with a justification comment above it.

set -uo pipefail
cd "$(git rev-parse --show-toplevel)"

ALLOW_FILE="scripts/lint-patterns.allow"
violations=0

allowed() { # $1 rule, $2 file, $3 detail
    [ -f "$ALLOW_FILE" ] && grep -qE "^$1[[:space:]]+$2[[:space:]]+$3([[:space:]]|$)" "$ALLOW_FILE"
}

report() { # $1 rule, $2 file, $3 line, $4 message
    printf 'lint-patterns: [%s] %s:%s\n    %s\n' "$1" "$2" "$3" "$4" >&2
    violations=$((violations + 1))
}

last_segment() { # strip a `path::to::Type` down to `Type`
    sed -E 's/.*:://' <<<"$1"
}

# ---------------------------------------------------------------------------
# global-option-event: one-shot events must not be Global<Option<...>>.
# Commands go through GPUI Actions; component events through EventEmitter.
# Durable state where Option is a real domain value needs an allow entry.
# ---------------------------------------------------------------------------
while IFS=: read -r file line content; do
    [ -z "${file:-}" ] && continue
    name=$(sed -E 's/.*pub struct ([A-Za-z0-9_]+)\(pub Option<.*/\1/' <<<"$content")
    if rg -q "impl ([A-Za-z0-9_]+::)*Global for $name" "$file" && ! allowed global-option-event "$file" "$name"; then
        report global-option-event "$file" "$line" \
            "$name is a Global<Option<...>> — one-shot signals must use Actions or EventEmitter (.agents/rules/gpui.md)"
    fi
done < <(rg -n --no-heading 'pub struct [A-Za-z0-9_]+\(pub Option<' crates -g '*.rs' 2>/dev/null)

# ---------------------------------------------------------------------------
# raw-key-command: keystroke modifier checks outside the keybinding layer.
# Operations that belong to the command system must be Actions bound through
# build_keybindings / the keybinding TOML, never ad-hoc modifier matching.
# ---------------------------------------------------------------------------
while IFS=: read -r file line content; do
    [ -z "${file:-}" ] && continue
    case "$file" in
        crates/*/tests/*) continue ;;
    esac
    if ! allowed raw-key-command "$file" "keystroke"; then
        report raw-key-command "$file" "$line" \
            "raw keystroke modifier check — route this through a GPUI Action and the keybinding table (.agents/rules/gpui.md)"
    fi
done < <(rg -n --no-heading 'keystroke\.modifiers\.(platform|control|secondary)' crates -g '*.rs' 2>/dev/null)

# ---------------------------------------------------------------------------
# panel-on-key-down: raw key handlers in panels bypass the command system.
# Only genuinely low-level input (text entry, transient drag modes) may use
# on_key_down, and each use needs an allow entry with justification.
# ---------------------------------------------------------------------------
while IFS=: read -r file line _content; do
    [ -z "${file:-}" ] && continue
    if ! allowed panel-on-key-down "$file" "on_key_down"; then
        report panel-on-key-down "$file" "$line" \
            "raw on_key_down in a panel — panel operations must be key-context-scoped Actions (.agents/rules/gpui.md)"
    fi
done < <(rg -n --no-heading '\.on_key_down\(' crates/ravel-app/src/panels -g '*.rs' 2>/dev/null)

# ---------------------------------------------------------------------------
# actions-outside-table: GPUI actions are declared once, from the
# for_each_command! table in workspace.rs. A second actions! site reintroduces
# the Command/Action mapping drift the table exists to prevent.
# ---------------------------------------------------------------------------
while IFS=: read -r file line _content; do
    [ -z "${file:-}" ] && continue
    [ "$file" = "crates/ravel-app/src/workspace.rs" ] && continue
    if ! allowed actions-outside-table "$file" "actions"; then
        report actions-outside-table "$file" "$line" \
            "actions! outside workspace.rs — add commands to CommandId + for_each_command! instead"
    fi
done < <(rg -n --no-heading 'actions!\(' crates -g '*.rs' 2>/dev/null)

# ---------------------------------------------------------------------------
# direct-handle-command: RavelWorkspace::dispatch_command is the single
# execution entry point in the GPUI host. Calling AppShell::handle_command
# from anywhere else creates a second dispatch path.
# ---------------------------------------------------------------------------
while IFS=: read -r file line _content; do
    [ -z "${file:-}" ] && continue
    [ "$file" = "crates/ravel-app/src/workspace.rs" ] && continue
    case "$file" in
        crates/*/tests/*) continue ;;
    esac
    if ! allowed direct-handle-command "$file" "handle_command"; then
        report direct-handle-command "$file" "$line" \
            "handle_command outside dispatch_command — commands must flow through the single dispatcher"
    fi
done < <(rg -n --no-heading '\.handle_command\(' crates/ravel-app/src -g '*.rs' 2>/dev/null)

# ---------------------------------------------------------------------------
# observe-global: Global observers are the legacy cross-panel signal path
# (Phase 5 of the command/focus refactor removes the remaining ones). New
# subscriptions need an allow entry and a reason; prefer EventEmitter.
# ---------------------------------------------------------------------------
while IFS=: read -r file line content; do
    [ -z "${file:-}" ] && continue
    ty=$(sed -E 's/.*observe_global::<([A-Za-z0-9_:]+)>.*/\1/' <<<"$content")
    ty=$(last_segment "$ty")
    if ! allowed observe-global "$file" "$ty"; then
        report observe-global "$file" "$line" \
            "new observe_global::<$ty> — prefer EventEmitter/subscriptions; allowlist only with justification (.agents/rules/gpui.md)"
    fi
done < <(rg -n --no-heading 'observe_global::<' crates -g '*.rs' 2>/dev/null)

if [ "$violations" -gt 0 ]; then
    echo >&2
    echo "lint-patterns: $violations violation(s). Fix them or add a justified entry to $ALLOW_FILE." >&2
    exit 1
fi

echo "lint-patterns: clean"
