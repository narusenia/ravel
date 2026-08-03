# キーバインド 仕様

> 最終更新: 2026-07-30 ／ 索引: [`../ui-spec.md`](../ui-spec.md)

関連要件: REQ-UI-007。

## 形式

`assets/keybindings/*.toml`。セクションごとのテーブルで、キー = コマンドの
アクション、値 = キーコード。**コマンド id は `<section>.<action>` で
`ravel_ui::command::CommandId` と一致必須**（不一致はロード時に検出する）。

修飾子トークン:

- `Cmd` — プラットフォーム主修飾（macOS では Cmd、Windows / Linux では Ctrl）
- `Ctrl` — 物理 Control キー
- `Shift` / `Alt`（Option）
- チョードは `+` 区切りで修飾子を先頭、キーを末尾に置く（`Cmd+Shift+Z`）

## 同梱の既定（`default.toml`）

```toml
[meta]
name = "Ravel Default"
author = "Ravel Team"

[app]
preferences = "Cmd+,"

[file]
new = "Cmd+N"
open = "Cmd+O"
import = "Cmd+I"
save = "Cmd+S"
save_as = "Cmd+Shift+S"
quit = "Cmd+Q"

[edit]
undo = "Cmd+Z"
redo = "Cmd+Shift+Z"
cut = "Cmd+X"
copy = "Cmd+C"
paste = "Cmd+V"

[view]
toggle_timeline = "Alt+1"
toggle_node_graph = "Alt+2"
toggle_viewer = "Alt+3"
toggle_properties = "Alt+4"
toggle_curve_editor = "Alt+5"
toggle_scopes = "Alt+6"

[composition]
settings = "Cmd+K"

[playback]
toggle = "Space"
stop = "K"
step_forward = "Right"
step_backward = "Left"

[workspace]
edit = "Cmd+F1"
node = "Cmd+F2"
color = "Cmd+F3"
motion = "Cmd+F4"

[panel]
detach = "Cmd+Shift+D"
reattach = "Cmd+Shift+R"

[help]
about = "F1"
```

## アセットに無いバインド

次のものは**アセットではなくコード側（`workspace.rs`）でキーコンテキスト付きに
登録している**ため、上記の TOML には現れない。

| バインド | コンテキスト |
|---|---|
| `V` / `P` / `R` / `E` / `H` / `Z`（ツール切替） | Viewer |
| `F`（ビューをフィット） | Node Editor |
| `Tab`（ノード検索パレット） | Node Editor |
| `Delete` / `Backspace`（削除） | Node Editor、Timeline |
| キーフレーム補間の切替 | Timeline |

パネル固有のショートカットはキーコンテキストに束縛するのが規約
（`.agents/rules/gpui.md`）。アセット由来のバインドを**コンテキスト無しで
登録している不具合**があり、テキスト入力から矢印キーを奪う
（`MED-APP-16`。フェーズ A で修正済みの扱い）。

## 未実装項目

| 項目 | 担当 |
|---|---|
| ユーザーによる上書き（キーバインドエディタと永続化） | `SET-5` / `LOW-APP-15` |
| NLE プリセット（Premiere / FCP 風）の同梱 | 未計画（REQ-UI-007 が要求） |
| コンテキスト付きバインドをアセットで表現する | `SET-5` と同時に決める（現在はコード側にしか書けない） |
