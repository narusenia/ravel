# closed / medium — ravel-gpu / ravel-nodes（GPU パイプライン・ノード処理）

解決済みの medium 項目。個票は起票時のまま残し、各項目の **解決済み** 行が結果を記録している。

未解決分は [`../medium/gpu-nodes.md`](../medium/gpu-nodes.md)。

---

## MED-GPU-01 | perf | ノードディスパッチごとに queue submit（ブラーは2回）、ディスパッチごとにユニフォームバッファとバインドグループを新規作成。`GpuTask` バッチング trait は実装ゼロ

> **解決済み**: PR #274（2026-08-03）。`ravel-gpu` の `dispatch` モジュールに
> 宣言的ディスパッチ API（`ComputeDispatch` / `GpuContext::dispatch_compute`）を置き、
> ユニフォームバッファを内容キーで、バインドグループを（パイプライン, テクスチャ,
> ユニフォーム）の同一性で再利用。ディスパッチはフレーム共有エンコーダに記録し、
> リードバック／アップロード／`wait`／明示 `flush`／バッチ上限でのみ submit する
> （アプリではビューアのリードバックがフレームごとの flush 点 = 1 submit / フレーム）。
> 未使用だった `GpuTask` trait は除去。10 レイヤー再生形で submits 29/評価 → 0.48/評価、
> `evaluate` 合計 −16%。設計は `docs/implementation/gpu-backend-plan.md`（GPUBK-2）、
> 計測は `docs/implementation/perf-baseline.md`。

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

**関連**: [HIGH-05](HIGH-05-shell-chain-cpu-per-pixel.md) の GPU 版シェル実装時に
併せて対処するのが効率的。

---

## MED-GPU-03 | bug | ブラー半径が未クランプ — 大きな値で per-pixel WGSL ループが無限膨張し GPU ハング / デバイスロス

**該当**: `crates/ravel-nodes/src/blur.rs:66-74`, `shaders/blur.wgsl:37`

> **解決済み**: フェーズ A2。半径は
> `radius.clamp(0.0, ravel_core::registry::builtin::MAX_BLUR_RADIUS)` を通る
> （`crates/ravel-nodes/src/blur.rs:18`）。

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

> **解決済み**: `RESP3-12` / `RESP3-13`（PR #396）。`GpuEvalHooks::finalize` が
> ビューアの Geometry 出力を GPU でラスタライズする（39.58 → 0.111 ms）。
> 残る CPU 経路はカバレッジマスク 1 枚をジオメトリ全体で再利用し、
> `blend_coverage` をプリミティブの bbox に限定した（39.58 → 2.28 ms）。
> `shape_layer_golden` は保存画素の pin から **CPU / GPU 一致テスト**へ
> 置き換えた（`RESP3-12`）。
>
> **票の「合成 `rasterize` ノードも CPU 経路に固定されている」は事実と違った。**
> `processor_for_node` の synthetic 分岐は実質デッドで、シェルコンパイラは
> `rasterize` の synthetic ノードを生成していない。ゴールデンが押さえていたのは
> `finalize` 側の経路だけで、この分岐がゲートだったわけではない。

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

> **解決済み**: `RESP3-14`（PR #396）。`TexturePool` がアップロードスコープを
> 持ち、同一 CPU `FrameBuffer` を**評価 1 回につき 1 度だけ**アップロードする
> （キーは画素アロケーションのアドレス + 幅・高さ・フォーマット）。
> 4K RGBA32F の 3 消費ノードで **3 → 1 回、398 → 133 MB / 評価**。
> スコープの境界は `finalize` — 対話ワーカーと書き出しワーカーの両方が
> 評価 1 回につき 1 回呼ぶ唯一のフックなので、メモがフレームを跨がない。
> 修正方針の前者（`GpuFrameBuffer` を評価器の値型にする）は採らず、
> フレーム内メモ化で足りることを測ってから閉じた。

N 個の GPU ノードに供給される CPU 常駐 `FrameBuffer`（デコード済みメディアフレームなど）は
フレームあたり N 回アップロードされる。
`ensure_gpu` は呼び出しごとにプールテクスチャを取得して `upload_texture` し、
アップロード済みテクスチャはディスパッチ直後に解放され、元バッファと紐付けられない。
4K RGBA32F では冗長アップロード1回あたり約 132MB の PCIe / ユニファイドメモリ帯域。

**修正方針**: CPU→GPU 変換結果を値側にキャッシュする
（最初の GPU 消費側で `GpuFrameBuffer` に変換して評価器キャッシュに格納）。
またはフレーム内でソースバッファの `Arc` ポインタでメモ化する。

---

## MED-GPU-07 | debt | wgpu が 2 本入っていて、デバイス共有が構造的に成立しない

> **解決済み**: 2026-08-05。`Cargo.toml` の `wgpu` / `naga` を crates.io の
> **29.0.4** に戻した。`Cargo.lock` の `wgpu` / `naga` / `wgpu-core` /
> `wgpu-hal` はいずれも 1 エントリになり、`ravel-gpu` と `gpui_wgpu` が
> **同じ wgpu を参照する**（lock の依存行がバージョン修飾子なしで並ぶ＝
> グラフに 1 本だけ）。これで `REQ-GPU-001` の「UI 描画とコンピュートが
> デバイスを共有する」を配線できる前提が整い、`GPUBK-9` と `GPUBK-14` が開いた。
>
> **フォークから失うものは無く、むしろ増えた。** 起票時に「フォークの唯一の
> パッチは Linux GLES 用の 23 行で Ravel は使わない」と書いたが、その
> XCB パッチは upstream の #9271 として **29.0.4 に取り込まれている**
> （`wgpu-hal/src/gles/egl.rs +23/-1`）。フォークで得ていたものまで
> crates.io 側に来ていた。
>
> **副産物: Metal のコマンドキューが取れるようになった。** 29.0.4 は
> `fix(metal): Restore the Queue::as_raw method`（#9560 / #9789）を含み、
> 上流 CHANGELOG が「v29 で *removed without good reason*」と書いている
> とおり、これは設計判断ではなく回帰だった。`GPUBK-8` の実装メモが
> 「`id<MTLCommandQueue>` は取れない」「OFX ホストは別タイムラインの
> キューを作って同期する」と申し送っていた前提が外れたので、
> `gpu-backend-plan.md` / `roadmap.md` / `interop.rs` の該当記述を訂正した。
> `interop` への取得口は消費者（OFX ホスト）と一緒に足す。
>
> D3D12 は 29.0.4 で無変更で、`raw_device` / `raw_queue` / `Queue::as_raw` /
> `raw_resource` は元から揃っていた。穴は Metal だけだった。
>
> **検証**: `mise run check` 全通過（ravel-gpu / ravel-nodes の GPU テストは
> macOS 実機 Metal で skip 無し）、`mise run docs:check` clean、
> `cargo clippy -p ravel-gpu --all-targets --target x86_64-pc-windows-msvc
> -- -D warnings` clean。GPU ノードのゴールデンは保存画像ではなく
> CPU 参照との一致比較および被覆率のしきい値比較で、いずれも無改変で通過。

**該当**: `Cargo.toml:40,44`（`wgpu` / `naga` を `zed-industries/wgpu` の
git rev に固定）、`Cargo.lock`（`wgpu` 2 エントリ / `naga` 2 エントリ /
`wgpu-core` `wgpu-hal` 各 2 エントリ）

依存グラフが 2 系統に割れている:

```text
ravel-gpu  -> wgpu 29.0.3  (git: zed-industries/wgpu@357a0c5) + naga 29.0.3
gpui_wgpu  -> wgpu 29.0.4  (crates.io)                        + naga 29.0.4
```

git ソースと registry ソースは Cargo にとって別クレートなので、**同じ
バージョンでも型が別**。`.agents/rules/rust.md` はこれを名指しで禁じている:

> Reuse the workspace-pinned `wgpu` revision. **Do not introduce a second
> incompatible wgpu version into application-facing GPU paths.**

帰結が 3 つ:

1. **`REQ-GPU-001` の受入条件「UI 描画とコンピュートがデバイスを共有する」が
   構造的に達成できない。** GPUI の `wgpu::Device` は 29.0.4 の型、Ravel の
   それは 29.0.3-git の型で、`GpuContext::from_handles` に渡せない。
   実際 `from_handles()` と `GpuContext::instance()` は**呼び出し元が 0**
   （`GPUBK-4` の調査で判明）。デバイス共有の API は用意されているが
   配線できる状態になっていない
2. **`GPUBK-9`（デバイス共有の契約と GPUI フォーク方針）の前提が無い。**
   契約を書く前に 1 本にする必要がある
3. wgpu + naga + wgpu-core + wgpu-hal をビルドが 2 組コンパイルする。
   CI キャッシュ（`#211` で触れた 10 GB 上限）とローカルの両方に効く

**そもそもこの git 固定から得ているものが無い。** upstream との差分は
1 ファイル 23 行:

```text
gfx-rs/wgpu v29 ... zed-industries/wgpu@357a0c5
  ahead 2, behind 4, files 1
  wgpu-hal/src/gles/egl.rs (+23/-1)  "Add XCB display handle support to EGL backend"
```

Linux の GLES/EGL 限定のパッチで、Ravel は GLES を使わない
（`gpu-backend-plan.md` の「非対象」に OpenGL が名指しされている）。
しかも upstream v29 より 4 コミット遅れている。`gpui-ce` 側も
`wgpu = "29.0.3"` を crates.io から取っており、このフォークを見ていない。

**修正方針**: `Cargo.toml` の `wgpu` / `naga` を crates.io の 29.0.4 に戻す。
Ravel が要る naga の feature（`wgsl-in` / `msl-out` / `hlsl-out` / `spv-out`）は
crates.io の 29.0.4 に**すべて存在する**ことを確認済み（`spv-out` と
`wgsl-in` は index の `features2` 側）。

**検証**: `Cargo.lock` の `wgpu` / `naga` / `wgpu-core` / `wgpu-hal` が
各 1 エントリになること。`mise run check` が通ること。GPU ノードの
ゴールデンが一致すること（wgpu の実装が変わるので**必ず確認する**）。
Windows / Linux の CI が通ること（EGL パッチを捨てるので Linux は要注意 —
ただし Linux は現状 CI 対象外）。
一本化できたら **`from_handles` 経由のデバイス共有が実際に配線できるか**を
確認し、`GPUBK-9` へ引き渡す。

**注意**: 将来 wgpu にパッチが要ると判断した場合（例: `wgpu-hal` の
`metal::QueueShared::raw` が private で OFX に `id<MTLCommandQueue>` を
渡せない件 — `gpu-backend-plan.md` の `GPUBK-8` 節）、フォークに戻す前に
**gpui-ce 側も同じ rev に揃える**こと。片側だけ動かすとこの状態に戻る。

## MED-GPU-10 | bug | `geometry.transform` がベジェ接線を変換しないので、回転・スケールで曲線の形が壊れる

**該当**: `crates/ravel-nodes/src/geometry.rs`（`GeometryTransformProcessor`）

`geometry.transform` が書き換えるのは Point ドメインの `P`、Detail の
`anchor`、Instance の `P` / `rot` / `scale` だけで、**`in_tan` / `out_tan` を
触らない**。パスの制御点は「点からのオフセット」なので、点だけ回して接線を
置き去りにすると**曲線の形が変わる** — 90 度回した円が卵になり、
拡大した曲線は制御点だけ元の長さのまま残る。

**これはテキスト以前からあるバグ**である。`shape.custom_path`
（`done/viewer-tool-extensions-plan.md` の `TOOLX-3`、ペンツールの出力）が
接線を持つので、**ペンで描いた曲線を `geometry.transform` に通すと壊れる**。
`text.layout`（`TYPE-2`）の輪郭も接線を持つようになったので、踏みやすくなった。

**直し方は既にリポジトリの中にある**: `TYPE-5`（PR #511）が
`InstanceTransform::apply_vector` を入れた — 「差分は回転とスケールだけ、
平行移動は掛けない」という変換で、接線に必要なのはまさにこれである。
`geometry.transform` の `apply` の隣に同じ線形部分を持たせ、
`in_tan` / `out_tan` の列があれば通す。

**severity の根拠**: bug。データは壊れないが**出る絵が間違う**。high でないのは
接線を持つジオメトリを `geometry.transform` に通したときだけで、
既定のシェイプ（rect / ellipse / polygon / star）は接線を持たないため。
low でないのは、ペンツールで描いた曲線という**ユーザーが直接作ったもの**が
黙って変形するため。

**関連**: `MED-GPU-09`（`Placement::compose` のアフィン近似）は
`TYPE-5` の実装で**独立に再発見された** — 同じ穴に別の入口から 2 回当たって
いるので、`InstanceTransform` に寄せた今が直しやすい。

**解決済み**: PR #514 で `geometry.transform` が `in_tan` / `out_tan` に変換の
線形部分を掛けるようにした。`TYPE-5`（#511）が入れた
`InstanceTransform::apply_vector`（差分は回転とスケールだけ、平行移動は掛けない）を
再利用したので、回転行列がこのノードに 2 本目として生えていない。
`a_transform_turns_and_stretches_the_bezier_tangents` が四分回転・非一様スケール・
平行移動の 3 つを解析値で固定し、「接線の変換を落とす」と「`apply_vector` の
代わりに `apply` を使う」の変異でどちらも落ちることを確認済み。

