# Assets (use_asset / Asset trait)

**Contents:** [Overview](#overview) · [Asset Trait](#asset-trait) · [use_asset](#use_asset) · [get_asset](#get_asset) · [Patterns](#patterns)

## Overview

gpui-ce provides an async asset loading system via the `Asset` trait and `window.use_asset()` hook.

**Key features:**
- Async loading with automatic re-render on completion
- Deduplication — multiple calls for the same source trigger a single load
- Returns `Option<Output>` — `None` until loaded

## Asset Trait

```rust
pub trait Asset: 'static {
    type Source: Clone + Hash + Send;
    type Output: Clone + Send;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static;
}
```

### Implementing an Asset

```rust
struct ImageAsset;

impl Asset for ImageAsset {
    type Source = String;  // URL or path
    type Output = ImageData;

    fn load(source: Self::Source, _cx: &mut App) -> impl Future<Output = Self::Output> + Send + 'static {
        async move {
            let bytes = reqwest::get(&source).await.unwrap().bytes().await.unwrap();
            ImageData::from_bytes(bytes)
        }
    }
}
```

## use_asset

```rust
pub fn use_asset<A: Asset>(
    &mut self,          // &mut Window
    source: &A::Source,
    cx: &mut App,
) -> Option<A::Output>
```

Returns `None` on first call, schedules async load, and re-renders the view when the asset is ready. Subsequent calls with the same source return the cached result immediately.

### Usage

```rust
impl Render for ImageViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let image = window.use_asset::<ImageAsset>(&self.image_url, cx);

        div().child(match image {
            Some(data) => div().child(img(data)),
            None => div().child("Loading..."),
        })
    }
}
```

## get_asset

```rust
pub fn get_asset<A: Asset>(
    &mut self,          // &mut Window
    source: &A::Source,
    cx: &mut App,
) -> Option<A::Output>
```

Same as `use_asset` but does **not** schedule a re-render when the asset finishes loading. Use when you want to check if an asset is available without triggering a render cycle.

## Patterns

### Graceful Loading State

```rust
fn render_avatar(url: &str, window: &mut Window, cx: &mut App) -> impl IntoElement {
    match window.use_asset::<AvatarAsset>(url, cx) {
        Some(img) => div().size_8().rounded_full().child(img),
        None => div().size_8().rounded_full().bg(rgb(0x666666)),  // placeholder
    }
}
```

### Multiple Assets

```rust
let thumbnail = window.use_asset::<Thumbnail>(&self.thumb_url, cx);
let metadata = window.use_asset::<Metadata>(&self.meta_url, cx);

// Each loads independently; view re-renders as each completes
```
