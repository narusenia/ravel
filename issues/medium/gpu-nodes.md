# medium — ravel-gpu / ravel-nodes（GPU パイプライン・ノード処理）

---

## MED-GPU-01 | perf | ノードディスパッチごとに queue submit（ブラーは2回）、ディスパッチごとにユニフォームバッファとバインドグループを新規作成。`GpuTask` バッチング trait は実装ゼロ

**該当**: `crates/ravel-nodes/src/blur.rs:75-118`, `:147-163`,
`color_correct.rs:88-134`, `merge.rs:120-173`, `transform.rs:130-176`,
`rasterize/mod.rs:286-365`, `crates/ravel-gpu/src/compute.rs:121-128`

全 GPU プロセッサが `process()` 内で `create_buffer_init` によるユニフォームバッファ、
バインドグループ、コマンドエンコーダを作り `queue.submit` を呼ぶ。
ブラーは分離可能2パスでこれを2回やる（1エンコーダで共有できる）。

`GpuTask` の doc コメントは「eval エンジンがディスパッチをフレームあたり1コマンドバッファに
バッチする」と約束しているが、ワークスペース内にこの trait の実装も消費側も存在しない。
M 個の GPU ノード連鎖でフレームあたり M 回超の submit と M 回のバッファ確保。
この粒度の submit オーバーヘッド（検証、フェンス管理）はフレームレートで測定可能なレベル。

**修正方針**: 最低限、ブラーの2パスを1エンコーダ・1 submit に記録し、
プロセッサごとのユニフォームバッファを `queue.write_buffer` で再利用。
望ましくは、プロセッサがフレーム共有エンコーダに記録する `GpuTask` 設計を実装し、
評価あたり1 submit にする。

---

## MED-GPU-02 | bug | `blur.wgsl` と `transform.wgsl` が直線アルファのまま RGBA をフィルタする — アルファ境界に暗いフリンジ、CPU 版と不一致

> **解決済み**: PR #198（2026-07-29）。乗算済みアルファのヘルパを
> `shaders/premultiplied.wgsl` に置き、Rust 側（`gpu_util::with_premultiplied_helpers`）で
> 前置して `blur.wgsl` / `transform.wgsl` / 新規 `comp_transform.wgsl` が共有する。
> ブラーは水平パスで premultiply、垂直パスで un-premultiply して2パス構成を維持。
> transform はタップ単位で範囲外を透明にし、CPU シェル transform と一致させた
> （クランプに戻すと落ちるテストあり）。設計は
> `docs/implementation/gpu-compositing-plan.md`（GPUCOMP-3 / 4）。

**該当**: `crates/ravel-nodes/src/shaders/blur.wgsl:34-49`,
`shaders/transform.wgsl:18-38`（対比: `crates/ravel-nodes/src/comp/transform.rs:94-125`）

パイプラインの規約は直線アルファ（`rasterize/mod.rs` と `merge.wgsl` に明記）。
ブラーは RGB と A を独立に平均するため、完全透明ピクセルの RGB（通常 0）が
隣接する不透明ピクセルの色に重み付けされ、アルファ境界の周囲に暗いハローが出る。
GPU transform の `bilinear_sample` も同じく直線アルファのテクセルを混ぜる。

一方 CPU の `comp.transform` は「フリンジを避けるため」明示的に乗算済みアルファでサンプルし
戻す。結果、同じ操作で GPU transform ノードと CPU シェル transform の境界が視覚的に異なる。

**修正方針**: 両シェーダでロード時に premultiply、フィルタ、ストア時に un-premultiply。
ブラーは水平パスで premultiply、垂直パスで un-premultiply すれば2パスを維持できる。

**関連**: [HIGH-05](../high/HIGH-05-shell-chain-cpu-per-pixel.md) の GPU 版シェル実装時に
併せて対処するのが効率的。

---

## MED-GPU-03 | bug | ブラー半径が未クランプ — 大きな値で per-pixel WGSL ループが無限膨張し GPU ハング / デバイスロス

**該当**: `crates/ravel-nodes/src/blur.rs:66-74`, `shaders/blur.wgsl:37`

`radius.round().max(0.0) as i32` に上限が無く、
`for (var i = -r; i <= r; ...)` がピクセルごと・パスごとに実行される。
半径 50,000（数値フィールドの打ち間違い、アニメーションチャンネルのオーバーシュート）で
ピクセルあたり 100,001 回のテクスチャロード — 1080p なら1パスあたり約 2×10¹¹ ロードで、
多くのドライバで TDR / ハングする。
params → dispatch の経路上に検証が一切無い。

**修正方針**: 半径を妥当な最大値にクランプ（かつ実際に使う σ の 3σ 相当に）。
大きな半径ではダウンサンプルまたは多段近似に切り替える。

---

## MED-GPU-04 | perf | CPU ラスタライズ経路がプリミティブごと・フレームごとに全画面カバレッジバッファを確保、しかもビューアがこれを使う

**該当**: `crates/ravel-nodes/src/rasterize/mod.rs:677`, `:686-694`
（呼び出し元: `crates/ravel-app/src/eval_hooks.rs:140` の `RasterizeProcessor::from_node`、
合成ノードは `lib.rs:112-114`）

`raster_paths` はプリミティブごとに fill 用 `vec![0u8; w*h]` と stroke 用の2枚目を確保し、
`blend_coverage` は形状の bbox に関係なくプリミティブごとに全画面を走査する
— O(プリミティブ数 × 解像度) の確保と走査。

`GpuEvalHooks::finalize` は、自身が `GpuContext` とプールを保持し GPU ラスタライザも存在するのに、
ビューアが表示する Geometry 出力すべてに対して CPU プロセッサ（`from_node` → `gpu: None`）を
構築する。結果、シェイプ / スキャッターノードをプレビューしながらのスクラブは
毎フレーム zeno の CPU ラスタライズを回す。
合成 `rasterize` ノードも CPU 経路に固定されている（`lib.rs` コメント: ゴールデンテストで固定）。

**修正方針**: `finalize` で GPU ラスタライザを使う（コンテキストとプールは既にスコープ内）。
CPU 経路では再利用可能なカバレッジバッファを1枚確保し、
`blend_coverage` をプリミティブの bbox に限定する。

---

## MED-GPU-05 | perf | `ensure_gpu` が同一 CPU フレームを消費 GPU ノードごとに再アップロードする

**該当**: `crates/ravel-nodes/src/gpu_util.rs:59-78`

N 個の GPU ノードに供給される CPU 常駐 `FrameBuffer`（デコード済みメディアフレームなど）は
フレームあたり N 回アップロードされる。
`ensure_gpu` は呼び出しごとにプールテクスチャを取得して `upload_texture` し、
アップロード済みテクスチャはディスパッチ直後に解放され、元バッファと紐付けられない。
4K RGBA32F では冗長アップロード1回あたり約 132MB の PCIe / ユニファイドメモリ帯域。

**修正方針**: CPU→GPU 変換結果を値側にキャッシュする
（最初の GPU 消費側で `GpuFrameBuffer` に変換して評価器キャッシュに格納）。
またはフレーム内でソースバッファの `Arc` ポインタでメモ化する。
