# 編集操作（`EditOp`）を追加する

> 索引: [`README.md`](README.md)

文書を変える操作は `ravel_core::edit` の `EditOp` として足す。UI もエージェントも
同じ入口を通るので、**型付きのエラー、バッチ単位の all-or-nothing、1 バッチ =
undo 1 段**が操作ごとに実装し直さずに手に入る（計画:
[`edit-operations-plan.md`](../implementation/done/edit-operations-plan.md)）。

## チェックリスト

- [ ] `crates/ravel-core/src/edit/ops.rs` の `EditOp` に variant を追加。JSON の形は
      `#[serde(tag = "op", rename_all = "snake_case")]` が決める。対象の参照は
      `CompRef` / `LayerRef` / `NodeRef`（数 = id、文字列 = エイリアス）、ポートは
      `PortRef`（数 = ポート番号、文字列 = ポート名）、
      ネットワーク内の操作は `path: NetworkRef` を取る（`comp` / `layer` / `subnets` が
      id でもエイリアスでもよい。`NetworkPath` からは `.into()`）
- [ ] 何かを作る操作なら `#[serde(rename = "as", default, skip_serializing_if =
      "Option::is_none")] alias: Option<String>` を持たせる（既存の
      `AddComposition` / `AddLayer` / `AddNode` と同じ属性）。新しい種類のものを
      作るなら `edit/mod.rs` の `Created` に variant と、`resolve_*` 関数、
      `AliasKindMismatch` の経路も足す
- [ ] `edit/mod.rs` の `step` に腕を追加（網羅 `match`。足さないとコンパイルエラー）。
      作る操作は**作業を始める前に** `check_alias`、成功したら `define_alias`。
      参照は `resolve_comp` / `resolve_layer` / `resolve_node` で解く。
      ネットワークの書き換えは `edit_network`、グラフ単位の処理は
      `edit::network` / `edit::param` / `edit::document` の既存関数を呼ぶ
      （UI からも使う関数をここに複製しない）
- [ ] `EditOp::is_destructive()` に腕を追加（何かを消すなら `true`。分類だけで、
      確認を取るのは呼び出し側）。`edit/tests.rs` の `only_removals_are_destructive`
      に新 variant を足す（網羅 `match` は腕の有無しか守らず、分類の誤りは見ない）
- [ ] 失敗の種類が既存の `EditError` に無ければ `edit/error.rs` に variant を足し、
      **`code()` にも**足す（`code()` は翻訳しない安定した識別子。変えない）
- [ ] `edit/tests.rs`: 失敗の全パターンを `refused_operations_leave_the_document_unchanged`
      の表に足す（`before == after`。表が空なら落ちるよう、操作の一覧を先に
      `assert!` する形を崩さない）
- [ ] `edit/tests.rs`: `edit_ops_round_trip_through_json` に新 variant を足す
      （エイリアスあり・なしの両方の JSON を通す）
- [ ] 正常系のテスト。`apply` は成功のたびに `Document::validate()` を走らせるので、
      結果が `validate` を通ることはテストで改めて確認しなくてよいが、**何が変わった
      か**は確認する
- [ ] UI から呼ぶなら `ProjectState::commit_edit_ops`（1 バッチ = `commit_document`
      1 回 = undo 1 段。何も変えないバッチは段を作らない）。ただし `commit_edit_ops` は
      `local_frame: None` で適用するので、**アニメ済みパラメータへの `SetParameter` は
      `AnimatedParameter` で拒否される**。プレイヘッドのフレームにキーを打つ書き込みと、
      選択全体への書き込み（一部の拒否で全体を止めない）は `edit_ops_skipping_refused`
      （フレームを取り、`commit` で undo を積むかを選ぶ。all-or-nothing ではない）。
      `commit_edit_ops` は駆動されたチャンネルへの `SetParameter` を `driven-parameter` で
      拒否し、`edit_ops_skipping_refused` だけが `EditContext::allow_driven_overwrite` を
      立てて従来のスクラブ（定数に置き換える）を保つ。
      `ravel-app` から `Graph::add_edge` などを直接呼んで文書を組み替えない
- [ ] [`../agent-api-reference.md`](../agent-api-reference.md) の `edit` 節に、
      操作の JSON 例と注意（クランプ、置換、エラーになる条件）を足す
- [ ] `mise run check`

## 動きの操作（`set_keyframes` / `clear_animation` / `set_expression` / `set_layer`）の足場

- 対象は `ChannelTarget`（`Parameter {path, node, key}` / `Shell {comp, layer, property}`、
  JSON は操作のフィールドの隣に平らに書く）。UI の `PropertyRowId` の `Network` / `Shell` と同じ区別
- 書き込みは `edit/animation.rs`。チャンネルの解決は UI と同じ関数を通す
  （`Layer::shell_channel_mut`、`ParameterValue::channel_mut`、`set_curve_interpolation`）。
  ここに別の解決を作らない。ベジェのハンドルは `set_curve_interpolation` の種づけ（UI の既定）
- キーの `frame` はコンポのフレーム。`Layer::key_frame_for_comp`（`comp_frame_for_key` の逆）で
  ローカルへ直す。`local_frame` は 0 で頭打ちなので、ここでは使わない
- 駆動されたチャンネルは `driven-parameter`、アニメ済みの殻への定数は `animated-parameter`。
  殻用の variant（`DrivenShellProperty` / `AnimatedShellProperty`）は**同じ `code()`** を返す
  （呼び出し側の対処が同じため）
- `Document::validate` は親子の循環を見ない。`set_layer` の `parent` は `edit/mod.rs` で
  自分自身・循環を `invalid-layer-edit` として拒否する
- 確認は出ない（`is_destructive` は `false`、ライブ口の `first_removal` に入れない）
- `set_expression`（`AI-9`）は同じ対象・同じ `write_channel` を通る。チャンネルの種類ごとの
  書き込みは `ChannelInput`（`Keys` / `Constant` / `Expression`）→ `ChannelWrite` で、**別の
  経路を作らない**
  - コンパイルは `ParameterExpression::new`（Properties パネルと評価器が使うものと同じ。語彙は
    `Scope::parameter_context()`）。ここで別の `compile` / `compile_with` を呼ぶと、受け入れた式を
    評価器が落とす（か、逆）ことになる。UI はコンパイルできない式も保存するが、**この経路は
    `ParameterExpression::error()` を見て `expression-error`（`EditError::InvalidExpression`、
    `line` / `column` 付き）で拒否する**。空・空白のみも拒否（`clear_animation` を使う）
  - 置き換えてよいのは定数・キー・式（`clear_animation` と同じ `removable`）。ノード出力・ブレンドは
    `driven-parameter`。`set_keyframes` は式を駆動扱いで拒否するが、`set_expression` は拒否しない
    （この差を `set_expression_on_a_driven_channel_is_refused_and_changes_nothing` が対で固定している）
  - 識別子のパラメータ（`layer.ref` の `layer` など）は `set_keyframes` と同じく拒否する
  - 式を外す操作は足さない（`clear_animation`）。MCP の `edit` の説明と
    [`mcp-cli.md`](mcp-cli.md) の「式」に操作を書く

## 忘れやすいもの

- **ジェスチャ中の更新は undo を積まない経路で流す**。パラメータの scrub は
  `edit_ops_skipping_refused` に `commit = false`、終わりの 1 回だけ `commit = true`。
  それ以外のドラッグ（ノードの移動など）は `apply_document` で、確定時に 1 段
  （[`ux.md`](../../.agents/rules/ux.md) の「1 操作 = undo 1 段」）
- ネットワークの指定は `NetworkRef` なので、**同じバッチ内で作った層のネットワークも
  エイリアスで指せる**（`{"comp": "c", "layer": "l"}`。数の `path` もそのまま読める）。
  適用時に解決済みの `NetworkPath` になる
- 文書を変えない `Ok` はありうる（同じ値の書き込みなど）。`commit_edit_ops` は
  結果が元と等しければ commit しない。段を作りたい呼び出し側（スクラブの確定）は
  `edit_ops_skipping_refused` の `commit` を使う

## 関連

- コマンドが文書を編集するなら、ハンドラはこの層を呼ぶ:
  [add-command.md](add-command.md)
- テストの置き場所: [testing.md](testing.md)
