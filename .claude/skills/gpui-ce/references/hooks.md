# Hooks (use_state / use_keyed_state)

**Contents:** [Overview](#overview) · [use_state](#use_state) · [use_keyed_state](#use_keyed_state) · [Patterns](#patterns) · [Best Practices](#best-practices)

## Overview

gpui-ce introduces React-like hooks for element-scoped state. These are methods on `Window`, not on contexts.

**Two variants:**
- **`window.use_state(cx, init)`** — automatic key via `#[track_caller]` (caller location)
- **`window.use_keyed_state(key, cx, init)`** — explicit key (required in loops/lists)

Both return `Entity<S>` that persists across renders as long as the element is rendered in consecutive frames.

## use_state

```rust
#[track_caller]
pub fn use_state<S: 'static>(
    &mut self,          // &mut Window
    cx: &mut App,
    init: impl FnOnce(&mut Self, &mut Context<S>) -> S,
) -> Entity<S>
```

Uses caller location to generate an `ElementId` automatically. The init closure runs only on first render.

### Basic Usage

```rust
fn my_counter(window: &mut Window, cx: &mut App) -> impl IntoElement {
    let state = window.use_state(cx, |_window, _cx| CounterState { count: 0 });
    let count = state.read(cx).count;

    div()
        .id("counter")
        .child(format!("Count: {}", count))
        .child(
            div()
                .id("increment")
                .child("+")
                .on_click(move |_, _, cx| {
                    state.update(cx, |s, cx| {
                        s.count += 1;
                        cx.notify();
                    });
                }),
        )
}
```

### In Render Trait

```rust
impl Render for MyView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hover_state = window.use_state(cx, |_, _| HoverState { hovered: false });
        let is_hovered = hover_state.read(cx).hovered;

        div()
            .id("main")
            .when(is_hovered, |el| el.bg(rgb(0x333333)))
            .child("Content")
    }
}
```

## use_keyed_state

```rust
pub fn use_keyed_state<S: 'static>(
    &mut self,          // &mut Window
    key: impl Into<ElementId>,
    cx: &mut App,
    init: impl FnOnce(&mut Self, &mut Context<S>) -> S,
) -> Entity<S>
```

Explicit key required when `use_state` would create ambiguity — loops, conditionally rendered elements, list items.

### In Lists

```rust
impl Render for TodoList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().children(self.items.iter().enumerate().map(|(i, item)| {
            let editing = window.use_keyed_state(
                ("todo-edit", i),
                cx,
                |_, _| EditState { active: false },
            );

            div()
                .id(("todo", i))
                .child(&item.text)
                .on_click(move |_, _, cx| {
                    editing.update(cx, |s, cx| {
                        s.active = !s.active;
                        cx.notify();
                    });
                })
        }))
    }
}
```

## Patterns

### State with Automatic Observer

`use_keyed_state` automatically observes the returned entity — when the entity is notified, the owning view re-renders. No manual `cx.observe()` needed.

### Combining with Transitions

```rust
fn animated_panel(window: &mut Window, cx: &mut App) -> impl IntoElement {
    let expanded = window.use_state(cx, |_, _| PanelState { open: false });
    let is_open = expanded.read(cx).open;

    let height = window
        .use_keyed_transition("panel-height", cx, Duration::from_millis(200), |_, _| 0.0_f32)
        .with_easing(ease_in_out);

    if is_open {
        height.update(cx, |v, cx| { *v = 1.0; cx.notify(); });
    }

    div().id("panel").h(px(*height.evaluate(window, cx) * 200.0))
}
```

## Best Practices

### ✅ Use `use_state` for Simple Element State

```rust
// ✅ Good: simple toggle in a function component
fn toggle_button(window: &mut Window, cx: &mut App) -> impl IntoElement {
    let state = window.use_state(cx, |_, _| ToggleState { on: false });
    // ...
}
```

### ✅ Use `use_keyed_state` in Loops

```rust
// ✅ Good: explicit key prevents state collision
for (i, item) in items.iter().enumerate() {
    let state = window.use_keyed_state(("item", i), cx, |_, _| ItemState::default());
}
```

### ❌ Don't Use `use_state` in Loops

```rust
// ❌ Bad: #[track_caller] produces same location for every iteration
for item in &items {
    let state = window.use_state(cx, |_, _| ItemState::default());
    // All iterations share the same state!
}
```

### When to Use Hooks vs Entity-backed Render

| Scenario | Approach |
|----------|----------|
| Simple element-local toggle/counter | `use_state` |
| List item state | `use_keyed_state` |
| Complex component with subscriptions | `Render` + `Entity<T>` |
| Stateless presentational component | `RenderOnce` |
| State shared between components | `Entity<T>` passed as prop |
