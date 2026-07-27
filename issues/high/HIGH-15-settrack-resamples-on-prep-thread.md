# [HIGH-15] SetTrack が prep スレッドでトラック全長の 256-tap sinc リサンプルを実行しコールバックを飢餓させる

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-audio / エンジン・リサンプラ |
| 該当 | `crates/ravel-audio/src/engine.rs:319-335`, `crates/ravel-audio/src/resampler.rs:163-194` |

## 現状

`handle_command(SetTrack)` はトラック全体（最大 `MAX_DECODE_BYTES` = 128MiB 相当のサンプル）に対し
`sinc_len: 256, oversampling_factor: 256` の `resampler::resample_buffer` を
prep スレッド上で同期実行する。

その間チャンクはミックスされず、8チャンク（約 170ms）のキューが枯渇して出力がアンダーランする
（[HIGH-14](HIGH-14-clock-advances-over-underrun.md) により、これが恒久ドリフトへ変換される）。

`AudioService::sync_from_document` はドキュメント編集時に `SetTrack` を再送するため、
メディアレートが 48kHz と異なる音声レイヤー（44.1kHz ファイルすべて）を追加・編集するたびに
再生が可聴レベルで途切れる。

## 影響

44.1kHz 素材を扱う限り、音声レイヤーの編集操作すべてで再生が破綻する。

## 修正方針

エンジンに届く前に ravel-app のバックグラウンドエグゼキュータでリサンプルを済ませる
（エンジンは出力レートのサンプルのみ受け付ける契約にする）。
または SetTrack のリサンプルを別ワーカースレッドに渡し、完了後に準備済みトラックを差し替える。

## 検証

- 44.1kHz レイヤーの編集中にアンダーランが発生しないことを計測
- prep スレッドのブロック時間を計測

## 関連

- [HIGH-14](HIGH-14-clock-advances-over-underrun.md)
- [medium/media-audio.md](../medium/media-audio.md) — `resample_buffer` のフィルタ遅延・末尾欠落バグ
- [medium/app-shell.md](../medium/app-shell.md) — `AudioService::sync` の UI スレッドブロック
