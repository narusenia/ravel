# [HIGH-09] ビューア画像経路が毎フレーム GPU→CPU→GPU の往復 + 冗長コピー + アトラス churn

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-gpu / frame, ravel-app / Viewer |
| 該当 | `crates/ravel-gpu/src/frame.rs:134-143`, `crates/ravel-app/src/eval_hooks.rs:121-130`, `crates/ravel-ui/src/panels/viewer.rs`（`ViewerResolution`）, `crates/ravel-app/src/panels/viewer.rs:283-288`, `:1599-1604` |

## 現状

GPU 評価されたフレームは毎回

1. `read_texture` で CPU f32 にリードバック
2. `bytemuck::cast_slice(&raw).to_vec()` で**2度目のコピー**
   （1024×576 RGBA-f32 で約 9.4MB）
3. UI 側で新規 `RenderImage` として GPUI スプライトアトラスへ再アップロード、
   前フレームの画像を `drop_image`（アトラス churn）

## 影響

ビューアのスループット上限を決めている。`VRES-1` 以前は対話評価が
`VIEWER_MAX_DIM = 1024`（長辺の絶対上限）に制限されており、その理由が
この経路のコストだった。`VRES-1` で上限は撤去され、ユーザーが選ぶ
`ViewerResolution` 係数（既定 `Half`）に置き換わっている。**この経路の
コストは変わっていない** — 変わったのは、ユーザーが `Full` を選んで
コンポ解像度で評価できるようになり、そのとき本 issue のコストを
まともに踏むこと。`GPUBK-9` の計測（`perf-baseline.md`）では 1080p 全体
約 15.8 ms のうち転送 2.04 ms + BGRA 変換 1.63 ms が本 issue の範囲で、
支配項ではない（約 23%）。

## 修正方針

- 短期: `to_frame_buffer` の2度目のコピーを除去し、変換バッファを永続化・再利用
- 本質（実装計画の Phase 4）: フレームを GPU 常駐のまま共有テクスチャ / カスタム GPUI 要素で表示し、
  フレームごとのリードバックとアトラス再アップロードを廃止

## 検証

- フレームあたりのバイトコピー量を計測（現状の半減が短期目標）
- `ViewerResolution::Full` でも再生レートが維持できることを確認

## 関連

- [HIGH-04](../closed/HIGH-04-per-frame-blocking-readback.md), [HIGH-05](../closed/HIGH-05-shell-chain-cpu-per-pixel.md), [HIGH-08](../closed/HIGH-08-ui-thread-f32-to-bgra-conversion.md)
