# [HIGH-05] シェル合成チェーンが CPU per-pixel — レイヤーごと・フレームごとにリードバックを強制する

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-nodes / 合成（comp.*） |
| 該当 | `crates/ravel-nodes/src/comp/transform.rs:70-85`, `comp/opacity.rs:51-60`, `comp/merge.rs:113-131`, `:187-207`, `crates/ravel-nodes/src/gpu_util.rs:82-90` |

## 現状

`comp.transform` / `comp.opacity` / `comp.merge.*` の3プロセッサはすべて `ensure_cpu` を呼ぶ。
入力が GPU 常駐なら `to_frame_buffer()`（= [HIGH-04](HIGH-04-per-frame-blocking-readback.md) の
ブロッキングリードバック）が**レイヤーごと・フレームごと**に走る。

その後それぞれ単一スレッドのスカラー `for y / for x` ループを `ctx.resolution` 全域に実行する
（transform: ピクセルごとにバイリニアサンプル4関数呼び、merge: 全フレーム Porter-Duff ループ）。
各ノードが `vec![0.0f32; w*h*4]`（4K で約 33MB）を新規確保する。

同じ処理を行う GPU シェーダ（`transform.wgsl`, `merge.wgsl`）は既に存在するが、
シェルチェーンからは使われていない。

## 影響

レイヤーネットワークが GPU ノードで終わる N レイヤーのコンポジションで、
毎フレーム N 回のリードバック + 約 3N 回の全フレーム CPU ループ。
さらに CPU 結果を下流に渡すため、以降は何も GPU 常駐で残らない。
複数レイヤー構成では描画もっさりの支配要因。

## 修正方針

シェルプロセッサの GPU 版を実装する。transform / merge の WGSL は9割再利用可能、
opacity は自明な1行シェーダ。CPU 実装はアダプタ無し環境のフォールバックとして残す。
これによりレイヤーネットワーク → 合成 → ビューアの単一リードバックまで GPU 常駐を維持できる。

## 検証

- 10レイヤー構成でフレームあたりのリードバック回数が 1 になることを確認
- CPU / GPU 実装の出力一致テスト（既存のゴールデンテストを流用）

## 関連

- [HIGH-04](HIGH-04-per-frame-blocking-readback.md), [HIGH-09](HIGH-09-viewer-gpu-cpu-gpu-roundtrip.md)
- [medium/gpu-nodes.md](../medium/gpu-nodes.md) — 直線アルファのフィルタリングバグ（GPU 版実装時に併せて対処）
