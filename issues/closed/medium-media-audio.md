# closed / medium — ravel-media / ravel-audio

解決済みの medium 項目。個票は起票時のまま残し、各項目の **解決済み** 行が結果を記録している。

未解決分は [`../medium/media-audio.md`](../medium/media-audio.md)。

---

## MED-MED-03 | bug | `resample_buffer` が sinc フィルタのテールを捨て、フィルタ遅延も補償しない

**該当**: `crates/ravel-audio/src/resampler.rs:163-194`

> **解決済み**: フェーズ A3。`resample_buffer` が `output_delay()` 分の先頭を捨て、
> 末尾はゼロ入力を押し込んで遅延した最終サンプルを取り出す。`sinc_len` は 64
> （`crates/ravel-audio/src/resampler.rs:60`, `:203-207`, `:239`）。

固定サイズチャンクを送る（最後はゼロパディング）が、入力が尽きた後にリサンプラを flush せず、
初期 `output_delay()`（約 sinc_len/2 = 128 入力フレーム）もトリムしない。
結果、リサンプルされた全トラックがタイムライン配置に対して約 2.9ms 遅れて始まり、
末尾の約 128 フレーム超が失われる。48kHz エンジンを通る 44.1kHz 素材すべてが影響を受ける。

**修正方針**: 入力ループ後に `process_partial(None, ...)` を rubato が出力を返さなくなるまで呼ぶ。
出力の先頭 `output_delay()` フレームをスキップして時間軸を揃える。

**関連**: [HIGH-15](HIGH-15-settrack-resamples-on-prep-thread.md)（同じリサンプル経路）

---

## MED-MED-04 | bug | 音声エンコーダが 3ch 以上を STEREO レイアウトにマップし、マルチチャンネル書き出しが必ず失敗する

**該当**: `crates/ravel-media/src/encoder.rs:171-175`, `:182-203`

> **解決済み**: フェーズ A3。レイアウトは
> `ChannelLayout::default_for_channels(channels)` から取る
> （`crates/ravel-media/src/encoder.rs:90`, `:268`, `:490`）。

`write_audio_chunk` は 2ch 超のチャンネル数すべてで `ChannelLayoutMask::STEREO` に
フォールバックするが、コピーするサンプル数は `samples_this_chunk × channels`。
2ch で確保したフレームは 6ch 入力の `byte_count` より小さいため、
プレーンサイズチェックが発火し「audio frame plane too small」という誤解を招くエラーになる。
5.1 音声はエクスポートできない。
（エンコーダにアプリ側呼び出し元は現状無いため latent だが、エクスポート機能で必ず踏む。）

**修正方針**: マスクの match ではなく `ChannelLayout::default_for_channels(channels)`
（`create_audio_stream` では既に使用）を使う。
または `create` で未対応チャンネル数を明示的に拒否する。

---

## MED-MED-05 | bug | `write_audio_chunk` が固定フレームサイズコーデックに対してストリーム途中で短いフレームを出す

**該当**: `crates/ravel-media/src/encoder.rs:160-221`

> **解決済み**: フェーズ A3。`write_audio_chunk` は `audio_pending` に溜め、
> `frame_size` の倍数だけを送る。端数は `finalize` が eof の直前に流す
> （`crates/ravel-media/src/encoder.rs:205-238`, `:241-246`）。

各 `write_audio_chunk` 呼び出しが自分のバッファを `frame_size` フレームに切り、
最後の部分スライスを即座に送る。
AAC などのコーデックは短いフレームをストリーム**最終**フレームとしてのみ受け付ける。
長さが `frame_size`（AAC で 1024）の倍数でないチャンクをストリームする呼び出し元は
途中で短いフレームを送ることになり、エンコーダが拒否する（またはタイムスタンプに隙間ができる）。
呼び出し間のキャリーオーバーバッファが無い。

**修正方針**: `FfmpegEncoder` に pending サンプルバッファを持ち、
`write_audio_chunk` からは完全な `frame_size` フレームのみ送り、残りを `finalize` で flush する。

---

## MED-AUD-01 | debt | 出力ストリームが 48kHz ステレオ固定、デバイス能力を一切参照しない

> **解決済み**: フェーズ A3（2026-07-29）。`AudioEngineConfig::output` が `None` の場合に
> `AudioEngine::new` が `default_device_config()` を採用する（`engine.rs:217-220`）。

**該当**: `crates/ravel-audio/src/device.rs:31-38`, `:66-79`,
`crates/ravel-audio/src/engine.rs:113-125`

`OutputConfig::default` が 48kHz / 2ch を固定し、`AudioEngine::new` がそれを
`build_output_stream` にそのまま渡す。
デバイスを問い合わせる `default_device_config`（`device.rs:49`）はエンジンから見て dead code。

48kHz をサポートしないデバイスではストリーム構築が失敗し、
`AudioService` が `engine_unavailable` を立て、アプリは無言で音声なし（壁時計にフォールバック）になる。

**修正方針**: `AudioEngine::new` で `default_output_config()` を問い合わせ、
デバイスのレート / チャンネル数を採用する（ミキサーとリサンプラは既に両方をパラメータ化済み）。
48kHz へのフォールバックはデバイスが受け付ける場合のみ。

---

## MED-AUD-02 | bug | デコード上限を超える音声が無言で永久に無音になる

**該当**: `crates/ravel-app/src/audio/mixdown.rs:41`, `:287-321`,
`crates/ravel-app/src/audio/mod.rs:396-417`

> **解決済み**: offline、デコード上限、decode / SRC error を
> `AudioServiceEvent` として workspace へ送り、アセット ID と原因を含む
> 非自動消去の warning notification を表示する（#212、2026-07-30）。

`MAX_DECODE_BYTES` = 128MiB は 48kHz ステレオ f32 で約 5.8 分。これを超える音源は
`decode_full_audio` が `anyhow::bail!` し、`AudioService::request_decode` の完了ハンドラが
`tracing::warn!` して `failed` に入れるだけで終わる。ユーザーには通知されず、
そのレイヤーは**ドキュメントを差し替えるまで永久に無音**（`failed` は
`on_document_replaced` でしか消えない）。長尺の BGM やポッドキャスト素材は
普通にこの長さを超える。

**修正方針**: 少なくとも `push_notification` でユーザーに見せる
（`workspace.rs:1378` の経路。`HIGH-20` のメディアインポート失敗通知と同じ形）。
本質的にはメモリ常駐の全長デコードをやめてストリーミング再生へ移す判断が要るが、
それは `AUDIO-*` の設計変更なので別単位。

**関連**: [HIGH-23](HIGH-23-resampled-audio-not-cached.md)（同じデコード /
準備経路）、[HIGH-20](HIGH-20-media-import-failure-invisible.md)（無言の失敗の先例）

---

## MED-AUD-03 | debt | 音声の準備中（デコード / レート変換）が UI に一切出ない

**該当**: `crates/ravel-app/src/audio/mod.rs:69-80`, `:288-302`

> **解決済み**: `AudioService` の準備状態を Timeline と MediaBin が observe し、
> 対象の layer bar / asset row にローカライズした「準備中」を表示する
> （#212、2026-07-30）。

`SentTrack.delivered == false` は「spec は記録したがまだミキサーに届いていない」
状態を既に持っているが、この状態は UI へ出ない。ユーザーから見ると
「再生を押したのに音が出ない」と区別がつかない。

**修正方針**: `delivered == false` のレイヤーを Timeline のレイヤーバーと
MediaBin に「準備中」として出す。進捗率まで出す必要はない
（`HIGH-23` を直せば release では 4 分の曲で 1 秒未満）。
モーダルな進捗バーは書き出し（`EXPORT-*`）の進捗基盤と一緒に設計する。

**関連**: [HIGH-23](HIGH-23-resampled-audio-not-cached.md)（待ち時間そのものを
削るのが先。本項目はその残りを見せる話）

---
