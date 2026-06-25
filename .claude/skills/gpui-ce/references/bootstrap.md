# Bootstrap / App Setup

**Contents:** [Overview](#overview) · [Minimal App](#minimal-app) · [With Assets](#with-assets) · [Window Options](#window-options) · [Root View](#root-view) · [Platform Notes](#platform-notes)

## Overview

gpui-ce uses `gpui_platform::application()` to bootstrap — **not** `Application::new()`.

**Required features:**
- `gpui_platform` with `font-kit` feature — without it, text renders as nothing (`NoopTextSystem`)

## Minimal App

```rust
use gpui::{App, Bounds, Context, Render, Window, WindowBounds, WindowOptions, div, prelude::*, px, size};

struct MyApp;

impl Render for MyApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child("Hello gpui-ce!")
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(800.), px(600.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| MyApp),
        )
        .expect("Failed to open window");
    });
}
```

## With Assets

```rust
use gpui::{AssetSource, SharedString};

struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        std::fs::read(path).map(Into::into).map_err(Into::into).map(Some)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(std::fs::read_dir(path)?
            .filter_map(|e| Some(SharedString::from(e.ok()?.path().to_string_lossy().into_owned())))
            .collect())
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(Assets)
        .run(|cx: &mut App| {
            // ...
        });
}
```

## Window Options

```rust
use gpui::{Bounds, WindowBounds, WindowOptions, px, size};

let bounds = Bounds::centered(None, size(px(800.), px(600.)), cx);

cx.open_window(
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // focus: true,              // focus on open
        // titlebar: None,           // frameless window
        ..Default::default()
    },
    |_window, cx| cx.new(|cx| MyRootView::new(cx)),
)
```

## Root View

When using `gpui_component`, wrap the root view in `Root` for theme, fonts, rem_size, and notification layer:

```rust
use gpui_component::{Root, theme::Theme};

cx.open_window(options, |window, cx| {
    cx.new(|cx| {
        gpui_component::init(cx);
        Theme::sync_system_appearance(None, cx);
        Root::new(view.into(), window, cx)
    })
})
```

**Without `Theme::sync_system_appearance`**, light theme is the default.

## Platform Notes

- **macOS**: Metal rendering via `gpui_macos`. `QuitMode::Default` = `Explicit` (process survives window close). Set `QuitMode::LastWindowClosed` explicitly.
- **Linux/Windows**: wgpu rendering via `gpui_wgpu`.
- **`font-kit` feature**: Must be enabled on `gpui_platform` or text won't render.
- **`gpui_component` fork**: For ravel, use `narusenia/gpui-component` on `gpui-ce-compat` branch with `[patch]` section to swap gpui → gpui-ce.
