# Custom Elements (Low-Level Element Trait)

## When to Use

Use the low-level `Element` trait when:
- Need fine-grained control over layout calculation
- Building complex, performance-critical components
- Implementing custom layout algorithms
- High-level `Render`/`RenderOnce` APIs are insufficient

**Prefer `Render`/`RenderOnce` for:** Simple components, standard layouts, declarative UI.

## Quick Start

```rust
impl Element for MyElement {
    type RequestLayoutState = MyLayoutState;
    type PrepaintState = MyPaintState;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    // Phase 1: Calculate sizes and positions
    fn request_layout(&mut self, .., window: &mut Window, cx: &mut App)
        -> (LayoutId, Self::RequestLayoutState)
    {
        let layout_id = window.request_layout(
            Style { size: size(px(200.), px(100.)), ..default() },
            vec![],
            cx
        );
        (layout_id, MyLayoutState { /* ... */ })
    }

    // Phase 2: Create hitboxes, prepare for painting
    fn prepaint(&mut self, .., bounds: Bounds<Pixels>, layout: &mut Self::RequestLayoutState,
                window: &mut Window, cx: &mut App) -> Self::PrepaintState
    {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        MyPaintState { hitbox }
    }

    // Phase 3: Render and handle interactions
    fn paint(&mut self, .., bounds: Bounds<Pixels>, layout: &mut Self::RequestLayoutState,
             paint_state: &mut Self::PrepaintState, window: &mut Window, cx: &mut App)
    {
        window.paint_quad(paint_quad(bounds, Anchor::all(px(4.)), cx.theme().background));

        window.on_mouse_event({
            let hitbox = paint_state.hitbox.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                if hitbox.is_hovered(window) && phase.bubble() {
                    cx.stop_propagation();
                }
            }
        });
    }
}

impl IntoElement for MyElement {
    type Element = Self;
    fn into_element(self) -> Self::Element { self }
}
```

## Three-Phase Rendering

1. **request_layout**: Calculate sizes, return layout ID and state
2. **prepaint**: Create hitboxes, compute final bounds
3. **paint**: Render element, set up interactions

### State Flow

```
RequestLayoutState → PrepaintState → paint
```

### Key Operations

- **Layout**: `window.request_layout(style, children, cx)`
- **Hitboxes**: `window.insert_hitbox(bounds, behavior)`
- **Painting**: `window.paint_quad(...)`
- **Events**: `window.on_mouse_event(handler)`
