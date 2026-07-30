# 変更の流し方

> 索引: [`README.md`](README.md)

## 着手前

1. **`docs/implementation/backlog.md` で単位を探す。** 「今すぐ着手できるもの」の
   表が依存の解決済みなもの
2. **順序の根拠は `docs/implementation/roadmap.md`。** backlog は「何があるか」、
   roadmap は「どの順でやるか、なぜその順か」
3. 設計は各計画書（`docs/implementation/*-plan.md`）が正。**古い計画文書と
   実装が食い違うときは実装が正**
4. バグ・負債・性能問題は `issues/`（深刻度別）。issue は実装単位ではないので
   backlog には載らない

## 設計ゲート

**複数クレート・複数パネルにまたがる変更、またはサブシステム（コマンド送出、
フォーカス、評価、永続化）の作り替えは、コードより先に
`docs/implementation/` の計画書が必要。** 雛形は
`done/gpui-command-focus-refactor-plan.md`。

計画書に書くこと: 問題 / 目標アーキテクチャ / レビュー可能な実装単位 /
単位ごとの完了条件 / 検証 / 非対象。

小さい修正と単一パネルの機能は計画書を要らない。

## 実装中

- 触る前に、対象ファイルに `paths` が一致する `.agents/rules/*.md` を読む
- 未実装の機能に UI の約束をしない（動かないハンドルやカーソルを作らない）
- ユーザーの作業ツリーの無関係な変更を壊さない

## 完了の定義（`AGENTS.md`）

- 要求された挙動が、無関係な変更なしに実装されている
- テストが挙動を覆っている。または自動テストが無い理由を述べている
- 整形と適切な検査が通っている
- リスクに応じて広めのテストを流した
- エラーとプラットフォーム制約を明示的に扱っている
- **影響する文書・ロケール・アセットを同じ変更で更新した**
- 最終報告に変更ファイル・実施した検証・残る制限を書いた

## 文書の同期規約

| 変えたもの | 一緒に直すもの |
|---|---|
| 計画書の単位 | `backlog.md` の表（片方だけ直さない） |
| 着手順の判断 | `roadmap.md`（クラスタ行と根拠） |
| 計画書の追加・完了 | `docs/implementation/README.md` の索引 |
| issue を計画が引き受けた | roadmap のクラスタ行から外し、issue 個票に引受先を書く |
| UI の挙動 | `docs/specifications/ui/<view>.md`（設計意図）と `docs/ui-impl-status.md`（実装状況） |
| 公開 API・登録経路・アセット形式 | `docs/dev/` の該当手順と `docs/agent-api-reference.md` |

## PR 前

1. `mise run check`
2. **`ravel-review` スキルを diff に対して流す。** lint では見えない文脈依存の
   不変条件（render 純粋性、focus 所有、コマンド経路の単一性、Global の用法、
   コア層の分離）を辿る。PASS しないと `gh pr create` がブロックされる
3. コミットは 1 概念 1 コミット、英語 1 行の Conventional Commit
   （`feat:` / `fix:` / `refactor:` / `docs:` / `test:` / `chore:` / `perf:` / `ci:`）
4. ブランチ名は同じ接頭辞 + 具体的な kebab-case（`fix/node-editor-shortcuts`）
5. **コミット・push・PR 作成はユーザーに頼まれたときだけ**
6. タスク ID・issue 番号・レビュー元・エージェント名をコミットメッセージに
   入れない（明示的に要求された場合を除く）
