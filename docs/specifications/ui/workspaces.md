# ワークスペースプリセットとレイアウト 仕様

> 最終更新: 2026-07-30 ／ 索引: [`../ui-spec.md`](../ui-spec.md)

関連要件: REQ-UI-005、REQ-UI-009、REQ-UI-013。

## 形式

`assets/workspaces/*.toml`。`name` はロケールキーで、`layout` は
`split` / `leaf` の再帰構造。

```toml
name = "workspace.preset.node"

[layout]
type = "split"
orientation = "horizontal"
ratio = 0.82

[layout.first]
type = "split"
orientation = "vertical"
ratio = 0.35

[layout.first.first]
type = "leaf"
panel = "outliner"
```

- `split` は `orientation`（`horizontal` / `vertical`）と `ratio`、`first` / `second`
- `leaf` は `panel`（パネルの識別子）
- **`Tabs` variant は存在しない** → プリセットでタブ統合を表現できない
  （下記「制約」）

## 同梱プリセット

| プリセット | 配置されるパネル |
|---|---|
| Edit | Outliner, Media Bin, Viewer, Node Graph, Timeline, Properties |
| Node | Outliner, Viewer, Node Graph, Dopesheet, Properties |
| Color | Viewer, Node Graph, Waveform, Vectorscope, Histogram, Parade, Dopesheet |
| Motion | Outliner, Viewer, Node Graph, Text Editor, Properties, Dopesheet |

Dopesheet / Scopes 4 種 / Text Editor は `PlaceholderPanel`（[パネル一覧](../ui-spec.md#パネル一覧)）。
プリセットは配置するので、開くとプレースホルダが出る。

## 実行時の操作

- パネルはドラッグで移動・タブ統合・引き剥がしができる（gpui-component DockArea）
- プリセットの切り替えは `Cmd+F1`〜`F4`
- パネルのデタッチ（別ウィンドウ）とリアタッチは `Cmd+Shift+D` / `Cmd+Shift+R`
- アクティブコンポジションは `ui_state.json` に永続化する（欠落時は
  `Document.root_comp` にフォールバック）

## 制約と未実装項目

| 項目 | 状態 |
|---|---|
| プリセットでのタブ統合 | 🔲 `LayoutNode` に `Tabs` variant が無いため、プリセットでは片方のパネルのみ配置する |
| カスタムワークスペースの保存 / 復元 | 🔲 未実装（REQ-UI-005 の受入条件）。担当は `panel-placement-plan.md` |
| アクティブプリセットが配置しないパネルの表示トグル | 🔲 `PANEL-1〜3`（`panel-placement-plan.md`、#181） |
| 分離ウィンドウの配置永続化 | 🔲 `LOW-APP-14`（未達の契約） |
| デタッチウィンドウのタイトルの国際化 | 🔲 英語ハードコード（`LOW-APP-11`） |
| 実効レイアウトの分離（挙動不変のリファクタ） | 🔲 `PANEL-1` |
