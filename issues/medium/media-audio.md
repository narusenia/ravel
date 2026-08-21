# medium — ravel-media / ravel-audio

---

`MED-MED-01`（全ての映像デコードが 8bit RGBA を経由する）は
[HIGH-31](../closed/HIGH-31-float-decode-through-8bit-rgba.md) へ昇格した
（2026-08-10）。フェーズ CM で仕様が「リニア EXR を取り込む」と規定したため、
同じ欠陥の深刻さが上がった。**解決ではなく移動**だったが、その `HIGH-31`
自体もこの変更で解決し `closed/` へ入った。`MED-MED-01` の ID は再利用しない。

---

## MED-MED-02 | perf | `read_image_frame` が静止画1枚ごとにハードウェアデバイスコンテキスト込みのデコーダを構築する

**該当**: `crates/ravel-media/src/image_seq.rs:74-86`, `crates/ravel-media/src/decoder.rs:361-366`

画像シーケンスの各フレームが `FfmpegDecoder::open` を通り、avformat のプローブと
`HwDeviceContext::try_create`（VideoToolbox / CUDA デバイス作成）を実行する
— HW アクセラレーションを使えない PNG / EXR 静止画に対して。
**複数フレームキャッシュの側は `CACHE-8` が解決した**（2026-08-10）。シーケンスの
各フレームは `ravel-media` の共有デコードキャッシュに入り、予算が許すかぎり
常駐する（`crates/ravel-media/src/frame_cache.rs`。回帰テストは
`a_sequence_keeps_the_recent_frames`）。**残っているのは HW デバイス作成の回避
だけ**で、それはキャッシュがミスしたフレームの代価として今も払われている。

**修正方針**: 単一画像入力では HW デバイス作成をスキップする
（`open` ではなく最初の映像デコード呼び出し時に遅延生成する）。

---

## MED-MED-06 | bug | 連番の最終配置が置換なので、レンダーワーカーの上書き拒否を競合で迂回できる

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


## MED-MED-08 | bug | 共有デコードキャッシュのキーに素材の版が無く、同一パスの上書きで古いフレームを返し続ける

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
## MED-MED-09 | bug | 静止画・連番の EXR / PNG の色メタデータ（chromaticities / iCCP / gAMA）が読まれない

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

## 低優先の付随項目

以下は [low/backlog.md](../low/backlog.md) に記載。

- `hw_get_format` のフォールバックが先頭要素（別の HW フォーマットの可能性）を返す
- prep スレッドのコメントが存在しない送信タイムアウトを約束している
- FFmpeg ラッパーに対する包括的 `unsafe impl Send`

## MED-MED-10 | debt | CI が `--features ffmpeg` を一度もビルドしない — 出荷する構成が検証されていない

**該当**: `.github/workflows/ci.yml`、`crates/ravel-media/Cargo.toml`
（`ffmpeg = ["dep:ffmpeg-the-third"]`、`default = []`）

`ffmpeg` はワークスペース全体でオプトインのフィーチャで、`ci.yml` に
`--features` が 1 つも出てこない。つまり **CI は素材のデコードとエンコードを
含むビルドを一度も compile していない**。`mise run check` も同じ既定なので、
手元でも同じ。

`#[cfg(feature = "ffmpeg")]` の中身は型検査すら通らないまま `main` に入りうる。
実際にこの穴で見つかったもの:

- `ravel-cli` の `probe_asset`（`media-unreadable` の警告、`WARN-2`）は
  cfg の中にあり、**CI では compile されない**
- `ravel-media` の `read_image_frame` / `format::probe` / `decoder.rs` の
  大部分が同じ位置にある

**影響**: リリースは当然 `--features ffmpeg` で作る（そうでなければ動画が
読めない）。その構成だけが壊れていても、PR は緑のまま通る。
`Cargo.lock` の再解決が Windows のクレートを降格させる類の「手元では絶対に
見えない」破損と同じ形で、**気づく場所が無い**のが問題。

**修正方針**: `ci.yml` に `--features ffmpeg` のジョブを 1 本足す。
少なくとも `cargo clippy -p ravel-media -p ravel-nodes -p ravel-cli
--features ffmpeg --all-targets` までは、FFmpeg の開発ヘッダを入れれば
runner で通る（macOS は `brew install ffmpeg`、Windows は vcpkg か
プリビルド）。テストの実行までやるかは別の判断 — 素材ファイルを用意する
必要があるので、**まず型検査だけでも価値がある**。

`ffmpeg-the-third` のバージョンと runner の FFmpeg の互換が要る点に注意
（手元では ffmpeg 8.1 と `ffmpeg-the-third 5.0` が非互換で、
`cargo clippy --all-features` が依存クレート内で落ちる）。CI では
runner の FFmpeg を固定して入れる方が安定する。

**備考**: `WARN-2`（#467）の独立レビュー中に見つけた。
