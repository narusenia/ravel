---
name: gpui-ce
description: GPUI-CE (community edition) framework knowledge. Covers everything in the gpui skill PLUS gpui-ce exclusive APIs — use_state/use_keyed_state hooks, use_transition/use_keyed_transition animations, use_asset async loading, Lerp trait, gpui_platform bootstrapping, and the Render/RenderOnce split. Use when working with gpui-ce applications, the ravel project, or any project that depends on gpui-ce instead of Zed's gpui.
---

## gpui-ce vs gpui (Zed)

gpui-ce is the community edition fork. Key differences from Zed's gpui:

1. **Bootstrap**: `gpui_platform::application().run(|cx| ...)` — not `Application::new()`
2. **Hook APIs**: `window.use_state()`, `window.use_keyed_state()` for element-scoped state
3. **Transition hooks**: `window.use_transition()`, `window.use_keyed_transition()` for animated values
4. **Asset hooks**: `window.use_asset()` for async asset loading
5. **Spawn signature**: `AsyncFnOnce` instead of closures — `cx.spawn(async move |this, cx| { ... })`
6. **`on_click` signature**: `|event, window, cx|` (3 args) — window is the second param
7. **Rendering backend**: wgpu unified (Metal on macOS via gpui_macos, wgpu on Linux/Windows)
8. **`gpui_platform` crate**: platform abstraction layer with `font-kit` feature required for text

## Navigation

Load the relevant reference file based on the task:

| Topic | File | When to load |
|-------|------|--------------|
| **Hooks (use_state)** | [hooks.md](references/hooks.md) | `use_state`, `use_keyed_state`, element-scoped state |
| **Transitions** | [transition.md](references/transition.md) | `use_transition`, `use_keyed_transition`, `Lerp`, animation interpolation |
| **Assets** | [asset.md](references/asset.md) | `use_asset`, `Asset` trait, async loading |
| **Component patterns** | [component.md](references/component.md) | `Render`, `RenderOnce`, `use_state` — choosing the right approach |
| Context management | [context.md](references/context.md) | `App`, `Window`, `Context<T>`, `AsyncApp` |
| Entity state | [entity.md](references/entity.md) | `Entity<T>`, `WeakEntity`, state management |
| Async & background tasks | [async.md](references/async.md) | `cx.spawn`, `background_spawn`, `Task`, async I/O |
| Events & subscriptions | [event.md](references/event.md) | `cx.emit`, `cx.subscribe`, `cx.observe` |
| Actions & keybindings | [action.md](references/action.md) | `actions!`, `bind_keys`, `on_action`, `key_context` |
| Focus & keyboard nav | [focus-handle.md](references/focus-handle.md) | `FocusHandle`, `track_focus`, Tab navigation |
| Global state | [global.md](references/global.md) | `Global` trait, `cx.set_global`, app-wide config |
| Layout & styling | [layout-style.md](references/layout-style.md) | `div()`, `h_flex()`, `v_flex()`, flexbox, overflow, positioning |
| ElementId | [element-id.md](references/element-id.md) | `ElementId`, `.id()`, uniqueness rules, stateful elements |
| Custom elements (low-level) | [element.md](references/element.md) | `Element` trait, `request_layout`, `prepaint`, `paint` |
| Animation | [animation.md](references/animation.md) | `with_animation`, `Animation`, `Transformation`, easing functions |
| Testing | [test.md](references/test.md) | `#[gpui::test]`, `TestAppContext`, `VisualTestContext` |
| Bootstrap / App setup | [bootstrap.md](references/bootstrap.md) | `gpui_platform::application()`, window creation, `Root` |
