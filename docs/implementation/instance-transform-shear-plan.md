# インスタンス配置の厳密な合成計画（`MED-GPU-09`）

> **Status**: 計画 — 2026-09-30。`IXF-1`〜`IXF-4`。フェーズ A6 の残り
> （[`roadmap.md`](roadmap.md#フェーズ-a6-結果がどの前提で作られたかを持つこと)）

## 背景

インスタンスの配置は `ravel_core::geometry::InstanceTransform`
（`crates/ravel-core/src/geometry/container.rs:346`）が `offset` / `rot` / `scale`
の分解表現で持つ。入れ子は `InstanceTransform::compose(outer, inner)`（:401）で
畳むが、その式は

```
rot   = outer.rot + inner.rot
scale = outer.scale * inner.scale   （成分ごと）
```

で、正しい合成 `R_o S_o R_i S_i` を `R_o R_i S_o S_i` として扱っている。一致するのは
**外側のスケールが一様か、内側の回転が 0 のときだけ**。それ以外の真の合成は
せん断を含み、`rot` / `scale` の 2 つでは表せない
（[`MED-GPU-09`](../../issues/medium/gpu-nodes.md)。再現: 外側 `scale = (2, 1)`、
内側 `rot = 90°`、`p = (1, 0)` → 正 `(0, 1)`、現状 `(0, 2)`）。

2026-09-30 に main（`1f9e9ca2`）で影響範囲を実測した。個票が挙げていない
事実が 3 つある。

- **展開はすでに厳密で、描画と食い違っている。** `expand_instances` の
  `expand_at`（`crates/ravel-core/src/geometry/ops.rs:2293-2395`）は内側を先に
  点へ焼いてから外側を掛ける（:2353 → :2385）ので真の積になる。一方
  `rasterize`（CPU / GPU）、`drawn_bounds` の `instance_bounds`（ops.rs:1430-1439）、
  `instance_pieces` / `placed()`（ops.rs:2579、:2733-2760）は近似の `compose` を通る。
  `container.rs:399-400`、ops.rs:1164-1169・1372-1379 の「展開と描画は同じ絵」
  という doc は入れ子では誤り
- **`geometry.transform` にも同じ近似がある。** `apply_transform`
  （`crates/ravel-nodes/src/geometry.rs:188-224`）は instance ドメインに
  `rot += rotation`、`scale *= scale` を直接書く。`compose` を通らない 2 つ目の
  近似で、**組み込みノードだけで踏める**（`text.on_path` が `rot` を書いた
  インスタンスに非一様な `geometry.transform` を掛ける）。レイヤーの
  トランスフォームセクション（`transform_section.rs:15,66`）も同じ関数を通る。
  `container.rs:395-398` の「組み込みノードはこの組み合わせを作らない」は外れている
- **最上位の配置も非一様になりうる。** `Placement::for_context`
  （`crates/ravel-nodes/src/rasterize/mod.rs:168-174`）のコンポ → キャンバスの
  スケールは軸ごとの比で、丸めで僅かに非一様になる（1920x1081 を Half で
  `(0.5, 0.4995…)`）。回転したインスタンスが常に微小にずれる

GPU で配置を使うのは**画像インスタンスだけ**。パスとスプライトは CPU で
`placement.apply` してから upload する（rasterize/mod.rs:977-995）。画像は
`push_image_item`（:790-842）が `data0 = [2.0, off.x, off.y, rot]`、
`data1 = [sx, sy, half_w, half_h]` に詰め、WGSL `image_color`
（`crates/ravel-nodes/src/shaders/rasterize.wgsl:200-211`）が逆回転・逆スケールする。

geometry 自体は永続化されない（`ravel-project` に geometry の直列化は無い）。
永続化されるのは**ノードパラメータに書かれた属性名**（`field.apply` の対象
`"scale"` / `"rot"` など、`names.rs:124-126`）なので、`rot`（F32）と `scale`
（Vec2）の名前と型は変えられない。

## 目的

- 入れ子・`geometry.transform`・最上位の配置のすべてで、線形部分を**厳密に**
  合成する。描画・バウンズ・展開・ピースが同じ変換を使う
- `rot` / `scale` の意味と、それを書く既存ノード（`scatter`、`text.layout`、
  `text.on_path`、`field.apply` 経由の変調）はそのまま
- せん断が生じない合成は、今と同じ `rot` / `scale` を返す

## 目標アーキテクチャ

### 表現: 予約属性 `shear` を 1 つ足す（2026-09-30 決定）

任意の 2×2 は「回転 × 上三角」に分解できる（QR 分解）。これを
`rot` / `scale` / `shear` の 3 つで持つ。

```
L = R(rot) · S(scale) · H(shear)
  = R(rot) · [[sx, sx·shear], [0, sy]]
```

- **`shear` は instance ドメインの予約属性、F32、無ければ 0**。無い列を 0 と
  読むので、既存のジオメトリと既存の書き手は意味が変わらない
- `merge` の型ゼロ埋め（`concat_attribute_sets`、`geometry.rs:425-437`）で
  片側に列が無いとき 0 が入る。**0 は恒等なので、ゼロ埋めがそのまま正しい**
  （`scale` のゼロ埋めが `(0, 0)` でインスタンスを潰すのと対照的。そちらは
  別件、下記「非対象」）
- 選ばなかった案: 2×2 行列の予約属性（`rot` / `scale` と二重になり、どちらが
  効いているかが属性から読めない。行列の属性型も要る）／描画だけ厳密にする
  （`placed()` と `geometry.transform` が属性へ書き戻す所で近似が残る）

### 合成は行列で、分解は 1 箇所で

- `InstanceTransform` に `shear: f32` を足す。`apply` / `apply_vector` は
  `R S H` をそのまま掛ける
- `compose(outer, inner)` は線形部分を 2×2 の積で求め、**1 つの分解関数**で
  `rot` / `scale` / `shear` に戻す。分解の規約:
  - せん断が生じない合成（外側が一様、または内側の回転が 0、かつ両側の
    `shear` が 0）は**今の式と同じ値**を返す（許容 1e-6）。鏡映（負のスケール）の
    符号と `rot` の値域を今と変えないため、分解の符号は `outer.scale` と
    `inner.scale` の積の符号に合わせる
  - `sx` が 0 に潰れた退化（行列式 0 を含む）は `shear = 0` とし、何も描かれない
    ことを今と同じに保つ
- 逆変換（画像の標本化）は同じ行列の逆行列。ゼロスケールのガード
  （rasterize/mod.rs:797-803、:1455-1462）は**行列式のガード**になる
- 流用: 2×3 の `ravel_core::composition::transform::Affine`
  （`composition/transform.rs:29`）が積と逆行列を持つ。`InstanceTransform` の
  内部計算に使ってよいが、公開型は `InstanceTransform` のまま

### 列の読み書きを 1 つにする

instance ドメインから配置を読む箇所が 5 つある（ops.rs:1405、2305、2562、2721、
rasterize/mod.rs:1036 / 1378）。`shear` を足すと 5 箇所に同じ読み取りが増えるので、
`InstanceTransform` を列から組む関数を 1 つ置いて全部をそこへ寄せる。
書き戻し（`placed()`、`geometry.transform`）も同様。`shear` 列は
**値が 1 つでも非 0 か、列が既にある場合だけ書く**（全ジオメトリに 0 の列が
生えてスプレッドシートを汚さないため）。

### GPU: 画像インスタンスの逆行列の置き場

逆変換に要るのは offset 2 + 逆 2×2 の 4 + half size 2 = 8 float。
`data0.x` の種別を除くと `data0` / `data1` の空きは 7。**画像では `stroke_color` が
未使用**（rasterize/mod.rs:832-833）なので、逆 2×2 をそこへ入れ、`DrawItem` の
サイズは変えない。

- `data0 = [2.0, off.x, off.y, 0]`、`data1 = [0, 0, half_w, half_h]`、
  `stroke_color = [m00, m01, m10, m11]`（逆行列）
- `path-shading-plan.md`（`PSHADE-*`）は全 `DrawItem` に「逆 Placement 6 float」を
  足す計画を持つ。そちらが入った時点で画像も同じ欄へ移る。**この計画は
  `DrawItem` のレイアウトを広げない**ので、どちらが先でも衝突しない

## Phase 1: コアの配置（ravel-core）— `IXF-1`

### 主な対象

- `crates/ravel-core/src/geometry/container.rs`（`InstanceTransform`）
- `crates/ravel-core/src/geometry/names.rs`（予約名）
- `crates/ravel-core/src/geometry/ops.rs`（`instance_bounds`、`expand_at`、
  `instance_pieces`、`placed()`、`is_placement_attribute`）

### 作業

- `InstanceTransform::shear`、厳密な `apply` / `apply_vector` / `compose`、
  分解関数、列から組む関数
- 予約名 `names::SHEAR = "shear"` と綴りの固定テスト
  （`reserved_names_keep_their_spelling`）、`is_placement_attribute` への追加
- ops.rs の 4 つの読み取りを列から組む関数へ寄せ、`placed()` が `shear` を書く
- doc の「展開と描画は同じ絵」「組み込みは作らない」を事実に合わせる

### 完了条件

- 個票の再現（外側 `(2, 1)`、内側 90°、`p = (1, 0)`）で
  `compose(outer, inner).apply(p) == outer.apply(inner.apply(p))`
- ランダムな `rot` / `scale` / `shear` の組（鏡映を含む）で、`compose` 後の
  `apply` が 2 回の `apply` と一致する（許容 1e-5）
- せん断が生じない組で `compose` が今の式と同じ `rot` / `scale` を返し、
  `shear == 0`
- **近似を固定しているテストを厳密値へ書き換える**:
  `nesting_is_bounded_the_way_the_placements_compose`（ops.rs:3331、コメントに
  「厳密なら正方形」とある）
- せん断を含む入れ子で、`drawn_bounds` が `expand_instances` した点の
  バウンズを含む（展開と描画の一致をコアで固定する）
- `instance_pieces` のピースが、せん断を含む入れ子でも元の配置と同じ点を持つ

## Phase 2: 描画（ravel-nodes rasterize、CPU / GPU / WGSL）— `IXF-2`

### 主な対象

- `crates/ravel-nodes/src/rasterize/mod.rs`（`Placement`、`flatten_geometry`、
  `raster_instances`、`push_image_item`、`raster_image`）
- `crates/ravel-nodes/src/shaders/rasterize.wgsl`（`image_color`）

### 作業

- `Placement` を `InstanceTransform` への委譲だけにする（`shear` を含む）
- CPU / GPU の列の読み取りを `IXF-1` の関数へ寄せる
- 画像インスタンスの逆 2×2 を `stroke_color` へ詰め、WGSL を逆行列の積にする。
  CPU の `raster_image` も同じ逆行列
- ゼロスケールのガードを行列式のガードにする

### 完了条件

- CPU / GPU 一致のゴールデンに**画像インスタンスをせん断の入れ子**に置く
  ケースを足す（既存の `gpu_matches_cpu_for_paths_points_and_nested_instances`
  は葉がスプライトで近似が見えないため）
- せん断を含む入れ子のパスで、`rasterize` の画素と `expand_instances` →
  `rasterize` の画素が一致する（今は食い違う）
- 既存の CPU / GPU 一致ゴールデン（`RESP3-12` 以降、rasterize/mod.rs:3180、3287、
  3322、3448、3598）と `shape_layer_golden` / `per_instance_modulation_golden` /
  `text_to_path_golden` が**無改変で通る**。せん断の無いケースは同じ絵のはず。
  許容誤差を動かす必要が出たら、動かす前に止めて報告する

## Phase 3: `geometry.transform`（ravel-nodes）— `IXF-3`

### 主な対象

- `crates/ravel-nodes/src/geometry.rs`（`apply_transform`）

### 作業

- instance ドメインの `rot +=` / `scale *=` を、ノードの変換を外側とする
  `InstanceTransform::compose` に置き換え、`shear` も読み書きする

### 完了条件

- `text.on_path`（`rot` を持つインスタンス）→ 非一様な `geometry.transform` の
  描画が、`expand_instances` してから同じ変換を掛けた点と一致する
- 既存の `instances_compose_placement_rotation_and_scale`（geometry.rs:1365、
  一様スケール）と `instances_gain_missing_rot_and_scale_columns`（:1432、
  既存の `rot` 無し）が無改変で通る
- `transform_section.rs` の `rotating_a_text_layout_matches_a_geometry_transform_downstream`
  （:500）が無改変で通る

## Phase 4: 文書と UI の列順 — `IXF-4`

### 作業

- `docs/specifications/procedural-geometry.md` の予約属性表（:117-118）に `shear`、
  入れ子の説明（:92-104）を厳密な合成に
- `crates/ravel-ui/src/panels/attribute_spreadsheet.rs` の `STANDARD_ORDER`
  （:31-43）で `scale` の後に `shear`、`docs/specifications/ui/attribute-spreadsheet.md`
  （:33、:53）も
- `docs/agent-api-reference.md`（:1438-1482 の「NOT the exact affine」ほか）
- `MED-GPU-09` を `issues/closed/` へ、計画を `done/` へ、backlog / roadmap を更新

### 完了条件

- `mise run docs:check` が通る
- 「近似」「NOT the exact」を述べる doc が残っていない
  （`rg -n "not the exact|NOT the exact|近似" docs crates` で配置に関するものが 0）

## テスト方針

- すべてヘッドレス。GPU のテストは既存の CPU / GPU 一致テストと同じ扱い
  （アダプタが無ければ skip する既存の仕組みに乗る）
- 厳密さのテストは「2 回 `apply` した結果」を正とする。**`compose` の出力を
  期待値に写したテストにしない**（近似をそのまま固定した ops.rs:3331 の失敗の形）
- 各単位で、分解または積を元の式へ戻すと落ちることを確かめる（変異検査）

## 非対象

- `uniform_scale()`（`|sx| + |sy|` の平均）によるストローク幅・スプライト半径・
  miter の近似。`shear` は含めない。非一様スケール下の線幅は
  `path-shading-plan.md` が既に「別単位で、`PSHADE-3` に合流」としている
- `merge` で片側だけ `scale` を持つと他方が `(0, 0)` で潰れる既存の穴
  （`concat_attribute_sets`）。別の欠陥なので起票して別に直す
- `orient` / `scale3`（3D 配置、どこからも読まれていない）
- ユーザーが `shear` を直接作るノード（`geometry.transform` のせん断パラメータ等）。
  属性として書けば効くが、パラメータを増やす要求はまだ無い

## 実装単位

| ID | 単位 | 依存 |
|---|---|---|
| `IXF-1` | `InstanceTransform` の厳密な合成と予約属性 `shear`、ops.rs の読み書きの一本化（ravel-core） | — |
| `IXF-2` | rasterize の CPU / GPU / WGSL を厳密な配置へ。画像の逆行列を `stroke_color` に | `IXF-1` |
| `IXF-3` | `geometry.transform` の instance ドメインを `compose` 経由に | `IXF-1` |
| `IXF-4` | 文書・スプレッドシートの列順・個票と計画のクローズ | `IXF-1`〜`3` |

`IXF-2` と `IXF-3` は互いに独立（rasterize/mod.rs と geometry.rs で触るファイルが
別）で、`IXF-1` のマージ後に並行できる。

## 決定事項（2026-09-30、ユーザー判断）

- **せん断は予約属性 `shear`（F32、無ければ 0）で持つ**。2×2 行列の属性や
  描画だけの修正は採らない

## 実施状況

未着手。
