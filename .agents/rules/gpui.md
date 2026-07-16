---
paths:
  - "crates/ravel-app/**/*.rs"
  - "crates/ravel-ui/**/*.rs"
  - "assets/keybindings/**/*.toml"
---

# GPUI rules

- Read `docs/gpui-ui-guide.md` before adding a panel or custom component.
- Use the `gpui-ce` skill for GPUI-CE APIs and the `gpui-component` skill when
  selecting or integrating gpui-component widgets.
- Bootstrap through `gpui_platform::application()` and wrap every window root
  with `gpui_component::Root`.
- Use GPUI Actions for commands and shortcuts. Do not add raw modifier/key checks
  for operations that belong in the command system.
- Keep `Render::render()` free of command dispatch, focus changes, and unrelated
  state mutation.
- Let a child editor or input retain focus until an explicit user action moves
  it. Never unconditionally return focus to a workspace during render.
- Do not introduce `Global<Option<Event>>` for one-shot events. Prefer Actions
  for commands and `EventEmitter` with subscriptions for component events.
- Use Globals only for durable shared application state.
- Call `cx.notify()` after changes that affect rendering and retain
  `Subscription`s for the lifetime of their observers.
- Avoid nested updates of the same Entity. Inside update closures, use the inner
  context passed to the closure.
- Add GPUI integration tests only for behavior that depends on actual focus,
  Action propagation, input routing, or rendering.

The grep-detectable subset of these rules is enforced by
`scripts/lint-patterns.sh` (run via `mise run lint:patterns`, pre-commit, and
CI). Exceptions require a justified entry in `scripts/lint-patterns.allow`.
The invariants below are context-dependent; the `ravel-review` skill checks
them before every pull request.

## Command path invariants

There is exactly one path from user input to command execution:

```text
KeyBinding / Menu / Button → GPUI Action → nearest focused on_action handler
                                          └─ unhandled → RavelWorkspace
                                                         dispatch_command()
```

- Add a command by extending `CommandId` (ravel-ui) and the single
  `for_each_command!` table in `crates/ravel-app/src/workspace.rs`. Never
  declare `actions!` anywhere else and never maintain a second
  Command↔Action list — the table's exhaustive `match`es make a missing
  entry a compile error.
- `RavelWorkspace::dispatch_command()` is the only place that calls
  `AppShell::handle_command` in the GPUI host. Do not queue commands in
  Globals and do not process them in `render()`.

```rust
// WRONG: a queued one-shot command global, drained during render.
pub struct PendingCommand(pub Option<CommandId>);   // lost on overwrite,
impl Global for PendingCommand {}                   // double/zero execution

// RIGHT: panel-local handling of the shared Action, nearest handler wins.
div().key_context(KEY_CONTEXT)
    .on_action(cx.listener(Self::on_duplicate))     // EditDuplicate
```

- Panel-specific shortcuts are key-context-scoped `KeyBinding`s (see
  `NodeEditor`), not raw `on_key_down` modifier checks. Raw key handling is
  reserved for genuinely low-level input (text entry, transient drag modes).

## Global usage taxonomy

Decide the mechanism from what the data is:

- Durable shared state (current selection, focused panel, window registry):
  `Global` is fine. `Option` may be part of the domain ("no panel focused").
- Component events (value changed, selection changed): `EventEmitter` +
  `Subscription`. Never park an event in a Global for another entity to
  observe — it re-fires on unrelated re-renders and drops coalesced events.
- Commands (undo, copy, delete): GPUI Actions dispatched through the focus
  hierarchy.

```rust
// WRONG: one-shot event parked in a Global (the pre-refactor PanelUndoRedo).
cx.set_global(PanelUndoRedo(Some(UndoRedoSignal::Undo)));

// RIGHT: the action reaches the focused editor directly.
.on_action(cx.listener(Self::on_undo))              // EditUndo
```

## Focus ownership

- A panel's focus state follows real GPUI focus events. Subscribe with
  `track_panel_focus()` (`crates/ravel-app/src/panels/mod.rs`) so
  `FocusedPanelGlobal` tracks `on_focus_in` / `on_focus_out`.
- Click-to-focus comes from `track_focus(&handle)` — do not grab focus in
  `on_mouse_down` and do not write `FocusedPanelGlobal` from click handlers.
- Nothing changes focus during `render()`. The workspace takes focus once at
  startup.

```rust
// WRONG: focus by click history, stolen back every frame.
.on_mouse_down(MouseButton::Left, move |_e, window, cx| {
    focus.focus(window, cx);
    cx.set_global(FocusedPanelGlobal(Some(kind)));
})

// RIGHT: declare focusability; sync state from focus events.
let subs = track_panel_focus(kind, &focus_handle, window, cx);
div().track_focus(&focus_handle)
```
