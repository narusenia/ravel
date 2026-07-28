# 評価・描画パフォーマンス baseline（Phase 0 計測結果）

> **Status**: Reference — measurement record

計画: `done/eval-render-performance-plan.md`。計測日: 2026-07-17。
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

### Phase 3（GPU ラスタライズ）完了時

`rasterize` の通常ノードを instanced-quad render pass（non-zero winding +
edge distance の fragment 評価、triangulation 不要）に置換。Composition
synthetic ノードと Viewer ad-hoc は golden 互換の CPU zeno 経路を維持。

| 指標（scatter 500 instances、512×512、release） | CPU (zeno) | GPU |
|------|-----------|-----|
| rasterize / 評価 | 50.2 ms | **2.6 ms（GPU 完了込み、~19×）** |
| + RGBA32Float 読み戻し | — | 3.0 ms |

GPU/CPU 等価: 自己交差パス 100.000%（0.1 許容内画素）/ coverage Δ0.004%、
開閉路 99.479% / Δ0.012%、ネスト instance + pscale/rot/scale/Cd/alpha
100.000% / Δ0.037%。しきい値: 画素一致 >99%、coverage Δ<2%。

### 再生（`done/playback-foundation-plan.md` 実装単位 3）完了時

シナリオ (e) = 30 fps × 90 フレーム（3 秒）の再生。`PlaybackClock` を
フレーム間隔で tick し、フレームが進んだときだけ `EvalService` に
`EvalContext::frame` 実値付きの要求を投函する（PlaybackController の
tick ループをヘッドレスに再現、ハーネスに追加済み）。デモグラフに
time-dependent ノードがまだ無いため、blur radius をフレーム毎に
アニメーションさせて毎フレーム実仕事を発生させている。

| 指標 | 値 |
|------|----|
| UI スレッド wall/公開フレーム（フレーム算出 + 要求投函） | **mean 0.01 ms** |
| 実測レート | 90 フレーム / 3.02 s、**27.4 fps 評価** |
| 公開フレーム | 83（tick ジッタによるスキップ 7） |
| latest-wins coalesce | **0**（評価 ~1.5 ms/フレーム、ワーカーは余裕で追従） |
| 転送/フレーム | 2 uploads / **0 readbacks**（Phase 2 の常駐維持） |

- tick ジッタのスキップは設計どおり「フレーム落ち」になり、クロックは
  ドリフトしない（90 フレームを 3.02 s で走破 = 実時間精度維持）。
  スキップ数は `Transport::dropped_frames` として PlaybackController が
  カウントし、停止/終端到達時に tracing へ記録する。
- 評価が 1 フレーム時間（33 ms）を超えるグラフでは coalesce 数が
  増えて表示 fps が落ちるが、UI スレッドは要求投函（~0.01 ms）のみで
  ブロックしない（Phase 1 の保証を引き継ぐ）。
- Viewer 表示側の読み戻し・`RenderImage` 変換コストは Phase 4 の表の
  とおり（フレーム更新毎の BGRA 変換のみ）。

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

### RESP-3（シェーダ・パイプライン共有）完了時

計測日: 2026-07-28。環境: Apple M5 / macOS 26.3 / release ビルド / 512×512。
`done/ui-responsiveness-plan.md` RESP-3（issue HIGH-06）。
同一ツリーの変更前コミットを `git worktree` で並べ、同じハーネスの
シナリオ (b) を各3回走らせて比較した（`register_processors` は
`Structural` 相当の全プロセッサ再登録スパン）。

| 指標 | 変更前（3回） | 変更後（3回） |
|------|--------------|--------------|
| `register_processors` mean/tick | 0.304 / 0.296 / 0.323 ms | **0.015 / 0.015 / 0.018 ms** |
| wall/tick | 1.31 / 1.27 / 1.35 ms | **1.02 / 1.03 / 1.17 ms** |

`register_processors` は約20分の1（−95%）。tick 全体は約 −18% で、
その差はほぼ `register_processors` の差分と一致する。

**読み方の注意（2点）**:

1. 上の 2026-07-17 のシナリオ (b) は wall/tick **14.62 ms**、readback 270 回と
   記録しているが、今日の**変更前**の同シナリオは 1.31 ms / readback 0 回。
   Phase 2〜4 の GPU 常駐化が間に入っているため、14.62 ms との差を
   RESP-3 の効果として読んではいけない。上表の比較のみが RESP-3 の効果。
2. シナリオ (b) は毎 tick 全プロセッサを再登録するので、RESP-3 のうち
   「検証をハッシュキャッシュの後ろへ」と「パイプライン共有」を測っている。
   「状態を持たないプロセッサは再登録せず invalidate だけ」（実 UI の
   `Params` 経路）はこのシナリオでは走らないため、実アプリの編集ティックでは
   さらに減る。

HIGH-06 は「編集中の体感を最も悪化させている要因」と書いていたが、
測定はそれを支持しない。変更前でも `register_processors` は tick の
約23%（0.31 / 1.31 ms）で、残りは `gpu_upload` と GPU ノードの
`node_process`。編集時の体感の主因は依然として第2段（HIGH-04 / HIGH-05 の
CPU↔GPU 往復とシェル合成の CPU per-pixel）側にある。

### GPU シェル合成チェーン baseline

計測日: 2026-07-28。環境: Apple M5 / macOS 26.3 / release ビルド / 512×512。
`gpu-compositing-plan.md` の最初の測定単位として、各レイヤーネットワークが
`source → blur → net.out`（末尾は GPU 常駐）となる Composition を
`compile_composition` で構築した。各シェルは非 identity transform、opacity 0.8、
Normal / Add / Multiply / Screen / Overlay の混在 merge を通る。

スクラブは既存 (b'') と同じく think time 無しで90要求を投函するため、
latest-wins により完成評価は1回。レイヤー数の定数を既定10と比較用3に変えて
別々に実行した。

| 指標 | 10 layers | 3 layers |
|------|-----------|----------|
| wall/iter（要求投函） | mean 0.00 ms（min 0.00 / max 0.00） | mean 0.00 ms（min 0.00 / max 0.00） |
| 完成評価 | 1 | 1 |
| uploads | 10（41.9 MB） | 3（12.6 MB） |
| **readbacks** | **10（41.9 MB）** | **3（12.6 MB）** |
| end-to-end / 90 ticks | 73.88 ms | 34.22 ms |

30 fps 再生形では、tick ジッタとワーカーの latest-wins により完成評価数が
実行ごとに異なるため、総数と評価1回あたりを併記する。

| 指標 | 10 layers | 3 layers |
|------|-----------|----------|
| wall/iter（要求投函） | mean 0.00 ms（min 0.00 / max 0.01） | mean 0.01 ms（min 0.00 / max 0.04） |
| playback | 90 frames / 3.07 s | 90 frames / 3.04 s |
| 完成評価 | 78（25.4 fps） | 76（25.0 fps） |
| uploads | 10（41.9 MB） | 3（12.6 MB） |
| **readbacks** | **780（3271.6 MB）** | **228（956.3 MB）** |
| **readbacks / 完成評価** | **10** | **3** |

読み戻しはスクラブ、再生ともに厳密に **N 回 / 完成評価** であり、
HIGH-05 の「GPU 常駐レイヤーごとに shell transform がブロッキング readback」
を再現した。span 集計には追加計装なしで `node_process:comp.transform`、
`node_process:comp.opacity` と全5種の `node_process:comp.merge.*` が現れ、
短絡せずシェルチェーンが実走していることも確認した。

#### ハーネスは両レイヤー数を1回の実行で回す

上の2表はレイヤー数の定数を書き換えて別々に走らせた結果だが、その後
`SHELL_LAYER_COUNTS = [3, 10]` を導入し、**両方が1回の実行で出る**ようにした。
完了条件「readback 回数がレイヤー数に比例」をコード編集なしで確認できる。
同一実行での確認値:

| シナリオ | 3 layers | 10 layers |
|---|---|---|
| (f) スクラブ readbacks（完成評価1回） | **3** | **10** |
| (g) 再生 readbacks / 完成評価 | 249 / 83 = **3** | 680 / 68 = **10** |
| (g) 評価レート | 27.4 fps | 22.4 fps |

#### コストの分解（同日、別機会の再実行 10 layers / 再生形）

同じハーネスを別に1回走らせた結果（完成評価 71、23.3 fps、readbacks 710）で
内訳を見ると、`evaluate` 合計 3052 ms のうち:

| 内訳 | 合計 | 評価1回あたり |
|------|------|--------------|
| `node_process:comp.transform` | 2396 ms（**78%**） | 33.7 ms（10 レイヤー分） |
| うち `gpu_readback` | 1589 ms | 22.4 ms |
| transform の CPU ループ（差分） | 約 807 ms | 約 11.3 ms |
| `node_process:comp.merge.*` 5種合計 | 480 ms | 6.8 ms |
| `node_process:comp.opacity` | 156 ms | 2.2 ms |

**HIGH-04 と HIGH-05 のどちらか一方では足りない**ことがこの分解から読める。
transform 1回 3.4 ms の内訳はブロッキング readback 約 2.2 ms +
CPU per-pixel ループ約 1.1 ms で、readback だけ速くしても per-pixel ループが、
per-pixel ループだけ消しても readback が残る。
GPU 化（HIGH-05）は readback の**回数**を N→1 にするので両方に効く —
だから計画の着手順は HIGH-05 が先で正しい。

読み戻し帯域は 512×512 で 2978 MB / 3.05 s ≈ 976 MB/s。
`VIEWER_MAX_DIM = 1024` では面積4倍なので単純計算で約 3.9 GB/s に達する。
この上限が対話評価の解像度キャップの実体。

### GPU シェル transform / opacity 投入後（GPUCOMP-2 / 3 / 4）

計測日: 2026-07-29。環境: Apple M5 / macOS 26.3 / release ビルド / 512×512。
変更前コミット `61d7e6c` を `git worktree` に並べ、**同一機体・同一条件で交互に3回**
走らせた（base → new → base → new → …）。下表は3回の中央値。

10 レイヤー / 30 fps 再生形（`(g)`）:

| 指標 | 変更前 `61d7e6c` | 変更後 | 変化 |
|------|-----------------|--------|------|
| 評価レート | 23.5 fps | 25.1 fps | +1.6 fps |
| `evaluate` 合計 | 3032 ms | 2621 ms | **−14%** |
| `node_process:comp.transform` | 3.322 ms/回 | **0.067 ms/回** | **50×** |
| `node_process:comp.opacity` | 0.211 ms/回 | **0.036 ms/回** | 6× |
| `node_process:comp.merge.*`（5モード） | 0.37–0.89 ms/回 | 1.67–4.71 ms/回 | 悪化（下記） |
| readbacks / 完成評価 | 10 | **10** | 変化なし |
| `gpu_readback` | 2.18 ms/回 | 2.15 ms/回 | 変化なし |

10 レイヤー / スクラブ形（`(f)`、完成評価1回）:

| 指標 | 変更前 | 変更後 |
|------|--------|--------|
| `node_process:comp.transform` | 3.32 ms/回 | **0.074 ms/回** |
| readbacks | 10 | **10** |
| end-to-end / 90 ticks | 53.29 ms | 47.25 ms |

#### merge が遅くなったのは readback が移動したから

シェルチェーンは `network(GPU) → transform → opacity → merge` の順で、
transform / opacity を GPU 化すると **ブロッキング readback の発生位置が
transform から merge の `ensure_cpu` へ移る**。merge 1回あたりの増分
（約 +2.6～3.8 ms）はほぼ readback 1回分（2.15 ms）+ CPU 合成ループで、
readback の**回数は 10 のまま変わっていない**。

つまりこの2単位で消えたのは **transform の CPU per-pixel ループ**（評価1回あたり
約 11 ms、10 レイヤー分）だけであり、これが `evaluate` の −14% の実体。
readback を N→1 にするのは merge を GPU 化する GPUCOMP-5 / 6 で、
そこまでは合計の readback 量は変わらない。**中間状態で退行していないこと**
（readbacks / 完成評価が 10 のまま）はこの表で確認済み。

#### 追加された submit 回数（MED-GPU-01 向けの記録）

10 レイヤーで transform と opacity が GPU 経路に移った分、評価1回あたり
**dispatch と submit が 20 回増える**（レイヤーごとに transform 1 + opacity 1、
いずれも短絡しない構成）。`gpu_upload` の回数は 10（初回のソース投入のみ）で
変わらず、`uploads` の総量も 41.9 MB のまま。バッチング（MED-GPU-01）の
判断材料として、GPU 化が進むほどこの増分が積み上がる。
