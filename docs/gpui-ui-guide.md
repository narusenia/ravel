# GPUI UI 実装ガイド

Ravel の UI 実装で得た知見をまとめたガイド。新しいパネルやカスタム描画を実装する際に参照する。

## パネル実装パターン

### 3層構造

| 層 | クレート | 役割 |
|---|---------|------|
| データモデル | `ravel-core` | Composition/Layer 等のドメイン型。`im::Vector` で構造共有、serde 対応 |
| ヘッドレス状態 | `ravel-ui` | GPUI 非依存のパネル状態（選択、スクロール、ズーム等） |
| GPUI ビュー | `ravel-app` | `Render` + `Focusable` の普通のエンティティ |

### GPUI パネルに必要なトレイト実装

パネルは**ドッキング用の trait を実装しない**。タブ・分割・detach は
`ravel-dock` がレイアウトツリーから描くので、パネル側は `Render` +
`Focusable` を持つ普通のエンティティでよい。

```rust
impl Focusable for MyPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle { self.focus_handle.clone() }
}
impl Render for MyPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().track_focus(&self.focus_handle)
    }
}
```

コンストラクタは**第 1 引数に `PanelInstanceId` を取る**。同じパネルが複数枚
開かれるので、フォーカス追跡もタブ表示もインスタンス単位になる。

```rust
impl MyPanel {
    pub fn new(instance: PanelInstanceId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let focus_subs = track_panel_focus(instance, &focus_handle, window, cx);
        Self { focus_handle, focus_subs, /* … */ }
    }
}
```

### パネル登録

`panels/mod.rs` の `build_panel_view()` が**パネルビューを生成する唯一の場所**。
戻り値は `AnyView` で、`PanelViews`（`PanelInstanceId` キーのレジストリ）が
キャッシュして `ravel_dock::PaneContent` として窓へ供給する。

```rust
match instance.kind {
    PanelKind::Timeline => cx
        .new(|cx| timeline::TimelineGpuiPanel::new(instance.id, window, cx))
        .into(),
    _ => { /* PlaceholderPanel */ }
}
```

**同期が必要な第 2 のレジストリは無い。** 手順の全体は
[`dev/add-panel.md`](dev/add-panel.md)。

## ドッキング（ravel-dock とウィンドウホスト）

ドッキング UI は独自実装で、gpui-component の `dock` モジュール
（`Panel` / `PanelEvent` / `DockArea` / `register_panels`）は**使わない**。
gpui-component にはテーマと汎用部品（`TitleBar` / `TabBar` / `Button` /
`Dialog` …）だけを頼る。挙動の仕様は
[`specifications/ui/workspaces.md`](specifications/ui/workspaces.md)、
型の地図は [`agent-api-reference.md`](agent-api-reference.md)。

### 一方向の輪

```text
AppShell（実効レイアウト = 唯一の真実）
   │  observe
   ▼
WindowHost（1 論理ウィンドウ = TitleBar + DockRoot + ダイアログ / 通知層）
   │  set_layout(tree)
   ▼
DockRoot（ツリーを描く。モデルは書かない）
   │  emit DockEvent
   └──▶ WindowHost が AppShell へ適用 → 通知が戻って再描画
```

**`ravel-dock` はモデルを書かない。** ユーザー操作は `DockEvent`
（`SplitRatioChanged` / `TabActivated` / `TabDropped` / `TabDetachRequested` /
`AreaActionRequested`）として出るだけで、ホストがそれを自分のレイアウト状態へ
適用し、更新後のツリーを `DockRoot::set_layout` で押し戻す。適用ヘルパ
（`ravel_dock::{set_ratio_at, activate_tab, apply_tab_drop, apply_area_action}`）が
全種類をカバーしているので、ホストがツリーを手で書き換える必要は無い。

この一方向のおかげで、View トグル・プリセット切替・reattach・別ウィンドウでの
ドロップが**全部同じ経路**で再描画される。誰もツリーを押し付けない。

### ペインの中身は `PaneContent` で供給する

```rust
impl PaneContent for PanelViews {
    fn tab_title(&self, instance: &PanelInstance, window: &Window, cx: &App) -> SharedString { … }
    fn tab_icon(&self, instance: &PanelInstance, window: &Window, cx: &App) -> Option<Icon> { … }
    fn view(&self, instance: &PanelInstance, window: &mut Window, cx: &mut App) -> AnyView { … }
}
```

- `ravel-dock` は `PanelKind` を知らない。**アプリのロジックを持ち込まない**
- `view()` は**インスタンスごとに安定したビューを返す**こと。毎フレーム新しい
  エンティティを作るとペインのビュー状態が毎フレーム消える。`PanelViews` が
  `PanelInstanceId` でキャッシュしているのはこのため
- `Tab::icon` はラベルを置き換える仕様なので、アイコン + ラベルを両方出すには
  `Tab::prefix` に入れる（`tab_icon` の値はドック側でそう扱われる）

### ウィンドウを開く・閉じる

`window_host::{open, close, close_all_detached, set_detached_minimized,
open_restored}` がウィンドウのライフサイクル。論理 `WindowId` ↔ GPUI
`AnyWindowHandle` の対応表は `WindowRegistry`（Global）**1 箇所だけ**で、
メインウィンドウもそこに載る。窓をまたぐドロップの解決（カーソル位置と各窓の
bounds のヒットテスト）と最小化 / クローズ追従が同じ表を引く。

**別ウィンドウを読む・開くのは他ウィンドウの update の中からできない。**
window bounds の読み取りと `open_window` は `cx.defer` で 1 サイクル後に回す
（`window_host` の detach 解決と `close` がその形）。

### やってはいけないこと

- `gpui_component::dock` から何かを import する（カットオーバー済み）
- パネルに `Panel` trait 相当のものを実装する / タブのラベルをパネル側で描く
- `DockRoot` の中でモデルを書き換える
- レイアウトツリーをホストからパネルへ渡す（パネルは自分の `PanelInstanceId` と
  durable Global だけを知っていればよい）

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
        // フォントは必ず fonts ヘルパー経由。テーマのファミリに
        // 日本語フォールバック（Noto Sans JP）が付く。数値・リードアウトは
        // `fonts::mono_font(cx)`。family をベタ書きすると canvas だけ
        // 別フォントになる（要素ツリーと違い継承が効かない）。
        font: crate::fonts::ui_font(cx),
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

### Canvas のポインタカーソル

canvas 内の描画物は GPUI element ではないため、hover の `CursorStyle` はパネルの
入力状態として解決する。

1. click と drag が使う既存ヒットテストから小さな `PointerHint` enum を返す。
   hover 専用の bounds 計算を増やさない。
2. `PointerHint -> CursorStyle` は副作用のない写像にし、テストする。
3. `on_mouse_move` はヒントが変わったときだけ `cx.notify()` する。ドラッグ中は
   hover ヒントを更新しない。
4. idle のカーソルは interaction surface の `.cursor(...)` に設定する。
   ドラッグ中は canvas の paint フェーズで
   `window.set_window_cursor_style(...)` を呼び、対象の外へ出ても維持する。

カーソルは「ここで何ができるか」の約束なので、未実装の操作には付けない。
Viewer の装飾だけの bbox ハンドルや dead な Hand / Zoom がその例。

### Viewer オーバーレイを追加する

Viewer の重ね描きは `ViewerOverlay` と `OverlayRegistry` に集約する。
`render()` の canvas クロージャやマウスハンドラへ個別の分岐を足さない。

1. `crates/ravel-app/src/panels/viewer/overlay.rs`、または Field / Geometry の
   ようにまとまりがある場合は `panels/viewer/` の専用モジュールに
   `ViewerOverlay` を実装する。`OverlayId` と描画順・ヒットテスト優先度を
   決め、`OverlayRegistry::builtin()` に登録する。
2. `is_active` は選択・ツール・表示条件だけで決める。評価結果が必要な場合も、
   結果の到着を `is_active` に含めない。`eval_targets` が返す対象を
   `ProjectState` が既存の viewer 評価要求へまとめるため、未到着の結果は
   `paint` / `labels` 側で何も描かない。
3. 描画は `OverlayPainter` の composition-space API を使い、固定 px の
   ハンドルやルールは screen-space API を使う。`paint` と `handles` は同じ
   resolved data から座標を作り、描いた印と掴める位置をずらさない。文字は
   `labels` から返す（canvas の painter で直接文字を描かない）。
4. ハンドルを持つ場合は `handles` と `drag` を同じオーバーレイに実装する。
   ドラッグは `OverlayEdit` に変換し、Document の live apply / mouse-up commit
   の既存経路へ渡す。1 ジェスチャ 1 undo、Escape で revert とし、接続で駆動
   される値や結果未到着の値を編集対象にしない。
5. 入力の競合は registry の優先度に任せる。新しいハンドルを専用の
   `on_mouse_down` 分岐へ割り込ませず、描画ツールの押下を奪わない条件を
   `is_active` に書く。既存の pointer hint からカーソルを割り当て、動作の
   ない印にはカーソルを付けない。
6. 表示名やツールバーのトグルを追加する場合は、`assets/locales/en.toml` と
   `ja.toml` の両方へキーを追加する。オーバーレイにトグルが無い設計なら、
   locale にトグルを作って「表示を切り替えられる」ように見せない。
7. 座標変換、優先度、ズーム不変の印、未評価時の空描画、編集の undo 単位を
   純粋なユニットテストで固定する。GPUI 統合テストは実際の focus・Action・
   入力ルーティングが必要な経路に限る。実装済みの範囲と制約は
   `docs/ui-impl-status.md`、意図した挙動は `docs/specifications/ui/viewer.md`
   に同期する。

**オーバーレイにできないもの**: `OverlayPainter` が知るのはコンプ矩形だけで、
ズームインすると矩形はパネルから出る。パネルの縁に貼り付くクロム（定規）は
オーバーレイでは書けないので、チェッカーボードと同じ `(panel, frame)` の組から
キャンバスの描画クロージャで描く。線の掴み（ガイド）も同じで、`OverlayHandle` は
「点からの半径」しか表現できないためパネル側の押下分岐が受ける。どちらの場合も
**ツールで条件を絞って描画ツールの押下を奪わない**（Viewer の定規とガイドは
`Select` 限定）。純粋関数へ切り出して headless で試験する点は変わらない。

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

## ダイアログ (gpui-component Dialog)

`window.open_dialog(cx, |dialog, window, cx| ...)` / `open_alert_dialog` で開く
（`WindowExt` トレイトの import が必要）。

**重要**: `Root` は view / tooltip / native menu overlay しか描かない。
ホストの root render に**モーダル層を子として置かないとダイアログは開いている
のに見えない**:

```rust
let dialog_layer = Root::render_dialog_layer(window, cx);
let notification_layer = Root::render_notification_layer(window, cx);
div().size_full()
    .child(/* 通常の UI */)
    .children(dialog_layer)
    .children(notification_layer)
```

**素の `Dialog` は `button_props` を描かない**（OK/Cancel の footer を組み立てて
いるのは `AlertDialog` 側）。自前で footer を作る:

```rust
dialog.title(title).w(px(360.0))
    .content(move |body, _, _| body.child(form.clone()))   // Entity<V: Render> を子に
    .footer(
        DialogFooter::new()
            .child(Button::new("cancel").label(t!("ui.cancel"))
                .on_click(|_, window, cx| window.close_dialog(cx)))
            .child(Button::new("ok").primary().label(t!("ui.ok"))
                .on_click(move |_, window, cx| { /* 確定処理 */ window.close_dialog(cx); })),
    )
```

- `content` のクロージャは毎 render 呼ばれるので、入力ウィジェットは外側の
  Entity（フォームビュー）に持たせてクローンで渡す。
- フォームは確定時に値を返すだけにして、ドキュメントに書くのは OK 押下時のみ
  （キャンセルで undo ステップが残らない）。
- フォーム側のコンストラクタで `focus` を取らない。ダイアログの focus trap に
  任せる（テストでは `Root` が無いと `InputState::focus` がパニックする）。

### インライン入力の確定・取消

`InputState` の `InputEvent` は `Change` / `PressEnter` / `Focus` / `Blur` だけ。

- **Escape のイベントは無い** → 取消は自前で拾う必要がある
- **Enter の action は購読側に届かないことがある**（Properties の名前入力でも
  Enter では確定しない）。確定は `InputEvent::Blur` に頼るか、要素側で
  `on_key_down` して `keystroke.key == "enter" / "escape"` を見る
  （`.agents/rules/gpui.md` がテキスト入力に認めている生キー処理。
  `scripts/lint-patterns.allow` に理由を書く）
- 実機自動化の注意: cliclick / System Events の**合成 Return は GPUI に
  届かない**（`Cmd+K` のような修飾付きコードは届く）。Enter / Escape の
  確認は物理キーで行う

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

### FocusedPanelGlobal は本物のフォーカスイベントだけから書く

`FocusedPanelGlobal(Option<PanelInstanceId>)` は「ユーザーがいま作業している
パネルインスタンス」を指す。`Cmd+Shift+D`（detach）と `Cmd+Shift+R`（reattach）は
これを起点にウィンドウを解決するので、設定していないパネルはこの 2 つのコマンドに
反応しない。

書き込みは `track_panel_focus(instance, &focus_handle, window, cx)`
（`panels/mod.rs`）に任せる。`on_focus_in` / `on_focus_out` を購読して
Global を張り替える 2 本の `Subscription` を返すので、パネルはそれを保持する。

```rust
// NG — クリック履歴でフォーカスを決め、毎フレーム奪い返す
.on_mouse_down(MouseButton::Left, move |_e, window, cx| {
    focus.focus(window, cx);
    cx.set_global(FocusedPanelGlobal(Some(instance)));
})

// OK — focusable だと宣言し、状態は本物のフォーカスイベントから同期する
let subs = track_panel_focus(instance, &focus_handle, window, cx);
div().track_focus(&focus_handle)
```

`render()` でフォーカスを触らないこと、クリックハンドラから Global を書かないことは
規約（[`.agents/rules/gpui.md`](../.agents/rules/gpui.md)）。

### フォーカスを動かすのはホストだけ — シェルは実フォーカスの写しを持つ

`AppShell::focused_instance()` は**シェルの意見ではなく実フォーカスの写し**。
書き込むのは `RavelWorkspace::dispatch_command` が毎回頭で呼ぶ
`set_focused_instance(FocusedPanelGlobal)` だけで、detach / reattach の対象は
その値から決まる。**ヘッドレス側で「ここを focused にする」と書いてはいけない** —
実フォーカスと 2 系統で動き、画面と食い違う（`MED-APP-24`）。

パネルを開くコマンドのようにフォーカスを移したい場合は、シェルは
`CommandOutcome` でホストに伝え、ホストが**本物のフォーカス移動**を行う。

```rust
// シェル: 挿入したインスタンスを報告するだけ（self.focused は書かない）
CommandOutcome::OpenPanel { instance }

// ホスト: 実フォーカスを渡す。focus イベントが FocusedPanelGlobal を張り替え、
// その値が次の dispatch でシェルへ戻る
CommandOutcome::OpenPanel { instance } => window_host::focus_pane(instance, cx),
```

`window_host::focus_pane` は `WindowRegistry` からそのインスタンスを描いている
ウィンドウを引き、`PanelViews::focus_pane` を呼ぶ。**ウィンドウの update 中に
別ウィンドウ（や自分自身）を update すると失敗する**ので中身は `cx.defer` 済み。
まだ描かれていないペインでもビューを作って focus できるので、挿入直後でも通る。

### フォーカス中のパネルの表示

**タブバーはドックが描く。** どのペインがフォーカスを持っているかの印
（タブアイコンの明度）は `ravel-dock` のタブバー側の仕事で、ホストが
`FocusedPanelGlobal` を observe して再描画する。**パネル側にフォーカス表示の
コードは要らない**（自前のヘッダを描いているパネルがあってもそれはパネルの
中身の話）。

### 「フォーカス中インスタンスへのハンドル」

`TimelinePanelHandle` / `NodeEditorHandle` のような単一ハンドルの Global は、
多重インスタンスの世界では「ユーザーが作業しているインスタンス」を指す約束に
なっている。生成時に最新インスタンスを入れ、`on_focus_in` で張り替える
（`track_focused_handle`）。

```rust
let handle_sub = track_focused_handle(&focus_handle, window, cx, NodeEditorHandle);
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

gpui-flow を `Entity<FlowGraph>` としてドックのペインに置くと、`text_color` の cascade が Entity 境界をまたがない。`gpui-component::Label` も `cx.theme().foreground` をハードコードしている。

**正しいアプローチ**: 外部ライブラリの Entity 埋め込みではなく、パネル内で canvas ベースの直接描画を行い、テーマ色は `cx.theme().colors` から取得して paint 内で使用する。
