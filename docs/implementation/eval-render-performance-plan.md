# 評価・描画パフォーマンス計画（バックグラウンド評価 + GPU 常駐パイプライン）

対象: UI のカクつき解消と GPU 描画パイプラインの確立。
関連タスク: TASK-042（GPU ラスタライズ残件）、TASK-013（再生 — 本計画の
基盤の上に載る）。関連要件: REQ-CORE-005（UI 非ブロッキング）、REQ-CORE-009、
REQ-GPU 系。

## 問題

UI 操作（スクラブ・選択・ドラッグ）でフレーム落ちが体感される。推定原因は
描画コストではなく **UI スレッドのブロッキング**:

1. **同期評価**: `NodeEditorPanel::evaluate_for_viewer` が選択変更・
   プロパティ編集（スクラブ中の Change 毎！）に UI スレッドで
   `Evaluator::evaluate` を同期実行する。
2. **ノード単位の CPU↔GPU 往復**: `FrameBuffer` が CPU 常駐
   （`Arc<[f32]>`）のため、GPU ノード（blur / transform / merge /
   color_correct）は毎回「アップロード → compute → **ブロッキング
   読み戻し**」を行う。チェーン内の GPU ノード数だけ往復が発生し、
   すべて評価スレッド（現状 = UI スレッド）で待つ。
3. Viewer の paint_quad 描画・CPU ラスタライズ自体のコスト（相対的に小）。

ただし配分は未計測。**数字を取ってから作る**。

## ターゲット構成

```text
UI スレッド                          評価ワーカー
──────────────                      ─────────────────────────────
選択/編集イベント ──要求(世代付き)──▶ Evaluator（Graph クローン、
   │                                │  processors、GpuContext）
   │◀─── 最新世代の結果のみ通知 ────┘
ViewerFrame 更新 → Viewer は GPU テクスチャを 1 回だけ CPU 化して表示
                    （将来: 共有サーフェスでゼロコピー）

ノード間: GpuFrameBuffer（wgpu テクスチャハンドル）のまま受け渡し
読み戻し境界: Viewer 表示・エクスポート・CPU 専用ノードの直前のみ
```

## Phase 分割と完了条件

### Phase 0: 計測（作る前に測る）

`tracing` span を評価経路に張り、カクつきの犯人配分を数字で確定する。

- span: `evaluate_for_viewer` 全体 / ノード毎 `process` /
  `ravel-gpu::transfer`（upload・readback 個別）/ CPU rasterize /
  Viewer `paint_framebuffer` / Properties `rebuild_widgets`。
- 計測シナリオ: (a) ノード選択切替、(b) blur radius スクラブ 3 秒、
  (c) scatter count=500 の Geometry チェーン選択、(d) 無操作アイドル。
- 完了: 計測結果（シナリオ × span の ms 表）を
  `docs/implementation/perf-baseline.md` に記録し、本計画の Phase 1/2 の
  想定効果と突き合わせる。想定が外れていたら計画を修正してから進む。

### Phase 1: バックグラウンド評価

評価を UI スレッドから追い出す。immutable `Graph`（`im` + `Arc` 構造共有）
なのでクローン渡しが安価 — この設計が活きる場面。

- `EvalService`（ravel-app、または ravel-core/runtime）:
  - 要求: `EvalRequest { graph: Graph, node: NodeId, ctx: EvalContext,
    generation: u64 }`。チャネル投函、**latest-wins**（ワーカーは
    キュー内の古い要求を捨て最新のみ評価）。
  - ワーカー: 専用 `std::thread`（`Evaluator` + `GpuContext` +
    `ShaderManager` を所有。graph 変更時は要求に同梱されるグラフで
    processors を再登録）。gpui の `background_executor` 案もあるが、
    `Evaluator` の常駐状態（キャッシュ）を持つには専用スレッドが素直。
  - 結果返却: 世代番号付き。UI 側は `cx.spawn` / チャネル受信で受け、
    **自分の発行した最新世代のみ** `ViewerFrame` に反映して `notify`。
    古い世代の結果は破棄。
- `NodeEditorPanel::evaluate_for_viewer` は要求投函のみに変更
  （選択 dedupe・SelectBox ガードは維持）。スクラブ Change は latest-wins
  で自然に間引かれる。
- Geometry のアドホック rasterize もワーカー側で行う。
- 完了: UI スレッド上で `Evaluator::evaluate` が呼ばれない（span で確認）。
  latest-wins・世代破棄・graph 差し替えのヘッドレステスト。
  Phase 0 シナリオ (b) の UI フレーム時間改善を計測で確認。

実装ノート（Phase 1 完了時追記）:

- `EvalService` は `ravel-core/runtime/eval_service.rs`。GPU・UI 依存を
  避けるため `EvalWorkerHooks` trait（`sync` = processor 登録更新、
  `finalize` = Geometry の viewer 用 rasterize 等）で処理系を注入し、
  ravel-app の `GpuEvalHooks` が `GpuContext` + `ShaderManager` +
  `ravel_nodes::processor_for_node` を所有・実装する。
- 要求は `InvalidationHint`（`None` / `Params(Vec<NodeId>)` /
  `Structural`）を運ぶ。latest-wins で要求を捨てる際は hint を
  マージ（強い方優先、`Params` は和集合）して再登録漏れを防ぐ。
  Phase 0 計測で processor 全再構築は 0.57 ms と軽いことが判明済みだが、
  `Params` 経路は evaluator キャッシュを保持し上流の再評価を回避する。
- 結果は世代番号付き `EvalUpdate` をワーカースレッド上のコールバックで
  返し、ravel-app は `futures::channel::mpsc`（workspace 依存に追加）
  経由で `cx.spawn` タスクに流し、最新世代のみ `ViewerFrame` に反映する。

### Phase 2: GPU 常駐パイプライン

ノード間の中間結果を GPU に置いたまま流す。

- 新 NodeData: `GpuFrameBuffer { texture: Arc<PooledTexture>, width,
  height }`（ravel-gpu 依存になるため **ravel-core には置かず**、
  トレイトオブジェクトとして流す設計を検討 — 選択肢:
  (a) ravel-core に不透明ハンドル trait `GpuResident` を定義し
  ravel-gpu が実装、(b) `DataTypeId::FRAME_BUFFER` はそのままに
  processor 側で表現をネゴシエート。**(a) を第一候補**として Phase 2
  冒頭で設計確定）。
- 変換ヘルパ: `ensure_gpu(input) -> GpuFrameBuffer` /
  `ensure_cpu(input) -> FrameBuffer`。境界（Viewer 表示・CPU ノード・
  永続化）でのみ読み戻す。
- GPU 4 ノード（blur / transform / merge / color_correct）を
  GpuFrameBuffer 入出力に対応させる。comp.* パススルー系はハンドルを
  素通し。
- evaluator キャッシュに GPU ハンドルが乗るため、テクスチャプールの
  寿命管理（LRU 予算）と dirty 無効化の整合を確認。
- 完了: blur → color_correct → merge チェーンで中間読み戻し 0 回
  （transfer カウンタで検証するテスト）。CPU/GPU 経路の画素等価テスト
  （許容誤差付き）。Phase 0 シナリオ (a)(b) の再計測で往復削減を確認。

### Phase 3: GPU ラスタライズ（TASK-042 step 4 回収）

- `crates/ravel-gpu` にパス塗り/ストローク・ポイントスプライト・
  インスタンス展開の描画パイプライン（render pass）。属性列
  （P/Cd/alpha/pscale/rot/scale）を storage buffer にアップロード。
- rasterize processor は Geometry → `GpuFrameBuffer` 直行
  （Phase 2 の型に出力）。CPU 実装（zeno）はリファレンス兼
  フォールバックとして維持。
- AA 品質は CPU（zeno のアナリティック AA）と完全一致しないため、
  等価テストは許容誤差 + カバレッジ指標で判定。エッジケース
  （自己交差パス、instance 深度、閉路）を CPU と突き合わせ。
- 完了: TASK-042 完了条件「GPU 経路と CPU フォールバックの結果が一致」
  をチェック。scatter count=500 シナリオの ms 改善を記録。
- **codex 並列委譲候補**（Phase 2 の型が main に入った後。WGSL +
  processor + テストで自己完結する単位）。

### Phase 4: Viewer の GPU 表示

- 最低ライン: `ViewerFrame` に `GpuFrameBuffer` を流し、表示直前の
  1 回だけ読み戻して gpui の image 要素（`RenderImage`）で描画
  （現行の paint_quad ランマージを置換）。
- ストレッチ: GPUI-CE レンダラと ravel-gpu の wgpu デバイス間の
  テクスチャ共有（IOSurface / 共有ハンドル）でゼロコピー表示。
  デバイスが別インスタンスのため要調査 — 不成立でも最低ラインで
  十分な改善が出る想定。
- 完了: Viewer 表示コストの計測比較（paint_quad vs image）。
  再生（TASK-013 step 5）への接続点をドキュメント化。

## 実施体制

- Phase 0–2: Claude（アーキテクチャ変更を含むため）。
- Phase 3: codex 委譲候補（worktree、`mise trust` 忘れず）。
- Phase 4 最低ラインは Phase 2 完了後なら小さい。ストレッチは別判断。

## リスク・注意

- **Phase 0 の結果次第で順序を変える**。例: 犯人の大半が paint_quad
  なら Phase 4 前倒し。計測より先に最適化しない。
- wgpu オブジェクト（Texture 等）は Send+Sync だが、評価ワーカーと
  UI の同時利用でキュー競合しないよう submit 経路をワーカーに限定する。
- evaluator キャッシュに GPU ハンドルが乗ると VRAM 使用量が増える —
  テクスチャプールの予算・LRU が効いていることをテストで担保。
- `GpuFrameBuffer` の設計（ravel-core の依存方向）は Phase 2 冒頭で
  確定させ、この計画書に追記してから実装に入る（design gate 内 gate）。
- undo/persistence は CPU `FrameBuffer` に一切依存していない（graph が
  真実源）ため影響なし — ただし将来のフレームキャッシュ（三層キャッシュ
  REQ-CORE-006）は GPU ハンドルを直接永続化できない点を設計に残す。

## 完了の定義

- スクラブ・選択切替中に UI スレッドが評価でブロックしない（計測で確認）。
- blur チェーンの中間 CPU↔GPU 往復ゼロ。
- rect → grid 複製 → GPU rasterize → GPU blur → Viewer が読み戻し
  1 回（表示時のみ）で回る。
- Phase 0 の baseline と最終計測の比較表を perf-baseline.md に追記。
- TASK-042 の GPU/CPU 一致項目チェック。ui-impl-status.md・
  agent-api-reference.md・procedural-geometry.md の影響表更新。
