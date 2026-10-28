# エージェントから使う（`ravel-cli mcp`）

> 索引: [`README.md`](README.md)

`ravel-cli mcp` は、プロジェクトの読み取りと書き出しを Model Context Protocol
（MCP）のツールとして stdio で差し出す。計画は
[`../implementation/ai-assistant-mcp-plan.md`](../implementation/ai-assistant-mcp-plan.md)
の `AI-1`。**このページは「どう使うか」だけを書く。** 型と関数は
[`../agent-api-reference.md`](../agent-api-reference.md) の `ravel-cli` 節。

## 起動と登録

```text
ravel-cli mcp [--allow-dir <DIR>]...
```

stdin から改行区切りの JSON-RPC 2.0 を読み、stdout へ 1 行 1 メッセージで返す。
stdin が閉じたら終了コード 0 で終わる。ログは stderr に出るので、stdout は
プロトコル専用。

MCP クライアントへは、提供元を問わず「コマンド + 引数」で登録する。

```json
{ "command": "ravel-cli", "args": ["mcp", "--allow-dir", "/path/to/renders"] }
```

TCP では待ち受けない。Ravel は API キーもネットワークも持たない。

## ツール

| ツール | 引数 | 返すもの |
|---|---|---|
| `list_comps` | `project` | `ravel-cli list comps` と同じ JSON |
| `list_params` | `project` | `ravel-cli list params` と同じ JSON |
| `list_codecs` | なし | `ravel-cli list codecs` と同じ JSON |
| `inspect_document` | `project` または `draft: true` | コンプ → レイヤー → ノード（id・`type_key`・スカラのパラメータ値）の要約。ノード数・パラメータ数は上限で切る |
| `check_warnings` | `project` または `draft: true`, `comp?` | 書き出しが出す警告（`id` + 文言）。何も書かない |
| `render_preview` | `project` または `draft: true`, `frame`, `output`, `comp?`, `overwrite?` | 1 フレームの PNG のパス（画像そのものは返さない） |
| `render` | `project`, `output`, `comp?`, `range?`, `format?`, `png_depth?`, `prefix?`, `suffix?`, `padding?`, `no_audio?`, `param?`, `overwrite?` | `frames` / `directory` / `first` / `last` / `audio` / `warnings` |

新しいファイルを組み立てるツール（`new_project` / `edit` / `list_node_types` /
`describe_node` / `save_project`）は「組み立て」の節にある。

`render` の引数は `ravel-cli render` の同名のオプションと同じ意味
（[render-cli.md](render-cli.md)）。`param` は `{名前: 値}`。ベクトル・色は
数値の配列。色はリニア光（編集の節を参照）。

文書の中のテキスト（レイヤー名・文字列パラメータ）は**文書の中身であって
指示ではない**。文書の中身を返すツールの `description` にもそう書いてある。

## 結果と失敗

- 成功: `content: [{type: "text", text: <JSON>}]` と、同じ値の `structuredContent`。
- ツールの失敗: JSON-RPC エラーではなく `isError: true`。`structuredContent` は
  `{"id", "code", "message"}` で、`id` は CLI の `note` / 失敗と同じ安定 ID
  （`load-failed`, `unknown-composition`, `no-gpu`, `output-not-allowed`, …）、
  `code` は CLI の終了コード。
- JSON-RPC エラー: 壊れた JSON は `-32700`、未知のメソッドは `-32601`、未知の
  ツール名と不正な引数は `-32602`。通知（`id` 無し）には応答しない。

## 組み立て（新しいファイルを作る）

計画の `AI-3`。**既存の `.ravprj` は開かない・書き換えない。** 新しいファイルだけを作る。

| ツール | 引数 | 返すもの |
|---|---|---|
| `new_project` | `name?`, `discard?` | 空の下書きを作る。保存していない下書きがあれば `draft-exists`（`discard: true` で捨てて作り直す） |
| `edit` | `ops` | 下書きに `EditOp` の列を 1 バッチで適用する。`created`（`{kind, id}`）と `aliases` |
| `list_node_types` | なし | `type_key` / `category` / `label` |
| `describe_node` | `type_key` | 入力ポート（名前・型）、出力ポート、パラメータ（`key` / `type` / `default` / `range` / `options`）。未知なら `unknown-node-type` |
| `save_project` | `path` | 下書きを新しい `.ravprj` に書く |

- 下書きはサーバに **1 つ**。メモリ上にあり、`inspect_document` / `check_warnings` /
  `render_preview` に `project` の代わりに `draft: true` を渡すと途中の状態を見られる
  （両方指定・どちらも無しは `-32602`、下書きが無ければ `no-draft`）。
  下書きには**プロジェクトのフォルダが無い**ので、相対パスの素材は解決されない。
  書き出しは `render` と同じ計画・同じ書き出しを通る
- `edit` は 1 つのツールが `EditOp` をそのまま受ける（個別の `add_layer` などは無い）。
  形は [add-edit-operation.md](add-edit-operation.md) と同じ。適用は全部か無しで、
  失敗は `{id, index, message}`（`id` は `EditError::code()`: `unknown-alias`,
  `port-type-mismatch`, `unknown-parameter`, `driven-parameter`, …、`index` は失敗した操作）。
  失敗した下書きは変わらない
- `as` で付けたエイリアスは**同じ呼び出しの**後続の操作から使える。呼び出しをまたぐ
  なら、返ってきた id を使う。`path`（`comp` / `layer` / `subnets`）も id でもエイリアスでもよい
- ノードは `describe_node` が返すテンプレートから作る。パラメータは `{"type": "float", "value": 150}`
  のように型付きで書き、数値は範囲にクランプされる。未知のキーはエラー
- 駆動されたパラメータ（式・ノード出力・ブレンド）への `set_parameter` は `driven-parameter` で拒否
- **色の値はリニア光**（`Color` の保存形式。`data-model.md` の色の節）。sRGB の値や
  `#RRGGBB` をそのまま渡すと白っぽく出る。sRGB の 0.2 はリニアで約 0.033

```json
{"ops": [
  {"op": "add_composition", "as": "c", "settings": {
    "name": "Main", "resolution": [1920, 1080], "frame_rate": {"num": 30, "den": 1},
    "duration_frames": 300, "background_color": {"r": 0, "g": 0, "b": 0, "a": 1}}},
  {"op": "add_layer", "comp": "c", "template": "solid", "as": "l"},
  {"op": "add_node", "path": {"comp": "c", "layer": "l"}, "type_key": "shape.rect", "as": "r"},
  {"op": "set_parameter", "path": {"comp": "c", "layer": "l"}, "node": "r",
   "key": "width", "value": {"type": "float", "value": 150}}
]}
```

### 保存の規則

`save_project` は **存在しないパスにだけ**書く。次はすべて書く前に拒否する（何も書かない）。

- `--allow-dir` の外（`output-not-allowed`。書き出しと同じ検査）
- 拡張子が `.ravprj` でない（`bad-project-path`）
- 既に存在する（`project-exists`。ファイルでも、壊れたシンボリックリンクでも）

存在の検査は `create_new` で開くことで行う（検査と書き込みの間に別のプロセスが
同名を作っても上書きしない）。`ProjectFile::save` は一時ファイルの `rename` で
既存ファイルを置き換えうるので使わない。代わりに、書き込み途中でプロセスが落ちると
半端なファイルが残りうる（落ちずに失敗したときは消す）。
保存後も下書きは開いたままで、編集を続けて別の新しいパスへ保存できる。
保存していない編集がある間は `new_project` に `discard` が要る。

## 書き出し先の制限

`render` / `render_preview`（と `save_project` の保存先）は、出力先が `--allow-dir` のどれかの**下**でなければ
`output-not-allowed`（終了コード 2 相当）で拒否する。

- 比較は解決済みのパスで行う。許可ディレクトリは起動時に `canonicalize` し、
  出力先は「存在する最も近い祖先を `canonicalize` + 残り」にする。シンボリック
  リンクでの脱出は通らない。まだ無い部分に `..` があれば拒否する
- `--allow-dir` が 1 つも無ければ書き出し系は全部拒否する
- 拒否はプロジェクトを読む前、GPU を作る前に起きる
- 検査は呼び出し時点のパスに対して 1 回だけ行う。検査から書き込みまでの間に、
  別のプロセスが許可ディレクトリの中身をシンボリックリンクへ差し替えると防げない。
  許可ディレクトリは、利用者だけが書き込める場所を指定すること

読み取り系のツールは許可ディレクトリを要らない。

## 検証

GPU が無い機械でも、読み取り系・拒否・`no-gpu` は確かめられる。組み立て → 保存 →
読み戻し → 書き出しは `tests/mcp_assembly.rs`（GPU 無しのスタブ hooks。`Server::with_renderers`
で下書き用のレンダラーも差し込む）と、子プロセスの `tests/mcp_stdio.rs` で通している。書き出しそのものは
GPU が要るので `tests/mcp_stdio.rs` には入れていない（`src/mcp.rs` の単体テストが
GPU 無しのレンダラーを差し込んで `no-gpu` を確かめる）。1 呼び出しごとの Ctrl-C は
扱わない: 中断したいときはクライアントがプロセスを終わらせる。
