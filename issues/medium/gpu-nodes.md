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
