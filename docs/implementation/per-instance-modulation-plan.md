# per-instance 変調 実装計画（REQ-MOGRAPH-001 残件）

> **Status**: Planned — 2026-07-27

対象要件: REQ-MOGRAPH-001（基本シェイプ + インスタンス複製 + per-instance
変調）の未達受入条件。関連: REQ-CORE-010（属性システム）、REQ-CORE-012
（汎用フィールド評価）。

シェイプ生成（`shape.*` 5 種）・複製（`scatter.*` 4 種）・属性操作
（`attribute.*` 4 種）・ラスタライズは
`done/geometry-pipeline-ui-plan.md`（#60–#63）で完了済み。本計画はその上に載る
**変調（modulation）層**だけを対象とする。

## 問題

REQ-MOGRAPH-001 の受入条件のうち、以下が未達。

- [ ] インスタンス属性をフィールド/式で変調してパラメータに反映できる
- [ ] 変調結果（例: 距離フォールオフでスケールが波打つグリッド）が動作する
- [ ] パラメータに Lua 式を設定できる

コードを読むと、変調の器（`field.apply` → `Geometry` → ラスタライザの
per-instance `P` / `rot` / `scale` / `Cd` / `alpha` 消費）は既に通っている。
足りないのは**フィールドと属性のあいだの接続能力**で、具体的には 4 点。

### 1. 型が一致しないと変調できない

`apply_field`（`crates/ravel-core/src/geometry/field.rs:278`）は
サンプル結果と既存カラムの `attr_type()` 完全一致を要求して弾く。
ビルトインフィールドは全て `AttributeArray::F32` を返すため、変調できる
標準属性は `rot`（F32）と `alpha`（F32）だけ。

**受入条件の代表例「距離フォールオフでスケールが波打つグリッド」の
`scale` は Vec2 なので、現状の実装では原理的に不可能。**
`Cd`（Color）・`P`（Vec2）も同様に届かない。

### 2. 合成モードが blend 1 種しかない

`apply_field(geometry, domain, target, field, amount, ctx)` の `amount` は
既存値とサンプル値の線形補間率にしかならない（`blend_arrays`,
`field.rs:293`）。モーショングラフィックスで最も使う
「既定のスケールにフォールオフを**乗算**する」「回転にノイズを**加算**する」
が書けない。`amount = 1.0` は常に「既存値の破棄」を意味する。

### 3. フィールドが位置しか読めない

```rust
pub trait Field: Send + Sync {
    fn sample(&self, positions: &[Vec2], ctx: &EvalContext) -> AttributeArray;
}
```

`index` / `id` / `age` / 任意のユーザ属性を駆動値にできない。結果として
per-instance 変調の主力である **stagger（index 順に遅延をずらす）が
表現できない**。要件記述の「時間オフセット」もここに掛かる。

なお `docs/specifications/procedural-geometry.md:71` のトレイト記述は既に
「位置（**と任意の入力属性**）から値への純関数」となっており、
実装がスペックに追いついていない状態。

### 4. `field.expression` はスタブ

`ExpressionField::sample`（`field.rs:179`）は式を無視して `default` を
定数として返す。式言語そのもの（mlua サンドボックス）は REQ-CODE-001 /
REQ-PLUGIN-003 の管轄で、本計画のスコープ外（後述「非対象」）。

## 決定事項

### 型昇格は「スカラー → 成分マスク」に限定する

F32 フィールドを Vec2 / Vec3 / Vec4 / Color の対象へ適用するとき、
`components` パラメータで指定した成分だけを書き換える。`"xy"` / `"x"` /
`"rgb"` / `"a"` のような成分名リストとし、既定は全成分。
逆方向（ベクタフィールド → スカラー属性）は導入しない。フィールドは
スカラー場という現在の性質を保ち、器だけを広げる。

**本計画の範囲ではフィールドはスカラーのまま**だが、これは「スカラーに
限定する」決定ではない。`Field::sample` の戻り値 `AttributeArray` は既に
`Vec2` / `Vec3` / `Vec4` / `Color` を持ち、型システム上はベクタ場が通る。
ベクタ場（look-at、カール noise、フロー）は `vector-field-plan.md` が扱う。

### 合成モードは属性側の演算として持たせる

`apply_field` にモード引数を足す。`Set` / `Add` / `Multiply` / `Min` /
`Max` / `Blend`。`Blend` のみ `amount` を補間率として使い、他のモードでは
`amount` はサンプル値に掛かる**強度**として作用する
（`existing op (sampled * amount)`）。これで `amount = 0` がどのモードでも
「変調なし」になり、UI 上の意味が一貫する。

既存の呼び出し互換のため、既定モードは `Blend`。既存テストは無改変で通る。

### `Field::sample` の引数を構造体化する

```rust
pub struct FieldSample<'a> {
    pub positions: &'a [Vec2],
    pub attributes: &'a AttributeSet,
    pub ctx: &'a EvalContext,
}

pub trait Field: Send + Sync {
    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray;
}
```

破壊的変更だが影響範囲は閉じている。`geometry/field.rs` の production 実装は
8 個（`Noise` / `Falloff` / `CurveRemap` / `Expression` / `Blend` と、
`binary_field!` マクロが生む `Add` / `Multiply` / `Max`）だが、マクロが
1 箇所にまとまるので**編集箇所は 6 つ**。加えて同ファイルのテスト実装 2 個と
`nodes/field/mod.rs` のテスト 1 個。`.sample(` の呼び出しは全て同ファイル内。

`FieldSample` はハブになる。`vector-field-plan.md` の VEC-1 がこの型に乗り、
`gpu-resident-geometry-plan.md` はこれを WGSL のバッチ評価境界として使う。
今後フィールドが必要とする入力（属性・sim 状態・オーディオ・`positions_3d`）
を追加するたびにシグネチャを壊さないよう、ここで構造体にしておく。

### stagger の順序は `index` の生成順に縛られる（既知の制限）

`index` は生成順に振られる固定値で、`scatter.grid` なら行優先。
したがって本計画だけでは stagger は「行優先で順に」しかできない。
「左から」「中心から外へ」「ランダムに」は
`geometry-ops-plan.md` の `geometry.sort`（要素の格納順を並べ替えて
`index` を振り直す）とセットで初めて成立する。

本計画は「index を駆動値にできる」ところまでを担い、
**その index が何順かを決めるのは Sort の仕事**と切り分ける。

ただし単位 5 の stagger ゴールデンテストは、行優先しか出せないと
「stagger が実用になっている」ことを示せない。**OPS-2 を単位 5 より前に
通しておくこと。** OPS-2 は依存が無く本計画とも独立なので、単位 1〜3 と
並行して進められる。

### 要素スコープ（group）規約は本計画が `field.apply` に入れる

規約そのものは `evaluation-scope-plan.md` が決めている（空文字列で全要素、
対象外の要素は入力値をそのまま通す、存在しない group 名は全要素に
フォールバックして警告）。**その規約を `field.apply` に適用するのは本計画の
単位 1** で、`evaluation-scope-plan.md` の単位 4 は残る 3 op
（`attribute.set` / `attribute.promote` / `geometry.transform`）を担う。

規約は決着済みなので単位 4 を待つ必要はない。両計画が同じ変更を自分の担当と
書いていた重複を、こちらへ寄せて解消した。

`amount` は soft な重み付け、`group` は hard な適用可否で直交する。両方
指定した場合は「group 内の要素にのみ `amount` を適用」。

### 時間オフセットは合成で表現する（専用ノードを作らない）

`field.time`（正規化時刻）と `field.attribute`（`index` 等）を
既存の `field.multiply` / `field.add` / `field.curve_remap` で組めば
`t_i = t - index * delay` が書ける。専用の「per-instance 時間オフセット」
ノードを持たずに済み、フィールド合成と直交する。

当初この方式を採った理由の一つは `KeyframeCurve::sample` が `u64` frame しか
取らずオフセットが量子化・クランプされることの回避だったが、**その制約は
`motion-blur-plan.md` の単位 1（#187）で解消済み** — `sample(f64)` になり、
連続フレーム位置は `EvalContext::sample_frame()` から取れる。

合成方式を維持する判断は変えない（専用ノードを増やさない方が良い）。
ただし `field.curve_remap` に連続時刻を直接食わせる経路が使えるように
なったので、単位 3 の `TimeField` は `frame` を `f64` で返してよい。
整数へ丸める必要はない。

**インスタンスソースのサブグラフを per-instance に別フレームで再評価する
機能は導入しない**（非対象、後述）。

## 目標構成

```text
scatter.grid ──────────────────────────────┐
                                           ▼
field.attribute(index) ─┐            ┌─ field.apply ─→ Geometry ─→ rasterize
field.time ─────────────┼─ 合成 ─→ ─┤   domain=instance
field.falloff ──────────┘  (add/mul/  │   target=scale
field.constant ─────────┘   curve)    │   combine=multiply
                                      │   components=xy
                                      └── amount=1.0
```

新規ノードは 3 つだけで、いずれも「フィールドの駆動源」。

| ノード | 出力 | 役割 |
|---|---|---|
| `field.attribute` | Field | サンプル対象ドメインの属性を読む。`name` / `component` / `normalize`（要素数で割る）パラメータ |
| `field.time` | Field | `ctx` から時刻を返す。`mode`（`frame` / `seconds` / `normalized`）、`scale`、`offset` |
| `field.constant` | Field | 定数スカラー。減算・除算を `multiply` と組んで表現するために要る |

## GPU 方針

**実測ベースライン（限定的）**: `perf-baseline.md` シナリオ (c) で
`shape.rect → scatter.grid(500)` のジオメトリ評価は 0.007 ms。ただし
これは**キャッシュ温**の計測で（同ファイルが「evaluator は構築済み・
キャッシュ温」と明記）、実質**キャッシュヒットのオーバーヘッド**を
測っている。フィールド評価や属性書き込みの実コストは測れていない。

**この数字を「ジオメトリ評価は速い」の根拠に使わない。** 分かるのは
「キャッシュが効く限り安い」だけ。変調はアニメーションすると毎フレーム
再計算になるので、キャッシュが効かない経路こそが本番。

それでも本計画は **CPU（rayon）で実装する**。理由は速いからではなく、
インターフェースが未確定なまま GPU に焼き付けたくないから。実コストは
`gpu-resident-geometry-plan.md` の Phase 0 で測る（そのために
Phase 0 のシナリオ B / C は**アニメーション中の未キャッシュ評価**を
測ることにしてある）。

**GPU を開いておく境界**: 本計画の 2 つの決定がそのまま GPU 移行点になる。

- `FieldSample` 構造体化（単位 2）— バッチ評価インターフェースなので
  1 フィールド = 1 WGSL 関数に写せる。引数を構造体にするのは、
  将来 `positions_3d` やサンプル対象の GPU バッファを足すときに
  シグネチャを壊さないため。
- `CombineMode` + 成分マスク（単位 1）— 変調の書き戻しが
  「既存列 op サンプル列」の単一演算に閉じるので、1 カーネルに落ちる。

フィールドが純関数でありバッチ評価であることも、WGSL 側の関数
シグネチャを単純に保つために効く。ベクタ場（`vector-field-plan.md`）が
入っても戻り値が `vec2<f32>` に増えるだけで構造は変わらない。

**着手トリガ**: `gpu-resident-geometry-plan.md` の Phase 0 で
10 万インスタンスの end-to-end が 16.6 ms を超えた場合。それまでは
CPU 実装のみ。

## 実装単位

### 単位 1: 合成モードと成分マスクと group（`ravel-core` / `ravel-nodes`）

- `geometry/field.rs`: `CombineMode` enum を追加、`apply_field` に
  `combine` と `components: ComponentMask` を渡す形へ拡張。
  型不一致は「昇格不可のときのみ」エラーにする。
- `blend_arrays` を `combine_arrays` に一般化。Bool / Str は従来どおり
  非対応エラー。
- `apply_field` に `group: &str` を足す（規約は上記「要素スコープ」節）。
  空文字列で全要素、group 外の要素は入力値をそのまま通す、存在しない
  group 名 / Bool でない属性は全要素へフォールバックして警告。
- `nodes/field/mod.rs`: `ApplyFieldProcessor` が `combine` / `components` /
  `group` パラメータを読む。
- `registry/builtin.rs`: `field.apply` テンプレートに 3 パラメータを追加し、
  `param_options` で列挙ドロップダウン化（`combine`）。

**完了条件**

- Vec2 の `scale` を F32 フォールオフで `multiply` 変調するユニットテスト。
- Color の `Cd` を `rgb` マスク付きで変調し、`a` が不変であるテスト。
- 全 `CombineMode` × `amount = 0.0` で入力ジオメトリと一致するテスト。
- `group` 未指定で既存の `field.apply` テストが無改変で通る
  （既定 `Blend` + 全要素の後方互換）。
- group 内のみ変化し、group 外が**バイト等価で不変**であるテスト。
- 存在しない group 名 / Bool でない属性が全要素フォールバックになる
  （`Err` ではない）テスト。

### 単位 2: フィールドのサンプル入力拡張 + `field.attribute`

- `FieldSample` 構造体を導入し `Field::sample` を差し替え。既存 7 実装 +
  テスト実装を機械的に移行。
- `apply_field` は対象ドメインの `AttributeSet` を `FieldSample` に載せる。
- `AttributeField` を追加（`name` / `component` / `normalize`）。
  対象属性が無い場合は `default` にフォールバックし、評価全体を落とさない
  （ノードエディタでタイプミス中に赤くならない）。
- `field.attribute` の processor / registry テンプレート。

**完了条件**

- `index` を読んで 0..n-1 を返すテスト、`normalize` で 0..1 になるテスト。
- Vec2 属性から `component = "y"` を取り出すテスト。
- 未知属性名で `default` が返り `Err` にならないテスト。
- 既存フィールド 7 種の挙動が移行前後で不変（既存テスト無改変）。

### 単位 3: 駆動ソースフィールド `field.time` / `field.constant`

- `TimeField` / `ConstantField` を `geometry/field.rs` に追加。
  `TimeField` は `ctx.frame` と `ctx` のフレームレートから
  `frame` / `seconds` / `normalized` を返す純関数。
- processor / registry テンプレート 2 件。

**完了条件**

- 同一 `ctx` で常に同値（純粋性）テスト。
- `frame` / `seconds` / `normalized` の各モードの値検証。
- `field.attribute(index, normalize) → multiply(field.constant) →
  add(field.time) → curve_remap → apply(rot, add)` の結合テストが
  インスタンスごとに異なる `rot` を出す（= stagger 成立）。

### 単位 4: `attribute.delete`（REQ-CORE-010 の取りこぼし）

REQ-CORE-010 は属性の「読み書き・追加・**削除**」を求めているが、削除だけ
コア API もノードも存在しない。変調グラフでは中間の駆動用属性
（`stagger_t` 等）を下流へ流したくない場面が出るため、ここで回収する。

- `AttributeSet::remove(&mut self, name: &str) -> Option<Arc<AttributeArray>>`。
- `geometry/ops.rs` に `attribute_delete(geometry, domain, name)`。
  `P` の削除は `Geometry::validate` を壊すため、Point / Instance ドメインの
  `P` は削除拒否のエラーにする。
- `attribute.delete` processor + registry テンプレート（`domain` / `name`）。

**完了条件**

- 削除後に他列が `Arc` 共有のまま残るテスト。
- 存在しない属性名の削除が no-op（`Err` ではない）テスト。
- `P` 削除が拒否されるテスト。

### 単位 5: 受入条件のゴールデン検証と文書更新

- `crates/ravel-nodes` に CPU ラスタライズ経由のゴールデンテスト 2 本。
  1. **波打つグリッド**: `scatter.grid` → `field.falloff` →
     `field.apply(scale, multiply, xy)` → `rasterize`。中心付近の
     インスタンスが大きく、外側が小さいことを画素で検証。
  2. **stagger**: 単位 3 の結合を 2 フレーム分評価し、フレーム間で
     インスタンスの回転差分が index に比例することを検証。
- `docs/specifications/procedural-geometry.md`: `Field` トレイト記述を
  実装に合わせて更新（`FieldSample`）、合成モードと成分マスクの節を追加。
- `docs/requirements/REQ-MOGRAPH.md`: REQ-MOGRAPH-001 の受入条件のうち
  達成分にチェックを入れる。Lua 式の項目には REQ-CODE-001 依存である旨を
  注記する。
- `docs/implementation/README.md`: 本計画を Live documents 表に追加。

**完了条件**

- ゴールデンテスト 2 本が GPU なしで通る。
- `mise run check` が通る。

なお REQ-CORE-010 の受入条件のうち、単位 4 で「削除」が埋まり、
残るのは「Lua 式から属性値を参照できる」のみになる。

## 完了条件（要件レベル）

| REQ-MOGRAPH-001 受入条件 | 本計画で | 単位 |
|---|---|---|
| インスタンス属性をフィールドで変調してパラメータに反映できる | 達成 | 1, 2 |
| 変調結果（距離フォールオフでスケールが波打つグリッド）が動作する | 達成 | 1, 4 |
| パラメータに Lua 式を設定できる | **未達のまま** | — |

Lua 式を除く 6 条件が満たされ、REQ-MOGRAPH-001 は
「式を除いて完了」の状態になる。

## 検証

- ユニットテスト: `cargo test -p ravel-core -p ravel-nodes`
- ゴールデン: CPU ラスタライズ経路のみ。GPU アダプタ不要。
- 決定性: 新規フィールドはいずれも `ctx` と属性のみに依存する純関数。
  乱数は `NoiseField` の既存 seed ハッシュのみで、新規導入なし。
- 事前 PR: `mise run check` + `ravel-review` スキル。

## 非対象

- **Lua 式**（`field.expression` の実装、パラメータ式）。mlua サンドボックス
  を含む REQ-CODE-001 / REQ-PLUGIN-003 の独立スコープ。本計画では
  `ExpressionField` をスタブのまま残し、削除もしない。
- **インスタンスソースの per-instance 時間再評価**。「インスタンス i の
  ソースサブグラフを frame `t - offset[i]` で評価する」は評価エンジンに
  per-element 時間軸を持ち込む変更で、キャッシュ設計（REQ-CORE-006）と
  sim キャッシュ（REQ-CORE-011）に波及する。属性レベルの時間オフセットで
  代替し、必要になった時点で別計画にする。
- **ベクタ場**。本計画ではスカラー場のみを扱い、ベクタ変調は成分マスク +
  複数 `field.apply` で表現する。ベクタ場そのものは
  `vector-field-plan.md`。
- **フィールドの GPU 評価**。`gpu-resident-geometry-plan.md` の単位 2。
  本計画は境界を開けておくだけで、WGSL は書かない。
- **属性スプレッドシート UI**。`attribute-spreadsheet-plan.md` に分離。
  変調結果の目視検査はそちらが担う。
- REQ-MOGRAPH-002（パーティクル）以降。004 プロシージャルタイポグラフィが
  本計画の直後の対象。
