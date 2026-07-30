# 実装者向けドキュメント 実装計画

> **Status**: DEV-1〜8 完了 — 2026-07-30

対象: 「ノードを追加したい」「パネルを追加したい」ときに読めば分かる手順書を
用意し、`docs/` 全体に索引を付ける。

## 問題

既存の文書は**参照系と規範系に偏っていて、手順系が無い**。

| 既存 | 種類 | 内容 |
|---|---|---|
| `.agents/rules/*.md` | 規範（MUST） | Rust / GPUI / 文書の規約。lint と `ravel-review` が強制する |
| `docs/agent-api-reference.md` | 参照（WHAT） | クレートごとの公開 API 地図（896 行） |
| `docs/gpui-ui-guide.md` | 参照（HOW の断片） | GPUI のパターン集（パネル実装、canvas、focus、i18n） |
| `docs/specifications/*` | 設計意図 | 何を作るか |
| `docs/implementation/*` | 計画 | いつ・どの順で作るか |

**「1 つの機能を足すのに何箇所を触るのか」がどこにも書いていない。** 実際には
ノード 1 個の追加で 4 箇所（テンプレート / プロセッサ / 配線の match / テスト）、
パネル 1 枚で 7 箇所（`PanelKind` / ロケール / 生成 / 登録 / コマンド / プリセット /
キーバインド）を触る。しかもロケールキーの欠落はテストで機械的に落ちるので、
知らないと最初の `mise run check` で初めて気づく。

`docs/` 直下に索引が無いのも同じ問題の一部で、どの文書がどの役割かは
`AGENTS.md` の箇条書きだけが知っている。

## 決定事項

### 手順書は `docs/dev/` に置き、役割を明示する

```text
docs/README.md          ← docs 全体の索引（役割の地図）
docs/dev/README.md      ← 手順書の索引 + 「どれを読むか」
docs/dev/add-node.md
docs/dev/add-panel.md
docs/dev/add-command.md
docs/dev/add-locale.md
docs/dev/persistence.md
docs/dev/testing.md
docs/dev/workflow.md
```

役割の切り分けは次のとおりで、**同じ内容を 2 箇所に書かない**。

| 種類 | 場所 | 書くこと |
|---|---|---|
| 規範 | `.agents/rules/` | 守らなければならないこと。違反は lint / review で落ちる |
| 手順 | `docs/dev/` | 触る箇所の順序とチェックリスト。規範へはリンクする |
| 参照 | `docs/agent-api-reference.md`、`docs/gpui-ui-guide.md` | 型と関数、コード断片 |
| 設計意図 | `docs/specifications/` | どう振る舞うべきか |
| 計画 | `docs/implementation/` | 何をどの順で作るか |

### 手順書は「触る箇所の全数」を先に出す

各手順書の冒頭に**チェックリスト**を置く。手順の説明よりチェックリストが本体。
忘れると落ちるもの（ロケールキー、テスト、`match` の網羅）を明示する。

### コードを引用するときは行番号を書かない

手順書は寿命が長いので、`crates/...` のパスと関数名までにする。行番号は
`agent-api-reference.md` 側の役割。

### 実装が変わったら手順書を直す義務を規約に入れる

`.agents/rules/documentation.md` に「公開 API・登録経路・アセット形式を変えたら
`docs/dev/` の該当手順を同じ変更で直す」を追加する。手順書が腐る主因は
「触ったのに気づかない」なので、規範側に 1 行入れる。

## 実装単位

### DEV-1: `docs/dev/add-node.md`

- 触る箇所: レジストリテンプレート（`registry/builtin.rs`）→ プロセッサ
  （`ravel-nodes`）→ `processor_for_node` の match → テスト
- パラメータは**プロセッサに capture しない**（評価器が毎フレーム
  `ResolvedParams` に解決する。だから編集は dirty マークだけで済む）という
  不変条件を明示する
- GPU 版を足す場合: `GpuContext` / `ShaderManager` / `TexturePool` を取る
  コンストラクタ、WGSL は `crates/ravel-nodes/src/shaders/`、CPU/GPU 等価性
  テストが必須（アルファ規約、タップ境界）
- `is_time_dependent` の判断基準
- 設計原則へのリンク（`procedural-geometry.md` の「固定機能のリピーターを
  作らない」）

### DEV-2: `docs/dev/add-panel.md`

- 触る箇所: `PanelKind`（`ravel-ui/src/panel.rs`）→ ロケールキー →
  ヘッドレス状態（`ravel-ui/src/panels/`）→ GPUI パネル（`ravel-app/src/panels/`）→
  `register_panels` の match → 表示トグルコマンド → ワークスペースプリセット
- **ロケールキーは `PanelKind::ALL` を走査するテストが強制する**ことを書く
- ヘッドレス層と GPUI 層の分離（状態とロジックは `ravel-ui`、描画と入力は
  `ravel-app`）
- focus 所有、`render()` の純粋性、Global の使い分けは `.agents/rules/gpui.md`
  と `gpui-ui-guide.md` へリンクする

### DEV-3: `docs/dev/add-command.md`

- `CommandId`（`ravel-ui`）→ `for_each_command!` テーブル（`workspace.rs`）→
  ロケール → キーバインド（アセット or コンテキスト付きのコード側）→ メニュー
- **`actions!` を他の場所で宣言しない / Command↔Action の第 2 のリストを
  作らない**（テーブルの網羅 match がコンパイルエラーで守る）
- パネル固有ショートカットはキーコンテキストに束縛する

### DEV-4: `docs/dev/add-locale.md`

- `assets/locales/en.toml` / `ja.toml` の構造（フラットキーとサブテーブル、
  `_self` の規約）
- `t!` の使い方と、ハードコード英語を増やさない規約（`LOW-APP-11`）
- キー欠落を落とすテストの一覧（コマンド / パネル / プリセット）

### DEV-5: `docs/dev/persistence.md`

- **追加フィールドは `#[serde(default)]` で format version を上げない**
  （`Layer.audio` の前例）。上げるのは既存フィールドの意味を変えるときだけ
- マイグレーションの書き方と連鎖（v1→v2→…）、`.bak` の扱い
- ID カウンタの前進、`ui_state.json` の任意エントリ

### DEV-6: `docs/dev/testing.md`

- どこに何を置くか: コアの単体テスト / `ravel-nodes` のゴールデン /
  `ravel-app` の GPUI テスト
- **GPUI テストは focus・Action 伝播・入力経路・描画に依存する挙動だけ**
  （`.agents/rules/gpui.md`）
- 純粋関数に切り出して単体テストで覆う原則、ゴールデン画像を増やさない原則
- `mise run check` の内訳（fmt / pattern lint / clippy / tests）

### DEV-7: `docs/dev/workflow.md`

- 設計ゲート（複数クレート・複数パネル・サブシステム改修は計画書が先）
- `issues/` と `docs/implementation/` の使い分け、`backlog.md` / `roadmap.md` /
  `README.md` の三者同期の規約
- `ravel-review` を PR 前に流す、`mise trust` が新しい worktree で必要
- コミット粒度とメッセージ規約へのリンク

### DEV-8: `docs/README.md` と索引の整備

- `docs/` 直下に索引を作り、役割の地図（上記の 5 分類）を置く
- `AGENTS.md` の「Important references」から索引へ 1 本にまとめる
- `.agents/rules/documentation.md` に手順書の更新義務を追加する

## 検証

- 文書のみなので `mise run check` は不要
- **各手順書のチェックリストを実物と突き合わせる**（`PanelKind::ALL` の要素数、
  `processor_for_node` の match、ロケールテストの存在）
- 行番号を書いていないことを確認する

## 非対象

- **rustdoc の整備**。`cargo doc` の内容は型側の doc コメントの仕事で、
  手順書とは別
- **チュートリアル（アプリの使い方）**。これは実装者向けではない
- **プラグイン API の手順**。REQ-PLUGIN / REQ-CODE-001 が固まってから
