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
