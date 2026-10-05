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

## MED-MED-07 | bug | 素材の色メタデータが読まれず、入力色空間の解決が常に拡張子既定へ落ちる

**該当**: `crates/ravel-app/src/media/import.rs:255`（`metadata_from_info`）、
`crates/ravel-core/src/media/*`（`MediaInfo` / `VideoStreamInfo`）

> **解決済み**: プローブがコンテナの宣言を読むようになった。
> `VideoStreamInfo` に `color_primaries` / `color_transfer` / `color_matrix`
> を追加し、FFmpeg の `AVColorPrimaries` / `AVColorTransferCharacteristic` を
> Ravel の語彙（`Primaries` / `Transfer`）へ写す。`metadata_from_info` は
> **名前のある組だけ**を `AssetMetadata::color_space` へ書く（
> `ColorSpace::name()` で往復できるものに限定）ので、未宣言・解釈不能は
> `None` のまま拡張子既定へ落ちる。どちらを採ったかのログは media ノードが
> 素材ごとに 1 度出す既存の仕組み（`ColorSpaceSource`）がそのまま効く。
> 静止画の EXR `chromaticities` / PNG `iCCP`・`gAMA` は未対応のまま —
> FFmpeg の codecpar には載らない情報で、専用の読み取りが要る。

`docs/specifications/color-management.md` は素材の入力色空間を 3 段で解決すると
規定している。

1. 明示指定（`MediaAssetEntry::color_space`）
2. **ファイルのメタデータ**（`AssetMetadata::color_space`）
3. 拡張子ごとの既定（float 形式 → リニア Rec.709、整数形式 → sRGB）

**優先順位 2 が常に空**。`metadata_from_info` は `color_space: None` を
無条件に書いており、`MediaInfo` / `VideoStreamInfo` にも色に関するフィールドが
無い（`pixel_format` はあるが色空間ではない）。プローブがファイルから色情報を
一切取り出していないので、ユーザーが明示指定しない限り**実データでは必ず
拡張子既定に落ちる**。

**影響**: 拡張子が嘘をつく素材で色が狂う。

- Rec.709 ではなく sRGB でグレーディングされた `.mov`、あるいは
  逆に log で収録された `.mov` — どちらも「整数形式 → sRGB」で読まれる
- `.tif` / `.dpx` は仕様どおり**リニア扱いにしない**方針だが、実際に
  リニアなファイルでもメタデータからそれを知る手段が無い
- 誤ったときの被害は非対称で、リニアを sRGB と誤ると暗く、逆は明るくなる。
  仕様はその非対称を承知で既定を選んでいるが、**既定に落ちる頻度が
  「メタデータが無いとき」ではなく「常に」になっている**のは想定外

**修正方針**: プローブに色情報を通す。

- `VideoStreamInfo` に色空間 / 伝達特性 / 原色（FFmpeg の
  `AVColorSpace` / `AVColorTransferCharacteristic` / `AVColorPrimaries`）を足し、
  `metadata_from_info` が `ColorSpace::from_name` の語彙へ写す
- 静止画は FFmpeg 経由では拾えない情報がある。**EXR の `chromaticities`**、
  **PNG の `iCCP` / `gAMA`** は専用の読み取りが要る
- **不明は `None` のまま**。推測で埋めると 3 段目の既定より悪くなる。
  仕様どおり「2 と 3 のどちらを採ったか」を素材ごとに 1 度ログへ出す

**関連**: [HIGH-31](HIGH-31-float-decode-through-8bit-rgba.md) とは独立。
あちらは取り込みのビット幅、こちらは入力色空間の判定。
設定 UI（明示指定を与える手段）は `CM-8`。

---

## MED-MED-08 | bug | 共有デコードキャッシュのキーに素材の版が無く、同一パスの上書きで古いフレームを返し続ける

> **解決済み**: `PROV-4` + `PROV-5`。個票の 2 案のうち「監視」を取り、版は
> `MediaAssetEntry::content_revision`（セッション限り・非永続）に持たせた。
> 版は `FrameKey` と `OpenReader` のキーに入り、`media_assets` の文書 diff が
> フレームキャッシュとノードキャッシュの無効化を引き受ける。`ravel-app` の
> `AssetWatch` が解決済みパスの親ディレクトリを `notify` で監視し、変更を 300 ms
> まとめて `ProjectState::advance_changed_assets` へ渡す。これは `rederive` で
> 版を進め（undo 段なし・dirty にしない）、飛んでいる結果を fence してから再要求する。
> 連番はディレクトリ単位で一致させる。パスをキーにした残り 2 つ（再生の音声デコード
> キャッシュ、Media Bin のサムネイル）も版を進めた素材について捨てる。
> mtime の毎フレーム `stat` は採らなかった。
> 検証は `ravel-app` の `media::watch` のテスト、`an_overwritten_file_*`（fence と
> 非編集性）、音声とサムネイルの破棄のテスト。監視そのものは手動確認。

**該当**: `crates/ravel-media/src/frame_cache.rs`（`FrameKey`）

キーは `(解決済みパス, 入力色空間, ストリーム番号, フレーム番号)` で、**ファイルの
mtime も内容の版も含まない**。プロジェクトを開いたまま素材を同じパスへ書き出し
直す（レンダーの差し替え、外部ツールでの再書き出し、`git checkout`）と、
`CACHE-8` のキャッシュは古いデコード結果を返し続け、予算で落ちるまで直らない。

**新種の退行ではない。** 置き換える前の `MediaProcessor` の 1 エントリ
キャッシュ（開いたリーダーと `CachedImage`）も mtime を見ていなかった。ただし
共有キャッシュは**アセット単位で予算いっぱいまで保持する**ので、古い絵が
生き残る時間と枚数が増えており、**露出は上がっている**。

`CACHE-8` のリリンクのテスト（`a_relinked_asset_never_hits_the_old_paths_frame`）が
証明しているのは**パスが変わる場合**だけ。パスが同じまま中身が変わる経路は
どのテストも見ていない。

**修正方針**: mtime をキーに入れるのは**採らない** — デコード経路は 1 フレーム
ごとに通るので、毎フレームの `stat` はキャッシュヒットの利得を食う（ヒットは
本来ハッシュ 1 回で済む）。取るなら次のどちらか:

- **インポート時に 1 度だけ** mtime + size を読み、`MediaAssetEntry` の版として
  持ち、キーに含める。ファイルシステムを触る回数がアセットあたり 1 回になる
- 素材ディレクトリの**監視**（`notify` は既に依存にある）で、変更されたパスの
  エントリだけ落とす

どちらも `CACHE-8` の範囲外で、素材の版という概念を先に決める必要がある。

---

## MED-MED-09 | bug | 静止画・連番の EXR / PNG の色メタデータ（chromaticities / iCCP / gAMA）が読まれない

> **解決済み**: `ravel_media::color_probe` がヘッダだけを読み（画素は復号しない）、
> `format::probe` が FFmpeg の宣言が無いときに `color_primaries` / `color_transfer` を埋める。
> 連番は先頭フレームが同じ経路を通る。EXR は `colorInteropID`（既知 4 種）→ `chromaticities`
> （Rec.709 / Rec.2020 / AP1、xy ±0.002、リニア）の順。PNG は `cICP` → `iCCP`（解析せず `None`）
> → `sRGB` → `gAMA` 1.0（`cHRM` 無しなら Rec.709）の順。`exr` / `png` は `image` が既に解決している
> 版を直接依存にしたので lockfile にパッケージは増えていない。対応表は
> `docs/specifications/color-management.md`。`ffmpeg` 有効ビルドでの配線はまだ誰もコンパイルしていない
> （[MED-MED-10](../medium/media-audio.md)）。PR #580。


**該当**: `crates/ravel-media/src/decoder.rs`（`build_media_info` のプローブ）、
`crates/ravel-media/src/image_seq.rs`

[MED-MED-07](../closed/medium-media-audio.md) でコンテナの色宣言
（`color_primaries` / `color_trc`）はプローブが読むようになったが、
**静止画と連番はその経路では拾えない情報を持つ**。EXR の `chromaticities`
属性と PNG の `iCCP` / `gAMA` チャンクは FFmpeg の codecpar に載らない
ので、専用の読み取り（EXR ヘッダ属性 / PNG チャンクのパース）が要る。

現状、これらの素材はメタデータ段（解決順の優先順位 2）が常に空で、
**拡張子既定へ落ちる** — float 形式（`exr` / `hdr`）はリニア Rec.709、
整数形式（PNG など）は sRGB とみなされる
（`docs/specifications/color-management.md` の解決順 3 段目）。

これは実害になりうる。color-management.md の前提として、**EXR は別の
色空間の値を入れて配布されうる**（`MediaAssetEntry::color_space` の
コメントが「a `.exr` really can carry sRGB-encoded values」と明記している
とおり）。`chromaticities` に Rec.2020 原色が書かれた EXR や、
`gAMA` / `iCCP` が sRGB と異なる PNG は、ファイルが自分の色を告げているのに
それを読まず既定で上書きすることになる。誤ったときの被害は非対称で、
リニアを sRGB と誤ると暗く、逆は明るくなる。

**修正方針**:

- EXR はヘッダの `chromaticities` 属性を、PNG は `iCCP` / `gAMA` チャンクを
  読み、`VideoStreamInfo` の `color_primaries` / `color_transfer`（
  MED-MED-07 で足したフィールド）へ写す。`image_seq` の連番経路は代表
  フレームから同じ読み取りを行う
- **不明・解釈不能は `None` のまま** — MED-MED-07 と同じ規約で、推測で
  埋めると拡張子既定より悪くなる。どちらを採ったかは media ノードの
  `ColorSpaceSource` ログがそのまま効く
- `image` クレートの EXR / PNG デコーダがこれらの属性を公開するかを
  まず確認すること。公開しない場合は最小限のヘッダパースを自前で持つか
  どうかの判断になる（依存追加は要相談）

**関連**: [MED-MED-07](../closed/medium-media-audio.md)（コンテナ側の
色メタデータ。本項目は静止画・連番側の残り）、
[HIGH-31](../closed/HIGH-31-float-decode-through-8bit-rgba.md)（取り込みの
ビット幅。色空間の判定とは独立）

---

## MED-MED-06 | bug | 連番の最終配置が置換なので、レンダーワーカーの上書き拒否を競合で迂回できる

> **解決済み**: `ImageSequenceEncoder::with_overwrite` で上書き方針を受け取り、
> `OverwritePolicy::Refuse` のときは最終配置を `std::fs::hard_link` で行う（POSIX の
> `link(2)` と Windows の `CreateHardLinkW` は名前が埋まっていれば `AlreadyExists` で
> 失敗し、置換しない）。**修正方針の OS 別 syscall は採らなかった** — 同じ保証が
> stdlib で得られ、`cfg` 分岐も依存追加も要らない。ハードリンクの無いファイル
> システム（exFAT / FAT、一部のネットワーク共有）では `AlreadyExists` 以外の失敗で
> 直前の存在確認 + `rename` へ戻るので、そこでは競合の窓が狭まるだけで閉じない。
> 事前検査はそのまま残した。PR #582。


**該当**: `crates/ravel-media/src/encode/sequence.rs:298`,
`crates/ravel-core/src/runtime/render.rs`（`check_preconditions`）

`ImageSequenceEncoder` は一時ファイルを `create_new` で作ってから
`std::fs::rename` で最終名へ移す。`rename` は**既存ファイルを黙って置換する**
（`sequence.rs:279-281` のコメントが「既存フレームは置換されてよい。範囲の
再レンダーは正当な操作」と、意図した挙動であることを述べている）。

`EXPORT-2` のレンダーワーカーはこの上に、ジョブ開始前に出力先を調べて
既存フレームがあれば 1 フレームも評価せずに失敗する事前ガードを載せた。
これは順次の再レンダー（圧倒的多数のケース）を塞ぐが、**検査と `rename` の
間に別プロセスがファイルを作ると、既定の拒否設定でも置換が通る**。

実害は限定的で、踏むには検査後・書き込み前という窓に別の書き手が入る必要が
ある。範囲を分割した並行レンダー（`--range`）は互いに素な名前を書くので
通常は衝突しない。**現状で成果物が壊れる経路ではなく、ガードが原子的でない
という限界**。

**修正方針**: 事前検査は高速な早期失敗として残したうえで、最終配置にも
「置換禁止」を渡す。`OverwritePolicy::Refuse` のときだけ no-replace な
rename を使う（Linux は `renameat2(RENAME_NOREPLACE)`、macOS は
`renamex_np(RENAME_EXCL)`、Windows は `MoveFileEx` を置換フラグなしで）。
プラットフォーム分岐が `ravel-media` に入るので、**Windows CI が回る状態で
着手すること**。`EXPORT-1` の書き込み経路に手を入れる変更になる。

---
