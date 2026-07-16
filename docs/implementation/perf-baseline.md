# 評価・描画パフォーマンス baseline（Phase 0 計測結果）

計画: `eval-render-performance-plan.md`。計測日: 2026-07-17。
環境: Apple M5 / macOS 26.3 / release ビルド / 512×512 RGBA f32。

## 計測方法

- 計測ハーネス: `crates/ravel-nodes/examples/perf_baseline.rs`
  （`cargo run -p ravel-nodes --release --example perf_baseline`）。
  `NodeEditorPanel` の UI スレッド処理のうち評価系
  （`apply_property_change` → `sync_processors` → `evaluate_for_viewer`）を
  ヘッドレスに再現し、計装済み `tracing` span（`evaluate` /
  `node_process` / `gpu_upload` / `gpu_readback` / `cpu_rasterize` /
  `register_processors`）を集計する。
- 除外している UI 側処理（ウィンドウが必要なため）: ノードサイズ再計算、
  undo push、`ViewerFrame` Global 発行、GPUI notify/paint。いずれも
  CPU 軽量だが、in-app 計測での上乗せ分として留意。
- グラフ: `source(512×512 グラデーション) → blur → color_correct →
  merge.A`、`source → merge.B`（GPU 3 ノード経由のビューア出力を模す）。
- 転送回数は `ravel_gpu::transfer::stats` のプロセス毎カウンタで検証。
- シナリオ (d) 無操作アイドルは、ヘッドレスでは評価パスが一切走らない
  ことの確認に留まる（アイドル時の再描画コストは UI 側の別問題）。

## 結果

### (a) ノード選択切替（グラフ不変・キャッシュ温）

| 指標 | 値 |
|------|----|
| wall/iter | ~0.00 ms（20 iters） |
| 転送 | 0 uploads / 0 readbacks |

選択切替そのものはキャッシュヒットで完結し、評価コストはほぼゼロ。
選択時の体感カクつきの犯人は評価ではなく、選択に伴う再描画・
Properties 再構築・（Geometry 選択時の）アドホック rasterize 側にある。

### (b) blur radius スクラブ 3 秒（90 ticks、現行 UI 経路）

Change 毎に evaluator を作り直し（`sync_processors` 相当）、`merge` を
再評価。radius は 1.0→23.25 を掃引。

| 指標 | 値 |
|------|----|
| wall/tick | **mean 14.62 ms**（min 10.42 / max 18.30） |
| 転送/90 ticks | 360 uploads (1.51 GB) / 270 readbacks (1.13 GB) |

| span | calls | mean ms |
|------|-------|---------|
| evaluate（全体） | 90 | 14.03 |
| node_process:blur | 90 | 7.21 |
| node_process:merge | 90 | 3.74 |
| node_process:color_correct | 90 | 3.07 |
| gpu_readback | 270 | 2.37 |
| gpu_upload | 360 | 0.43 |
| register_processors | 90 | **0.57** |

1 tick あたり読み戻し 3 回 (7.1 ms) + アップロード 4 回 (1.7 ms) =
**約 8.8 ms（~60%）が CPU↔GPU 往復**。各 GPU ノード内の dispatch 毎
`ctx.wait()`（ブロッキング）も node_process に含まれる。
一方 evaluator 再構築（`register_processors`、パイプライン再生成込み）は
0.57 ms と軽い — shader モジュールがハッシュキャッシュされるため。

### (b') 同スクラブ、変更ノードのみ再登録（evaluator・キャッシュ維持）

processor はパラメータを構築時に取り込むため、radius 変更には blur
processor の再生成・再登録が必要（`register` が dirty 化し、下流は
freshness 伝播で再計算、source はキャッシュ利用）。

| 指標 | 値 |
|------|----|
| wall/tick | mean 15.06 ms（min 10.88 / max 22.10） |
| 転送/90 ticks | (b) と同一: 360 uploads / 270 readbacks |

**(b) との差はノイズ範囲（±0.5 ms）。「processor 全再構築が高い」仮説は
棄却**。転送回数も不変 — 各 GPU processor が CPU 入力を毎回アップロード
し直すため、evaluator キャッシュを維持しても往復は 1 回も減らない。
スクラブコストの本体は GPU 往復と同期待ちそのもの。

### (c) scatter count=500 の Geometry チェーン選択

`shape.rect → scatter.grid(25×20=500)`。evaluator は構築済み・キャッシュ温
（選択では processors を再構築しない実挙動に合わせる）。Viewer 用の
アドホック rasterize（`evaluate_for_viewer` と同経路、キャッシュ無し）を
毎回実行。

| 指標 | 値 |
|------|----|
| wall/iter | **mean 38.02 ms** |
| cpu_rasterize | mean 37.75 ms（ほぼ全部） |
| evaluate（geometry 部分、温） | 0.007 ms |

Geometry ノードを選択するたびに UI スレッドが 38 ms ブロックする。
犯人は CPU ラスタライズ単体（キャッシュされないため毎選択で発生）。

### paint プロキシ（run-merge 走査、512×512）

| コンテンツ | quads | 走査 wall |
|-----------|-------|-----------|
| フラット形状（scatter 出力） | 402 | — |
| グラデーション（merge 出力） | **262,144（= 全ピクセル）** | 0.26 ms |

run-merge はフラット塗りには有効だが、グラデーション・実写系
FrameBuffer では 1 ピクセル 1 quad に退化する。GPUI への 26 万 quad
提出コストは本計測の範囲外（ヘッドレス不可）だが、メディア表示では
paint_quad 経路が支配的コストになることが確実。

## 計画との突き合わせと Phase 順序の判断

計画の推定（§問題）は概ね実測と一致:

1. 「同期評価が UI をブロック」 — 実測 14.6–38 ms/操作（フレーム予算
   16.6 ms 前後〜2 倍超）で**確定**。
2. 「ノード単位の CPU↔GPU 往復」 — スクラブ時間の約 60% で**確定**。
   さらに (b') により、往復は evaluator キャッシュの持ち方と無関係に
   processor 内部で発生することを確認（Phase 2 の設計対象そのもの）。
3. 「paint_quad・CPU rasterize は相対的に小」 — **部分修正**:
   CPU rasterize はインスタンス 500 で 38 ms と大きい（Phase 3 の
   価値を裏付け）。paint_quad はフラット形状では小さいが、
   グラデーション/メディアでは per-pixel quad に退化する
   （Phase 4 の価値はメディア再生の文脈で急上昇する）。

想定外だった点: `sync_processors`（evaluator 再構築 0.57 ms）は軽く、
「processor 再登録の回避」単体はスクラブ最適化として効果がない。

**結論: Phase 順序は計画どおり 1 → 2 → 3 → 4 を維持。**

- Phase 1（バックグラウンド評価）が最優先 — どのシナリオでも
  14.6–38 ms を UI スレッドから排除でき、体感カクつきを直接解消。
- Phase 2（GPU 常駐）はスクラブ評価時間の ~60% を削り、再生
  （TASK-013）のフレーム予算に必須。
- Phase 3（GPU ラスタライズ）は 38 ms/eval の解消。
- Phase 4 は最低ライン（読み戻し 1 回 + RenderImage）をメディア
  対応前に入れる価値がある（per-pixel quad 退化のため）。

## 制約・未計測

- GPUI の実 paint コスト（quad 提出・レイアウト）と
  `rebuild_widgets` は in-app 計測が必要。span は仕込み済みだが、
  アプリの fmt subscriber は span close を出力しないため、取得には
  タイミング集計レイヤ（本ハーネスの `TimingLayer` 相当）または
  `FmtSpan::CLOSE` の有効化が別途必要。
- 計測はプロセス単発実行。GPU ドライバ状態によるばらつきは
  min/max の幅（(b) で 10.4–18.3 ms）として記録。

## Phase 完了時の再計測

Phase 1/2/4 の完了条件の再計測はこのファイルに追記する。

### Phase 1（バックグラウンド評価）完了時

シナリオ (b'') = (b) と同じ 90 tick スクラブを `EvalService` 経由で実行
（ハーネスに追加済み）。

| 指標 | Phase 0 (b) | Phase 1 (b'') |
|------|------------|---------------|
| UI スレッド wall/tick | 14.62 ms | **~0.00 ms（要求投函のみ）** |
| 評価回数/90 ticks | 90 | **1**（think time なし投函のため全て coalesce） |
| 転送/評価 1 回 | 4 uploads / 3 readbacks | 同（Phase 2 の対象のまま） |

実スクラブ（Change 間隔 ~33 ms）ではワーカーが追従できる限り毎 tick
評価されるが、UI スレッドはブロックしない。評価 1 回あたりの GPU 往復
（4 up / 3 down、~8.8 ms）は Phase 2 で削減する。

### Phase 2（GPU 常駐パイプライン）完了時

GPU 4 ノードが `GpuFrameBuffer` を入出力し、dispatch 毎の `ctx.wait()` を
除去。読み戻しは Viewer 境界（`GpuEvalHooks::finalize`）の 1 回のみ。

計測注記: Phase 2 以降の evaluate は GPU 作業を投入するだけで完了を
待たない。表の「評価 wall/tick」は評価スレッドの占有時間（投入まで）、
「end-to-end」は 90 tick 分の GPU 完了（`ctx.wait()`）込みの実測。

| 指標（(b) 90 ticks） | Phase 0/1 | Phase 2 |
|------|-----------|---------|
| 評価 wall/tick（投入まで） | 14.62 ms | 1.41 ms |
| **end-to-end /tick（GPU 完了込み）** | 14.62 ms | **1.45 ms（-90%）** |
| readbacks | 270（3/tick） | **0** |
| uploads | 360（4/tick） | 180（2/tick、CPU ソースの GPU チェーン流入点のみ） |
| node_process:blur | 7.21 ms | 0.50 ms（ブロッキング待ち消滅） |

- 中間読み戻しゼロは `gpu_resident_pipeline.rs` の転送カウンタテストで
  担保（`GpuContext::transfer_stats` — カウンタはコンテキスト毎に分離）。
- 常駐経路と CPU 経由ステージング経路の画素等価テスト済み（誤差 <1e-5）。
- evaluator キャッシュ上の GPU ハンドルは drop で共有プールに自動返却
  （テストで担保）。プール予算は eval ワーカー共有で 512 MiB。
  **既知の制約**: LRU 予算が束縛するのはアイドル（返却済み）テクスチャ
  のみで、キャッシュが保持する常駐ハンドルの総量は未束縛。三層フレーム
  キャッシュ（REQ-CORE-006）設計時に GPU 対応のキャッシュ eviction と
  合わせて解決する。
- 残る uploads 2/tick は CPU ソース（将来のメディアデコード出力が GPU
  常駐になれば 0）。Viewer 表示の読み戻し ~1.9 ms/フレームは Phase 4
  （RenderImage / ゼロコピー）の対象。

### Phase 4（Viewer の image 表示、最低ライン）完了時

paint_quad ランマージを `RenderImage` + `img` 要素に置換。GPUI の実
paint コストはヘッドレスで測れないため、提出プリミティブ数で比較:

| コンテンツ（512×512） | paint_quad 経路 | RenderImage 経路 |
|----------------------|----------------|------------------|
| フラット形状 | 402 quads / render | **1 textured quad** |
| グラデーション/実写 | **262,144 quads / render**（ピクセル毎に退化） | **1 textured quad** |
| CPU 側前処理 | run-merge 走査 0.26 ms × **render 毎** | BGRA u8 変換 ~O(n) × **フレーム更新毎のみ** |

読み戻し（~1.9 ms/フレーム）は評価ワーカー側 finalize に留めたまま
（UI 非ブロッキング）。ゼロコピー共有（ストレッチ）は未着手 — メディア
再生で変換・読み戻しがボトルネック化した時点で再評価する。
