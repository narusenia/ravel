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
- 分離ウィンドウは共通のウィンドウホスト（`window_host::WindowHost`）が描く。
  タイトルバー + `ravel-dock` のレイアウトツリー + ダイアログ / 通知レイヤー。
  クローズボタンはそのウィンドウのインスタンスを破棄する（メインへ自動で
  戻らない。戻すのは `Cmd+Shift+R`）。メインウィンドウを閉じると分離
  ウィンドウも閉じ、最小化 / 復帰にも追従する
- タイトルバーは全ウィンドウ共通のコンポーネント（`title_bar::RavelTitleBar`）。
  中央にウィンドウのラベル（メイン = プロジェクト名、分離 = パネル名。
  複数パネルを持つ分離ウィンドウは `window.panels` の「N 個のパネル」）を置き、
  窓種別のスロットに要素を差す（メイン = アプリ名、分離 = AlwaysOnTop ピン）。
  プリセット切替はキーバインドと Workspace メニューのみで、バーには置かない
- AlwaysOnTop は分離ウィンドウごとに独立で、ピンの状態は
  `WindowLayout.always_on_top` が持つ。ウィンドウを開くときにも適用する
  （セッション内のみ。ファイルへの保存は未実装 — 下表）
- アクティブコンポジションは `ui_state.json` に永続化する（欠落時は
  `Document.root_comp` にフォールバック）

## 制約と未実装項目

このドッキング全体は `docs/implementation/free-pane-docking-plan.md`
（REQ-UI-005 v2 / REQ-UI-009 v2）で独自実装（`ravel-dock`、多重インスタンス、
全ウィンドウ同型、レイアウト永続化）へ置き換える計画がある。下表の担当単位は
同計画のもの。

| 項目 | 状態 |
|---|---|
| プリセットでのタブ統合 | 🔲 `LayoutNode` に `Tabs` variant が無いため、プリセットでは片方のパネルのみ配置する。`DOCK-1` |
| カスタムワークスペースの保存 / 復元 | 🔲 未実装（REQ-UI-005 の受入条件）。担当は `DOCK-9` |
| アクティブプリセットが配置しないパネルの表示トグル | 🔲 `DOCK-2`（#181。旧 `panel-placement-plan.md` を supersede） |
| 分離ウィンドウの配置永続化 | 🔲 `LOW-APP-14`（未達の契約）。担当は `DOCK-9` |
| AlwaysOnTop の永続化 | 🔲 セッション内のみ。`layout.toml` への保存は `DOCK-9` |
| 分離ウィンドウをクローズボタンで閉じるとシェルが desync | ✅ `DOCK-6` で解消（クローズは `AppShell::close_window` を通り、ハンドル表からも消える）。`MED-APP-01` の起票は `DOCK-10` で締める |
| 同一パネルの複数表示 | 🔲 パネルはシングルトン。多重インスタンス化は `DOCK-1〜2` |

（旧記載の「デタッチウィンドウのタイトルが英語ハードコード」は解消済み —
`window_host` は `panel_display_name`（ロケールキー）を使っている。）
