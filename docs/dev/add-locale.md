# ロケール文字列を追加する

> 索引: [`README.md`](README.md)

## チェックリスト

- [ ] `assets/locales/en.toml` にキーを追加（**英語は fallback なので必須**）
- [ ] `assets/locales/ja.toml` に同じキーを追加
- [ ] 新しいロケールを追加する場合は `[language] name`（そのロケール自身の
      呼び名。言語選択の表示ラベル）も入れる
- [ ] コード側は `t!("<key>")` で引く
- [ ] `mise run check`

## 形式

```toml
# フラットキー → "app.title"
[app]
title = "Ravel"

# サブテーブルの _self → "menu.file" 自身のラベル
[menu.file]
_self = "File"
new = "New"        # → "menu.file.new"
```

`_self` はそのテーブル自体のラベルを表す規約。メニューやパネルのように
「見出しにも子項目にも名前がある」ものに使う。

ノード種別のキーは type_key をそのままテーブル名に使う（ドットを含むので
クォートする）:

```toml
[node."field.noise"]
label = "Noise Field"
description = "Generates a 2D simplex noise field over position."
[node."field.noise".params]
frequency = "Spatial frequency of the base octave."
```

## 使い方

```rust
use ravel_i18n::t;

div().child(SharedString::from(t!("panel.outliner")))
```

`t!` は現在のカタログから引き、見つからなければ英語カタログへ、
それも無ければキー文字列そのものを返す。**panic しない**ので、
キーの欠落は画面にキー名が出る形で現れる。

## キー欠落を落とすテスト

`crates/ravel-ui/src/lib.rs` の `#[cfg(test)]` が、次の集合を走査して
en カタログにキーがあるか検査する。

| テスト | 対象 |
|---|---|
| `all_command_label_keys_in_catalog` | `CommandId` 全 variant の `label_key()` |
| `all_panel_label_keys_in_catalog` | `PanelKind::ALL` の `label_key()` |
| `all_preset_label_keys_in_catalog` | ワークスペースプリセットの `name` |

**ja 側は機械的に強制されていない**（en が fallback なので動いてしまう）。
追加時は両方に入れる。

## 規約

- **ユーザーに見える文字列をコードにハードコードしない。** 既存の違反は
  `LOW-APP-11` として台帳にある（分離ウィンドウのタイトル、一部のエラー文言）。
  新しい違反を増やさない
- ノード種別のラベル・説明・パラメータ説明は `[node."<type_key>"]` テーブルに
  持つ（type_key はドットを含むのでクォートキー）。`label` は必須で、
  `ravel-ui::node_locale` のレジストリ走査テストが en / ja 両方の欠落を落とす。
  `description` と `params.<name>` は任意。解決は
  `ravel-app::node_locale`（ラベルのキー欠落時は `type_key` にフォールバック）
- **ロケールは環境設定 ▸ 言語から切り替えられる**（`settings-screen-plan.md` の
  `SET-4`）。選択肢は `ravel_i18n::available_locales()`（順序不定なので
  呼び出し側でソート）で、ラベルは各カタログの `language.name`
  （`ravel_i18n::locale_display_name`）。**新しいロケールには `language.name` を
  必ず入れる** — 無いと選択肢がロケールコードのまま出る。切り替えは
  `app_settings::update` 経由で `global` 層に書かれ、`cx.refresh_windows()` で
  開いている全ウィンドウが新しい言語で再描画される（メニューバーは要素ツリーの
  外なので `RavelWorkspace` が設定 Global を購読して組み直す）
- 文字列の組み立てを翻訳側に押し付けない（語順が変わるため、値の差し込みが
  必要なら 1 キー 1 文で持つ）
- **値を差し込む文は `{name}` プレースホルダを含む 1 キーにする。**
  `t!` は引数を取らないので、呼び出し側が `pattern.replace("{name}", …)` で
  埋める（例: `window.panels = "{count} panels"` / `"{count} 個のパネル"`。
  埋める側は `window_host::panel_count_title`）。翻訳済み断片と数値を
  `format!` で連結すると語順が英語固定になる
- **複数形は持たない。** カタログはキー → 文字列の平坦な対応で、複数形の
  選択機構が無い（日本語には複数形も無い）。数を含む英語のキーは
  `"{count} frames"` のように**数に依らない一つの言い回し**で書く
- **`ravel-ui` は i18n に依存しない。** 画面に出る語を作る箇所では、
  文字列そのものではなくロケールキーを載せ、`ravel-app` の表示境界で
  解決する（`properties::layer::VALUE_ON` や
  `keyframes::CHANNEL_VALUE` がその形）。数を含む文は
  `properties::counted_value(key, n)` でキーと数を一緒に載せ、
  `panels::properties::read_only_value` が `{count}` を埋める。
  **保存値・比較はキーのまま**なので、言語を切り替えても編集結果は変わらない
- **記法は翻訳しない。** 単位記号（`f` / `fps`）、トグルのグリフ
  （`S` / `M` / `L` / `F`）、軸とカラーチャネルの文字（`X` / `Y` /
  `R` / `G` / `B` / `A`）はロケールキーを持たない。意味は必ず
  ローカライズされた対（ツールチップ、親の行ラベル）で伝え、その一覧を
  `docs/specifications/ui/timeline.md` の「翻訳しない表記」に載せる
