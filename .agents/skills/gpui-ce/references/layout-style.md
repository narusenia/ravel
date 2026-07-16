# Layout & Styling

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Common Patterns](#common-patterns) · [Styling Methods](#styling-methods) · [h_flex / v_flex](#h_flex--v_flex-helpers) · [Tailwind Shorthands](#tailwind-style-shorthand) · [Overflow & Scroll](#overflow-and-scroll) · [Absolute Positioning](#absolute-positioning) · [Theme Integration](#theme-integration) · [Conditional Styling](#conditional-styling) · [Text Styling](#text-styling)

## Overview

gpui-ce provides CSS-like styling with Rust type safety.

- Flexbox layout system
- Styled trait for chaining
- Size units: `px()`, `rems()`, `relative()`
- Colors, borders, shadows

## Quick Start

### Basic Styling

```rust
div()
    .w(px(200.))
    .h(px(100.))
    .bg(rgb(0x2196F3))
    .text_color(rgb(0xFFFFFF))
    .rounded(px(8.))
    .p(px(16.))
    .child("Styled content")
```

### Flexbox Layout

```rust
div()
    .flex()
    .flex_row()
    .gap(px(8.))
    .items_center()
    .justify_between()
    .children([
        div().child("Item 1"),
        div().child("Item 2"),
    ])
```

### Size Units

```rust
div()
    .w(px(200.))           // Pixels
    .h(rems(10.))          // Relative to font size
    .w(relative(0.5))      // 50% of parent
    .min_w(px(100.))
    .max_w(px(400.))
```

## Common Patterns

### Centered Content

```rust
div()
    .flex()
    .items_center()
    .justify_center()
    .size_full()
    .child("Centered")
```

### Card Layout

```rust
div()
    .w(px(300.))
    .bg(cx.theme().surface)
    .rounded(px(8.))
    .shadow_md()
    .p(px(16.))
    .gap(px(12.))
    .flex()
    .flex_col()
    .child(heading())
    .child(content())
```

## Styling Methods

### Dimensions

```rust
.w(px(200.))     .h(px(100.))     .size(px(200.))
.min_w(px(100.))  .max_w(px(400.))
```

### Colors

```rust
.bg(rgb(0x2196F3))
.text_color(rgb(0xFFFFFF))
.border_color(rgb(0x000000))
```

### Borders

```rust
.border(px(1.))    .rounded(px(8.))    .rounded_t(px(8.))
.border_color(rgb(0x000000))
```

### Spacing

```rust
.p(px(16.))    .m(px(8.))    .gap(px(8.))
```

### Flexbox

```rust
.flex()    .flex_row()    .flex_col()
.items_center()    .justify_between()    .flex_grow_1()
```

## h_flex / v_flex Helpers

From `gpui_component`:

```rust
use gpui_component::{h_flex, v_flex};

h_flex().gap_2().child(icon).child(label)
// = div().flex().flex_row().items_center()

v_flex().gap_4().p_4().child(input).child(button)
// = div().flex().flex_col()
```

## Tailwind-style Shorthand

```rust
.p_2()    // padding: 8px       (0=0, 1=4px, 2=8px, 3=12px, 4=16px)
.px_4()   // padding x: 16px
.py_3()   // padding y: 12px
.m_2()    // margin: 8px
.gap_3()  // gap: 12px

.size_full()    // w: 100%, h: 100%
.w_full()       // width: 100%
.flex_1()       // flex: 1 1 0
.flex_shrink_0()
```

## Overflow and Scroll

```rust
.overflow_hidden()
.overflow_x_hidden()
.overflow_y_scrollbar()
.overflow_scroll()
```

## Absolute Positioning

```rust
div()
    .relative()
    .child(
        div().absolute().top_0().right_0().child("badge")
    )

div().absolute().inset_0()  // fill parent
```

### Stacking Order

No general `z_index()` method in gpui-ce. Stacking controlled by:
- Parent/child composition
- Absolute positioning
- Render order (later siblings paint above earlier ones)

## Theme Integration

```rust
div()
    .bg(cx.theme().surface)
    .text_color(cx.theme().foreground)
    .border_color(cx.theme().border)
```

## Conditional Styling

```rust
use gpui::prelude::FluentBuilder as _;

div()
    .when(is_active, |el| el.bg(cx.theme().primary))
    .when(!is_active, |el| el.opacity(0.5))
    .when_some(optional_color.as_ref(), |el, color| el.bg(*color))
```

## Text Styling

```rust
.text_sm()    .text_base()    .text_lg()    .text_2xl()
.font_bold()  .font_weight(FontWeight::SEMIBOLD)
.line_height_snug()    .truncate()    .whitespace_nowrap()
```
