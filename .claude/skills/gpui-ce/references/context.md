# Context Management

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Common Operations](#common-operations) · [Context Hierarchy](#context-hierarchy) · [cx.listener](#cxlistener--binding-callbacks-to-self) · [subscribe_in](#subscribe_in--subscribe-with-window-access) · [observe_window_activation](#observe_window_activation) · [observe_global](#observe_global) · [defer / defer_in](#defer-and-defer_in) · [Naming Convention](#context-naming-convention)

## Overview

gpui-ce uses different context types for different scenarios:

**Context Types:**
- **`App`**: Global app state, entity creation
- **`Window`**: Window-specific operations, painting, layout, hooks
- **`Context<T>`**: Entity-specific context for component `T`
- **`AsyncApp`**: Async context for foreground tasks
- **`AsyncWindowContext`**: Async context with window access

## Quick Start

### Context<T> - Component Context

```rust
impl MyComponent {
    fn update_state(&mut self, cx: &mut Context<Self>) {
        self.value = 42;
        cx.notify();

        cx.spawn(async move |this, cx| {
            // Async work — this is WeakEntity<Self>
        }).detach();

        let entity = cx.entity();
    }
}
```

### App - Global Context

```rust
fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let entity = cx.new(|cx| MyState::default());

        cx.open_window(WindowOptions::default(), |window, cx| {
            cx.new(|cx| Root::new(view, window, cx))
        });
    });
}
```

### Window - Window Context

```rust
impl Render for MyView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_focused = window.is_window_focused();
        let bounds = window.bounds();

        // Hook APIs are on Window
        let state = window.use_state(cx, |_, _| MyState::default());

        div().child("Content")
    }
}
```

### AsyncApp - Async Context

```rust
cx.spawn(async move |cx: &mut AsyncApp| {
    let data = fetch_data().await;

    entity.update(cx, |state, inner_cx| {
        state.data = data;
        inner_cx.notify();
    }).ok();
}).detach();
```

## Common Operations

### Entity Operations

```rust
let entity = cx.new(|cx| MyState::default());

entity.update(cx, |state, cx| {
    state.value = 42;
    cx.notify();
});

let value = entity.read(cx).value;
```

### Notifications and Events

```rust
cx.notify();
cx.emit(MyEvent::Updated);

cx.observe(&entity, |this, observed, cx| {
    // React to changes
}).detach();

cx.subscribe(&entity, |this, source, event, cx| {
    // Handle event
}).detach();
```

### Window Operations

```rust
let focused = window.is_window_focused();
let bounds = window.bounds();
let scale = window.scale_factor();

window.remove_window();
```

### Async Operations

```rust
// From Context<T> — closure receives (WeakEntity<T>, &mut AsyncApp)
cx.spawn(async move |this, cx| {
    // ...
}).detach();

// From App — closure receives (&mut AsyncApp)
cx.spawn(async move |cx: &mut AsyncApp| {
    // ...
}).detach();

// Background thread
cx.background_spawn(async move {
    // Heavy computation
}).detach();
```

## Context Hierarchy

```
App (Global)
  └─ Window (Per-window, hosts hooks)
       └─ Context<T> (Per-component)
            └─ AsyncApp (In async tasks)
                 └─ AsyncWindowContext (Async + Window)
```

## cx.listener — Binding Callbacks to Self

`cx.listener` creates a callback that borrows `&mut self`. Use for `on_click`, `on_action`, etc:

```rust
impl Render for MyView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .on_action(cx.listener(Self::on_save))
            .child(
                Button::new("btn")
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.count += 1;
                        cx.notify();
                    }))
            )
    }
}

impl MyView {
    fn on_save(&mut self, _: &Save, _window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
    }
}
```

## subscribe_in — Subscribe with Window Access

```rust
let _subscription = cx.subscribe_in(&input, window, |this, state, event, window, cx| {
    match event {
        InputEvent::Change => {
            let val = state.read(cx).value();
            this.on_input_change(val, window, cx);
        }
        _ => {}
    }
});
```

`subscribe` vs `subscribe_in`:
- `subscribe(&entity, |this, source, event, cx|)` — no window
- `subscribe_in(&entity, window, |this, source, event, window, cx|)` — window access

## observe_window_activation

```rust
let _sub = cx.observe_window_activation(window, |this, window, cx| {
    if window.is_window_active() {
        this.resume(cx);
    } else {
        this.pause(cx);
    }
});
```

## observe_global

```rust
cx.observe_global::<Theme>(|cx| {
    cx.notify();
});
```

## defer and defer_in

```rust
// defer: runs after current App update
cx.defer(|cx| {
    // No window access
});

// defer_in: runs after update, with window access
cx.defer_in(window, |this, window, cx| {
    // CAUTION: never call entity.update(cx) on *this same entity*
    this.some_method(window, cx);
});
```

## Context Naming Convention

Always name contexts `cx`:

```rust
fn new(window: &mut Window, cx: &mut App) {}
impl Render for View {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) {}
}
cx.spawn(async move |this, cx: &mut AsyncApp| {})
```
