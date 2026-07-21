# ツールシステム実装計画（REQ-UI-011）

## 問題

Viewer は表示専用で、形状の作成・選択・移動はすべて Node Editor /
Properties / Timeline 経由の間接操作になっている。モーショングラフィックス
ツールとして「キャンバス上で描いて動かす」直接操作（選択/移動・ペン・
シェイプ描画）が必要。要件と設計判断は `docs/requirements/REQ-UI.md` の
REQ-UI-011 に確定済み（2026-07-21 設計セッション。ヒット粒度 a→c 段階、
Space 一時ハンドの H ホールドへの変更、殻 transform 非 identity 時の
読み取り専用原則、共有フラット化 → GPU 曲線評価の展望などの経緯込み）。

前提となる既存基盤: 選択駆動評価（Node Editor 選択ノードの playhead
フレーム評価）、`GeometricData::bounds` / `bounds_center`（core geometry
ops）、Viewer オーバーレイ（グリッド/セーフエリア）と
`ViewerViewport`（comp↔screen 変換、#129 で comp 空間 WYSIWYG 化済み）、
`layer_matrix`（殻 transform 行列）、Document スナップショット undo、
`for_each_command!` コマンドテーブル、lucide アイコン vendoring 方式。

## 目標アーキテクチャ

- **選択の正**: `CanvasSelection { path: NetworkPath, nodes: HashSet<NodeId> }`
  durable Global（ravel-ui）。Node Editor はパネル内部の `selected_nodes` を
  廃止しこれを読み書き（`selected_edges` はパネルローカルのまま）。
  `SelectedPropertiesTarget` は選択変更時の導出発行に変更なし。
  Timeline のレイヤー選択は v1 では統合しない（Outliner 設計時に再訪）。
- **ツール状態**: `ToolState`（現在ツール + 一時ハンド押下状態）
  durable Global。切替は `CommandId` + Action（`for_each_command!` に追加、
  キーバインドは Viewer の key_context スコープ）。ドラッグ処理は
  Viewer の DragMode パターンを拡張。一時ハンド（H ホールド）と
  ペンのドラッグ中モディファイアのみ生キーハンドリング可
  （transient drag mode の既存規約どおり）。
- **bbox / ヒット**: 選択ノードの評価済み Geometry から AABB
  （point P → instance P、`bounds` 規約）を取得し、殻 transform を
  順方向適用して Viewer オーバーレイに描画。ヒットテストは
  アクティブ network 内の geometry 出力ノードの AABB（逆順 = 前面優先）。
  ドラッグ移動は枠のみローカル予測、パラメータは
  `apply_document` → マウスアップで `commit_document`、Esc で revert。
- **ペンデータモデル**: `ParameterValue::PathPoints(Vec<PathPoint>)`
  （`PathPoint { p: Vec2, in_tan: Vec2, out_tan: Vec2 }`、接線ゼロ =
  コーナー）。`ParameterValue` への variant 追加は RON / bincode 両直列化の
  レイアウト変更 — **`JOURNAL_FORMAT_VERSION` bump 要**（v5 → v6）。
  `shape.custom_path` processor は PathPoints から点群 + `in_tan`/`out_tan`
  点属性付き Geometry を生成する。
- **曲線描画**: 接線非ゼロ区間をベジェとして comp 空間固定 tolerance で
  折れ線化する共有フラット化関数（ravel-nodes）。rasterize は CPU / GPU
  ともフラット化済み折れ線を消費し、GPU/CPU 等価性テストを維持する。
  tolerance 値は実装時にゴールデンで決定（0.25px 級を起点）。

## 実装単位

1. **選択一元化**: `CanvasSelection` Global 新設、Node Editor 移行
   （機能変化なし、既存テスト維持 + Global 経由の選択テスト）。
2. **ツール基盤**: `ToolState` Global、CommandId/Action/キーバインド
   （V/P/R/E/H/Z、H ホールド）、Viewer 上部ツールバー（lucide アイコン
   vendoring: mouse-pointer / pen-tool / square / circle / hand / zoom-in）、
   ロケール。ツールはまだ機能しなくてよい（切替と表示のみ）。
3. **bbox 表示**: 選択ノードの bounds 取得経路（選択駆動評価の結果から）、
   殻 transform 順適用、単一/合成 AABB オーバーレイ描画。
4. **クリック選択 + bbox 移動**: AABB ヒットテスト（Shift トグル、
   空白クリック解除）、移動セマンティクス（center_x/y / PathPoints /
   直下流 transform translate）、ローカル予測枠 + apply/commit/Esc、
   複数移動。
5. **シェイプ描画ツール**: rect/ellipse ドラッグ生成（Shift/Alt 修飾）、
   rasterize 空き入力への自動配線、レイヤー不在時の Shape テンプレート
   自動作成。
6. **ペン基盤（core/nodes）**: `PathPoints` variant + journal v6 bump、
   `PathPoint` 型、Properties の read-only 表示（点数表示程度）、
   `in_tan`/`out_tan` 名前定数、`shape.custom_path` processor、
   共有フラット化 + rasterize 統合（GPU/CPU 等価性テスト含む）。
7. **ペンツール UI**: 描画ステートマシン（クリック=コーナー/ドラッグ=
   スムーズ対称/閉路/Esc 確定）、点・ハンドルの表示と移動、
   path→rasterize 自動配線 + 選択切替。
8. **PathChannel 設計書**: パスアニメーション（点数変化パスの補間が難問）
   の設計メモを docs/implementation/ に残す（実装しない）。

単位 1〜5 が UI 系列（順に依存）、単位 6 は core 系列で 2〜5 と並行可能。
7 は 2 と 6 に依存。PR は単位ごと。

## 完了条件（フェーズ別）

- 単位 1: 選択の読み書きが Global 経由に一本化され、Node Editor の
  既存選択挙動（矩形選択・クリック・delete/duplicate 対象）が不変。
- 単位 2: ツール切替がキー/ツールバー両方から機能し、H ホールド・
  中ドラッグパンが動作する。
- 単位 3: 選択ノードの bbox が殻 transform 込みで見た目と一致して表示。
- 単位 4: REQ-UI-011 受入条件の選択・移動項目を満たす（1 ドラッグ =
  1 undo、Esc revert、非 identity 殻で読み取り専用）。
- 単位 5: ドラッグ描画 → 自動配線 → 即座に Viewer に表示、undo 1 回で
  ノードごと消える。
- 単位 6: PathPoints の保存・再読込・undo が破綻せず、曲線が CPU/GPU で
  同一描画（等価性テスト）。既存プロジェクト読み込み互換
  （variant 追加は既存データに影響しないが journal bump を忘れない）。
- 単位 7: REQ-UI-011 受入条件のペン項目を満たす。

## 検証

- 各 PR で `mise run check`。ヒットテスト・移動セマンティクス・
  フラット化は headless テスト（ravel-ui / ravel-nodes）を基本とし、
  ツールのドラッグ操作は GPUI 統合テスト（入力ルーティング依存のもののみ）。
- Phase 5/6 と同様、既存プロジェクトの読み込み互換を必須確認項目とする。
