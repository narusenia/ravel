# Focus & Keyboard Navigation

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Focus Events](#focus-events) · [Keyboard Navigation](#keyboard-navigation) · [Common Patterns](#common-patterns)

## Overview

gpui-ce's focus system enables keyboard navigation and focus management.

- **FocusHandle**: Reference to a focusable element
- **Focusable trait**: `fn focus_handle(&self, cx: &App) -> FocusHandle`
- **Focus tracking**: `.track_focus(&handle)` on elements
- **Tab navigation**: Automatic from render order

## Quick Start

### Creating Focus Handles

```rust
struct FocusableComponent {
    focus_handle: FocusHandle,
}

impl FocusableComponent {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}
```

### Making Elements Focusable

```rust
impl Render for FocusableComponent {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_enter))
            .child("Focusable content")
    }
}
```

### Focus Management

```rust
self.focus_handle.focus(cx);           // set focus
self.focus_handle.is_focused(cx);      // check focus
cx.blur();                             // remove focus
```

## Focus Events

```rust
impl Render for MyInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_focused = self.focus_handle.is_focused(cx);

        div()
            .track_focus(&self.focus_handle)
            .on_focus(cx.listener(|this, _event, cx| {
                this.on_focus(cx);
            }))
            .on_blur(cx.listener(|this, _event, cx| {
                this.on_blur(cx);
            }))
            .when(is_focused, |el| el.bg(cx.theme().focused_background))
    }
}
```

## Keyboard Navigation

Elements with `track_focus()` automatically participate in Tab navigation:

```rust
div()
    .child(input1.track_focus(&focus1))  // Tab order: 1
    .child(input2.track_focus(&focus2))  // Tab order: 2
    .child(input3.track_focus(&focus3))  // Tab order: 3
```

## Common Patterns

### Auto-focus on Mount

```rust
impl MyDialog {
    fn new(cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(cx);
        Self { focus_handle }
    }
}
```

### Focus Trap (Modal)

```rust
div()
    .track_focus(&self.focus_handle)
    .on_key_down(cx.listener(|this, event: &KeyDownEvent, cx| {
        if event.key == Key::Tab {
            this.focus_next_in_modal(cx);
            cx.stop_propagation();
        }
    }))
```

### Visual Focus Indicator

```rust
let is_focused = self.focus_handle.is_focused(cx);
div().when(is_focused, |el| el.border_color(cx.theme().focused_border))
```
