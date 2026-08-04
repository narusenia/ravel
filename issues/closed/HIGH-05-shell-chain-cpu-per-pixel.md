# [HIGH-05] シェル合成チェーンが CPU per-pixel — レイヤーごと・フレームごとにリードバックを強制する

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-nodes / 合成（comp.*） |
| 該当 | `crates/ravel-nodes/src/comp/transform.rs:70-85`, `comp/opacity.rs:51-60`, `comp/merge.rs:113-131`, `:187-207`, `crates/ravel-nodes/src/gpu_util.rs:82-90` |

> **解決済み**: シェル3プロセッサすべてに GPU 版が入り、`processor_for_node` の
> 既定経路になった（`comp.opacity` / `comp.transform` は GPUCOMP-2 / 3、
> `comp.merge.*` と `comp.merge.adjustment` は GPUCOMP-5 / 6 = PR #199、2026-07-29）。
> 10 レイヤー / 30 fps 再生形でシェルチェーン由来のリードバックは
> **10 回 / 完成評価 → 0 回**、`evaluate` 合計は −94%
> （`docs/implementation/perf-baseline.md`「GPU シェル merge 投入後」）。
> CPU 実装は `pub` のまま残り、ゴールデンテストが明示登録するリファレンス経路として
> 単位ごとの一致テストの基準になっている。
> チェーン全体を pin する回帰テストは GPUCOMP-7 で入れる。

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

シェルプロセッサの GPU 版を実装する。opacity は自明な1行シェーダ。
transform / merge は既存 WGSL を**流用できるが drop-in ではない** —
アルファ規約（直線 vs 乗算済み補間）、merge のモード数（3 vs 6）と合成式の形、
transform の入出力サイズの前提が違う。
CPU 実装は残すが、目的は**リファレンス経路**（`shape_layer_golden` が
ピクセルを pin する土台）であって「アダプタ無し環境のフォールバック」ではない —
アプリは `project_state.rs:196` でアダプタが無ければ起動時に panic する。
これによりレイヤーネットワーク → 合成 → ビューアの単一リードバックまで GPU 常駐を維持できる。

設計と実装単位: `docs/implementation/gpu-compositing-plan.md`

## 検証

- 10レイヤー構成でフレームあたりのリードバック回数が 1 になることを確認
- CPU / GPU 実装の出力一致テスト（既存のゴールデンテストを流用）

## 関連

- [HIGH-04](HIGH-04-per-frame-blocking-readback.md), [HIGH-09](../high/HIGH-09-viewer-gpu-cpu-gpu-roundtrip.md)
- [medium/gpu-nodes.md](../medium/gpu-nodes.md) — 直線アルファのフィルタリングバグ（GPU 版実装時に併せて対処）
