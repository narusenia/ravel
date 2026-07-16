# Entity State Management

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Core Principles](#core-principles) · [Common Use Cases](#common-use-cases)

## Overview

An `Entity<T>` is a handle to state of type `T`, providing safe access and updates.

**Key Methods:**
- `entity.read(cx)` → `&T` - Read-only access
- `entity.read_with(cx, |state, cx| ...)` → `R` - Read with closure
- `entity.update(cx, |state, cx| ...)` → `R` - Mutable update
- `entity.downgrade()` → `WeakEntity<T>` - Create weak reference
- `entity.entity_id()` → `EntityId` - Unique identifier

**Entity Types:**
- **`Entity<T>`**: Strong reference (increases ref count)
- **`WeakEntity<T>`**: Weak reference (doesn't prevent cleanup, returns `Result`)

## Quick Start

### Creating and Using Entities

```rust
let counter = cx.new(|cx| Counter { count: 0 });

let count = counter.read(cx).count;

counter.update(cx, |state, cx| {
    state.count += 1;
    cx.notify();
});

let weak = counter.downgrade();
let _ = weak.update(cx, |state, cx| {
    state.count += 1;
    cx.notify();
});
```

### In Components

```rust
struct MyComponent {
    shared_state: Entity<SharedData>,
}

impl MyComponent {
    fn new(cx: &mut App) -> Entity<Self> {
        let shared = cx.new(|_| SharedData::default());
        cx.new(|cx| Self { shared_state: shared })
    }

    fn update_shared(&mut self, cx: &mut Context<Self>) {
        self.shared_state.update(cx, |state, cx| {
            state.value = 42;
            cx.notify();
        });
    }
}
```

### Async Operations

When calling `cx.spawn` from `Context<Self>`, the closure receives `(WeakEntity<Self>, &mut AsyncApp)`:

```rust
impl MyComponent {
    fn fetch_data(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let data = fetch_from_api().await;
            let _ = this.update(cx, |state, cx| {
                state.data = Some(data);
                cx.notify();
            });
        }).detach();
    }
}
```

## Core Principles

### Always Use Weak References in Closures

```rust
// ✅ Good: Weak reference prevents retain cycles
let weak = cx.entity().downgrade();
callback(move || {
    let _ = weak.update(cx, |state, cx| cx.notify());
});

// ❌ Bad: Strong reference may cause memory leak
let strong = cx.entity();
callback(move || {
    strong.update(cx, |state, cx| cx.notify());
});
```

### Use Inner Context

```rust
// ✅ Good
entity.update(cx, |state, inner_cx| {
    inner_cx.notify();
});

// ❌ Bad: multiple borrow
entity.update(cx, |state, inner_cx| {
    cx.notify(); // Wrong!
});
```

### Avoid Nested Entity Updates

**Same entity → always panics.**

```rust
// ❌ Panic
entity_a.update(cx, |state, cx| {
    entity_a.update(cx, |_, _| {}); // same lock
});
```

**Different entity → generally safe, but cycles panic.**

```rust
// ✅ Usually fine
entity_a.update(cx, |_, cx| {
    entity_b.update(cx, |_, _| {}); // different lock
});

// ❌ Panic: indirect cycle
entity_a.update(cx, |_, cx| {
    entity_b.update(cx, |_, cx| {
        entity_a.update(cx, |_, _| {}); // entity_a still locked
    });
});
```

### defer_in Does Not Bypass the Lock

```rust
// ❌ Panic
cx.defer_in(window, |list_state, window, cx| {
    parent.update(cx, |this, cx| {
        this.list.update(cx, |_, _| {}); // list is already locked above
    });
});

// ✅ Fix: use the direct &mut reference
cx.defer_in(window, |list_state, window, cx| {
    list_state.delegate_mut().some_hook();
    parent.update(cx, |this, cx| { /* different entity, safe */ });
});
```

### Snapshot Pattern for Render Callbacks

```rust
// ❌ Panic in render_item — entity is already locked
fn render_item(&mut self, ix: IndexPath, ...) -> ... {
    let checked = parent_entity.read(cx).selection.contains(&ix); // PANIC
}

// ✅ Read from a snapshot field
fn render_item(&mut self, ix: IndexPath, ...) -> ... {
    let checked = self.selection_snapshot.contains(&ix);
}
```

## Common Use Cases

1. **Component State**: Internal state that needs reactivity
2. **Shared State**: State shared between multiple components
3. **Parent-Child**: Coordinating via weak refs
4. **Async State**: State updated from async operations
5. **Observations**: Reacting to changes in other entities
