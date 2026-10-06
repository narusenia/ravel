# 不在属性の埋め値を 1 箇所にする計画（`MED-CORE-13`）

> **Status**: 完了 — 2026-10-05 計画、`FILL-1`〜`FILL-6` ✅（#591 と閉じる PR）。フェーズ A6「出力そのものの誤り」
> （[`roadmap.md`](../roadmap.md#フェーズ-a6-結果がどの前提で作られたかを持つこと完了)）。
> 決定事項 3 件は 2026-10-05 にユーザー判断で確定（下記）

## 背景

属性列を連結する処理は、片側に列が無いとき、その行を**列の型のゼロ**で埋める。
読み手は予約属性の列が**無い**ことを別の値として読む。両者が食い違うので、
マージや展開を通すと片側が黙って消える・透明になる
（[`MED-CORE-13`](../../../issues/closed/medium-core-evaluator.md)）。

2026-10-05 に main（`5f773c05`）で実測した。

### 列を埋める箇所

| # | 箇所 | 何を埋めるか | 今の値 |
|---|---|---|---|
| 1 | `crates/ravel-nodes/src/geometry.rs:397` `concat_attribute_sets` / `:437` `concat_columns`（`geometry.merge`、`:329-346` で Point / Primitive / Instance に使う） | 片側に無い列の行 | 型ゼロ |
| 2 | `crates/ravel-core/src/geometry/ops.rs:2759` `ColumnAccumulator` / `:2851` `append_rows`（`expand_instances` の `expand_at`、`:2352-2363`） | ブロック（ホスト自身 + インスタンスごとのソース）に無い列の行。途中で現れた列の前方埋めも | 型ゼロ |
| 3 | `ops.rs:2604` `attach_piece_attributes`（`scatter.*` の `piece_mode = instances`、`scatter/mod.rs:157`） | 列を持たないピースを打つインスタンスの行 | 型ゼロ（`append_rows` 経由） |
| 4 | `crates/ravel-core/src/geometry/field.rs:1840` `created_column`（`field.apply` の `create_if_missing`、`:1913`） | 対象列が無いときに作る列の全行 | `Cd` / `stroke_color` は白、`fill` は `false`、`stroke_width` は 0、他は型ゼロ |
| 5 | `ops.rs:106` `attribute_set_in_group` の `unset`（`style.fill` / `style.stroke`、`crates/ravel-nodes/src/style.rs:26-33` の `UNSET_*`） | group 外の行（列の型や長さが違うとき） | `rasterize` のテンプレート既定（`fill = true`、`stroke_width = 0`、色は白） |

意味どおりの値で埋めていて直す対象でないもの: `ops.rs:525` `tangent_column`
（`in_tan` / `out_tan` が無ければ 0 = 角。読みと一致）、`expand_at` が
インスタンス列を各ブロックへ配る `select_values(… repeat_n(index, count))`
（`:2790`。値の複製で、欠けの埋めではない）。

### 予約属性が「無い」ときの読み

| 名前 | 型 | ドメイン | 無いときの読み | 読む場所 | 型ゼロとの差 |
|---|---|---|---|---|---|
| `P` | Vec2/Vec3 | Point/Instance | 無い（要素があれば `validate` が拒否、`container.rs:682-690`） | — | 対象外 |
| `index` | I32 | Point/Instance | 無い（`sort` / 展開が振り直す、`ops.rs:880`、`:2394`） | — | 対象外（下記「非対象」） |
| `source_index` | I32 | Instance | 0（`ops.rs:2437`、`rasterize/mod.rs:1760`） | 同左 | 一致 |
| `id` | I32 | Point/Instance | 読み手なし | — | 対象外 |
| `rot` | F32 | Instance | 0（`container.rs:583`） | `InstanceColumns::placement` | 一致 |
| `scale` | Vec2 | Instance | `(1, 1)`（`container.rs:587`） | 同上 | **差あり**（`(0, 0)` で潰れる） |
| `shear` | F32 | Instance | 0（`container.rs:588`） | 同上 | 一致 |
| `orient` / `scale3` / `N` | Vec4/Vec3 | — | 読み手なし（3D は未配線） | — | 対象外 |
| `Cd` | Color | Instance | 白（掛け算の色味、`rasterize/mod.rs:1282-1287`、`:1719-1724`） | `raster_instances` / `flatten_*` | **差あり**（透明で消える） |
| `Cd` | Color | Primitive | `rasterize` の `color`（ピン > パラメータ。`element_color`、`:2052`、`base_color` `:1944`） | `element_colors` | **差あり**（透明）。**定数でない** |
| `Cd` | Color | Point（スプライト） | 同上（`:1238-1241`、`:1909-1912`） | スプライト描画 | **差あり**。定数でない |
| `Cd` | Color | Point（パスの頂点） | 列ごと無ければ頂点色を使わず、プリミティブの線色（`vertex_stroke_colors`、`:842-865`） | 線の頂点色 | **差あり**（透明な頂点色）。行ごとの値は所属プリミティブで決まる |
| `alpha` | F32 | Point/Primitive/Instance | 1.0（`element_alpha` `:2056`、`:859-860`） | 全描画 | **差あり**（透明） |
| `pscale` | F32 | Point | 2.0（`DEFAULT_POINT_RADIUS`、`:96`、`:1234`、`:1905`） | スプライト | **差あり**（半径 0 で描かれない） |
| `fill` | Bool | Primitive/Instance | 継承（`rasterize` の `fill`、既定 `true`、または囲むインスタンス。`element_style` `:2063-2072`） | `element_style` | **差あり**（`false`）。**定数でない** |
| `stroke_width` | F32 | Primitive/Instance | 継承（`rasterize` の `stroke_width`、既定 0） | 同上。バウンズは 0 と読む（`ops.rs:1278-1290`、`:1417`） | 既定値なら一致、パラメータを上げると差。定数でない |
| `stroke_color` | Color | Primitive/Instance | 継承（囲むインスタンスの `stroke_color`）、無ければ自分の塗り色（`element_colors` `:2077-2083`） | 同上 | **差あり**（透明な線）。**他の属性へのフォールバック** |
| `stroke_color` | Color | Point | Point の `Cd` → プリミティブの連鎖（`:848-853`） | 線の頂点色 | **差あり** |
| `stroke_align` | I32 | Primitive | 0 = 中央（`sample.rs:74-79`） | 同左 | 一致 |
| `dash` / `dash_offset` / `cap` / `join` / `anchor` | — | Detail | — | — | 対象外（Detail は連結しない。A の勝ち） |
| `in_tan` / `out_tan` | Vec2 | Point | 0 = 角 | パス平坦化 | 一致 |
| `u` / `age` / `life` / `velocity` | — | Point | 読み手は `field.attribute` の `default` パラメータ（既定 0、`field.rs:1080`） | — | 一致（対象外） |
| `char_index` ほか文字属性 | I32/F32 | Instance | 同上。`advance` が無いと `text.on_path` はエラー（`text.rs:284-291`） | — | 対象外 |

読み手同士の食い違い:

- `field.apply` の `created_column` は `Cd` / `stroke_color` を白で作る。
  `rasterize` が「無い `stroke_color`」を読むのは白ではなく**自分の塗り色**、
  「無い Primitive の `Cd`」は**`color` パラメータ**
- `created_column` は `fill` を `false` で作り、`style.rs` の `UNSET_FILL` は
  `true`。`Bool` は変調できず `combine` がエラーにするので今は表に出ない
- `alpha` / `scale` / `pscale` は `created_column` でも型ゼロで、`combine = multiply`
  の結果が 0 になる（個票の再現 5）

「無いときの値」の正はどこにも無い。`rasterize` の `unwrap_or` と定数、
`InstanceColumns` の恒等配置、`style.rs` の `UNSET_*`、`created_column` の
`match` がそれぞれ自前で持つ。

### 固定されているテスト

- `sources_with_different_columns_fill_with_typed_zeros`（`ops.rs:5584`）は
  `pscale = [0.0, 0.0, 8.0, 0.0, 0.0]` を期待する。`pscale` を持たない点は
  パスの頂点で、`rasterize` は頂点をスプライトにしない（`path_vertex_mask`）ので
  この入力では絵に出ないが、**規則として誤り**（パスに属さない点なら半径 2.0 が
  消える）
- `merge_unions_attributes_with_typed_zero_fill`（`geometry.rs:1821`）は
  `pscale` の `[.., 0.0, 0.0]` と、`Vec3` 型の `Cd` のゼロ埋めを期待する。
  前者は上と同じ。後者は**予約名に予約外の型**が載った列で、型ゼロのままが正しい
- `merge_fills_primitive_attrs_for_the_attributeless_side`（`:1892`）は予約外の
  `mat` で、型ゼロのままが正しい
- `a_piece_without_a_column_fills_with_the_typed_zero`（`scatter/mod.rs:1488`）は
  予約外の値（`char_index`）で、型ゼロのままが正しい。テスト名と doc の
  「型ゼロで埋める」という規則の説明だけ直す

### 永続化

`Geometry` は `Serialize` を持たず、`ravel-project` にジオメトリの直列化は無い。
永続化されるのはノードのパラメータ（`field.apply` の対象名など）だけなので、
**フォーマット移行は要らない**。変わるのは評価結果だけ。

## 目的

- 予約属性について「列が無いときの値」の正を ravel-core に 1 つ置く
- 列を埋める 5 箇所と、`rasterize` の読みがその正を使う。マージ・展開・
  ピース・作成を通っても、欠けていた側は**欠けていたときと同じに描かれる**
  （定数で表せる属性は厳密に。継承する属性は決定事項 1 の範囲で）
- 予約されていない列（ユーザー属性）と、予約名に予約外の型が載った列は
  型ゼロのまま

## 目標アーキテクチャ

### 不在値の正: `geometry::absent`

ravel-core に `geometry/absent.rs` を置く。予約名の綴りは `names.rs`、
「無いときにどう読まれるか」はこのモジュールが持つ。

```rust
/// What a reserved attribute reads as on `domain` when its column is absent.
pub enum Absent {
    /// A constant: `alpha` = 1, `scale` = (1, 1), `pscale` = 2, Instance `Cd` = white, …
    Value(AttributeValue),
    /// Inherited from the drawing context (`rasterize` parameters, an enclosing
    /// instance): Primitive/Point `Cd`, `fill`, `stroke_width`, `stroke_color`.
    Inherited,
}

pub fn absent(domain: Domain, name: &str) -> Option<Absent>; // None = not reserved

/// The column `geometry` would resolve `name` to on `domain` if it carried one,
/// `len` rows long. Typed zero for an unreserved name or a type mismatch.
pub fn absent_column(geometry: &Geometry, domain: Domain, name: &str,
                     attr_type: AttributeType, len: usize) -> AttributeArray;
```

- 定数は `names.rs` の隣に名前付きで置く（`DEFAULT_PSCALE = 2.0` など）。
  `rasterize` の `DEFAULT_POINT_RADIUS` と `unwrap_or(1.0)`、インスタンスの
  色味の白、`style.rs` の `UNSET_*` はこれを参照する
- `AttributeValue`（`ops.rs:20`）はそのまま使う。型ゼロは
  `AttributeValue::zero(AttributeType)` として同じモジュールに 1 つ置き、
  `concat_columns` / `append_rows` / `created_column` の 3 つの `match` を畳む
- **型の照合**: 列の型が予約型と違う（`Cd` が `Vec3` など）なら予約の意味を
  当てず型ゼロ。`rasterize` も予約型以外の列は読まない（`as_color` が失敗して
  `None`）ので、読みと一致する
- `absent_column` が `&Geometry` を取るのは、**行ごとの値が同じジオメトリの
  他の列で決まる**属性があるため（下記）。連結の 3 箇所はどれも欠けた側の
  ジオメトリ（`merge` の各入力、`expand_at` の各ブロック）を手元に持つ。
  `attach_piece_attributes` のピースは Instance 行の `AttributeSet` なので、
  その場合は `Absent::Value` だけを使う関数を呼ぶ。**ただし Instance の
  `stroke_color` は例外**で、欠けた行は同じ出力 Instance 行の `Cd`（それも
  無ければ Instance の不在値の白）から解決する。既にある出力列は上書きしない
  （`FILL-2` の完了条件に含める）

### 継承する属性（決定事項 1・2）

Primitive / Point の `Cd`、`fill`、`stroke_width`、`stroke_color` の「無い」は
**定数ではなく、描画の文脈から継承する**という意味。マージの時点では
`rasterize` のパラメータも、囲むインスタンスも分からない。密な列は
「意見なし」を持てない（`style.rs:45-52` が group の種まきを拒否しているのと同じ理由）。

決定事項 1 により **`rasterize` テンプレートの既定で実体化する**:

| 属性 | 埋める値 |
|---|---|
| `fill` | `true`（テンプレートの `fill`） |
| `stroke_width` | `0.0`（テンプレートの `stroke_width`） |
| Primitive の `Cd` | 白（テンプレートの `color`） |
| Primitive / Instance の `stroke_color` | **その行の塗り色**（同じ側の `Cd`。`Cd` も無ければ上の白） |
| Point の `Cd` / `stroke_color` | パスの頂点なら**所属プリミティブの線色**（Primitive `stroke_color` > Primitive `Cd` > 白）、それ以外の点は白 |

`style.rs` の `unset` が既に同じ実体化をしている（`attribute_set_in_group`）ので、
新しい方針ではなく既存の方針を連結にも当てる形になる。残る誤差は
「`rasterize` のパラメータを既定から変えた」「`color` ピンを繋いだ」
「囲むインスタンスが `fill` / `stroke_*` を持つ」ときで、そのとき埋めた側は
テンプレート既定で描かれる。今の「消える」よりは常に近い。

`stroke_color` の行ごとの値は、**連結より前に**欠けた側の `Cd`（それ自体が
欠けていれば埋めた値）から作る。実体化した `stroke_color` は下流で `Cd` を
変調しても追随しない（「無い」なら追随した）。これも (a) の誤差に含める。

### 読み手

`rasterize` の `element_alpha` / `element_color` の既定、`DEFAULT_POINT_RADIUS`、
インスタンスの色味の白、`detail_cap` / `detail_join` / `StrokeAlign` の既定は
`absent` の定数を参照する。**挙動は変えない**（値の出所を 1 つにするだけ）。
`InstanceColumns::placement` の恒等配置は `InstanceTransform::IDENTITY` の
まま（既に 1 箇所。`absent` は `scale` の値をそこから取る）。

## Phase 1: 不在値の正（ravel-core）— `FILL-1`

### 主な対象

- `crates/ravel-core/src/geometry/absent.rs`（新規）、`geometry/mod.rs`
- `crates/ravel-core/src/geometry/names.rs`（定数の doc から参照）

### 作業

- `Absent`、`absent(domain, name)`、`absent_column`、`AttributeValue::zero`
- 表の全予約名に対する `absent` の答え。決定事項 1・2 のとおりに実装する

### 完了条件

- 予約名ごとの期待値テスト。**期待値は上の「無いときの読み」表（読み手の
  コード）から書き、`absent` の出力を写さない**: `alpha` = 1、Instance `scale` = `(1, 1)`、
  `pscale` = 2、Instance `Cd` = 白、`rot` / `shear` / `stroke_align` /
  `source_index` / `in_tan` / `out_tan` = 0
- 全予約名を `names.rs` の綴り固定テストと同じ一覧で走査し、`absent` が
  「対象外」以外の名前に答えを持つことを固定する（予約名を足して `absent` を
  忘れたら落ちる）
- `stroke_color` の行ごとの値: `Cd = [赤, 青]` の 2 プリミティブで `[赤, 青]`、
  `Cd` が無ければ白 2 行
- Point の `Cd`: 頂点 0..2 が Primitive `stroke_color = 緑` のパス、頂点 2..4 が
  Primitive `Cd` だけ赤のパス、パスに属さない点 1 つ → `[緑, 緑, 赤, 赤, 白]`
- 予約名に予約外の型（`Cd` が `Vec3`）は型ゼロ

## Phase 2: 連結の 3 箇所 — `FILL-2`（ravel-core）・`FILL-3`（ravel-nodes）

### 主な対象

- `FILL-2`: `crates/ravel-core/src/geometry/ops.rs`（`ColumnAccumulator`、
  `append_rows`、`attach_piece_attributes`）
- `FILL-3`: `crates/ravel-nodes/src/geometry.rs`（`concat_attribute_sets`、
  `concat_columns`）

### 作業

- 欠けた行を `absent_column` で埋める。`ColumnAccumulator` は途中で現れた列の
  前方埋めも、ブロックごとに `absent_column(block, …)` で作る
- `stroke_color` は `Cd` の埋めより後に解決する（同じ側の `Cd` を読むため）
- 型ゼロを規則として述べる doc（`ops.rs:2752-2757`、`:2844-2847`、
  `:2601-2602`、`geometry.rs:395-396`、`:436`）を新しい規則に

### 完了条件

すべて `rasterize` の画素か、読み手の関数（`InstanceColumns::placement`、
`element_alpha` 相当）で確かめる。

- `FILL-3`: `geometry.from_image` の出力と、それに `geometry.transform`
  （`scale = 2`）を掛けたものを `geometry.merge` → 両方の画像が描かれ、
  未変換側の `scale` は `(1, 1)`（個票の再現 2）
- `FILL-3`: `style.fill`（赤）のパスと素のパスの `merge` → 素のパスが
  `rasterize` の既定色（白）で塗られる（個票の再現 1。決定事項 1 の期待値）
- `FILL-3`: Point に `Cd` を持つパスと素のパス（Primitive `Cd = 青`）の `merge` →
  素のパスの線が青（個票の再現 3）
- `FILL-3`: 片側だけ `alpha = 0.5` → 他方の `alpha` は 1.0
- `FILL-2`: ホストの図形 + `alpha` / `Cd` を持つ文字インスタンスの
  `expand_instances` → ホストの図形の `alpha` は 1.0、Primitive `Cd` は白
  （個票の再現 4）
- `FILL-2`: 2 ソースの片方だけ `pscale` を持ち、もう片方は**パスに属さない点** →
  展開後の `pscale` は `[2.0, …, 8.0, …]` で、`rasterize` すると両方の点が描かれる
- `FILL-2`: `attach_piece_attributes` で、片方のピースだけ Instance `stroke_color`
  を持ち、他方は `Cd = 赤` だけを持つ → 欠けた行の `stroke_color` は赤。`Cd` も
  無い行は白。既にある出力列の値は変わらない
- **固定テストの書き換え**: `sources_with_different_columns_fill_with_typed_zeros`
  は `pscale` の欠けを 2.0 に、`merge_unions_attributes_with_typed_zero_fill` は
  `pscale` の欠けを 2.0 に（`Vec3` の `Cd` は型ゼロのまま）。テスト名も規則に
  合わせて改名する。`merge_fills_primitive_attrs_for_the_attributeless_side` と
  `a_piece_without_a_column_fills_with_the_typed_zero` は値を変えない
  （予約外。後者は名前と doc だけ）

## Phase 3: 作成と読み — `FILL-4`・`FILL-5`

### 主な対象

- `FILL-4`: `crates/ravel-core/src/geometry/field.rs`（`created_column`）、
  `crates/ravel-nodes/src/style.rs`（`UNSET_*`）
- `FILL-5`: `crates/ravel-nodes/src/rasterize/mod.rs`、`rasterize/sample.rs`

### 作業

- `FILL-4`: `created_column` を `absent_column` に置き換える（決定事項 3）。
  `style.rs` の `UNSET_*` を `absent` の定数へ
- `FILL-5`: `rasterize` の既定値の直書きを `absent` の定数へ。挙動は変えない

### 完了条件

- `FILL-4`: 列の無い `alpha` に `field.apply`（`combine = multiply`、定数 0.5）
  → `alpha` は 0.5（今は 0）。`scale` に `multiply`（`(2, 2)`）→ `(2, 2)`
  （今は `(0, 0)`）
- `FILL-4`: 既存の `a_missing_target_is_created_before_it_is_modulated` など
  `Cd` の作成テストが無改変で通る（パスに属さない点・Primitive・Instance の
  `Cd` は白のまま）
- `FILL-5`: `rasterize` の既存テストと CPU / GPU 一致ゴールデンが**無改変で**通る
- `FILL-5`: `DEFAULT_POINT_RADIUS` と `unwrap_or(1.0)` の直書きが
  `rasterize/mod.rs` に残らない（`rg` で 0）

## Phase 4: 文書 — `FILL-6`

### 作業

- `docs/specifications/procedural-geometry.md` の予約属性表に「無いとき」列を足し、
  `geometry.merge` と展開の「欠けた列は不在値で埋める。予約外は型ゼロ」を書く
- `docs/agent-api-reference.md` に `geometry::absent`
- `MED-CORE-13` を `issues/closed/` へ、計画を `done/` へ、backlog / roadmap を更新

### 完了条件

- `mise run docs:check` が通る
- 「typed zero」「型ゼロ」を連結の規則として述べる doc が予約属性について残っていない

## テスト方針

- すべてヘッドレス。GPU のテストは既存の CPU / GPU 一致テストと同じ扱い
  （アダプタが無ければ skip する既存の仕組みに乗る）
- **期待値は読み手の意味から書く**。`absent` / `absent_column` の出力を期待値へ
  写さない — 今のテストが型ゼロを写して欠陥を固定した形をもう一度作らない
- 連結の完了条件は列の値だけでなく `rasterize` の画素でも確かめる
  （`pscale` の例のように、列が誤っていても絵に出ない入力がある）
- 変異検査: 各単位で `absent_column` の呼び出しを型ゼロへ戻すと完了条件の
  テストが落ちること、`FILL-1` で `alpha` / `scale` / `pscale` の定数を 1 つずつ
  0 にすると落ちることを確かめる

## 非対象

- `index` の振り直し。`geometry.merge` は両側の `index` をそのまま連結する
  （片側が欠ければ 0）が、`sort` / 展開は振り直す。「無いときの値」ではなく
  「連結後の生成順」の問題なので別に扱う
- `field.attribute` の `default` パラメータ（既定 0）。読み手がユーザーの指定で
  既定を持つ形で、予約属性の不在値に揃えるかは別の判断
- 列ごとの「意見なし」を持つ表現（決定事項 1 で選ばなかった案）。採るなら別計画
- `orient` / `scale3` / `N`（3D。読み手が無い）。3D の配置が配線された時点で
  `absent` に足す
- `drawn_bounds` の `stroke_width`。無い列を 0 と読むのは既に
  `LOW-APP-33` の残余（基底パラメータが見えない）として記録済み

## 実装単位

| ID | 単位 | 依存 | 規模の目安 |
|---|---|---|---|
| `FILL-1` | `geometry::absent`（不在値の正、型ゼロ、行ごとの解決）と予約名の走査テスト（ravel-core） | — | +300（半分はテスト） |
| `FILL-2` | `expand_instances` の `ColumnAccumulator` と `attach_piece_attributes` を不在値で埋める。固定テストの書き換え（ravel-core） | `FILL-1` | +150 / −40 |
| `FILL-3` | `geometry.merge` の連結を不在値で埋める。固定テストの書き換え（ravel-nodes） | `FILL-1` | +180 / −50 |
| `FILL-4` | `field.apply` の `created_column` と `style` の `UNSET_*` を `absent` へ | `FILL-1` | +80 / −40 |
| `FILL-5` | `rasterize` の既定値の直書きを `absent` の定数へ（挙動不変） | `FILL-1` | +30 / −25 |
| `FILL-6` | 仕様・API 地図・個票と計画のクローズ | `FILL-1`〜`5` | 文書のみ |

`FILL-2`〜`FILL-5` は互いに独立（触るファイルが別）で、`FILL-1` のマージ後に
並行できる。

## 決定事項（2026-10-05、ユーザー判断）

1. **継承する属性（Primitive / Point の `Cd`、`fill`、`stroke_width`、
   `stroke_color`）の欠けは `rasterize` テンプレートの既定で実体化する**
   （`fill = true`、`stroke_width = 0`、色は白）。`style` の `unset` と同じ方針。
   パラメータを既定から変えた・`color` ピンを繋いだ・囲むインスタンスが継承元の
   ときだけずれる。選ばなかった案: 列に行ごとの「有無」のマスクを持たせる
   （行を並べ替える・選ぶ全 op がマスクを運ぶ必要があり別計画の規模。要求が
   出たら別計画で）／片側にしか無ければエラーにする（`style` 済みと素の
   ジオメトリのマージが通らなくなる）
2. **「無い」が他の属性へのフォールバックを意味する列（`stroke_color` → `Cd`、
   Point の `Cd` → 所属プリミティブの線色）は、埋める時点で同じ側の参照先の値を
   行ごとに写す**（「継承する属性」の表）。既知の限界: 写した後は下流の `Cd` の
   変調に追随しない（「無い」なら追随した）。この差は `FILL-6` で仕様に書く。
   選ばなかった案: 定数（白）で埋める（`Cd` が白でないジオメトリの線色が変わる）
3. **`field.apply` の `create_if_missing` も同じ不在値で列を作る**。列の無い
   `alpha` / `scale` / `pscale` への `multiply` や `amount < 1` の結果は
   0 起点から 1 / `(1, 1)` / 2 起点に変わり、既存プロジェクトの出力が変わる
   （ジオメトリは保存されないのでデータは壊れない）。Point の `Cd` を頂点に
   作るときも白から所属プリミティブの線色に変わる

## 実施状況

- 2026-10-05 計画。`MED-CORE-13` 起票。同日、決定事項 1〜3 をユーザー判断で確定
- `FILL-1` ✅ #591。`geometry::absent`（`Absent` / `absent` / `absent_value` / `absent_column` / `AttributeValue::zero`）と、予約名の一覧 `names::ALL` を走査するテスト。定数（`DEFAULT_ALPHA` ほか）は `absent.rs` に置いた。Point の `Cd` の例は、プリミティブの列が密なので 1 つのジオメトリに「`stroke_color` だけ」と「`Cd` だけ」のパスを同居させられず、`stroke_color` が `Cd` に勝つ形と `Cd` だけの形の 2 つに分けた
- `FILL-2` ✅ #591。`ColumnAccumulator` はブロックの行を溜めず、ブロックごとに追記する。後から現れた列の前方埋めに要るのはブロックのジオメトリ（借用）・行数・実効の `Cd` だけなので、それだけを覚える。`attach_piece_attributes` は `stroke_color` を最後に処理し、同じ出力行の `Cd` から解決する
- `FILL-3` ✅ #591。`geometry.merge` は各側のジオメトリから `absent_column` で埋める。画素で確かめたのは `scale`（画像）、`style.fill` 済みと素のパス、Point の `Cd`
- `FILL-4` ✅ #591。`created_column` は `absent_column` で作る。型は、`Cd` / `stroke_color` / `stroke_width` / `fill` は宣言の型、不在値が float・ベクタ・色の予約名（`alpha` / `scale` / `pscale` ほか）はその型、**I32 / Bool の予約名（`source_index` / `stroke_align`）はスカラーフィールドの型のまま型ゼロ**（宣言の型にすると、成功していたグラフが数値合成のエラーになるため）。`style` の `UNSET_*` は `absent::DEFAULT_*` になった
- `FILL-5` ✅ #591。`rasterize` の `DEFAULT_POINT_RADIUS` / `unwrap_or(1.0)` / 白の直書きを `absent` の定数へ。`StrokeAlign` と Detail の `cap` / `join` の既定は Detail・Primitive の名前で `absent` に答えが無く、すでに `names::` の定数を読んでいるので触らなかった
- `FILL-6` ✅。`procedural-geometry.md` の予約属性表に「無いとき」列と「欠けた列の埋め方」節、`agent-api-reference.md`、`MED-CORE-13` の個票（`issues/closed/`）、この計画の `done/` への移動
