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

### `ffmpeg` フィーチャ配下のテストは既定で走らない

`mise run check` も CI も既定フィーチャで `cargo test --workspace` を回すので、
`#[cfg(feature = "ffmpeg")]` の下にあるテストは**1 つも実行されない**。
`mise run clippy:all` はコンパイルするだけで、走らせはしない。

デコードを要する経路（`ravel-media` の統合テスト、`ravel-audio` のデコード上限、
`ravel-cli` の音声書き出し）はここに入る。**触ったならローカルで**

```bash
cargo test --workspace --features ffmpeg
```

**を自分で回し、結果を報告に書く。** FFmpeg の共有ライブラリが要る。

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

## 他プラットフォーム向けのコードをどこまで確かめられるか

CI の matrix は `macos-latest` と `windows-latest` だけで、**Linux ランナーは
無い**。それでも Linux 向けのコードを書いたまま放置する理由にはならない。

**ビルドはコンテナで確かめられる**（Apple Silicon ならエミュレーション無しの
aarch64 ネイティブ、`ravel-app` 全体で 3 分程度）:

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/work \
  -v ravel-linux-cargo:/usr/local/cargo/registry -w /work rust:1.95-slim bash -c '
    apt-get update -qq && apt-get install -y -qq pkg-config \
      libfontconfig1-dev libasound2-dev libx11-dev libxkbcommon-dev \
      libwayland-dev libxcb1-dev libssl-dev cmake clang
    cargo check -p ravel-app'
```

名前付きボリュームに registry を残すと 2 回目以降が速い。

**コンテナに GPU は無い。** GPU テストは
`skipping: no GPU adapter available` で全部飛び、`test result: ok` と
表示される — **通ったのではなく飛んだ**ので、`--nocapture` で確かめること。
実行時の挙動（リードバック回数、実際の描画）は実機でしか確かめられない。

つまりコンテナで言えるのは「型と `cfg` が成立する」ところまでで、
そこから先は下の節と同じ扱いになる。

## カーソルや描画結果のように検証できないもの

プラットフォーム状態（マウスカーソルの形など）はテストプラットフォームで
意味のある検証ができない。その場合は:

1. 「入力 → 意図」の写像を純粋関数に切り出して単体テストで覆う
2. 実機確認の手順と確認した内容を PR 本文に書く

（`done/pointer-feedback-plan.md` がこの形を採っている）
