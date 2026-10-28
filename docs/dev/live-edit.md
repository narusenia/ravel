# 動作中のアプリを外から編集する（ライブ編集ソケット）

> 索引: [`README.md`](README.md)

同じマシンのエージェントが、開いている文書を編集するための**ローカル接続口**。
計画は [`../implementation/ai-assistant-mcp-plan.md`](../implementation/ai-assistant-mcp-plan.md)
の `AI-4`。実装は `crates/ravel-app/src/live_edit/`（`protocol.rs` が I/O を持たない
プロトコル、`unix.rs` / `windows.rs` が通信路）。**このページは「どう使うか」だけ**で、
型と関数は [`../agent-api-reference.md`](../agent-api-reference.md)。

## 有効にする

既定は**閉じている**。global 設定（`<config>/ravel/settings.toml`）に書く。

```toml
[agent]
live_edit_enabled = true
```

- 値は**起動時に 1 回**読む。実行中に切り替えても口は開閉しない（再起動する）
- **プロジェクト層（`.ravprj` の設定）に書いても無視する。** 他人から届いた
  プロジェクトが口を開けられないようにするため
- 設定画面の項目は無い（設定ファイルだけ）

## 口の場所

| OS | 場所 | 権限 |
|---|---|---|
| macOS / Linux | `<runtime_dir>/ravel/live-edit.sock`（`runtime_dir` が無ければ `cache_dir`） | 親ディレクトリ 0700、ソケット 0600 |
| Windows | 名前付きパイプ `\\.\pipe\ravel-live-edit-<ユーザー SID>` | 現在のユーザーと SYSTEM だけ。リモートクライアント拒否 |

- TCP では待ち受けない
- 既存のソケットファイルに接続できなければ古いものとして消して作り直す。
  接続できれば別の Ravel が使用中なので、開かずに警告ログを出す
- アプリ終了時にソケットファイルを消す

## プロトコル

改行区切りの JSON。1 行 1 要求 → 1 行 1 応答。1 行は 4 MiB まで。

```text
-> {"id": 1, "ops": [<EditOp>, ...]}
<- {"id": 1, "ok": {"created": [{"kind": "layer", "id": 4}]}}
<- {"id": 1, "error": {"code": "layer-not-found", "index": 0, "message": "..."}}
```

- `ops` は [`add-edit-operation.md`](add-edit-operation.md) の `EditOp`（`"op"` タグ）
- 参照は id でもエイリアスでもよい。`as` で付けたエイリアスは同じ要求の後続の操作から、
  ネットワークの指定（`path: {"comp": "c", "layer": "l"}`）を含めて使える
- 1 要求は 1 回の `ProjectState::commit_edit_ops`。成功すれば undo が 1 段増え、
  Cmd+Z で戻る。何も変えない要求（空の `ops` を含む）は undo を積まない
- 失敗は全体が適用されない。`code` は `EditError::code()`、`index` は失敗した操作
- 壊れた JSON・形の違う要求は `{"id": null, "error": {"code": "bad-request", ...}}`

## 拒否される操作

`RemoveComposition` / `RemoveLayer` / `RemoveNode` を 1 つでも含む要求は、
**全体を** `confirmation-required` で拒否し、文書を変えない。確認ダイアログ
（`AI-5`）ができるまでの措置。`Disconnect` は undo で戻せるので通す。
