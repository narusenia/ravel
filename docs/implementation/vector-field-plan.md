# ベクタ場 実装計画

> **Status**: In progress — 単位 7 の `vector.construct`（`VEC-7a`）と
> 単位 5（`VEC-5`）が実装済み（2026-07-30）。他の単位は未着手

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
- **Color / Vec4 を含める**。`AttributeArray` は両方を持ち
  （`component_arity` は Color を 4 として扱う。
  `crates/ravel-core/src/geometry/field.rs:592`）、`style-attributes-plan.md`
  単位 6 の `field.ramp` が Color を返すため、Color の二項合成が必要になる。
  Color どうしの成分ごと演算と、スカラー × Color のブロードキャストのテスト。

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

### 単位 5: Vec パラメータの正規化（`_x` / `_y` → `Channel2`）— 実装済み

フィールド側をベクタ化しても、**値ドメインの Vec がノードとして存在せず、
パラメータも Vec になっていない**ので繋ぐ先が無い。

組み込みノードは Vec を**別々の Float パラメータに分解して**宣言していた
（以下は実装前の状態。現在は右列を 1 つの `Channel2` / `Channel3` に統合済み）。

| ノード | 統合前のパラメータ |
|---|---|
| `field.falloff` | `center_x` / `center_y`、`direction_x` / `direction_y` |
| `geometry.transform` | `translate_x/y`、`scale_x/y`、`pivot_x/y` |
| `transform` | `translate_x/y` |
| `shape.rect` | `center_x` / `center_y` |
| `shape.ellipse` | `center_x/y`、`radius_x/y` |
| `shape.polygon` | `center_x/y` |
| `shape.star` | `center_x/y` |
| `scatter.grid` | `center_x/y`、`spacing_x/y`（`count_x/y` は Int） |
| `scatter.circular` | `center_x/y` |
| `scatter.path_array` | `center_x/y` |
| `scatter.scatter` | `center_x/y`、`area_x/y` |
| `attribute.set` | `value` / `value_y` / `value_z` / `value_w`（アリティは `type` 次第） |

**畳む対象は Float の成分パラメータだけ**。`scatter.grid` の `count_x` /
`count_y` は `Int` で、`Channel2` は成分ごとの float チャネルなので畳むと型の
意味が変わる — Int の対は分解のまま残す。

帰結が 3 つ:

1. **Properties の Vector 行が使われない**。横並びの成分表示は実装済み
   （`crates/ravel-app/src/panels/properties.rs:294-299` が
   `div().flex().gap_1()` で成分ごと `ScrubInput` を並べる）だが、
   到達するのは `Channel2` / `Channel3` パラメータのみ。分解された Float は
   独立した行が 2 本並ぶ
2. **パラメータポートが 2 本に割れる**。`expose_param_port` はキー単位なので
   `center_x` と `center_y` が別ポートとして露出する。`Channel2` なら
   `port_data_type()` が VEC2 を返して 1 ポートで受けられる
   （`crates/ravel-core/src/graph.rs`）。
   **ただし `Channel3` は現在 `port_data_type()` が `None`** を返す
   （「3 成分の wire 型が無い」というコメント付き）。`DataTypeId::VEC3` と
   `types::Vec3` は既に存在し、`net.rs` の `zero_value` も VEC3 を扱うので、
   これは対応漏れであって設計上の欠落ではない。**`Channel3` を VEC3 に
   対応付ける修正を本単位に含める**（`port_data_type()` と
   `eval.rs` の wire → パラメータ強制の両方）。含めないと、
   `translate_x` / `translate_y` が今は SCALAR ポートで露出できているのに、
   `Channel3` へ統合した途端に露出不能になって**退行する**
3. **Viewer のマニピュレータが宣言的に書けない**。
   `viewer-overlay-manipulator-plan.md` の `ParamRole` は 1 パラメータに
   1 つの意味を付ける仕組みで、分解されていると名前の組を推測することになる

- 上記ノードの Vec パラメータを `Channel2` / `Channel3` に統合する
- **3D 対応が来るパラメータは最初から `Channel3` にする。** `Channel2` に
  統合してから 3D で `Channel3` へ移すと、`.ravprj` の移行が 2 回走る。
  `3d-scene-plan.md` の単位 1a が `geometry.transform` を 3 成分対応にするので、
  統合先を先回りして決めておく

  | パラメータ | 統合先 | 既定値 | 根拠 |
  |---|---|---|---|
  | `geometry.transform` / `transform` の `translate_x/y` | `Channel3` | z = 0 | 単位 1a で 3D 対応 |
  | `geometry.transform` の `scale_x/y` | `Channel3` | z = 1 | 同上 |
  | `geometry.transform` の `pivot_x/y` | `Channel3` | z = 0 | 同上 |
  | `geometry.transform` の `rotation`（F32 度） | `Channel3`（オイラー） | x = y = 0 | 2D の回転は Z 軸回りなので `(0, 0, θ)` で挙動が保存される |
  | `field.falloff` の `center` / `direction` | `Channel3` | z = 0 | 3D フィールドを作るときに要る |
  | `shape.rect` / `shape.ellipse` の `center` / `radius` | `Channel2` | — | 形状定義は 2D のまま（3D は `mesh.*` が担う） |
  | `attribute.set` の `value` 系 | 型パラメータに従う | — | 既に Vec2/Vec3/Vec4/Color を選べる |

- ロード時マイグレーション: `<name>_x` / `<name>_y`（`attribute.set` は
  `value` / `value_y` / …）を `<name>` に畳む。片方だけ存在する場合は
  欠けた成分を既定値で埋める
- **マイグレーションの実施層**: グラフは `graph/main.ron` に RON で保存され、
  `GraphDoc` を経て `Node` / `ParameterValue` へ**型付きでデシリアライズ**
  される。`project/migration.rs` の既存連鎖は `manifest.json` を
  untyped JSON として扱うものなので、パラメータの畳み込みには使えない。
  一方、ノードのパラメータは自由なキー / 値の対なので、旧ファイルの
  `center_x: Float(..)` は**そのまま RON パースを通る**（テンプレート宣言と
  一致しなくてもよい）。したがって移行は**ロード後のグラフに対する型付き
  パス**として書き、`manifest.json` の `format_version` を 5 に上げて
  ゲートする。パスは**レイヤーネットワークと `subnet` の内側グラフを含む
  全グラフ**を走査する必要がある（`Layer::network`、`Node::subnet`）
- **露出済みパラメータポートの移行**: 旧ファイルで `center_x` と `center_y` が
  それぞれ入力ポートとして露出しエッジが繋がっている場合、`center` の
  1 ポートに畳む。**両方に別のノードが繋がっている場合は畳めない**ので、
  その場合は `vector.construct` ノードを挿入して両方のエッジを保存する。
  **したがって `vector.construct`（単位 7）は本単位より前に入っている
  必要がある**（`vector.construct` は Scalar 入力と Vec 出力だけで成立し、
  単位 6 の定数ノードを必要としないので、単位 7 のうち `construct` だけを
  先に切り出してよい）
- プロセッサ側の読み出しを `params.vec2_or("center", ..)` に統一する
  （GPU の uniform 詰めも同じ箇所）
- パラメータ範囲（`with_param_range`）を成分共通の 1 宣言に統合する

**実装結果**

- 統合先: `ParameterValue::vec2` / `vec3` コンストラクタを追加し、
  `registry/builtin.rs` の全テンプレートを上表どおりに統合。
  `geometry.transform` の `rotation` は `Channel3`（オイラー度、Z が 2D の角度）
- `Channel3` → `DataTypeId::VEC3`: `ParameterValue::port_data_type()` と
  `eval.rs` の wire → パラメータ強制（`Vec3` 経路）を追加。これで
  `translate` は VEC3 パラメータポートとして露出でき、統合による露出の退行が無い
- マイグレーション: `ravel_core::composition::param_fold`
  （`Document::fold_component_params()`）が全グラフ（平坦グラフ・
  `Layer::network`・`Node::subnet` の内側）を走査して畳む。`manifest.json` の
  `CURRENT_FORMAT_VERSION` を 5 に上げ、`migrate_v4_to_v5` はバージョン印だけを
  進める（連鎖は RON を見ない）。`ProjectFile::from_archive` が
  `source_version < 5` のときに畳み込みを実行する（`advance_id_counters()` の
  **後**。`vector.construct` の挿入で ID を発行するため）
- **`attribute.set` は型駆動で畳んだ**。`value` / `value_y` / `value_z` /
  `value_w` を、そのノードが保存している `type` に従って `value` 1 本にする
  （`f32` → `Channel`、`vec2` → `Channel2`、`vec3` → `Channel3`、`vec4` /
  `color` → `Channel4`）。`type` が読まない成分は捨てる。欠けた成分は
  `attribute_set_value_defaults` で埋める（`color` のアルファだけ 1、他は 0。
  レジストリの他の色と揃える。v4 のテンプレートは 4 成分すべてを書くので、
  実在の v4 ファイルでは自前のアルファが使われこの既定は効かない）。
  `i32` / `bool` / `string` は `int_value` / `bool_value` / `string_value` を
  読むので、`value` は 1 成分チャネルとして残る
- **4 成分パラメータポートは `COLOR` と `VEC4` の両方を受ける**。
  `ParameterValue::port_accepted_types()` を足し、`expose_param_port` と
  ロード時の `normalize_param_ports` がこれを使う（`port_data_type()` は
  ポート色などで 1 つの型が要る場面のための**主型**として残す。役割の違いは
  doc コメントに明記）。同じ 4 つの float の 2 通りの読み方なので、
  `attribute.set` の `type = "vec4"` か `"color"` かをパラメータ側は知らない。
  `eval.rs` の `param_port_overlay` も `Color` に加えて `Vec4` を受ける。
  これが無いと `vector.construct.vec4` を 4 成分パラメータへ繋げず、
  畳む前に 4 本の SCALAR ポートで駆動できていたことに対する**退行**になる
- **`type` 変更時の再型付け**: `type` を変えると `value` のアリティが変わる。
  `registry::builtin::dependent_param_updates` が付随する更新を返し、
  `Graph::set_params` が値とポートを**1 回の呼び出しで**適用する
  （Document スナップショット = undo 単位なので、値だけ変わった中間状態を
  コミットさせない）。露出済みパラメータポートは**受け入れ集合**が変わった
  ときだけ作り直し、**そこへ入っていたエッジは落とす** — Scalar 出力は VEC3
  ポートを駆動できないので、残せば「型が嘘をつくポート」になる。集合が
  変わらない変更（`f32` の `Float` → `Channel`、`vec4` ↔ `color`）は
  ポートもエッジもそのまま
- `type` は `param_options` を持つ closed set になり、Properties では
  ドロップダウンで選ぶ（再型付け経路が自由入力から届かないようにするため）
- `scatter.grid` の `count_x` / `count_y` は `Int` なので対象外
  （`Channel2` は成分ごとの float チャネルで、畳むと型の意味が変わる）。
  `scatter.scatter` の `area_x` / `area_y` は本計画の表に無かったが、
  幾何ベクタの Float 対なので `area`（`Channel2`）へ統合した — 表に
  追記済み

**完了条件**

- 旧形式で保存されたプロジェクトが開き、同じ描画結果になるゴールデンテスト。
- 片方の成分だけを持つ旧ファイルが既定値で埋められて開くテスト。
- `Channel3` に統合したパラメータで、z 成分の既定値が挙動を変えないテスト
  （`translate` は 0、`scale` は 1、`rotation` は x = y = 0）。
- `rotation` を `Channel3` に統合したとき 2D の回転結果が bit 一致するテスト。
- `center_x` / `center_y` の両方にエッジがある旧ファイルが
  `vector.construct` 挿入で開き、評価結果が一致するテスト。
- 統合後の Properties が Vector 行（横並び）になる `ravel-ui` テスト。
- ラウンドトリップ（保存 → ロード）で値が保存されるテスト。
- `subnet` 内側とレイヤーネットワークのノードも移行されるテスト。
- `Channel3` パラメータが VEC3 ポートとして露出でき、Vec3 出力で駆動できる
  テスト（`translate` の露出が退行していないことの確認）。

すべて実装済み。所在:

| 完了条件 | テスト |
|---|---|
| 旧形式が同じ描画結果 | `crates/ravel-nodes/tests/shape_layer_golden.rs::a_folded_v4_network_renders_the_same_pixels_as_a_v5_one` |
| 片方だけの成分が既定値で埋まる | `composition::param_fold::a_missing_component_takes_the_template_default`、`project::a_v4_project_with_one_component_fills_the_other_with_the_default` |
| z 既定値が挙動を変えない | `composition::param_fold::channel3_folds_use_behaviour_preserving_z_defaults`、`ravel-nodes` `geometry::channel3_z_defaults_keep_the_identity_fast_path` |
| `rotation` の 2D 回転が bit 一致 | `ravel-nodes` `geometry::euler_rotation_z_reproduces_the_scalar_rotation_bit_for_bit` |
| 両方にエッジ → `vector.construct` 挿入 | `composition::param_fold::two_driven_component_ports_gain_a_vector_construct`、`project::a_v4_project_with_two_driven_component_ports_gains_a_vector_construct` |
| Properties が Vector 行になる | `ravel-ui` `properties::node::folded_builtin_vector_params_render_as_vector_rows` |
| 保存 → ロードのラウンドトリップ | `project::the_folded_value_roundtrips_through_save_and_load` |
| `subnet` / レイヤーネットワーク | `composition::fold_component_params_reaches_every_graph_of_the_document` |
| `Channel3` が VEC3 ポートになる | `graph::channel_arities_map_to_wire_types`、`eval::vec3_param_ports_convert_componentwise` |
| `attribute.set` の型ごとの移行 | `composition::param_fold::attribute_set_value_folds_to_the_arity_its_type_reads`、`project::a_v4_attribute_set_folds_by_type_and_roundtrips` |
| `attribute.set` の片方だけの成分 | `composition::param_fold::attribute_set_partial_components_take_the_type_defaults` |
| `type` 変更で成分が保たれる | `registry::builtin::attribute_set_value_retyping_keeps_shared_components`、`…preserves_keyframes` |
| 露出ポートの追随とエッジの扱い | `graph::set_params_retypes_a_port_and_drops_its_now_invalid_edges`、`graph::set_params_keeps_a_port_whose_wire_type_is_unchanged`、`graph::set_params_keeps_a_port_when_only_the_principal_type_would_differ`、`param_fold::a_scalar_attribute_set_value_keeps_its_port_and_edge` |
| 4 成分が個別駆動された旧ファイルでエッジが残る | `param_fold::a_four_component_attribute_set_value_keeps_its_drivers`、`project::a_v4_attribute_set_with_driven_components_keeps_its_edges`（`vec4` / `color` 両方） |
| `Channel4` ポートが VEC4 でも駆動できる | `eval::vec4_and_color_both_drive_a_channel4_param_port`、`composition::normalize_param_ports_flags_legacy_pins_and_widens_accepted_types` |
| 再型付けが 1 undo に収まる | `panels::node_editor::changing_attribute_set_type_retypes_value_and_its_port_in_one_undo` |
| `attribute.set` が Vector 行になる | `ravel-ui` `properties::node::attribute_set_value_renders_at_the_arity_its_type_selects` |

### 単位 6: 値ドメインのベクタ定数（`constant.vec2` / `vec3` / `vec4`）

registry に Vec を出力するテンプレートが 1 つも無い（`constant` は Scalar、
`constant.color` は Color）。単位 5 で Vec2 パラメータポートができても、
そこへ繋ぐ**値の供給源が無い**。

- `constant.vec2` / `constant.vec3` / `constant.vec4`。成分ごとの
  `Channel` を持ち、キーフレーム可能
- Properties では単位 5 と同じ Vector 行になる

**完了条件**

- 各ノードが宣言どおりの型を出力するテスト。
- 成分ごとにキーフレームが打てるテスト。
- `shape.rect` の `center` ポートへ接続して位置が変わる結合テスト。

### 単位 7: 値ドメインの構成・分解（`vector.construct` / `split` / `swizzle`）

単位 2 は**フィールド**の変換（Field → Field）。こちらは**値**の変換
（Scalar / Vec ポート同士）で、出力型が違うため実装を共有しない。

**`vector.construct` は単位 5 より前に必要**（単位 5 のパラメータポート移行が
挿入するため）。この 3 ノードは互いに独立なので、`construct` だけを先に
入れて `split` / `swizzle` を後にしてよい。`construct` 自体は単位 6 に
依存しない。

- `vector.construct`: Scalar × N → Vec。**アリティごとに別 `type_key`**
  （`vector.construct.vec2` / `.vec3` / `.vec4`、実装済み）。`type`
  パラメータにしないのは、**ポート型がノードインスタンスに保存される**ため
  アリティを切り替えるには出力ポートの再型付けと既存エッジの整理が必要で、
  その機構は `network-interface-editing-plan.md` 単位 1 の担当になるから。
  単位 6 の定数ノードを `constant.vec2` / `vec3` / `vec4` に分けるのと同じ理由。
  成分は `x` / `y` / `z` / `w` の **Float パラメータ**（`math.scalar` と同じ形 —
  未接続なら Properties で編集でき、接続すればパラメータポートで駆動できる）。
  未設定の成分は 0
- `vector.split`: Vec → Scalar × N。**多出力**なので `PortRecord` を返す
  既存規約（`net.in` / `subnet` と同じ）に乗る
- `vector.swizzle`: Vec → Vec。`"xy"` / `"zyx"` / `"xxx"` のような
  文字列パラメータ。存在しない成分の指定はエラー

**完了条件**

- `construct` が宣言どおりの型を出力し、成分がパラメータポートで駆動できる
  テスト（単位 5 の移行が依存する形）。**済み**。
- `construct` → `split` の往復一致テスト。
- `split` が `PortRecord` を返し、各出力が単独で pull できるテスト。
- `swizzle` が成分を並べ替えるテスト。
- 存在しない成分（Vec2 に対する `"z"`）でエラーになるテスト。
- アリティ変更（Vec3 → Vec2）でエッジがどう扱われるかのテスト
  （`network-interface-editing-plan.md` 単位 1 の再インデックスを使う）。

### 単位 8: 値ドメインのベクタ演算（`vector.length` / `normalize` / `dot` / `cross`）

- `length`: Vec → Scalar
- `normalize`: Vec → Vec。ゼロベクトルの扱いを定義する（ゼロを返す）
- `dot`: Vec × Vec → Scalar
- `cross`: Vec2 × Vec2 → Scalar（2D の外積はスカラー）、
  Vec3 × Vec3 → Vec3

**完了条件**

- 各演算の値検証テスト。
- `normalize` がゼロベクトルでゼロを返すテスト（NaN を出さない）。
- `dot` / `cross` の型不一致（Vec2 × Vec3）がエラーになるテスト。
- 単位 2 のフィールド版と値が一致するテスト（同じ入力に対して）。

### 単位 4: 結合検証と文書更新

**単位 1〜3 と 5〜8 のすべてを対象にする**（最後に実施する）。

- **look-at のゴールデンテスト**: `scatter.grid` の各インスタンスが
  1 点を向く。本計画の目的の検証。
- **フロー場のゴールデンテスト**: `curl_noise → apply(P, add)` で
  ポイント群が渦を巻く。
- `per-instance-modulation-plan.md` の「スカラー場に限る」決定を撤回する
  記述に差し替え。
- `gpu-resident-geometry-plan.md` の該当記述を修正。
- `docs/specifications/procedural-geometry.md` のフィールド節を更新。
- `docs/agent-api-reference.md` に値ドメインのベクタノードと
  `Channel2` 化したパラメータを記載。
- `docs/ui-impl-status.md` の Properties 表を更新（Vector 行が実際に
  使われるようになる）。

## 非対象

- **暗黙の型変換**。
- **テンソル場 / 行列場**。
- **フィールドの空間キャッシュ**（グリッドへの事前サンプル）。
- **`rot` を Vec2 で持つ設計変更**。`rot` は F32 のまま、角度変換で繋ぐ。
- **`ParameterValue::Vec2` のような非アニメート Vec 型の追加**。Vec は
  `Channel2` / `Channel3`（成分ごとのアニメーションチャネル配列）で表す。
  殻の Transform が既にこの形（`crates/ravel-ui/src/properties/layer.rs:547`）。
- **`Channel4` の Properties 描画が常に Color であること**。
  `properties/node.rs` が `Channel4` を常に Color フィールドにしている問題は
  UI 側の局所修正で、`issues/medium/` の `MED-APP-19` に起票済み。単位 5 で
  `attribute.set` の `type = "vec4"` が**色でない `Channel4` の実例**に
  なった（`color` は色なので現状で正しい）。**wire 型のほうは単位 5 で解決した**
  — 4 成分パラメータポートは `COLOR` と `VEC4` の両方を受ける
  （`ParameterValue::port_accepted_types`）。残るのは描画の話だけ。
- **Vector 行の成分ラベルとリンクトグル**（`MED-APP-20`）。単位 5 は Vector 行を
  到達可能にしただけ。
- **`ParamRole` の宣言とマニピュレータ**。
  `viewer-overlay-manipulator-plan.md` 単位 5 が担当する（本計画の単位 5 に
  依存する側）。
