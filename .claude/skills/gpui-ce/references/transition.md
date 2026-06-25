# Transitions (use_transition / use_keyed_transition)

**Contents:** [Overview](#overview) · [use_keyed_transition](#use_keyed_transition) · [use_transition](#use_transition) · [Transition API](#transition-api) · [Lerp Trait](#lerp-trait) · [Easing Functions](#easing-functions) · [Patterns](#patterns)

## Overview

Transitions provide interpolated animation between values. They are hook-like methods on `Window` that return a `Transition<T>` handle.

**Two variants:**
- **`window.use_keyed_transition(key, cx, duration, init)`** — persistent state with explicit key (recommended)
- **`window.use_transition(cx, duration, init)`** — ephemeral, recreated each render

The animated type `T` must implement `Lerp + Clone + PartialEq + 'static`.

## use_keyed_transition

```rust
pub fn use_keyed_transition<T: Lerp + Clone + PartialEq + 'static>(
    &mut self,          // &mut Window
    key: impl Into<ElementId>,
    cx: &mut App,
    duration: Duration,
    init: impl Fn(&mut Window, &mut Context<TransitionState<T>>) -> T,
) -> Transition<T>
```

Persistent state across renders. **Recommended for most animations.**

### Hover Animation

```rust
impl RenderOnce for MyButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let hover = window
            .use_keyed_transition(
                (self.id.clone(), "hover"),
                cx,
                Duration::from_millis(300),
                |_window, _cx| 0.0_f32,
            )
            .with_easing(ease_in_out);

        let base = rgb(0x663399);
        let bg = base.lerp(&rgb(0x000), *hover.evaluate(window, cx) * 0.3);

        div()
            .id(self.id)
            .bg(bg)
            .on_hover(move |is_hovered, _window, cx| {
                hover.update(cx, |v, cx| {
                    *v = *is_hovered as u8 as f32;
                    cx.notify();
                });
            })
            .child(self.label)
    }
}
```

## use_transition

```rust
#[track_caller]
pub fn use_transition<T: Lerp + Clone + PartialEq + 'static>(
    &mut self,          // &mut Window
    cx: &mut App,
    duration: Duration,
    init: impl Fn(&mut Window, &mut Context<TransitionState<T>>) -> T,
) -> Transition<T>
```

State recreated each render. Use for ephemeral transitions that don't need persistence.

## Transition API

### Core Methods

```rust
// Evaluate current interpolated value (cached per frame)
let value: Ref<'_, T> = transition.evaluate(window, cx);

// Get raw progress delta (0.0..1.0 after easing)
let delta: f32 = transition.evaluate_delta(cx);

// Update the goal value — returns true if goal changed
let changed: bool = transition.update(cx, |current_goal, cx| {
    *current_goal = new_value;
    cx.notify();
});

// Instantly jump to a value (no animation)
transition.jump_to(target_value, cx);

// Reset to initial state
transition.reset(cx);

// Read the end goal without triggering evaluation
let goal: &T = transition.read_goal(cx);

// Scale both start and end goals (useful for resize)
transition.scale_by(ratio, cx);  // requires T: Mul<f32>

// Get underlying entity ID
let id: EntityId = transition.entity_id();
```

### Builder Methods

```rust
let transition = window
    .use_keyed_transition(key, cx, duration, init)
    .with_easing(ease_in_out)    // set easing function
    .continuous(true);            // smooth from current value (default: true)
```

**Continuous mode** (default `true`): animates from current interpolated position to new goal.
**Non-continuous** (`false`): always restarts from initial value to new goal.

## Lerp Trait

```rust
pub trait Lerp<Output = Self> where Self: Sized {
    fn lerp(&self, to: &Self, delta: f32) -> Output;
}
```

**Built-in implementations:**
- Primitives: `f32`, `f64`, `bool`, `i8`–`i128`, `u8`–`u128`, `usize`, `isize`
- Geometry: `Point<T>`, `Size<T>`, `Edges<T>`, `Corners<T>`, `Bounds<T>`
- Color: `Rgba`
- Units: `Pixels`, `Rems`, `DevicePixels`, `Radians`, `Percentage`

### Using Lerp Directly

```rust
use gpui::Lerp;

let color_a = rgb(0xFF0000);
let color_b = rgb(0x0000FF);
let mid = color_a.lerp(&color_b, 0.5);  // purple

let start = 0.0_f32;
let end = 100.0_f32;
let value = start.lerp(&end, 0.75);  // 75.0
```

## Easing Functions

All easing functions have signature `fn(f32) -> f32` (input 0..1 → output 0..1).

```rust
use gpui::{linear, quadratic, ease_in_out, ease_out_quint, bounce, pulsating_between};

// Linear (no easing)
.with_easing(linear)

// Quadratic ease-in
.with_easing(quadratic)

// Smooth ease-in-out (most common)
.with_easing(ease_in_out)

// Quint ease-out (fast start, slow end)
.with_easing(ease_out_quint())

// Bounce (forward then reverse — pulsing effects)
.with_easing(bounce(ease_in_out))

// Pulsating alpha between min and max
.with_easing(pulsating_between(0.3, 1.0))
```

## Patterns

### Color Transition

```rust
let color_t = window
    .use_keyed_transition("bg-color", cx, Duration::from_millis(200), |_, _| {
        rgb(0x333333)
    })
    .with_easing(ease_in_out);

let bg: Rgba = *color_t.evaluate(window, cx);
div().bg(bg)
```

### Size Transition

```rust
let width_t = window
    .use_keyed_transition("panel-width", cx, Duration::from_millis(300), |_, _| 200.0_f32);

let w = *width_t.evaluate(window, cx);
div().w(px(w))
```

### Composite Key for Multiple Transitions

```rust
let hover = window.use_keyed_transition((self.id.clone(), "hover"), cx, dur, init);
let press = window.use_keyed_transition((self.id.clone(), "press"), cx, dur, init);
let focus = window.use_keyed_transition((self.id.clone(), "focus"), cx, dur, init);
```

### Transition in Lists

```rust
for (i, item) in items.iter().enumerate() {
    let fade = window.use_keyed_transition(
        ("item-fade", i),
        cx,
        Duration::from_millis(150),
        |_, _| 0.0_f32,
    );
    // Each item gets its own independent transition
}
```
