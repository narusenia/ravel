# UI 仕様書の分割と現状合わせ 実装計画

> **Status**: Complete — PR #213, 2026-07-30

対象: `docs/specifications/ui-spec.md` をビュー単位に分割し、記述を実装に
合わせる。関連要件: REQ-UI-001〜013。

## 問題

`ui-spec.md`（361 行）は v1 設計のまま止まっており、**冒頭の注記が但し書きして
いる範囲より広く食い違っている**。2026-07-30 に全節を実装と突き合わせた結果:

| 節 | 記述 | 実装 |
|---|---|---|
| 設計原則 | 「タイムラインは Sequence ノードの糖衣 UI」「Timeline の二面性」 | Sequence ノードは存在しない。AE 型 Composition / Layer |
| シーンモデル / ワークフロー | 図と 3 つの流れがすべて Sequence 前提 | 同上 |
| サブグラフ | `Ctrl+G` で Group 化 | Group 化コマンドは `CommandId` に無い（`NETIF-5` 待ち） |
| パネル一覧 | Dopesheet / Curve Editor を独立パネルとして既定表示 | Curve Editor は Timeline 内のモード切替。Dopesheet は `PlaceholderPanel` |
| ノードグラフ / パン | 「中ボタン / **Space+左ドラッグ**」 | 中ボタン + **Alt+左ドラッグ**（Space は再生） |
| ノードグラフ / 移動 | 「スナップガイド表示」 | グリッドスナップのみ（**10px**。`ui-impl-status.md` の「20px」も古い） |
| ノードグラフ / 追加 | Tab / ダブルクリックで検索パレット + 詳細仕様 | 未実装（右クリックメニューのみ） |
| ノード表示の図 | インラインパラメータがスライダ | `key: value` のテキスト |
| ビューア / 表示ノード切替 | 「ノードグラフで Alt+クリック」 | **不採用として削除済み**。Alt+クリックはパン |
| ビューア / フィット | 「フィット: F」 | Viewer に F のバインドは無い（F で Fit するのは NodeEditor） |
| ビューア詳細 | ツールシステムの記述が無い | 6 ツール + bbox + パス編集 + シェイプ描画が実装済み（REQ-UI-011） |
| テーマシステム | TOML（`[colors] background/surface/primary`、`color_vision`） | **形式ごと違う**。gpui-component の JSON スキーマ（`assets/themes/ravel.json`） |
| キーバインド | 例示に `[composition]` / `[playback]` が無い、`import` 欠落 | 実物は 9 セクション。末尾の TASK 参照は archive 世代 |
| 制約 | 「GPUI 0.2.2」「TASK-037 後に国際化」「関連要件 REQ-UI-001〜010」 | gpui-ce の rev 固定。REQ-UI-011/012/013 が抜けている |

一致していたのは **Outliner 詳細**（REQ-UI-013 準拠）と、タブグルーピング未実装の
制約記述だけ。

**1 ファイルであることが古さの原因になっている。**パネルを 1 つ触るたびに
361 行のどこを直すべきか探すことになり、結果として誰も直さない。

## 決定事項

### ビュー単位に分割し、旧パスは索引として残す

```text
docs/specifications/ui-spec.md   ← 薄い索引（設計原則 / パネル一覧 / 制約 / 各ビューへのリンク）
docs/specifications/ui/
  workspaces.md    ワークスペースプリセットとレイアウト
  outliner.md
  node-editor.md
  timeline.md
  viewer.md
  properties.md
  media-bin.md
  theme.md
  keybindings.md
```

旧パスを残すのは**参照が 22 箇所あり、Rust の doc コメント
（`ravel-ui/src/lib.rs:20`、`preset.rs:210`）も含むから**。すべてアンカー無しの
ファイル参照なので、索引化すれば全部生きたままになる。`CLAUDE.md` →
`AGENTS.md` と同じ形。

### 仕様書は「設計意図」、実装状況は `ui-impl-status.md`

両方に同じ表を持たせない。仕様書は「こう動くべき」を書き、実装済みかどうかは
`docs/ui-impl-status.md` を正とする。ただし
`.agents/rules/documentation.md` の「計画中の機能を実装済みとして書かない」を
守るため、**仕様書側でも未実装項目には明示的に印を付ける**（節の冒頭に
「未実装。担当は `<plan>`」の 1 行）。

### v1 の記述は移設せず、破棄する

Sequence ノード / Track・Clip の記述は「参考情報」としても残さない。
`AGENTS.md` が「古い計画文書が実装と食い違う場合は実装を正とする。特に
Track/Clip モデルが現行だと仮定しないこと」と明記しているものを、仕様書側が
繰り返し提示している状態を終わらせる。

歴史的経緯が必要なら `docs/implementation/archive/` を見れば足りる。

### 未実装パネルは「パネル一覧」に残すが、状態を書く

Scopes 4 種 / Text Editor / Render Queue / Shader Editor / Lua Console は
`PlaceholderPanel` だが、ワークスペースプリセット（`assets/workspaces/*.toml`）が
実際に配置しているので一覧からは消さない。**状態列を足す。**

## 実装単位

### UISPEC-1: 骨組みと索引

- `docs/specifications/ui/` を作る
- 旧 `ui-spec.md` を索引に置き換える: 設計原則（Composition / Layer 版）、
  パネル一覧（状態列つき）、制約・前提条件、各ビューへのリンク
- Sequence ノード / Track・Clip / ワークフロー別の流れの v1 記述を破棄する
- 制約節を更新（gpui-ce の rev 固定、TASK 参照の除去、関連要件に
  REQ-UI-011/012/013 を追加）

### UISPEC-2: `viewer.md`

- ツールシステム（6 ツール、一時ハンド、ツールバー）
- 選択とヒットテスト、bbox の表示規約（**8 ハンドルは現状飾り**であることを
  含む。`OVL-7` が動作を与える）
- オーバーレイ 5 種（グリッド / セーフエリア / bbox / パス編集 / 評価エラー）
- コントロールの実物合わせ（Alt+クリックの表示ノード切替は削除、フィットは
  ツールバー、ズームは Cmd/Ctrl+スクロール）
- 未実装項目の印: Hand / Zoom（`TOOLX-1`）、スコープ（`INSP-5`）、
  チャンネル表示（`INSP-2`）、吸着とガイド（`SNAP-*`）

### UISPEC-3: `node-editor.md`

- インタラクションの実物合わせ（パンは中ボタン / Alt+左、ズームは
  Cmd/Ctrl+スクロールとピンチ、矩形選択は Shift+ドラッグ、10px グリッドスナップ）
- ノード表示（`key: value`、ポート色、bypass の半透明、評価時間の表示）
- ネットワークコンテキストとパンくず、synthetic ノードの非表示
- 未実装項目の印: 検索パレット（`DISC-3`）、ホバー Popover（`DISC-2`）、
  Group 化（`NETIF-5`）、ミニマップ、スナップガイド

### UISPEC-4: `timeline.md`

- Composition / Layer モデルでの記述に置き換える（レイヤーバー、トリム、
  レイヤーヘッダーのトグル、キーフレーム行、プロパティツリー）
- Dopesheet / Curve のモード切替（独立パネルではない）
- ルーラーとスクラブ、フォロー再生、ズームアンカー
- 未実装項目の印: 行の仮想化（`MED-UI-03`）、キャッシュ帯（`CACHE-6`）、
  縦ズーム（`PARAM-5`）

### UISPEC-5: `outliner.md` / `properties.md` / `media-bin.md`

- Outliner は現状の記述がほぼ正しいので移設が中心
- Properties: ターゲット 4 種（Layer / Node / Composition / MediaAsset）、
  キーフレーム菱形、ポートトグル、スクラブ入力
- MediaBin: 種別フィルタ、検索、サムネイル、オフライン表示、行の操作
- 未実装項目の印: Parent 行（`SHELL-5`）、Relink（`MEDIA-6`）、
  Vector の成分ラベル（`VEC-5` / `MED-APP-20`）

### UISPEC-6: `theme.md` / `keybindings.md` / `workspaces.md`

- テーマ: gpui-component の JSON スキーマに合わせる（`assets/themes/ravel.json`、
  ドット区切りキー、light / dark の 2 モード）。TOML の架空スキーマを破棄
- キーバインド: 実物の 9 セクションと修飾子規約。ユーザー上書きは未実装
  （`SET-5` / `LOW-APP-15`）
- ワークスペース: 4 プリセットの実配置（`assets/workspaces/*.toml`）と
  `LayoutNode::Tabs` 未実装の制約

### UISPEC-7: 参照の確認

- 22 箇所の参照が索引経由で成立していることを確認する
- `docs/ui-impl-status.md` の古い記述を直す（NodeEditor の「20px グリッド
  スナップ」は実装は 10px、「ハンドル付き bbox」はハンドルが動作しない）
- `docs/specifications/README.md` があれば索引を追加、無ければ作らない

## 検証

- 文書のみの変更なので `mise run check` は不要（実行するなら `lint:patterns` まで）
- 各単位で「実装のどの行を根拠にしたか」を PR 本文に残す
- 未実装項目に印が付いていること、印の担当計画が実在することを目視確認

## 非対象

- **ポインタフィードバックの節**。`done/pointer-feedback-plan.md` の `PTR-6` が
  実装と同時に書く（先に書くと未実装を実装済みとして書くことになる）
- **要件（REQ-UI-*）の書き換え**。仕様書と要件は別物で、要件の受入条件は
  各機能計画が更新する
- **`docs/ui-impl-status.md` の構成変更**。古い記述の修正だけ行う
- **英語版の作成**。仕様書は日本語のまま
