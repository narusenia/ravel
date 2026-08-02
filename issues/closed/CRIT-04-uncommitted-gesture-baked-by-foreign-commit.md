# [CRIT-04] 未コミットのペン/ドラッグ状態が他パネルのコミットで焼き込まれ、Esc が無効化される

| 項目 | 内容 |
| --- | --- |
| 深刻度 | critical |
| 種別 | bug |
| 領域 | ravel-app / Viewer, ravel-ui / DocumentStore |
| 該当 | `crates/ravel-app/src/panels/viewer.rs:1014-1050`, `:1153-1184`, `crates/ravel-ui/src/document.rs:107-123` |

> **解決済み**: フェーズ A2。キャンセルは `revert_document` に依存しなくなり、
> ジェスチャー開始時点の `original_document` スナップショットを
> `restore_document_snapshot` で戻す（`crates/ravel-app/src/panels/viewer.rs:745-759`。
> `cancel_shape` / `cancel_handle_drag` も同型）。回帰テストは
> `cancelling_a_move_discards_a_foreign_commit_of_its_preview`（`:4110`）。

## 現状

ペンツールの最初のクリックは構造変更（多くの場合 Shape レイヤーの自動生成を含む）を
`apply_document` し、クリック間で未コミットのまま保持される。

この間に他パネルが `commit_document`（例: Properties の編集）を行うと、ライブドキュメント全体が
そのままコミットされ dirty フラグがクリアされる。その後 Esc を押すと `revert_document` が
`false` を返して何も起きず、余計なレイヤーと1点だけのパスノードがドキュメントに残り、保存される。

Viewer の move / shape / path-edit ドラッグの Esc キャンセルも同じ機構で壊れる。
エージェントによる end-to-end 検証済み。

## 影響

ユーザーが取り消したつもりの操作が不可視の形でドキュメントに残留し、そのまま永続化される。
ドキュメント汚染 = 実質的なデータ破損。ジェスチャー中に他パネルを触るだけで発生し、再現条件が緩い。

## 修正方針

ジェスチャーのキャンセルを `revert_document` に依存させない。
ジェスチャー開始時点のスナップショットをジェスチャー状態に保持し、キャンセル時はそれを復元する
（またはジェスチャーが生成したノードを特定して除去する）。

## 検証

- ペンセッション中に Properties コミット → Esc で追加ノード・レイヤーが残らないテスト
- move / shape / path-edit ドラッグ中の外部コミット → Esc 復元テスト

## 関連

- [medium/app-shell.md](../medium/app-shell.md) — no-op undo ステップ、ジェスチャー終了時コミットの周辺問題
