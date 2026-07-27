# [HIGH-14] アンダーラン中の無音でも同期クロックが進み、A/V ドリフトが累積する

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-audio / デバイス |
| 該当 | `crates/ravel-audio/src/device.rs:114-133` |

## 現状

アンダーラン時、コールバックは残りをゼロ埋めした後、
`data.len() / channels` の**全フレーム分**（無音を含む）クロックを進める。
ミキサーの `read_position` はそのフレーム分進んでいないため、
以降の内容はアンダーラン累積長だけ遅れて再生される。
映像レンダラが追従するクロックが音声内容より先行し、アンダーランごとにドリフトが増える。

## 影響

[HIGH-15](HIGH-15-settrack-resamples-on-prep-thread.md) により prep スレッドがブロックするため
アンダーランは稀ではない。通常の編集作業中にドリフトが累積する。

## 修正方針

クロックはゼロ埋めではなく、実際にチャンクからコピーしたフレーム数だけ進める
（チャンク由来の `written` を別カウントする）。

## 検証

- 人為的にアンダーランを起こしてクロックと内容位置の乖離が生じないテスト

## 関連

- [HIGH-12](HIGH-12-pause-does-not-stop-queued-audio.md), [HIGH-13](HIGH-13-seek-does-not-flush-audio-queue.md), [HIGH-15](HIGH-15-settrack-resamples-on-prep-thread.md)
