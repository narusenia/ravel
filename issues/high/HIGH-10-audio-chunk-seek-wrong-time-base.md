# [HIGH-10] `decode_audio_chunk` の seek がストリーム時間基準の tick を渡す（マイクロ秒が必要）

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-media / デコーダ |
| 該当 | `crates/ravel-media/src/decoder.rs:532-541` |

## 現状

`decode_audio_chunk` は `target_ts` を音声ストリーム自身の時間基準で計算し、
`self.input_ctx.seek(target_ts, ..=target_ts)` に渡す。
`Input::seek` は `stream_index = -1` で `avformat_seek_file` を呼ぶため、
タイムスタンプは**マイクロ秒**として解釈される。

これは映像側で既に修正済みのバグと同一。`seek_target` の doc コメント（`decoder.rs:165-171`）と
`decoder.rs:895` の回帰テストが同じ問題を記述しているが、音声経路は更新されなかった。

## 影響

AAC の典型的な時間基準 1/44100 で、t 秒への seek が t×44100/1e6 ≒ 目標の 4.4% の位置に着地する
（実質ファイル先頭付近）。その後パケットループが前方デコードするため、
位置 N のチャンクデコードコストが O(N) で増大する。
チャンク単位で音声をストリームする呼び出し元は毎回ほぼファイル先頭から再デコードする。

## 修正方針

映像経路の `SeekTarget` を再利用する（`start_sample / sample_rate` から `pts` と `micros` の
両方を計算）。`seek` にはマイクロ秒値を渡す。

## 検証

- 非ゼロ `start_sample` での seek 着地位置の回帰テスト（映像側テストと対称に）

## 関連

- [HIGH-11](HIGH-11-audio-chunk-no-trim.md) — 同関数の対になるバグ。同時に直すべき
