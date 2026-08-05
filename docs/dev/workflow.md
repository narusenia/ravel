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

## 文書の同期

対応表は [doc-checklist.md](doc-checklist.md) が持つ（ここには複製しない）。
要点だけ:

- `backlog.md` / `roadmap.md` / `implementation/README.md` の**三者は同時に直す**
- UI の挙動を変えたら `specifications/ui/<view>.md`（設計意図）と
  `ui-impl-status.md`（実装状況）の**両方**
- 登録経路とアセット形式を変えたら `docs/dev/` の該当手順（規約で義務）

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

## CI が何を走らせるか

`.github/workflows/ci.yml` は macOS と Windows で `fmt` → `lint:patterns` →
`clippy` → `test` を流す。時間の大半は**テストバイナリのコンパイル**で、
Windows は macOS の約 3 倍かかる。省けるものは省いてある:

- **文書だけの変更は CI を走らせない。** `docs/**` と `**.md` は
  `paths-ignore` の対象
- ただし `paths-ignore` は pull request では**PR 全体の差分**で判定されるので、
  すでに Rust を変えたブランチに文書だけのコミットを足すとワークフロー自体は
  起動する。そこは**ジョブ側のガードが「この push が変えたもの」を見て**
  重いステップを飛ばす。判定できないとき（初回、force push で `before` が
  消えたとき）は必ず全部走る側に倒れる
- **`fmt` と `lint:patterns` は macOS だけ。** ソーステキストしか読まないので
  プラットフォームで結果が変わらない
- **片方のプラットフォームが落ちたら他方はキャンセルされる**（`fail-fast`）。
  プラットフォーム固有の失敗は、もう一方が通ることで最後まで走るので拾える
- **ベンチは main へのマージ時だけ。** PR では `clippy --all-targets` が
  ベンチのコンパイルを覆っている

レビュー指摘のうち**文書だけの修正は、PR に push せずマージ後の `main` 側の
文書コミットにまとめる**ほうが速い。`main` では上記の `paths-ignore` が効く。
