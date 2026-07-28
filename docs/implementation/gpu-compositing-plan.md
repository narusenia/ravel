# GPU 合成パイプライン — 描画1回あたりのコスト削減計画（もっさり第2段）

対象 issue: [HIGH-05](../../issues/high/HIGH-05-shell-chain-cpu-per-pixel.md),
[HIGH-04](../../issues/high/HIGH-04-per-frame-blocking-readback.md),
[HIGH-08](../../issues/high/HIGH-08-ui-thread-f32-to-bgra-conversion.md),
[HIGH-09](../../issues/high/HIGH-09-viewer-gpu-cpu-gpu-roundtrip.md),
[MED-GPU-02](../../issues/medium/gpu-nodes.md)

第1段（`done/ui-responsiveness-plan.md`、RESP-1〜3 / PR #191 #192 #193 #195）は
評価と再構築の**回数**を削った。本計画は**1回あたりのコスト**を削る。

## 背景 — なぜ第2段が本命なのか

第1段完了時点の実測（`perf-baseline.md`「RESP-3 完了時」、2026-07-28、
Apple M5 / 512×512）では、シナリオ (b) の 1 tick 1.02〜1.17 ms のうち
`register_processors` は 0.015 ms まで落ちている。残りはほぼ `gpu_upload`
（0.47 ms × 2）と GPU ノードの `node_process`。

`issues/README.md` と HIGH-06 は「パイプライン再コンパイルが編集中の体感の主因」と
書いていたが、測定はそれを支持しなかった。主因は本計画の側にある。

### 1. シェル合成チェーンが CPU per-pixel（HIGH-05）

`comp.transform` / `comp.opacity` / `comp.merge.*` の3プロセッサはすべて
`gpu_util::ensure_cpu` を呼ぶ。入力が GPU 常駐なら `to_frame_buffer()` =
ブロッキングリードバックが**レイヤーごと・フレームごと**に走り、
その後 `ctx.resolution` 全域を単一スレッドのスカラーループで処理する
（`comp/transform.rs:70-85`, `comp/opacity.rs:51-60`, `comp/merge.rs:113-131`,
`:187-207`）。

レイヤーネットワークが GPU ノードで終わる N レイヤー構成では、
毎フレーム N 回のリードバックと約 3N 回の全フレーム CPU ループになる。
さらに出力が CPU `FrameBuffer` なので、以降は何も GPU 常駐で残らない。
`VIEWER_MAX_DIM = 1024`（`project_state.rs:42-47`）はこの制約への譲歩で、
コメント自身がそう認めている。

### 2. リードバック実装そのもの（HIGH-04）

`transfer::read_texture` は呼び出しごとに `MAP_READ` ステージングバッファを新規作成し
（`transfer.rs:162`）、`ctx.wait()` = `device.poll(wait_indefinitely)` で
**そのコピーだけでなく、それ以前に submit した全デバイス作業**の完了を待ち
（`transfer.rs:205`）、行単位で `Vec<u8>` に再パックする。
`GpuFrameBuffer::to_frame_buffer` がさらに `bytemuck::cast_slice(&raw).to_vec()` で
**2度目の**全バッファコピーを行う（`frame.rs:137`）。

### 3. UI スレッドの全フレーム色変換と往復（HIGH-08 / HIGH-09）

`ViewerFrame` observer から、メインスレッド上で
`frame_buffer_to_render_image`（`viewer.rs:1826-1856`）がフレーム全域の
`Vec::push` ループで f32→BGRA 変換し、`ImageBuffer` と `RenderImage` を新規確保する。
`ImageSource::Render` は gpui の画像キャッシュを通らないので、
前フレームは `cx.drop_image` で明示破棄される（`viewer.rs:280-287`）= アトラス churn。

## 着手前に確定させた事実（issue の記述より正確）

実際にシェーダと CPU 実装を突き合わせた結果、issue の見立てには誤差があった。
以下は本計画の前提として扱う。

### `transform.wgsl` は shell transform の drop-in ではない

- **境界外の扱いは issue の記述より近い**。`transform.wgsl:57-61` は
  ソース矩形外を既に透明で返している。差分は**バイリニア4タップの clamp**だけ
  （`:28-31`）で、CPU 側（`comp/transform.rs:127-134`）はタップ単位で
  範囲外を透明にする。結果はソース端 1px の挙動差 —
  「シェーダは端を引き伸ばし、CPU は端で透明へフェードする」。
- **アルファ規約が違う**。`transform.wgsl:18-38` の `bilinear_sample` は
  **直線アルファ**のテクセルを `mix` する。CPU の `sample_bilinear` は
  **乗算済みアルファ**で補間してから戻す（「フリンジを避けるため」と明記）。
  そのまま使うとアルファ境界に暗いハローが出て CPU 版と不一致になる。
  これが MED-GPU-02 で、「HIGH-05 の GPU 版実装時に併せて対処するのが効率的」と
  起票済み。その判断に従う。
- **入出力サイズの前提が違う**。シェーダは dispatch 範囲を
  `textureDimensions(input_tex)` で束縛し、ソース寸法を `params.width/height` で
  別に受ける = 入力と出力が同サイズという前提。シェルの transform は
  **出力が `ctx.resolution`、入力は任意サイズ**（メディアレイヤーは素材の
  ネイティブ寸法を持ち得る）。dispatch を出力サイズで回す形に直す必要がある。

### `merge.wgsl` はモードが足りず、合成式の形も違う

- シェーダは **over / add / multiply の3つ**（`params.operation` 0/1/2）。
  シェルが必要なのは **6つ**: `Normal` / `Add` / `Multiply` / `Screen` /
  `Overlay` / `Adjustment`（`comp/merge.rs:23-42`）。
- CPU 側は「per-channel blend `B(Cb, Cf)` を直線色に適用してから Porter-Duff」の
  **2段構造**（`comp/merge.rs:45-61`, `:142-157`）。シェーダは各モードに
  合成式を直書きしていて、`add` は Porter-Duff を通さない単純な rgb 加算。
  **シェーダを CPU 側の2段構造に書き直す**のが筋で、既存の3モードをそのまま
  拡張するのではない。
- **サイズ不一致と片側欠落**を扱えない。`merge.rs:106-114` は寸法一致を要求するが、
  シェルの `pixel_at`（`comp/merge.rs:244-250`）は範囲外を透明として読み、
  結果として padding / crop になる。欠落側は `empty_frame()` =
  **0×0 の `FrameBuffer`**（`:240-242`）で表現されている。
  **0×0 テクスチャは作れない**ので、GPU 版は「その入力は存在しない」を
  uniform のフラグで表現する必要がある。
- `Adjustment` はピクセル単位の合成でなく**フレーム全体の mix**
  （`comp/merge.rs:187-207`）。しかも**乗算済みアルファ**で mix して戻す。
  `merge.wgsl:56` の `mix(b, result, params.mix_val)` は**直線アルファ**の mix で、
  同じものではない。

### `comp.opacity` は自明

`comp/opacity.rs:51-60`。アルファ 1 チャンネルの乗算のみ。1行シェーダで済む。

### CPU 実装の存在意義は「アダプタ無しのフォールバック」ではない

HIGH-05 は「CPU 実装はアダプタ無し環境のフォールバックとして残す」と書いているが、
アプリは `project_state.rs:196` で
`GpuContext::new_blocking().expect("GPU context initialization failed")` を呼び、
アダプタが無ければ起動時に panic する。`processor_for_node` の署名も
`&GpuContext` を必須で取る。**現状フォールバック経路は存在しない**。

CPU 実装を残す本当の理由は**リファレンス経路**であること:
`shape_layer_golden.rs` は CPU リファレンスラスタライザを明示登録して
ピクセルを pin している（同ファイル冒頭コメント、`lib.rs:109-114` の
`node.metadata.synthetic` 分岐も同じ意図）。この土台を壊さない。
よって**残すが、既定経路ではなく明示登録で選ばれる参照実装として残す**。

### 既存テストへの影響範囲

- `crates/ravel-nodes/tests/layer_network.rs` の **26 テストすべて**が
  `register_all_processors` 経由（`:73-80`）。既定を GPU に切り替えると
  26本すべてがそのまま GPU 経路に移る。閾値比較が多いので大半は通るはずだが、
  **1本ずつ許容誤差を確認する**。通ることを確認せずに単位を閉じない。
- `shape_layer_golden.rs` の1本目
  （`shape_layer_network_rasterizes_rect_pixels`）は、
  identity transform → 早期リターン（`comp/transform.rs:62-64`）、
  opacity 1.0 → 早期リターン（`comp/opacity.rs:47-49`）、
  片側のみの merge → パススルー（`comp/merge.rs:96-105`）で
  **シェル3ノードすべてが短絡している**。GPU 版でも同じ短絡を保てば
  ピクセルは不変。2本目（`..._scales_comp_coordinates_without_cropping`）は
  comp→canvas スケールが入るので transform が実走する。
- `perf_baseline` は**シェルチェーンを一切通していない**
  （`examples/perf_baseline.rs` に `compile_composition` / `comp.transform` /
  `comp.merge` の出現ゼロ。グラフを直に組んで評価している）。
  つまり現ハーネスでは HIGH-05 の効果を測れない。**シナリオ追加が最初の作業**。

## 目標アーキテクチャ

```text
現在:
 layer 1 network (GPU 常駐) ─▶ readback ─▶ comp.transform(CPU) ─▶ comp.opacity(CPU) ─┐
 layer 2 network (GPU 常駐) ─▶ readback ─▶ …(CPU)                                    ├─▶ comp.merge(CPU) ─▶ … ─▶ finalize ─▶ readback は不要（既に CPU）
 …                                                                                    ┘
 → フレームあたり readback N 回、CPU 全域ループ 約 3N 回、以降 GPU 常駐ゼロ

目標:
 layer 1 network (GPU 常駐) ─▶ comp.transform(GPU) ─▶ comp.opacity(GPU) ─┐
 layer 2 network (GPU 常駐) ─▶ …(GPU)                                    ├─▶ comp.merge(GPU) ─▶ … ─▶ finalize
 …                                                                        ┘                              └─▶ readback 1 回 + BGRA 変換（ワーカースレッド）
 → フレームあたり readback 1 回、CPU 全域ループ 0 回、UI スレッドは完成済みバイト列を包むだけ
```

規約は既存どおり**直線アルファ**（`rasterize/mod.rs` と `merge.wgsl` に明記）。
フィルタリング（バイリニア補間・ブラーの畳み込み）だけをロード時 premultiply →
ストア時 un-premultiply で行う。これが MED-GPU-02 の修正方針そのもの。

処理の選択は `processor_for_node`（`lib.rs:96-191`）の単一 match で行う。
GPU 版を既定にし、CPU 実装は `pub` のまま残してテストが明示登録できるようにする
（`rasterize` の `node.metadata.synthetic` 分岐と同じ構図）。

## スコープ外（判断込み）

- **MED-GPU-01（`GpuTask` バッチング）**。シェルチェーン GPU 化でノード数が増え
  submit 回数も増えるため一緒に潰す価値はあるが、スコープが膨らむので分ける。
  代わりに GPUCOMP-1 のハーネスで **submit 回数相当（`gpu_upload` / dispatch 数）を
  記録し、増分を計画完了時に `perf-baseline.md` へ明記する**。
  数字を出したうえで別 PR に送る。
- **ゼロコピー Viewer 表示**（HIGH-09 の「本質」側）。GPUI は自前の wgpu デバイスを
  持つため、共有テクスチャ表示はデバイス間 interop の設計が要る。
  GPUCOMP-9 の測定後に判断する（GPUCOMP-11）。
- **MED-GPU-03（ブラー半径クランプ）/ MED-GPU-04 / MED-GPU-05**。
  独立した bug / perf で、本計画の経路変更に依存しない。

## 実装単位

1単位1PR のスタックで進める（第1段で機能した形）。

| ID | 単位 | 対象 issue | 状態 |
|---|---|---|---|
| GPUCOMP-1 | `perf_baseline` に N レイヤーのシェル合成シナリオを追加 | 測定の土台 | ✅ #197 |
| GPUCOMP-2 | `comp.opacity` の GPU 版 | HIGH-05 | 🟡 |
| GPUCOMP-3 | `comp.transform` の GPU 版 + アルファ規約・タップ境界の是正 | HIGH-05, MED-GPU-02 | ⬜ GPUCOMP-2 |
| GPUCOMP-4 | `blur.wgsl` のアルファ規約統一 | MED-GPU-02 | ⬜ GPUCOMP-3 |
| GPUCOMP-5 | `comp.merge.*`（Normal/Add/Multiply/Screen/Overlay）の GPU 版 | HIGH-05 | ⬜ GPUCOMP-3 |
| GPUCOMP-6 | `comp.merge.adjustment` の GPU 版 | HIGH-05 | ⬜ GPUCOMP-5 |
| GPUCOMP-7 | リードバック回数と CPU/GPU 一致の回帰テスト | HIGH-05 検証 | ⬜ GPUCOMP-6 |
| GPUCOMP-8 | リードバック実装の改善（ステージング再利用・二重コピー除去・wait 範囲） | HIGH-04 | ⬜ GPUCOMP-7 |
| GPUCOMP-9 | f32→BGRA 変換を評価ワーカーへ移す | HIGH-08, HIGH-09 | ⬜ GPUCOMP-8 |
| GPUCOMP-10 | 非同期リードバック（フレーム N の map と N+1 の評価を重ねる） | HIGH-04 | ❓ GPUCOMP-9 の測定待ち |
| GPUCOMP-11 | `VIEWER_MAX_DIM` の引き上げ / ゼロコピー表示の判断 | HIGH-09 | ❓ GPUCOMP-9 の測定待ち |

### GPUCOMP-1 `perf_baseline` に N レイヤーのシェル合成シナリオを追加

これが無いと以降の効果を測れない。**最初にやる**。

- `compile_composition` で N レイヤー（既定 10）の `Composition` を組み、
  各レイヤーのネットワークを「GPU ノードで終わる」形にする
  （`blur` か `color_correct` を末尾に置く。CPU で終わると HIGH-05 を再現しない）
- レイヤーは非 identity transform / opacity < 1 / 複数の merge モードを混ぜる
  （短絡経路だけ測っても意味が無い）
- `EvalService` 経路（既存シナリオ (b'') / (e) と同じ形）で
  スクラブと 30fps 再生の2形で回す
- `report()` は既に `TransferSnapshot` を出しているので、
  **readback 回数がそのまま指標になる**（`before.delta(&after)`）
- `TRACKED_SPANS` に `node_process:comp.transform` などが載るよう
  シェルノードのスパンが出ることを確認する（出ていなければ計装を足す）

**完了条件**

- `cargo run -p ravel-nodes --release --example perf_baseline` が
  新シナリオを出力し、**readback 回数がレイヤー数に比例している**ことを示す
  （= HIGH-05 の再現が取れている）
- 結果を `perf-baseline.md` に日付付きで追記。**過去の記録は書き換えない**

### GPUCOMP-2 `comp.opacity` の GPU 版

いちばん単純な単位で、GPU シェルプロセッサの型・登録・テストの形を確立する。
以降の単位はこの形を踏襲する。

- `shaders/comp_opacity.wgsl`: `textureLoad` → `a *= opacity` → `textureStore`
- `CompOpacityGpuProcessor`（`GpuContext` / `ShaderManager` / `TexturePool` を取る、
  既存 GPU プロセッサと同じ構造）。`rebuild_on_node_change()` は `false`
  （レイヤーは `Document` から process 時に解決するので何も captured しない）
- **短絡を保つ**: `opacity` が 1.0 なら入力をそのまま返す（CPU 版と同じ）。
  入力欠落なら透明フレーム。ここを落とすとゴールデンテストが変わる
- 出力は `GpuFrameBuffer`。入力の適応は `gpu_util::ensure_gpu`
- CPU 版 `CompOpacityProcessor` は残す。`processor_for_node` の
  `"comp.opacity"` を GPU 版に差し替える

**完了条件**

- CPU 版と GPU 版の出力一致テスト（アダプタ無しは既存パターンで skip:
  `GpuContext::new_blocking().ok()`、`crates/ravel-gpu/tests/compute_invert.rs` 参照）
- 短絡ケース（opacity = 1.0）で入力の `Arc` がそのまま返ることのテスト
- `layer_network.rs` の 26 テストが通る
- **修正を外すと落ちることを機械的に確認する**（シェーダの乗算を消す等）

### GPUCOMP-3 `comp.transform` の GPU 版 + アルファ規約・タップ境界の是正

本計画でいちばん間違えやすい単位。焦点は**数式の一致**。

- `shaders/comp_transform.wgsl` を新規に置く。`transform.wgsl` を編集して
  兼用しないこと（出力サイズの前提が違う）
  - dispatch 範囲は**出力サイズ**（`ctx.resolution`）。ソース寸法は uniform で受ける
  - `bilinear_sample` は**乗算済みアルファで補間 → 戻す**
    （`comp/transform.rs:97-125` と同じ手順）
  - 4タップは clamp せず、**タップ単位で範囲外を透明**にする
    （`premultiplied_at` と同じ）
- 併せて既存 `shaders/transform.wgsl` と `MED-GPU-02` の transform 側を是正する。
  共通化は WGSL に include が無いので、Rust 側で snippet を `include_str!` して
  `format!` で前置するか、複製してどちらのファイルにも
  「もう一方と同一である」ことを doc コメントで固定する（どちらでもよい）
- 逆行列は `world_matrix(&comp, layer, ctx).inverse()`（CPU 版と同じ関数）を使う。
  行列計算を GPU 側で再実装しないこと — ビューアの bbox / ヒットテストと
  同じ行列を使う不変条件（`comp/transform.rs` のモジュールコメント）を壊す
- **短絡を保つ**: `matrix.is_identity()` なら入力そのまま、
  `inverse()` が `None`（ゼロスケール）なら透明フレーム

**完了条件**

- CPU / GPU 一致テスト。**アルファ境界を含むソース**（半透明の縁を持つ矩形）で
  比較する。単色不透明フレームだけで比較すると乗算済み補間の差が出ないので
  **偽陽性テストになる**
- 回転・スケール・アンカー移動それぞれの一致
- ソース端 1px の挙動が CPU と一致すること（clamp を残すと落ちるテスト）
- `shape_layer_golden.rs` の2本が通る（1本目は短絡、2本目は実走）
- `layer_network.rs` の 26 テストが通る

### GPUCOMP-4 `blur.wgsl` のアルファ規約統一

MED-GPU-02 の残り半分。GPUCOMP-3 で作った premultiply の形を再利用する。

- 水平パスで premultiply、垂直パスで un-premultiply すれば2パス構成を維持できる
  （issue の修正方針どおり）
- `blur` は GPU ノードなので出力が変わる。`gpu_resident_pipeline.rs` の
  期待値を見直す

**完了条件**

- アルファ境界に暗いハローが出ないことを示すテスト
  （不透明白 + 完全透明の境界をブラーして、暗くならないことを確認）
- 変更前のシェーダだと落ちることを確認
- `gpu_resident_pipeline.rs` が通る

### GPUCOMP-5 `comp.merge.*`（Normal/Add/Multiply/Screen/Overlay）の GPU 版

- `shaders/comp_merge.wgsl` を新規に置く。`merge.wgsl` の拡張ではなく、
  **CPU の2段構造に合わせて書き直す**:
  1. per-channel blend `B(Cb, Cf)`（`comp/merge.rs:45-61` と同じ式）
  2. `mixed = (1 - ab) * Cf + ab * blended`
  3. Porter-Duff: `out = (af * mixed + (1 - af) * ab * Cb) / ao`,
     `ao = af + ab * (1 - af)`、`ao <= 0` なら全 0
- サイズ不一致は**範囲外を透明として読む**（CPU の `pixel_at` と同じ）。
  出力は `ctx.resolution`
- 片側欠落（CPU では 0×0 の `empty_frame`）は uniform のフラグで表現する。
  0×0 テクスチャは作れない。ダミー 1×1 透明テクスチャ + フラグでもよいが、
  **どちらを選んだかコメントで固定する**
- **短絡を保つ**: 両側欠落 → 透明、片側のみで寸法が `ctx.resolution` 一致 →
  そのままパススルー、非フレーム値（スカラープローブ等）→ パススルー
  （`comp/merge.rs:93-111`。ここを落とすとゴールデンテストと
  `layer_network.rs` が変わる）

**完了条件**

- 5モード × CPU/GPU 一致テスト。**半透明の前景 × 半透明の背景**で比較する
  （どちらも不透明だと Porter-Duff の分母が 1 になり、
  2段構造の誤りが出ない = 偽陽性）
- `Overlay` は背景 0.5 の両側（`cb <= 0.5` の分岐）を踏むテスト
- サイズ不一致（前景が小さい / 大きい）で padding / crop が CPU と一致
- 片側欠落の3ケース（背景のみ / 前景のみ / 両方無し）
- `layer_network.rs` の 26 テストが通る

### GPUCOMP-6 `comp.merge.adjustment` の GPU 版

ピクセル単位の合成ではなくフレーム全体の mix なので、別扱いが必要。

- **乗算済みアルファで mix して戻す**（`comp/merge.rs:193-201`）。
  `merge.wgsl:56` の直線アルファ mix を流用しないこと
- 強度（= レイヤー opacity）と表示区間の判定は `adjustment_strength`
  （`comp/merge.rs:212-226`）をそのまま使う。GPU 側で再実装しない
- **短絡を保つ**: 表示区間外 / 強度 0 → 背景そのまま、
  強度 1.0 かつ寸法一致 → 前景そのまま

**完了条件**

- CPU / GPU 一致テスト（半透明背景 × 半透明調整結果、強度 0.5）
- 短絡3ケース（区間外 / 強度 0 / 強度 1）
- 直線アルファで mix する実装だと落ちるテスト

### GPUCOMP-7 リードバック回数と CPU/GPU 一致の回帰テスト

単位ごとの一致テストとは別に、**チェーン全体**を pin する。
`tests/gpu_resident_pipeline.rs` と `tests/shape_layer_golden.rs` を土台にする。

- 10 レイヤー構成（GPU ノードで終わるネットワーク、非 identity transform、
  opacity < 1、複数 merge モード）を `compile_composition` で組み、
  1 フレーム評価して `TransferCounters` の readback delta が
  **finalize の 1 回だけ**であることをアサートする
- 同じ構成を CPU プロセッサ明示登録で評価し、GPU 経路の出力と
  ピクセル比較する（許容誤差を明記する）

**完了条件**

- readback 回数 = 1 のテスト。**レイヤー数を変えても 1 のまま**であること
  （N に比例するなら CPU 経路が残っている）
- CPU / GPU チェーン一致テスト
- どちらのテストも、GPU シェルプロセッサの登録を CPU に戻すと落ちる

### GPUCOMP-8 リードバック実装の改善（HIGH-04）

ここまでで readback は 1 回になっているので、次はその 1 回を速くする。

- ステージングバッファをサイズ別にプールする（`TexturePool` と同じ発想。
  `transfer.rs:162` の毎回 `create_buffer` を消す）
- `frame.rs:137` の `cast_slice(&raw).to_vec()` を消す。
  `read_texture` が `Vec<f32>` に直接読む形にするか、
  行アンパックの出力先を呼び出し側が渡す形にする
- `ctx.wait()`（デバイス全体待ち）の範囲を、可能なら submit index 指定の
  待ちに絞る（wgpu の `PollType::WaitForSubmissionIndex`）

**完了条件**

- ステージングバッファのアロケーション回数がフレーム数に比例しないテスト
  （プールのカウンタで検証）
- 1080p / 4K でのフレームあたりリードバック時間の前後比較を記録
- バイトコピー量が半減していること（`to_vec` の除去分）

### GPUCOMP-9 f32→BGRA 変換を評価ワーカーへ移す（HIGH-08 / HIGH-09）

- `frame_buffer_to_render_image` の変換を `GpuEvalHooks::finalize`
  （既にリードバックを所有している）またはバックグラウンドタスクへ移す。
  UI スレッドは完成済み BGRA バイト列を `RenderImage` で包むだけにする
- バイトごとの `Vec::push` をやめ、事前確保・再利用バッファへ書き込む
- `ViewerFrame` が運ぶ型を BGRA バイト列（+ 寸法）に変える。
  `viewer_content` / `ViewerContent` の経路もそれに合わせる

**完了条件**

- 変換がワーカースレッドで実行されていることをスレッド ID で確認するテスト
- 再生中のメインスレッド占有時間の前後比較
- 変換結果のピクセルが変わらないこと（既存の変換と同一出力）
- アトラス churn（`drop_image`）の扱いが変わらないこと —
  `ImageSource::Render` は gpui のキャッシュを通らないので、
  明示破棄を落とすと VRAM リークになる（`viewer.rs:280-287` のコメント）

### GPUCOMP-10 非同期リードバック（❓測定ゲート）

フレーム N のリードバック完了待ちとフレーム N+1 の評価を重ねる。
`EvalService` のライフサイクルに触るので、**GPUCOMP-9 完了時点の測定で
リードバック待ちが依然として支配的な場合のみ**着手する。
着手判断の根拠を `perf-baseline.md` に書く。

### GPUCOMP-11 `VIEWER_MAX_DIM` の引き上げ / ゼロコピー表示の判断（❓測定ゲート）

`VIEWER_MAX_DIM = 1024` は「シェル合成が CPU でレイヤーごとに readback する」ことへの
譲歩として入っている（`project_state.rs:42-47`）。その前提が消えるので、
**上げても再生レートが維持できるか測って決める**。
維持できないならボトルネックを特定し、ゼロコピー表示（GPUI カスタム要素 /
デバイス間 interop）を別計画に切る。

## 検証

- `mise run check`（fmt / パターン lint / clippy -D warnings / workspace テスト）
- GPU テストはアダプタが必要。`GpuContext::new_blocking().ok()` で skip する
  既存パターンに合わせる（`crates/ravel-gpu/tests/compute_invert.rs`）
- 単位ごとに `ravel-review` を通してから PR を出す
- **テストは「修正を外すと落ちる」ことを機械的に確認する**。
  第1段ではこれで2回、通ってしまう（= 何も検証していない）テストを見つけた

### 計画全体の完了条件

- **10レイヤー構成でフレームあたりのリードバック回数が 1**
  （`ravel_gpu::transfer::stats::TransferCounters::snapshot()` / `delta()`）
- **CPU / GPU 実装の出力一致テスト**が単位ごととチェーン全体の両方にある
- **`perf_baseline` の前後比較**。変更前コミットを `git worktree` に並べ、
  **同一機体・同一ツリー状態**で各3回走らせる
  （`cargo run -p ravel-nodes --release --example perf_baseline`）
- 結果は `perf-baseline.md` に日付付きで追記。**過去の記録を書き換えない**
  （第1段で 2026-07-17 の記録が Phase 2〜4 の GPU 常駐化で無効になっていたことに
  気づけたのは、記録が残っていたから）
- MED-GPU-01 に送る submit 回数の増分を数字で記録する

## 落とし穴

- スタック PR: `.github/workflows/ci.yml` は `pull_request: branches: [main]` なので、
  下の PR をマージして base が main に自動リターゲットされても **CI が起動しない**。
  CodeRabbit も同様。**close → reopen で発火する**（ツリーを触らないので
  review-gate マーカーは生きる）。CI は Windows が遅い（macOS 8分 / Windows 20分超）
- 新しい `git worktree` では `mise trust` を先に1回実行する。
  していないと `mise run` が全部 `Config files … are not trusted` で落ちる
- `crates/ravel-app/src/panels/mod.rs` のテストモジュールで `use super::*;` を書くと
  **rustc 1.95 が SIGBUS で落ちる**（gpui proc macro 内でスタック枯渇）。明示 import なら通る

## 関連

- `issues/README.md`「UI / 描画のもっさり」— 段の定義
- `done/ui-responsiveness-plan.md`（第1段）、`perf-baseline.md`
- `done/eval-render-performance-plan.md`（Phase 2 の GPU 常駐化、Phase 4 の表示）
- `done/layer-network-model-plan.md`、`docs/requirements/REQ-LAYER.md`
- `issues/medium/gpu-nodes.md`（MED-GPU-01/02）
