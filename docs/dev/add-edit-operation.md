# 編集操作（`EditOp`）を追加する

> 索引: [`README.md`](README.md)

文書を変える操作は `ravel_core::edit` の `EditOp` として足す。UI もエージェントも
同じ入口を通るので、**型付きのエラー、バッチ単位の all-or-nothing、1 バッチ =
undo 1 段**が操作ごとに実装し直さずに手に入る（計画:
[`edit-operations-plan.md`](../implementation/edit-operations-plan.md)）。

## チェックリスト

- [ ] `crates/ravel-core/src/edit/ops.rs` の `EditOp` に variant を追加。JSON の形は
      `#[serde(tag = "op", rename_all = "snake_case")]` が決める。対象の参照は
      `CompRef` / `LayerRef` / `NodeRef`（数 = id、文字列 = エイリアス）、ポートは
      `PortRef`（数 = ポート番号、文字列 = ポート名）、
      ネットワーク内の操作は `path: NetworkPath` を取る
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
      1 回 = undo 1 段。何も変えないバッチは段を作らない）。選択全体への書き込みの
      ように一部の拒否で全体を止めたくないときだけ `edit_ops_skipping_refused`。
      `ravel-app` から `Graph::add_edge` などを直接呼んで文書を組み替えない
- [ ] [`../agent-api-reference.md`](../agent-api-reference.md) の `edit` 節に、
      操作の JSON 例と注意（クランプ、置換、エラーになる条件）を足す
- [ ] `mise run check`

## 忘れやすいもの

- **ドラッグ中の毎フレームの更新は `EditOp` にしない**（`apply_document` の仕事）。
  確定時に 1 操作として `commit_edit_ops` へ渡す
  （[`ux.md`](../../.agents/rules/ux.md) の「1 操作 = undo 1 段」）
- パスは id で層を指すので、**同じバッチ内で作った層のネットワークは `NetworkPath`
  で指せない**（エイリアスは `CompRef` / `LayerRef` / `NodeRef` にだけ効く）。
  層の作成とその中身の編集は別のバッチになる
- 文書を変えない `Ok` はありうる（同じ値の書き込みなど）。`commit_edit_ops` は
  結果が元と等しければ commit しない。段を作りたい呼び出し側（スクラブの確定）は
  `edit_ops_skipping_refused` の `commit` を使う

## 関連

- コマンドが文書を編集するなら、ハンドラはこの層を呼ぶ:
  [add-command.md](add-command.md)
- テストの置き場所: [testing.md](testing.md)
