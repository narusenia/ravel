# [HIGH-12] Pause でキュー済み音声が止まらず、恒久的な A/V ずれが残る

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-audio / デバイス・エンジン |
| 該当 | `crates/ravel-audio/src/device.rs:93-133`, `crates/ravel-audio/src/engine.rs:308-311` |

> **解決済み**: フェーズ A3（epoch 導入）。`AudioChunk` は生成時の transport epoch を
> 持ち、コールバックは `active_epoch` より古いチャンクを破棄する
> （`crates/ravel-audio/src/device.rs:24`, `:61`）。

## 現状

CPAL コールバックは `chunk_rx` を無条件に drain してサンプルをコピーし、
`sync_clock.is_playing()` でゲートしているのはクロック進行のみ。

Pause で prep スレッドは生成を止めるが、既にキューされた最大
`queue_depth × chunk_frames` = 8×1024 フレーム（48kHz で約 170ms）は鳴り続ける。
それらのフレームはミキサーの内容位置（`read_position` は mix 時点で既に進行済み）を進めるが
クロックは進めないため、再開後は音声内容がクロック/映像より最大 170ms **恒久的に**先行する。

## 影響

一時停止するたびに A/V ずれが累積する。編集作業の通常操作で発生する。

## 修正方針

チャンク消費を `is_playing` でゲートする（一時停止中はチャンクを消費せず無音出力）。
または prep スレッドが Pause 時にチャネルを drain / flush し、
未再生キュー分だけ `read_position` を巻き戻す。

## 検証

- Pause → Resume 後にクロックと音声内容位置が一致するテスト
- 一時停止直後に音声が即座に止まることを実機確認

## 関連

- [HIGH-13](HIGH-13-seek-does-not-flush-audio-queue.md), [HIGH-14](HIGH-14-clock-advances-over-underrun.md) — 同一の同期系。まとめて設計し直すのが早い
