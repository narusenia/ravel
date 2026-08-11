# [HIGH-16] デコード済みフレームキャッシュが無い — 再表示・逆方向スクラブで GOP 丸ごと再デコード

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-media / デコーダ, ravel-nodes / media |
| 該当 | `crates/ravel-media/src/decoder.rs:430-449`, `crates/ravel-nodes/src/media.rs:129-158` |

> **解決済み**: `CACHE-8` が `ravel-media` に共有デコードフレームキャッシュを
> 置いた（`crates/ravel-media/src/frame_cache.rs`）。キーは
> `(解決済みパス, 入力色空間, ストリーム番号, フレーム番号)` で、
> **アセット単位**なので同じ素材を使う複数レイヤーが 1 部を共有する。
> `MediaProcessor` は開いたリーダーだけを持ち、デコード結果はこのキャッシュへ
> 入る。上限は `CacheBudget` の `CacheKind::MediaFrame` が単独で持ち、
> 退避は LRU（ヒットで `touch`）。回帰テストは
> `scrubbing_backwards_does_not_decode_again` /
> `two_layers_on_one_clip_share_the_decode` /
> `a_frame_past_the_budget_is_dropped_and_decoded_again`。
> 修正方針の「最低限」（直近 1 フレームのメモ化）は採らず、望ましい側の
> バイト予算付き多エントリを直接入れた — 予算の権威が既に 1 つある以上、
> 途中段階を挟む理由が無い。

## 現状

高速経路は `can_continue` のみで、条件は `target_pts > last_returned_pts`。
つまり

- 同一フレームの再要求（一時停止中の再描画、パラメータ編集による再評価）
- 任意の逆方向スクラブ

はすべて `can_continue = false` となり、デコーダを flush して直前キーフレームへ seek し、
GOP を前方デコードし直す。60フレーム GOP の H.264 なら表示1フレームあたり約30フレームの無駄デコード。

`MediaProcessor` がキャッシュしているのは開いたリーダーのみ。
`decode_container_frame` に `(path, frame) → FrameBuffer` のメモ化は無く、
静止画だけが1エントリの `CachedImage` を持つ。

## 影響

タイムライン上の逆方向スクラブが前方再生より GOP 長倍だけ高コスト。
報告されている「もっさり」と症状が一致する。

## 修正方針

- 最低限: `CachedVideoDecoder` に直近の `(frame_number, FrameBuffer)` をメモ化し、
  同一フレーム再要求をゼロコストにする
- 望ましい: `MediaProcessor` に `(path, frame)` キーの小さな LRU（フレームは既に `Arc`）を
  バイト数上限で持ち、プレイヘッド近傍のスクラブがキャッシュヒットするようにする

## 検証

- 同一フレーム2回要求でデコード回数が1回であることを確認
- 逆方向スクラブのフレームあたり時間を計測

## 関連

- **`docs/implementation/cache-plan.md` の CACHE-8 が引き受ける**（アセット単位の
  共有キャッシュにし、バイト予算を単一の権威の下に置く）
- [HIGH-17](HIGH-17-sws-scaler-recreated-per-frame.md) — 同じデコード経路のコスト
- [medium/media-audio.md](../medium/media-audio.md) — 画像シーケンスの毎フレームデコーダ生成
