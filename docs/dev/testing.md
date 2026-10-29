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

### `ffmpeg` フィーチャ配下のテストは `check` では走らない

`mise run check` も CI の `check` ジョブも既定フィーチャで `cargo test --workspace`
を回すので、`#[cfg(feature = "ffmpeg")]` の下にあるテストはそこでは実行されない。
CI は別ジョブ `ffmpeg`（macOS、Homebrew の FFmpeg 9）で `ravel-media` / `ravel-audio` /
`ravel-nodes` / `ravel-cli` を `--features ffmpeg` で clippy + test する。`ffmpeg-the-third` 6 は
FFmpeg 9 を束ねる（Windows では回していない）。

上記 4 クレート以外や、手元での確認には

```bash
cargo test --workspace --features ffmpeg
```

を自分で回し、結果を報告に書く。FFmpeg の共有ライブラリと `ffmpeg` CLI
（テストが素材を合成する）が要る。

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
  ゴールデン（`shape_layer_golden.rs`）は合成チェーンの画を固定する目的が
  あり、安易に足すと GPU 経路の変更ごとに更新作業が生まれる。同ファイルは
  各画を CPU 参照ラスタライザと GPU ラスタライザの両方で描き、
  **両者が許容誤差内で一致すること**を要求する形になっている
  （片方の画素を pin すると、もう片方への切り替えが回帰に見える）
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

## 動いているアプリを MCP で確かめる（実機確認）

UI に出る変化（レイヤーが増える、値が動く、確認ダイアログが出る）は、アプリを起動して
外から編集すると確かめられる。編集と読み取りは `ravel-cli mcp --attach` で、画面は
OS のスクリーンショットで見る。仕組みは [`live-edit.md`](live-edit.md) と
[`mcp-cli.md`](mcp-cli.md)。以下は macOS の手順。

1. **global 設定を退避してから口を開ける。** 設定は全 Ravel 共通なので、必ず戻す前提で触る

   ```bash
   F="$HOME/Library/Application Support/ravel/settings.toml"
   cp "$F" /tmp/settings.toml.bak            # 無ければ空ファイルを作って退避扱いにする
   printf '\n[agent]\nlive_edit_enabled = true\n' >> "$F"
   ```

   既に `[agent]` 表があるなら、追記せずその表に `live_edit_enabled = true` を書く
   （同じ表が 2 つあると読み込みに失敗する）

2. **アプリと CLI を別々に組む。** `--workspace` で組むと CLI に音声フレームワークが
   リンクされる（[`rust.md`](../../.agents/rules/rust.md)）

   ```bash
   cargo build -p ravel-app && cargo build -p ravel-cli
   ```

3. **起動して口ができるのを待つ。** macOS の口は `~/Library/Caches/ravel/live-edit.sock`

   ```bash
   (./target/debug/ravel > /tmp/ravel-app.log 2>&1 &)
   for i in $(seq 1 40); do [ -S ~/Library/Caches/ravel/live-edit.sock ] && break; sleep 1; done
   ```

4. **`ravel-cli mcp --attach` に JSON-RPC を流す。** 1 行 1 メッセージ。`initialize` →
   `notifications/initialized` → `tools/call` の順

   ```json
   {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}
   {"jsonrpc":"2.0","method":"notifications/initialized"}
   {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"live_edit","arguments":{"ops":[{"op":"add_composition","settings":{"name":"Check","resolution":[640,360],"frame_rate":{"num":24,"den":1},"duration_frames":48,"background_color":{"r":0.2,"g":0.4,"b":0.8,"a":1}},"as":"c"},{"op":"add_layer","comp":"c","template":"solid"}]}}}
   {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"inspect_document","arguments":{"live":true}}}
   {"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"render_preview","arguments":{"live":true,"frame":0,"output":"/tmp/ravel-check/frame.png"}}}
   ```

   ```bash
   ./target/debug/ravel-cli mcp --attach --allow-dir /tmp/ravel-check < requests.jsonl
   ```

   - `inspect_document` の `live: true` で、テンプレートが作ったノードの id と今の値を読む。
     次の編集はこの id で指す
   - `render_preview` の `live: true` は合成結果だけを PNG にする（CLI 側の GPU。UI は映らない）。
     書き出し先は `--allow-dir` の中だけ
   - 削除を含む `live_edit` は、アプリの確認ダイアログで承認されるまで返らない。
     承認は Enter、拒否は Escape をアプリへ送る（`osascript -e 'tell application "System Events" to key code 36'`）
   - ソケットへ直接 `nc -U` で `{"id":1,"ops":[…]}` / `{"id":2,"read":"document"}` を送っても
     よい（MCP を挟まない分、プロトコルの確認向き）

5. **画面を撮る。** アプリを前面に出してから撮る

   ```bash
   P=$(pgrep -f 'target/debug/ravel$' | head -1)
   osascript -e "tell application \"System Events\" to set frontmost of (first process whose unix id is $P) to true"
   screencapture -x -o /tmp/ravel-check/screen.png
   ```

   Cmd+Z など特殊キーの送り方は `osascript` の `keystroke` / `key code`。undo は
   1 呼び出し 1 段なので、`live_edit` 1 回の後の Cmd+Z 1 回で元に戻ることも確かめられる

6. **後片付け。** 終了 → 口が消えたこと → 設定を戻して一致を確かめる

   ```bash
   osascript -e "tell application \"System Events\" to tell (first process whose unix id is $P) to keystroke \"q\" using command down"
   [ -S ~/Library/Caches/ravel/live-edit.sock ] && rm ~/Library/Caches/ravel/live-edit.sock   # 強制終了で残った場合だけ
   cp /tmp/settings.toml.bak "$F" && cmp /tmp/settings.toml.bak "$F"
   ```

確かめたこと（操作・期待・結果）とスクリーンショットの要点を PR 本文に書く。
