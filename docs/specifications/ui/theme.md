# テーマ 仕様

> 最終更新: 2026-07-30 ／ 索引: [`../ui-spec.md`](../ui-spec.md)

関連要件: REQ-UI-006。

> **旧仕様との違い**: v1 が書いていた TOML スキーマ
> （`[colors] background/surface/primary`、`[colors.node_types]`、
> `[colors.scopes]`、`color_vision`）は**実装されていない**。実装は
> gpui-component のテーマ JSON スキーマに乗っている。

## 形式

`assets/themes/*.json`。gpui-component の
[`.theme-schema.json`](https://github.com/longbridge/gpui-component) に従う。
1 ファイルに複数テーマを含められる。

```json
{
  "$schema": "https://github.com/longbridge/gpui-component/raw/refs/heads/main/.theme-schema.json",
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
- 色キーはドット区切りのフラットな名前（現在 36 キー）。役割名（`accent` /
  `muted` / `list` / `popover` / `primary` / `secondary` / `scrollbar` / `tab` /
  `danger` / `ring` など）で構成され、**用途名ではなく意味名**
- 半透明は 8 桁の hex で表す（例 `#5B6EE115`）
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

## 未実装項目

| 項目 | 担当 |
|---|---|
| テーマを選ぶ UI（`ThemeRegistry` に読み込み済みのものへ dropdown） | `SET-2〜7`（`settings-screen-plan.md`） |
| 色覚特性ごとのバリアント（v1 の `color_vision`） | 未計画。REQ-UI-006 のアクセシビリティ要件として再設計が必要 |
| ノード型ごとの色をテーマで指定する（v1 の `[colors.node_types]`） | 未計画。現在は `DataTypeId` ごとの色をコード側が持つ |
| スコープの色（v1 の `[colors.scopes]`） | スコープ自体が未実装（`INSP-5`） |
| ユーザーのテーマ追加（外部ファイルの読み込み） | `SET-*` と同時に判断 |
