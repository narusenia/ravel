# テーマ 仕様

> 最終更新: 2026-09-07 ／ 索引: [`../ui-spec.md`](../ui-spec.md)

関連要件: REQ-UI-006。

> **旧仕様との違い**: v1 が書いていた TOML スキーマ
> （`[colors] background/surface/primary`、`[colors.node_types]`、
> `[colors.scopes]`、`color_vision`）は**実装されていない**。

## 形式

`assets/themes/*.json`。**スキーマは Ravel が持つ**
（`crates/ravel-widgets/src/tokens.rs`）。1 ファイルに複数テーマを含められる。

ファイルの並びと色キーの名前は gpui-component の `.theme-schema.json` から
そのまま受け継いでいる（既存のテーマファイルを書き換えずに移れるように）が、
**正はもう向こうではない**。Ravel のスキーマが正で、借りている部品
（`Input`、window の `Root`、裾の 8 部品）が読む
`gpui_component::ThemeConfig` は `ravel_app::theme_tokens` が
Ravel のテーマから**導出**する。逆方向の変換は無い。

```json
{
  "name": "Ravel",
  "author": "Ravel Contributors",
  "url": "https://github.com/NaruseNia/ravel",
  "themes": [
    {
      "name": "Ravel Light",
      "mode": "light",
      "is_default": true,
      "shadow": false,
      "radius": 4,
      "radius.lg": 6,
      "font.size": 14,
      "font.family": "Geist",
      "mono_font.size": 12,
      "mono_font.family": "JetBrains Mono",
      "colors": {
        "accent.background": "#E0E0E0",
        "background": "#F9F9F9",
        "foreground": "#000000",
        "border": "#D2D2D2",
        "list.active.background": "#5B6EE115",
        "muted.foreground": "#808090",
        "tab.active.foreground": "#000000"
      }
    }
  ]
}
```

- 同梱は `assets/themes/ravel.json` の 1 ファイル、**Ravel Light / Ravel Dark の
  2 モード**
- 色キーはドット区切りのフラットな名前（現在 39 キー）。役割名（`accent` /
  `muted` / `list` / `popover` / `primary` / `secondary` / `scrollbar` / `tab` /
  `danger` / `ring` など）で構成され、**用途名ではなく意味名**
- **Ravel のスキーマが持つのは色 10 種**（`background` / `foreground` /
  `border` / `muted.foreground` / `accent.background` / `primary.background` /
  `secondary.background` / `danger.background` / `info.background` /
  `drop_target.background`）と `font.*` / `mono_font.*` / `radius` /
  `radius.lg`、加えて Ravel だけが持つ `spacing` / `row` / `motion`。
  残りの 26 色と `highlight` は**スキーマに入っていない**が、ファイルからは
  素通しで `ThemeConfig` に渡る（借りている部品だけが読むため）
- **書かなかったキーは Ravel の組み込み値に落ちる**（`tokens.rs` の
  `Colors::light()` / `Colors::dark()` と各 `Default`）。落ちるのは
  **キー単位**で、1 つの誤記が他の色やテーマを壊すことはない。
  ただし JSON そのものが壊れている場合は**ファイル単位で捨てる**
- Ravel 側のトークン（`spacing` / `row` / `motion`）は既定値のままなので
  同梱ファイルには書かれていない。値は `tokens.rs` にある
- 半透明は 8 桁の hex で表す（例 `#5B6EE115`）。**アルファは末尾の 2 桁**
  （`#RRGGBBAA`）。`#RGB` / `#RGBA` の短縮形も各桁を 2 回にして読む
- `font.family` / `mono_font.family` は**同梱フォント**を指す。実体は
  `assets/fonts/` に置き、`crates/ravel-app/src/fonts.rs` が起動時に
  `add_fonts` で登録する（テーマ適用より前）。日本語は Noto Sans JP に
  フォールバックするが、これはテーマの管轄外 — フォールバックは 1 ロール
  1 ファミリの schema で表せないため、`fonts::ui_font` /
  `fonts::mono_font` がテーマのファミリに付け足す
- canvas に自前で `shape_line` するコード（ノードエディタ、タイムライン、
  カーブエディタ）は要素ツリーの継承が効かない。必ず `fonts::ui_font(cx)` /
  `fonts::mono_font(cx)` から `TextRun` の font を作る
- パネル側は `cx.theme().colors.*` を通して参照する。パネルが独自の色定数を
  持たないのが規約（`.agents/rules/gpui.md`）
- **どのテーマを着るかは設定値**（`settings.toml` の `[appearance]`。環境設定 ▸
  外観の dropdown、`SET-3`）。モードは システム / ライト / ダークで、ライト用と
  ダーク用のテーマ名を別に持つ。既定は OS 追従 + 同梱の 2 テーマで、これは設定
  ファイルが無いときの従来の挙動そのまま。存在しないテーマ名や、枠と `mode` が
  食い違うテーマ名は同梱テーマへフォールバックする（`app_settings.rs`）
- **テーマファイルは `assets/themes/*.json` を全部読む**（起動時に同期で 1 度、
  以降は `ThemeRegistry::watch_dir` が変更ごとに再読込）。再読込後の再適用は
  設定側が `ThemeRegistry` を観測して行うので、後から置かれたテーマも
  設定が名指ししていれば適用される
- **再読込は Ravel のスキーマを通らない。** `ThemeRegistry::watch_dir` は
  ディレクトリを gpui-component 自身のパーサで読み直すので、最初のファイル
  変更以降、レジストリが持つのは導出後ではなく gpui-component が読んだ形に
  なる。同梱の `ravel.json` は Ravel が持つ色を全部書いているので両者は
  一致するが、キーを省いた手書きテーマだけは編集の前後で値が変わる

## 未実装項目

| 項目 | 担当 |
|---|---|
| 色覚特性ごとのバリアント（v1 の `color_vision`） | `SET-15`（`settings-screen-plan.md`）。テーマ資産の追加が前提 |
| ノード型ごとの色をテーマで指定する（v1 の `[colors.node_types]`） | 未計画。現在は `DataTypeId` ごとの色をコード側が持つ |
| スコープの色（v1 の `[colors.scopes]`） | スコープ自体が未実装（`MON-1〜7`、`viewer-scopes-plan.md`） |
| UI スケーリング（`font.size` をユーザーが変える） | `SET-14`。パネルが px 直書きでどれだけ無視するかの調査が前提 |
| ユーザーのテーマディレクトリ（同梱と別に置く、ユーザー側が勝つ） | `UIX-8`（`ui-component-layer-plan.md`）。スキーマの文書化と 2 ディレクトリ監視、上の再読込の穴もここで閉じる |
| `spacing` / `row` / `motion` をパネルが読む | `UIX-3` / `UIX-5`。値は定義済みだが配線はまだ無い |
