# パネル配置と View トグル 実装計画（#181）

> **Status**: Superseded by `done/free-pane-docking-plan.md` — 2026-07-31
>
> フリードッキング再設計（REQ-UI-005 v2）が本計画の課題意識を吸収した。
> 「実効レイアウトの分離」（旧 PANEL-1）と「既定ドックスロット挿入」
> （旧 PANEL-2）は `done/free-pane-docking-plan.md` の DOCK-2 が新レイアウト
> モデル上で実装する。旧 PANEL-1〜3 は未着手のまま取り下げ。
> 以下は取り下げ時点の内容（由来の記録）。

対象: Issue #181「View トグルはプリセットがそのパネルを配置している
ワークスペースでしか効かない」。関連要件: REQ-UI-013（ワークスペース /
パネル管理）、REQ-UI-001。

`done/attribute-spreadsheet-plan.md` はこの計画の完了を前提にする（新規パネルが
どのプリセットのレイアウトツリーにも無い状態で追加されるため）。

## 問題

`RavelWorkspace::rebuild_layout`（`crates/ravel-app/src/workspace.rs:1236`）は
**アクティブプリセットのレイアウトツリーを可視性でフィルタしているだけ**。

```rust
let layout = self.shell.presets().active().layout.clone();
let visibility = self.shell.visibility().clone();
let new_center = build_dock_item(&layout, &visibility, ...);
```

`PanelVisibility`（`crates/ravel-ui/src/panel.rs:128`）は
`BTreeMap<PanelKind, bool>` でしかなく、**ツリーに存在するノードの
表示/非表示しか表現できない**。ツリーに無いパネルには置き場所が無いので、
`visibility.set(kind, true)` にしても `build_dock_item` が到達しない。

ユーザーから見ると「メニュー項目にチェックが付くのに何も現れない」。
現状 16 個の `PanelKind` に対し、各プリセットが配置しているのは数個なので、
**View メニューの大半の項目が無反応**。#180 は MediaBin を Edit プリセットの
ツリーに足すことで個別に回避したが、制限そのものは残っている。

## 決定事項

### レイアウトの持ち主をプリセットから shell に移す

プリセットの `layout` は「**初期値**」に降格し、shell が
`effective_layout: LayoutNode` を保持する。`rebuild_layout` は
effective layout を読む。プリセット切替は effective layout をその
プリセットの初期値でリセットする（＝現在の挙動と一致）。

これで「プリセットに無いパネルを足す」が**ツリーの編集**として表現できる。

### 挿入先は `PanelKind` ごとの既定ドックスロット

`LayoutNode` は `Leaf` / `Split` の 2 種しか無く、タブ共有を表現できない。
タブ変種の追加は dock 側の再設計になるため本計画では持ち込まない。

代わりに `PanelKind::default_slot() -> DockSlot`（`Left` / `Right` /
`Bottom` / `Center`）を定義し、ツリーに無いパネルをトグルしたときは
**ルートをそのスロット方向へ分割して Leaf を挿入**する。既定比率は
スロットごとの定数（サイド 0.2、ボトム 0.3）。

- 消すときは Leaf を削除し、残った片側で `Split` を畳む。
- 既にツリーにあるパネルのトグルは**従来どおり可視性フラグだけ**を触る
  （プリセットが意図した配置を壊さない）。

### 永続化は本計画に含めない

ワークスペースのレイアウトはプロジェクトではなく**ユーザー**に属するので、
`ui_state.json`（プロジェクトアーカイブ内）ではなく
`paths::global_settings_path()`（`<config>/ravel/settings.toml`）が
置き場所として正しい。

設定の器は既にある。`crates/ravel-project/src/settings.rs` が
`default → global → project → user` の 4 層マージと TOML の
シリアライズ/デシリアライズを実装済み。**欠けているのはグローバル層の
ディスク I/O だけ**で、`Project::resolved_settings` は global 層を
`Option<&SettingsLayer>` 引数で受け取るが、production の呼び出し元が無く
（テストのみ）、`global_settings_path()` も production から呼ばれていない。

つまり永続化は当初の想定より安い。それでも本計画には含めない — グローバル
設定層の配線は「レイアウト」以外の設定にも効く独立した仕事で、
パネル配置の修正と混ぜると両方の完了条件がぼやける。本計画は
「セッション内でトグルが効く」までを対象にする。アプリ再起動でプリセット
初期値に戻るのは、現状（そもそも現れない）より劣化しない。

## 実装単位

### 単位 1: 実効レイアウトの分離（挙動不変のリファクタ）

- `ShellState` に `effective_layout: LayoutNode` を追加。
  `PresetLibrary::active().layout` から初期化。
- プリセット切替コマンドが effective layout をリセット。
- `rebuild_layout` が `self.shell.effective_layout()` を読む。

**完了条件**

- 既存のワークスペース/プリセットのテストが無改変で通る。
- プリセット切替 → レイアウトが切替先の初期値になるテスト。

### 単位 2: 既定ドックスロットと挿入/削除

- `crates/ravel-ui/src/panel.rs`: `DockSlot` enum と
  `PanelKind::default_slot()`。16 種すべてに割り当てる。
- `crates/ravel-ui/src/preset.rs`: `LayoutNode::insert_at(slot, panel, ratio)`
  と `LayoutNode::remove(panel)`（空 `Split` の畳み込み込み）。
- `ShellState::toggle` が「ツリーに在る/無い」で分岐する。

**完了条件**

- 空ツリー / 単一 Leaf / 深いネストへの挿入を網羅する `LayoutNode` の
  ユニットテスト（`ravel-ui`、ヘッドレス）。
- 挿入 → 削除でツリーが元に戻る往復テスト。
- 各プリセットで 16 パネル全てをトグル → 表示されることを検証する
  テーブル駆動テスト。**これが #181 の回帰テスト。**

### 単位 3: 実機確認と文書更新

- 全プリセット × 全 View トグルを実機で確認（cliclick）。
  特に #181 が名指しした Dopesheet（Edit プリセット）と MediaBin
  （Edit 以外）。
- `docs/specifications/ui-spec.md`: プリセットの `layout` が初期値である
  ことと既定ドックスロットの表を追記。
- Issue #181 に、永続化が未対応で残る旨を書いてクローズ。

**完了条件**

- `mise run check` が通る。
- 実機でトグルが全プリセットで効く。

## 非対象

- **レイアウトの永続化**。グローバル設定層（`settings.toml` の読み書き）の
  新設が必要で、独立スコープ。本計画完了後に別計画とする。
- **タブ共有**（`LayoutNode::Tabs`）。Dopesheet / CurveEditor の
  タブ共有は現状 dock 側の別機構で扱っており、レイアウトツリーには
  出てこない。統合は別課題。
- **パネルのドラッグ＆ドロップ再配置**。gpui-component の dock が持つ
  機能との擦り合わせが要るため別課題。
- **フローティングパネル**。
