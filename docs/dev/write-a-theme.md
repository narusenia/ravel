# テーマを書く

> 索引: [`README.md`](README.md)

## チェックリスト

- [ ] 自分のテーマは**ユーザーディレクトリ**に置く（下表。**同梱の
      `assets/themes/` を書き換えない** — 更新で消え、macOS では署名も壊れる）
- [ ] ファイル先頭に `"$schema": "./ravel.schema.json"` を書く
      （**同梱スキーマを相対パスで指す**。エディタ補完が効く）
- [ ] `themes[].name` と `themes[].mode` を書く（**この 2 つが設定ダイアログの
      行と、モードごとの選択肢を決める**）
- [ ] 色は 10 キーだけ（下表）。**書いたキーだけ効き、残りは組み込み値**
- [ ] 保存すれば**再起動なしで反映される**（アプリ起動中に編集して確認）
- [ ] スキーマ（`ThemeSpec`）を変えたなら
      `UPDATE_THEME_SCHEMA=1 cargo test -p ravel-widgets the_shipped_schema`
      で `assets/themes/ravel.schema.json` を再生成し、このページを直す

## 置き場所

| プラットフォーム | ユーザーディレクトリ |
|---|---|
| macOS | `~/Library/Application Support/ravel/themes/` |
| Windows | `%APPDATA%\ravel\themes\` |
| Linux | `$XDG_CONFIG_HOME/ravel/themes/`（既定 `~/.config/ravel/themes/`） |

**無ければ自分で作る。** アプリは作らない（無いのは正常な状態）。

読む順は**同梱 → ユーザー**で、**後に読んだ方が勝つ**。同じ `name` と `mode`
を名乗るテーマはユーザーのもので置き換わるので、同梱の `Ravel Dark` を
上書きしたければ自分のファイルでその名前を名乗ればよい。ファイル名は
`*.json` であること以外自由（同一ディレクトリ内はファイル名順）。

## 形式

```jsonc
{
  "$schema": "./ravel.schema.json",  // 同梱スキーマ。相対パスなのでオフラインで効く
  "name": "My Themes",               // セット名。テーマ名ではない（作者用）
  "themes": [
    {
      "name": "Midnight",            // 設定ダイアログに出る名前
      "mode": "dark",                // "light" | "dark"。モードごとの選択肢を決める
      "colors": { "primary.background": "#7AA2F7" }
    }
  ]
}
```

**1 キーだけのファイルも有効。** 上の例は `primary.background` しか書いて
いないが、残りは全部モードごとの組み込み値で埋まる（`ThemeSpec::resolve`）。
色以外に `font.family` / `font.size` / `mono_font.family` /
`mono_font.size` / `radius` / `radius.lg` / `spacing` / `row` / `motion` を
書けるが、いずれも省略できる。全キーは
[`assets/themes/ravel.schema.json`](../../assets/themes/ravel.schema.json) と
同梱の [`ravel.json`](../../assets/themes/ravel.json) が正。

## 書ける色は 10 個

| キー | 何になるか |
|---|---|
| `background` | パネルの地。**浮く面**（Tooltip）もここから導出 |
| `foreground` | 文字とアイコン。**押下・無効・混色の向き**を決める |
| `border` | 境界線 |
| `muted.foreground` | 弱い文字 |
| `accent.background` | **hover の面**、および**タブ帯**と非アクティブタブ |
| `primary.background` | 主要色。**フォーカスリング**、**選択行**、スライダの塗り |
| `secondary.background` | 副ボタンの面 |
| `danger.background` | 破壊的操作 |
| `info.background` | 情報 |
| `drop_target.background` | ドラッグ中の受け先 |

**導出は書けない。** hover / press / 無効文字 / フォーカスリング / 選択行 /
タブ帯 / 浮く面 / スライダの塗りは、上の 10 色から純関数で決まる
（`ravel_widgets::tokens::Colors`）。トークンを増やさないのがこの設計で、
`press` を直接指定する場所は無い — 変えたいなら元になる色を変える。

書いた色は gpui-component 側の `Theme` にも導出されるので、借りている部品
（`Input` など）も同じ色で塗られる（`ravel_app::theme_tokens`）。

## 壊れた値の扱い

**キー単位で落ちる。** 色文字列 1 つの typo（`"#GGHHII"`）はその 1 色が組み込み
値に戻るだけで、ファイルの残りは生きる。

**ファイルごと落ちるのは 2 つだけ**:

- JSON 自体が壊れている（閉じ括弧の抜けなど）
- 値の**JSON 型**が違う（`"radius": "4"` のように数値の位置に文字列）

どちらもそのファイルのテーマだけが消え、他のファイルは読まれる。ログに
`ignored invalid theme file` が出る（`RAVEL_LOG=warn`）。

## 反映のしかた

同梱とユーザーの両ディレクトリを `notify` で監視していて、保存すると
**両方を読み直して作り直す**（`ravel_app::themes`）。差分を足すのではなく
作り直すので、**ファイルを消せばそのテーマは Ravel の集合から消える**。

**ただし借りている `gpui_component::ThemeRegistry` は削除 API を持たない**
ので、一度読まれた名前は次の起動までダイアログの候補に残ることがある。
**残った名前を選んでも着られない** — 着るテーマの解決は Ravel の集合を正と
見るので（`app_settings::theme_named`）、同梱テーマにフォールバックする。
候補から消したいなら再起動する。

再読み込みは起動時とまったく同じ経路（Ravel のスキーマ →
`RavelTheme` と `ThemeConfig` の両方を導出 → 外観を再適用）を通る。
`gpui_component::ThemeRegistry::watch_dir` を使っていないのはこのためで、
あちらは gpui-component のパーサで読み直すので、編集した瞬間に Ravel の
スキーマが経路から外れる。

## 規約

- **同梱の `assets/themes/ravel.json` はアプリの既定であって、編集する場所では
  ない。** 設定の既定値（`DEFAULT_LIGHT_THEME` / `DEFAULT_DARK_THEME`）がこの
  ファイルのテーマ名を指しているので、名前を変えると起動時のフォールバックが
  変わる。
- **`ThemeSpec` のキー名を変えない。** 既存のユーザーテーマが黙って既定に
  戻る。増やすときは全部 `Option`（省略できること）のまま増やす。
- **スキーマを変えたら `ravel.schema.json` を再生成する。** 手書きの schema は
  必ず本体と乖離するので、生成物と同梱ファイルの一致を
  `the_shipped_schema_is_the_one_the_wire_form_generates` が縛っている。
  再生成せずにフィールドを足すとこのテストが落ちる。
- **色を増やしたくなったら導出を先に疑う。** 10 色から導けるものはトークンに
  しない（`.agents/rules/ux.md` の不変条件 12、
  `docs/implementation/ui-component-layer-plan.md`）。
