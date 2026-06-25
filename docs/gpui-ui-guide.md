# GPUI UI 実装ガイド

Ravel の UI 実装で得た知見をまとめたガイド。新しいパネルやカスタム描画を実装する際に参照する。

## パネル実装パターン

### 3層構造

| 層 | クレート | 役割 |
|---|---------|------|
| データモデル | `ravel-core` | Track/Clip 等のドメイン型。`im::Vector` で構造共有、serde 対応 |
| ヘッドレス状態 | `ravel-ui` | GPUI 非依存のパネル状態（選択、スクロール、ズーム等） |
| GPUI ビュー | `ravel-app` | `Panel` トレイト実装 + `Render` で描画 |

### GPUI パネルに必要なトレイト実装

```rust
use gpui_component::dock::{Panel, PanelEvent};

impl Panel for MyPanel {
    fn panel_name(&self) -> &'static str { "my_panel" }
    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(t!("panel.my_panel"))
    }
}
impl EventEmitter<PanelEvent> for MyPanel {}
impl Focusable for MyPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle { self.focus_handle.clone() }
}
impl Render for MyPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().track_focus(&self.focus_handle)
    }
}
```

### パネル登録

`panels/mod.rs` の `panel_for_kind()` で `PanelKind` ごとに分岐:

```rust
match kind {
    PanelKind::Timeline => {
        let entity = cx.new(TimelineGpuiPanel::new);
        Arc::new(entity)
    }
    _ => { /* PlaceholderPanel */ }
}
```

## Theme カラーの使い方

### 取得方法

```rust
use gpui_component::ActiveTheme;  // トレイト import 必須

// Render::render() 内で
let theme = cx.theme();  // &Theme
let colors = theme.colors;  // ThemeColor (Copy)
```

### 主要カラーフィールド (ThemeColor)

全て `Hsla` 型。

| フィールド | 用途 |
|-----------|------|
| `background` | パネル背景 |
| `foreground` | テキスト |
| `border` | ボーダー |
| `accent` | アクセント色（クリップ等） |
| `accent_foreground` | アクセント上のテキスト |
| `muted` | 控えめな背景 |
| `muted_foreground` | 控えめなテキスト（ラベル、サブ情報） |
| `list` | リスト/ヘッダー背景 |
| `list_hover` | リストホバー |
| `list_active` | リスト選択 |
| `tab_bar` | タブバー/ルーラー背景 |
| `danger` | 危険操作 |

その他: `primary`, `secondary`, `warning`, `info`, `success`, `chart_1`〜`chart_5`, `scrollbar`, `sidebar` 等多数。

### 透明度の調整

`Hsla` 構造体の `a` フィールドを直接変更:

```rust
// 良い（Copyなのでspread可能）
Hsla { a: 0.5, ..colors.background }

// gpui_component::Colorize トレイトも使える
use gpui_component::Colorize;
colors.foreground.opacity(0.6)
```

### RGBA → HSLA 変換

`clip.color` 等が `[f32; 4]` (RGBA) の場合:

```rust
use gpui::{Rgba, Hsla};
Hsla::from(Rgba { r: c[0], g: c[1], b: c[2], a: c[3] })
```

**注意**: `hsla(h, s, l, a)` に RGBA 値を直接渡さないこと — 色空間が異なる。

## Canvas によるカスタム描画

### 基本構造

```rust
use gpui::canvas;

canvas(
    // prepaint: レイアウト後に呼ばれる。描画は不可。戻り値が paint に渡される。
    move |bounds, _window, _cx| {
        // bounds キャプチャや状態の準備
        my_state  // → paint の第2引数になる
    },
    // paint: 描画フェーズ。paint_quad, shape_line 等が使える。
    move |bounds, my_state, window, cx| {
        window.paint_quad(fill(bounds, color));
    },
)
.h(px(24.0))
.w_full()
```

### 重要: prepaint vs paint

- **prepaint**: `paint_quad()` 等を呼ぶと **パニック** する（`"this method can only be called during paint"`）
- **paint**: ここでのみ描画関数を呼べる
- prepaint の戻り値が paint の第2引数として渡される

### 矩形描画

```rust
// 塗りつぶし
window.paint_quad(fill(bounds, color));

// 角丸
window.paint_quad(fill(bounds, color).corner_radii(px(4.0)));

// 枠線のみ
window.paint_quad(
    outline(bounds, border_color, BorderStyle::default())
        .corner_radii(px(4.0))
        .border_widths(px(2.0))
);
```

### テキスト描画

```rust
let text: SharedString = "Hello".into();
let text_len = text.len();  // 必ず実際の文字列長を使う

let shaped = window.text_system().shape_line(
    text,
    px(11.0),  // font size
    &[TextRun {
        len: text_len,  // ← usize::MAX にするとパニック
        font: Font { family: SharedString::from("sans-serif"), ..Default::default() },
        color: colors.foreground,
        background_color: None,
        underline: None,
        strikethrough: None,
    }],
    None,  // force_width: Option<Pixels>
);

// shape_line は ShapedLine を直接返す（Result ではない）
shaped.paint(origin, line_height, TextAlign::Left, None, window, cx).ok();
//                                                              ^^  ^^ 6引数（cx を忘れない）
```

## イベントハンドリング

### マウスイベント

```rust
// クリック（id 必須）
div()
    .id("my-element")
    .on_click(cx.listener(|this, _event, _window, cx| {
        // this: &mut Self にアクセス可能
        cx.notify();  // 再描画トリガー
    }))

// マウスダウン（id 不要）
div()
    .on_mouse_down(MouseButton::Left, cx.listener(|this, event: &MouseDownEvent, _window, cx| {
        let pos = event.position;  // ウィンドウ座標（パネル相対ではない）
    }))

// マウス移動（ドラッグ追従に使う）
div()
    .id("my-element")
    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
        if event.pressed_button == Some(MouseButton::Left) {
            // ドラッグ中のみ反応
        }
    }))
```

### スクロール

```rust
.on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
    let delta = event.delta.pixel_delta(px(20.0));
    let dx: f32 = delta.x.into();  // Pixels → f32 は .into()
    let dy: f32 = delta.y.into();

    if event.modifiers.platform || event.modifiers.control {
        // ズーム操作
    } else {
        // スクロール操作
    }
    cx.notify();  // 忘れると再描画されない
}))
```

### cx.notify() を忘れない

状態を変更した後に `cx.notify()` を呼ばないと**画面が更新されない**。全てのイベントハンドラで状態変更後に必ず呼ぶこと。

### ウィンドウ座標 → パネルローカル座標

`event.position` はウィンドウ全体の座標。パネル内の相対位置を得るには、canvas の prepaint で要素の bounds origin をキャプチャする:

```rust
let origin_x = Rc::new(Cell::new(px(0.0)));

// canvas prepaint で origin を記録
canvas(
    { let ox = origin_x.clone(); move |bounds, _, _| { ox.set(bounds.origin.x); state } },
    move |bounds, state, window, cx| { /* paint */ },
)

// イベントハンドラで相対座標を計算
.on_mouse_down(MouseButton::Left, cx.listener({
    let origin_x = origin_x.clone();
    move |this, event: &MouseDownEvent, _window, cx| {
        let click_x: f32 = event.position.x.into();
        let ox: f32 = origin_x.get().into();
        let local_x = (click_x - ox).max(0.0);
    }
}))
```

## コンテキストメニュー

```rust
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};

div()
    .id("my-element")
    .context_menu(move |menu, _window, _cx| {
        let entity = entity.clone();  // WeakEntity<Self>
        menu.item(
            PopupMenuItem::new("メニュー項目").on_click({
                let entity = entity.clone();
                move |_, _window, cx| {
                    // on_click は Fn(&ClickEvent, &mut Window, &mut App)
                    // Context<Self> がないので WeakEntity 経由で更新
                    entity.update(cx, |this, cx| {
                        this.do_something();
                        cx.notify();
                    }).ok();
                }
            }),
        )
    })
```

**重要**: `PopupMenu::on_click` のシグネチャは `Fn(&ClickEvent, &mut Window, &mut App)` で `Context<Self>` がない。`cx.entity().downgrade()` で `WeakEntity` を取得し、closure にキャプチャして `entity.update(cx, ...)` で entity を操作する。

## Pixels 型の扱い

`Pixels` の内部フィールド `0: f32` は `pub(crate)` で外部から直接アクセス不可。

```rust
// Pixels → f32
let f: f32 = my_pixels.into();  // From<Pixels> for f32

// f32 → Pixels
let p = px(42.0);

// 比較・演算
bounds.size.width * 0.5  // Pixels * f32 → Pixels (Mul impl あり)
```

## Modifiers

```rust
event.modifiers.platform  // Cmd (macOS) / Win (Windows) / Super (Linux)
event.modifiers.control
event.modifiers.alt
event.modifiers.shift
event.modifiers.function
```

**注意**: `command` フィールドは存在しない。macOS の Cmd キーは `platform`。

## i18n (TOML カタログ)

```toml
# assets/locales/en.toml

# フラットキー → "panel.outliner" として参照
[panel]
outliner = "Outliner"

# サブテーブル → "_self" は "panel.timeline" として参照
[panel.timeline]
_self = "Timeline"
empty = "No tracks"
```

**衝突注意**: 同じキー（例: `panel.timeline`）をフラットキーとサブテーブルの両方に定義すると TOML パースエラーになる。サブテーブルを使うパネルはフラットキーから除外すること。

## レイアウト Tips

- `div().size_full()` → 親要素いっぱい
- `div().flex().flex_col()` → 縦方向 flex
- `div().flex().flex_row()` → 横方向 flex
- `.flex_grow()` → 残りスペースを埋める
- `.flex_shrink_0()` → 縮小しない（固定幅要素に使う）
- `.overflow_hidden()` → はみ出しをクリップ（canvas が突き抜ける場合に必須）
- `.w(px(150.0))` / `.h(px(24.0))` → 固定サイズ
- `.gap_1()` / `.px_2()` → spacing / padding
- `.border_r_1().border_color(color)` → 右ボーダー
