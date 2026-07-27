# [HIGH-17] sws スケーラをフレームごとに再生成し、スカラー per-pixel 変換でデコード経路を圧迫する

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

デコード経路の CPU 時間を支配する。[HIGH-16](HIGH-16-no-decoded-frame-cache.md) と乗算的に効く。

## 修正方針

1. スケーラを `CachedVideoDecoder` にキャッシュ（フォーマット・サイズ変化時のみ再生成）
2. push ループを事前確保バッファへの行単位書き込みに置換
   （`chunks_exact` + `iter().map()`、または u8→f32 の 256 エントリ LUT）
3. フレームごと確保ではなくプールされたバッファを再利用

## 検証

- 1080p デコードのフレームあたり時間とアロケーション量を計測
- スケーラ生成回数がフォーマット変化回数と一致することを確認

## 関連

- [HIGH-16](HIGH-16-no-decoded-frame-cache.md)
- [medium/media-audio.md](../medium/media-audio.md) — 同関数の 8bit 固定変換による精度損失
