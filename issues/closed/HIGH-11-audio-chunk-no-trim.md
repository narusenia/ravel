# [HIGH-11] `decode_audio_chunk` が seek 着地点から `start_sample` までをトリムしない

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-media / デコーダ |
| 該当 | `crates/ravel-media/src/decoder.rs:547-583` |

> **解決済み**: フェーズ A3。`AudioChunkCollector` が `start_pts` / `start_sample` /
> `sample_count` を持ち、`push` が各フレームの PTS から `frame_start_sample` を
> 求めて要求位置までを捨てる（`crates/ravel-media/src/decoder.rs:557-564`, `:653-662`）。

## 現状

seek（目標以前の最近パケットに着地。[HIGH-10](HIGH-10-audio-chunk-seek-wrong-time-base.md) の
単位バグがあるため実際はファイル先頭付近）の後、パケットループはデコードした全サンプルを
`collected` に集め、`sample_count` に truncate するだけ。
フレーム PTS を `start_sample` と比較していないため、返るバッファは
**要求したサンプル位置ではなく seek が着地した位置**から始まる。

## 影響

非ゼロ `start_sample` に対して返る音声内容が誤り。`MediaReader` の契約違反であり、
チャンクストリーム再生を実装した時点で A/V 同期が破綻する。

既に実害が出ている: `decode_full_audio` の上限プローブ
（`crates/ravel-app/src/audio/mixdown.rs:308`）が
`decode_audio_chunk(stream_index, cap_frames, 1)` を呼ぶ。
seek が手前に着地しトリムもされないため、上限をちょうど満たすストリームでも `extra` が非空になり、
「メモリ内デコード上限を超過」として誤って拒否される。

## 修正方針

seek 後、フレーム終端（`pts + nb_samples`）が `start_sample` より前のフレームをスキップし、
境界を跨ぐフレームの先頭サンプルを落とす。PTS はストリーム時間基準で換算する。

## 検証

- 非ゼロ `start_sample` で返るサンプルが要求位置と一致するテスト
- `decode_full_audio` の上限プローブが上限ちょうどのストリームを受理するテスト

## 関連

- [HIGH-10](HIGH-10-audio-chunk-seek-wrong-time-base.md) — 同時修正が前提
