# 塗り・線のスタイル属性化 実装計画

> **Status**: Planned — 2026-07-27

対象: `rasterize` に焼き込まれている fill / stroke をジオメトリ属性へ降ろし、
要素ごとに設定・変調できるようにする。関連要件: REQ-CORE-010、
REQ-MOGRAPH-001、REQ-MOGRAPH-005。

## 問題

`rasterize` は `Style { fill: bool, stroke_width: f32 }` を**ノード
パラメータ**から組み立てる（`rasterize/mod.rs:44,132`）。色は `Cd` 属性。

他の見た目要素（`Cd` / `alpha` / `pscale`）がすべて属性なのに対し、
fill と stroke だけがノードパラメータという不整合があり、帰結が 3 つある。

### 1. 要素ごとに変えられない

`scatter.grid` で 500 個並べたら全部同じ線幅になる。「グリッドの中心ほど
線が太い」「文字ごとに線幅が違う」が表現できない。

### 2. 塗りと線で色を分けられない

CPU 経路は `blend_coverage(pixels, &fill_cov, color)` と
`blend_coverage(pixels, &stroke_cov, color)` に**同じ `color`** を渡している
（`rasterize/mod.rs:678-695`）。赤い塗りに青い線という最も基本的な
ベクター表現ができない。

### 3. 変調システムから外れている

`field.apply` は属性にしか作用しないため、フィールドで線幅を駆動できない。
`per-instance-modulation-plan.md` が用意する変調層の資産が、見た目の
パラメータには一切効かない。

`procedural-geometry.md` の設計原則 1「固定機能のリピーターを作らない」に
照らしても、ここは例外になっている。

## 決定事項

### スタイルノードはラスタライズしない。属性を書くだけ

```text
geometry ─→ style.fill ─→ style.stroke ─→ field.apply(stroke_width) ─→ rasterize
              (属性を書く)                  (変調が効く)
```

`Geometry → FrameBuffer` は Rasterize のみ、という
`procedural-geometry.md` の型変換規約を保つ。

**fill / stroke がそれぞれラスタライズして FrameBuffer を merge する形は
採らない。** 描画順が壊れる（図形 A の線は A の塗りより上、かつ図形 B の
塗りより下、が表現できない）うえ、パスが 2 回走る。

### 予約する標準属性

| 名前 | ドメイン | 型 | 意味 |
|---|---|---|---|
| `fill` | Point/Primitive/Instance | Bool | 塗りの有無 |
| `Cd` | 同上 | Color | **塗り色**（既存。意味を明示するだけ） |
| `stroke_width` | 同上 | F32 | 線幅（0 = 線なし） |
| `stroke_color` | 同上 | Color | 線色 |
| `stroke_align` | 同上 | I32 | 0=中央 / 1=内側 / 2=外側 |
| `dash` | Detail | Str | ダッシュパターン（`"4,2"` 形式） |
| `dash_offset` | 同上 | F32 | ダッシュ開始位置 |
| `cap` / `join` | Detail | I32 | 端点・角の形状 |

`Cd` の意味は変わらないので後方互換。`stroke_color` 未設定時は `Cd` に
フォールバックし、現在の挙動と一致する。

### ノードパラメータは既定値として残す

`rasterize` の `fill` / `stroke_width` パラメータは削除せず、
**属性が無いときの既定値**にする。既存プロジェクトはそのまま動く。

優先順位: 要素の属性 > ノードパラメータ > ハードコード既定。

### ダッシュ・キャップ・ジョインは Detail ドメイン

要素ごとに変える需要が薄く、ドメインを Point/Primitive/Instance に広げると
GPU シェーダのバッファが増える。Detail（ジオメトリ全体で 1 値）に置く。

### 積み重ねスタイルは対象外

Illustrator 的な「stroke を 2 本重ねて縁取り」は属性 1 名 1 値と相性が
悪い。必要なら `geometry.merge` で別スタイルのコピーを重ねる。

### 「1 要素内の」グラデーション塗りは対象外

`Cd` が要素ごとの単色である前提を崩す。フィールドや FrameBuffer を
1 要素の塗りソースにする設計は別スコープ。

**要素ごとに色を変えるグラデーション（例: パスに沿って赤→青）は対象内**で、
既存の `field.apply(target = "Cd")` がそのまま担う。境界はこう:

| | 扱い |
|---|---|
| 100 個の要素が位置に応じて別の色になる | 本計画（単位 6 の `field.ramp`） |
| 1 個の矩形の内部が左から右へ色が変わる | 非対象 |

### Color ターゲットの既定コンポーネントマスクは `rgb`

現状 `field.apply` はスカラーフィールドを Color ターゲットの**全 4 成分に
ブロードキャストする**（`crates/ravel-core/src/geometry/field.rs:686-688`）。
結果として、

- 色相は動かず明度方向にしか変化しない（r = g = b になる）
- **アルファまで一緒に動く**（暗くすると同時に透明になる）

`ComponentMask` は既に成分選択に対応している（`field.rs:568-580`）ので、
**Color / Vec4 ターゲットの既定マスクを `rgb` にする**。アルファを動かしたい
場合は明示的に `a` を含める。`Cd` と `stroke_color` の両方に適用する。

既定値の変更なので、既存プロジェクトで `field.apply(target = "Cd")` を
使っているものは挙動が変わる。ロード時マイグレーションは行わず、
移行ノートに記載する（アルファまで動かす意図で使っていたケースは、
明度変調の副作用として偶発的に得られていたものであり、意図的な利用とは
考えにくい）。

## 実装単位

### 単位 1: スタイル属性の読み出し（CPU / GPU 両経路）

- `rasterize` の `Style` を要素ごとに解決する形へ変更。
  属性 → パラメータ → 既定の優先順位。
- CPU 経路: `stroke_color` を fill と別に渡す。
- GPU 経路: インスタンスバッファに `stroke_width` / `stroke_color` /
  `stroke_align` を追加（現在 `data1: [fill, scaled_stroke, 0, 0]` に
  空きがある）。
- **CPU / GPU の出力一致を維持する**（既存のゴールデン等価テストを拡張）。

**完了条件**

- 属性未設定でパラメータどおりに描かれる（**既存ゴールデンが無改変で
  通る**）テスト。
- 要素ごとに異なる `stroke_width` が反映されるゴールデンテスト。
- 塗りと線で異なる色になるゴールデンテスト。
- `stroke_color` 未設定時に `Cd` へフォールバックするテスト。
- CPU / GPU 等価テストを新属性込みで拡張。

### 単位 2: `style.fill` / `style.stroke` ノード

- 指定ドメインへ属性を書くだけ。`group` パラメータに対応
  （`evaluation-scope-plan.md` の規約）。
- `style.fill`: `enabled` / `color` / `domain` / `group`。
- `style.stroke`: `width` / `color` / `align` / `domain` / `group`。

**完了条件**

- 属性が書かれ、他の列が不変であるテスト。
- `group` 指定で対象外要素が変わらないテスト。
- 2 回適用で後勝ちになるテスト。

### 単位 3: ダッシュ・キャップ・ジョイン

- `style.dash`: `pattern` / `offset`。
- キャップ・ジョインは `style.stroke` のパラメータから Detail 属性へ。
- CPU（zeno の `Cap` / `Join` / dash）と GPU シェーダの両対応。
  **GPU 側のダッシュは弧長が要る**ので、実装コストが高ければ
  「ダッシュ時のみ CPU 経路へフォールバック」を許容し、その旨をログに出す。

**完了条件**

- 各キャップ・ジョインのゴールデンテスト。
- ダッシュのゴールデンテスト。
- GPU フォールバックが起きた場合に CPU と一致するテスト。

### 単位 4: 変調との結合と文書更新

- `field.apply(stroke_width, multiply)` でフィールド駆動の可変線幅が
  効くことの結合テスト。**本計画の目的の検証。**
- `docs/specifications/procedural-geometry.md`: 標準属性表に追加。
- registry テンプレート、ロケール。

**完了条件**

- `scatter.grid → field.falloff → apply(stroke_width) → rasterize` で
  中心ほど線が太いゴールデンテスト。
- `mise run check` 通過。

### 単位 5: `field.apply` の属性自動作成と Color 既定マスク

`apply_field` は対象属性が存在しないと `AttributeNotFound` で失敗する
（`crates/ravel-core/src/geometry/field.rs:670-674`）。`Cd` を持たない
ジオメトリに色を変調するには、毎回 `attribute.set(name = "Cd", type = "color")`
を前置する儀式が必要になる。`stroke_color` / `stroke_width` は本計画で
新設する属性なので、**この儀式が全ユーザーに必ず発生する**。

- `field.apply` に `create_if_missing` パラメータを追加（既定 **有効**）。
  対象属性が無いとき、型を推論して既定値で作ってから変調する
- 型の決定順: 予約標準属性なら `procedural-geometry.md` の宣言型
  （`Cd` / `stroke_color` → Color、`stroke_width` → F32）、それ以外は
  フィールドのサンプル型
- 既定値は型ゼロではなく**その属性の意味的な既定**（`Cd` は白、
  `stroke_color` は `Cd` へのフォールバックがあるので白、`stroke_width` は 0）
- Color / Vec4 ターゲットの既定コンポーネントマスクを `rgb` にする
  （決定事項の項を参照）

**完了条件**

- `Cd` を持たないジオメトリに `field.apply(target = "Cd")` が成功するテスト。
- `create_if_missing = false` で従来どおり `AttributeNotFound` になるテスト。
- 作られた属性の型が予約標準属性の宣言型に一致するテスト。
- スカラーフィールドで `Cd` を変調してもアルファが変化しないテスト
  （既定マスク `rgb` の pin）。
- 明示的に `a` を含めたときアルファが変化するテスト。

### 単位 6: `field.ramp`（位置 → 色のランプ）

現状、色を色として駆動できるフィールドが存在しない。Color を返せるのは
`field.attribute`（既存の Color 列を読むだけ）のみで、`field.curve_remap` は
F32 カーブ。つまり**「赤→青のグラデーションで塗る」が組めない**。

- `field.ramp`: スカラー入力（フィールド、または `u` / `index` などの属性）を
  0..1 に正規化し、多ストップのカラーランプで Color を返す
- ストップの表現とエディタは `properties-parameter-editors-plan.md`
  （`ParameterValue::Ramp` とグラデーションエディタ）が担当する。
  本計画はフィールドとしての意味と評価を定義する
- 補間: linear / smooth / constant
- 入力の正規化範囲（`in_min` / `in_max`）をパラメータで持つ

**完了条件**

- 既知のストップで特定入力値の色が期待値になるテスト。
- ストップが 1 個のとき全域が単色になるテスト。
- 入力が範囲外のとき両端の色にクランプされるテスト。
- `field.apply(target = "Cd")` に入れて**色相が変化する**テスト
  （スカラーフィールドではグレースケールにしかならない現状との差分を pin する）。
- パスに沿ったグラデーションの結合テスト:
  `shape.line → attribute.curveu → field.attribute("u") → field.ramp
  → field.apply("Cd") → rasterize` で始点と終点の色が異なるゴールデンテスト。
  `attribute.curveu` は `geometry-ops-plan.md` 単位 13 が追加する
- 同じ経路で `target = "stroke_color"` にしたとき、**線だけ**にグラデーションが
  掛かるテスト（塗りは `Cd` のまま）。

## 非対象

- **積み重ねスタイル**（複数 fill / stroke）。
- **1 要素内のグラデーション塗り / パターン塗り**（要素ごとの色は単位 6 で
  可能になる。上記の決定事項の境界表を参照）。
- **可変線幅（テーパー）**。1 要素内で線幅が変化するもの。
  要素ごとの線幅は本計画で可能になるが、パスに沿った変化は別。
- **`Cd` の意味変更**。塗り色のまま。
