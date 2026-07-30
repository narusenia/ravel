# 実装者向け手順書

> 索引: [`../README.md`](../README.md)

「〜を追加するには何を触るのか」に答える文書。**各ページの冒頭のチェックリストが
本体**で、忘れると `mise run check` で落ちるものを明示している。

| やりたいこと | ページ |
|---|---|
| ノード型を追加する（CPU / GPU） | [add-node.md](add-node.md) |
| パネルを追加する | [add-panel.md](add-panel.md) |
| コマンド・ショートカット・メニュー項目を追加する | [add-command.md](add-command.md) |
| ユーザーに見える文字列を追加する | [add-locale.md](add-locale.md) |
| 永続化（`.ravprj`）を変更する | [persistence.md](persistence.md) |
| テストをどこに書くか、どう検証するか | [testing.md](testing.md) |
| 着手から PR までの流し方 | [workflow.md](workflow.md) |
| **何を変えたらどの文書を直すのか** | [doc-checklist.md](doc-checklist.md) |

## この文書群の位置づけ

同じ内容を 2 箇所に書かない。役割で分かれている。

| 種類 | 場所 | 内容 |
|---|---|---|
| **規範** | [`.agents/rules/`](../../.agents/rules/) | 守らなければならないこと。lint と `ravel-review` が強制する |
| **手順** | `docs/dev/`（ここ） | 触る箇所の順序とチェックリスト。規範へリンクする |
| **参照** | [`../agent-api-reference.md`](../agent-api-reference.md)、[`../gpui-ui-guide.md`](../gpui-ui-guide.md) | 型・関数の地図、コード断片 |
| **設計意図** | [`../specifications/`](../specifications/) | どう振る舞うべきか |
| **計画** | [`../implementation/`](../implementation/) | 何をどの順で作るか |
| **実装状況** | [`../ui-impl-status.md`](../ui-impl-status.md) | 今どこまで動くか |

## 最初に読むもの

1. [`../../AGENTS.md`](../../AGENTS.md) — リポジトリ全体の地図と規約
2. 触るファイルに `paths` が一致する `.agents/rules/*.md`
3. [workflow.md](workflow.md) — 設計ゲート、文書の同期規約、PR 前の手順

## 手順書が古くなったら

**公開 API・登録経路・アセット形式を変えたら、同じ変更で該当ページを直す**
（[`.agents/rules/documentation.md`](../../.agents/rules/documentation.md)）。
手順書が腐る主因は「触ったのに気づかない」なので、気づいた人が直す。
どの文書が対象かは [doc-checklist.md](doc-checklist.md) の対応表で引く。

コード引用に行番号を書かないのは、この文書群の寿命を長くするため。行番号が
必要な精度の情報は `agent-api-reference.md` 側の役割。
