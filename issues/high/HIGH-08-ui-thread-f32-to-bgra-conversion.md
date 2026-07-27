# [HIGH-08] UI スレッドで全フレーム f32→BGRA 変換

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-app / Viewer |
| 該当 | `crates/ravel-app/src/panels/viewer.rs:1826-1856`（`frame_buffer_to_render_image`）、呼び出し元 `:275-289` |

## 現状

`ViewerFrame` observer から、メインスレッド上で以下を実行する。

- フレーム全域のピクセルループ（1024×576 = 約 59万px、clamp + 乗算を伴う約 240万回の
  `Vec::push` バイト書き込み）
- `ImageBuffer` の新規確保（約 2.3MB）
- 新規 `RenderImage` 作成

再生・スクラブ中はこれがフレームごとに走る。

## 影響

UI スレッドのレイテンシに直接ミリ秒単位で加算される。
[CRIT-01](../critical/CRIT-01-eval-update-notifies-whole-workspace.md) の再レンダー嵐と重なり体感を悪化させる。

## 修正方針

1. 色変換を評価ワーカー（`GpuEvalHooks::finalize` が既にリードバックを所有）または
   バックグラウンドタスクへ移し、UI スレッドは完成済み BGRA バイト列を包むだけにする
2. バイトごとの push をやめ、事前確保・再利用バッファへ書き込む

## 検証

- 再生中のメインスレッド占有時間を計測
- 変換がワーカースレッドで実行されていることをスレッド ID で確認

## 関連

- [HIGH-09](HIGH-09-viewer-gpu-cpu-gpu-roundtrip.md), [HIGH-04](HIGH-04-per-frame-blocking-readback.md)
