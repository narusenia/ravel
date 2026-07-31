# ロケール文字列を追加する

> 索引: [`README.md`](README.md)

## チェックリスト

- [ ] `assets/locales/en.toml` にキーを追加（**英語は fallback なので必須**）
- [ ] `assets/locales/ja.toml` に同じキーを追加
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
- ノードのラベルと説明は現在ロケールに無い（英語リテラル）。キー化は
  [`../implementation/node-discoverability-plan.md`](../implementation/node-discoverability-plan.md)
  の `DISC-1`
- **ユーザーがロケールを切り替える手段は今のところ無い**（`MED-APP-10`。
  `ja.toml` は 235 キー維持されているが到達できない）。担当は
  `settings-screen-plan.md` の `SET-1`
- 文字列の組み立てを翻訳側に押し付けない（語順が変わるため、値の差し込みが
  必要なら 1 キー 1 文で持つ）
- **値を差し込む文は `{name}` プレースホルダを含む 1 キーにする。**
  `t!` は引数を取らないので、呼び出し側が `pattern.replace("{name}", …)` で
  埋める（例: `window.panels = "{count} panels"` / `"{count} 個のパネル"`。
  埋める側は `window_host::panel_count_title`）。翻訳済み断片と数値を
  `format!` で連結すると語順が英語固定になる
