# Testing

## Overview

gpui-ce provides a testing framework with `#[gpui::test]` attribute. Tests run on a single-threaded executor for deterministic execution.

**Rule**: If test does not require windows or rendering, write a plain Rust test instead.

## Test Attributes

### Basic Test

```rust
#[gpui::test]
fn my_test(cx: &mut TestAppContext) {
    // Test implementation
}
```

### Async Test

```rust
#[gpui::test]
async fn my_async_test(cx: &mut TestAppContext) {
    // Async test implementation
}
```

### Property Test

```rust
#[gpui::test(iterations = 10)]
fn my_property_test(cx: &mut TestAppContext, mut rng: StdRng) {
    // Randomized testing
}
```

## Test Contexts

### TestAppContext

For testing without windows:

```rust
#[gpui::test]
fn test_entity_operations(cx: &mut TestAppContext) {
    let entity = cx.new(|cx| MyComponent::new(cx));

    entity.update(cx, |component, cx| {
        component.value = 42;
        cx.notify();
    });

    let value = entity.read_with(cx, |component, _| component.value);
    assert_eq!(value, 42);
}
```

### VisualTestContext

For window-dependent tests:

```rust
#[gpui::test]
fn test_with_window(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| MyComponent::new(cx))
        }).unwrap()
    });

    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let component = window.root(&mut cx).unwrap();
}
```
