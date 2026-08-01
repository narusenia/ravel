# ノードを追加する

> 索引: [`README.md`](README.md)

新しいノード型（`type_key`）を 1 つ足す手順。規約は
[`.agents/rules/rust.md`](../../.agents/rules/rust.md)、型と関数の地図は
[`../agent-api-reference.md`](../agent-api-reference.md)。

## チェックリスト

- [ ] `crates/ravel-core/src/registry/builtin.rs` に `NodeTemplate` を追加
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
- `label` は現在英語リテラル。ロケールキー化は
  [`node-discoverability-plan.md`](../implementation/node-discoverability-plan.md)
  の `DISC-1` で入る（そのときこの引数の扱いが変わる）
- アイコンは現在どこにも無い。種別ごとのアイコンは同じ計画の `DISC-5` で
  入る（対応表は UI 側の `RavelIcon`、`NodeTemplate` にフィールドは増えない）
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
  を受け取る（`processor_for_node` がすでに持っている）
- WGSL は `crates/ravel-nodes/src/shaders/*.wgsl`
- 中間テクスチャと常駐出力は `TexturePool` から取る。`GpuFrameBuffer` は
  drop でテクスチャを返す
- **アルファ規約を揃える**（既存シェーダは straight alpha。混ぜると合成結果が
  変わる）
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
- ノード 1 個で解けないものを 1 個に詰め込まない。組み合わせで解く
