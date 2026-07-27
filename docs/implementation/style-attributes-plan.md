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

### グラデーション塗りは対象外

`Cd` が要素ごとの単色である前提を崩す。フィールドや FrameBuffer を
塗りソースにする設計は別スコープ。

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

## 非対象

- **積み重ねスタイル**（複数 fill / stroke）。
- **グラデーション塗り / パターン塗り**。
- **可変線幅（テーパー）**。1 要素内で線幅が変化するもの。
  要素ごとの線幅は本計画で可能になるが、パスに沿った変化は別。
- **`Cd` の意味変更**。塗り色のまま。
