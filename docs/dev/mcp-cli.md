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
| `inspect_document` | `project` | コンプ → レイヤー → ノード（id・`type_key`・スカラのパラメータ値）の要約。ノード数・パラメータ数は上限で切る |
| `check_warnings` | `project`, `comp?` | 書き出しが出す警告（`id` + 文言）。何も書かない |
| `render_preview` | `project`, `frame`, `output`, `comp?`, `overwrite?` | 1 フレームの PNG のパス（画像そのものは返さない） |
| `render` | `project`, `output`, `comp?`, `range?`, `format?`, `png_depth?`, `prefix?`, `suffix?`, `padding?`, `no_audio?`, `param?`, `overwrite?` | `frames` / `directory` / `first` / `last` / `audio` / `warnings` |

`render` の引数は `ravel-cli render` の同名のオプションと同じ意味
（[render-cli.md](render-cli.md)）。`param` は `{名前: 値}`。ベクトル・色は
数値の配列。

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

## 書き出し先の制限

`render` / `render_preview` は、出力先が `--allow-dir` のどれかの**下**でなければ
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

GPU が無い機械でも、読み取り系・拒否・`no-gpu` は確かめられる。書き出しそのものは
GPU が要るので `tests/mcp_stdio.rs` には入れていない（`src/mcp.rs` の単体テストが
GPU 無しのレンダラーを差し込んで `no-gpu` を確かめる）。1 呼び出しごとの Ctrl-C は
扱わない: 中断したいときはクライアントがプロセスを終わらせる。
