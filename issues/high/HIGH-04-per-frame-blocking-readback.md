# [HIGH-04] 表示フレームごとにブロッキング GPU リードバック（毎回ステージングバッファ確保 + デバイス全体待ち + 二重 CPU コピー）

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-gpu / 転送 |
| 該当 | `crates/ravel-gpu/src/transfer.rs:162-221`, `crates/ravel-gpu/src/frame.rs:134-143`, 呼び出し元 `crates/ravel-app/src/eval_hooks.rs:121-130` |

## 現状

`read_texture` は呼び出しごとに

1. `MAP_READ` ステージングバッファを新規作成（`transfer.rs:162`）
2. コピーを submit
3. `ctx.wait()` = `device.poll(PollType::wait_indefinitely())` — このコピーだけでなく
   **それ以前に submit した全デバイス作業**の完了までブロック（`transfer.rs:205`, `device.rs:167-171`）
4. 行単位で `Vec<u8>` に再パック

さらに `GpuFrameBuffer::to_frame_buffer` が `bytemuck::cast_slice(&raw).to_vec()` で
**もう一度**全バッファをコピーする（`frame.rs:137`）。

`GpuEvalHooks::finalize` が評価フレームごとにこれを実行する。

## 影響

4K RGBA32F で約 132MB のリードバック + CPU コピー2回 + パイプライン全同期を毎フレーム。
これ単体で再生レートの上限を決めている。対話評価が `VIEWER_MAX_DIM = 1024` に制限されている理由。

## 修正方針

1. 再利用可能なステージングバッファのリングをプールに持つ
2. デバイス全体を wait せず map をパイプライン化（フレーム N のリードバック中に N+1 を評価）
3. `Vec<f32>` へ直接読み、2回目のコピーを消す
4. 本質的には readback せずテクスチャを直接表示（コード内で既に「Phase 4」として言及済み）

## 検証

- 1080p / 4K でのフレームあたりリードバック時間を計測
- ステージングバッファのアロケーション回数がフレーム数に比例しないことを確認

## 関連

- [HIGH-05](../closed/HIGH-05-shell-chain-cpu-per-pixel.md) — レイヤーごとに本問題を踏む
- [HIGH-09](HIGH-09-viewer-gpu-cpu-gpu-roundtrip.md) — 表示側の往復
