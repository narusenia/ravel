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

normalize_path() { # ripgrep emits backslash separators on Windows
    printf '%s' "${1//\\//}"
}

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
    file=$(normalize_path "$file")
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
    file=$(normalize_path "$file")
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
    file=$(normalize_path "$file")
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
    file=$(normalize_path "$file")
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
    file=$(normalize_path "$file")
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
    file=$(normalize_path "$file")
    ty=$(sed -E 's/.*observe_global::<([A-Za-z0-9_:]+)>.*/\1/' <<<"$content")
    ty=$(last_segment "$ty")
    if ! allowed observe-global "$file" "$ty"; then
        report observe-global "$file" "$line" \
            "new observe_global::<$ty> — prefer EventEmitter/subscriptions; allowlist only with justification (.agents/rules/gpui.md)"
    fi
done < <(rg -n --no-heading 'observe_global::<' crates -g '*.rs' 2>/dev/null)

# ---------------------------------------------------------------------------
# framebuffer-direct-index: FrameBuffer pixels must be read through
# FrameBuffer::as_f32() — direct byte indexing of FrameBuffer.data couples
# callers to the storage format (RgbaF32 / RgbaF16 / Rgba8). A whole-buffer
# byte view for GPU upload (`&fb.data[..]`) is exempt; `.data` fields of
# other types (AudioBuffer, FFmpeg AVFrame) need an allow entry.
# ---------------------------------------------------------------------------
while IFS=: read -r file line content; do
    [ -z "${file:-}" ] && continue
    file=$(normalize_path "$file")
    case "$content" in
        *.data\[..]*) continue ;; # whole-buffer byte view: &fb.data[..]
    esac
    if ! allowed framebuffer-direct-index "$file" "data-index"; then
        report framebuffer-direct-index "$file" "$line" \
            "direct .data[...] indexing — read FrameBuffer pixels through as_f32() (cache-plan unit 1)"
    fi
done < <(rg -n --no-heading '\.data\[' crates -g '*.rs' 2>/dev/null)

# ---------------------------------------------------------------------------
# `ravel_gpu::interop` holds two different concerns, and they have two
# different allowed sets — which is why the rules below are split rather than
# keyed on the module path (GPUBK-9). Matching the module path made "carry a
# backend pointer out" and "accept the toolkit's device at startup" the same
# violation, and they are not: the first pins a caller to one backend, the
# second is the contract REQ-GPU-001 rests on.
#
# gpu-native-handle-escape: the handle vocabulary — `native_device`,
# `native_texture`, `NativeHandle`, `NativeDevice`, `NativeTexture`,
# `NativeGpuContext` — hands out or carries backend-native pointers. It is the
# documented hole in the GPU façade
# (GPUBK-8) and exists for the OpenFX host (REQ-PLUGIN-001) and hardware decode
# (REQ-GPU-001) only. Reaching it from a node processor pins that node to one
# backend and bypasses dispatch batching and the texture pool's lifetime
# bookkeeping. Allowed callers: ravel-gpu itself, ravel-media (hardware decode),
# and the future OFX host crate.
#
# `native_api` / `NativeApi` are deliberately *not* matched: they answer "which
# API is live" out of the adapter description, name no pointer, need no
# `unsafe`, and hand out nothing whose lifetime anyone has to uphold. The
# symbols are matched rather than the module path so that an alias or a
# re-`use` still reads as the escape it is.
#
# **The symbol list is the coverage.** Matching names instead of the module path
# is what lets the two concerns split, but it trades deny-by-default for a
# denylist: a *new* handle-returning item in `interop.rs` is invisible here until
# its name is added below. Adding one to that module obliges you to add it to
# this list (or to the pair below, whichever direction it goes). The public
# surface of `interop.rs` is small and every current item is accounted for —
# keep it that way.
# ---------------------------------------------------------------------------
while IFS=: read -r file line symbol; do
    [ -z "${file:-}" ] && continue
    file=$(normalize_path "$file")
    case "$file" in
        crates/ravel-gpu/* | crates/ravel-media/* | crates/ravel-ofx/*) continue ;;
    esac
    if ! allowed gpu-native-handle-escape "$file" "$symbol"; then
        report gpu-native-handle-escape "$file" "$line" \
            "$symbol outside the GPU/media/OFX crates — backend-native handles are for the OFX host and hardware decode only (GPUBK-8)"
    fi
done < <(rg -no --no-heading \
    -e '\bnative_device\b' \
    -e '\bnative_texture\b' \
    -e '\bNativeHandle\b' \
    -e '\bNativeDevice\b' \
    -e '\bNativeTexture\b' \
    -e '\bNativeGpuContext\b' \
    crates -g '*.rs' 2>/dev/null)

# ---------------------------------------------------------------------------
# gpu-device-sharing: `interop::context_from_wgpu`,
# `interop::context_from_native`, `interop::wgpu_instance`,
# `native_gpu_handles` and `NativeGpuHandles` are the other direction — Ravel
# receives the graphics objects instead of handing them out. REQ-GPU-001
# requires the UI framework and the compute pipeline to run on one device, and
# a shared device is by definition one the host creates and Ravel accepts, so
# this is a contract to keep rather than a hole to close (GPUBK-9). It is still
# not free-for-all: whoever calls it decides which device the whole evaluation
# pipeline runs on, and that is the application host's job alone. Allowed
# callers: ravel-gpu itself and ravel-app, the GPUI host.
#
# Called once at startup, it bypasses neither dispatch batching nor the texture
# pool — every subsystem is built on the context it returns — which is exactly
# why it does not belong to the rule above.
# ---------------------------------------------------------------------------
while IFS=: read -r file line symbol; do
    [ -z "${file:-}" ] && continue
    file=$(normalize_path "$file")
    case "$file" in
        crates/ravel-gpu/* | crates/ravel-app/*) continue ;;
    esac
    if ! allowed gpu-device-sharing "$file" "$symbol"; then
        report gpu-device-sharing "$file" "$line" \
            "$symbol outside ravel-gpu and the GPUI host — the shared device is chosen once, by the application host (GPUBK-9, REQ-GPU-001)"
    fi
done < <(rg -no --no-heading \
    -e '\bcontext_from_wgpu\b' \
    -e '\bcontext_from_native\b' \
    -e '\bnative_gpu_handles\b' \
    -e '\bNativeGpuHandles\b' \
    -e '\bwgpu_instance\b' \
    crates -g '*.rs' 2>/dev/null)

# ---------------------------------------------------------------------------
# gpu-facade-wgpu: no wgpu type in ravel-gpu's public API. The crate exists so
# that replacing the graphics backend does not reach its callers (GPUBK-4), and
# one `pub fn` returning a `wgpu::Device` — or one `pub` field of a wgpu type —
# hands that guarantee back. Describe the work instead (BindingDesc,
# TextureFormat, ComputeDispatch, PooledTexture, AdapterInfo) and convert
# inside the crate.
#
# `interop.rs` is exempt, and the two rules above guard who may reach each half
# of it. For the handle accessors the exemption is a concession (GPUBK-8); for
# the device-sharing entry points it is structural: naming the toolkit's device
# type *is* the job, so the signature has to move when the backend does. That is
# the definition of the interop boundary rather than a leak through it
# (GPUBK-9).
#
# Signatures wrap, so the search is multi-line and bounded by the `{` that ends
# a signature; the results are then narrowed to the lines that actually name a
# wgpu type, which are the ones worth pointing at. `fn` is matched with its
# modifiers (`pub async fn`, `pub const fn`, `pub unsafe fn`) because the crate
# uses all three, and tuple structs and enum bodies are covered too.
#
# Known limits, since this is grep and not rustc: an enum whose body contains a
# struct variant is only scanned up to that variant's closing brace, and a
# `pub trait` method is not matched (the crate has no public traits today). A
# reviewer still has to look; this catches the accidental re-export.
# ---------------------------------------------------------------------------
while IFS=: read -r file line content; do
    [ -z "${file:-}" ] && continue
    file=$(normalize_path "$file")
    if ! allowed gpu-facade-wgpu "$file" "wgpu"; then
        report gpu-facade-wgpu "$file" "$line" \
            "wgpu type in ravel-gpu's public API (${content#"${content%%[![:space:]]*}"}) — state it in the crate's own vocabulary; ravel_gpu::interop is the only exception (GPUBK-4)"
    fi
done < <(rg -nU --no-heading \
    -e 'pub (async |unsafe |const |extern "C" )*fn [^{;]*\bwgpu' \
    -e '^[[:space:]]*pub [a-z_0-9]+: [^,]*\bwgpu' \
    -e '^[[:space:]]*pub (type|const|static|use) [^;{]*\bwgpu' \
    -e 'pub struct [^;{]*\bwgpu' \
    -e 'pub enum [^}]*\bwgpu' \
    crates/ravel-gpu/src -g '*.rs' -g '!interop.rs' 2>/dev/null | rg '\bwgpu')

# ---------------------------------------------------------------------------
# raw-pixel-quantisation: no hand-rolled float → integer pixel conversion.
#
# The pipeline composites in linear light (`CM-2`), so turning a pixel into a
# byte is two steps that have to happen together: encode into the display
# space, then quantise. Every exit — the viewer, the PNG and video writers —
# goes through `ravel_core::color::to_display_rgba8` (or its 16-bit twin) so
# that they agree bit for bit, which is the property `CM-4`'s round-trip
# criteria rest on. A stray `* 255.0` is how the four exits drifted apart
# before `CM-1`, and it is silent: the picture merely looks wrong.
#
# `color.rs` defines the conversion and is exempt. Anything else that really
# wants the file's own values rather than a display of the composite needs a
# justified allow entry.
#
# The scale factor is matched on either side of the `*` and with the spacing
# and trailing zeros optional, because `255.0 * v`, `v*255.0` and `v * 255.`
# are the same mistake written three ways. A bare `as u8` is deliberately not
# matched: most casts in the tree are indices and counts, and a lint that
# cries wolf gets an allow entry rather than a fix. Neither is `* scale` —
# `scale` is what half the geometry code calls a geometric factor.
# ---------------------------------------------------------------------------
while IFS=: read -r file line content; do
    [ -z "${file:-}" ] && continue
    file=$(normalize_path "$file")
    case "$file" in
        crates/ravel-core/src/color.rs) continue ;;
    esac
    if ! allowed raw-pixel-quantisation "$file" "quantise"; then
        report raw-pixel-quantisation "$file" "$line" \
            "hand-rolled pixel quantisation (${content#"${content%%[![:space:]]*}"}) — use ravel_core::color::to_display_rgba8 so every exit agrees (CM-1)"
    fi
done < <(rg -n --no-heading \
    -e '\*[[:space:]]*255(\.[0-9]*)?([^0-9]|$)' \
    -e '[^0-9A-Za-z_.]255(\.[0-9]*)?[[:space:]]*\*' \
    -e '\*[[:space:]]*65535(\.[0-9]*)?([^0-9]|$)' \
    -e '[^0-9A-Za-z_.]65535(\.[0-9]*)?[[:space:]]*\*' \
    -e '\*[[:space:]]*max[[:space:]]+as[[:space:]]+f32' \
    crates -g '*.rs' 2>/dev/null)

if [ "$violations" -gt 0 ]; then
    echo >&2
    echo "lint-patterns: $violations violation(s). Fix them or add a justified entry to $ALLOW_FILE." >&2
    exit 1
fi

echo "lint-patterns: clean"
