# Async & Background Tasks

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Core Patterns](#core-patterns) · [Common Pitfalls](#common-pitfalls)

## Overview

gpui-ce provides an integrated async runtime for foreground UI updates and background computation.

**Key Concepts:**
- **Foreground tasks**: UI thread, can update entities (`cx.spawn`)
- **Background tasks**: Worker threads, CPU-intensive work (`cx.background_spawn`)
- All entity updates happen on the foreground thread
- gpui-ce uses `AsyncFnOnce` for spawn closures

## Quick Start

### Foreground Tasks (from Context<Self>)

Closure receives `(WeakEntity<Self>, &mut AsyncApp)`:

```rust
impl MyComponent {
    fn fetch_data(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let data = fetch_from_api().await;

            this.update(cx, |state, cx| {
                state.data = Some(data);
                cx.notify();
            }).ok();
        }).detach();
    }
}
```

### Foreground Tasks (from App)

Closure receives `(&mut AsyncApp)`:

```rust
cx.spawn(async move |cx: &mut AsyncApp| {
    // No entity reference
}).detach();
```

### Spawn with Window Context (spawn_in)

```rust
impl MyComponent {
    fn animate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            this.update_in(cx, |state, window, cx| {
                state.frame += 1;
                cx.notify();
            }).ok();
        }).detach();
    }
}
```

### Background Tasks

```rust
impl MyComponent {
    fn process_file(&mut self, cx: &mut Context<Self>) {
        let entity = cx.entity().downgrade();

        cx.background_spawn(async move {
            heavy_computation().await
        })
        .then(cx.spawn(move |result, cx| {
            entity.update(cx, |state, cx| {
                state.result = result;
                cx.notify();
            }).ok();
        }))
        .detach();
    }
}
```

### Task Management

```rust
struct MyView {
    _task: Task<()>,
}

impl MyView {
    fn new(cx: &mut Context<Self>) -> Self {
        let _task = cx.spawn(async move |this, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;

                this.update(cx, |state, cx| {
                    state.tick();
                    cx.notify();
                }).ok();
            }
        });

        Self { _task }
    }
}
```

## Core Patterns

### 1. Async Data Fetching

```rust
cx.spawn(async move |this, cx: &mut AsyncApp| {
    let data = fetch_data().await?;
    this.update(cx, |state, cx| {
        state.data = Some(data);
        cx.notify();
    })?;
    Ok::<_, anyhow::Error>(())
}).detach();
```

### 2. Background + UI Update Chain

```rust
cx.background_spawn(async move { heavy_work() })
    .then(cx.spawn(move |result, cx: &mut AsyncApp| {
        entity.update(cx, |state, cx| {
            state.result = result;
            cx.notify();
        }).ok();
    }))
    .detach();
```

### 3. Periodic Tasks

```rust
cx.spawn(async move |this, cx: &mut AsyncApp| {
    loop {
        cx.background_executor().timer(Duration::from_secs(5)).await;
        this.update(cx, |state, cx| {
            state.tick();
            cx.notify();
        }).ok();
    }
}).detach();
```

### 4. Task Cancellation

Tasks are automatically cancelled when dropped. Store in struct to keep alive:

```rust
struct MyView {
    counting_task: Option<Task<()>>,
}

impl MyView {
    fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.counting_task.is_some() {
            self.counting_task = None; // drops = cancels
        } else {
            self.counting_task = Some(cx.spawn(async move |this, cx| {
                // ...
            }));
        }
    }
}
```

## Common Pitfalls

### ❌ defer_in re-entrancy

```rust
// ❌ Panic: entity is locked for defer_in
cx.defer_in(window, |list_state, window, cx| {
    parent.update(cx, |this, cx| {
        this.inner_list.update(cx, |_, _| {}); // PANIC if inner_list == deferred entity
    });
});

// ✅ Use the direct &mut reference
cx.defer_in(window, |list_state, window, cx| {
    list_state.delegate_mut().some_method();
    parent.update(cx, |this, cx| { /* different entity, safe */ });
});
```

### ❌ Don't update entities from background tasks

```rust
// ❌ Compile error
cx.background_spawn(async move {
    entity.update(cx, |state, cx| { ... }); // No App in background
});

// ✅ Chain with foreground task
cx.background_spawn(async move { data })
    .then(cx.spawn(move |data, cx| {
        entity.update(cx, |state, cx| {
            state.data = data;
            cx.notify();
        }).ok();
    }))
    .detach();
```
