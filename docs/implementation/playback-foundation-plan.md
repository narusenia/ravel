# 再生基盤計画（TASK-013 のメディアデコード非依存部分）

対象: フレーム精度の再生クロック、トランスポート、Timeline 再生ヘッド連携、
Viewer の連続更新。関連タスク: TASK-013（映像/音声同期再生）。
関連要件: REQ-MEDIA-002、REQ-CORE-005（UI 非ブロッキング）。
基盤: `eval-render-performance-plan.md` Phase 1（バックグラウンド評価）と
Phase 4（RenderImage 表示）の上に載る。

## 問題

再生機能が存在しない。評価は選択イベント駆動の単発（frame 0 固定）で、
Timeline の `playhead` は手動シークのみ、`CommandId::PlaybackToggle` は
定義済みだが何も駆動しない。

## スコープ判断（design gate）

**スコープ外（明示的繰延）**: 音声同期（オーディオマスタークロック、
TASK-013 step 2）とデコード済みメディアフレームの表示（step 5 の
メディア部分）。理由: 依存する TASK-012（タイムライン基盤 + メディアビン）
が In Progress であり、メディアデコード（ravel-media）と Composition/Layer
の接続、および ravel-audio の出力エンジン統合が未配線。これらが揃うまで
**再生クロックは wall-clock（monotonic）をマスター**とし、オーディオ
マスターへの切替は将来 `PlaybackClock` の時刻源差し替えで行えるよう
インターフェースだけ分離しておく。

スコープ内 = TASK-013 steps 1, 3(既存), 4, 5(Viewer 連続更新のみ), 6(部分)。

## ターゲット構成

```text
UI スレッド                                    評価ワーカー(既存 EvalService)
─────────────────────────────                 ──────────────────────────
PlaybackToggle / FrameStep (Action)
   │
PlaybackController (gpui タスク)
   │  tick: PlaybackClock::current_frame()
   ├─ TimelinePanel.set_playhead(frame) → notify
   └─ eval.request(graph, node, ctx{frame}, None) ──▶ latest-wins 評価
                                                        │
Viewer ◀── ViewerFrame（世代フィルタ済み）◀── finalize ─┘
```

- `PlaybackClock`（ravel-core、純粋・ヘッドレス）:
  `play(now: Instant)` / `pause(now)` / `toggle(now)` / `stop()` /
  `seek(frame, now)` / `step(±delta, now)` /
  `current_frame(now: Instant) -> u64`。フレーム算出は
  「基準フレーム + (now − 基準時刻) × num / (den × 10⁹)」の**整数有理数
  演算**（u128）で、境界の浮動小数点切り捨ても tick 間隔の誤差蓄積も
  ない（フレーム精度）。時刻源は引数で注入しテスト可能にする。
  範囲は `[0, duration)` で clamp。最終フレームも 1 フレーム分の区間
  表示されてから自動 pause（ループは将来）。空タイムラインでは
  トランスポートは no-op。
- `PlaybackController`（ravel-app）: 再生中のみ `cx.spawn` ループが
  フレーム間隔で起床し、クロックの現在フレームが前回と変わったときだけ
  playhead 更新 + 評価要求を投函する。評価が追いつかない場合は
  latest-wins が自然にフレームドロップとして機能する（計測用に
  ドロップ数をカウント）。
- トランスポート: 既存 `CommandId::PlaybackToggle` を配線し、
  `PlaybackStop`（先頭へ戻して停止）と `FrameStepForward` /
  `FrameStepBackward` を追加。追加は `CommandId` + workspace の
  `for_each_command!` テーブルのみ（コマンド経路不変条件）。
  キーバインド: Space / K のトグル等は `assets/keybindings/default.toml`、
  メニューラベルは locale 資産に追加。
- 評価対象ノード: 当面は現行どおり「NodeEditor の選択ノード」
  （Composition 出力の常時評価は composition 評価統合後）。frame は
  これまで固定 0 だった `EvalContext::frame` に実値が入る —
  time-dependent ノード（comp.time_offset 等）のキャッシュ動作は
  evaluator の frame 対応キャッシュで既にテスト済み。

## 実装単位（レビュー可能な粒度）

1. `feat: add frame-accurate playback clock`（ravel-core）
   - `PlaybackClock` + 単体テスト（ドリフト無し、pause/resume 境界、
     seek/step、末尾 clamp）。完了: テストのみで検証可能。
2. `feat: add transport commands and playback controller`（ravel-ui +
   ravel-app + assets）
   - CommandId 追加、for_each_command! 配線、キーバインド、locale。
   - `PlaybackController`: トグル/停止/ステップが TimelinePanel の
     playhead を動かす。完了: ヘッドレス（AppShell/Timeline）テスト +
     コマンドディスパッチテスト。
3. `feat: drive viewer evaluation from the playback clock`（ravel-app）
   - 再生中の評価要求投函（frame 付き）と ViewerFrame 連続更新。
   - 完了: スモーク実行でフレームが進むこと（ログ/span）、
     ドロップカウンタの記録。perf-baseline.md に再生時の
     フレームレート計測を追記（step 6 部分）。

## リスク・注意

- gpui タイマー（`background_executor().timer`）の分解能とジッタ —
  クロックが wall-clock 基準なので tick ジッタはフレーム落ちにはなるが
  ドリフトにはならない（設計上の要点）。
- 評価が 1 フレーム時間を超えるグラフでは表示 fps が落ちる。
  latest-wins により UI はブロックしない（Phase 1 の保証を引き継ぐ）。
- undo/redo・グラフ編集との併走: 再生中の編集は通常の
  `InvalidationHint` 経路に乗る（コントローラは hint None で投函）。
- 将来のオーディオマスター化: `PlaybackClock` の時刻源
  （`now: Instant`）注入点を audio クロックのサンプル位置に差し替える。
  本計画ではインターフェース分離のみ担保し、実装しない。

## 完了の定義

- Space でデモグラフの blur アニメーション（time-dependent ノードが
  無い場合は playhead の進行のみ）が再生/停止でき、フレームステップが
  1 フレーム単位で動く。
- UI スレッドが再生中も評価でブロックしない（Phase 1 の span 検証を
  引き継ぎ）。
- 音声同期・メディア表示はスコープ外として本書と ui-impl-status.md に
  明記（TASK-013 の残項目として保持）。
