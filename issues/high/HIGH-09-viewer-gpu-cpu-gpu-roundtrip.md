# [HIGH-09] ビューア画像経路が毎フレーム GPU→CPU→GPU の往復 + 冗長コピー + アトラス churn

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-gpu / frame, ravel-app / Viewer |
| 該当 | `crates/ravel-gpu/src/frame.rs:134-143`, `crates/ravel-app/src/eval_hooks.rs:121-130`, `crates/ravel-app/src/project_state.rs:42-47`, `crates/ravel-app/src/panels/viewer.rs:283-288`, `:1599-1604` |

## 現状

GPU 評価されたフレームは毎回

1. `read_texture` で CPU f32 にリードバック
2. `bytemuck::cast_slice(&raw).to_vec()` で**2度目のコピー**
   （1024×576 RGBA-f32 で約 9.4MB）
3. UI 側で新規 `RenderImage` として GPUI スプライトアトラスへ再アップロード、
   前フレームの画像を `drop_image`（アトラス churn）

`project_state.rs:42-47` のコメントが、シェル合成チェーンが GPU ノードごとに
追加のリードバックを行うことを認めている。

## 影響

ビューアのスループット上限を決めている。対話評価が `VIEWER_MAX_DIM = 1024` に
制限されているのはこの経路のコストが理由。

## 修正方針

- 短期: `to_frame_buffer` の2度目のコピーを除去し、変換バッファを永続化・再利用
- 本質（実装計画の Phase 4）: フレームを GPU 常駐のまま共有テクスチャ / カスタム GPUI 要素で表示し、
  フレームごとのリードバックとアトラス再アップロードを廃止

## 検証

- フレームあたりのバイトコピー量を計測（現状の半減が短期目標）
- `VIEWER_MAX_DIM` を上げても再生レートが維持できることを確認

## 関連

- [HIGH-04](../closed/HIGH-04-per-frame-blocking-readback.md), [HIGH-05](../closed/HIGH-05-shell-chain-cpu-per-pixel.md), [HIGH-08](HIGH-08-ui-thread-f32-to-bgra-conversion.md)
