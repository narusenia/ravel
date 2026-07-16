# ジオメトリパイプライン UI 結合計画（TASK-043 + TASK-039 + 最小 Viewer）

対象タスク: TASK-043（シェイプのジオメトリ化 + インスタンス複製）、
TASK-039（属性操作ノード群）、および最小 Viewer パネル。
関連要件: REQ-MOGRAPH-001 (v2)、REQ-CORE-010。

## 問題

ジオメトリ基盤（TASK-038/040/042、マージ済み）は headless で完結しており、

1. 絵が画面に出ない — Viewer が PlaceholderPanel のままで、Rasterize の
   FrameBuffer 出力に表示先がない。
2. シェイプが Geometry を生成しない — `LayerSource::Shape` は
   `comp.source.shape` プレースホルダのままで、Geometry → Rasterize
   チェーンに乗らない（TASK-042 のゴールデンテストがブロックされている）。
3. ノードエディタから触れない — field.* に registry テンプレートがなく、
   `DataTypeId::FIELD` のポート色も未定義（灰色フォールバック）。
   属性の生成・転送・昇格ノードが存在しない。

## ターゲット構成

```text
shape.* ノード ──┐
                 ├─→ Geometry ─→ attribute.* / field 変調 ─→ rasterize ─→ FrameBuffer
scatter.* ノード ┘                                                          │
                                                                            ▼
compile.rs: LayerSource::Shape → ShapeGeometry → Rasterize 展開      Viewer パネル（静的表示）
```

- Viewer は「選択ノード（または Composition 出力）の FrameBuffer を canvas
  に描くだけ」の静的パネル。再生・同期・スクラブは TASK-013 スコープに残す。
- 属性検査 UI（GeometrySummary の Properties 表示）は本計画外
  （TASK-047 属性スプレッドシートまで保留）。
- Timeline の Document 統合は独立負債として本計画に含めない。

## 実施体制

前セッション（TASK-040+042）と同型の並列体制:

- Claude: Phase 1–4（TASK-043 + Viewer）。ブランチ
  `feat/shape-geometry-nodes` → `feat/minimal-viewer-panel` の 2 PR 構成。
- codex 委譲: Phase 5（TASK-039、ブランチ `feat/attribute-op-nodes`、
  git worktree）。**worktree 作成直後に `mise trust` を必ず実行**
  （pre-commit の mise タスクが未 trust で停止する既知問題）。
- 両者とも `crates/ravel-nodes/src/lib.rs` の processor match と
  `registry/builtin.rs` を触るため、マージ順に応じて後発をリベース。

## Phase 分割と完了条件

### Phase 1: シェイプ生成ノードの Geometry 出力化（TASK-043 step 1）

`crates/ravel-nodes/src/shape/` 新設。rect / ellipse / polygon / star /
custom path の各ノードが `Geometry`（Path プリミティブ + P 列）を出力。
ellipse はセグメント数パラメータでポリライン近似。

- 完了: 各シェイプの点数・閉路・バウンディングボックスを検証する
  headless テスト。registry テンプレート（カテゴリ Generator、出力
  GEOMETRY）登録、builtin カウントテスト更新。

### Phase 2: 複製ノード群（TASK-043 steps 2–5）

`crates/ravel-nodes/src/scatter/` 新設。grid / circular / path-array /
scatter の 4 ノード。全インスタンスに `index`/`P`/`rot`/`scale` を必ず付与。
scatter は seed 決定的（`seed` パラメータ + ハッシュ、`Date`/乱数グローバル
禁止）。複製ソースは入力ポート（任意 Geometry、`instance_source` に接続）。

- 完了: 属性付与の網羅テスト、同一 seed 再現性テスト、複製数テスト。
  path-array は接線から rot を算出（TASK-043 step 3）。

### Phase 3: compile.rs の Shape 展開差し替え（TASK-042 残件回収）

`LayerSource::Shape` の合成チェーンを
`comp.source.shape` → `ShapeGeometry → Rasterize` 展開へ変更。

- 完了: コンパイルスナップショットテスト更新。ゴールデンイメージテスト
  1 本（Solid 系との合成結果の画素検証、GPU 不要の CPU 経路）。
  TASK-042 完了条件のゴールデンテスト項目をチェック。

### Phase 4: 最小 Viewer パネル

`PanelKind::Viewer` を PlaceholderPanel から実パネル化。
Node Editor の選択ノードを evaluator で評価し、FrameBuffer を canvas に
描画（アスペクト維持、fit 表示のみ）。評価は既存の評価スレッド経由で
UI スレッドを塞がない。選択変更は `SelectedPropertiesTarget` 監視を再利用。

- 完了: FrameBuffer → 表示のヘッドレス変換ロジックにユニットテスト。
  描画自体は手動確認（rect → rasterize → Viewer で絵が見えること）。
  render 純粋性・focus 所有権は ravel-review で確認。

### Phase 5（codex 並列）: TASK-039 属性操作ノード群

- Attribute Set（定数書き込み）/ Promote（point↔instance↔detail、
  平均・最大・最初）/ Transfer（最近傍・距離重み）/ Path Sample
  （弧長→P/接線/法線）。純ロジックは `ravel-core/src/geometry/ops.rs`、
  processor は `ravel-nodes/src/attribute/`。
- TASK-040 残件をここで回収: field.* の registry テンプレート、
  `DataTypeId::FIELD` のポート色、`apply_field` の変調ノード配線。
- **繰延**: TASK-039 step 5（Lua からの属性参照）は mlua 依存が未導入のため
  TASK-031（Lua スクリプティング環境）まで保留。ExpressionField placeholder
  と同じ扱い。依存追加はユーザー承認事項。

- 完了: 転送精度・昇格集約・複製経由の属性伝播の統合テスト。
  Lua 項目を除く TASK-039 完了条件のチェック。

## リスク・注意

- Phase 3 は既存の comp スナップショットテストを広く割る可能性 —
  差分は意味を確認してから更新する（機械的に上書きしない）。
- Viewer の評価トリガ設計が焦点: render() 内で評価しない
  （dirty 通知 → バックグラウンド評価 → cx.notify のパターン）。
- ellipse/star の頂点数はゴールデンテストの安定性に効くため定数化する。
- 検証はフェーズごとに `mise run check`、PR 前に ravel-review + gate。

## 完了の定義

- rect シェイプ → grid 複製 → rasterize → Viewer で絵が画面に出る（手動確認）。
- TASK-043 完了条件 4 項目すべてチェック、TASK-042 のゴールデンテスト項目
  チェック、TASK-039 は Lua 項目以外チェック。
- ドキュメント反映: ui-impl-status.md（Viewer 節新設）、
  agent-api-reference.md（shape/scatter/attribute type_key 追記）、
  procedural-geometry.md の影響表更新。
