# Animation

**Contents:** [Overview](#overview) · [with_animation](#with_animation) · [Animation Config](#animation-config) · [Transformation](#transformation) · [Easing Functions](#easing-functions) · [Examples](#examples)

## Overview

gpui-ce provides two animation systems:

1. **`with_animation`** — declarative, element-level animations (rotation, scale, translate)
2. **Transitions** — value interpolation hooks (see [transition.md](transition.md))

Use `with_animation` for continuous/looping animations. Use transitions for state-driven interpolation.

## with_animation

```rust
use gpui::{Animation, AnimationExt, Transformation};

element
    .with_animation(
        "unique-key",
        Animation::new(Duration::from_secs(2))
            .repeat()
            .with_easing(ease_in_out),
        |element, delta| {
            // delta: 0.0..1.0 (after easing)
            element.with_transformation(Transformation::rotate(percentage(delta)))
        },
    )
```

## Animation Config

```rust
Animation::new(Duration::from_secs(2))    // duration
    .repeat()                              // loop forever
    .with_easing(ease_in_out)              // easing function
```

## Transformation

```rust
use gpui::{Transformation, percentage, size};

// Rotation (0..1 maps to 0..360°)
Transformation::rotate(percentage(delta))

// Scale
Transformation::scale(size(scale_x, scale_y))

// Combined
Transformation::rotate(percentage(delta))
    .with_scaling(size(scale, scale))
```

## Easing Functions

```rust
use gpui::{linear, quadratic, ease_in_out, ease_out_quint, bounce, pulsating_between};

linear          // constant speed
quadratic       // accelerate (ease-in)
ease_in_out     // smooth start and end
ease_out_quint()  // fast start, slow end (returns closure)
bounce(ease_in_out)  // forward then reverse
pulsating_between(0.3, 1.0)  // oscillating alpha
```

## Examples

### Spinning Icon

```rust
svg()
    .size_16()
    .path("icons/spinner.svg")
    .with_animation(
        "spin",
        Animation::new(Duration::from_secs(1)).repeat().with_easing(linear),
        |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
    )
```

### Pulsing Scale

```rust
svg()
    .size_8()
    .path("icons/dot.svg")
    .with_animation(
        "pulse",
        Animation::new(Duration::from_millis(1500)).repeat().with_easing(bounce(linear)),
        |el, delta| {
            let scale = 0.8 + (delta * 0.4);
            el.with_transformation(Transformation::scale(size(scale, scale)))
        },
    )
```

### Combined Rotation + Scale

```rust
svg()
    .size_16()
    .path("icons/star.svg")
    .with_animation(
        "combined",
        Animation::new(Duration::from_secs(3)).repeat().with_easing(ease_in_out),
        |el, delta| {
            let scale = 0.7 + (delta * 0.6);
            el.with_transformation(
                Transformation::rotate(percentage(delta))
                    .with_scaling(size(scale, scale)),
            )
        },
    )
```
