# テストと検証

> 索引: [`README.md`](README.md)

## 検証コマンド

| コマンド | 内容 |
|---|---|
| `mise run check` | **正規の検証入口**。fmt + pattern lint + clippy（`-D warnings`）+ workspace テスト |
| `mise run fmt:fix` | 整形を適用 |
| `mise run lint:patterns` | grep で検出できるアンチパターンの検査（`scripts/lint-patterns.sh`） |
| `mise run clippy:all` | optional feature も含む clippy（FFmpeg が必要） |
| `mise run docs:check` | ドキュメントの整合性（リンク切れ / 索引漏れ / issue 件数） |
| `mise run docs:search <語>` | 役割別のドキュメント検索（`scripts/docs.sh` に他のサブコマンド） |
| `mise run hooks:install` | pre-commit フックを入れる |

pre-commit フックは**変更したファイルの種類で絞られる**: `*.rs` を含むときだけ
clippy、`*.md` を含むときだけ `docs:check` が走る（lint-patterns と fmt は常時）。
フルの検証は `mise run check`。

- CI は同じタスクを流す
- **新しい clone や新しい `git worktree` では最初に `mise trust`** を実行する。
  mise が知らないパスでは `mise.toml` が untrusted 扱いになり、すべての
  `mise run` がタスク実行前に失敗する
- `lint-patterns.sh` を緩めて通さない。例外は
  `scripts/lint-patterns.allow` に理由付きで 1 行足す（`.agents/rules/` が
  その例外を文書化しているときだけ）

## どこに何を置くか

| 対象 | 置き場所 | 形 |
|---|---|---|
| 純粋なロジック（座標変換、ヒット判定、状態遷移、補間） | 実装と同じファイルの `#[cfg(test)]` | 単体テスト |
| パネルの状態遷移 | `crates/ravel-ui/src/panels/` | 単体テスト（GPUI 不要） |
| 評価器を通した挙動、CPU / GPU 等価性 | `crates/ravel-nodes/tests/` | 統合テスト |
| 永続化のラウンドトリップとマイグレーション | `crates/ravel-project/src/` と `crates/ravel-project/tests/` | 単体 + 統合テスト（GPUI 不要） |
| フォーカス・Action 伝播・入力経路・描画に依存する挙動 | `crates/ravel-app/` | GPUI テスト |

## 原則

- **GPUI テストは上記の 4 つに依存する挙動だけ**（`.agents/rules/gpui.md`）。
  それ以外は純粋関数へ切り出して単体テストで覆う。GPUI テストは遅く、
  壊れたときに原因の切り分けが難しい
- **ゴールデン画像を増やさない。** 数値で検証できるものは数値で。既存の
  ゴールデン（`shape_layer_golden.rs`）は合成チェーンの確立済みピクセルを
  固定する目的があり、安易に足すと GPU 経路の変更ごとに更新作業が生まれる
- **CPU / GPU 両実装があるノードは等価性テストを必ず持つ。** アルファ規約と
  タップ境界がずれる形のバグは目視で気づけない
- パフォーマンスの主張には測定を添える（`docs/implementation/perf-baseline.md`）。
  warm cache の数字を cold の根拠に使わない
- テストが書けない場合は**その旨を明示する**（`AGENTS.md` の Definition of done）

## カーソルや描画結果のように検証できないもの

プラットフォーム状態（マウスカーソルの形など）はテストプラットフォームで
意味のある検証ができない。その場合は:

1. 「入力 → 意図」の写像を純粋関数に切り出して単体テストで覆う
2. 実機確認の手順と確認した内容を PR 本文に書く

（`done/pointer-feedback-plan.md` がこの形を採っている）
