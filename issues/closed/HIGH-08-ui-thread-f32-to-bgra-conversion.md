# [HIGH-08] UI スレッドで全フレーム f32→BGRA 変換

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-app / Viewer |
| 該当 | `crates/ravel-app/src/panels/viewer.rs:1826-1856`（`frame_buffer_to_render_image`）、呼び出し元 `:275-289` |

> **解決済み**: `GPUCOMP-9`（PR #284、2026-08-05）。変換が UI スレッドから
> **評価ワーカー**へ移った。`ViewerFrame::Frame` は `Arc<FrameBuffer>`（f32）でなく
> BGRA の `RenderImage` を運ぶ形（`ViewerImage`）になり、UI スレッドは完成済み
> 画像を `Arc` で受け取るだけになった。バイトごとの `Vec::push` も、厳密サイズ
> 1 回確保 + 添字書き込みに置き換わっている。
>
> UI スレッド占有の実測（Apple M5 / macOS 26.3 / release、40 フレーム平均）:
> **1024×576（対話評価の上限）1.21 ms → 0、1920×1080 4.33 ms → 0**。
> 60 fps 予算 16.7 ms に対し 1080p で 26% が空いた。変換自体もワーカー上で
> 2.5 倍速くなっている。測定は `docs/implementation/perf-baseline.md`。
>
> ピクセル出力は不変。旧変換を `reference_bgra` として逐語で保存し、
> `produces_the_same_bytes_as_the_previous_conversion` が丸め境界 64 段 +
> 非有限 + 半 LSB 境界でバイト完全一致を固定する。ワーカーで走ることは
> `the_display_conversion_runs_on_the_evaluation_worker` がスレッド名で固定する。
>
> 「変換をアトラスに載せる前に済ませる」以外の残件
> （GPU→CPU→GPU の往復そのもの）は [HIGH-09](HIGH-09-viewer-gpu-cpu-gpu-roundtrip.md)
> に残り、`GPUBK-9` / `GPUCOMP-11` の範囲。

## 現状

`ViewerFrame` observer から、メインスレッド上で以下を実行する。

- フレーム全域のピクセルループ（1024×576 = 約 59万px、clamp + 乗算を伴う約 240万回の
  `Vec::push` バイト書き込み）
- `ImageBuffer` の新規確保（約 2.3MB）
- 新規 `RenderImage` 作成

再生・スクラブ中はこれがフレームごとに走る。

## 影響

UI スレッドのレイテンシに直接ミリ秒単位で加算される。
[CRIT-01](../closed/CRIT-01-eval-update-notifies-whole-workspace.md) の再レンダー嵐と重なり体感を悪化させる。

## 修正方針

1. 色変換を評価ワーカー（`GpuEvalHooks::finalize` が既にリードバックを所有）または
   バックグラウンドタスクへ移し、UI スレッドは完成済み BGRA バイト列を包むだけにする
2. バイトごとの push をやめ、事前確保・再利用バッファへ書き込む

## 検証

- 再生中のメインスレッド占有時間を計測
- 変換がワーカースレッドで実行されていることをスレッド ID で確認

## 関連

- [HIGH-09](HIGH-09-viewer-gpu-cpu-gpu-roundtrip.md), [HIGH-04](../closed/HIGH-04-per-frame-blocking-readback.md)
