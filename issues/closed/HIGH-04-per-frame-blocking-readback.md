# [HIGH-04] 表示フレームごとにブロッキング GPU リードバック（毎回ステージングバッファ確保 + デバイス全体待ち + 二重 CPU コピー）

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-gpu / 転送 |
| 該当 | `crates/ravel-gpu/src/transfer.rs:162-221`, `crates/ravel-gpu/src/frame.rs:134-143`, 呼び出し元 `crates/ravel-app/src/eval_hooks.rs:121-130` |

> **解決済み**: `GPUBK-6`（PR #282、2026-08-05）。指摘の 3 点すべてに対応した。
> ①ステージングバッファをバイトサイズをキーにしたプール（`ravel-gpu/src/staging.rs`）
> から借りる。②`read_texture` のデバイス全体待ちを、そのコピーの
> `SubmissionIndex` に絞った待ちに置き換えた。③`to_frame_buffer` が
> `read_texture` → `Vec<f32>` → `Arc<[u8]>` の 3 段だったのを、readback バイトが
> `FrameBuffer` の `Arc<[u8]>` に直接着地する形にした。
>
> 検証欄の受入条件 2 つを満たしている（Apple M5 / macOS 26.3 / release、
> 各 20 フレーム）: **1080p 6.13 ms → 2.36–2.44 ms（−61%）、
> 4K 26.89 ms → 6.23–7.58 ms（−72〜−77%）**。ステージング確保数は
> 20 フレームで **0**。最悪ケースの改善が平均より大きく、変更前 4K の
> max 67.11 ms が 7.22–9.79 ms に収まった。測定は
> `docs/implementation/perf-baseline.md`、計画は
> `docs/implementation/gpu-backend-plan.md` の `GPUBK-6` 節。
>
> 「対応案 4（readback せずテクスチャを直接表示）」は未実施で、
> `GPUCOMP-11` / `GPUBK-9` の範囲。残件として
> **ステージングプールのアイドル上限（256 MiB）が共有 `CacheBudget` の外にある**
> — 起票済み。

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

- [HIGH-05](HIGH-05-shell-chain-cpu-per-pixel.md) — レイヤーごとに本問題を踏む
- [HIGH-09](HIGH-09-viewer-gpu-cpu-gpu-roundtrip.md) — 表示側の往復
