# [HIGH-32] 線形 ingest が画素ごとに f64 の transfer function を評価し、デコードが 1 フレーム数十 ms に落ちる

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-media / デコーダ、ravel-core / color |
| 該当 | `crates/ravel-core/src/color.rs:147-176`（`Transfer::decode_f64`）、`crates/ravel-core/src/color.rs:596-627`（`ingest_rgba8` / `ingest_rgba16`）、`crates/ravel-media/src/decoder.rs:1040-1095`（`framebuffer_from_rgba8` / `framebuffer_from_rgba64`） |
| 導入 | 2026-08-10（`93919c6`、PR #371 に同梱。`a1b571d` / `cc7e194` が上乗せする） |

## 現状

`93919c6`（線形作業空間への ingest、`CM-2`）が `FfmpegDecoder` に
`input_color_space` を足した。既定は `ColorSpace::WORKING` で変換は no-op だが、
`media` ノードは `with_input_color_space` で実際の色空間を渡す
（`crates/ravel-nodes/src/media.rs:333`）。その結果、**再生経路でデコードされる
全フレームの全画素に transfer function の逆変換が掛かる**。

コストが重い理由は 3 つ重なっている。

1. **f64 で評価する。** `Transfer::decode_f64` の doc が明記しているとおり、
   PQ の丸め誤差のために全 transfer を倍精度で評価する。`powf` は
   チャンネルごと 1 回（PQ は 2 回）
2. **直列。** `ravel-media` は `rayon` に依存していない。1080p の
   200 万画素を 1 スレッドで舐める
3. **ingest 側にテーブルが無い。** exit 側は `DISPLAY_CODE_THRESHOLDS`
   （255 要素）で `powf` を消してあるのに、入口の `ingest_rgba8` /
   `ingest_rgba16` は `convert()` を直接呼ぶ

さらに同日の 2 コミットが上乗せする:

- `a1b571d`: 8 bit 超の素材が RGBA64 経由になり `ingest_rgba16` へ。
  **sws 側の増分は無い**（下記の実測で否定済み）。効くのは 16 bit レーン読みと
  倍のメモリ帯域
- `cc7e194`: コンテナの色メタデータを読むようになった。素材が PQ タグ付きなら
  `Transfer::Pq` に解決され、`powf` がチャンネルごと 2 回になる

## 影響

**1080p の再生でフレームあたり数十 ms。** 60 fps 予算 16.7 ms を単独で超える。

デコーダの既定が no-op なので、**払うのは `media` ノードの再生経路だけ**。
サムネイル生成とプローブは影響を受けない。

`HIGH-16`（デコード済みフレームキャッシュ）が `CACHE-8` で解決済みなので
スクラブの往復では効かないが、**順再生では毎フレームが未キャッシュ**なので
そのまま乗る。

## 実測

同じ演算を単体で切り出して 1080p 1 フレーム分（`rustc -O`、
Apple M 系 / macOS、loadavg 6〜19、3 ラウンド）:

| 経路 | 1 フレーム |
|---|---|
| sRGB / Rec709（`powf` 1 回 / チャンネル） | 30.4 / 31.5 / 53.4 ms |
| PQ（`powf` 2 回 / チャンネル） | 78.2 / 79.4 / 82.5 ms |

これは**下界**で、実コードはさらに `odd()` のラッパ、`convert()` の f64 往復、
画素ごとの `extend_from_slice`、フレームごとの 33 MB `Vec` 確保を払う。

**sws スケーラの出力形式は無関係だと確認した。** 10 bit の 1080p / 150 フレームを
`ffmpeg -threads 1` で `rgba` と `rgba64le` へ交互に変換した比は 1.00〜1.03 で、
差は無い（`a1b571d` の RGBA64 化そのものはコストではない）。

> 変換 / コピー / 確保の内訳はリポジトリ内のハーネスで測り直す必要がある。
> 上表は「transfer function だけでこの桁になる」ことを示すだけで、
> 60 ms の内訳ではない。

## 修正方針

**線形 ingest 自体は `CM-2` の意図した正しさの変更なので、戻さずに安くする。**

1. **ingest 側の LUT。** u8 入力は 256 通りしかないので、色空間ごとに
   256 要素の `[f32; 256]` を持てば **厳密に一致**する（exit 側の
   `DISPLAY_CODE_THRESHOLDS` と同じ論拠 — 入力が有限で、変換が単調）。
   u16 は 65 536 要素 × 4 B = 256 KB で、これも厳密
2. **float（EXR）はテーブル不可**なので rayon で行分割する。
   `ravel-media` への `rayon` 追加が要る
3. `HIGH-17` の方針 1・3（スケーラのキャッシュ、出力バッファのプール）は
   同じ関数を触るので合わせて回収する

## 検証

- 1080p / 4K のデコード 1 フレームあたり時間を、色空間ごと
  （sRGB / Rec709 / PQ / Linear）に計測して `perf-baseline.md` に記録する
- **LUT の出力が `convert()` と厳密に一致する**ことをテストで固定する
  （`the_display_table_reproduces_the_transfer_function` と同じ形）
- 既定の no-op 経路（`ColorSpace::WORKING`）がテーブルを引かずに素通りすること

## 関連

- [HIGH-17](HIGH-17-sws-scaler-recreated-per-frame.md) — 同じ関数。
  スケーラ再生成と per-pixel ループ。**同時に直すのが自然**
- [HIGH-16](../closed/HIGH-16-no-decoded-frame-cache.md) — 解決済み。
  順再生では効かないので本件を隠さない
- [closed/HIGH-31](../closed/HIGH-31-float-decode-through-8bit-rgba.md) —
  `a1b571d` が解決した本体。本件はその副作用ではなく `93919c6` が主因
- `docs/specifications/color-management.md` — ingest 規則（`CM-2`）
