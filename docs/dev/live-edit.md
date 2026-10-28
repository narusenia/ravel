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

## 削除は確認ダイアログで承認されるまで保留される

`RemoveComposition` / `RemoveLayer` / `RemoveNode` を 1 つでも含む要求は、拒否も
適用もせず**確認待ち**にする（`AI-5`）。確認は MCP クライアントの画面ではなく
Ravel の画面で取る（クライアントの自動承認に左右されないため）。

- `LiveEdit` がウィンドウを持たないので、`LiveEditEvent::ConfirmRemoval` を
  `EventEmitter` で出し、ウィンドウを持つ `RavelWorkspace` が購読してダイアログを開く。
  Global にも `render()` にも載せない
- ダイアログは消すものを列挙する（コンプ名・レイヤー名・ノードの種類と id）。名前は
  文書の中身なので、文字として表示するだけ
- **承認** → 1 回の `commit_edit_ops`（undo 1 段）で適用して `ok` を返す
- **拒否** → 文書を変えず `confirmation-denied`（`index` は最初の削除の位置）。
  Escape・閉じるボタン・ほかのダイアログが開いていた・購読者がいない、も同じ。
  答えは 1 回きりの送信側で、落ちた（答えなかった）ことが「拒否」になるので、
  拒否を忘れずに書く経路は無い
- 待っている間、その接続のスレッドは応答を待つ。**ほかの接続の要求は順番に待つ**
  （受け付けは 1 要求ずつ）。確認待ちに終わりは無い（閉じるまで）
- 確認待ちの間に文書が変わった（利用者が編集した・別のプロジェクトを開いた）場合は、
  承認されても適用せず `document-changed` を返す。質問したときと同じプロジェクト（`ProjectState::generation`）で文書も一致するときだけ適用する。
  開き直した同じ内容の文書は別物として扱う
- `Disconnect` は確認しない。`EditOp::is_destructive()` は分類で、確認の条件ではない
  （条件は `protocol::first_removal`）
- 「上書き」（既存ファイルへの上書き書き出し、保存先の変更）を確認する対象は、
  今の `EditOp` に**無い**。その操作ができたときに同じ経路へ足す
- `set_keyframes` / `clear_animation` / `set_layer` も確認しない（undo で戻る）。駆動されたチャンネルへの
  `set_keyframes` は `driven-parameter` で拒否する
- 駆動されたパラメータへの書き込みは、従来どおり `driven-parameter` で拒否する

## 呼ぶ側

`ravel-cli mcp --attach` の `live_edit` ツールが、この口へつなぐ（
[`mcp-cli.md`](mcp-cli.md)）。口の場所は `ravel_project::paths::live_edit_endpoint()`
の 1 か所で決まり、アプリと CLI の両方がこれを呼ぶ。
