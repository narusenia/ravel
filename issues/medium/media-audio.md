# medium — ravel-media / ravel-audio

---

## MED-MED-01 | bug | 全ての映像デコードを 8bit RGBA 経由に強制し、10bit / float の精度を f32 バッファ到達前に破壊する

**該当**: `crates/ravel-media/src/decoder.rs:741-750`

`convert_video_frame_to_rgba` は常に `PixelFormat::RGBA`（8bpc）へ変換し 255 で割る。
ProRes 422 10bit（このクレートが明示的に謳っているコーデック）、DNxHR、
EXR / DPX 画像シーケンス（`image_seq.rs` も同じ関数を通る）が、
f32 `FrameBuffer` に入る前に 8bit に量子化される。
f32 フレーム型がまさにその精度を運ぶために存在するモーショングラフィックスパイプラインで、
バンディングと HDR / linear データの損失が発生する。

**修正方針**: ソースフォーマットに応じて sws のターゲットを選ぶ。
高ビット深度 / float ソースは `RGBAF32`（または `RGBA64`）へ変換し適切なスケールで割る。
8bit RGBA は 8bit ソースのみに使う。

**関連**: [HIGH-17](../high/HIGH-17-sws-scaler-recreated-per-frame.md)（同関数。同時に手を入れる）

---

## MED-MED-02 | perf | `read_image_frame` が静止画1枚ごとにハードウェアデバイスコンテキスト込みのデコーダを構築する

**該当**: `crates/ravel-media/src/image_seq.rs:74-86`, `crates/ravel-media/src/decoder.rs:361-366`

画像シーケンスの各フレームが `FfmpegDecoder::open` を通り、avformat のプローブと
`HwDeviceContext::try_create`（VideoToolbox / CUDA デバイス作成）を実行する
— HW アクセラレーションを使えない PNG / EXR 静止画に対して。
`MediaProcessor::decode_image` はパスキーで1フレームだけキャッシュするため、
24fps のシーケンス再生ではフレームごとにデバイス作成 + プローブ + open を払う。

**修正方針**: 単一画像入力では HW デバイス作成をスキップする
（`open` ではなく最初の映像デコード呼び出し時に遅延生成する）。
併せてシーケンスに複数フレームキャッシュを与える — **後者は
`docs/implementation/cache-plan.md` の CACHE-8（アセット単位の共有キャッシュ）が
引き受ける**ので、この項目に残るのは HW デバイス作成の回避だけ。

---

## 低優先の付随項目

以下は [low/backlog.md](../low/backlog.md) に記載。

- `hw_get_format` のフォールバックが先頭要素（別の HW フォーマットの可能性）を返す
- prep スレッドのコメントが存在しない送信タイムアウトを約束している
- FFmpeg ラッパーに対する包括的 `unsafe impl Send`
