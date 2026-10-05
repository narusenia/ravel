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
