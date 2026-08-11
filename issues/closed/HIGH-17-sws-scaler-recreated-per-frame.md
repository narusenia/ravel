# [HIGH-17] sws スケーラをフレームごとに再生成し、スカラー per-pixel 変換でデコード経路を圧迫する

**解決済み** — この PR

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-media / デコーダ・エンコーダ |
| 該当 | `crates/ravel-media/src/decoder.rs:741-771`, 対称の問題が `crates/ravel-media/src/encoder.rs:118-132` |

## 現状

`convert_video_frame_to_rgba` はデコードフレームごとに `sws::Context::get` を呼ぶ
（毎回フィルタテーブル構築）。その後ネストループでピクセルごとに
境界チェック付き `Vec::push` を4回実行する。

1080p では約 830万回の push と、フレームごとに新規 33MB の `Vec<f32>` 確保（4K で 132MB）。
これが表示フレームごと、さらにスクラブ中は再デコードした GOP フレームごとに走る。

## 影響

デコード経路の CPU 時間を支配する。[HIGH-16](../closed/HIGH-16-no-decoded-frame-cache.md) と乗算的に効く。

## 修正方針

1. スケーラを `CachedVideoDecoder` にキャッシュ（フォーマット・サイズ変化時のみ再生成）
2. push ループを事前確保バッファへの行単位書き込みに置換
   （`chunks_exact` + `iter().map()`、または u8→f32 の 256 エントリ LUT）
3. フレームごと確保ではなくプールされたバッファを再利用

> 追記: 方針 2 は #378 が先に回収した。現在の decoder.rs は画素ごとの
> `Vec::push` ではなく、u8/u16 の厳密 LUT と float の rayon 行分割を使う。

> **方針 3（バッファのプール）は採らなかった。** 一度実装したうえで撤回している。
> decoder が返す `FrameBuffer` は production では必ず上流の
> [`MediaFrameCache`](../../crates/ravel-media/src/frame_cache.rs) が
> `Arc` で保持する（`crates/ravel-nodes/src/media.rs`）。したがって
> プールのスロットは常に `strong_count >= 2` で、**再利用条件が production では
> 一度も成立しない**。それでいて decoder 側が強参照を握り続けるので、
> `CacheBudget` がキャッシュエントリを evict してもバイトが解放されず、
> プールは high-water mark で永久に縮まない — 予算の外に常駐するメモリができる。
> 1080p RgbaF32 は 1 枚 33.2 MB、4K は 132.7 MB なので、4K なら約 15 枚で
> 2 GiB の RAM 予算に匹敵する量が会計から外れる。`frame_cache.rs` の
> モジュールドキュメントが "unsafe direction" と名指ししている失敗そのものであり、
> **発火しない最適化のために予算の正しさを失う取引**になる。
> 票を閉じるのは方針 1 のみで、方針 3 は意図的に見送った。

> **方針 1 の実測効果は小さい。** 1080p / 8bit の `decode_video_frame` で
> **-0.06〜0.10 ms（約 1%）**。この経路のコストは ingest が約 89% を占めており、
> スケーラ生成はその外側の小さな項でしかない。詳細と計測順バイアスの訂正は
> [`../../docs/implementation/perf-baseline.md`](../../docs/implementation/perf-baseline.md)
> の `HIGH-17` 節。

## 検証

- 1080p デコードのフレームあたり時間とアロケーション量を計測
- スケーラ生成回数がフォーマット変化回数と一致することを確認

## 関連

- [HIGH-16](../closed/HIGH-16-no-decoded-frame-cache.md)
- [medium/media-audio.md](../medium/media-audio.md) — 同関数の 8bit 固定変換による精度損失
