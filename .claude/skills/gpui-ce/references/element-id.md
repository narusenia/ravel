# ElementId

`ElementId` is a unique identifier for a gpui-ce element. Required for:
- Mouse event handling (`on_click`, `on_hover`, etc.)
- State storage via `window.use_keyed_state`
- Transition hooks via `window.use_keyed_transition`
- Interaction tracking

## Making an Element Stateful

```rust
div().id("my-element")          // from &str
div().id(42usize)               // from usize
div().id(ElementId::from(idx))  // explicit
```

Without `.id()`, a div cannot receive mouse events or store state.

## Accepted Types

```rust
impl Into<ElementId> for &str
impl Into<ElementId> for String
impl Into<ElementId> for usize
impl Into<ElementId> for u64
impl Into<ElementId> for SharedString
```

## Composite Keys

Keys can be tuples for disambiguation:

```rust
// Tuple keys for hooks
window.use_keyed_state(("item", i), cx, init);
window.use_keyed_transition((self.id.clone(), "hover"), cx, dur, init);
```

## Uniqueness Rules

IDs are scoped to parent — not global. GPUI builds `GlobalElementId` by chaining:

```rust
div().id("app").child(
    div().id("list1").children(vec![
        div().id(1usize),  // GlobalId: ["app", "list1", 1]
    ])
).child(
    div().id("list2").children(vec![
        div().id(1usize),  // GlobalId: ["app", "list2", 1] — no conflict
    ])
)
```

## In Component Structs

```rust
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    base: Stateful<Div>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        let id = id.into();
        Self {
            id: id.clone(),
            base: div().id(id),
        }
    }
}

impl RenderOnce for Button {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.base.on_click(/* ... */)
    }
}
```

## Usage Patterns

```rust
Button::new("save-btn")           // named components
for (i, item) in items.iter().enumerate() {
    div().id(i)                    // index-based in lists
}
Input::new("search-input")        // descriptive for debugging
```
