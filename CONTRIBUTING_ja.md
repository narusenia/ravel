# Ravel への貢献

> English: [`CONTRIBUTING.md`](CONTRIBUTING.md)

Ravel を見てくれてありがとうございます。このページは**地図**で、規約その
ものではありません。規約は、それが縛るコードの隣に置いてあります — ここに
写すと、いつか片方が必ず古くなります。

参加にあたっては [行動規範](CODE_OF_CONDUCT_ja.md) に同意したものとします。

## 規約の場所

| 読むもの | 何が書いてあるか |
|---|---|
| [`AGENTS.md`](AGENTS.md) | **リポジトリガイドの正本** — クレート構成、設計ゲート、検証、git の作法、完了の定義 |
| [`.agents/rules/`](.agents/rules) | 変更が守るべきこと: Rust とアーキテクチャ、GPUI、**UX の 12 個の不変条件**、文書の一貫性 |
| [`docs/dev/`](docs/dev) | 具体的な手順書: ノード・パネル・部品・コマンド・ロケール文字列の追加、永続化の変更、テーマの書き方、テスト |
| [`docs/README.md`](docs/README.md) | どの文書がどの役割か |

`AGENTS.md` はコーディングエージェント向けに書かれていますが、内容は
エージェント固有ではありません。単にリポジトリの規約が置かれている場所で、
`CLAUDE.md` はその 1 行の import です。

## 環境

安定版の Rust で建ちます。タスクランナーは [mise](https://mise.jdx.dev/):

```bash
mise trust          # clone や git worktree ごとに一度、`mise run` の前に
mise run hooks:install   # 任意: pre-commit で fmt / lint / clippy / docs
mise run check      # fmt + アンチパターン lint + clippy -D warnings + テスト
```

`mise run check` が CI と同じものです。PR を出す前に通してください。

時間を取られやすいビルドの注意 2 つ:

- ヘッドレスのバイナリが欲しいときは **`cargo build -p ravel-cli`。
  `cargo build --workspace` は使わない。** Cargo は 1 回のビルドで
  feature を統合するので、workspace ビルドは `ravel-cli` に音声デバイス
  ライブラリをリンクします — feature 分割がまさに避けているものです。
- 新しい `git worktree` は target が空なので、最初のコミットで
  pre-commit の clippy が数分かかります。

## 変更の作り方

- **ブランチ名**: 意味のある prefix + 具体的なケバブケース。
  `fix/node-editor-shortcuts` は良く、`fix/phase2` は駄目です。
- **コミット**: 1 コミット 1 概念、件名は**英語 1 行**の
  [Conventional Commit](https://www.conventionalcommits.org/)
  （`feat:` / `fix:` / `refactor:` / `docs:` / `test:` / `chore:` /
  `perf:` / `ci:`）。件名に issue 番号やチケット参照を入れません。
- **テスト**: バグ修正には回帰テストを付け、**修正を戻すとそのテストが
  落ちる**ことを確かめてください。窓が要らない挙動は
  ヘッドレス（`ravel-core` / `ravel-ui`）で書くのが優先です。
- **文書**: [`docs/dev/doc-checklist.md`](docs/dev/doc-checklist.md) が
  「どの種類の変更がどの文書を義務づけるか」の対応表です。
  `mise run docs:check` がリンク・索引の網羅・issue 件数を検証します。
- **依存**: 本番依存の追加とピン留めした git 依存の変更は事前に相談を。
  FFmpeg は動的リンクを保ち、配布バイナリに GPL の条件を課す依存は
  入れません。
- **クレートやパネルを跨ぐ変更、サブシステムの作り直し**（コマンド
  ディスパッチ、フォーカス、評価、永続化）は、コードの前に
  [`docs/implementation/`](docs/implementation) に計画書が欲しいところです。
  小さな修正と単一パネルの機能は要りません。

## Pull request

テンプレートが訊くのは、**差分からは再構成できないこと**です: なぜ今この
変更なのか、**設計判断とその理由**、既存の挙動が変わらないと考える根拠、
実際に回した検証。それを埋めることがレビューの中身です。

## issue の置き場

- **GitHub Issues** がバグ報告と機能要望の場所です。テンプレートを
  使ってください（英語版と日本語版があります）。
- リポジトリ内の [`issues/`](issues) ディレクトリは別物です。監査所見の
  内部台帳（`MED-APP-05` / `LOW-CORE-03` …）で、深刻度ごとのファイルと
  索引を持っています。**メンテナから依頼がない限り、PR でここに項目を
  追加しないでください** — 採番は中央でやっており、
  `mise run docs:check` が件数を検証します。

## 言語

コード・コメント・コミット件名・公開向け文書は英語です。`docs/` の多くは
日本語で、issue と PR はどちらの言語でも構いません — はっきり書ける方で。

## ライセンス

Ravel は [Apache 2.0](LICENSE-APACHE) と [MIT](LICENSE-MIT) のデュアル
ライセンスです。貢献も同じ条件で受け入れます。新しいソースファイルには、
既存のものと同じ `Apache-2.0 OR MIT` の SPDX ヘッダを付けてください。
