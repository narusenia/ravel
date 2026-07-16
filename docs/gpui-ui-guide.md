# GPUI UI 実装ガイド

Ravel の UI 実装で得た知見をまとめたガイド。新しいパネルやカスタム描画を実装する際に参照する。

## パネル実装パターン

### 3層構造

| 層 | クレート | 役割 |
|---|---------|------|
| データモデル | `ravel-core` | Composition/Layer 等のドメイン型。`im::Vector` で構造共有、serde 対応 |
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
        let entity = cx.new(|cx| TimelineGpuiPanel::new(window, cx));
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
- `.border_t_1()` → タイトル/コンテンツ間の区切り線に使う

## パネルフォーカス管理の注意点

### FocusedPanelGlobal パターン

パネルがフォーカスされたことを全体に伝えるには `FocusedPanelGlobal` グローバルを使う。

```rust
// パネルの render 内の on_mouse_down で設定
.on_mouse_down(MouseButton::Left, move |_event, window, cx| {
    focus.focus(window, cx);
    cx.set_global(FocusedPanelGlobal(Some(PanelKind::Timeline)));
})
```

**重要**: `FocusedPanelGlobal` を設定しないと `AppShell::handle_detach()` で `focused_panel` が `None` になり、detach が効かない。新しいパネルを実装したら必ず `on_mouse_down` で設定すること。

### タイトル文字色の切り替え

DockArea の Tab は単一タブパネルでは active/inactive のスタイル切り替えをしない。`Panel::set_active()` も単一タブでは呼ばれない。

**正しいアプローチ**: `observe_global::<FocusedPanelGlobal>` で全パネルに通知 → `title()` 内で `is_panel_focused()` チェック。

```rust
// コンストラクタで observe_global を登録
let focused_sub = cx.observe_global::<FocusedPanelGlobal>(|_this, cx| {
    cx.notify();  // FocusedPanelGlobal 変更時に再描画
});

// title() で判定
fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let focused = is_panel_focused(PanelKind::Timeline, cx);
    let color = if focused { cx.theme().colors.foreground } else { cx.theme().colors.muted_foreground };
    div().text_xs().text_color(color).child(...)
}
```

**やってはいけないこと**:
- `Panel::set_active()` に頼る → 単一タブパネルでは呼ばれない
- `focus_handle.contains_focused()` を `title()` で使う → TabPanel の render 時に呼ばれるが、focus 変更で TabPanel は再描画されない
- `Panel::title_style()` で foreground を制御する → Tab コンポーネント側のスタイルと競合する

### register_panels と panel_for_kind

`register_panels()` の factory closure は DockArea がパネルを復元（reattach 含む）するときに使われる。`panel_for_kind()` はレイアウト構築時に使われる。**両方で** concrete パネルを返さないと、reattach 時に PlaceholderPanel に戻る。

```rust
// register_panels 内
register_panel(cx, &panel_id, move |_, _, _, window, cx| match kind {
    PanelKind::Timeline => {
        let entity = cx.new(|cx| TimelineGpuiPanel::new(window, cx));
        Box::new(entity)
    }
    _ => { /* PlaceholderPanel */ }
});
```

## ノードエディタ実装で得た知見

### on_key_down vs on_action (キーボードショートカット)

`Cmd+Z` 等のキーバインドはアプリの `build_keybindings()` で GPUI のアクションシステムに登録される。メニューバー経由で消費されるため、`on_key_down` には到達しない。

**正しいアプローチ**: `on_action` を使う:

```rust
.on_action(cx.listener(|this, _: &crate::workspace::EditUndo, _window, cx| {
    this.undo();
    cx.notify();
}))
```

`on_key_down` は Delete/Backspace 等、アクションシステムに登録されていないキーにのみ使う。

### ノード重なり時のヒットテスト順序

`im::HashMap` のイテレーション順は不定。canvas 描画は `graph.nodes()` の順で行うため、後に描画されたノードが視覚的に手前になる。ヒットテストで早期 return すると、手前のノードではなく背面のノードを選択してしまう。

**正しいアプローチ**: 全ノードを走査し、最後にヒットしたノードを返す:

```rust
fn node_at_local_pos(&self, lx: f32, ly: f32) -> Option<NodeId> {
    let mut hit = None;
    for node in self.graph.nodes() {
        // ... bounds check ...
        if lx >= sx && lx <= sx + w && ly >= sy && ly <= sy + h {
            hit = Some(node.id);  // 上書き（最後のヒット = 最前面）
        }
    }
    hit
}
```

### ズーム連動ノードスケーリング

ノードサイズを固定にすると、ズームアウト時にノードが重なる。全レイアウト定数（パディング、行高さ、フォントサイズ、ドット半径、角丸）をズーム倍率でスケーリングすることでカメラズーム的な挙動を実現。

```rust
// BASE 定数をズーム倍率で乗算
let pad = BASE_NODE_PAD * zoom;
let font_size = 12.0 * zoom;
let dot_r = BASE_PORT_DOT_R * zoom;
```

ズーム変更時は `node_sizes` を再計算すること。

### コンテキストメニューのサブメニュー

`PopupMenuItem::submenu(label, entity)` でサブメニューの `Entity<PopupMenu>` を直接渡すと `parent_menu` が未設定になり、メニューが閉じなくなる。

**正しいアプローチ**: `PopupMenu::submenu(label, window, cx, builder_fn)` メソッドを使う。これは内部で `parent_menu` を自動設定し、dismiss チェーンが正しく動作する。

```rust
// NG — parent_menu 未設定、dismiss が壊れる
menu.item(PopupMenuItem::submenu(label, my_submenu_entity))

// OK — parent_menu が自動設定される
menu.submenu(label, window, cx, |sub, _window, _cx| {
    sub.item(PopupMenuItem::new("item").on_click(...))
})
```

### Entity 境界と text_color cascade

gpui-flow を `Entity<FlowGraph>` として DockArea に配置すると、`text_color` の cascade が Entity 境界をまたがない。`gpui-component::Label` も `cx.theme().foreground` をハードコードしている。

**正しいアプローチ**: 外部ライブラリの Entity 埋め込みではなく、パネル内で canvas ベースの直接描画を行い、テーマ色は `cx.theme().colors` から取得して paint 内で使用する。
