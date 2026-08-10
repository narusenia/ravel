# [HIGH-31] float / EXR のデコードが 8bit RGBA を経由する — 1 超の値がクリップされ、f32 の精度が落ちる

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-media / デコード |
| 該当 | `crates/ravel-media/src/decoder.rs`（`convert_video_frame_to_rgba`）, `crates/ravel-media/src/image_seq.rs:82` |

> **解決済み**: デコードが素材の画素形式から取り込み経路を選ぶようになった。
> float の RGB 形式（`GBRPF32` / `GBRAPF32` / `RGBAF32`）はスケーラを通さず
> 面を直接読み、1 超の値をクリップしない。8bit 超の整数形式は RGBA64 へ
> スケールして `ingest_rgba16` で取り込む（`ingest_rgba16` /
> `ingest_rgbaf32` は `ravel_core::color` の新規関数で、既存関数の挙動は
> 不変）。8bit 素材の経路は従来どおりで、出力はビット単位で同一
> （`eight_bit_source_decodes_bit_identically`）。連番経路
> （`read_image_frame_in`）も同じデコーダを通るので一緒に直った
> （`float_source_keeps_values_above_one`）。

## 現状

`convert_video_frame_to_rgba` は素材の画素形式に関わらず、sws スケーラの
**出力を `PixelFormat::RGBA`（8bit 整数）に固定**している。

```rust
let mut scaler = sws::Context::get(
    frame.format(),
    width, height,
    PixelFormat::RGBA,   // ← 素材が float でもここで 8bit になる
    width, height,
    sws::Flags::BILINEAR,
)?;
```

その後 `ingest_rgba8` が `u8 → f32 / 255.0` して伝達関数を外すので、
**f32 のバッファに入るのは 8bit へ落とした後の値**でしかない。

対象は float 素材だけではない。**ProRes 422 10bit**（このクレートが明示的に
謳っているコーデック）、**DNxHR**、DPX のような 8bit を超える整数素材も、
f32 `FrameBuffer` に届く前に 256 段へ量子化される。f32 のフレーム型は
まさにその精度を運ぶために存在している。

## 影響

1. **1 を超える値がクリップされる。** EXR / HDR はハイライトに 1 超のリニア値を
   持つのが普通で、それが `PixelFormat::RGBA` の時点で頭打ちになる。
   ブルーム・露出調整・トーンマッピングの材料が失われ、後段で持ち上げても
   戻らない
2. **f32 の精度が落ちる。** 256 段に量子化されてから f32 に戻るので、
   暗部のバンディングが素材由来でなく取り込み由来で発生する。
   10bit 素材（ProRes 422 / DNxHR）でも同じで、モーショングラフィックスの
   グラデーションに素材には無いバンドが出る
3. **画像連番も同じ経路。** `image_seq::read_image_frame_in` は
   `FfmpegDecoder::open` → `decode_video_frame` を呼ぶだけなので、
   **EXR 連番の取り込みも同じ 8bit スケーラを通る**。連番素材を「リニアで
   持ち込める」と読める仕様書の記述に対して、実装は 8bit を経由している

## この欠陥は `CM` が持ち込んだものではない

`origin/main` の `crates/ravel-media/src/decoder.rs:900` に**同じスケーラが
そのまま存在する**（`convert_video_frame_to_rgba` の引数に
`input_color_space` が無いだけで、`PixelFormat::RGBA` は同一）。
フェーズ CM は `ingest_rgba8` を差し込んだだけで、8bit 経由は元からある。

**変わったのは深刻さの方。** CM 以前はパイプライン全体が display-referred な
8bit で自己整合していたので、取り込みが 8bit なのは前提と矛盾しなかった。
CM 後は `docs/specifications/color-management.md` が
「素材（sRGB PNG / Rec.709 動画 / **リニア EXR** …）→ 入力変換 → 32bit float
作業空間」と規定している。**仕様が約束している範囲を実装が満たしていない**
状態になったので high に上げる。

## 修正方針

**素材の画素形式から出力形式を選ぶ。** `frame.format()` が float 形式
（`AV_PIX_FMT_GBRPF32*` / `AV_PIX_FMT_RGBAF32*` など）または 8bit を超える
整数形式なら、スケーラの出力を `PixelFormat::RGBA64` か
`PixelFormat::RGBAF32`（利用可能なら）にし、そのビット幅に合う取り込み関数へ
渡す。

- `ravel_core::color` 側に `ingest_rgba8` の 16bit / f32 版が要る。
  **`ingest_rgba8` と同じ規約**（正規化 → 伝達関数の除去、アルファは非変換）を
  守ること。出口が `quantize_u8` に一本化されているのと同じ理由で、
  入口も 1 箇所に揃える
- f32 経路では**クランプしない**。1 超はそのまま作業空間へ通す
- 判定は「拡張子」ではなく **FFmpeg が報告する画素形式**で行う。
  入力色空間の解決（`MediaAssetEntry::input_color_space`）とは別問題で、
  こちらは「値が何段あるか」の話

## 備考

- **`MED-MED-01` を high へ昇格したもの**（2026-08-10）。元の個票は
  「10bit / float の精度が f32 バッファ到達前に壊れる」と書いており、
  この文書はその内容を取り込んだうえで、CM 後に仕様が
  「リニア EXR を取り込む」と約束したことによる深刻さの変化を足している。
  `issues/medium/media-audio.md` から `MED-MED-01` の項は取り下げた
  （解決ではなく移動なので `closed/` には入れない）
- `HIGH-17`（sws スケーラをフレームごとに再生成）と同じ関数を触るので、
  まとめて直すのが自然
- サムネイル経路の暗さ（`MED-APP-32`）とは**別の問題**。あちらは表示変換の
  欠落、こちらは取り込みのビット幅
- 素材の色メタデータが読まれない件（`MED-MED-07`）とも独立。仮に
  メタデータからリニアと判定できても、値が 8bit を経由する事実は変わらない
