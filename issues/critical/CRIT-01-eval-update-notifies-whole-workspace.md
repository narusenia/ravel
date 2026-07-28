# [CRIT-01] 評価結果ごとに全パネルが再構築される（もっさりの主因）

| 項目 | 内容 |
| --- | --- |
| 深刻度 | critical |
| 種別 | perf |
| 領域 | ravel-app / ProjectState |
| 該当 | `crates/ravel-app/src/project_state.rs:960` |

> **解決済み**: PR #191（2026-07-28）。`on_eval_update` の `cx.notify()` を削除し、NodeEditor は `NodeEvalTimings` を直接購読する形にした。
> 設計は `docs/implementation/done/ui-responsiveness-plan.md`。

## 現状

`ProjectState::on_eval_update` は評価結果を `ViewerFrame` グローバルへ publish した後、
さらに `cx.notify()` を呼ぶ。Viewer は既に `ViewerFrame` を購読済み
（`crates/ravel-app/src/panels/viewer.rs:275`）なので、この notify は Viewer 更新には不要。

一方 `ProjectState` の observer には以下が並ぶ。

- Timeline `sync_from_project` — `Composition` の deep compare 後、無条件 `cx.notify()`
  (`panels/timeline.rs:280`, `:400`)
- NodeEditor `sync_from_project` — document clone + `Graph` deep compare + 無条件 notify
  (`panels/node_editor.rs:504`, `:743-746`)
- Outliner `rebuild_rows` — 全コンポジション・全レイヤー・全ノードを走査して行を再構築
  (`panels/outliner.rs:97`, `:146-168`)
- MediaBin `rebuild_rows` (`panels/media_bin.rs:78`)
- Properties `refresh_values_checked` — 全セクション再構築 (`panels/properties.rs:579-587`)

## 影響

再生中は評価結果が毎フレーム（30〜60Hz）到着する。ドキュメントは一切変わっていないのに、
上記5パネルすべてがモデル再構築 + 再レンダーを毎フレーム実行する。
他のすべてのレンダーコストがこの回数分だけ倍率で乗るため、体感「もっさり」の系統的な最大要因。

## 修正方針

`on_eval_update` から `ProjectState` observer への notify を外す。
エンティティ notify は実際のドキュメント変更（`document_changed`）に限定し、
「ドキュメント変更」と「評価結果到着」を別チャネルに分離する。

## 検証

- 再生中に各パネルの `render` 呼び出し回数を計測し、評価結果到着では増えないことを確認
- 単発の評価結果 publish で Viewer だけが更新されるテスト

## 関連

- [HIGH-07](../high/HIGH-07-document-changed-cascade-per-mouse-move.md) — ドラッグ中の同種カスケード
- [medium/ui-rendering.md](../medium/ui-rendering.md) — 各パネル側の無条件再構築
