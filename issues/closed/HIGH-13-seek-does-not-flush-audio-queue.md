# [HIGH-13] Seek でチャンクキューを flush しない — 古い音声が鳴り、恒久オフセットが生まれる

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-audio / エンジン |
| 該当 | `crates/ravel-audio/src/engine.rs:312-318` |

> **解決済み**: フェーズ A3（epoch 導入）。Seek は epoch を進め、キューに残る旧 epoch の
> チャンクはコールバック側で捨てられる（`crates/ravel-audio/src/device.rs:47-66`）。

## 現状

`AudioCommand::Seek` は `read_position` とクロックをリセットするが、
bounded チャネルには seek 前の位置でミックスされたチャンクが最大8個残っている。
コールバックはそれらを先に再生するため、

1. 旧位置の音声が可聴で鳴る
2. その間クロックは新位置から進む
3. seek 後にミックスされたチャンクは、クロックが示す時刻より
   `queue_depth × chunk_frames`（約 170ms）遅れて再生される

## 影響

スクラブ・シークごとに誤った音声のバーストが鳴り、その後「音声が映像より約 170ms 遅れる」
恒久オフセットが残る。

## 修正方針

各チャンクに generation / epoch カウンタを付与し（例: `(epoch, Arc<[f32]>)` を送る）、
コールバックは最後の seek より古い epoch のチャンクを破棄する。
または prep スレッドが seek 時にミックス再開前に `chunk_rx` を drain する。

## 検証

- Seek 直後に旧位置の音声が再生されないテスト
- Seek 後のクロックと音声内容位置の一致テスト

## 関連

- [HIGH-12](HIGH-12-pause-does-not-stop-queued-audio.md), [HIGH-14](HIGH-14-clock-advances-over-underrun.md)
