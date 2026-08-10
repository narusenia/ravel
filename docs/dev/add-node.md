# ノードを追加する

> 索引: [`README.md`](README.md)

新しいノード型（`type_key`）を 1 つ足す手順。規約は
[`.agents/rules/rust.md`](../../.agents/rules/rust.md)、型と関数の地図は
[`../agent-api-reference.md`](../agent-api-reference.md)。

## チェックリスト

- [ ] `crates/ravel-core/src/registry/builtin.rs` に `NodeTemplate` を追加
- [ ] `assets/locales/en.toml` / `ja.toml` に `[node."<type_key>"]` の
      `label`（必須）と `description` / `params.<name>`（任意）を追加
- [ ] `crates/ravel-app/src/assets.rs` の `RavelIcon::for_node_type` に
      `type_key` のアイコンを 1 行追加（SVG が新規なら Lucide から
      `assets/icons/` へ vendoring。手順は `ui-design-impl` スキル。
      未登録でもカテゴリ既定アイコンにフォールバックするのでビルドは壊れない）
- [ ] `crates/ravel-nodes/src/<領域>/` に `NodeProcessor` の実装を追加
- [ ] `crates/ravel-nodes/src/lib.rs` の `processor_for_node` の `match` に
      `type_key` を追加
- [ ] 単体テスト（プロセッサの入出力）
- [ ] GPU 版を足す場合: WGSL を `crates/ravel-nodes/src/shaders/` に置き、
      **CPU 経路との等価性テスト**を追加
- [ ] `mise run check`

**忘れやすいもの**: `processor_for_node` の `match` は `type_key` の文字列比較
なので、追加を忘れてもコンパイルは通る。テンプレートだけ足すと「ノードは
置けるが評価されない」状態になる。

## 1. テンプレートを宣言する

`registry/builtin.rs` にノードの「形」を宣言する。UI（Properties / Node Editor /
右クリックメニュー）はここだけを見る。

```rust
NodeTemplate::new("field.noise", "Noise Field", NodeCategory::Field)
    .with_output(OutputPort::new("field", DataTypeId::FIELD))
    .with_param(Parameter::float("frequency", 2.0))
    .with_param_range("frequency", ParamRange::new(0.0, 32.0))
```

- `type_key` は `<領域>.<名前>` のドット区切り。**永続化に載る識別子**なので
  後から変えるとマイグレーションが必要
- `label` は英語リテラルのまま渡す。生成されたノードの `metadata.label` の
  既定値（= ユーザーリネーム検出の基準）として残る一方、UI の表示は
  `assets/locales/{en,ja}.toml` の `[node."<type_key>"] label` を使う
  （[`add-locale.md`](add-locale.md)）。**テンプレートを足したら en / ja 両方に
  キーを足すこと** — `ravel-ui::node_locale` のレジストリ走査テストが欠落を
  落とす。キーが無い型は `type_key` 表示にフォールバックする
- アイコンは `RavelIcon::for_node_type(type_key, Some(category))`
  （`ravel-app` の `assets.rs`）が種別ごとに決める。対応表はあそこ 1 箇所
  だけで、`NodeTemplate` にフィールドは増えない。未登録の `type_key` は
  カテゴリ既定アイコン（`RavelIcon::for_category`）にフォールバックする
- `param_ranges` はスクラブ入力のソフトクランプに使う。範囲が無い数値は
  無制限スクラブになる
- `param_options` を付けた文字列パラメータは Properties で dropdown になる
  （自由入力にしない）
- 可変長入力は `variadic_input_group`
- **幾何ベクタは 1 パラメータで宣言する。** `center_x` / `center_y` のような
  Float 2 本ではなく `ParameterValue::vec2` / `vec3`（= `Channel2` /
  `Channel3`）を使う。理由は 3 つ: Properties が成分横並びの Vector 行 1 本に
  なる、`expose_param_port` が VEC2 / VEC3 の 1 ポートで受けられる、
  `with_param_range` が成分共通の 1 宣言で済む。3D 対応が来る見込みの
  パラメータは最初から `Channel3` にする（`.ravprj` の移行が 2 回走らない）。
  読み出しは `params.vec2_or(key, default)` / `vec3_or`。
  **成分ごとの `Int` 対は畳まない**（`Channel2` は float チャネルの対なので
  型の意味が変わる）
- **構造的な値を文字列に押し込まない。** カーブは
  `ParameterValue::Curve(CurveParam)`（`ravel_core::param_curve`）、パスは
  `PathPoints` を使う。文字列にすると Properties が手打ちのテキスト欄になり、
  後から型を変えるのに `.ravprj` の移行が要る（`field.curve_remap` の
  `points` が実際にそうなった。format v6）。読み出しは
  `params.curve(key)` / `params.path_points(key)`。この 2 種は wire 型を
  持たない（`port_data_type()` が `None`）のでパラメータポートに露出できない
- **あるパラメータが別のパラメータの型を決めるなら**、その対応を
  `registry::builtin::dependent_param_updates` に足し、書き込み経路が
  `Graph::set_params` を通るようにする（`attribute.set` の `value` が `type` に
  従う形）。値とポート型が 1 回の呼び出しで変わるので、Document スナップショット
  = undo 単位が保たれる
- パラメータポートが受ける wire 型は `ParameterValue::port_accepted_types()`
  が決める（**集合**。`Channel4` は `[COLOR, VEC4]`）。`port_data_type()` は
  ポート色などで 1 つの型が要る場面のための**主型**なので、接続可否や
  「値の変更でポートが無効になるか」の判定には使わない

## 2. プロセッサを実装する

`crates/ravel-nodes/src/` の該当領域（`field/`、`shape/`、`comp/`、`math.rs` …）に
`NodeProcessor` を実装する。

```rust
impl NodeProcessor for NoiseFieldProcessor {
    fn process(
        &self,
        node: &Node,
        ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> { ... }
}
```

守るべき不変条件:

- **パラメータ値を構造体に capture しない。** 評価器がフレームごとに
  `ResolvedParams` へ解決して渡す。capture すると「パラメータを編集したら
  プロセッサを作り直す」ことになり、編集ごとの再構築が発生する
  （現在はパラメータ編集は dirty マークだけで済んでいる）
- `is_time_dependent()` は**出力がフレームに依存するときだけ** `true`。
  アニメーション付きパラメータを持つノードは評価器側が時間依存として扱うので、
  ここで `true` を返す必要は無い。定数生成器で `true` を返すとキャッシュが効かない
- 入力の欠落（`None`）を必ず扱う。未接続で panic させない
- ネストしたグラフ（ネットワーク境界 / Subnet）や Document 参照が必要なときは
  `scope` を使う。自前で評価器を作らない
- エラーは `anyhow::Result` で返す。`unwrap` で落とさない（評価はワーカースレッド上）

## 3. 配線する

`crates/ravel-nodes/src/lib.rs` の `processor_for_node` に分岐を足す。

```rust
"field.noise" => Some(Arc::new(field::NoiseFieldProcessor::from_node(node))),
```

`register_all_processors` が Graph（と `subnet` の中）を走査してこの関数を呼ぶ。
`None` を返す `type_key` はプラグイン空間として扱われ、評価されない。

## 4. GPU 版を足す場合

CPU 実装を先に置き、GPU はその**同一結果の高速経路**として足す。

- コンストラクタで `GpuContext` / `&mut ShaderManager` / `Arc<Mutex<TexturePool>>`
  を受け取る（`processor_for_node` がすでに持っている。デコード済みフレームを
  読むノードなら `&MediaFrameCache` も同様）
- WGSL は `crates/ravel-nodes/src/shaders/*.wgsl`
- 中間テクスチャと常駐出力は `TexturePool` から取る。`GpuFrameBuffer` は
  drop でテクスチャを返す
- `TextureKey` は `ravel_gpu::TextureFormat` / `TextureUsage` で書く
  （wgpu の `TextureFormat` / `TextureUsages` を渡さない）。rw 中間テクスチャの
  既定は `gpu_util::tex_key_rw(width, height)`
- ディスパッチは `GpuContext::dispatch_compute(&ComputeDispatch { .. })` 1 回で
  書く（`blur.rs` が代表形）。`create_bind_group` / `create_buffer_init` /
  `create_command_encoder` / `queue().submit` をプロセッサから直接呼ばない。
  テクスチャは `PooledTexture::binding()` / `GpuFrameBuffer::binding()`（入力は
  `GpuImage::binding()`）で `TextureBinding` として渡す
- バインド順は契約: 入力が `@binding(0..N)`、出力ストレージテクスチャが
  `@binding(N)`、ユニフォームが `@binding(N+1)`。`BindingDesc` のレイアウトと
  WGSL をこの順で宣言する。パラメータの無いパスは `uniform: &[]` を渡し、
  `@binding(N+1)` を宣言しない（`rasterize.wgsl` の `unpremultiply` がその例）
- **描画パスも同じ形で書く。** `GpuContext::draw_quads(&QuadDraw { .. })` に
  渡す（`rasterize/mod.rs` が唯一の例）。パイプラインは `RasterPipeline::new`
  で作り、カラーアタッチメントは `ColorTarget::new(TextureFormat, BlendMode)`
  で書く。バインド順の契約はユニフォームが `@binding(0)`、読み取り専用
  ストレージバッファが `@binding(1..N+1)`。描画も同じフレーム共有エンコーダに
  載るので、submit も flush 点も compute と同じ
- ユニフォームは内容をキーに、バインドグループは（パイプライン, テクスチャ,
  ユニフォーム）の同一性で自動的に再利用される。記録はフレーム共有エンコーダに
  載り、submit はリードバック（アプリではビューア境界の 1 フレーム 1 回）などの
  flush 点でまとめて起きる
- 一時テクスチャの `pool.release` / `GpuImage::release` は記録直後に呼んでよい。
  未 flush のバッチが使うテクスチャはプールが再利用を差し止める
- **アルファ規約を揃える**（既存シェーダは straight alpha。混ぜると合成結果が
  変わる）
- **WGSL を 1 本足したら変換テストが増える。** `gpu_util` の
  `shader_translation` テストが `crates/ravel-nodes/src/shaders/` と
  `crates/ravel-gpu/src/shaders/` を走査し、全ファイルが MSL / HLSL / SPIR-V へ
  変換できることを見る（`ravel_gpu::translate`）。ファイルは自動で拾われるので、
  足したら `SHADER_COUNT` を更新して変換が通ることを確認する。
  `premultiplied.wgsl` の前置が必要なファイルは
  「Prepend `premultiplied.wgsl`」のコメント行を残す — テストの合成判定が
  この行を見ているので、消すと未合成の断片を検証してしまう
- 合成チェーンの synthetic ノードは CPU 参照経路に固定している箇所がある
  （ゴールデンテストが既存のピクセルを固定しているため）。`rasterize` の分岐が
  その例
- **CPU / GPU 等価性テストを必ず追加する**。`crates/ravel-nodes/tests/` の
  既存テスト（`gpu_resident_pipeline.rs`、`shape_layer_golden.rs`）と同じ形

## 5. テスト

- プロセッサの単体テストは実装と同じファイルの `#[cfg(test)]`
- 評価器を通した確認は `crates/ravel-nodes/tests/`
- ゴールデン画像は増やさない。数値で検証できるものは数値で

## 設計原則

- **固定機能のリピーターやスタイル専用ノードを作らない。** 属性とフィールドの
  合成で表現する（[`../specifications/procedural-geometry.md`](../specifications/procedural-geometry.md)）
- 型は `DataTypeId` の既存集合で表す。新しいデータ型を足すのは別の判断
  （`NodeData` の実装と `match` の網羅が全域に波及する）
- **カテゴリも既存集合から選ぶ。** `NodeCategory` を足すのは別の判断で、
  enum・カテゴリ色・カテゴリ既定アイコン・メニュー順・ロケールキーが連動する
  （波及先の一覧は [`../agent-api-reference.md`](../agent-api-reference.md) の
  `registry` 節）
- ノード 1 個で解けないものを 1 個に詰め込まない。組み合わせで解く
- **画素の値はリニア光である。** プロセッサが受け取る `FrameBuffer` も
  `COLOR` パラメータも作業空間（リニア Rec.709）で、伝達関数はすでに外れて
  いる。ノードの中でガンマを掛けたり外したりしない — 変換点は入力・表示・
  出力の 3 つだけ
  （[`../specifications/color-management.md`](../specifications/color-management.md)）。
  色を扱うパラメータは `Channel4` で宣言し、`VEC4` のベクタと区別できるように
  する（`.ravprj` の移行がポート宣言型で色を見分ける）

## パラメータが外部契約に出られるか (REQ-PROJ-006)

Properties のパラメータ行には**公開トグル（□ / ■）**があり、押すとその
パラメータがプロジェクトの公開パラメータ宣言になる（CLI の `--param`、
サブグラフテンプレートの公開入力）。**ノード側に書くことは何も無い**が、
どのパラメータにトグルが出るかは `ParameterValue` の種別で決まる。

| `ParameterValue` | 宣言される型 |
|---|---|
| `Float` / `Int` / `Bool` / `String` | 同じ定数型 |
| `Channel` | `float` |
| `Channel2` / `Channel3` | `vec2` / `vec3` |
| `Channel4` | `color`（Properties が色として描くもの） |
| `media` ノードの `asset_id` | `media`（素材差し替え） |
| `PathPoints` / `Curve` | **対象外**（トグルを出さない） |

対応は `ravel-core::exposed::apply::seed_value` の 1 箇所だけが持つ。
**UI 側で種別判定を書き足さないこと** — 2 つ目の対応表ができた瞬間、
`apply` が書き戻せない宣言を作れるようになる。`PathPoints` / `Curve` が
外れているのは内部表現を外部契約に露出させないため
（[`../specifications/data-model.md`](../specifications/data-model.md) の
公開パラメータ宣言モデル）。
