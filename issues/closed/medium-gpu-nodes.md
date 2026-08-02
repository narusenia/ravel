# closed / medium — ravel-gpu / ravel-nodes（GPU パイプライン・ノード処理）

解決済みの medium 項目。個票は起票時のまま残し、各項目の **解決済み** 行が結果を記録している。

未解決分は [`../medium/gpu-nodes.md`](../medium/gpu-nodes.md)。

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
