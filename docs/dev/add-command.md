# コマンド・ショートカット・メニュー項目を追加する

> 索引: [`README.md`](README.md)

規約は [`.agents/rules/gpui.md`](../../.agents/rules/gpui.md) の
「Command path invariants」。

## チェックリスト

- [ ] `crates/ravel-ui/src/command.rs` の `CommandId` に variant を追加
- [ ] 同ファイルの `label_key()` にロケールキーを追加
- [ ] `crates/ravel-app/src/workspace.rs` の `for_each_command!` テーブルに追加
- [ ] `assets/locales/en.toml` / `ja.toml` にラベルを追加
- [ ] キーバインドを付けるなら `assets/keybindings/default.toml`
      （グローバル。ユーザー上書きが効くのはここ）またはコード側
      （パネル固有。キーコンテキスト付き）
- [ ] メニューに出すなら `crates/ravel-ui/src/menu.rs`
- [ ] ハンドラを置く（パネル固有なら `on_action`、それ以外は
      `RavelWorkspace::dispatch_command`）
- [ ] `mise run check`

**忘れやすいもの**: ロケールキーは `CommandId` 全 variant を走査するテストが
強制する。`for_each_command!` テーブルの漏れは網羅 `match` がコンパイルエラーで
教えてくれる。

## 経路は 1 本だけ

```text
キーバインド / メニュー / ボタン
        └→ GPUI Action → 最も近い focus 上の on_action ハンドラ
                          └─ 未処理 → RavelWorkspace::dispatch_command()
```

やってはいけないこと:

- `actions!` を `workspace.rs` の外で宣言する
- Command ↔ Action の対応表を 2 つ持つ
- Global にコマンドを積んで別のエンティティが拾う（`Global<Option<Event>>`）
- `render()` の中でコマンドを処理する

`for_each_command!` の網羅 `match` が「テーブルに無い `CommandId`」を
コンパイルエラーにするので、テーブルが唯一の対応表になっている。

## キーバインドの置き場所

| 種類 | 置き場所 |
|---|---|
| グローバル（File / Edit / View / Playback …） | `assets/keybindings/default.toml`。セクション名 + アクション名が `CommandId` と一致必須 |
| パネル固有（ツール切替、Fit View、Delete …） | `workspace.rs` の `PANEL_BINDINGS` に 1 行足す（コマンド / chord / パネル / キーコンテキスト） |
| ユーザーによる上書き | `<config>/ravel/keybindings.toml`。既定と同じ形式で、起動時に上へ重ねる（`ravel_app::keybindings`） |

アセット側にはコンテキストを表現する形が無いので、パネル固有のものはコードに
しか書けない。**したがってユーザー上書きが効くのは既定アセットに載っている
グローバルなバインドだけ**で、パネル固有のものは対象外（`SET-12`）。

`PANEL_BINDINGS` は**表を 1 つだけ持つ**という規約に従う（`for_each_command!` と
同じ理由）。GPUI への登録と環境設定のキーバインド一覧の両方がこの表を読み、
`panel_bound_commands()` が「ユーザーファイルから再割り当てさせないコマンド」を
そこから導く。**バインドを直接 `KeyBinding::new` で足すな** — 一覧に出てこない
ショートカットができ、ユーザーには「割り当てなし」と見える。

ユーザーファイルは既定へ**重ねる**（`parser::overlay_user_toml`）。同じコマンドを
別 chord に割り当てると既定の chord は外れ、chord が既定と衝突すればユーザーが
勝つ。ファイル内で同じ chord が衝突した場合は id の昇順で先のものが勝つ。
解釈できない行はその行だけ警告して捨てるので、1 行の typo が起動や他の
バインドを壊すことはない。**追加した経路は必ず `AppShell` 経由**にすること —
`build_keybindings` が全バインドに `!Input` コンテキストを付けており、そこを
迂回して `KeyBinding` を作ると `MED-APP-16`（テキスト入力から矢印を奪う）が戻る。

生の `on_key_down` で修飾キーを見るのは、テキスト入力や一時的なドラッグモード
（Viewer の `H` ホールドなど）のような本当に低レベルな入力に限る。

## ハンドラの置き場所

- **パネル固有の意味を持つコマンド**（Delete、キーフレーム補間の変更など）は
  パネルの `on_action` で受ける。focus 階層で最も近いハンドラが勝つので、
  同じ Action を複数パネルが持ってよい
- **アプリ全体のコマンド**は `RavelWorkspace::dispatch_command()` の 1 箇所。
  ここが `AppShell::handle_command` を呼ぶ唯一の場所
- パネルが受けて処理しない場合は `cx.propagate()` で上へ流す
