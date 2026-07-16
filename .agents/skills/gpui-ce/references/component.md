# Component Patterns

**Contents:** [Overview](#overview) · [Three Approaches](#three-approaches) · [Render (Entity-backed)](#1-render-entity-backed) · [RenderOnce (Stateless)](#2-renderonce-stateless) · [use_state (Hook)](#3-use_state-hook) · [Choosing the Right Approach](#choosing-the-right-approach)

## Overview

gpui-ce provides three ways to create components. Each has distinct trade-offs:

| Approach | State | Identity | Use When |
|----------|-------|----------|----------|
| `Render` | Internal, persistent | `Entity<T>` | Complex components with subscriptions, tasks, events |
| `RenderOnce` | None (props only) | Consumed on render | Presentational components, lightweight UI |
| `use_state` | Element-scoped | Caller location / key | Simple stateful elements without full entity overhead |

## Three Approaches

### 1. Render (Entity-backed)

Full control over internal state. Can subscribe to events, spawn tasks, and be passed around as `Entity<T>`.

```rust
struct Counter {
    count: i32,
}

impl Counter {
    fn new() -> Self {
        Self { count: 0 }
    }

    fn increment(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.count += 1;
        cx.notify();
    }
}

impl Render for Counter {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("counter")
            .child(format!("{}", self.count))
            .child(
                div()
                    .id("inc")
                    .child("+")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.increment(window, cx);
                    })),
            )
    }
}

// Creation
let counter: Entity<Counter> = cx.new(|_| Counter::new());

// Usage in parent render
div().child(self.counter.clone())
```

### 2. RenderOnce (Stateless)

Consumed when rendered. Receives data as props, delegates events to parent via callbacks.

```rust
#[derive(IntoElement)]
struct Badge {
    label: String,
    on_click: Option<Box<dyn Fn(&mut Window, &mut App) + 'static>>,
}

impl Badge {
    fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), on_click: None }
    }

    fn on_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
}

impl RenderOnce for Badge {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .id("badge")
            .px_2()
            .rounded_full()
            .bg(rgb(0x2196F3))
            .child(self.label)
            .when_some(self.on_click, |el, cb| {
                el.on_click(move |_, window, cx| cb(window, cx))
            })
    }
}

// Usage
Badge::new("New").on_click(|_, cx| { /* ... */ })
```

### 3. use_state (Hook)

Element-scoped state via `window.use_state()`. Works in free functions or inside `Render::render`.

```rust
struct ToggleState { on: bool }

fn toggle_button(window: &mut Window, cx: &mut App) -> impl IntoElement {
    let state = window.use_state(cx, |_, _| ToggleState { on: false });
    let is_on = state.read(cx).on;

    div()
        .id("toggle")
        .bg(if is_on { rgb(0x4CAF50) } else { rgb(0x666666) })
        .child(if is_on { "ON" } else { "OFF" })
        .on_click(move |_, _, cx| {
            state.update(cx, |s, cx| {
                s.on = !s.on;
                cx.notify();
            });
        })
}

// Usage in parent render
div().child(toggle_button(window, cx))
```

## Choosing the Right Approach

```
Need subscriptions/events/tasks? → Render
  ↓ No
Need internal state? → use_state (simple) or Render (complex)
  ↓ No
Presentational only? → RenderOnce
```

**Render** when:
- Component subscribes to events from other entities
- Component spawns async tasks
- Component needs to be passed around as `Entity<T>`
- Complex lifecycle (init, teardown)

**RenderOnce** when:
- Pure presentational (badge, card, label)
- All data comes from props
- No internal state needed
- Builder pattern for configuration

**use_state** when:
- Simple toggle, counter, hover state
- State doesn't need to be shared with other components
- Avoid entity overhead for trivial state
- Free function components
