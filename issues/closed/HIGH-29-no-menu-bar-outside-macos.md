# HIGH-29 | bug | Windows / Linux にメニューが 1 つも出ない — `set_menus` は macOS しか実装していない

**解決済み**: PR #343（2026-08-08）。非 macOS では
`gpui_component::menu::AppMenuBar` をメインウィンドウのタイトルバーに描く。
`workspace::install_menus` がメニューの唯一の出口で、macOS の OS メニューバーと
同じ `build_menus` を配る。プラットフォーム分岐は
`title_bar::render_main_title_bar` の `cfg!` 1 箇所。

**Windows 実機でメニューバーの表示と操作を確認した**（2026-08-09）。
**Linux は実機が無く未確認のまま閉じた** — `set_menus` の実装が無いのは
ソース上確かで、描画先も同じ経路なので同じ結果になる見込み。
Linux で出ないと分かったら再起票する。

`crates/ravel-app/src/main.rs:99`、`crates/ravel-app/src/workspace.rs:673`,
`:736`, `:759`, `:865`（すべて `cx.set_menus(build_menus(&shell))`）

## 症状

**Windows でメニューが出ない。** File / Edit / Composition / Layer / View —
アプリのメニュー階層に到達する手段が 1 つも無い。

## 原因

Ravel はメニューを `cx.set_menus()` にしか渡していない。これは gpui の
**アプリケーションメニューバー**の API で、`App::set_menus`
（`gpui/src/app.rs:2218`）は `self.platform.set_menus(...)` へ委譲する。

その `Platform::set_menus` を実装しているのは **macOS とテストプラットフォームだけ**。
Windows / Linux では**何も起きない** — エラーも警告も無い。macOS はメニューバーが
OS 側にあるので成立しているが、他の 2 つには置き場所が無い。

つまりこれは配線ミスではなく、**メニューを持つ場所をそもそも用意していない**。

`build_menus`（`workspace.rs`）が返す `Vec<Menu>` は正しく、
`CommandId` → Action の対応（`for_each_command!`）も正しい。
**足りないのは非 macOS の描画先だけ。**

## 影響

リリース阻害。Windows ではキーバインドを覚えている操作しか実行できず、
キーバインドの無いコマンド（`docs/specifications/ui/keybindings.md` が
「メニューと `on_action` だけ」と書いているキーフレーム補間の切り替えなど）は
**到達不能**。

## 修正方針

**非 macOS では自前のメニューバーを描く。** `gpui_component` の Menu を
TitleBar（`DOCK-7` で共通化済み）に置き、`build_menus` の同じ
`Vec<Menu>` から組む。

- **メニューの定義は 1 つに保つ。** `build_menus` を唯一の出所にし、
  プラットフォームごとに「どう描くか」だけを分ける。2 つ目のメニュー表を
  作らない（`for_each_command!` が 1 表なのと同じ理由）
- 発行するのは**同じ Action**。`.agents/rules/gpui.md` の
  「メニュー・キーバインド・ボタンが同じ Action を生成する」を守る
- macOS は `set_menus` のまま（OS のメニューバーが正しい置き場所）

`cfg(target_os)` の分岐は 1 箇所に閉じること。

## 閉じた時点で残した未確認

- **Linux** — 実機が無く見ていない。`set_menus` の実装が無いのはソース上確かで、
  非 macOS の描画先は Windows と同じ 1 経路なので同じ結果になる見込み
- **Windows 固有のタイトルバーの寸法** — 右側 3 ボタン分の幅と、狭いウィンドウで
  中央ラベルとメニューが重なるか。中央ラベルはリスナを持たないオーバーレイ
  なので機能には影響しない
