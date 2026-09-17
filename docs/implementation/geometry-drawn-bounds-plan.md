# ジオメトリの bbox が測るものを 1 箇所にする実装計画

> **Status**: 未着手 — 2026-09-17

対象: `ravel-core` の `geometry`（`container` / `ops`）、`ravel-nodes` の
`rasterize`、`ravel-app` の Viewer（`panels/viewer/geometry.rs`）。
要件は `REQ-UI-013`（Viewer の bbox）と `REQ-MOGRAPH-001`（シェイプ）、
`REQ-MOGRAPH-004`（タイポグラフィ）。

解消する issue: `MED-APP-45` / `MED-CORE-11` / `LOW-APP-33`。
3 件とも `layer-content-size-plan.md` の「問題 2」で見つかった同じ走査の中にあり、
**同じ 1 つの欠陥の別の症状**なので 1 本で直す。

## 問題

**bbox は「置かれた位置」しか測っていない。** インスタンスが何を
スタンプするか（画像の矩形、グリフの輪郭）と、線がその位置からどこまで
届くかは、どちらも測る対象に入っていない。

症状が 4 つある。上 3 つは起票済み、4 つ目は 2026-09-17 の観察。

| # | 症状 | 取りこぼすもの | issue |
|---|---|---|---|
| 1 | 画像ジオメトリのレイヤーが Viewer から掴めない | `InstanceSource::Image` の `rect()` | `MED-APP-45` |
| 2 | 「このジオメトリの範囲」の答えがコアと Viewer で 2 つある | コアは Instance 域を見ない | `MED-CORE-11` |
| 3 | 太い線のシェイプが枠から溢れる | ストロークの張り出し | `LOW-APP-33` |
| 4 | **`text.to_path` 前の Text の bbox が高さ 0 の線になる** | `InstanceSource::Geometry`（グリフ輪郭） | 新規（別起票しない。下記） |

### 実測（`text.font` → `text.layout`、`"Ravel"`、`size = 72`）

| 測る側 | 結果 |
|---|---|
| コアの `bounds()` | `(0, 0, 0, 0)` — Point 域が空なので `positions_bounds` が `None` |
| Viewer の `geometry_bounds` | `(0, 0, 162.79, 0)` — **高さ 0**、左端が 6.62 でなく 0、幅も末尾グリフの分だけ足りない |
| インスタンスの source を置いて測った真値 | `(6.62, -51.12, 173.66, 51.98)` |
| 同じ文字列の `text.to_path` 出力 | `(6.62, -51.12, 173.66, 51.98)` — **真値と一致** |

`text.layout` は **点を 1 つも作らない**（`points = 0`、`instances = 5`、
`sources = 5`）。5 つのインスタンス位置はベースライン上のグリフ原点なので、
それだけを測れば高さ 0 の線分になる。`text.to_path`
（= `ops::expand_instances`）を通すと輪郭が点になるので、同じ文字列の bbox が
**ノードを 1 つ挟むだけで変わる**。

これが 4 つ目の症状で、**症状 1 と同じ 1 行**（`geometry_bounds` が位置だけを
走査する）が原因。`from_image` は source が `Image`、`text.layout` は source が
`Geometry` という違いしかない。新しい issue を起票せず本計画で扱うのは、
直す箇所が `MED-APP-45` と完全に同一だから。

### 4 つとも同じ根

`MED-APP-21`（Viewer の bbox が `type_key` の固定 match で再構成される、解決済み）
が「**どのノードが描くか**」の答えを 1 箇所に寄せたのと同じで、今回は
「**何を測るか**」の答えが 2 箇所（コアの `positions_bounds` と Viewer の
`geometry_bounds`）に分かれている。片方だけ直すともう片方が静かにずれ続ける。

## 目標アーキテクチャ

### 測る関数は 1 つ。コアに置く

`ravel_core::geometry::ops` に、**そのジオメトリが描くものすべて**の AABB を
返す関数を 1 つ置く。

```rust
/// Axis-aligned bounds of everything a geometry *draws*: point positions,
/// each instance's source placed through its own `InstanceTransform`, and
/// the reach of the stroke that covers them.
pub fn drawn_bounds(geometry: &Geometry) -> Option<Rect>
```

- `GeometricData::bounds()`（`container.rs:848`）はこれに委譲する
- Viewer の `geometry_bounds`（`viewer/geometry.rs:94`）もこれに委譲し、
  `CompRect` へ移すだけの薄い関数になる
- `positions_bounds` は「Point 域の位置の範囲」という部品として残す
  （`drawn_bounds` がその一部として呼ぶ）

**名前に `extent` を使わない。** `layer-content-size-plan.md` の決定 2 が
ラスタの範囲（RoD）の語として `extent` を空けている。`drawn_bounds` は
「描かれるもの」を指すので、ストロークの張り出しを含むことと整合する。

### インスタンスは source を再帰で測り、`InstanceTransform` で置く

配置は `InstanceTransform`（`container.rs:346`）が「scale → rot → translate の
唯一の定義」なので、bbox もそれを使う。自前で行列を書くと 3 つ目の正になる。

- 回転があるとき、source の AABB を**そのまま回さない**。4 隅を
  `InstanceTransform::apply` に通して、その AABB を取る
  （`text.layout` は rot を書かないが `scatter` は書く）
- `InstanceSource::Image` は `InstanceImage::rect()`（原点中心）を置く
- `InstanceSource::Geometry` は **その source の `drawn_bounds` を再帰で**取る。
  入れ子は `MAX_INSTANCE_DEPTH`（= 4）で打ち切る。`rasterize` が描くのをやめる
  深さと `expand_instances` が畳むのをやめる深さがこれなので、**測るのを
  やめる深さも同じ**でなければ「描かれているのに測られていない」が生まれる
- `source_index` 列の読み方は `ops::select_source` に既にある。再実装しない

### source は重複排除されているので、source ごとに 1 回測る

`text.layout` の `sources()` は重複排除されている（`"Ravel"` で 5 文字 5 source、
同じ文字が 2 回出れば source は 1 つ）。素朴に「インスタンスごとに source を
走査」すると同じ輪郭を何度も測る。

**source index → その source のローカル `Rect`** を 1 度だけ計算して持ち、
インスタンスごとには 4 隅を置くだけにする。`O(sources × points + instances)`。

これは性能の都合ではなく**約束の維持**で、`viewer/geometry.rs` の doc comment が
`geometry_bounds` の実測コスト（100 万点で 197 µs、ポインタ移動ごとに支払う）を
明記しており、これを崩さない。

### ストロークの張り出しはラスタライザと同じ関数で

`stroke_margin(width, join)`（`rasterize/mod.rs:143`）が「線がパスからどこまで
届くか」を、マイタースパイクが `miter_limit` 半幅まで伸びることまで含めて
持っている。bbox が自前で計算し直すと 2 つ目の答えになる。

`stroke_margin` は `zeno::Join` を取るが、コアは zeno に依存しない。**判定に
必要なのは「マイターか否か」だけ**なので、コアに

```rust
pub fn stroke_reach(width: f32, miter: bool) -> f32
```

を置き、`rasterize` が `self.shape.join == Join::Miter` を渡して呼ぶ。
`ZENO_MITER_LIMIT`（= 4.0）もコアへ移す。**本番依存は 1 つも増えない。**

`stroke_width` は要素ごとの属性（Detail / Primitive / Point 域、
`names::STROKE_WIDTH`）なので、bbox は**見つかった最大値**で全体を膨らませる。
要素ごとに正確に測るより広く出るが、**足りなくなることはない**
（`LOW-APP-33` の症状は「小さすぎる」）。`ponytail:` ではなく計画上の決定として
残す — 要素ごとに必要になったら `drawn_bounds` の走査の中で分ければよい。

### `bounds()` の本番消費者は今いない

`GeometricData::bounds()` を `Geometry` で呼んでいるのは**テストだけ**
（`shape/mod.rs:427`、`eval_hooks.rs:617,636`、`container.rs:1246,1481`）。
本番で「範囲」を読むのは Viewer だけなので、`bounds()` の意味が広がっても
描画も評価も動かない。だから 1 つの関数にストロークまで含めてよい。

将来 `auto` サイズ（`EXT-1`）や RoD が「インクだけの範囲」を要るようになったら
そのとき分ける。**先に 2 つ用意しない。**

## 実装単位

### 単位 1（`BBOX-1`）: コアの `drawn_bounds` — インスタンスの source を測る

`ravel-core` のみ。

- `ops::drawn_bounds` を追加。Point 域の位置 ∪ 各インスタンスの source を
  `InstanceTransform` で置いたもの
- source ごとのローカル `Rect` を 1 度だけ計算するキャッシュ
- 入れ子は `MAX_INSTANCE_DEPTH` で打ち切る
- 回転は 4 隅を通す
- `GeometricData::bounds()` を `drawn_bounds` に委譲
- `positions_bounds` は Point 域の部品として残す

**完了条件**

- インスタンスしか持たないジオメトリの `bounds()` が 0×0 でなくなる
- 画像 1 インスタンスのジオメトリの `bounds()` が、その画像の `rect()` を
  インスタンス位置へ動かした矩形に一致する
- 45° 回した source を置いたインスタンスの bbox が、**回した矩形の外接矩形**
  （source の矩形を回しただけのものより広い）になる
- `MAX_INSTANCE_DEPTH` を超える入れ子で、`expand_instances` が畳む範囲と
  `drawn_bounds` が測る範囲が一致する
- 100 万点のジオメトリ 1 つに対する `drawn_bounds` が、同じ点数の
  `positions_bounds` と同じ桁のコストに収まる

### 単位 2（`BBOX-2`）: ストロークの張り出しを共有する

`ravel-core` + `ravel-nodes`。

- `stroke_reach(width, miter)` と `ZENO_MITER_LIMIT` をコアへ
- `rasterize::stroke_margin` を撤去し、呼び出しを `stroke_reach` へ
  差し替える（`rasterize/mod.rs:1316`）
- `drawn_bounds` が `names::STROKE_WIDTH` の最大値と `names::JOIN` を読み、
  `stroke_reach` の分だけ膨らませる

**完了条件**

- `stroke_width` を 0 から 40 に上げたシェイプの `bounds()` が、
  ちょうど `stroke_reach(40, miter)` だけ広がる
- `join = miter` の bbox が `join = round` の bbox より広い
- ラスタライザが描くピクセルが `bounds()` の外に出ない
  （太い線のシェイプを実際にラスタライズして、非ゼロなカバレッジが
  bbox の内側にあることを確かめる）
- `rasterize` の既存ゴールデンが無改変で通る

### 単位 3（`BBOX-3`）: Viewer が委譲する

`ravel-app` のみ。

- `geometry_bounds` を `drawn_bounds` の呼び出し + `CompRect` への変換にする
- doc comment を「2 つの域の位置の union」から「コアが測るものを移すだけ」に
  書き換える。実測コストの表は `drawn_bounds` 側へ移す
- `MED-APP-45` / `MED-CORE-11` / `LOW-APP-33` を `issues/closed/` へ
- `layer-content-size-plan.md` の「問題 2」に解決済みを追記

**完了条件**

- `text.font` → `text.layout` の bbox が、**同じ文字列を
  `text.to_path` に通した bbox と一致する**（数値を書かない関係で固定する。
  ノードを 1 つ挟むだけで bbox が変わってはいけない）
- `geometry.from_image` を置いたレイヤーが Viewer で掴める
  （`layer_comp_rect` が 0 幅 0 高さを返さない）
- コアの `bounds()` と Viewer の `geometry_bounds` が、
  点だけ / インスタンスだけ / 両方 / 空 の 4 つで同じ矩形を答える
- Viewer の既存 bbox テストが無改変で通る

## 範囲外

- **`ops::bounds_center` を `drawn_bounds` に寄せること。** これは
  「範囲」ではなく**ピボット**で、`scatter`（`scatter/mod.rs:71`）と
  `field`（`field/mod.rs:64`）と `geometry`（`geometry.rs:92`）が
  中心として読んでいる。インクを含めると散布の中心が動く
  ＝ **ユーザーに見える挙動の変更**になるので、この計画では触らない
- **要素ごとの正確なストローク幅。** 最大値で膨らませる（上記）
- **ラスタの範囲（RoD）。** `layer-content-size-plan.md` の範囲外節がそのまま効く。
  `FrameBuffer` は原点を持たない
- **Media レイヤーの bbox。** `media` → `net.out` にジオメトリノードが無いので
  マニピュレータが出ないという別の話（RoD 側）
- **bbox ハンドルで内容を編集すること。** ハンドルが書くのは殻の `scale`
- **`shape.rect` の `sizing` / `auto`。** `EXT-1`〜`4`。独立に進む
  （こちらは「正しく測る」、あちらは「内容にサイズを持たせる」）

## 決定（2026-09-17）

1. **関数名は `drawn_bounds`。** `extent` は RoD の語として空けてある
   （`layer-content-size-plan.md` の決定 2）。`placed_bounds` も考えたが、
   ストロークの張り出しは「置かれたもの」ではなく「描かれるもの」なので
   `drawn` を採った
2. **Text の bbox はインク（グリフ輪郭）の範囲。** フォントメトリクスから
   組版上の箱（アセント / ディセント / 送り幅）を作る案は却下した —
   Text を特別扱いする分岐が要るのに対し、インクの範囲は一般の修正から
   そのまま落ちてくる。さらに `text.to_path` の後と**同じ値**になるので、
   完了条件が数値ではなく関係で書ける
3. **1 つの関数にストロークまで含める。** `bounds()` の本番消費者が
   今いないので分ける理由が無い。`auto` サイズか RoD がインクだけの範囲を
   要求したら、そのときに分ける
4. **Text の 4 つ目の症状は別起票しない。** 直す箇所が `MED-APP-45` と
   同一行で、`BBOX-3` の完了条件が固定する
