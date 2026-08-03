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

### GPU シェル merge 投入後（GPUCOMP-5 / 6）

計測日: 2026-07-29。環境: Apple M5 / macOS 26.3 / release ビルド / 512×512。
変更前コミット `0c971a5`（GPUCOMP-2/3/4 まで載った状態）を `git worktree` に並べ、
**同一機体・同一条件で交互に3回**走らせた（base → new → base → new → …）。
下表は3回の中央値。

10 レイヤー / 30 fps 再生形（`(g)`、完成評価 75 回）:

| 指標 | 変更前 `0c971a5` | 変更後 | 変化 |
|------|-----------------|--------|------|
| readbacks / 完成評価 | 10 | **0** | **N → 0** |
| 読み戻し量 | 3145.7 MB | **0.0 MB** | 全消滅 |
| `evaluate` 合計 | 2849 ms | **175 ms** | **−94%** |
| `evaluate` 1回あたり | 3.453 ms | **0.209 ms** | 17× |
| `gpu_readback` | 2.495 ms/回 × 750 | **0 回** | 消滅 |
| `comp.merge.add` | 5.292 ms/回 | **0.053 ms/回** | 100× |
| `comp.merge.overlay` | 3.915 ms/回 | **0.046 ms/回** | 85× |
| `comp.merge.multiply` | 3.804 ms/回 | **0.047 ms/回** | 81× |
| `comp.merge.screen` | 3.560 ms/回 | **0.046 ms/回** | 77× |
| `comp.merge.normal` | 1.747 ms/回 | **0.024 ms/回** | 73× |
| `comp.transform` | 0.062 ms/回 | 0.081 ms/回 | 悪化（下記） |
| `comp.opacity` | 0.034 ms/回 | 0.051 ms/回 | 悪化（下記） |
| 公開レート | 24.9 fps | 24.9 fps | 変化なし（下記） |

10 レイヤー / スクラブ形（`(f)`、完成評価1回）:

| 指標 | 変更前 | 変更後 |
|------|--------|--------|
| readbacks | 10 | **0** |
| `evaluate` 合計 | 51.99 ms | **19.98 ms** |
| end-to-end / 90 ticks | 46.72 ms | **15.95 ms** |

3 レイヤー / 30 fps 再生形でも同じ形（readbacks 225 → 0、
`evaluate` 2.996 → 0.190 ms/回）。**readback 回数がレイヤー数に比例しなくなった**
ことが GPUCOMP-1 で入れた2つのレイヤー数から直接読める。

#### readbacks が 1 ではなく 0 になっている理由

計画の完了条件は「フレームあたり readback 1 回（finalize の分）」だが、
`perf_baseline` の `BenchHooks` は `finalize` を実装していない
（表示経路を持たないハーネスなので）。**シェル合成チェーンに起因する readback が
0 になった**のが上表の意味で、アプリ側に残る1回は
`ravel-app` の `GpuEvalHooks::finalize` の分。両者を合わせた
「完成評価あたり 1」の pin は GPUCOMP-7 で入れる。

#### transform / opacity が僅かに遅くなった理由

merge の `ensure_cpu` が消えたことで、**チェーン全体の同期点がなくなった**。
変更前はレイヤーごとの readback が暗黙のキューフラッシュとして働いていたため、
その直前の transform / opacity の submit は空のキューに載っていた。変更後は
1評価あたり 29 回の dispatch がまとめて積まれるので、1回あたりの submit コストに
キュー圧が乗る。絶対値は 0.02 ms 程度で、消えた readback 2.5 ms × 10 に対して
無視できる。バッチング（MED-GPU-01）で回収する対象。

#### 公開レートが変わらないのは tick 律速だから

`evaluate` が 17× 速くなっても公開フレーム数は 24.9 fps のまま。この形は
30 fps の tick で駆動していて、90 フレームを 3.0 秒で回すループの
**tick ジッタ（15 フレーム分の取りこぼし）が上限**になっている。
評価側の余裕が増えたことは `evaluate` 合計 2849 → 175 ms に出ている。
再生レートそのものを上げるには第4段（メディア・スクラブ）側の tick 経路を見る必要がある。

#### 追加された submit 回数（MED-GPU-01 向けの記録）

10 レイヤーで merge が GPU 経路に移った分、評価1回あたり
**dispatch と submit がさらに 9 回増える**（merge ノードは 10 個だが、
最下段レイヤーの merge は片側のみでパススルーするため dispatch しない）。
GPUCOMP-2/3/4 の +20 と合わせて、変更前の CPU シェルチェーン基準で
**評価1回あたり +29 回**。`gpu_upload` は 10 回 / 41.9 MB のまま変わらない。
MED-GPU-01 に送る数字はこれで確定（第2段の GPU 化による増分の合計）。

### 宣言的ディスパッチとフレームバッチング投入後（GPUBK-2 / MED-GPU-01）

計測日: 2026-08-03。環境: Apple M5 / macOS 26.3 / release ビルド / 512×512。
7 コンピュートノードのディスパッチを宣言的 API に畳み、ユニフォームバッファ
（内容ハッシュ）とバインドグループ（パイプライン・テクスチャ・ユニフォームの
同一性）を再利用し、**1 フレーム 1 コマンドエンコーダ**にまとめた。
submit はリードバック（アプリではビューア境界の1回）・`GpuContext::wait`・
明示 `flush`・バッチ上限（64 dispatch）のいずれかでだけ発生する。
変更前は直前コミット `7622c75`（GPUBK-1 マーク時点）を同じ worktree で
同日に計測した。ハーネスには `dispatch submits` の出力を追加し、
`GpuContext::dispatch_stats()` の差分を完成評価あたりで割っている。

10 レイヤー / 30 fps 再生形（`(g)`、完成評価 変更前 83 回 / 変更後 82 回）:

| 指標 | 変更前 `7622c75` | 変更後 | 変化 |
|------|-----------------|--------|------|
| dispatch submits / 完成評価 | **29**（定常。上記 GPUCOMP-5/6 の確定値） | **0.48**（39 / 82） | **−98%** |
| `evaluate` 合計 | 199.27 ms | **167.91 ms** | **−16%** |
| `evaluate` 1回あたり | 0.218 ms | 0.186 ms | −15% |
| `node_process:blur` | 1.163 ms/回 | **0.518 ms/回** | 2.2×（2 submit が消えた分） |
| `node_process:comp.opacity` | 0.035 ms/回 | 0.011 ms/回 | 3× |
| `node_process:comp.transform` | 0.057 ms/回 | 0.049 ms/回 | −14% |
| `node_process:comp.merge.add` | 0.033 ms/回 | 0.018 ms/回 | −45% |
| `node_process:comp.merge.normal` | 0.377 ms/回 | 0.453 ms/回 | 悪化（下記） |
| 評価レート | 27.4 fps | 27.2 fps | 変化なし（tick 律速） |
| uploads / readbacks | 93 / 0 | 92 / 0 | 変化なし |

10 レイヤー / スクラブ形（`(f)`、完成評価1回）:

| 指標 | 変更前 | 変更後 |
|------|--------|--------|
| dispatch submits | 49（定常 29 + コールドの blur 2 パス × 10 レイヤー） | **1** |
| `evaluate` 合計 | 16.89 ms | **14.05 ms** |
| end-to-end / 90 ticks | 13.69 ms | 21.37 ms（下記） |

3 レイヤー / 再生形でも同じ形（submits 12 / 82 評価 = **0.15 / 評価**）。

#### submits / 評価が 1 ちょうどでない理由

`perf_baseline` の `BenchHooks` は `finalize` を持たず（GPUCOMP-5/6 の
記録のとおり）再生中にリードバックが発生しないため、フレーム境界の
flush が無い。バッチは 64 dispatch の上限で自己 flush するので、
29 dispatch / 評価に対し約 2.2 評価ごとに 1 submit となる（0.48 の実体）。
**アプリ側では `GpuEvalHooks::finalize` のビューアリードバックがフレーム
ごとの flush 点になり、正確に 1 submit / フレーム**になる。こちらは
`ravel-nodes/tests/dispatch_batching.rs` の
`a_frame_of_gpu_nodes_submits_once`（4 dispatch → readback で submit 1）が
直接 pin している。

#### merge.normal の悪化について

変更前から `comp.merge.normal` だけは兄弟モードの 10 倍の外れ値だった
（0.377 vs 0.033–0.035 ms/回）。バッチ化でチェーン中の同期点が消え、
flush 直後の記録位置がこのノードに載るようになったものと読んでいる
（GPUCOMP-5/6 で transform / opacity に起きたのと同じキュー圧の移動）。
絶対値は評価1回あたり +0.08 ms で、`evaluate` 合計は −16% 改善しており、
トータルでは回収の方が大きい。

#### (f) の end-to-end が伸びた理由

スクラブ形の完成評価は latest-wins で 1 回だけで、`evaluate` 合計は
16.89 → 14.05 ms に減っている。end-to-end（投函〜完了待ち＋`gpu.wait`）
の差は tick タイミングのジッタで、再実行で変動する範囲。
`dispatch submits: 1` がこの形の完了条件を直接示している。

#### 再利用の定量

`dispatch_stats()` の `uniform_buffers_created` / `bind_groups_created` は、
同一パラメータの連続評価でどちらも 0 になることを
`identical_parameters_create_no_new_bind_groups_or_uniforms` が pin する
（プールテクスチャの役割ローテーションで最初の数評価は新規作成され、
作業集合が回り切った定常状態で 0 になる）。

## ジオメトリ評価スケーリング baseline（GPU-0 / パーティクル / 3D の判断用）

計画: `gpu-resident-geometry-plan.md` Phase 0。計測日: 2026-07-29。
環境: Apple M5 / macOS 26.3 / release / 512×512。
ハーネス: 同 `perf_baseline.rs`（`# GPU-0` と `# Particle proxy` 節）。

**全シナリオを未キャッシュで測っている。** 毎フレーム `scatter.grid` の
`spacing_x` を動かし `mark_dirty` するので、チェーン全体が毎フレーム再評価
される（＝変調をアニメーションさせた実使用に相当）。シナリオ (c) の
0.007 ms が**キャッシュ温の数字**でしかなかった点への対応。

`raster_flatten` / `raster_upload` / `raster_submit` は今回
`rasterize` の GPU 経路に入れた span。`node_process:rasterize` はこの3つを
含む（二重計上しない）。

### 構成

| # | チェーン | 出力 |
|---|---|---|
| A | `shape.rect → scatter.grid` | Geometry |
| B | A + `field.falloff → field.apply(P)` | Geometry |
| C | B + `(falloff + noise) × attribute(index) → field.apply(P)` | Geometry |
| D | C + `rasterize`（GPU）| GpuFrameBuffer |

`field.apply` は instance ドメインの `P` 列に書く（`MOD-*` が未完のため
手組みのフィールドチェーンで代用。計画が想定した代替手段）。

### 結果（ms / フレーム、すべて未キャッシュ）

| stage | 要素数 | wall | node_process | flatten | upload | submit |
|---|---|---|---|---|---|---|
| A | 500 | 0.00 | 0.00 | — | — | — |
| A | 10k | 0.01 | 0.01 | — | — | — |
| A | 100k | 0.07 | 0.07 | — | — | — |
| A | 1M | 1.31 | 1.31 | — | — | — |
| B | 500 | 0.01 | 0.00 | — | — | — |
| B | 10k | 0.05 | 0.05 | — | — | — |
| B | 100k | 0.48 | 0.47 | — | — | — |
| B | 1M | 5.13 | 5.13 | — | — | — |
| C | 500 | 0.01 | 0.01 | — | — | — |
| C | 10k | 0.13 | 0.13 | — | — | — |
| C | 100k | 1.17 | 1.17 | — | — | — |
| C | 1M | 11.92 | 11.91 | — | — | — |
| D | 500 | 2.44 | 0.13 | 0.04 | 0.04 | 0.03 |
| D | 10k | 5.33 | 1.19 | 0.88 | 0.13 | 0.03 |
| D | **100k** | **18.24** | 11.32 | **8.75** | 1.20 | 0.07 |
| D | 1M | 164.30 | 143.21 | 104.35 | 17.08 | 0.14 |

D の 100k の内訳（`evaluate` 11.33 ms の分解）:

| 内訳 | ms | 比 |
|---|---|---|
| `raster_flatten`（CPU でのインスタンス展開） | 8.75 | 77% |
| `raster_upload`（storage buffer 生成） | 1.20 | 11% |
| `field.apply`（3段フィールドを全要素サンプル） | 1.15 | 10% |
| `scatter.grid`（生成） | 0.14 | 1% |
| `raster_submit` | 0.07 | 1% |

wall 18.24 − evaluate 11.33 = **6.91 ms が GPU 完了待ち**（512×512 に
10 万 quad を重ね描きする分）。

### 直列化とパイプライン化

上表は毎フレーム `gpu.wait()` するので CPU と GPU が直列になる。待たずに
回し最後に1回だけ待つ形も測った:

| stage | 要素数 | 直列（wall） | パイプライン | CPU のみ |
|---|---|---|---|---|
| D | 500 | 2.44 | 0.51 | 0.13 |
| D | 10k | 5.33 | 3.69 | 1.19 |
| D | **100k** | **18.24** | **13.59** | 11.32 |
| D | 1M | 164.30 | 258.38 | 143.21 |

1M でパイプライン化が**遅くなる**のは、待たずに 5 フレーム分の
100 MB 級バッファを積むためにメモリ圧が上がるから。100k までは
パイプライン化が効く。

### 実行間のばらつき（5 回実行）

絶対値はマシンの状態で最大 1.7 倍ぶれる。**比率はぶれない。**

| 実行 | D / 100k 直列 | 同 パイプライン | `flatten` が CPU 側に占める比 |
|---|---|---|---|
| 1 | 19.51 | — | 77% |
| 2 | 18.24 | 13.59 | 77% |
| 3 | 31.36 | 15.02 | 79% |
| 4 | 25.60 | 19.54 | 76% |
| 5 | 19.91 | 12.64 | 75% |

上の各表は**最も軽かった実行（2 回目）**の値。判断に使う行は
**5 回すべてで予算 16.6 ms を超えている**（18.24〜31.36 ms）ので、
ばらつきは結論を変えない。パイプライン化した値は 12.64〜19.54 ms で、
**予算を跨いでいる**（4 回中 2 回が超過）。

### 判断: 実施（`GPU-0` = 実施）

計画の基準は「D の end-to-end が 10 万インスタンスで 16.6 ms を超えるなら
実施」。**5 回すべてで超過**（最速の実行でも 18.24 ms、min 16.82 ms）。
パイプライン化すると最速の実行では 13.59 ms で予算内に入るが、これは
**512×512・単一レイヤー・シェル合成なしの条件**で、1080p では GPU 側の
フラグメントコストが約 8 倍になり、レイヤーが増えれば CPU 側も比例する。
遅い実行ではパイプライン化しても 19.54 ms で超過する。

**ただし優先順は計画の想定と違う。**

- 計画は「CPU 評価 vs アップロード」で単位の優先を決める想定だった。
  実測では**どちらでもなく `raster_flatten`（CPU でのインスタンス展開）が
  支配的**（100k で 77%）。
- したがって「アップロードの差分化だけ（単位 1 のみ）に縮小」は**足りない**。
  100k で回収できるのは 18.24 ms のうち 1.20 ms。
- CPU フィールド評価（C の 100k で 1.17 ms）は 100k では問題ではない。
  1M で 11.92 ms になるので、WGSL フィールド評価（単位 2）が効くのは
  50 万要素超、またはフィールド段数が増えたとき。

## パーティクルの CPU / GPU 判断（`particle-plan.md` 単位 6）

`particle.simulate` は未実装なので、**同じ属性列に対する明示 Euler の
手書きステップ**を代用にした（proxy であることを明記する）。
点ドメイン（プリミティブなし）＝ラスタライザが円スプライトで描く形。

### 結果（ms / フレーム）

| 点数 | ステップ | wall（直列） | パイプライン | step のみ | flatten | upload |
|---|---|---|---|---|---|---|
| 10k | serial | 1.63 | 0.37 | 0.05 | 0.15 | 0.13 |
| 10k | rayon | 1.81 | 0.69 | 0.17 | 0.18 | 0.13 |
| 100k | serial | 5.09 | 4.02 | 0.41 | 2.29 | 0.99 |
| 100k | rayon | 6.53 | 3.60 | 0.35 | 2.40 | 0.98 |
| 1M | serial | 44.25 | 32.80 | 4.04 | 20.83 | 9.24 |
| 1M | rayon | 37.20 | 33.30 | 1.01 | 19.10 | 8.11 |

上表の「step のみ」は描画ループの中で計った値で、ジオメトリを毎フレーム
`Arc` に包んで渡すため属性列が一時的に共有される。**共有の影響を排除した
ステップ単体**も別に測った（描画に一切渡さない状態で回す）:

| 点数 | serial ms | rayon ms |
|---|---|---|
| 10k | 0.04 | 0.07〜0.11 |
| 100k | 0.38〜0.45 | **0.16〜0.27** |
| 1M | 4.15〜5.32 | 1.19〜1.57 |

**ステップ自体は安い。** 10 万点で rayon 0.2 ms 前後（予算の 1〜2%）、
100 万点でも 1.2〜1.6 ms。10k で rayon が serial より遅いのは、この規模では
スレッド起動コストが仕事量を上回るため。

**律速は描画経路**。10 万点で `flatten` 2.29 ms + `upload` 0.99 ms で、
ステップの 10 倍以上。インスタンス（D の 8.75 ms / 100k）より安いのは、
点はソースジオメトリを展開しないため（インスタンス展開が約 4 倍高い）。

### GPU 常駐状態の読み戻し

ステージングバッファは**計測区間の外で 1 回だけ確保**している（毎フレーム
読み戻す実装はバッファを使い回すため）。マップ後は 64 バイトおきに走査して
実際にページを触っている。

| 点数 | バイト数 | 読み戻し ms（3 回実行） |
|---|---|---|
| 10k | 160 KB | 1.28〜1.37 |
| 100k | 1.6 MB | 1.34〜1.36 |
| 1M | 16 MB | 1.91〜2.14 |

**1.6 MB までは submit + map の固定レイテンシ（≈1.3 ms）が支配的**で
サイズに依らない。16 MB では +0.7 ms 増えるので、そこから先はサイズにも
効く（増分だけ見れば約 23 GB/s）。GPU sim の状態を毎フレーム CPU 側へ
戻すコストは、10 万点で 16.6 ms 予算の約 8%、100 万点で約 12%。

### 判断: 単位 6（GPU sim）の根拠はステップコストではない

`particle-plan.md` は単位 6 の完了条件に「10 万点で 1 フレームの step が
16.6 ms を大きく下回ること」を置いているが、**CPU 側が既に 0.2 ms 前後で
下回っている**。よって:

- **単位 1〜5 を CPU（rayon）で作る方針は実測で裏付けられた。** 10 万点なら
  CPU ステップで 60fps 予算の 1〜2% しか使わない。
- **GPU sim の価値はステップの高速化ではなく、状態を VRAM 常駐にして
  `flatten` + `upload`（10 万点で 3.3 ms、100 万点で 28 ms）を消すこと。**
  つまり単位 6 の実質的な前提は `GPU-1`（`GpuGeometry`）であり、
  計画がそう書いているとおり。ステップの WGSL 化は副産物。
- 読み戻しが 10 万点で ≈1.35 ms（固定レイテンシ律速）なので、
  「スクラブ中は CPU / 再生中は GPU」の使い分けは帯域面では成立する。
  100 万点だと ≈2.0 ms でサイズにも効き始める。VRAM とキャッシュの衝突
  （同計画の未解決節）は別問題として残る。

## 3D の頂点アップロード（`3d-scene-plan.md` 単位 4）

同計画は「GPU 常駐ジオメトリを前提にせず、`scene.render` の内部で頂点
バッファへアップロードする」と決めている。その前提が成立する規模を
`raster_upload` のスループットから読む。

| 転送 | 実測 |
|---|---|
| アップロード（6.4 MB 以上） | 6.5〜7.9 GB/s |
| アップロード（0.6 MB） | 約 5.0 GB/s（固定コストの比率が上がる） |
| 読み戻し（buffer → CPU） | 16 MB で 1.41 ms（実効 11 GB/s）。1.6 MB でも 1.32 ms なので**固定レイテンシ律速**で、帯域は測れていない |

頂点 32 B（位置 + 法線 + UV 相当）換算、上表の下限 6.5 GB/s で計算:

| 頂点数 / フレーム | アップロード ms |
|---|---|
| 10 万（3.2 MB） | ≈0.5 |
| 50 万（16 MB） | ≈2.5 |
| 100 万（32 MB） | ≈4.9 |

**数十万頂点までは毎フレームアップロードで成立する**（16.6 ms 予算の
3〜15%）。100 万頂点を毎フレーム上げると単一メッシュで予算の 30% を使う。
`3D-4` の実装時に**静的メッシュを毎フレーム上げ直さない**
（変更のないメッシュはバッファを保持する）ことを条件に入れれば、
`GPU-1` を待たずに済む。この点で 3D を `GPU-0` に依存させない判断は妥当。

## 制約・未計測（ジオメトリスケーリング）

- 512×512 での計測。1080p ではフラグメント側（D の 6.91 ms）が約 8 倍に
  なるが、CPU 側（`flatten` / `upload` / フィールド評価）は解像度に依存しない。
- 5 回実行。絶対値は最大 1.7 倍ぶれる（D の 100k で 18.24〜31.36 ms）。
  各表は最も軽かった実行の値で、比率は実行間で安定している。
- パーティクルのステップは proxy。実際の `particle.simulate` は発生・削除・
  寿命処理を含むので、ステップコストは上表より増える。
- 3D の表は `raster_upload` のスループットからの換算で、三角形レンダラ自体
  （`Primitive::Mesh` は未実装）は測っていない。
- GPU 時間は wall − CPU span の差分。タイムスタンプクエリは未使用。
