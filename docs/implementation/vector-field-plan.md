# ベクタ場 実装計画

> **Status**: Planned — 2026-07-27

対象: フィールドをスカラー場に限定している制約を外し、look-at・フロー場・
カール noise を可能にする。関連要件: REQ-CORE-012、REQ-MOGRAPH-001、
REQ-MOGRAPH-002。

**前提**: `per-instance-modulation-plan.md` の単位 2（`FieldSample` 構造体化）。

## 問題

「各インスタンスが中心を向く」「渦を巻く流れ」が書けない。モーション
グラフィックスでは定番の絵で、`scatter` と組み合わせたときの表現力を
大きく左右する。

### ただし型システムの制約ではない

```rust
pub trait Field: Send + Sync {
    fn sample(&self, positions: &[Vec2], ctx: &EvalContext) -> AttributeArray;
}
```

**戻り値の `AttributeArray` は既に `Vec2` / `Vec3` / `Vec4` / `Color` の
バリアントを持つ。** `apply_field` の型完全一致要求（`field.rs:278`）も、
Vec2 フィールドを Vec2 属性へ適用するぶんには通る。`blend_arrays` も
ベクタ対応済み。

スカラー限定なのは**組み込み実装がすべて `AttributeArray::F32` を返すこと**と、
二項合成が `scalar_values()` で強制変換していること（`field.rs:344`）だけ。

つまり**インターフェース変更は不要**で、実装の追加と合成側の多相化で済む。
`per-instance-modulation-plan.md` に書いた「フィールドはスカラー場という
性質を保つ」という決定は、この事実を踏まえて**撤回する**。

## 決定事項

### 暗黙の型変換は入れない

ベクタ場をスカラー消費地点に繋いだとき、長さを取って通すような暗黙変換は
しない。**明示ノード**を用意する。

- `field.length`: ベクタ場 → スカラー場（長さ）
- `field.component`: ベクタ場 → スカラー場（成分選択）
- `field.compose`: スカラー場 2〜4 本 → ベクタ場

暗黙変換を入れると、繋ぎ間違いが黙って動いてデバッグ不能になる。
型不一致は `apply_field` の既存エラーで弾く。

### 二項合成を多相化する

`field.add` / `multiply` / `max` / `blend` が `scalar_values()` で
F32 に潰しているのを、**両辺の型が一致すればその型で演算**する形に変える。

- 同型どうし: その型で成分ごとに演算
- スカラー × ベクタ: スカラーをブロードキャスト（`multiply` で
  「ベクタ場を強度で縮める」が書けるようになる。実用上ここが一番効く）
- 型が食い違い、かつブロードキャストでも解決しない場合はエラー

### 追加するベクタ場

| ノード | 出力 | 用途 |
|---|---|---|
| `field.direction_to` | Vec2 | 指定点（またはジオメトリ入力の位置）への単位ベクトル。**look-at の素** |
| `field.curl_noise` | Vec2 | 発散ゼロの渦。パーティクルの乱流が自然になる |
| `field.gradient` | Vec2 | 入力スカラー場の勾配。等高線に沿う/垂直な流れ |
| `field.radial` | Vec2 | 中心からの放射 / 接線方向 |

### look-at は `field.direction_to` + 角度変換で書く

`rot` は F32（ラジアン）なので、ベクタ場から直接は入らない。
`field.angle`（Vec2 場 → 角度のスカラー場、`atan2`）を足す。

```text
field.direction_to(target) ─→ field.angle ─→ field.apply(rot, set)
```

専用の「look-at ノード」は作らない。合成で書ける形にしておけば、
「中心を向く + ノイズで揺らす」が自然に繋がる。

### GPU 移行の境界は変わらない

`gpu-resident-geometry-plan.md` の単位 2（フィールドの WGSL 評価）は
戻り値が `f32` から `vec2<f32>` に増えるだけで、1 フィールド = 1 WGSL 関数
という構造は保たれる。同計画の「WGSL のシグネチャを単純に保つため
スカラーに限る」という記述は本計画に合わせて修正する。

## 実装単位

### 単位 1: 二項合成の多相化

- `scalar_values()` による強制変換を廃し、型ごとの演算へ。
- スカラー × ベクタのブロードキャスト。
- 解決不能な型の組み合わせはエラー。

**完了条件**

- スカラーどうしの既存挙動が不変（**既存テスト無改変**）。
- Vec2 どうしの成分ごと演算のテスト。
- スカラー × Vec2 のブロードキャストのテスト。
- 型不一致がエラーになるテスト。

### 単位 2: 変換ノード（`length` / `component` / `compose` / `angle`）

**完了条件**

- 各変換の値検証テスト。
- `compose` → `component` の往復一致テスト。
- `angle` が `atan2` の値域（-π..π）を返すテスト。
- ゼロベクトルでの `length` / `angle` の定義を明示したテスト。

### 単位 3: ベクタ場ノード（`direction_to` / `curl_noise` / `gradient` / `radial`）

- `direction_to` はターゲットをパラメータまたはジオメトリ入力で受ける。
- `curl_noise` は既存の simplex 実装から偏微分で構成し、決定性を保つ。
- `gradient` は入力スカラー場を有限差分でサンプルする（刻み幅パラメータ）。

**完了条件**

- `direction_to` が単位ベクトルを返すテスト。
- `curl_noise` の発散がゼロ近傍であるテスト（数値的に）。
- 同一 seed での `curl_noise` 再現性テスト。
- `gradient` が既知のスカラー場で解析解と一致するテスト。

### 単位 4: 結合検証と文書更新

- **look-at のゴールデンテスト**: `scatter.grid` の各インスタンスが
  1 点を向く。本計画の目的の検証。
- **フロー場のゴールデンテスト**: `curl_noise → apply(P, add)` で
  ポイント群が渦を巻く。
- `per-instance-modulation-plan.md` の「スカラー場に限る」決定を撤回する
  記述に差し替え。
- `gpu-resident-geometry-plan.md` の該当記述を修正。
- `docs/specifications/procedural-geometry.md` のフィールド節を更新。

## 非対象

- **暗黙の型変換**。
- **テンソル場 / 行列場**。
- **フィールドの空間キャッシュ**（グリッドへの事前サンプル）。
- **`rot` を Vec2 で持つ設計変更**。`rot` は F32 のまま、角度変換で繋ぐ。
