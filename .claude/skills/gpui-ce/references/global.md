# Global State

**Contents:** [Overview](#overview) · [Quick Start](#quick-start) · [Common Use Cases](#common-use-cases) · [Best Practices](#best-practices)

## Overview

Global state in gpui-ce provides app-wide shared data accessible from any context.

**Key Trait**: `Global` — implement on types to make them globally accessible.

## Quick Start

### Define Global State

```rust
use gpui::Global;

#[derive(Clone)]
struct AppSettings {
    theme: Theme,
    language: String,
}

impl Global for AppSettings {}
```

### Set and Access Globals

```rust
gpui_platform::application().run(|cx: &mut App| {
    cx.set_global(AppSettings {
        theme: Theme::Dark,
        language: "en".to_string(),
    });

    let settings = cx.global::<AppSettings>();
});
```

### Update Globals

```rust
cx.update_global::<AppSettings, _>(|settings, cx| {
    settings.theme = new_theme;
});
cx.notify();
```

## Common Use Cases

### App Configuration

```rust
#[derive(Clone)]
struct AppConfig {
    api_endpoint: String,
    max_retries: u32,
}
impl Global for AppConfig {}
```

### Shared Services

```rust
#[derive(Clone)]
struct ServiceRegistry {
    http_client: Arc<HttpClient>,
}
impl Global for ServiceRegistry {}
```

## Best Practices

### ✅ Arc for Shared Resources

```rust
#[derive(Clone)]
struct GlobalState {
    database: Arc<Database>,
}
impl Global for GlobalState {}
```

### ✅ Observe Global Changes

```rust
cx.observe_global::<AppSettings>(|cx| {
    cx.notify();
});
```

### ❌ Don't Overuse Globals

Use entities for component-specific or frequently-changing state. Globals are best for configuration, services, and read-only reference data.
