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
