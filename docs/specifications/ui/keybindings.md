# キーバインド 仕様

> 最終更新: 2026-07-30 ／ 索引: [`../ui-spec.md`](../ui-spec.md)

関連要件: REQ-UI-007。

## 形式

`assets/keybindings/*.toml`。セクションごとのテーブルで、キー = コマンドの
アクション、値 = キーコード。**コマンド id は `<section>.<action>` で
`ravel_ui::command::CommandId` と一致必須**（不一致はロード時に検出する）。

ユーザーは同じ形式の `<config>/ravel/keybindings.toml` で上書きできる。起動時に
既定へ重ね、同じコマンドを別 chord に割り当てると既定の chord は外れる。
既定アセットの誤りはテストで落とす（厳格）が、ユーザーファイルの解釈できない行は
その行だけ警告して捨てる（寛容）。

ユーザーファイル内で同じ chord を 2 つのコマンドに割り当てた場合は、
**`<section>.<action>` の昇順で先のものが chord を保持**し、もう一方は警告して
捨てる（負けた側は既定の chord を保つ）。TOML の反復順ではなく明示的なソートで
決めているので、規則としてテストで固定されている。

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
toggle_outliner = "Alt+7"
toggle_media_bin = "Alt+8"

[composition]
settings = "Cmd+K"

[project]
exposed_parameters = "Cmd+Shift+K"

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

次のものは**アセットではなくコード側でキーコンテキスト付きに登録している**ため、
上記の TOML には現れない。実体は `workspace.rs` の `PANEL_BINDINGS` という
**1 つの表**で、GPUI への登録と環境設定の一覧の両方がそれを読む。

| バインド | コンテキスト |
|---|---|
| `V` / `P` / `R` / `E` / `H` / `Z`（ツール切替） | Viewer |
| `Cmd+D`（複製） | Node Editor、Timeline |
| `F`（ビューをフィット） | Node Editor |
| `L`（自動整列） | Node Editor |
| `Tab`（ノード検索パレット） | Node Editor |
| `Delete` / `Backspace`（削除） | Node Editor、Timeline |
| `U` / `A` / `P` / `S` / `R` / `T` / `L`（プロパティ行の絞り込み） | Timeline |
| `Shift+U` / `Shift+A` / `Shift+P` / `Shift+S` / `Shift+R` / `Shift+T` / `Shift+L`（絞り込みに追加） | Timeline |
| `Alt+U` / `Alt+E`（変更済み / 式を持つ行） | Timeline |
| `Alt+Shift+U` / `Alt+Shift+E`（同じく追加） | Timeline |
| `Cmd+Shift+D`（プレイヘッドでレイヤーを分割） | Timeline |
| `[` / `]`（始端 / 終端をプレイヘッドに合わせる） | Timeline |
| `I` / `O`（選択レイヤーの始端 / 終端へプレイヘッドを移動） | Timeline |

絞り込みは**修飾なしが置換、`Shift` 併用が追加**という 2 つの意味を持つが、
GPUI の Action は修飾キーを運ばないので**コマンドを 2 本に分けている**
（`timeline.reveal_position` と `timeline.reveal_position_add`）。AE の
二度押し（`UU` / `EE`）は `KeyChord` に表現が無いため `Alt+U` / `Alt+E` に
割り当てた。挙動は [`timeline.md`](timeline.md) の「行の絞り込み」節。
Viewer の `P` / `R` と Node Editor の `L` はキーコンテキストが違うので
衝突しない。

AE 相当のプレイヘッド操作（`Cmd+Shift+D` / `[` / `]` / `I` / `O`）も
コンテキスト付きで登録する。どれも Timeline のレイヤー選択とプレイヘッドを
読むので、アセット側に置くと Viewer やノードエディタからも発火してしまう。
挙動は [`timeline.md`](timeline.md) の「プレイヘッド基準のレイヤー操作」節。

**`Cmd+Shift+D` は Timeline に focus があるあいだ `panel.detach` を覆い隠す。**
gpui は深いキーコンテキストのバインドを優先するため、Timeline では分割が勝つ。
AE 型のタイムラインでこの chord が意味するのは分割であること、パネルの切り離しは
他のパネルからそのまま効くこと、`panel.detach` はアセット由来なのでユーザーが
別の chord に移せることを踏まえた割り当て。

**そのうえで `panel.detach` を View メニューに出した。** 覆い隠すからには
chord 以外の到達手段が要る。ラベルキー `menu.panel.detach` は前から
あったのにメニュー行が無く、**キーバインドだけが唯一の到達経路**だった
（この単位が塞いだ取りこぼし）。

キーフレーム補間の切替はメニューと `on_action` だけで、キーバインドは持たない。

**ユーザーファイルはこの表のコマンドを再割り当てできない。** 割り当てを受理すると
コンテキストの無いグローバルバインドになり、パネル限定という設計を黙って広げて
しまうため、該当行は警告して捨てる。どこにも束縛が無いコマンド
（`composition.new` など）に chord を付けるのは正当な用途で、拒否しない。

パネル固有のショートカットはキーコンテキストに束縛するのが規約
（`.agents/rules/gpui.md`）。アセット由来のバインドを**コンテキスト無しで
登録している不具合**があり、テキスト入力から矢印キーを奪っていた
（`MED-APP-16`。フェーズ A で修正済み）。以後アセット由来もユーザー由来も
**キーボードの所有者に道を譲る述語**付きで登録される。所有者はテキスト入力と
**開いているメニュー**の 2 種で、述語は
`!Input && !PopupMenu && !AppMenuBar`（`workspace::workspace_binding_context`
が 1 箇所で組む。文脈名は `gpui_component::menu` の定数が正）。

メニューを外したのは `MED-APP-31`。gpui は同じ深さのバインドを**登録順**で
決め（`Keymap::bindings_for_input`）、Ravel は `gpui_component::init` の後に
束縛するので、述語で降りない限りワークスペース側が勝ってしまう。
**否定文脈はその文脈がスタックにある間バインドを丸ごと無効にする**ので、
メニューが開いている間は Space も含めどのワークスペース chord も発火しない。
開いているメニューはキーボードに対してモーダル、という意図的な挙動。

**パネル固有のバインドも同じ narrowing を受ける**（`NodeEditor && !PopupMenu
&& !AppMenuBar` の形）。ポップアップは開いたパネルの子として dispatch tree に
載るので、パネルの文脈はメニューが開いている間もスタックに残る — narrowing が
無いと `L`（自動整列）がメニューの裏で走る。

アセット側にコンテキスト欄は**足していない**。パネル固有のバインドは引き続き
コード側にしかなく、ユーザー上書きの対象は既定アセットに載っているグローバルな
バインドだけ。

## 未実装項目

| 項目 | 担当 |
|---|---|
| 画面からのキー割り当ての編集（chord のキャプチャと衝突検出） | `SET-12`。環境設定にあるのは読み取り専用の一覧まで |
| NLE プリセット（Premiere / FCP 風）の同梱 | 未計画（REQ-UI-007 が要求） |
| コンテキスト付きバインドをアセットで表現する | 未計画。`SET-5` で「足さない」と決めた（パネル固有はコード側のまま、ユーザー上書きの対象外） |
