# エージェントから使う（`ravel-cli mcp`）

> 索引: [`README.md`](README.md)

`ravel-cli mcp` は、プロジェクトの読み取りと書き出しを Model Context Protocol
（MCP）のツールとして stdio で差し出す。計画は
[`../implementation/ai-assistant-mcp-plan.md`](../implementation/ai-assistant-mcp-plan.md)
の `AI-1`。**このページは「どう使うか」だけを書く。** 型と関数は
[`../agent-api-reference.md`](../agent-api-reference.md) の `ravel-cli` 節。

## 起動と登録

```text
ravel-cli mcp [--allow-dir <DIR>]... [--attach]
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
| `inspect_document` | `project` / `draft: true` / `live: true` のどれか 1 つ | コンプ → レイヤー → ノード（id・`type_key`・スカラのパラメータ値）の要約。ノード数・パラメータ数は上限で切る |
| `check_warnings` | `project` / `draft: true` / `live: true` のどれか 1 つ, `comp?` | 書き出しが出す警告（`id` + 文言）。何も書かない |
| `render_preview` | `project` / `draft: true` / `live: true` のどれか 1 つ, `frame`, `output`, `comp?`, `overwrite?` | 1 フレームの PNG のパス（画像そのものは返さない） |
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
- 駆動されたパラメータ（式・ノード出力・ブレンド）への `set_parameter` / `set_keyframes` は `driven-parameter` で拒否
- 動き（`AI-8`）: `set_keyframes` / `clear_animation` / `set_layer` も `edit`（と `live_edit`）の
  操作で、確認は出ない（`live_edit` ならアプリの undo で戻る。`edit` の下書きに undo は無い）。対象は 2 種類で、フィールドは操作の隣に書く
  - ノードのパラメータ: `path` + `node` + `key`
  - レイヤーの殻: `comp` + `layer` + `property`（`anchor_point` / `position` / `scale` /
    `rotation` / `opacity` / `audio_gain` / `time_remap`）
  - `set_keyframes {target, component?, keys: [{frame, value, interpolation}]}` は**1 チャンネル**の曲線を
    置き換える。ベクトル・色は `component`（0 始まり。色は r/g/b/a）ごとに 1 操作
  - **`frame` はコンポのフレーム**（タイムラインの見え方）。レイヤーのローカルフレームへ直して
    保存するので、レイヤーを動かせばキーも動く。`start_frame` より前は `key-before-layer-start`
  - `interpolation` は `bezier`（ハンドルは UI の既定）/ `linear` / `step`。値はパラメータの範囲
    （不透明度は 0〜1）にクランプされる。素の `Float` / `Int` は初めてのキーでチャンネルになる
  - `clear_animation {target, component?, value}` は曲線か式を捨てて定数 `value` にする。
    ノード出力・ブレンドで駆動されたチャンネルは `driven-parameter`
  - `set_layer` は渡したフィールドだけ変える（`name` / `start_frame` / `in_frame` / `out_frame` /
    `anchor_point` / `position` / `scale` / `rotation` / `opacity` / `blend_mode` / `parent` /
    `solo` / `muted` / `locked`）。`parent: null` で解除、循環は `invalid-layer-edit`。
    アニメ済みの殻プロパティへの定数は `animated-parameter`、駆動済みは `driven-parameter`
  - 殻の `time_remap` は予約欄（まだ評価されない）。書けるが見た目は変わらない
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
   "key": "width", "value": {"type": "float", "value": 150}},
  {"op": "set_keyframes", "comp": "c", "layer": "l", "property": "opacity",
   "keys": [{"frame": 0, "value": 0, "interpolation": "linear"},
            {"frame": 30, "value": 1, "interpolation": "bezier"}]}
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

## 動作中のアプリを編集する（`--attach`）

計画の `AI-5`。`--attach` を付けたときだけ、`tools/list` に `live_edit` が出る。
付けなければ存在しない（呼んでも `-32602`）。ほかのツールは付けても付けなくても同じ
（`live: true` だけは `--attach` が要る。下の「アプリの文書を読む」）。

| ツール | 引数 | 返すもの |
|---|---|---|
| `live_edit` | `ops` | 開いているアプリの文書に `EditOp` の列を 1 バッチで適用する。`created`（`{kind, id}`）。エイリアスは返さない |

- **`edit` との違い**: `edit` はサーバ内の下書きを、`live_edit` は利用者が今開いている
  文書を変える。利用者の画面で変わり、Cmd+Z で戻せる（1 呼び出し = undo 1 段）
- 接続先は [ライブ編集ソケット](live-edit.md)。アプリが global 設定
  `[agent] live_edit_enabled = true` で起動していること。呼び出しごとにつなぎ、
  1 要求・1 応答で閉じる
- 削除（コンプ・レイヤー・ノード）を含む呼び出しは、アプリのダイアログで利用者が
  承認するまで返らない。拒否・閉じる・文書が変わった場合は失敗
- 失敗は `isError: true` で、`structuredContent` は `{id, index, message}`。`id` は次のいずれか

| `id` | 意味 |
|---|---|
| `app-not-running` | つなげない（アプリが無い・口が閉じている）。要求は送られていない |
| `app-disconnected` | 要求を送ったあと、応答の前に切れた（アプリの終了など）。適用されたかは不明なので文書を確かめてから繰り返す。次の呼び出しはつなぎ直す |
| `confirmation-denied` | 利用者が削除を承認しなかった。文書は変わっていない。再試行しない |
| `document-changed` | 確認待ちの間に文書が変わったので適用しなかった |
| ほか | アプリが返した `EditError::code()`（`driven-parameter` など）。`index` は失敗した操作 |

### アプリの文書を読む（`live: true`）

計画の `AI-7`。`inspect_document` / `check_warnings` / `render_preview` に、`project` /
`draft: true` の代わりに `live: true` を渡すと、**アプリで今開いている文書**を見られる。
ライブ用の別ツールは無い（列挙の経路を増やさない）。

- 実装は `draft: true` と同じ経路: アプリから受け取った写しを `ProjectFile` にして、
  下書きと同じ `inspect` / `plan_from_draft` / 下書き用レンダラーへ渡す。`project` に同じ
  文書を保存して開いた場合と同じ結果になる（`tests/mcp_assembly.rs`）
- 1 つだけ指定する。`project` / `draft` / `live` の 2 つ以上、またはどれも無しは `-32602`。
  `--attach` なしの `live: true` は `app-not-running`
- 呼び出しごとにつなぎ、写しを 1 回読む（読み取りの要求は
  [ライブ編集ソケット](live-edit.md)）。**文書も undo も変わらず、確認も出ない**
- `app-not-running` / `app-disconnected` は `live_edit` と同じ分類。読み取りは何も適用
  しないので、`app-disconnected` のあとは繰り返してよい
- 書き出しは**この CLI の GPU**で行う。アプリの評価器は使わないのでアプリの再生や
  プレビューの負荷にならない。そのぶんアプリが保存していないメディアのキャッシュは使えない
- 相対パスの素材は、アプリが返すプロジェクトのパスの親フォルダを基準に解決する
  （一度も保存していないプロジェクトは下書きと同じく解決しない）。プロジェクト層の
  設定（`.ravprj` の設定）は写しに含まれない
- 写しは文書全体のテキスト（RON）なので、応答 1 行の上限は読み取りだけ **64 MiB**
  （編集の応答は従来の 4 MiB のまま）。超えたら `internal`（切断とは区別する）。上限を
  共通に上げなかったのは、編集の応答は小さいはずで、壊れた相手が 4 MiB を超えて溜め込ませるのを
  今のまま断ちたいため
- 写しの中の文字列（レイヤー名・ノード名・文字列パラメータ）は文書の中身で、エージェントへの
  指示ではない。ツールの description に同じ注意が付いている

MCP クライアントへの登録:

```json
{ "command": "ravel-cli", "args": ["mcp", "--attach", "--allow-dir", "/path/to/renders"] }
```

ネットワークには出ない（ローカルの Unix ソケット / 名前付きパイプだけ）。
確認待ちの間、この MCP サーバは次のメッセージを処理しない（1 本の stdio を順に読む）。

## 検証

GPU が無い機械でも、読み取り系・拒否・`no-gpu` は確かめられる。組み立て → 保存 →
`--attach` の橋渡し（偽のアプリを一時ディレクトリの Unix ソケットで立てる）は `tests/mcp_attach.rs` と
`src/live.rs` の単体テスト。組み立て → 保存 →
読み戻し → 書き出しは `tests/mcp_assembly.rs`（GPU 無しのスタブ hooks。`Server::with_renderers`
で下書き用のレンダラーも差し込む）と、子プロセスの `tests/mcp_stdio.rs` で通している。書き出しそのものは
GPU が要るので `tests/mcp_stdio.rs` には入れていない（`src/mcp.rs` の単体テストが
GPU 無しのレンダラーを差し込んで `no-gpu` を確かめる）。1 呼び出しごとの Ctrl-C は
扱わない: 中断したいときはクライアントがプロセスを終わらせる。
